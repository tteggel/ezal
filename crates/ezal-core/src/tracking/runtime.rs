//! Input supervision around the pure lifecycle. Firmware serializes source
//! publications through this owner before draining commands to the actuator.

use super::{
    BoundTrackingPlan, ExecutionInputs, InputError, LifecycleError, PassEvent, PlanRevisions,
    PlanTicket, PlanningError, PlanningReport, RunToken, TimingError, TrackingAction, TrackingMode,
    TrackingPhase, TrackingRequest, TrackingSession, TrackingSnapshot, TrackingStop,
};
use crate::mailbox::CommandEnvelope;
use crate::network::NetworkGate;
use crate::ntp::{UtcAnchor, UtcClock};
use crate::operator::{OperatorGrant, OperatorToken};

/// Coherent current inputs supplied by the device, never by a browser. A
/// timestamped network gate detects outages even if it recovers between calls.
#[derive(Clone, Copy, Debug)]
pub struct RuntimeObservation {
    /// Current monotonic instant, shared with output leases.
    pub now_ms: u64,
    /// Most recently accepted SNTP clock, or the same clock after a failed retry.
    pub clock: UtcClock,
    /// Link/address observations from the existing network supervisor.
    pub network: NetworkGate,
}

/// Admission failure for a runtime mutation or planning publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeError {
    /// Lifecycle permission, ordering or phase rejection.
    Lifecycle(LifecycleError),
    /// Input snapshot or source validity rejection.
    Inputs(InputError),
    /// Cannot arm while the device's network permission is absent/expired.
    NetworkUnavailable,
}

impl From<LifecycleError> for RuntimeError {
    fn from(error: LifecycleError) -> Self {
        Self::Lifecycle(error)
    }
}
impl From<InputError> for RuntimeError {
    fn from(error: InputError) -> Self {
        Self::Inputs(error)
    }
}

/// Device-owned lifecycle plus the T5 input boundary. No mutable session
/// accessor can bypass checks on output admission/drain. Actual GPIO, feedback,
/// duty and motion protection remain the production actuator's responsibility.
pub struct TrackingRuntime {
    session: TrackingSession<ExecutionInputs>,
    network_generation: Option<u64>,
    latest_clock: Option<UtcAnchor>,
    observed_ms: Option<u64>,
}

impl Default for TrackingRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl TrackingRuntime {
    /// Reboot always starts in Manual with no selected pass or output authority.
    pub const fn new() -> Self {
        Self {
            session: TrackingSession::new(),
            network_generation: None,
            latest_clock: None,
            observed_ms: None,
        }
    }
    /// Read-only lifecycle status; does not renew any lease.
    pub const fn snapshot(&self) -> TrackingSnapshot {
        self.session.snapshot()
    }
    /// Immutable accepted plan and exact execution inputs.
    pub const fn bound_plan(&self) -> Option<&BoundTrackingPlan<ExecutionInputs>> {
        self.session.bound_plan()
    }
    /// Latest device planning job, if any.
    pub fn planning_ticket(&self) -> Option<PlanTicket> {
        self.session.planning_ticket()
    }
    /// Read-only operator grant at the supplied uptime.
    pub fn operator(&self, now_ms: u64) -> Option<OperatorGrant> {
        self.session.operator(now_ms)
    }

    /// Claim exclusive browser authority independently of any executing pass.
    pub fn claim(
        &mut self,
        client_id: u64,
        observation: RuntimeObservation,
    ) -> Result<OperatorGrant, LifecycleError> {
        self.service(observation)?;
        self.session.claim(client_id, observation.now_ms)
    }
    /// Renew only the current browser grant; it does not renew source validity.
    pub fn renew(
        &mut self,
        token: OperatorToken,
        observation: RuntimeObservation,
    ) -> Result<OperatorGrant, LifecycleError> {
        self.service(observation)?;
        self.session.renew(token, observation.now_ms)
    }
    /// Browser disconnect leaves tracking running under device authority.
    pub fn release(
        &mut self,
        token: OperatorToken,
        observation: RuntimeObservation,
    ) -> Result<bool, LifecycleError> {
        self.service(observation)?;
        self.session.release(token, observation.now_ms)
    }
    /// Apply operator intent before servicing a simultaneous preparation deadline.
    pub fn request(
        &mut self,
        request: TrackingRequest,
        observation: RuntimeObservation,
    ) -> Result<u64, RuntimeError> {
        self.check_inputs(observation)?;
        if request.action == TrackingAction::Arm && !observation.network.ready(observation.now_ms) {
            return Err(RuntimeError::NetworkUnavailable);
        }
        let result = self.session.request(request, observation.now_ms)?;
        if self.session.snapshot().mode == TrackingMode::Tracking
            && self.network_generation.is_none()
        {
            self.network_generation = Some(observation.network.generation());
        }
        self.service(observation)?;
        Ok(result)
    }
    /// Publish complete input/forecast revisions before a simultaneous freeze.
    /// After freeze, the lifecycle retains its inputs and only installation
    /// changes abort the route; live clock corrections are checked separately.
    pub fn refresh(
        &mut self,
        forecast: &[PassEvent],
        revisions: PlanRevisions,
        observation: RuntimeObservation,
    ) -> Result<(), LifecycleError> {
        self.check_inputs(observation)?;
        self.session
            .refresh(forecast, revisions, observation.now_ms)?;
        self.service(observation)
    }
    /// Publish only the current job, retaining the precise source used by its
    /// planner. Sources/clock bindings are validated before accepting success.
    pub fn publish_plan(
        &mut self,
        ticket: PlanTicket,
        result: Result<PlanningReport, PlanningError>,
        inputs: ExecutionInputs,
        observation: RuntimeObservation,
    ) -> Result<(), RuntimeError> {
        self.check_inputs(observation)?;
        if self.session.planning_ticket() != Some(ticket) {
            return Err(LifecycleError::StaleJob.into());
        }
        let error = if inputs.ticket() != ticket
            || result
                .as_ref()
                .ok()
                .and_then(|report| report.plan.as_ref())
                .is_some_and(|plan| !inputs.matches(plan, ticket))
        {
            Some(InputError::Binding)
        } else {
            inputs
                .validate_preview(observation.clock, observation.now_ms)
                .err()
        };
        if let Some(error) = error {
            self.session.invalidate_inputs(error, observation.now_ms)?;
            return Err(error.into());
        }
        let deadline = inputs.freeze_before_ms()?;
        self.session
            .publish_plan(ticket, result, inputs, deadline, observation.now_ms)?;
        self.service(observation)?;
        Ok(())
    }
    /// Report an input failure when the newest planning job cannot be captured.
    pub fn reject_inputs(
        &mut self,
        ticket: PlanTicket,
        error: InputError,
        observation: RuntimeObservation,
    ) -> Result<(), LifecycleError> {
        self.check_inputs(observation)?;
        if self.session.planning_ticket() != Some(ticket) {
            return Err(LifecycleError::StaleJob);
        }
        self.session.invalidate_inputs(error, observation.now_ms)?;
        self.service(observation)
    }
    /// Service input validity before the lifecycle can freeze or follow. Failed
    /// internet refreshes are not outages: still-valid caches remain usable.
    pub fn service(&mut self, observation: RuntimeObservation) -> Result<(), LifecycleError> {
        self.check_inputs(observation)?;
        self.session.tick(observation.now_ms)?;
        if self.session.snapshot().mode == TrackingMode::Manual {
            self.network_generation = None;
        }
        Ok(())
    }
    fn check_inputs(&mut self, observation: RuntimeObservation) -> Result<(), LifecycleError> {
        let now = observation.now_ms;
        if self.observed_ms.is_some_and(|old| now < old) {
            return self.session.clock_failure(now);
        }
        self.observed_ms = Some(now);
        let current = observation.clock.anchor();
        let clock_regressed = self.latest_clock.is_some_and(|previous| {
            current.is_some_and(|anchor| {
                anchor.revision() < previous.revision()
                    || anchor.monotonic_ms() < previous.monotonic_ms()
                    || (anchor.revision() == previous.revision() && anchor != previous)
            })
        });
        if !clock_regressed && current.is_some() {
            self.latest_clock = current;
        }
        let state = self.session.snapshot();
        if state.mode == TrackingMode::Tracking
            && (!observation.network.ready(now)
                || self
                    .network_generation
                    .is_some_and(|generation| generation != observation.network.generation()))
        {
            self.session
                .interrupt(TrackingStop::NetworkUnavailable, now)?;
            self.network_generation = None;
            return Ok(());
        }
        let Some(bound) = self.session.bound_plan() else {
            return Ok(());
        };
        // At/after LOS, the lifecycle's normal completion/late-preparation rule
        // owns the reason; no target will be submitted beyond that deadline.
        if state.run.is_some() && now >= bound.inputs().clock().through_ms() {
            return Ok(());
        }
        let freezing =
            state.phase == TrackingPhase::Armed && now >= bound.plan().assessment.prepare_at_ms;
        let error = if clock_regressed {
            Some(InputError::Timing(TimingError::ClockOrder))
        } else if state.run.is_some() {
            bound
                .inputs()
                .validate_execution(observation.clock, now)
                .err()
        } else {
            bound
                .inputs()
                .validate_preview(observation.clock, now)
                .and_then(|()| {
                    if freezing {
                        bound.inputs().validate_execution(observation.clock, now)
                    } else {
                        Ok(())
                    }
                })
                .err()
        };
        if let Some(error) = error {
            if state.run.is_some() {
                self.session.interrupt(
                    match error {
                        InputError::Timing(error) => TrackingStop::Timing(error),
                        other => TrackingStop::Inputs(other),
                    },
                    now,
                )?;
                self.network_generation = None;
            } else {
                self.session.invalidate_inputs(error, now)?;
            }
        }
        Ok(())
    }
    /// Readiness is a measured executor result, never browser intent.
    pub fn preparation_ready(
        &mut self,
        run: RunToken,
        observation: RuntimeObservation,
    ) -> Result<bool, LifecycleError> {
        self.service(observation)?;
        self.session.preparation_ready(run, observation.now_ms)
    }
    /// Fresh manual intent still passes through the same lease and output gate.
    pub fn manual_command(
        &mut self,
        token: OperatorToken,
        command: CommandEnvelope,
        observation: RuntimeObservation,
    ) -> bool {
        self.service(observation).is_ok()
            && observation.network.ready(observation.now_ms)
            && self
                .session
                .manual_command(token, command, observation.now_ms)
    }
    /// Every executor refresh checks frozen source/clock/network validity.
    pub fn tracking_command(
        &mut self,
        run: RunToken,
        command: CommandEnvelope,
        observation: RuntimeObservation,
    ) -> bool {
        self.service(observation).is_ok()
            && self
                .session
                .tracking_command(run, command, observation.now_ms)
    }
    /// Recheck current observations before the sole output task drains a command.
    pub fn take_command(&mut self, observation: RuntimeObservation) -> Option<CommandEnvelope> {
        let _ = self.service(observation);
        self.session.take_command(observation.now_ms)
    }
    /// Preserve the production actuator's independent physical fault authority.
    pub fn interrupt(
        &mut self,
        reason: TrackingStop,
        observation: RuntimeObservation,
    ) -> Result<(), LifecycleError> {
        self.check_inputs(observation)?;
        self.network_generation = None;
        self.session.interrupt(reason, observation.now_ms)
    }
}
