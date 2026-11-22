//! Background planning for the selected pass. Firmware polls one
//! [`PlanningWorker`] from its own task and yields between units; host tests
//! poll the same worker against a simulated rotator. A browser cannot supply
//! revisions, source data, motor policy or a start pose.

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
        if self
            .job
            .as_ref()
            .is_some_and(|job| Some(job.source().inputs().ticket()) != ticket)
        {
            self.job = None;
        }
        if state.tracking_operation(now(), |runtime, observation| {
            runtime.step_reacquisition(observation).unwrap_or(false)
        }) {
            return PlanningUnit::Recovery;
        }
        if let Some(ticket) = ticket.filter(|_| self.job.is_none()) {
            self.start(state, ticket, &source, &now);
        }
        let Some(job) = self.job.as_mut() else {
            return PlanningUnit::Idle;
        };
        let _ = job.step();
        if job.is_finished() {
            let inputs = job.source().inputs().clone();
            let result = job.take_report();
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
            Ok(planner) => self.job = Some(planner),
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
        let inputs = state.tracking_operation(now(), |_, observation| {
            let anchor = observation.clock.anchor().ok_or(TimingError::Unavailable)?;
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
                observation.clock,
                window,
                &config,
                observation.now_ms,
            )
        })?;
        Ok(Some((inputs, start, config)))
    }
}
