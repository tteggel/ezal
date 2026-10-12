//! Device-owned single-pass policy. It owns the Manual/Tracking mode, the operator lease and
//! the command mailbox; [`crate::coordination`] serializes its inputs and delivers its commands.
//! No method performs I/O or GPIO. `S` owns immutable execution inputs; `TrackingRuntime`
//! supplies their gates. Transport never supplies those inputs or planning results directly.

use super::mailbox::CommandMailbox;
use super::{
    reconcile_event, AdmissionPolicy, CandidateFailure, EventError, EventId, InputError, PassEvent,
    PlanningError, PlanningReport, PreparationTiming, ReceptionCoverage, TrackingPlan,
    REACQUISITION_LIMIT_MS,
};
use crate::drive::{Command, CommandEnvelope, MOVEMENT_LEASE_MS};
use crate::feedback::FeedbackHealth;
use crate::motion::MotionFault;
use crate::operator::{LeaseError, OperatorGrant, OperatorLease, OperatorToken};

/// Input revisions are boot-local server counters, monotonic within a selection.
/// A new satellite selection can use its own older orbital publication.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlanRevisions {
    /// Accepted orbital data, including source/expiry metadata.
    pub orbit: u64,
    /// Ground station and reception mask.
    pub observer: u64,
    /// Installed feedback mapping and mechanical limits.
    pub calibration: u64,
    /// Motor, relay, duty and pointing assumptions.
    pub motion: u64,
    /// Fractional UTC anchor and its uncertainty.
    pub clock: u64,
    /// Starting pose/output history; advance when that planning assumption changes.
    pub start: u64,
}

impl PlanRevisions {
    fn follows(self, previous: Self) -> bool {
        self.orbit >= previous.orbit
            && self.observer >= previous.observer
            && self.calibration >= previous.calibration
            && self.motion >= previous.motion
            && self.clock >= previous.clock
            && self.start >= previous.start
    }
}

/// Exactly one selection, associated with its original event throughout refreshes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    /// Stable identity across changes to AOS/LOS or table order.
    pub id: EventId,
    /// Immutable association anchor; refreshes cannot walk toward another orbit.
    pub original: PassEvent,
    /// Latest uniquely associated window, or none when reconciliation is blocked.
    pub current: Option<PassEvent>,
}

/// One planning job's immutable identity. Only the latest job can publish.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanTicket {
    serial: u64,
    pass: PassEvent,
    revisions: PlanRevisions,
    timing: PreparationTiming,
}

impl PlanTicket {
    /// Selected event whose complete window must be evaluated.
    pub const fn pass(self) -> PassEvent {
        self.pass
    }
    /// Inputs to capture with this job, including starting output history.
    pub const fn revisions(self) -> PlanRevisions {
        self.revisions
    }
    /// The planner must evaluate this schedule explicitly.
    pub const fn preparation_timing(self) -> PreparationTiming {
        self.timing
    }
}

/// A fresh execution generation, independent of operator and planning leases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunToken {
    serial: u64,
}

impl RunToken {
    /// Opaque identity required on every executor callback/output request.
    pub const fn serial(self) -> u64 {
        self.serial
    }
}

/// One bounded episode. Replanning never changes this identity or deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoveryToken {
    serial: u64,
    started_at_ms: u64,
    deadline_ms: u64,
    cause: RecoveryCause,
}

/// Why the selected frozen pass entered bounded acquisition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryCause {
    /// Fresh observations established excessive pointing error.
    Deviation,
    /// The frozen preparation had not established readiness at AOS.
    LatePreparation,
    /// The admitted partial plan starts with a validated late intercept.
    LateAcquisition,
    /// The frozen plan scheduled a bounded interruption in reception.
    PlannedGap,
}

impl RecoveryToken {
    /// Boot-local episode identity, distinct from intercept attempts.
    pub const fn serial(self) -> u64 {
        self.serial
    }
    /// Entry instant, including all settling, planning and retry time.
    pub const fn started_at_ms(self) -> u64 {
        self.started_at_ms
    }
    /// Exclusive deadline, the earlier of 30 seconds after entry and LOS.
    pub const fn deadline_ms(self) -> u64 {
        self.deadline_ms
    }
    /// Distinguishes a missed start from loss of following during the pass.
    pub const fn cause(self) -> RecoveryCause {
        self.cause
    }
}

/// Mode controls command-source admission as well as eventual UI visibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackingMode {
    /// Leased manual commands may be admitted; selection alone does not track.
    Manual,
    /// Exactly one armed pass; manual commands are refused.
    Tracking,
}

/// Device-owned phases, independent of browser presence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackingPhase {
    /// No armed run; an optional selection may be previewed.
    Idle,
    /// Waiting for a current supported plan and its preparation time.
    Armed,
    /// Execution inputs frozen; acquire the first route position.
    Preparing,
    /// Executor has confirmed preparation; hold until AOS.
    Ready,
    /// Follow the frozen route until LOS.
    Following,
    /// Hold or follow a validated future intercept within one bounded episode.
    Reacquiring,
    /// LOS serviced; outputs inhibited and mode returned to Manual.
    Complete,
    /// Explicit operator cancellation or switch to Manual.
    Cancelled,
    /// Interrupted with an explicit terminal reason; no automatic restart.
    Stopped,
}

/// Why planning cannot currently authorize preparation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanProblem {
    /// The source snapshot needs replacement before preparation can begin.
    Inputs(InputError),
    /// The original selected event cannot be uniquely associated.
    Event(EventError),
    /// The complete search found no supported route.
    Unsupported(Option<CandidateFailure>),
    /// Incremental planning itself failed.
    Planning(PlanningError),
    /// Result disagreed with its event, schedule or explicit reception contract.
    InvalidResult,
    /// Admission validity expired before preparation could freeze the plan.
    Expired,
    /// Insufficient preparation time or a missed fixed partial-acquisition entry.
    TooLate,
}

/// Feasibility publication separate from the run's mode/phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanStatus {
    /// Awaiting a complete input/forecast publication for this selection.
    Unplanned,
    /// The newest job is in progress; an older plan cannot start preparation.
    Planning,
    /// Complete, supported result available for preparation.
    Supported,
    /// Validated partial reception, with an explicit missed interval and suffix.
    SupportedPartial,
    /// Frozen result belongs to the running executor.
    Frozen,
    /// Explicit reason no preparation is permitted.
    Blocked(PlanProblem),
}

/// Terminal causes; none request a park or another pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackingStop {
    /// Normal end of the reception window.
    LossOfSignal,
    /// Current leased operator cancelled.
    Cancelled,
    /// Current leased operator selected Manual mode.
    ManualMode,
    /// A replacement selection stopped the preceding run first.
    Replaced,
    /// Installed calibration, travel or motor policy changed during execution.
    InstallationChanged,
    /// Preparation was not established before AOS.
    LatePreparation,
    /// Device link/address permission was lost; browser loss is separate.
    NetworkUnavailable,
    /// Frozen plan data could not be evaluated.
    SourceInvalid,
    /// Frozen orbital or clock input failed its execution validity checks, including
    /// timing uncertainty, sample age and a new correction.
    Inputs(InputError),
    /// A physical output fault, independently enforced by the actuator.
    Motion(MotionFault),
    /// Feedback is missing, stale, invalid or out of timestamp order.
    Feedback(FeedbackHealth),
    /// The device follower missed the maximum interval evaluated by its plan.
    ExecutionDeadline,
    /// Recovery exhausted its original 30-second episode budget.
    ReacquisitionTimeout,
    /// LOS arrived while the dish was still reacquiring.
    ReacquisitionLossOfSignal,
    /// Monotonic clock/counter failure; cannot safely continue or reuse identities.
    ClockOrCounter,
}

/// Last terminal event, retained while previewing another pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackingTermination {
    /// Selection that was stopped, when any.
    pub event: Option<EventId>,
    /// Frozen run that was stopped, when any.
    pub run: Option<RunToken>,
    /// Explicit cause, independent of operator presence.
    pub reason: TrackingStop,
    /// Service instant when inhibition was requested.
    pub at_ms: u64,
}

/// Small read-only status; reading never renews ownership or performs transitions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackingSnapshot {
    /// Required by select, arm and prepare-now mutations.
    pub revision: u64,
    /// Current admitted command source.
    pub mode: TrackingMode,
    /// Device-owned execution phase.
    pub phase: TrackingPhase,
    /// At most one selected event.
    pub selection: Option<Selection>,
    /// Feasibility of the latest job, never an older successful result.
    pub plan: PlanStatus,
    /// Current execution generation, if frozen.
    pub run: Option<RunToken>,
    /// Current recovery episode, unchanged by intercept retries.
    pub recovery: Option<RecoveryToken>,
    /// Retained completion/cancellation/failure reason.
    pub termination: Option<TrackingTermination>,
}

/// Operator intent. Only selection/arming/preparation require a state revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackingAction {
    /// Preview a pass, replacing and inhibiting any preceding run.
    Select(PassEvent),
    /// Enter Tracking mode using the current supported plan.
    Arm,
    /// Re-evaluate preparation from now before freezing/executing it.
    PrepareNow,
    /// Stop across plan revisions, retaining the last selection for inspection.
    Cancel,
    /// Stop and allow fresh manual input.
    Manual,
}

/// Lease-bound, ordered request. A request number must never be reused for
/// different intent; duplicate or older requests cannot repeat an action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackingRequest {
    /// Current grant, bound to the server's actual transport connection.
    pub operator: OperatorToken,
    /// Strictly increasing nonzero number within this grant.
    pub sequence: u64,
    /// Ignored for Cancel/Manual so those remain effective across revisions.
    pub expected_revision: u64,
    /// Mutation to apply atomically.
    pub action: TrackingAction,
}

/// Admission/ordering failure. It never grants output permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleError {
    /// Wrong/expired operator, or an occupied lease.
    Lease(LeaseError),
    /// Sequence was older, zero or reused for different intent.
    RequestOrder,
    /// State changed since the operator observed it.
    StateRevision,
    /// No selection, no valid plan, or action does not apply to this phase.
    NotReady,
    /// Selection was outside supported forecast bounds.
    InvalidEvent,
    /// An old planning job cannot update or revive this selection.
    StaleJob,
    /// Older input publications cannot supersede newer ones.
    StaleInputs,
    /// Counter/time failure permanently disarmed this instance.
    ClockOrCounter,
}

/// Accepted immutable planning result and the exact inputs used to create it.
/// There is no mutable accessor; freezing retains this value without cloning it.
pub struct BoundTrackingPlan<S> {
    plan: TrackingPlan,
    inputs: S,
    ticket: PlanTicket,
    freeze_before_ms: u64,
}

impl<S> BoundTrackingPlan<S> {
    /// Evaluated route, output policy and assumptions.
    pub const fn plan(&self) -> &TrackingPlan {
        &self.plan
    }
    /// Captured orbital/observer/clock inputs owned by this plan.
    pub const fn inputs(&self) -> &S {
        &self.inputs
    }
    /// Revisions and event binding used for acceptance.
    pub const fn ticket(&self) -> PlanTicket {
        self.ticket
    }
}

#[derive(Clone, Copy)]
struct Receipt {
    request: TrackingRequest,
    result: Result<u64, LifecycleError>,
}

/// One operator lease, one selection, one plan and one output mailbox. Browser
/// loss only removes the operator; tracking requests use a separate execution
/// generation. Keep the final production actuator/network/feedback gates
/// downstream of this policy.
pub struct TrackingLifecycle<S> {
    operator: OperatorLease,
    revision: u64,
    serial: u64,
    failed: bool,
    mode: TrackingMode,
    phase: TrackingPhase,
    selection: Option<Selection>,
    revisions: Option<PlanRevisions>,
    timing: PreparationTiming,
    ticket: Option<PlanTicket>,
    plan: Option<BoundTrackingPlan<S>>,
    plan_status: PlanStatus,
    run: Option<RunToken>,
    recovery: Option<RecoveryToken>,
    planned_recovery_handled: bool,
    armed_deadline_ms: Option<u64>,
    termination: Option<TrackingTermination>,
    receipt: Option<Receipt>,
    commands: CommandMailbox,
}

impl<S> Default for TrackingLifecycle<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> TrackingLifecycle<S> {
    /// Boot Manual, disarmed and without a selection or operator.
    pub const fn new() -> Self {
        Self {
            operator: OperatorLease::new(),
            revision: 0,
            serial: 0,
            failed: false,
            mode: TrackingMode::Manual,
            phase: TrackingPhase::Idle,
            selection: None,
            revisions: None,
            timing: PreparationTiming::Scheduled,
            ticket: None,
            plan: None,
            plan_status: PlanStatus::Unplanned,
            run: None,
            recovery: None,
            planned_recovery_handled: false,
            armed_deadline_ms: None,
            termination: None,
            receipt: None,
            commands: CommandMailbox::new(),
        }
    }

    /// Observe state without performing expiry or reading any external clock.
    pub const fn snapshot(&self) -> TrackingSnapshot {
        TrackingSnapshot {
            revision: self.revision,
            mode: self.mode,
            phase: self.phase,
            selection: self.selection,
            plan: self.plan_status,
            run: self.run,
            recovery: self.recovery,
            termination: self.termination,
        }
    }

    /// Observe a grant without renewing it.
    pub fn operator(&self, now_ms: u64) -> Option<OperatorGrant> {
        self.operator.current(now_ms)
    }

    /// Claim unowned control without taking over a current lease or stopping tracking.
    pub fn claim(&mut self, client_id: u64, now_ms: u64) -> Result<OperatorGrant, LifecycleError> {
        self.advance(now_ms)?;
        let previous = self.operator.current(now_ms);
        let grant = self
            .operator
            .claim(client_id, now_ms)
            .map_err(LifecycleError::Lease)?;
        if previous.is_none() && self.mode == TrackingMode::Manual {
            self.commands.submit(CommandEnvelope::INHIBIT);
        }
        Ok(grant)
    }

    /// Heartbeat exactly the current grant; never reacquires an expired grant.
    pub fn renew(
        &mut self,
        operator: OperatorToken,
        now_ms: u64,
    ) -> Result<OperatorGrant, LifecycleError> {
        self.advance(now_ms)?;
        self.operator
            .renew(operator, now_ms)
            .map_err(LifecycleError::Lease)
    }

    /// Disconnect/release removes authority but only inhibits browser-owned motion.
    pub fn release(
        &mut self,
        operator: OperatorToken,
        now_ms: u64,
    ) -> Result<bool, LifecycleError> {
        self.advance(now_ms)?;
        let released = self
            .operator
            .release(operator, now_ms)
            .map_err(LifecycleError::Lease)?;
        if released && self.mode == TrackingMode::Manual {
            self.commands.submit(CommandEnvelope::INHIBIT);
        }
        Ok(released)
    }

    fn advance(&mut self, now_ms: u64) -> Result<(), LifecycleError> {
        match self.operator.advance(now_ms) {
            Err(_) => {
                self.fail_clock(now_ms);
                Err(LifecycleError::ClockOrCounter)
            }
            _ if self.failed => Err(LifecycleError::ClockOrCounter),
            Ok(expired) => {
                if expired && self.mode == TrackingMode::Manual {
                    self.commands.submit(CommandEnvelope::INHIBIT);
                }
                Ok(())
            }
        }
    }

    fn fail_clock(&mut self, now_ms: u64) {
        if !self.failed {
            self.stop(TrackingStop::ClockOrCounter, TrackingPhase::Stopped, now_ms);
            self.failed = true;
        }
        self.commands.submit(CommandEnvelope::INHIBIT);
    }

    pub(super) fn clock_failure(&mut self, now_ms: u64) -> Result<(), LifecycleError> {
        self.fail_clock(now_ms);
        Err(LifecycleError::ClockOrCounter)
    }

    fn bump(&mut self, now_ms: u64) -> Result<(), LifecycleError> {
        let Some(revision) = self.revision.checked_add(1) else {
            self.fail_clock(now_ms);
            return Err(LifecycleError::ClockOrCounter);
        };
        self.revision = revision;
        Ok(())
    }

    fn serial(&mut self, now_ms: u64) -> Result<u64, LifecycleError> {
        let Some(serial) = self.serial.checked_add(1) else {
            self.fail_clock(now_ms);
            return Err(LifecycleError::ClockOrCounter);
        };
        self.serial = serial;
        Ok(serial)
    }

    fn stop(&mut self, reason: TrackingStop, phase: TrackingPhase, now_ms: u64) {
        self.termination = Some(TrackingTermination {
            event: self.selection.map(|s| s.id),
            run: self.run,
            reason,
            at_ms: now_ms,
        });
        self.mode = TrackingMode::Manual;
        self.phase = phase;
        self.run = None;
        self.recovery = None;
        self.planned_recovery_handled = false;
        self.armed_deadline_ms = None;
        self.ticket = None;
        self.plan = None;
        self.plan_status = PlanStatus::Unplanned;
        self.timing = PreparationTiming::Scheduled;
        self.commands.submit(CommandEnvelope::INHIBIT);
    }

    /// Apply one lease-bound mutation. An exact retry returns its original result
    /// even after automatic revisions; earlier sequence numbers have no effect.
    pub fn request(
        &mut self,
        request: TrackingRequest,
        now_ms: u64,
    ) -> Result<u64, LifecycleError> {
        self.advance(now_ms)?;
        if !self.operator.permits(request.operator, now_ms) {
            return Err(LifecycleError::Lease(LeaseError::Invalid));
        }
        if request.sequence == 0 {
            return Err(LifecycleError::RequestOrder);
        }
        if let Some(receipt) = self
            .receipt
            .filter(|r| r.request.operator == request.operator)
        {
            if receipt.request == request {
                return receipt.result;
            }
            if request.sequence <= receipt.request.sequence {
                return Err(LifecycleError::RequestOrder);
            }
        }
        let result = self.apply(request, now_ms).map(|()| self.revision);
        self.receipt = Some(Receipt { request, result });
        result
    }

    fn apply(&mut self, request: TrackingRequest, now_ms: u64) -> Result<(), LifecycleError> {
        if !matches!(
            request.action,
            TrackingAction::Cancel | TrackingAction::Manual
        ) && request.expected_revision != self.revision
        {
            return Err(LifecycleError::StateRevision);
        }
        match request.action {
            TrackingAction::Select(pass) => {
                if !pass.is_valid() {
                    return Err(LifecycleError::InvalidEvent);
                }
                let id = EventId(self.serial(now_ms)?);
                self.bump(now_ms)?;
                let previous_termination = self.termination;
                let replacing_run = self.mode == TrackingMode::Tracking;
                self.stop(TrackingStop::Replaced, TrackingPhase::Idle, now_ms);
                if !replacing_run {
                    self.termination = previous_termination;
                }
                self.selection = Some(Selection {
                    id,
                    original: pass,
                    current: Some(pass),
                });
                // Orbital publication revisions belong to a satellite. A
                // different selection may legitimately use an older cached
                // publication; installation/clock/start counters still follow.
                if let Some(revisions) = self.revisions.as_mut() {
                    revisions.orbit = 0;
                }
            }
            TrackingAction::Arm => {
                if self.mode != TrackingMode::Manual || !self.armable(now_ms) {
                    return Err(LifecycleError::NotReady);
                }
                self.bump(now_ms)?;
                self.mode = TrackingMode::Tracking;
                self.phase = TrackingPhase::Armed;
                self.armed_deadline_ms =
                    Some(admission_deadline(&self.plan.as_ref().unwrap().plan));
                self.commands.submit(CommandEnvelope::INHIBIT);
            }
            TrackingAction::PrepareNow => {
                if self.phase != TrackingPhase::Armed
                    || self.selection.is_none_or(|s| s.current.is_none())
                {
                    return Err(LifecycleError::NotReady);
                }
                self.timing = PreparationTiming::Immediate;
                self.start_job(now_ms)?;
            }
            TrackingAction::Cancel | TrackingAction::Manual => {
                self.bump(now_ms)?;
                self.stop(
                    if request.action == TrackingAction::Cancel {
                        TrackingStop::Cancelled
                    } else {
                        TrackingStop::ManualMode
                    },
                    TrackingPhase::Cancelled,
                    now_ms,
                );
            }
        }
        Ok(())
    }

    fn armable(&self, now_ms: u64) -> bool {
        matches!(
            self.plan_status,
            PlanStatus::Supported | PlanStatus::SupportedPartial
        ) && self.plan.as_ref().is_some_and(|bound| {
            self.ticket == Some(bound.ticket)
                && now_ms < bound.freeze_before_ms
                && has_preparation_time(&bound.plan, now_ms)
        })
    }

    fn start_job(&mut self, now_ms: u64) -> Result<(), LifecycleError> {
        let selection = self.selection.ok_or(LifecycleError::NotReady)?;
        let pass = selection.current.ok_or(LifecycleError::NotReady)?;
        let revisions = self.revisions.ok_or(LifecycleError::NotReady)?;
        let serial = self.serial(now_ms)?;
        self.bump(now_ms)?;
        self.ticket = Some(PlanTicket {
            serial,
            pass,
            revisions,
            timing: self.timing,
        });
        self.plan = None;
        self.plan_status = PlanStatus::Planning;
        Ok(())
    }

    /// Publish a complete forecast/input revision atomically before servicing
    /// preparation. Refreshes cannot replace frozen inputs; installation changes
    /// stop them instead. `TrackingRuntime` enforces clock/source validity.
    pub fn refresh(
        &mut self,
        forecast: &[PassEvent],
        revisions: PlanRevisions,
        now_ms: u64,
    ) -> Result<(), LifecycleError> {
        self.advance(now_ms)?;
        if self
            .revisions
            .is_some_and(|previous| !revisions.follows(previous))
        {
            return Err(LifecycleError::StaleInputs);
        }
        if self.run.is_some() {
            let frozen = self
                .plan
                .as_ref()
                .expect("run owns its frozen plan")
                .ticket
                .revisions;
            if revisions.calibration != frozen.calibration || revisions.motion != frozen.motion {
                self.bump(now_ms)?;
                self.stop(
                    TrackingStop::InstallationChanged,
                    TrackingPhase::Stopped,
                    now_ms,
                );
            }
            self.revisions = Some(revisions);
            return Ok(());
        }
        let changed = self.revisions != Some(revisions);
        self.revisions = Some(revisions);
        let Some(selection) = self.selection else {
            return Ok(());
        };
        match reconcile_event(selection.original, forecast) {
            Ok(pass) => {
                self.selection.as_mut().unwrap().current = Some(pass);
                if changed || selection.current != Some(pass) || self.ticket.is_none() {
                    self.start_job(now_ms)?;
                }
            }
            Err(error) => {
                let status = PlanStatus::Blocked(PlanProblem::Event(error));
                if self.plan_status != status || changed {
                    self.bump(now_ms)?;
                    self.selection.as_mut().unwrap().current = None;
                    self.ticket = None;
                    self.plan = None;
                    self.plan_status = status;
                }
            }
        }
        Ok(())
    }

    /// Job the worker should evaluate; reading it neither starts nor renews work.
    pub fn planning_ticket(&self) -> Option<PlanTicket> {
        (self.plan_status == PlanStatus::Planning)
            .then_some(self.ticket)
            .flatten()
    }

    /// Borrow the accepted result, including its captured inputs, for inspection.
    pub const fn bound_plan(&self) -> Option<&BoundTrackingPlan<S>> {
        self.plan.as_ref()
    }

    /// The input owner can invalidate a preview without disarming its selection.
    /// A subsequent complete input refresh starts a new job; no stale fallback.
    pub(super) fn invalidate_inputs(
        &mut self,
        error: InputError,
        now_ms: u64,
    ) -> Result<(), LifecycleError> {
        self.advance(now_ms)?;
        if self.run.is_some() {
            return Err(LifecycleError::NotReady);
        }
        self.bump(now_ms)?;
        self.plan = None;
        self.ticket = None;
        self.plan_status = PlanStatus::Blocked(PlanProblem::Inputs(error));
        Ok(())
    }

    /// Publish only the latest completed job. `freeze_before_ms` is an exclusive
    /// admission deadline supplied by the trusted input/clock owner, not a browser.
    /// `TrackingRuntime` validates inputs through LOS as well as before freezing.
    pub fn publish_plan(
        &mut self,
        ticket: PlanTicket,
        result: Result<PlanningReport, PlanningError>,
        inputs: S,
        freeze_before_ms: u64,
        now_ms: u64,
    ) -> Result<(), LifecycleError> {
        self.advance(now_ms)?;
        if self.run.is_some() || self.planning_ticket() != Some(ticket) {
            return Err(LifecycleError::StaleJob);
        }
        self.bump(now_ms)?;
        let report = match result {
            Ok(report) => report,
            Err(error) => {
                self.plan_status = PlanStatus::Blocked(PlanProblem::Planning(error));
                return Ok(());
            }
        };
        let Some(plan) = report.plan else {
            self.plan_status = PlanStatus::Blocked(PlanProblem::Unsupported(
                report.closest_attempt.and_then(|attempt| attempt.failure),
            ));
            return Ok(());
        };
        let freeze_before_ms = freeze_before_ms.min(admission_deadline(&plan));
        let problem = if !valid_plan(&plan, ticket) {
            Some(PlanProblem::InvalidResult)
        } else if now_ms >= freeze_before_ms {
            Some(PlanProblem::Expired)
        } else if !has_preparation_time(&plan, now_ms) {
            Some(PlanProblem::TooLate)
        } else {
            None
        };
        if let Some(problem) = problem {
            self.plan_status = PlanStatus::Blocked(problem);
            return Ok(());
        }
        if self.phase == TrackingPhase::Armed {
            self.armed_deadline_ms = Some(admission_deadline(&plan));
        }
        self.plan_status = if plan.reception().is_complete() {
            PlanStatus::Supported
        } else {
            PlanStatus::SupportedPartial
        };
        self.plan = Some(BoundTrackingPlan {
            plan,
            inputs,
            ticket,
            freeze_before_ms,
        });
        Ok(())
    }

    /// Service monotonic deadlines. Preparation freezes the latest result in one
    /// state transition; subsequent worker results cannot overwrite it.
    pub fn tick(&mut self, now_ms: u64) -> Result<(), LifecycleError> {
        self.advance(now_ms)?;
        if self.phase == TrackingPhase::Armed
            && self
                .armed_deadline_ms
                .is_some_and(|deadline| now_ms >= deadline)
        {
            self.bump(now_ms)?;
            self.stop(
                TrackingStop::LatePreparation,
                TrackingPhase::Stopped,
                now_ms,
            );
            return Ok(());
        }
        if self.run.is_some() {
            let window = self.plan.as_ref().unwrap().plan.window;
            let los = window.aos_at_ms + u64::from(window.duration_ms);
            if self.phase == TrackingPhase::Preparing && now_ms >= window.aos_at_ms {
                self.enter_reacquisition(RecoveryCause::LatePreparation, window.aos_at_ms, now_ms)?;
            }
            if let Some(gap) = self
                .plan
                .as_ref()
                .unwrap()
                .plan
                .reception()
                .gap()
                .filter(|gap| !self.planned_recovery_handled && now_ms >= gap.recovery_at_ms)
            {
                self.planned_recovery_handled = true;
                // An unexpected earlier episode keeps its original deadline.
                if self.recovery.is_none() {
                    self.enter_reacquisition(
                        RecoveryCause::PlannedGap,
                        gap.recovery_at_ms,
                        now_ms,
                    )?;
                }
            }
            if let Some(recovery) = self.recovery.filter(|r| now_ms >= r.deadline_ms) {
                self.bump(now_ms)?;
                self.stop(
                    if recovery.deadline_ms == los {
                        TrackingStop::ReacquisitionLossOfSignal
                    } else {
                        TrackingStop::ReacquisitionTimeout
                    },
                    TrackingPhase::Stopped,
                    now_ms,
                );
            } else if now_ms >= los {
                self.bump(now_ms)?;
                let complete = self.phase == TrackingPhase::Following;
                self.stop(
                    if complete {
                        TrackingStop::LossOfSignal
                    } else {
                        TrackingStop::LatePreparation
                    },
                    if complete {
                        TrackingPhase::Complete
                    } else {
                        TrackingPhase::Stopped
                    },
                    now_ms,
                );
            } else if now_ms >= window.aos_at_ms && self.phase == TrackingPhase::Ready {
                self.bump(now_ms)?;
                self.phase = TrackingPhase::Following;
            }
        } else if let Some(bound) = self.plan.as_ref() {
            if now_ms >= bound.freeze_before_ms {
                self.bump(now_ms)?;
                self.plan = None;
                self.plan_status = PlanStatus::Blocked(PlanProblem::Expired);
            } else if self.phase == TrackingPhase::Armed
                && now_ms >= bound.plan.assessment.prepare_at_ms
            {
                if !self.armable(now_ms) {
                    self.bump(now_ms)?;
                    self.plan = None;
                    self.plan_status = PlanStatus::Blocked(PlanProblem::TooLate);
                } else {
                    let token = RunToken {
                        serial: self.serial(now_ms)?,
                    };
                    self.bump(now_ms)?;
                    self.run = Some(token);
                    self.phase = TrackingPhase::Preparing;
                    self.plan_status = PlanStatus::Frozen;
                    if let Some(gap) = self
                        .plan
                        .as_ref()
                        .unwrap()
                        .plan
                        .reception()
                        .gap()
                        .filter(|gap| gap.late_acquisition)
                    {
                        self.planned_recovery_handled = true;
                        self.enter_reacquisition(
                            RecoveryCause::LateAcquisition,
                            gap.recovery_at_ms,
                            now_ms,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Trusted executor callback after measured readiness/dwell and relay checks.
    /// It is never a browser action. A callback for a cancelled run has no effect.
    pub fn preparation_ready(
        &mut self,
        run: RunToken,
        now_ms: u64,
    ) -> Result<bool, LifecycleError> {
        self.tick(now_ms)?;
        if self.run != Some(run) {
            return Ok(false);
        }
        if self.phase == TrackingPhase::Ready {
            return Ok(true);
        }
        if self.phase != TrackingPhase::Preparing {
            return Ok(false);
        }
        self.bump(now_ms)?;
        self.phase = TrackingPhase::Ready;
        Ok(true)
    }

    /// Trusted deviation monitor enters recovery once; repeated calls retain
    /// the same absolute deadline. Old following commands are inhibited first.
    pub(super) fn begin_reacquisition(
        &mut self,
        run: RunToken,
        now_ms: u64,
    ) -> Result<Option<RecoveryToken>, LifecycleError> {
        self.tick(now_ms)?;
        if self.run != Some(run) {
            return Ok(None);
        }
        if self.recovery.is_some() {
            return Ok(self.recovery);
        }
        if self.phase != TrackingPhase::Following {
            return Ok(None);
        }
        self.enter_reacquisition(RecoveryCause::Deviation, now_ms, now_ms)
            .map(Some)
    }

    fn enter_reacquisition(
        &mut self,
        cause: RecoveryCause,
        started_at_ms: u64,
        now_ms: u64,
    ) -> Result<RecoveryToken, LifecycleError> {
        let window = self.plan.as_ref().unwrap().plan.window;
        let deadline_ms = started_at_ms
            .saturating_add(REACQUISITION_LIMIT_MS)
            .min(window.aos_at_ms + u64::from(window.duration_ms));
        let recovery = RecoveryToken {
            serial: self.serial(now_ms)?,
            started_at_ms,
            deadline_ms,
            cause,
        };
        self.bump(now_ms)?;
        self.recovery = Some(recovery);
        self.phase = TrackingPhase::Reacquiring;
        self.commands.submit(CommandEnvelope::INHIBIT);
        Ok(recovery)
    }

    /// Only the monitor may confirm fresh recovery on the validated intercept.
    pub(super) fn finish_reacquisition(
        &mut self,
        recovery: RecoveryToken,
        now_ms: u64,
    ) -> Result<bool, LifecycleError> {
        self.tick(now_ms)?;
        if self.recovery != Some(recovery) {
            return Ok(false);
        }
        self.bump(now_ms)?;
        self.phase = TrackingPhase::Following;
        self.recovery = None;
        self.commands.submit(CommandEnvelope::INHIBIT);
        Ok(true)
    }

    /// Recovery commands are generated from a validated intercept by the runtime.
    pub(super) fn recovery_command(
        &mut self,
        recovery: RecoveryToken,
        command: CommandEnvelope,
        now_ms: u64,
    ) -> bool {
        if self.tick(now_ms).is_err()
            || self.recovery != Some(recovery)
            || !matches!(command.command, Command::Drive(_) | Command::Inhibit)
            || !fresh(command, now_ms)
        {
            return false;
        }
        self.commands.submit(command);
        true
    }

    /// Physical/network/source interruption is independent of operator authority.
    pub fn interrupt(&mut self, reason: TrackingStop, now_ms: u64) -> Result<(), LifecycleError> {
        self.advance(now_ms)?;
        self.bump(now_ms)?;
        self.stop(reason, TrackingPhase::Stopped, now_ms);
        Ok(())
    }

    /// Admit fresh manual intent only in Manual mode and under the current lease.
    pub fn manual_command(
        &mut self,
        operator: OperatorToken,
        command: CommandEnvelope,
        now_ms: u64,
    ) -> bool {
        if self.tick(now_ms).is_err()
            || self.mode != TrackingMode::Manual
            || !self.operator.permits(operator, now_ms)
            || !matches!(command.command, Command::Jog(_) | Command::Inhibit)
            || !fresh(command, now_ms)
        {
            return false;
        }
        if matches!(command.command, Command::Jog(drive) if drive.is_active())
            && (self.plan.is_some() || self.ticket.is_some())
        {
            if self.bump(now_ms).is_err() {
                return false;
            }
            self.plan = None;
            self.ticket = None;
            self.plan_status = PlanStatus::Unplanned;
        }
        self.commands.submit(command);
        true
    }

    /// Admit a short internal output lease for exactly the current frozen run.
    /// Cancelling/replacing/LOS invalidates old executor work before mailbox drain.
    pub fn tracking_command(
        &mut self,
        run: RunToken,
        command: CommandEnvelope,
        now_ms: u64,
    ) -> bool {
        if self.tick(now_ms).is_err()
            || self.run != Some(run)
            || self.mode != TrackingMode::Tracking
            || self.phase == TrackingPhase::Reacquiring
            || !matches!(command.command, Command::Drive(_) | Command::Inhibit)
            || !fresh(command, now_ms)
        {
            return false;
        }
        self.commands.submit(command);
        true
    }

    /// Drain after deadline/phase checks. An unconsumed stop cannot be overwritten
    /// by any source; no movement expiry is extended while waiting in the mailbox.
    pub fn take_command(&mut self, now_ms: u64) -> Option<CommandEnvelope> {
        let _ = self.tick(now_ms);
        self.commands.take().map(|command| {
            if fresh(command, now_ms) {
                command
            } else {
                CommandEnvelope::INHIBIT
            }
        })
    }
}

fn fresh(command: CommandEnvelope, now_ms: u64) -> bool {
    command.command == Command::Inhibit
        || (command.valid_until_ms > now_ms && command.valid_until_ms - now_ms <= MOVEMENT_LEASE_MS)
}

fn admission_deadline(plan: &TrackingPlan) -> u64 {
    plan.reception()
        .gap()
        .filter(|gap| gap.late_acquisition)
        .map_or(plan.window.aos_at_ms, |gap| {
            gap.recovery_at_ms.saturating_add(1)
        })
}

fn has_preparation_time(plan: &TrackingPlan, now_ms: u64) -> bool {
    if let Some(gap) = plan.reception().gap().filter(|gap| gap.late_acquisition) {
        now_ms <= gap.recovery_at_ms
    } else {
        now_ms
            .checked_add(plan.assessment.preparation_ms)
            .is_some_and(|ready| ready < plan.window.aos_at_ms)
    }
}

fn valid_plan(plan: &TrackingPlan, ticket: PlanTicket) -> bool {
    let window = plan.window;
    let assessment = &plan.assessment;
    let allocation = plan.config.tolerance_deg - plan.config.uncertainty_deg;
    window.aos_unix_s.is_finite()
        && (window.aos_unix_s - ticket.pass.aos_unix_s as f64).abs() <= 1.0
        && (window.aos_unix_s + f64::from(window.duration_ms) / 1000.0
            - ticket.pass.los_unix_s as f64)
            .abs()
            <= 1.0
        && window.duration_ms > 0
        && window.duration_ms <= 1_800_000
        && window
            .aos_at_ms
            .checked_add(u64::from(window.duration_ms))
            .is_some()
        && plan.route.duration_ms() == window.duration_ms
        && plan.route.point_count() >= 2
        && plan.config.preparation_timing == ticket.timing
        && plan.actuator.is_valid()
        && plan
            .calibration
            .validate_for_adc(crate::ads1015::FEEDBACK_FULL_SCALE)
            .is_ok()
        && plan.route.points.iter().all(|point| {
            crate::position::Pointing::new(point.az, point.el).is_valid_for(plan.calibration)
        })
        && plan.config.tolerance_deg.is_finite()
        && (0.1..=10.0).contains(&plan.config.tolerance_deg)
        && plan.config.uncertainty_deg.is_finite()
        && plan.config.uncertainty_deg >= 0.0
        && assessment.choice == plan.route.choice()
        && match plan.reception() {
            ReceptionCoverage::Complete => {
                assessment.prepare_at_ms <= window.aos_at_ms
                    && assessment.bounded_peak_deg <= allocation
                    && assessment.gap_time_ms == 0
                    && assessment.gaps.is_empty()
            }
            ReceptionCoverage::Partial(gap) => {
                plan.config.admission == AdmissionPolicy::AllowPartial
                    && plan.reception().valid(window)
                    && assessment.gaps.as_slice()
                        == [super::PlannedGap {
                            start_ms: gap.missed.start_ms,
                            end_ms: gap.missed.end_ms,
                        }]
                    && assessment.gap_time_ms == u64::from(gap.missed.end_ms - gap.missed.start_ms)
                    && if gap.late_acquisition {
                        assessment.prepare_at_ms == gap.recovery_at_ms
                            && assessment.preparation_ms == 0
                    } else {
                        assessment.prepare_at_ms <= window.aos_at_ms
                    }
            }
        }
        && assessment.failure.is_none()
        && assessment.failure_scenario.is_none()
        && assessment.failure_phase.is_none()
        && !plan.config.scenarios.is_empty()
        && assessment.completed_scenarios == plan.config.scenarios.len()
        && allocation.is_finite()
        && allocation > 0.0
        && assessment.bounded_peak_deg.is_finite()
        && assessment.bounded_peak_deg >= 0.0
        && assessment.sampled_peak_deg.is_finite()
        && assessment.sampled_peak_deg >= 0.0
        && assessment.sampled_peak_deg <= assessment.bounded_peak_deg
        && !assessment.gaps_truncated
}
