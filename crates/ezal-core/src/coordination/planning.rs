//! Background planning for the selected pass. Firmware polls one
//! [`PlanningWorker`] from its own task and yields between units; host tests
//! poll the same worker against a simulated rotator. Offload uses a device-issued
//! immutable snapshot; browsers supply only a reproducible candidate to recheck.

use heapless::Vec;

use super::CoordinationState;
use crate::catalog::OrbitalSnapshot;
use crate::passes::MAX_PASSES;
use crate::position::PositionCalibration;
use crate::tracking::{
    execution_window, ExecutionInputs, InputError, InputSettings, PassEvent, PlanRevisions,
    PlanTicket, Planner, PlanningConfig, PlanningTrack, RestingStart, TimingError,
};

/// One satellite's elements and the forecast published with them.
pub type TrackingSource = (OrbitalSnapshot, Vec<PassEvent, MAX_PASSES>);

/// What one [`PlanningWorker::poll`] spent its unit of work on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanningUnit {
    /// Nothing is pending; the caller may sleep.
    Idle,
    /// One reacquisition step, which takes precedence over planning.
    Recovery,
    /// One planner step, including starting or publishing its job.
    Planning,
}

/// Owns the single planning job for the lifecycle's latest ticket.
pub struct PlanningWorker {
    calibration: PositionCalibration,
    config: PlanningConfig,
    settings: InputSettings,
    job: Option<Planner<PlanningTrack>>,
    verifying: bool,
}

impl CoordinationState {
    /// Publish the selected satellite's forecast with the orbital and
    /// starting-state revisions current now. A clock sample is not one: it
    /// only checks the plan's timeline, so synchronizing never restarts it. Called on every forecast
    /// publication and periodically by the planning task.
    pub fn refresh_tracking(
        &mut self,
        source: impl FnOnce(u64) -> Option<TrackingSource>,
        now_ms: u64,
    ) {
        let Some(selected) = self.tracking.snapshot().selection else {
            return;
        };
        let Some((orbit, events)) = source(selected.original.norad_id) else {
            return;
        };
        let start = self.start_revision;
        self.tracking_operation(now_ms, |runtime, observation| {
            let revisions = PlanRevisions {
                orbit: orbit.publication().revision,
                start,
            };
            let _ = runtime.refresh(&events, revisions, observation);
        });
    }
}

impl PlanningWorker {
    /// Plan with the installed calibration, motion profile and station.
    pub fn new(
        calibration: PositionCalibration,
        config: PlanningConfig,
        settings: InputSettings,
    ) -> Self {
        Self {
            calibration,
            config,
            settings,
            job: None,
            verifying: false,
        }
    }

    /// Perform one bounded unit: a reacquisition step when recovery has one,
    /// otherwise start, step or publish the job for the latest ticket. A job
    /// for an older ticket is dropped unpublished. `now` reads the monotonic
    /// clock, because a step takes real time before its result is published.
    pub fn poll(
        &mut self,
        state: &mut CoordinationState,
        source: impl Fn(u64) -> Option<TrackingSource>,
        now: impl Fn() -> u64,
    ) -> PlanningUnit {
        let ticket = state.tracking().planning_ticket();
        let at = now();
        if let Some(ticket) = ticket {
            state.planning.begin(
                ticket,
                state.tracking().snapshot().selection.map(|s| s.id.serial()),
                at,
            );
        }

        if self
            .job
            .as_ref()
            .is_some_and(|job| Some(job.source().inputs().ticket()) != ticket)
        {
            self.job = None;
            self.verifying = false;
        }
        if state.tracking_operation(now(), |runtime, observation| {
            runtime.step_reacquisition(observation).unwrap_or(false)
        }) {
            return PlanningUnit::Recovery;
        }
        // A receipt cannot replace ongoing verification or consume repeated work.
        if !self.verifying {
            if let Some(upload) = state.planning.upload.take() {
                if let Some(job) = self.job.as_mut() {
                    if Some(upload.job) == ticket.map(PlanTicket::serial)
                        && job.queue_verification(upload)
                    {
                        self.verifying = true;
                        state.planning.reset_work(at);
                    }
                }
            }
        }
        if let Some(ticket) = ticket.filter(|_| self.job.is_none()) {
            self.start(state, ticket, &source, &now);
        }
        let Some(job) = self.job.as_mut() else {
            return PlanningUnit::Idle;
        };
        let _ = job.step();
        let verification_result = job.take_verification_result();
        if verification_result == Some(false) {
            self.verifying = false;
            state.planning.offer_len = 0;
            state.planning.reset_work(now());
            if let Some(status) = &mut state.planning.status {
                status.upload_error = true;
                status.offload_ready = false;
            }
        }
        let progress = job.progress();
        state.planning.observe(progress, self.verifying, now());
        // Geometry has visited the entire window, so every minute knot is now
        // cached. Preparing the offer performs no additional SGP4 propagation.
        if state.planning.offer_len == 0
            && progress.phase != "geometry"
            && !job.is_finished()
            && !state.planning.status.is_some_and(|s| s.upload_error)
        {
            if let Some(track) = job.source().prepare() {
                let inputs = job.source().inputs();
                let mut config = self.config.clone();
                config.preparation_timing = inputs.ticket().preparation_timing();
                let offer = PlanningInput {
                    job: inputs.ticket().serial(),
                    track,
                    window: inputs.window(),
                    calibration: self.calibration,
                    start: job.resting_start(),
                    config,
                };
                state.planning.offer_len = offer.encode(&mut state.planning.offer).unwrap_or(0);
                if let Some(status) = &mut state.planning.status {
                    status.offload_ready = state.planning.offer_len > 0 && !status.upload_error;
                }
            }
        }
        if job.is_finished() {
            let inputs = job.source().inputs().clone();
            let result = job.take_report();
            self.verifying = false;
            if let Some(status) = &mut state.planning.status {
                status.offload_ready = false;
                // A queued receipt may lose the race to local completion.
                status.verifying = verification_result == Some(true);
            }
            self.job = None;
            state.tracking_operation(now(), |runtime, observation| {
                let _ = runtime.publish_plan(inputs.ticket(), result, inputs, observation);
            });
        }
        PlanningUnit::Planning
    }

    /// Build the ticket's planner in place, or publish why it cannot plan.
    /// Nothing is published while the start pose is not yet resting; a later
    /// poll tries again. Writing the planner straight into `self.job` keeps
    /// its workspace from taking a second copy in a caller's stack frame.
    fn start(
        &mut self,
        state: &mut CoordinationState,
        ticket: PlanTicket,
        source: impl Fn(u64) -> Option<TrackingSource>,
        now: impl Fn() -> u64,
    ) {
        let (inputs, start, config) = match self.capture(state, ticket, source, &now) {
            Ok(Some(captured)) => captured,
            Ok(None) => return,
            Err(error) => {
                state.tracking_operation(now(), |runtime, observation| {
                    let _ = runtime.reject_inputs(ticket, error, observation);
                });
                return;
            }
        };
        match Planner::new(
            PlanningTrack::new(inputs.clone()),
            inputs.window(),
            self.calibration,
            start,
            config,
        ) {
            Ok(planner) => {
                self.job = Some(planner);
            }
            Err(error) => state.tracking_operation(now(), |runtime, observation| {
                let _ = runtime.publish_plan(ticket, Err(error), inputs, observation);
            }),
        }
    }

    fn capture(
        &self,
        state: &mut CoordinationState,
        ticket: PlanTicket,
        source: impl Fn(u64) -> Option<TrackingSource>,
        now: impl Fn() -> u64,
    ) -> Result<Option<(ExecutionInputs, RestingStart, PlanningConfig)>, InputError> {
        let Some((orbit, _)) = source(ticket.pass().norad_id) else {
            return Err(InputError::MissingOrbit);
        };
        let Some(start) = state.resting_start() else {
            return Ok(None);
        };
        let config = PlanningConfig {
            preparation_timing: ticket.preparation_timing(),
            ..self.config.clone()
        };
        let inputs = state.tracking_operation(now(), |runtime, observation| {
            let clock = runtime.planning_clock(observation.clock);
            let anchor = clock.clock().anchor().ok_or(TimingError::Unavailable)?;
            let pass = ticket.pass();
            let window = execution_window(
                anchor,
                pass.aos_unix_s as u64 * 1000,
                pass.los_unix_s as u64 * 1000,
            )?;
            ExecutionInputs::capture(
                ticket,
                orbit,
                self.settings,
                clock,
                window,
                &config,
                observation.now_ms,
            )
        })?;
        Ok(Some((inputs, start, config)))
    }
}

use crate::tracking::offload::{PlanningInput, UploadedPlan, INPUT_CAPACITY};
use crate::tracking::PlannerProgress;

/// UI diagnostics for exactly one immutable planning ticket.
#[derive(Clone, Copy, Debug)]
#[allow(missing_docs)]
pub struct PlanningStatus {
    pub job: u64,
    pub started_at_ms: u64,
    pub updated_at_ms: u64,
    pub last_progress_at_ms: u64,
    pub restarts: u32,
    pub attempt: u32,
    pub restart_reason: &'static str,
    pub work: Option<PlannerProgress>,
    pub verifying: bool,
    pub offload_ready: bool,
    pub upload_error: bool,
    pub eta_ms: Option<u64>,
    pub upper_ms: Option<u64>,
}

pub(super) struct PlanningState {
    ticket: Option<PlanTicket>,
    selection: Option<u64>,
    status: Option<PlanningStatus>,
    offer: [u8; INPUT_CAPACITY],
    offer_len: usize,
    upload: Option<UploadedPlan>,
    work_started_at_ms: u64,
}
impl PlanningState {
    pub const fn new() -> Self {
        Self {
            ticket: None,
            selection: None,
            status: None,
            offer: [0; INPUT_CAPACITY],
            offer_len: 0,
            upload: None,
            work_started_at_ms: 0,
        }
    }
    fn begin(&mut self, ticket: PlanTicket, selection: Option<u64>, now: u64) {
        if self.ticket == Some(ticket) {
            return;
        }
        let restarts = if selection == self.selection {
            self.status.map_or(0, |s| s.restarts.saturating_add(1))
        } else {
            0
        };
        let reason = match self.ticket {
            Some(old) if old.revisions().orbit != ticket.revisions().orbit => {
                "orbital data changed"
            }
            Some(old) if old.revisions().start != ticket.revisions().start => {
                "starting pose or motor history changed"
            }
            Some(old) if old.preparation_timing() != ticket.preparation_timing() => {
                "preparation schedule changed"
            }
            Some(_) => "selected window or timing inputs changed",
            None => "new selection",
        };
        self.ticket = Some(ticket);
        self.selection = selection;
        self.offer_len = 0;
        self.upload = None;
        self.work_started_at_ms = now;
        self.status = Some(PlanningStatus {
            job: ticket.serial(),
            started_at_ms: now,
            updated_at_ms: now,
            last_progress_at_ms: now,
            restarts,
            attempt: 1,
            restart_reason: reason,
            work: None,
            verifying: false,
            offload_ready: false,
            upload_error: false,
            eta_ms: None,
            upper_ms: None,
        });
    }
    fn reset_work(&mut self, now: u64) {
        // Verification shares the planner's cumulative counters. Keep their
        // original time origin when switching between local work and validation.
        if let Some(status) = &mut self.status {
            status.attempt = status.attempt.saturating_add(1);
            status.work = None;
            status.verifying = false;
            status.updated_at_ms = now;
            status.last_progress_at_ms = now;
            status.eta_ms = None;
            status.upper_ms = None;
        }
    }
    fn observe(&mut self, work: PlannerProgress, verifying: bool, now: u64) {
        if let Some(status) = &mut self.status {
            if status.work.is_none_or(|p| p.work_steps != work.work_steps) {
                status.last_progress_at_ms = now;
            }
            status.updated_at_ms = now;
            status.work = Some(work);
            status.verifying = verifying;
            let elapsed = now.saturating_sub(self.work_started_at_ms);
            if elapsed >= 1000 && work.work_steps > 0 {
                status.upper_ms = Some(
                    elapsed.saturating_mul(
                        crate::tracking::MAX_PLANNING_STEPS.saturating_sub(work.work_steps),
                    ) / work.work_steps,
                );
                // Early failures make candidate costs variable. This is an estimate,
                // only after two candidates, not a percentage or promised deadline.
                if !verifying && work.candidates_done >= 2 {
                    let total = work.candidates_total;
                    status.eta_ms = Some(
                        elapsed.saturating_mul(total.saturating_sub(work.candidates_done) as u64)
                            / work.candidates_done as u64,
                    );
                }
            }
        }
    }
}
impl CoordinationState {
    /// Current job diagnostics; reading does not renew work or operator authority.
    pub fn planning_status(&self) -> Option<PlanningStatus> {
        let pending = self.tracking().planning_ticket();
        let ticket = pending.or_else(|| self.tracking().bound_plan().map(|p| p.ticket()))?;
        self.planning.status.filter(|s| s.job == ticket.serial())
    }
    /// Read an immutable input chunk. A job replacement aborts an in-flight copy.
    pub fn planning_chunk(
        &self,
        job: u64,
        offset: usize,
        out: &mut [u8],
    ) -> Option<(usize, usize)> {
        if self.tracking().planning_ticket()?.serial() != job
            || self.planning.ticket?.serial() != job
            || self.planning.offer_len == 0
            || offset > self.planning.offer_len
        {
            return None;
        }
        let count = out.len().min(self.planning.offer_len - offset);
        out[..count].copy_from_slice(&self.planning.offer[offset..offset + count]);
        Some((count, self.planning.offer_len))
    }
    /// Queue one browser candidate for independent validation. Transport must
    /// authorize the current connection and observation before calling this.
    pub(crate) fn upload_plan(&mut self, plan: UploadedPlan) -> bool {
        if self.tracking().planning_ticket().map(PlanTicket::serial) != Some(plan.job)
            || self.planning.ticket.map(PlanTicket::serial) != Some(plan.job)
            || self.planning.offer_len == 0
            || self.planning.upload.is_some()
            || self
                .planning
                .status
                .is_some_and(|s| s.verifying || s.upload_error)
        {
            return false;
        }
        self.planning.upload = Some(plan);
        true
    }
}
