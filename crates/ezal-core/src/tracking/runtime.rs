//! Input supervision around the pure lifecycle. Firmware serializes source
//! publications through this owner before draining commands to the actuator.

use super::deviation::DeviationMonitor;
use super::recovery::Recovery;
use super::{
    BoundTrackingPlan, DeviationStatus, ExecutionInputs, FollowingController, InputError,
    LifecycleError, PassEvent, PlanRevisions, PlanTicket, PlanningError, PlanningReport,
    PlanningWindow, ReceptionCoverage, RecoveryStatus, RestingStart, RunToken, TimingError,
    TrackingAction, TrackingLifecycle, TrackingMode, TrackingPhase, TrackingPlan, TrackingRequest,
    TrackingSnapshot, TrackingStop, TrackingTermination, REACQUISITION_DWELL_MS,
};
use crate::actuator::Actuator;
use crate::ads1015::FEEDBACK_FULL_SCALE;
use crate::drive::{CommandEnvelope, DriveCommand, MOVEMENT_LEASE_MS};
use crate::feedback::{
    FeedbackHealth, FeedbackSample, FeedbackSnapshot, FEEDBACK_MAX_AGE_MS, FEEDBACK_MAX_LATENCY_MS,
};
use crate::network::NetworkGate;
use crate::ntp::{UtcAnchor, UtcClock};
use crate::operator::{OperatorGrant, OperatorToken};
use crate::position::{InstalledCalibration, Pointing};

/// Coherent current inputs supplied by the device, never by a browser. A
/// timestamped network gate detects outages even if it recovers between calls.
#[derive(Clone, Copy, Debug)]
pub struct RuntimeObservation {
    /// Current monotonic instant, shared with output leases.
    pub now_ms: u64,
    /// SNTP clock after its latest synchronization attempt; disarmed, it has no anchor.
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

/// Final pointing metrics paired with their pass/run and termination reason.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackingSummary {
    /// Original pass coordinates for the retained predicted reception windows.
    pub window: PlanningWindow,
    /// Frozen prediction retained after LOS/cancellation, including missed reception.
    pub reception: ReceptionCoverage,
    /// Identifies the completed/cancelled/stopped run and its reason.
    pub termination: TrackingTermination,
    /// Final maximum and observed time outside tolerance; current is unavailable.
    pub deviation: DeviationStatus,
}

/// Device-owned lifecycle, input boundary and bounded recovery. No mutable lifecycle
/// accessor can bypass checks on output admission/drain. Actual GPIO, feedback,
/// duty and motion protection remain the production actuator's responsibility.
pub struct TrackingRuntime {
    lifecycle: TrackingLifecycle<ExecutionInputs>,
    network_generation: Option<u64>,
    latest_clock: Option<UtcAnchor>,
    observed_ms: Option<u64>,
    last_summary: Option<TrackingSummary>,
    /// Some exactly while the lifecycle has a run, once `sync_execution` has run.
    execution: Option<Execution>,
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
            lifecycle: TrackingLifecycle::new(),
            network_generation: None,
            latest_clock: None,
            observed_ms: None,
            last_summary: None,
            execution: None,
        }
    }
    /// Read-only lifecycle status; does not renew any lease.
    pub const fn snapshot(&self) -> TrackingSnapshot {
        self.lifecycle.snapshot()
    }
    /// Fresh timestamp-matched pointing, or unavailable, plus retained metrics.
    pub fn deviation(&self, now_ms: u64) -> DeviationStatus {
        match &self.execution {
            Some(execution) => execution.deviation(now_ms),
            None => self.last_summary.map(|s| s.deviation).unwrap_or_default(),
        }
    }
    /// Most recent finished run, retained even while another pass is executing.
    pub const fn last_summary(&self) -> Option<TrackingSummary> {
        self.last_summary
    }
    /// Active episode and intercept ETA, without renewing its deadline.
    pub fn recovery(&self) -> Option<RecoveryStatus> {
        self.execution
            .as_ref()?
            .recovery
            .as_ref()
            .map(Recovery::status)
    }
    /// Immutable accepted plan and exact execution inputs.
    pub const fn bound_plan(&self) -> Option<&BoundTrackingPlan<ExecutionInputs>> {
        self.lifecycle.bound_plan()
    }
    /// Latest device planning job, if any.
    pub fn planning_ticket(&self) -> Option<PlanTicket> {
        self.lifecycle.planning_ticket()
    }
    /// A changed physical starting state invalidates preview work immediately,
    /// before a simultaneous preparation deadline can freeze it. The worker
    /// subsequently supplies the new complete input revisions.
    pub fn invalidate_start(&mut self, observation: RuntimeObservation) {
        if self.snapshot().run.is_none()
            && (self.bound_plan().is_some() || self.planning_ticket().is_some())
        {
            let _ = self
                .lifecycle
                .invalidate_inputs(InputError::RevisionChanged, observation.now_ms);
        }
    }
    /// Read-only operator grant at the supplied uptime.
    pub fn operator(&self, now_ms: u64) -> Option<OperatorGrant> {
        self.lifecycle.operator(now_ms)
    }

    /// Claim exclusive browser authority independently of any executing pass.
    pub fn claim(
        &mut self,
        client_id: u32,
        observation: RuntimeObservation,
    ) -> Result<OperatorGrant, LifecycleError> {
        self.service(observation)?;
        self.lifecycle.claim(client_id, observation.now_ms)
    }
    /// Renew only the current browser grant; it does not renew source validity.
    pub fn renew(
        &mut self,
        token: OperatorToken,
        observation: RuntimeObservation,
    ) -> Result<OperatorGrant, LifecycleError> {
        self.service(observation)?;
        self.lifecycle.renew(token, observation.now_ms)
    }
    /// Browser disconnect leaves tracking running under device authority.
    pub fn release(
        &mut self,
        token: OperatorToken,
        observation: RuntimeObservation,
    ) -> Result<bool, LifecycleError> {
        self.service(observation)?;
        self.lifecycle.release(token, observation.now_ms)
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
        let result = self.lifecycle.request(request, observation.now_ms)?;
        if self.lifecycle.snapshot().mode == TrackingMode::Tracking
            && self.network_generation.is_none()
        {
            self.network_generation = Some(observation.network.generation());
        }
        self.service(observation)?;
        Ok(result)
    }
    /// Publish complete input/forecast revisions before a simultaneous freeze.
    /// After freeze, the lifecycle retains its inputs; installation changes and
    /// live clock corrections are checked separately.
    pub fn refresh(
        &mut self,
        forecast: &[PassEvent],
        revisions: PlanRevisions,
        observation: RuntimeObservation,
    ) -> Result<(), LifecycleError> {
        self.check_inputs(observation)?;
        self.lifecycle
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
        if self.lifecycle.planning_ticket() != Some(ticket) {
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
            self.lifecycle
                .invalidate_inputs(error, observation.now_ms)?;
            return Err(error.into());
        }
        let deadline = inputs.freeze_before_ms()?;
        self.lifecycle
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
        if self.lifecycle.planning_ticket() != Some(ticket) {
            return Err(LifecycleError::StaleJob);
        }
        self.lifecycle
            .invalidate_inputs(error, observation.now_ms)?;
        self.service(observation)
    }
    /// Service input validity before the lifecycle can freeze or follow. Failed
    /// internet refreshes are not outages: still-valid caches remain usable.
    pub fn service(&mut self, observation: RuntimeObservation) -> Result<(), LifecycleError> {
        let now = observation.now_ms;
        self.check_inputs(observation)?;
        self.lifecycle.tick(now)?;
        self.sync_execution(now);
        let phase = self.lifecycle.snapshot().phase;
        if let Some(reason) = self
            .execution
            .as_ref()
            .zip(self.lifecycle.bound_plan())
            .and_then(|(execution, bound)| {
                execution.missed_deadline(phase, bound.plan().config.step_ms, now)
            })
        {
            self.lifecycle.interrupt(reason, now)?;
        }
        self.sync_execution(now);
        self.check_recovery_schedule(now);
        if self.lifecycle.snapshot().mode == TrackingMode::Manual {
            self.network_generation = None;
        }
        Ok(())
    }

    fn check_recovery_schedule(&mut self, now: u64) {
        let step_ms = self.lifecycle.bound_plan().map(|b| b.plan().config.step_ms);
        let Some((recovery, step_ms)) = self
            .execution
            .as_mut()
            .and_then(|e| e.recovery.as_mut())
            .zip(step_ms)
        else {
            return;
        };
        if recovery.service(now, step_ms) {
            self.lifecycle
                .recovery_command(recovery.episode(), CommandEnvelope::INHIBIT, now);
        }
    }

    /// Begin or finish the run's execution to match the lifecycle, and the
    /// recovery episode to match its token.
    fn sync_execution(&mut self, now: u64) {
        let state = self.lifecycle.snapshot();
        if self.execution.as_ref().map(|e| e.run) != state.run {
            if let Some(execution) = self.execution.take() {
                if let Some(termination) =
                    state.termination.filter(|t| t.run == Some(execution.run))
                {
                    self.last_summary = Some(execution.finish(termination, now));
                }
            }
            self.execution = state
                .run
                .zip(self.lifecycle.bound_plan())
                .map(|(run, bound)| Execution::new(run, bound.plan()));
        }
        let Some(execution) = self.execution.as_mut() else {
            return;
        };
        if execution
            .recovery
            .as_ref()
            .is_some_and(|r| Some(r.episode()) != state.recovery)
        {
            if state.phase == TrackingPhase::Following {
                // Preserve the physical pulse/coast history when the recovery
                // follower joins the original route. Starting a new follower
                // here could reuse feedback acquired before the last release.
                execution.follower = execution
                    .recovery
                    .as_mut()
                    .and_then(Recovery::take_follower);
            }
            execution.recovery = None;
        }
        if let Some(episode) = state.recovery.filter(|_| execution.recovery.is_none()) {
            execution.recovery = Some(Recovery::new(episode));
        }
    }

    /// Publish feedback and the coherently serviced output owner before issuing
    /// following commands. New feedback wins a simultaneous freshness deadline.
    /// Firmware calls this in Preparing/Ready as well as Following/Reacquiring;
    /// the output actuator still enforces its own independent interlocks.
    pub fn observe_tracking(
        &mut self,
        run: RunToken,
        feedback: FeedbackSnapshot,
        actuator: &Actuator,
        observation: RuntimeObservation,
    ) -> Result<(), LifecycleError> {
        let now = observation.now_ms;
        self.check_inputs(observation)?;
        self.lifecycle.tick(now)?;
        self.sync_execution(now);
        if self.lifecycle.snapshot().run != Some(run) {
            return Ok(());
        }
        self.check_recovery_schedule(now);
        let (Some(bound), Some(execution)) = (self.lifecycle.bound_plan(), self.execution.as_mut())
        else {
            return Ok(());
        };
        let reason = if let Some(fault) = actuator.fault() {
            Some(TrackingStop::Motion(fault))
        } else if actuator.configuration() != bound.plan().actuator {
            // Installation settings are compile-time constants, so the run compares
            // the actuator configuration by value. A persisted calibration must bring
            // its own by-value or revision check.
            Some(TrackingStop::InstallationChanged)
        } else if feedback.health != FeedbackHealth::Healthy {
            Some(TrackingStop::Feedback(feedback.health))
        } else {
            match feedback.last_valid {
                None => Some(TrackingStop::Feedback(FeedbackHealth::Unavailable)),
                Some(sample)
                    if sample.sampled_at_ms() > now
                        || execution.feedback.is_some_and(|old| {
                            sample.sampled_at_ms() < old.sampled_at_ms()
                                || (sample.sampled_at_ms() == old.sampled_at_ms() && sample != old)
                        }) =>
                {
                    Some(TrackingStop::Feedback(FeedbackHealth::TimestampInvalid))
                }
                Some(sample) if now - sample.sampled_at_ms() >= FEEDBACK_MAX_AGE_MS => {
                    Some(TrackingStop::Feedback(FeedbackHealth::Stale))
                }
                Some(sample) if !sample.position().is_valid_for(bound.plan().calibration) => {
                    Some(TrackingStop::Feedback(FeedbackHealth::Invalid))
                }
                _ => None,
            }
        };
        if let Some(reason) = reason {
            self.lifecycle.interrupt(reason, now)?;
            self.sync_execution(now);
            return Ok(());
        }
        let sample = feedback.last_valid.unwrap();
        let repeated_sample = execution.feedback == Some(sample);
        execution.feedback = Some(sample);
        if let Some(recovery) = execution.recovery.as_mut() {
            recovery.observe_output(actuator.actual(), now);
        }
        let phase = self.lifecycle.snapshot().phase;
        if repeated_sample
            || !matches!(phase, TrackingPhase::Following | TrackingPhase::Reacquiring)
            || sample.sampled_at_ms() < execution.window.aos_at_ms
        {
            return Ok(());
        }
        let Some(look) = bound.inputs().look_at_uptime(sample.sampled_at_ms()) else {
            self.lifecycle.interrupt(TrackingStop::SourceInvalid, now)?;
            self.sync_execution(now);
            return Ok(());
        };
        let installed = InstalledCalibration::new(bound.plan().calibration, FEEDBACK_FULL_SCALE)
            .expect("accepted plan calibration");
        let resolution = installed.resolution_deg(0.5);
        // A0 (elevation) is the timestamp reference; only A1 can be younger.
        // Use the hardware acquisition bound, not a favorable simulation case.
        let uncertainty = bound.plan().config.uncertainty_deg
            + libm::sqrtf(
                resolution.azimuth_deg * resolution.azimuth_deg
                    + resolution.elevation_deg * resolution.elevation_deg,
            )
            + bound.plan().actuator.travel.azimuth.max_speed_deg_per_s
                * FEEDBACK_MAX_LATENCY_MS as f32
                / 1000.0;
        let monitor = &mut execution.monitor;
        if monitor
            .observe(
                sample,
                Pointing::new(look.azimuth_deg as f32, look.elevation_deg as f32),
                uncertainty,
            )
            .is_none()
        {
            self.lifecycle.interrupt(
                TrackingStop::Feedback(FeedbackHealth::TimestampInvalid),
                now,
            )?;
        } else if phase == TrackingPhase::Following && monitor.needs_recovery() {
            self.lifecycle.begin_reacquisition(run, now)?;
        } else if monitor.recovered() {
            if let Some(recovery) = execution
                .recovery
                .as_ref()
                .filter(|r| r.rejoined(sample.sampled_at_ms()))
            {
                self.lifecycle
                    .finish_reacquisition(recovery.episode(), now)?;
            }
        }
        self.sync_execution(now);
        Ok(())
    }

    /// Record every physical transition, including the first service before
    /// generating a command. Never report an output which was not applied.
    pub fn observe_output(&mut self, applied: DriveCommand, now_ms: u64) {
        let Some(execution) = self.execution.as_mut() else {
            return;
        };
        if let Some(follower) = execution.follower.as_mut() {
            follower.observe_output(applied, now_ms);
        }
        if let Some(recovery) = execution.recovery.as_mut() {
            recovery.observe_output(applied, now_ms);
        }
    }

    /// One device execution turn, called after servicing/applying the actuator
    /// at `observation.now_ms`. Planning runs separately. The output owner must
    /// drain again with its actual current time, preserving this command's age.
    pub fn execution_command(
        &mut self,
        feedback: FeedbackSnapshot,
        actuator: &Actuator,
        observation: RuntimeObservation,
    ) {
        // Observe before servicing so new feedback wins a simultaneous freshness
        // deadline, and again after, since the run may begin inside the service.
        if let Some(run) = self.snapshot().run {
            let _ = self.observe_tracking(run, feedback, actuator, observation);
        }
        if self.service(observation).is_err() {
            return;
        }
        if let Some(run) = self.snapshot().run {
            let _ = self.observe_tracking(run, feedback, actuator, observation);
        }
        let state = self.snapshot();
        let (Some(run), Some(bound), Some(execution)) = (
            state.run,
            self.lifecycle.bound_plan(),
            self.execution.as_mut(),
        ) else {
            return;
        };
        let plan = bound.plan();
        let now = observation.now_ms;
        if state.phase == TrackingPhase::Reacquiring {
            // A subsequent normal follower must not reuse pre-gap velocity.
            execution.follower = None;
            execution.last_control_at_ms = Some(now);
            let (Some(recovery), Some(feedback)) =
                (execution.recovery.as_mut(), execution.feedback)
            else {
                return;
            };
            let start = RestingStart {
                at_ms: now,
                feedback,
                actuator: *actuator,
                network: observation.network,
            };
            let command = recovery.command(plan, start, now);
            self.lifecycle
                .recovery_command(recovery.episode(), command, now);
            return;
        }
        let preparing = matches!(state.phase, TrackingPhase::Preparing | TrackingPhase::Ready);
        let offset = if preparing {
            0
        } else {
            now.saturating_sub(plan.window.aos_at_ms)
        };
        let Some(target) = u32::try_from(offset)
            .ok()
            .and_then(|t| plan.route.pointing_at(t))
        else {
            let _ = self.interrupt(TrackingStop::SourceInvalid, observation);
            return;
        };
        let follower = execution.follower.get_or_insert_with(|| {
            FollowingController::new(
                plan.config.reference,
                plan.config.deadband_deg,
                plan.actuator.relays,
            )
            .expect("accepted follower configuration")
        });
        let command = follower.command(target, feedback.last_valid, actuator.actual(), now);
        execution.last_control_at_ms = Some(now);
        if preparing {
            let sample = feedback.last_valid.expect("observed healthy feedback");
            let age = now.saturating_sub(sample.sampled_at_ms());
            let acquired = actuator.resting_history(now).is_some_and(|(_, idle)| {
                idle[0] >= age.saturating_add(plan.actuator.travel.azimuth.coast_ms)
                    && idle[1] >= age.saturating_add(plan.actuator.travel.elevation.coast_ms)
            }) && (sample.position().azimuth_deg - target.azimuth_deg).abs()
                <= plan.config.preparation_tolerance_deg
                && (sample.position().elevation_deg - target.elevation_deg).abs()
                    <= plan.config.preparation_tolerance_deg;
            if execution
                .readiness
                .observe(acquired, sample.sampled_at_ms())
            {
                let _ = self.lifecycle.preparation_ready(run, now);
            }
        }
        self.lifecycle.tracking_command(
            run,
            CommandEnvelope {
                command,
                valid_until_ms: now.saturating_add(MOVEMENT_LEASE_MS),
            },
            now,
        );
    }

    /// Deadlines the output task must include alongside relay/feedback/network
    /// deadlines. A regular poll alone cannot hit a fixed recovery launch.
    pub fn execution_deadline_ms(&self) -> Option<u64> {
        let bound = self.bound_plan()?;
        let plan = bound.plan();
        let state = self.snapshot();
        let phase_deadline = match state.phase {
            TrackingPhase::Armed => Some(plan.assessment.prepare_at_ms),
            TrackingPhase::Preparing | TrackingPhase::Ready => Some(plan.window.aos_at_ms),
            TrackingPhase::Following => {
                let gap = plan.reception().gap().map(|g| g.recovery_at_ms);
                gap.filter(|at| self.observed_ms.is_none_or(|now| *at > now))
                    .or(Some(
                        plan.window.aos_at_ms + u64::from(plan.window.duration_ms),
                    ))
            }
            TrackingPhase::Reacquiring => self.recovery().map(|r| {
                r.launch_at_ms
                    .filter(|at| self.observed_ms.is_none_or(|now| *at > now))
                    .unwrap_or(r.episode.deadline_ms())
            }),
            _ => None,
        };
        phase_deadline
            .into_iter()
            .chain(
                self.execution
                    .as_ref()
                    .and_then(|e| e.last_control_at_ms)
                    .map(|at| at.saturating_add(plan.config.step_ms)),
            )
            .min()
    }

    /// One bounded planning unit, for a yielding background task. The immutable
    /// frozen plan stays in this owner; no external worker can publish an old
    /// result after cancellation, expiry or replacement.
    pub fn step_reacquisition(
        &mut self,
        observation: RuntimeObservation,
    ) -> Result<bool, LifecycleError> {
        self.service(observation)?;
        let now = observation.now_ms;
        let (Some(bound), Some(recovery)) = (
            self.lifecycle.bound_plan(),
            self.execution.as_mut().and_then(|e| e.recovery.as_mut()),
        ) else {
            return Ok(false);
        };
        match recovery.step(bound.plan(), bound.inputs(), now) {
            Ok(searching) => Ok(searching),
            Err(()) => {
                self.lifecycle.interrupt(TrackingStop::SourceInvalid, now)?;
                self.sync_execution(now);
                Ok(false)
            }
        }
    }
    fn check_inputs(&mut self, observation: RuntimeObservation) -> Result<(), LifecycleError> {
        let now = observation.now_ms;
        if self.observed_ms.is_some_and(|old| now < old) {
            let result = self.lifecycle.clock_failure(now);
            self.sync_execution(now);
            return result;
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
        let state = self.lifecycle.snapshot();
        if state.mode == TrackingMode::Tracking
            && (!observation.network.ready(now)
                || self
                    .network_generation
                    .is_some_and(|generation| generation != observation.network.generation()))
        {
            self.lifecycle
                .interrupt(TrackingStop::NetworkUnavailable, now)?;
            self.network_generation = None;
            self.sync_execution(now);
            return Ok(());
        }
        let Some(bound) = self.lifecycle.bound_plan() else {
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
                self.lifecycle.interrupt(TrackingStop::Inputs(error), now)?;
                self.network_generation = None;
                self.sync_execution(now);
            } else {
                self.lifecycle.invalidate_inputs(error, now)?;
            }
        }
        Ok(())
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
                .lifecycle
                .manual_command(token, command, observation.now_ms)
    }
    /// Recheck current observations before the sole output task drains a command.
    pub fn take_command(&mut self, observation: RuntimeObservation) -> Option<CommandEnvelope> {
        let _ = self.service(observation);
        let command = self.lifecycle.take_command(observation.now_ms);
        if let Some(recovery) = self.execution.as_mut().and_then(|e| e.recovery.as_mut()) {
            recovery.command_taken();
        }
        command
    }
    /// Preserve the production actuator's independent physical fault authority.
    pub fn interrupt(
        &mut self,
        reason: TrackingStop,
        observation: RuntimeObservation,
    ) -> Result<(), LifecycleError> {
        self.check_inputs(observation)?;
        self.network_generation = None;
        self.lifecycle.interrupt(reason, observation.now_ms)?;
        self.sync_execution(observation.now_ms);
        Ok(())
    }
}

/// Executor state owned by one run. It begins with the run and is finished
/// into a [`TrackingSummary`] the instant the run ends.
struct Execution {
    run: RunToken,
    window: PlanningWindow,
    reception: ReceptionCoverage,
    monitor: DeviationMonitor,
    feedback: Option<FeedbackSample>,
    follower: Option<FollowingController>,
    last_control_at_ms: Option<u64>,
    readiness: Readiness,
    recovery: Option<Recovery>,
}

impl Execution {
    fn new(run: RunToken, plan: &TrackingPlan) -> Self {
        Self {
            run,
            window: plan.window,
            reception: plan.reception(),
            monitor: DeviationMonitor::new(plan.config.tolerance_deg),
            feedback: None,
            follower: None,
            last_control_at_ms: None,
            readiness: Readiness::default(),
            recovery: None,
        }
    }

    fn deviation(&self, now_ms: u64) -> DeviationStatus {
        DeviationStatus {
            run: Some(self.run),
            ..self.monitor.status(now_ms)
        }
    }

    /// A missed control step outside recovery, or stale feedback, stops the run.
    fn missed_deadline(
        &self,
        phase: TrackingPhase,
        step_ms: u64,
        now: u64,
    ) -> Option<TrackingStop> {
        if phase != TrackingPhase::Reacquiring
            && self
                .last_control_at_ms
                .is_some_and(|last| now.saturating_sub(last) > step_ms)
        {
            Some(TrackingStop::ExecutionDeadline)
        } else if self.feedback.is_some_and(|sample| {
            now.checked_sub(sample.sampled_at_ms())
                .is_none_or(|age| age >= FEEDBACK_MAX_AGE_MS)
        }) {
            Some(TrackingStop::Feedback(FeedbackHealth::Stale))
        } else {
            None
        }
    }

    fn finish(mut self, termination: TrackingTermination, now_ms: u64) -> TrackingSummary {
        self.monitor.finish(now_ms);
        TrackingSummary {
            window: self.window,
            reception: self.reception,
            termination,
            deviation: self.deviation(now_ms),
        }
    }
}

/// The measured readiness dwell during preparation. Repeated reads of one sample
/// add nothing; a lost acquisition or a feedback gap restarts the dwell.
#[derive(Default)]
struct Readiness {
    since_ms: Option<u64>,
    sample_ms: Option<u64>,
}

impl Readiness {
    /// Whether this sample completes the readiness dwell.
    fn observe(&mut self, acquired: bool, sampled_at_ms: u64) -> bool {
        let previous = self.sample_ms.replace(sampled_at_ms);
        if !acquired {
            self.since_ms = None;
            return false;
        }
        if previous == Some(sampled_at_ms) {
            return false;
        }
        if previous.is_none_or(|last| sampled_at_ms.saturating_sub(last) >= FEEDBACK_MAX_AGE_MS) {
            self.since_ms = None;
        }
        let since = *self.since_ms.get_or_insert(sampled_at_ms);
        sampled_at_ms.saturating_sub(since) >= REACQUISITION_DWELL_MS
    }
}
