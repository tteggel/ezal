//! Incremental, finite route search. Support is conditional on the supplied
//! motor/measurement scenarios and sky-rate bound, never a hardware guarantee.

use super::route::{self, Bridge, Knot, Route, RouteChoice, RouteKind, MAX_ROUTE_POINTS};
use super::{
    pointing_deviation_deg, AdmissionPolicy, FollowingController, MotorProfile, ReceptionCoverage,
    RestingStart, Simulation, SimulationError, SimulationSettings, TrackingTarget,
};
use crate::passes::Ephemeris;
use crate::position::{Pointing, PositionCalibration};
use heapless::Vec;

mod partial;
use partial::PartialEvaluation;

/// Maximum explicitly evaluated plant/measurement scenarios.
pub const MAX_SCENARIOS: usize = 9;
/// Maximum retained gap intervals for the worst-duration scenario.
pub const MAX_GAPS: usize = 16;
/// Hard work bound, including geometry, preparation probes and pass evaluation.
pub const MAX_PLANNING_STEPS: u64 = 20_000_000;
const MAX_PASS_MS: u32 = 1_800_000;
const MAX_PREPARATION_MS: u64 = 300_000;
const READY_DWELL_MS: u64 = 2_000;

/// The execution owner supplies the UTC/monotonic mapping; planning never reads
/// or changes a clock. Source provenance/expiry belongs to that owner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlanningWindow {
    /// Fractional UTC at acquisition.
    pub aos_unix_s: f64,
    /// Acquisition in the output owner's monotonic time domain.
    pub aos_at_ms: u64,
    /// Complete reception duration, at most 30 minutes.
    pub duration_ms: u32,
}

/// One repeatable uncertainty case. These finite cases do not prove all
/// intermediate combinations of motor behavior, noise or atmospheric effects.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scenario {
    /// Assumed loaded motor response; the follower keeps its own fixed reference.
    pub motor: MotorProfile,
    /// Total paired acquisition latency, at most 150 ms.
    pub latency_ms: u64,
    /// Delay between capturing A0 and A1, included in that latency.
    pub channel_skew_ms: u64,
    /// Azimuth/elevation ADC-code offsets before quantization.
    pub noise_codes: [i8; 2],
    /// Alternating offsets exercise code-boundary chatter as well as fixed bias.
    pub alternate_noise: bool,
    /// Explicit gear engagement at preparation start, where known/assumed.
    pub initial_engagement: [Option<bool>; 2],
}

impl Scenario {
    /// Nominal AC case, initially engaged in the first requested direction.
    pub const NOMINAL: Self = Self {
        motor: MotorProfile::AC_50HZ,
        latency_ms: 100,
        channel_skew_ms: 0,
        noise_codes: [0, 0],
        alternate_noise: false,
        initial_engagement: [None, None],
    };
}

/// Preparation schedule that must be included in the complete simulation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PreparationTiming {
    /// Wait until the computed preparation lead before AOS.
    #[default]
    Scheduled,
    /// Evaluate preparation beginning at the supplied starting snapshot.
    Immediate,
}

/// Explicit planning assumptions, frozen with a supported route.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanningConfig {
    /// Complete support is preferred; partial support requires recovery validation.
    pub admission: AdmissionPolicy,
    /// Starting early needs its own duty/relay evaluation, not a schedule override.
    pub preparation_timing: PreparationTiming,
    /// Total operational pointing target (provisional default 3°).
    pub tolerance_deg: f32,
    /// Reserved alignment/orbit/time uncertainty (provisional default 1°).
    pub uncertainty_deg: f32,
    /// Assumed Lipschitz bound on true sky motion, in degrees/second.
    /// Used to bound pointing error between simulation samples. Observed
    /// violations reject the source; unobserved violations remain an assumption.
    pub sky_rate_bound_deg_per_s: f32,
    /// Maximum following/control interval; phase and actuator deadlines shorten it.
    pub step_ms: u64,
    /// Fixed reference used by every scenario's feedback follower.
    pub reference: MotorProfile,
    /// Extra residual improvement needed to request a minimum pulse.
    pub deadband_deg: f32,
    /// Allowed mechanical error on either axis to complete preparation.
    /// Full sky pointing at AOS is still checked against the stricter budget.
    pub preparation_tolerance_deg: f32,
    /// Finite scenarios, all of which must complete successfully for support.
    pub scenarios: Vec<Scenario, MAX_SCENARIOS>,
}

impl Default for PlanningConfig {
    fn default() -> Self {
        let nominal = Scenario::NOMINAL;
        let slow = MotorProfile::AC_SLOW;
        let fast = MotorProfile::AC_FAST;
        Self {
            admission: AdmissionPolicy::AllowPartial,
            preparation_timing: PreparationTiming::Scheduled,
            tolerance_deg: 3.0,
            uncertainty_deg: 1.0,
            sky_rate_bound_deg_per_s: 2.0,
            step_ms: 20,
            reference: MotorProfile::AC_50HZ,
            deadband_deg: 0.15,
            preparation_tolerance_deg: 3.0,
            scenarios: Vec::from_slice(&[
                nominal,
                Scenario {
                    motor: slow,
                    latency_ms: 150,
                    channel_skew_ms: 50,
                    ..nominal
                },
                Scenario {
                    motor: fast,
                    latency_ms: 150,
                    channel_skew_ms: 50,
                    ..nominal
                },
                Scenario {
                    motor: MotorProfile {
                        elevation: fast.elevation,
                        ..slow
                    },
                    latency_ms: 150,
                    channel_skew_ms: 150,
                    noise_codes: [1, -1],
                    alternate_noise: true,
                    ..nominal
                },
                Scenario {
                    motor: MotorProfile {
                        elevation: slow.elevation,
                        ..fast
                    },
                    latency_ms: 150,
                    channel_skew_ms: 150,
                    noise_codes: [-1, 1],
                    alternate_noise: true,
                    ..nominal
                },
                Scenario {
                    motor: slow,
                    latency_ms: 0,
                    noise_codes: [1, 1],
                    initial_engagement: [Some(true); 2],
                    ..nominal
                },
                Scenario {
                    motor: fast,
                    latency_ms: 0,
                    noise_codes: [-1, -1],
                    initial_engagement: [Some(false); 2],
                    ..nominal
                },
                Scenario {
                    latency_ms: 150,
                    channel_skew_ms: 150,
                    noise_codes: [1, 1],
                    initial_engagement: [Some(false); 2],
                    ..nominal
                },
                Scenario {
                    latency_ms: 150,
                    channel_skew_ms: 150,
                    noise_codes: [-1, -1],
                    initial_engagement: [Some(true); 2],
                    ..nominal
                },
            ])
            .expect("fixed scenario capacity"),
        }
    }
}

/// A failure of the planning job itself, distinct from an unsupported route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanningError {
    /// Invalid assumptions, calibration or unsupported source time span.
    Configuration,
    /// The caller must supply a serviced, settled, fault-free starting snapshot.
    StartingState,
    /// A complete-only job cannot start after AOS; partial admission was disabled.
    PassAlreadyStarted,
    /// No future reception remains in the selected original window.
    PassEnded,
    /// Propagation failed or returned a non-finite/out-of-range visible sky direction.
    Source,
    /// Observed true sky movement exceeded the supplied intersample bound.
    SkyRateBound,
    /// Required geometric refinement exceeded fixed route storage.
    RouteCapacity,
    /// Search exceeded its hard work bound instead of blocking indefinitely.
    WorkLimit,
    /// Cancelled work cannot publish or revive an earlier candidate.
    Cancelled,
    /// Result requested before all candidates were evaluated.
    Incomplete,
}

/// Why one candidate cannot establish its requested reception coverage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateFailure {
    /// A real actuator or physical-model constraint stopped evaluation.
    Simulation(SimulationError),
    /// The starting pose could not be acquired within the bounded preparation probe.
    PreparationTimeout,
    /// Preparation was not complete at AOS. No complete-window claim is made.
    LatePreparation,
    /// An interval could not be bounded within the modeled pointing allocation.
    PointingBudget,
    /// No bounded recovery with a supported remainder qualified for this route.
    NoRecovery,
}

/// Which part of a candidate evaluation encountered its reported failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvaluationPhase {
    /// Moving to, or holding, the first route position before AOS.
    Preparation,
    /// Following the reception window or draining its final coast.
    Following,
}

/// An interval not established to be within the pointing budget, relative to AOS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlannedGap {
    /// First possibly out-of-budget instant.
    pub start_ms: u32,
    /// Last possibly out-of-budget instant; LOS if there is no recovery in the window.
    pub end_ms: u32,
}

/// Aggregate result for a candidate, including unsupported diagnostic attempts.
#[derive(Clone, Debug, PartialEq)]
pub struct Assessment {
    /// Route family and mechanical overlap.
    pub choice: RouteChoice,
    /// Starting scenarios whose advertised windows and final coast were validated.
    pub completed_scenarios: usize,
    /// Worst probed movement/readiness time, excluding any initial duty rest.
    pub preparation_ms: u64,
    /// `max(60 s, preparation + 20 s)`, or zero for initial late acquisition.
    pub preparation_lead_ms: u64,
    /// Scheduled start; initial late acquisition freezes at its fixed episode entry.
    pub prepare_at_ms: u64,
    /// Worst sampled actual boresight deviation, including intentional route offset.
    pub sampled_peak_deg: f32,
    /// Conservative intersample upper bound, plus a 0.002° numeric allowance.
    pub bounded_peak_deg: f32,
    /// Worst nominal gap duration, or the admitted partial plan's missed interval.
    pub gap_time_ms: u64,
    /// Scenario whose diagnostic intervals are retained; partial plans aggregate cases.
    pub gap_scenario: usize,
    /// Diagnostic gaps, or the validated missed interval for a supported partial plan.
    pub gaps: Vec<PlannedGap, MAX_GAPS>,
    /// Additional gaps existed beyond storage; totals and rejection remain valid.
    pub gaps_truncated: bool,
    /// Worst total preparation/following energized time per axis across scenarios.
    pub energized_ms: [u64; 2],
    /// Maximum reversal count, conservatively bounded across a partial recovery.
    pub reversals: u64,
    /// Maximum total relay transition count in any scenario.
    pub transitions: u64,
    /// Terminal failure, or pointing-budget failure if all scenarios completed.
    pub failure: Option<CandidateFailure>,
    /// Scenario that produced the reported failure, when any.
    pub failure_scenario: Option<usize>,
    /// Distinguishes preparation failures even if no pass samples ran.
    pub failure_phase: Option<EvaluationPhase>,
}

impl Assessment {
    fn new(choice: RouteChoice) -> Self {
        Self {
            choice,
            completed_scenarios: 0,
            preparation_ms: 0,
            preparation_lead_ms: 60_000,
            prepare_at_ms: 0,
            sampled_peak_deg: 0.0,
            bounded_peak_deg: 0.0,
            gap_time_ms: 0,
            gap_scenario: 0,
            gaps: Vec::new(),
            gaps_truncated: false,
            energized_ms: [0, 0],
            reversals: 0,
            transitions: 0,
            failure: None,
            failure_scenario: None,
            failure_phase: None,
        }
    }
    fn better_than(&self, other: &Self) -> bool {
        (self.failure.is_none(), self.completed_scenarios)
            > (other.failure.is_none(), other.completed_scenarios)
            || ((self.failure.is_none(), self.completed_scenarios)
                == (other.failure.is_none(), other.completed_scenarios)
                && (
                    self.bounded_peak_deg,
                    self.reversals,
                    self.transitions,
                    self.preparation_ms,
                ) < (
                    other.bounded_peak_deg,
                    other.reversals,
                    other.transitions,
                    other.preparation_ms,
                ))
    }
}

/// Supported in every supplied scenario for its explicit reception coverage. Keep its
/// ephemeris/calibration/clock revisions bound externally when freezing a run.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackingPlan {
    pub(super) reception: ReceptionCoverage,
    /// Frozen continuous motor route, with no implicit wrapping during execution.
    pub route: Route,
    /// Original UTC and monotonic reception window.
    pub window: PlanningWindow,
    /// Measurements/assumptions under which the route was evaluated.
    pub config: PlanningConfig,
    /// Installed travel/calibration used for the result.
    pub calibration: PositionCalibration,
    /// Output policy that must match execution.
    pub actuator: crate::actuator::ActuatorConfig,
    /// Evaluation metrics for the advertised reception coverage.
    pub assessment: Assessment,
}

impl TrackingPlan {
    /// The reception claim validated by the planner; diagnostics cannot set it.
    pub const fn reception(&self) -> ReceptionCoverage {
        self.reception
    }
}

/// Completed finite search. No supported plan means “none found in this search”,
/// not proof that every physically possible trajectory is impossible.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanningReport {
    /// Best supported result; unsupported diagnostics cannot be armed as a plan.
    pub plan: Option<TrackingPlan>,
    /// Best evaluated candidate, including a failure/gap explanation if unsupported.
    pub closest_attempt: Option<Assessment>,
    /// Evaluations with valid installed coordinates, including the partial round.
    pub evaluated_candidates: usize,
    /// Evaluations rejected by travel/bridge geometry, including the partial round.
    pub geometry_rejections: usize,
    /// Incremental work performed, bounded by `MAX_PLANNING_STEPS`.
    pub work_steps: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Geometry,
    Build,
    Probe,
    Verify,
    PartialPrefix,
    PartialSettle,
    PartialSearch,
    Complete,
}

#[derive(Default)]
struct RunMetrics {
    previous: Option<(u64, f32, Pointing)>,
    peak: f32,
    bound: f32,
    gap_time: u64,
    gaps: Vec<PlannedGap, MAX_GAPS>,
    open_gap: Option<u32>,
    truncated: bool,
}

/// One `step` performs at most one propagation and one simulator service, or
/// bounded route bookkeeping. No allocation, waiting, networking or live GPIO.
/// The caller yields between steps and can cancel at any phase.
pub struct Planner<E> {
    source: E,
    window: PlanningWindow,
    calibration: PositionCalibration,
    start: RestingStart,
    config: PlanningConfig,
    geometry: Vec<Knot, MAX_ROUTE_POINTS>,
    geometry_step_ms: u32,
    phase: Phase,
    candidate: usize,
    route: Route,
    bridge: Option<Bridge>,
    build_index: usize,
    scenario: usize,
    sim: Option<Simulation>,
    ready_since_ms: Option<u64>,
    prepared: bool,
    probe_start_ms: u64,
    metrics: RunMetrics,
    assessment: Assessment,
    report: PlanningReport,
    error: Option<PlanningError>,
    partial_round: bool,
    partial_cut: Option<u32>,
    partial: Option<PartialEvaluation>,
}

impl<E: Ephemeris> Planner<E> {
    /// Start from a coherent resting snapshot. Planning may be repeated before
    /// preparation; each job owns its immutable inputs and publishes only at completion.
    pub fn new(
        source: E,
        window: PlanningWindow,
        calibration: PositionCalibration,
        start: RestingStart,
        config: PlanningConfig,
    ) -> Result<Self, PlanningError> {
        let output = start.actuator.configuration();
        if window.duration_ms == 0
            || window.duration_ms > MAX_PASS_MS
            || !window.aos_unix_s.is_finite()
            || window.aos_unix_s.abs() > 1.0e11
            || window
                .aos_at_ms
                .checked_add(u64::from(window.duration_ms))
                .and_then(|los| los.checked_add(60_000))
                .is_none()
            || !config.tolerance_deg.is_finite()
            || !(0.1..=10.0).contains(&config.tolerance_deg)
            || !config.uncertainty_deg.is_finite()
            || config.uncertainty_deg < 0.0
            || config.uncertainty_deg >= config.tolerance_deg
            || !config.sky_rate_bound_deg_per_s.is_finite()
            || !(0.01..=20.0).contains(&config.sky_rate_bound_deg_per_s)
            || !(1..=20).contains(&config.step_ms)
            || config.scenarios.is_empty()
            || !config.preparation_tolerance_deg.is_finite()
            || !(0.1..=5.0).contains(&config.preparation_tolerance_deg)
            || !output.is_valid()
        {
            return Err(PlanningError::Configuration);
        }
        if start.at_ms >= window.aos_at_ms + u64::from(window.duration_ms) {
            return Err(PlanningError::PassEnded);
        }
        if window.aos_at_ms < start.at_ms && config.admission == AdmissionPolicy::CompleteOnly {
            return Err(PlanningError::PassAlreadyStarted);
        }
        let follower =
            FollowingController::with_timing(config.reference, config.deadband_deg, output.relays)
                .map_err(|_| PlanningError::Configuration)?;
        for scenario in &config.scenarios {
            if scenario.motor.azimuth.rate_deg_per_s > output.travel.azimuth.max_speed_deg_per_s
                || scenario.motor.elevation.rate_deg_per_s
                    > output.travel.elevation.max_speed_deg_per_s
                || scenario.motor.azimuth.coast_ms > output.travel.azimuth.coast_ms
                || scenario.motor.elevation.coast_ms > output.travel.elevation.coast_ms
            {
                return Err(PlanningError::Configuration);
            }
            Simulation::from_rest(
                calibration,
                scenario.motor,
                follower,
                Self::settings(&config, start, *scenario),
                start,
            )
            .map_err(|error| {
                if error == SimulationError::InvalidStart {
                    PlanningError::StartingState
                } else {
                    PlanningError::Configuration
                }
            })?;
        }
        let choice = route::choice(0);
        let partial_round =
            start.at_ms >= window.aos_at_ms && config.admission == AdmissionPolicy::AllowPartial;
        Ok(Self {
            source,
            window,
            calibration,
            start,
            config,
            geometry: Vec::new(),
            geometry_step_ms: 2_000,
            phase: Phase::Geometry,
            candidate: 0,
            route: Route {
                choice,
                points: Vec::new(),
            },
            bridge: None,
            build_index: 0,
            scenario: 0,
            sim: None,
            ready_since_ms: None,
            prepared: false,
            probe_start_ms: 0,
            metrics: RunMetrics::default(),
            assessment: Assessment::new(choice),
            report: PlanningReport {
                plan: None,
                closest_attempt: None,
                evaluated_candidates: 0,
                geometry_rejections: 0,
                work_steps: 0,
            },
            error: None,
            partial_round,
            partial_cut: None,
            partial: None,
        })
    }

    fn settings(
        config: &PlanningConfig,
        start: RestingStart,
        scenario: Scenario,
    ) -> SimulationSettings {
        SimulationSettings {
            step_ms: config.step_ms,
            feedback_latency_ms: scenario.latency_ms,
            channel_skew_ms: scenario.channel_skew_ms,
            noise_codes: scenario.noise_codes,
            alternate_noise: scenario.alternate_noise,
            initial_engagement: scenario.initial_engagement,
            actuator: start.actuator.configuration(),
            tolerance_deg: config.tolerance_deg - config.uncertainty_deg,
            ..SimulationSettings::default()
        }
    }

    /// True after a terminal error, complete search or validated partial fallback.
    pub fn is_finished(&self) -> bool {
        self.phase == Phase::Complete || self.error.is_some()
    }
    /// Cancelling invalidates all partial and earlier candidate results.
    pub fn cancel(&mut self) {
        self.error = Some(PlanningError::Cancelled);
        self.sim = None;
        self.partial = None;
        self.report.plan = None;
    }
    /// Consume the completed result; partial work never becomes a supported plan.
    pub fn into_report(self) -> Result<PlanningReport, PlanningError> {
        if let Some(error) = self.error {
            Err(error)
        } else if self.phase != Phase::Complete {
            Err(PlanningError::Incomplete)
        } else {
            Ok(self.report)
        }
    }
    /// Perform one bounded unit of work. Repeated calls after failure retain it.
    pub fn step(&mut self) -> Result<(), PlanningError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        if self.phase == Phase::Complete {
            return Ok(());
        }
        self.report.work_steps += 1;
        let result = if self.report.work_steps > MAX_PLANNING_STEPS {
            Err(PlanningError::WorkLimit)
        } else {
            match self.phase {
                Phase::Geometry => self.sample_geometry(),
                Phase::Build => {
                    self.build();
                    Ok(())
                }
                Phase::Probe => self.simulate(true),
                Phase::Verify => self.simulate(false),
                Phase::PartialPrefix => self.simulate(false),
                Phase::PartialSettle => self.settle_partial(),
                Phase::PartialSearch => self.search_partial(),
                Phase::Complete => Ok(()),
            }
        };
        if let Err(error) = result {
            self.error = Some(error);
            self.report.plan = None;
            self.sim = None;
            self.partial = None;
        }
        result
    }

    fn sky_at(&self, relative_ms: u32) -> Result<Pointing, PlanningError> {
        let look = self
            .source
            .look_at(self.window.aos_unix_s + f64::from(relative_ms) / 1000.0)
            .ok_or(PlanningError::Source)?;
        if !look.azimuth_deg.is_finite()
            || !(0.0..360.0).contains(&look.azimuth_deg)
            || !look.elevation_deg.is_finite()
            || !(0.0..=90.0).contains(&look.elevation_deg)
        {
            return Err(PlanningError::Source);
        }
        Ok(Pointing::new(
            look.azimuth_deg as f32,
            look.elevation_deg as f32,
        ))
    }

    fn sample_geometry(&mut self) -> Result<(), PlanningError> {
        let previous = self.geometry.last().copied();
        let at_ms = previous.map_or(0, |point| {
            (point.at_ms + self.geometry_step_ms).min(self.window.duration_ms)
        });
        let sky = self.sky_at(at_ms)?;
        let az = previous.map_or(sky.azimuth_deg, |point| {
            route::unwrap_near(point.az, sky.azimuth_deg)
        });
        if let Some(previous) = previous {
            let dt = at_ms - previous.at_ms;
            let previous_sky = Pointing::new(route::wrap(previous.az), previous.el);
            let angle = pointing_deviation_deg(previous_sky, sky).ok_or(PlanningError::Source)?;
            if angle > self.config.sky_rate_bound_deg_per_s * dt as f32 / 1000.0 + 0.002 {
                return Err(PlanningError::SkyRateBound);
            }
            if dt > 20 && ((az - previous.az).abs() > 5.0 || angle > 1.0) {
                self.geometry_step_ms = (dt / 2).max(20);
                return Ok(());
            }
        }
        self.geometry
            .push(Knot {
                at_ms,
                az,
                el: sky.elevation_deg,
            })
            .map_err(|_| PlanningError::RouteCapacity)?;
        if at_ms == self.window.duration_ms {
            self.phase = Phase::Build;
        }
        self.geometry_step_ms = if sky.elevation_deg > 85.0 {
            100
        } else if sky.elevation_deg > 70.0 || !(5.0..=355.0).contains(&sky.azimuth_deg) {
            500
        } else {
            2_000
        };
        Ok(())
    }

    fn next_candidate(&mut self) {
        self.sim = None;
        self.partial = None;
        self.partial_cut = None;
        self.candidate += 1;
        self.build_index = 0;
        self.scenario = 0;
        self.route.points.clear();
        if self.candidate == route::CANDIDATES {
            if self.report.plan.is_none()
                && !self.partial_round
                && self.config.admission == AdmissionPolicy::AllowPartial
            {
                self.partial_round = true;
                self.candidate = 0;
            } else {
                self.phase = Phase::Complete;
                return;
            }
        }
        self.route.choice = route::choice(self.candidate);
        self.assessment = Assessment::new(self.route.choice);
        self.phase = Phase::Build;
    }

    fn build(&mut self) {
        if self.build_index == 0 {
            self.bridge = route::bridge(&self.geometry, self.route.choice);
            if matches!(self.route.choice.kind, RouteKind::OverTop { .. }) && self.bridge.is_none()
            {
                self.report.geometry_rejections += 1;
                self.next_candidate();
                return;
            }
        }
        let point = route::transform(
            self.geometry[self.build_index],
            self.route.choice,
            self.bridge,
        );
        if !Pointing::new(point.az, point.el).is_valid_for(self.calibration) {
            self.report.geometry_rejections += 1;
            self.next_candidate();
            return;
        }
        // A linear segment between in-range endpoints remains in each mechanical range.
        self.route
            .points
            .push(point)
            .expect("same capacity as geometry");
        self.build_index += 1;
        if self.build_index == self.geometry.len() {
            self.report.evaluated_candidates += 1;
            self.phase = Phase::Probe;
            if self.start.at_ms >= self.window.aos_at_ms && self.partial_round {
                self.begin_partial(None);
            }
        }
    }

    fn start_simulation(&mut self, probe: bool) -> Result<(), PlanningError> {
        let case = self.config.scenarios[self.scenario];
        let follower = FollowingController::with_timing(
            self.config.reference,
            self.config.deadband_deg,
            self.start.actuator.configuration().relays,
        )
        .map_err(|_| PlanningError::Configuration)?;
        let mut sim = Simulation::from_rest(
            self.calibration,
            case.motor,
            follower,
            Self::settings(&self.config, self.start, case),
            self.start,
        )
        .map_err(|_| PlanningError::StartingState)?;
        let duty = self.start.actuator.status().duty;
        let rest_end = self
            .start
            .at_ms
            .checked_add(duty.azimuth.cooldown_ms.max(duty.elevation.cooldown_ms))
            .ok_or(PlanningError::Configuration)?;
        self.probe_start_ms =
            if probe && self.config.preparation_timing == PreparationTiming::Scheduled {
                let (used, idle) = self
                    .start
                    .actuator
                    .resting_history(self.start.at_ms)
                    .ok_or(PlanningError::StartingState)?;
                let rest_ms = self.start.actuator.configuration().duty.rest_ms;
                let until_full_rest = (0..2)
                    .map(|i| {
                        if used[i] > 0 {
                            rest_ms.saturating_sub(idle[i])
                        } else {
                            0
                        }
                    })
                    .max()
                    .unwrap_or(0);
                let full_rest_at = self
                    .start
                    .at_ms
                    .checked_add(until_full_rest)
                    .ok_or(PlanningError::Configuration)?;
                // Credit guaranteed idle time before the earliest possible
                // preparation start. A partial allowance may also replenish
                // before AOS; measure the slew after that rest when available.
                // Verification must fit the real duty at the scheduled instant.
                rest_end
                    .max(
                        self.window
                            .aos_at_ms
                            .saturating_sub(MAX_PREPARATION_MS + 20_000),
                    )
                    .max(if full_rest_at <= self.window.aos_at_ms {
                        full_rest_at
                    } else {
                        self.start.at_ms
                    })
            } else if probe {
                rest_end
            } else {
                rest_end.max(self.assessment.prepare_at_ms)
            };
        sim.skip_idle_until(self.probe_start_ms)
            .map_err(|_| PlanningError::StartingState)?;
        self.sim = Some(sim);
        self.ready_since_ms = None;
        self.prepared = false;
        self.metrics = RunMetrics::default();
        Ok(())
    }

    fn fail_candidate(&mut self, reason: CandidateFailure) {
        self.assessment.failure = Some(reason);
        self.assessment.failure_scenario = Some(self.scenario);
        self.assessment.failure_phase = Some(
            if self.phase == Phase::Probe
                || reason == CandidateFailure::LatePreparation
                || self
                    .sim
                    .as_ref()
                    .is_some_and(|sim| sim.now_ms() < self.window.aos_at_ms)
            {
                EvaluationPhase::Preparation
            } else {
                EvaluationPhase::Following
            },
        );
        if self.phase == Phase::Verify {
            if let Some((last_ms, _, _)) = self.metrics.previous {
                self.close_gap((last_ms - self.window.aos_at_ms) as u32);
            }
        }
        if self.phase == Phase::Probe {
            self.assessment.preparation_ms = self.assessment.preparation_ms.max(
                self.sim
                    .as_ref()
                    .unwrap()
                    .now_ms()
                    .saturating_sub(self.probe_start_ms),
            );
        }
        self.merge_metrics();
        self.finish_candidate();
    }

    fn simulate(&mut self, probe: bool) -> Result<(), PlanningError> {
        if self.sim.is_none() {
            self.start_simulation(probe)?;
            return Ok(());
        }
        let now = self.sim.as_ref().unwrap().now_ms();
        let aos = self.window.aos_at_ms;
        let los = aos + u64::from(self.window.duration_ms);
        if self.phase == Phase::PartialPrefix
            && now >= self.partial.as_ref().unwrap().recovery_at_ms
        {
            self.phase = Phase::PartialSettle;
            return Ok(());
        }
        let start_pose = self.route.pointing_at(0).expect("complete route");
        let preparing = probe || now < aos;
        if preparing
            && !self.prepared
            && now.saturating_sub(self.probe_start_ms) > MAX_PREPARATION_MS
        {
            self.fail_candidate(CandidateFailure::PreparationTimeout);
            return Ok(());
        }
        if !probe && now >= aos && !self.prepared {
            if self.phase == Phase::PartialPrefix {
                self.begin_partial(None);
                return Ok(());
            }
            self.fail_candidate(CandidateFailure::LatePreparation);
            return Ok(());
        }
        // Include the LOS endpoint for error accounting, but remove drive there.
        let sky = if !preparing && now <= los {
            Some(self.sky_at((now - aos) as u32)?)
        } else {
            None
        };
        let target = if preparing {
            Some(TrackingTarget::exact(start_pose))
        } else if now < los {
            Some(TrackingTarget {
                mechanical: self
                    .route
                    .pointing_at((now - aos) as u32)
                    .expect("within route"),
                sky: sky.unwrap(),
            })
        } else {
            None
        };
        let sim = self.sim.as_mut().unwrap();
        let sample = match sim.step_target(target) {
            Ok(sample) => sample,
            Err(error) => {
                self.fail_candidate(CandidateFailure::Simulation(error));
                return Ok(());
            }
        };
        if preparing {
            let acquired = sim.is_settled()
                && !sim.actuator().target().is_active()
                && (sample.position.azimuth_deg - start_pose.azimuth_deg).abs()
                    <= self.config.preparation_tolerance_deg
                && (sample.position.elevation_deg - start_pose.elevation_deg).abs()
                    <= self.config.preparation_tolerance_deg;
            if acquired {
                self.ready_since_ms.get_or_insert(now);
            } else {
                self.ready_since_ms = None;
            }
            if self
                .ready_since_ms
                .is_some_and(|since| now - since >= READY_DWELL_MS)
            {
                self.prepared = true;
            }
            if probe && self.prepared {
                self.assessment.preparation_ms = self
                    .assessment
                    .preparation_ms
                    .max(now - self.probe_start_ms);
                self.scenario += 1;
                self.sim = None;
                if self.scenario == self.config.scenarios.len() {
                    self.assessment.preparation_lead_ms =
                        60_000.max(self.assessment.preparation_ms + 20_000);
                    self.assessment.prepare_at_ms = match self.config.preparation_timing {
                        PreparationTiming::Scheduled => self
                            .start
                            .at_ms
                            .max(aos.saturating_sub(self.assessment.preparation_lead_ms)),
                        PreparationTiming::Immediate => self.start.at_ms,
                    };
                    self.scenario = 0;
                    self.phase = Phase::Verify;
                }
                return Ok(());
            }
        } else if let Some(sky) = sky {
            let error =
                pointing_deviation_deg(sample.position, sky).ok_or(PlanningError::Source)?;
            self.observe_error(now, error, sky)?;
            if self.phase == Phase::PartialPrefix
                && self.partial_cut.is_some_and(|cut| {
                    aos + u64::from(cut) < self.partial.as_ref().unwrap().recovery_at_ms
                })
            {
                self.begin_partial(self.partial_cut);
                return Ok(());
            }
        }
        let sim = self.sim.as_mut().unwrap();
        if !probe && now >= los && sim.is_settled() {
            self.finish_scenario();
            return Ok(());
        } else if !probe && now < aos {
            sim.stop_step_at(aos);
        } else if !probe && now < los {
            sim.stop_step_at(los);
        }
        if let Some(partial) = &self.partial {
            if self.phase == Phase::PartialPrefix {
                self.sim
                    .as_mut()
                    .unwrap()
                    .stop_step_at(partial.recovery_at_ms);
            }
        }
        Ok(())
    }

    fn observe_error(&mut self, now: u64, error: f32, sky: Pointing) -> Result<(), PlanningError> {
        let case = self.config.scenarios[self.scenario];
        let az_rate = case.motor.azimuth.rate_deg_per_s;
        let el_rate = case.motor.elevation.rate_deg_per_s;
        let motor_rate = libm::sqrtf(az_rate * az_rate + el_rate * el_rate);
        let mut bound = error + 0.002;
        let mut begin = now;
        if let Some((previous_ms, previous_error, previous_sky)) = self.metrics.previous {
            let dt = (now - previous_ms) as f32 / 1000.0;
            if pointing_deviation_deg(previous_sky, sky).ok_or(PlanningError::Source)?
                > self.config.sky_rate_bound_deg_per_s * dt + 0.002
            {
                return Err(PlanningError::SkyRateBound);
            }
            // Every instant lies within dt/2 of an endpoint. Mechanical sky
            // speed <= sqrt(az_rate² + el_rate²), including startup/coast.
            bound = error.max(previous_error)
                + (motor_rate + self.config.sky_rate_bound_deg_per_s) * dt / 2.0
                + 0.002;
            begin = previous_ms;
        }
        self.metrics.peak = self.metrics.peak.max(error);
        self.metrics.bound = self.metrics.bound.max(bound);
        let relative = (now - self.window.aos_at_ms) as u32;
        if bound > self.config.tolerance_deg - self.config.uncertainty_deg {
            let cut = (begin - self.window.aos_at_ms) as u32;
            self.partial_cut = Some(self.partial_cut.map_or(cut, |previous| previous.min(cut)));
            self.metrics
                .open_gap
                .get_or_insert((begin - self.window.aos_at_ms) as u32);
            self.metrics.gap_time += now - begin;
        } else {
            self.close_gap((begin - self.window.aos_at_ms) as u32);
        }
        if relative == self.window.duration_ms {
            self.close_gap(relative);
        }
        self.metrics.previous = Some((now, error, sky));
        Ok(())
    }

    fn close_gap(&mut self, end_ms: u32) {
        if let Some(start_ms) = self.metrics.open_gap.take() {
            if self
                .metrics
                .gaps
                .push(PlannedGap { start_ms, end_ms })
                .is_err()
            {
                self.metrics.truncated = true;
            }
        }
    }

    fn merge_metrics(&mut self) {
        let summary = self.sim.as_ref().unwrap().summary();
        self.assessment.sampled_peak_deg = self.assessment.sampled_peak_deg.max(self.metrics.peak);
        self.assessment.bounded_peak_deg = self.assessment.bounded_peak_deg.max(self.metrics.bound);
        if self.assessment.failure.is_none()
            && self.metrics.bound > self.config.tolerance_deg - self.config.uncertainty_deg
        {
            self.assessment.failure = Some(CandidateFailure::PointingBudget);
            self.assessment.failure_scenario = Some(self.scenario);
            self.assessment.failure_phase = Some(EvaluationPhase::Following);
        }
        if self.metrics.gap_time > self.assessment.gap_time_ms {
            self.assessment.gap_time_ms = self.metrics.gap_time;
            self.assessment.gap_scenario = self.scenario;
            self.assessment.gaps = self.metrics.gaps.clone();
            self.assessment.gaps_truncated = self.metrics.truncated;
        }
        for (max, used) in self
            .assessment
            .energized_ms
            .iter_mut()
            .zip(summary.energized_ms)
        {
            *max = (*max).max(used);
        }
        self.assessment.reversals = self
            .assessment
            .reversals
            .max(summary.reversals.iter().sum());
        self.assessment.transitions = self
            .assessment
            .transitions
            .max(summary.transitions.iter().sum());
    }

    fn finish_scenario(&mut self) {
        self.merge_metrics();
        self.sim = None;
        self.assessment.completed_scenarios += 1;
        self.scenario += 1;
        if self.scenario == self.config.scenarios.len() {
            self.finish_candidate();
        }
    }

    fn finish_candidate(&mut self) {
        if self
            .report
            .closest_attempt
            .as_ref()
            .is_none_or(|previous| self.assessment.better_than(previous))
        {
            self.report.closest_attempt = Some(self.assessment.clone());
        }
        if self.assessment.failure.is_none()
            && self
                .report
                .plan
                .as_ref()
                .is_none_or(|plan| self.assessment.better_than(&plan.assessment))
        {
            self.report.plan = Some(TrackingPlan {
                reception: ReceptionCoverage::Complete,
                route: self.route.clone(),
                window: self.window,
                config: self.config.clone(),
                calibration: self.calibration,
                actuator: self.start.actuator.configuration(),
                assessment: self.assessment.clone(),
            });
        }
        if self.partial_round && self.partial.is_none() {
            match self.assessment.failure {
                Some(CandidateFailure::PointingBudget) => {
                    self.begin_partial(self.partial_cut);
                    return;
                }
                Some(CandidateFailure::LatePreparation | CandidateFailure::PreparationTimeout) => {
                    self.begin_partial(None);
                    return;
                }
                Some(CandidateFailure::Simulation(_))
                    if self.assessment.failure_phase == Some(EvaluationPhase::Following)
                        && self.partial_cut.is_some() =>
                {
                    // The nominal continuation may fault after losing pointing.
                    // A different, inhibited recovery path must still pass every
                    // physical check; never carry the failed actuator into it.
                    self.begin_partial(self.partial_cut);
                    return;
                }
                _ => {}
            }
        }
        self.next_candidate();
    }
}
