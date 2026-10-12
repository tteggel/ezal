//! Finite future-intercept search. Each candidate slews to a future point on
//! the frozen mechanical route, then rejoins that route at its original time.
//! Every configured scenario must complete the remaining pass and final coast
//! using the copied production actuator, including its existing duty history.

use super::{
    pointing_deviation_deg, FollowingController, RecoveryToken, RestingStart, Simulation,
    SimulationSettings, TrackingPlan, TrackingTarget, MAX_PLANNING_STEPS, REACQUISITION_DWELL_MS,
    REACQUISITION_TOLERANCE_DEG,
};
use crate::feedback::FEEDBACK_MAX_AGE_MS;
use crate::passes::Ephemeris;
use crate::position::{Pointing, PositionCalibration};

/// Borrows either a frozen plan or a planner candidate. Constructing this does
/// not certify a candidate: only a completed search establishes recovery.
#[derive(Clone, Copy)]
pub(super) struct SearchRoute<'a> {
    pub route: &'a super::Route,
    pub window: super::PlanningWindow,
    pub config: &'a super::PlanningConfig,
    pub calibration: PositionCalibration,
    pub actuator: crate::actuator::ActuatorConfig,
}

impl<'a> From<&'a TrackingPlan> for SearchRoute<'a> {
    fn from(plan: &'a TrackingPlan) -> Self {
        Self {
            route: &plan.route,
            window: plan.window,
            config: &plan.config,
            calibration: plan.calibration,
            actuator: plan.actuator,
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct RecoveryMetrics {
    pub peak: f32,
    pub bound: f32,
    pub energized_ms: [u64; 2],
    pub reversals: u64,
    pub transitions: u64,
}

/// Initial lead for incremental planning while the dish remains inhibited.
pub const INTERCEPT_PLANNING_LEAD_MS: u64 = 2_000;
/// Finite search resolution. “Earliest” refers to these candidates, not a
/// proof that every possible path or intermediate arrival time was considered.
pub const INTERCEPT_GRID_MS: u64 = 1_000;
/// Wait before retrying an unsuccessful search, inside the original episode.
pub const INTERCEPT_RETRY_MS: u64 = 1_000;

/// Why recovery is holding instead of executing an intercept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryHold {
    /// Inhibit, coast and acquire a settled feedback pair before searching.
    Settling,
    /// No candidate met the remaining-pass constraints in every scenario.
    NoIntercept,
    /// The bounded job exhausted its computation allowance.
    WorkLimit,
    /// Planning or the first execution step missed the validated launch instant.
    MissedLaunch,
    /// Pose or actuator history changed while the job was being evaluated.
    ChangedStart,
    /// The validated intercept did not establish measured recovery on time.
    MissedIntercept,
    /// Recovery control stopped meeting the frozen simulation cadence.
    MissedControlDeadline,
}

/// Device telemetry for a bounded episode; reads never extend its deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoveryStatus {
    /// Same token for all retries in the episode.
    pub episode: RecoveryToken,
    /// Number of searches begun, including unsuccessful ones.
    pub attempts: u32,
    /// Incremental search is active; outputs remain inhibited.
    pub planning: bool,
    /// Validated absolute launch time. The output owner must wake at this time.
    pub launch_at_ms: Option<u64>,
    /// Earliest validated future route join in this search, not measured lock.
    pub intercept_at_ms: Option<u64>,
    /// Why the dish is currently holding, if no intercept is available.
    pub hold: Option<RecoveryHold>,
    /// Work performed in the current/most recent search.
    pub work_steps: u64,
}

pub(super) struct Recovery {
    pub status: RecoveryStatus,
    pub search: Option<InterceptSearch>,
    pub intercept: Option<Intercept>,
    pub follower: Option<FollowingController>,
    pub retry_at_ms: u64,
    pub last_command_at_ms: Option<u64>,
    pub launch_pending: bool,
}

impl Recovery {
    pub fn new(episode: RecoveryToken) -> Self {
        Self {
            status: RecoveryStatus {
                episode,
                attempts: 0,
                planning: false,
                launch_at_ms: None,
                intercept_at_ms: None,
                hold: Some(RecoveryHold::Settling),
                work_steps: 0,
            },
            search: None,
            intercept: None,
            follower: None,
            retry_at_ms: episode.started_at_ms(),
            last_command_at_ms: None,
            launch_pending: false,
        }
    }

    pub fn hold(&mut self, reason: RecoveryHold, now: u64) {
        self.search = None;
        self.intercept = None;
        self.follower = None;
        self.last_command_at_ms = None;
        self.launch_pending = false;
        self.status.planning = false;
        self.status.launch_at_ms = None;
        self.status.intercept_at_ms = None;
        self.status.hold = Some(reason);
        self.retry_at_ms = now.saturating_add(INTERCEPT_RETRY_MS);
    }
}

#[derive(Clone, Copy)]
pub(super) struct Intercept {
    pub start: RestingStart,
    pub launch_at_ms: u64,
    pub join_at_ms: u64,
    pub pointing: Pointing,
}

impl Intercept {
    /// While planning, neither hidden motion nor a changed measured pose may
    /// invalidate the start. Continuous idle and used-duty checks also catch an
    /// intervening movement that returned to the same ADC code before launch.
    pub fn start_unchanged(&self, current: RestingStart) -> bool {
        if current.at_ms != self.launch_at_ms
            || current.feedback.position() != self.start.feedback.position()
            || current.actuator.configuration() != self.start.actuator.configuration()
            || current.network.generation() != self.start.network.generation()
            || !current.network.ready(current.at_ms)
            || current
                .at_ms
                .checked_sub(current.feedback.sampled_at_ms())
                .is_none_or(|age| age >= FEEDBACK_MAX_AGE_MS)
        {
            return false;
        }
        let Some((old_used, old_idle)) = self.start.actuator.resting_history(self.start.at_ms)
        else {
            return false;
        };
        let Some((used, idle)) = current.actuator.resting_history(current.at_ms) else {
            return false;
        };
        let elapsed = current.at_ms - self.start.at_ms;
        (0..2).all(|axis| {
            used[axis] <= old_used[axis] && idle[axis] >= old_idle[axis].saturating_add(elapsed)
        })
    }

    pub fn target(&self, plan: &TrackingPlan, now: u64) -> Option<Pointing> {
        if now < self.launch_at_ms {
            return None;
        }
        if now < self.join_at_ms {
            return Some(self.pointing);
        }
        route_at(plan, now)
    }
}

/// Does not copy the large frozen route or SGP4 inputs. The runtime supplies
/// the same immutable bound plan/source on every step, and discards this job
/// on cancellation, episode change or expiry.
pub(super) struct InterceptSearch {
    start: RestingStart,
    launch_at_ms: u64,
    deadline_ms: u64,
    join_at_ms: u64,
    scenario: usize,
    sim: Option<Simulation>,
    previous: Option<(u64, f32, Pointing)>,
    pub work_steps: u64,
    pub result: Option<Intercept>,
    pub finished: bool,
    pub hold: RecoveryHold,
    pub metrics: RecoveryMetrics,
}

impl InterceptSearch {
    pub fn new<'a>(
        plan: impl Into<SearchRoute<'a>>,
        start: RestingStart,
        deadline_ms: u64,
    ) -> Option<Self> {
        let plan = plan.into();
        let launch_at_ms = start.at_ms.checked_add(INTERCEPT_PLANNING_LEAD_MS)?;
        let join_at_ms = launch_at_ms.checked_add(INTERCEPT_GRID_MS)?;
        let follower = FollowingController::new(
            plan.config.reference,
            plan.config.deadband_deg,
            plan.actuator.relays,
        )
        .ok()?;
        if plan.actuator != start.actuator.configuration()
            || plan.config.scenarios.is_empty()
            || start.at_ms < plan.window.aos_at_ms
            || !start.network.ready(start.at_ms)
        {
            return None;
        }
        // Besides verifying settlement, this validates the actual snapshot's
        // feedback age, duty accounting and post-coast acquisition timestamp.
        for case in &plan.config.scenarios {
            Simulation::from_rest(
                plan.calibration,
                follower,
                SimulationSettings {
                    step_ms: plan.config.step_ms,
                    scenario: *case,
                },
                start,
            )
            .ok()?;
        }
        Some(Self {
            start,
            launch_at_ms,
            deadline_ms,
            join_at_ms,
            scenario: 0,
            sim: None,
            previous: None,
            work_steps: 0,
            result: None,
            finished: false,
            hold: RecoveryHold::NoIntercept,
            metrics: RecoveryMetrics::default(),
        })
    }

    pub fn launch_at_ms(&self) -> u64 {
        self.launch_at_ms
    }

    /// At most one propagation and one simulation step. `Err(())` denotes a
    /// frozen-source/rate failure, which stops the run rather than retrying it.
    pub fn step<'a, E: Ephemeris>(
        &mut self,
        plan: impl Into<SearchRoute<'a>>,
        source: &E,
    ) -> Result<(), ()> {
        let plan = plan.into();
        if self.finished {
            return Ok(());
        }
        if self.work_steps >= MAX_PLANNING_STEPS {
            self.hold = RecoveryHold::WorkLimit;
            self.finished = true;
            return Ok(());
        }
        self.work_steps += 1;
        // Leave time for two seconds of fresh measured recovery before expiry.
        if self
            .join_at_ms
            .saturating_add(REACQUISITION_DWELL_MS + FEEDBACK_MAX_AGE_MS)
            >= self.deadline_ms
        {
            self.finished = true;
            return Ok(());
        }
        let Some(pointing) = route_at(plan, self.join_at_ms) else {
            return Err(());
        };
        let case = plan.config.scenarios[self.scenario];
        if self.sim.is_none() {
            let follower = FollowingController::new(
                plan.config.reference,
                plan.config.deadband_deg,
                plan.actuator.relays,
            )
            .map_err(|_| ())?;
            let sim = Simulation::from_rest(
                plan.calibration,
                follower,
                SimulationSettings {
                    step_ms: plan.config.step_ms,
                    scenario: case,
                },
                self.start,
            )
            .map_err(|_| ())?;
            self.sim = Some(sim);
        }
        let sim = self.sim.as_mut().unwrap();
        let now = sim.now_ms();
        let los = plan.window.aos_at_ms + u64::from(plan.window.duration_ms);
        let sky = if now <= los {
            let look = source
                .look_at(plan.window.aos_unix_s + (now - plan.window.aos_at_ms) as f64 / 1000.0)
                .ok_or(())?;
            let sky = Pointing::new(look.azimuth_deg as f32, look.elevation_deg as f32);
            if !sky.is_valid() {
                return Err(());
            }
            Some(sky)
        } else {
            None
        };
        let target = if now >= self.launch_at_ms && now < los {
            Some(TrackingTarget {
                mechanical: if now < self.join_at_ms {
                    pointing
                } else {
                    route_at(plan, now).ok_or(())?
                },
                sky: sky.ok_or(())?,
            })
        } else {
            None
        };
        let sample = match sim.step_target(target) {
            Ok(sample) => sample,
            Err(_) => {
                self.reject_candidate();
                return Ok(());
            }
        };
        if let Some(sky) = sky {
            let error = pointing_deviation_deg(sample.position, sky).ok_or(())?;
            let mut bound = error + 0.002;
            let mut whole_bound = bound;
            if let Some((previous_ms, previous_error, previous_sky)) = self.previous {
                let dt = (now - previous_ms) as f32 / 1000.0;
                if pointing_deviation_deg(previous_sky, sky).ok_or(())?
                    > plan.config.sky_rate_bound_deg_per_s * dt + 0.002
                {
                    return Err(());
                }
                let az = case.motor.azimuth.rate_deg_per_s;
                let el = case.motor.elevation.rate_deg_per_s;
                whole_bound = error.max(previous_error)
                    + (libm::sqrtf(az * az + el * el) + plan.config.sky_rate_bound_deg_per_s) * dt
                        / 2.0
                    + 0.002;
                if previous_ms >= self.join_at_ms {
                    bound = whole_bound;
                }
            }
            self.previous = Some((now, error, sky));
            self.metrics.peak = self.metrics.peak.max(error);
            self.metrics.bound = self.metrics.bound.max(whole_bound);
            let allocation = (plan.config.tolerance_deg - plan.config.uncertainty_deg)
                .min(REACQUISITION_TOLERANCE_DEG);
            if now >= self.join_at_ms && bound > allocation {
                self.reject_candidate();
                return Ok(());
            }
        }
        if now >= los && sim.is_settled() {
            let summary = sim.summary();
            for axis in 0..2 {
                self.metrics.energized_ms[axis] =
                    self.metrics.energized_ms[axis].max(summary.energized_ms[axis]);
            }
            self.metrics.reversals = self.metrics.reversals.max(summary.reversals.iter().sum());
            self.metrics.transitions = self
                .metrics
                .transitions
                .max(summary.transitions.iter().sum());
            self.scenario += 1;
            self.sim = None;
            self.previous = None;
            if self.scenario == plan.config.scenarios.len() {
                self.result = Some(Intercept {
                    start: self.start,
                    launch_at_ms: self.launch_at_ms,
                    join_at_ms: self.join_at_ms,
                    pointing,
                });
                self.finished = true;
            }
        } else {
            if now < self.launch_at_ms {
                sim.stop_step_at(self.launch_at_ms);
            }
            if now < self.join_at_ms {
                sim.stop_step_at(self.join_at_ms);
            }
            if now < los {
                sim.stop_step_at(los);
            }
        }
        Ok(())
    }

    fn reject_candidate(&mut self) {
        self.join_at_ms += INTERCEPT_GRID_MS;
        self.scenario = 0;
        self.sim = None;
        self.previous = None;
        self.metrics = RecoveryMetrics::default();
    }
}

pub(super) fn route_at<'a>(plan: impl Into<SearchRoute<'a>>, now: u64) -> Option<Pointing> {
    let plan = plan.into();
    let relative = u32::try_from(now.checked_sub(plan.window.aos_at_ms)?).ok()?;
    plan.route.pointing_at(relative)
}

#[cfg(test)]
mod tests;
