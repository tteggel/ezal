//! Device ownership, independent tracking and coherent actuator delivery, with
//! the selected pass's background planning in [`PlanningWorker`]. Reads never
//! renew leases or drain commands. Firmware serializes mutations.

use crate::actuator::{Actuator, ActuatorInput, ActuatorUpdate};
use crate::drive::{CommandEnvelope, DriveCommand};
use crate::duty::DutyStatus;
use crate::feedback::{FeedbackHealth, FeedbackSnapshot};
use crate::interlock::FeedbackInterlock;
use crate::motion::MotionFault;
use crate::network::NetworkGate;
use crate::ntp::UtcClock;
use crate::operator::{OperatorGrant, OperatorToken};
use crate::position::{FeedbackVoltages, Pointing};

/// How far an idle pose may stray from the one planning started from before
/// planning starts again, unless the calibrated feedback-noise allowance is
/// larger. Six ADC counts cover the observed five-count span with one count
/// of headroom. They span about 2.45° in the installed azimuth calibration.
/// That error adds under 0.9 s of slew at the slowest modelled 2.8°/s, and
/// preparation is scheduled 20 s beyond the slowest rehearsal. Any output this
/// device drives restarts planning regardless; a turn of the rotator's own
/// controller beyond this does too.
pub const START_TOLERANCE_DEG: f32 = 2.0;
const START_NOISE_CODES: u8 = 6;
use crate::tracking::{
    RestingStart, RuntimeObservation, TrackingMode, TrackingRuntime, TrackingStop,
};

mod planning;

pub use planning::{PlanningUnit, PlanningWorker, TrackingSource};

struct StartupRestOverride {
    operator: OperatorToken,
    valid_until_ms: u64,
    network_generation: u64,
}

/// Leased ownership and the output actually applied by the GPIO task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlStatus {
    /// Whether this connection owns the current exclusive operator lease.
    pub controller: bool,
    /// Last output applied to GPIOs.
    pub drive: DriveCommand,
    /// Whether a usable network observation is still current.
    pub network_ready: bool,
    /// Latched physical feedback fault, cleared only by restart.
    pub motion_fault: Option<MotionFault>,
    /// Shared per-axis allowance, including any rest required before movement.
    pub duty: DutyStatus,
}

/// One coherent observation for a dashboard connection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TelemetrySnapshot {
    /// Raw diagnostics and last calibrated feedback.
    pub feedback: FeedbackSnapshot,
    /// Ownership and applied output.
    pub control: ControlStatus,
    /// Permission generation used to bind this observation's freshness proof.
    pub network: NetworkGate,
}

/// One device owner for manual leases, tracking and coherent actuator delivery.
/// The physical actuator remains outside this state, owned by the GPIO task.
pub struct CoordinationState {
    feedback: FeedbackSnapshot,
    interlock: FeedbackInterlock,
    tracking: TrackingRuntime,
    next_client_id: u32,
    network: NetworkGate,
    clock: UtcClock,
    applied: Actuator,
    applied_at_ms: Option<u64>,
    start_revision: u64,
    start_identity: Option<(Pointing, [u64; 2])>,
    startup_rest_override: Option<StartupRestOverride>,
}

impl Default for CoordinationState {
    fn default() -> Self {
        Self::new()
    }
}

impl CoordinationState {
    /// Boot idle, with unknown duty, no clock, network permission or owner.
    pub const fn new() -> Self {
        Self {
            feedback: FeedbackSnapshot::INITIAL,
            interlock: FeedbackInterlock::INHIBITED,
            tracking: TrackingRuntime::new(),
            next_client_id: 0,
            network: NetworkGate::new(),
            clock: UtcClock::new(),
            applied: Actuator::new(),
            applied_at_ms: None,
            start_revision: 1,
            start_identity: None,
            startup_rest_override: None,
        }
    }

    fn observation(&self, now_ms: u64) -> RuntimeObservation {
        RuntimeObservation {
            now_ms,
            clock: self.clock,
            network: self.network,
        }
    }

    fn revoke_network_motion(&mut self, now_ms: u64) {
        let token = self.tracking.operator(now_ms).map(|g| g.token);
        let observation = self.observation(now_ms);
        if let Some(token) = token {
            let _ = self.tracking.release(token, observation);
        }
        let _ = self.tracking.service(observation);
    }

    /// Service network permission without reading or moving GPIOs.
    pub fn advance(&mut self, now_ms: u64) {
        if self.network.expire(now_ms) {
            self.revoke_network_motion(now_ms);
        }
    }

    /// Publish readiness; an outage revokes browser ownership and manual motion.
    /// Armed and executing passes retain device authority.
    pub fn publish_network(&mut self, ready: bool, now_ms: u64) -> bool {
        let lost = self.network.observe(now_ms, ready);
        if lost {
            self.revoke_network_motion(now_ms);
        }
        lost
    }

    /// Read permission without renewing it.
    pub const fn network(&self) -> NetworkGate {
        self.network
    }

    /// Publish the clock after every synchronization attempt; a disarmed clock
    /// stops a run and blocks preview.
    pub fn publish_clock(&mut self, clock: UtcClock, now_ms: u64) {
        self.clock = clock;
        let _ = self.tracking.service(self.observation(now_ms));
    }

    /// Retain the ADC POST readings before recurring acquisition starts.
    pub fn store_raw_feedback(&mut self, raw: FeedbackVoltages) {
        self.feedback.raw = Some(raw);
    }

    /// Publish one acquisition. A pose outside the planning start's tolerance
    /// invalidates preview work immediately, before a simultaneous preparation
    /// deadline can freeze it.
    pub fn publish_feedback(
        &mut self,
        feedback: FeedbackSnapshot,
        interlock: FeedbackInterlock,
        now_ms: u64,
    ) -> bool {
        let moved = feedback
            .last_valid
            .zip(self.start_identity)
            .is_some_and(|(sample, (start, ..))| !self.near_start(sample.position(), start));
        let changed = self.feedback.health != feedback.health || moved;
        self.feedback = feedback;
        self.interlock = interlock;
        if changed {
            self.changed_start(now_ms);
        }
        if let Some(run) = self.tracking.snapshot().run {
            let _ = self.tracking.observe_tracking(
                run,
                feedback,
                &self.applied,
                self.observation(now_ms),
            );
        }
        self.applied.actual().is_active() || self.tracking.snapshot().run.is_some()
    }

    /// Read the latest feedback diagnostics.
    pub const fn feedback(&self) -> FeedbackSnapshot {
        self.feedback
    }

    /// Allocate a never-reused connection id; zero fails closed on exhaustion.
    pub fn register_client(&mut self, now_ms: u64) -> u32 {
        self.advance(now_ms);
        let Some(id) = self.next_client_id.checked_add(1) else {
            return 0;
        };
        self.next_client_id = id;
        if self.network.ready(now_ms) {
            let _ = self.tracking.claim(id, self.observation(now_ms));
        }
        id
    }

    /// Current exclusive lease for this connection; a read cannot renew it.
    pub fn operator(&self, client_id: u32, now_ms: u64) -> Option<OperatorGrant> {
        self.tracking
            .operator(now_ms)
            .filter(|g| g.token.client_id() == client_id)
    }

    /// Renew only a still-current grant after a fresh transport round trip.
    pub fn renew_operator(&mut self, token: OperatorToken, now_ms: u64) -> bool {
        self.advance(now_ms);
        self.network.ready(now_ms) && self.tracking.renew(token, self.observation(now_ms)).is_ok()
    }

    /// Observe one coherent snapshot without performing transitions.
    pub fn telemetry(&self, client_id: u32, now_ms: u64) -> TelemetrySnapshot {
        let ready = self.network.ready(now_ms);
        TelemetrySnapshot {
            feedback: self.feedback,
            control: ControlStatus {
                controller: ready && self.operator(client_id, now_ms).is_some(),
                drive: self.applied.actual(),
                network_ready: ready,
                motion_fault: self.applied.fault(),
                duty: self.applied.status().duty,
            },
            network: self.network,
        }
    }

    /// Claim unowned/expired control; a viewer cannot displace a live lease.
    pub fn take_control(&mut self, client_id: u32, now_ms: u64) -> bool {
        self.advance(now_ms);
        self.network.ready(now_ms)
            && self
                .tracking
                .claim(client_id, self.observation(now_ms))
                .is_ok()
    }

    /// Closing a socket releases its lease, leaving a device-owned run intact.
    pub fn release_client(&mut self, client_id: u32, now_ms: u64) -> bool {
        self.advance(now_ms);
        self.operator(client_id, now_ms).is_some_and(|g| {
            self.tracking
                .release(g.token, self.observation(now_ms))
                .unwrap_or(false)
        })
    }

    /// Admit manual intent through the same operator lease and mode as tracking.
    pub fn accept_command(
        &mut self,
        client_id: u32,
        envelope: CommandEnvelope,
        now_ms: u64,
    ) -> bool {
        self.advance(now_ms);
        self.operator(client_id, now_ms).is_some_and(|g| {
            self.tracking
                .manual_command(g.token, envelope, self.observation(now_ms))
        })
    }

    /// Queue an explicit startup-only rest assertion from a fresh, leased
    /// controller in Manual mode. The GPIO owner applies it to the real actuator;
    /// admitting it never changes telemetry or grants movement by itself.
    pub fn request_startup_rest_override(
        &mut self,
        operator: OperatorToken,
        valid_until_ms: u64,
        now_ms: u64,
    ) -> bool {
        self.advance(now_ms);
        if now_ms >= valid_until_ms
            || !self.startup_override_permitted(operator, now_ms)
            || !self.applied.status().duty.startup_rest_required
            || self.applied.fault().is_some()
            || self.applied.actual().is_active()
            || self.applied.target().is_active()
            || self.startup_rest_override.is_some()
        {
            return false;
        }
        if !self.tracking.manual_command(
            operator,
            CommandEnvelope::INHIBIT,
            self.observation(now_ms),
        ) {
            return false;
        }
        self.startup_rest_override = Some(StartupRestOverride {
            operator,
            valid_until_ms,
            network_generation: self.network.generation(),
        });
        true
    }

    fn startup_override_permitted(&self, operator: OperatorToken, now_ms: u64) -> bool {
        self.network.ready(now_ms)
            && self
                .operator(operator.client_id(), now_ms)
                .is_some_and(|grant| grant.token == operator)
            && self.tracking.snapshot().mode == TrackingMode::Manual
    }

    /// Service the physical actuator, including any startup rest assertion.
    /// Ownership, network generation, freshness and mode are rechecked here.
    /// The caller must apply the returned GPIO output before `record_actuator`.
    pub fn service_actuator(&mut self, actuator: &mut Actuator, now_ms: u64) -> ActuatorUpdate {
        let pending = self.startup_rest_override.take();
        let input = self.take_actuator_input(now_ms);
        let update = actuator.step(input, now_ms);
        if pending.is_some_and(|request| {
            now_ms < request.valid_until_ms
                && self.network.generation() == request.network_generation
                && self.startup_override_permitted(request.operator, now_ms)
        }) && actuator.override_startup_rest(now_ms)
        {
            // Discard any buffered hold; a new press is required after the
            // authoritative telemetry reports the replenished allowance.
            actuator.step(
                ActuatorInput {
                    command: Some(CommandEnvelope::INHIBIT),
                    ..input
                },
                now_ms,
            )
        } else {
            update
        }
    }

    /// Serialize a trusted device operation (planning, or leased operator intent)
    /// with current network/clock inputs. Runtime checks cannot be bypassed.
    pub fn tracking_operation<R>(
        &mut self,
        now_ms: u64,
        operation: impl FnOnce(&mut TrackingRuntime, RuntimeObservation) -> R,
    ) -> R {
        self.advance(now_ms);
        operation(
            &mut self.tracking,
            RuntimeObservation {
                now_ms,
                clock: self.clock,
                network: self.network,
            },
        )
    }

    /// Read-only device execution status, independent of browser ownership.
    pub const fn tracking(&self) -> &TrackingRuntime {
        &self.tracking
    }

    fn changed_start(&mut self, now_ms: u64) {
        if let Some(next) = self.start_revision.checked_add(1) {
            self.start_revision = next;
            self.tracking.invalidate_start(self.observation(now_ms));
        } else {
            let _ = self
                .tracking
                .interrupt(TrackingStop::ClockOrCounter, self.observation(now_ms));
        }
    }

    /// Identity of the physical starting assumptions. Fresh timestamps and
    /// feedback noise do not restart planning; a moved pose or changed
    /// duty history does.
    pub const fn start_revision(&self) -> u64 {
        self.start_revision
    }

    /// Last coherent, settled output snapshot. Its original time is preserved.
    pub fn resting_start(&self) -> Option<RestingStart> {
        self.resting_snapshot().map(|(start, _)| start)
    }

    fn resting_snapshot(&self) -> Option<(RestingStart, [u64; 2])> {
        let at_ms = self.applied_at_ms?;
        let feedback = self.feedback.last_valid?;
        if self.feedback.health != FeedbackHealth::Healthy || feedback.sampled_at_ms() > at_ms {
            return None;
        }
        let start = RestingStart {
            at_ms,
            feedback,
            actuator: self.applied,
        };
        let (used, _) = start.history()?;
        Some((start, used))
    }

    /// Record the actuator only AFTER applying its GPIO output, at its service time.
    pub fn record_actuator(&mut self, actuator: &Actuator, now_ms: u64) {
        self.applied = *actuator;
        self.applied_at_ms = Some(now_ms);
        self.tracking.observe_output(actuator.actual(), now_ms);
        let identity = self
            .resting_snapshot()
            .map(|(start, used)| (start.feedback.position(), used));
        if !self.same_start(identity) {
            self.start_identity = identity;
            self.changed_start(now_ms);
        }
    }

    /// Duty history must match exactly; the pose only within noise.
    fn same_start(&self, identity: Option<(Pointing, [u64; 2])>) -> bool {
        match (identity, self.start_identity) {
            (Some((pose, used)), Some((start, start_used))) => {
                used == start_used && self.near_start(pose, start)
            }
            (identity, start) => identity.is_none() && start.is_none(),
        }
    }

    /// [`START_TOLERANCE_DEG`], or six physical ADC codes if that is larger,
    /// around the pose planning started from. A restart per
    /// flicker would never let a plan finish. Drift is measured from that
    /// start, so it adds up.
    fn near_start(&self, pose: Pointing, start: Pointing) -> bool {
        let code = self
            .interlock
            .feedback_code_deg()
            .unwrap_or(Pointing::new(0.0, 0.0));
        let tolerance =
            |code_deg: f32| (code_deg * f32::from(START_NOISE_CODES)).max(START_TOLERANCE_DEG);
        (pose.azimuth_deg - start.azimuth_deg).abs() <= tolerance(code.azimuth_deg)
            && (pose.elevation_deg - start.elevation_deg).abs() <= tolerance(code.elevation_deg)
    }

    /// Generate a device command only after the actuator was serviced/applied at
    /// this instant. Drain again using actual current time before applying it.
    pub fn execute_tracking(&mut self, now_ms: u64) {
        self.advance(now_ms);
        if self.applied_at_ms == Some(now_ms) {
            self.tracking
                .execution_command(self.feedback, &self.applied, self.observation(now_ms));
        }
    }

    /// Drain pending work with current inputs. New feedback is observed before
    /// freshness/cadence checks and before a pending relay activation can occur.
    pub fn take_actuator_input(&mut self, now_ms: u64) -> ActuatorInput {
        self.advance(now_ms);
        let observation = self.observation(now_ms);
        if let Some(run) = self.tracking.snapshot().run {
            let _ = self
                .tracking
                .observe_tracking(run, self.feedback, &self.applied, observation);
        }
        ActuatorInput {
            interlock: self.interlock,
            network: self.network,
            command: self.tracking.take_command(observation),
        }
    }
}
