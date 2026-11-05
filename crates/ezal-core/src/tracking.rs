//! Device-owned tracking planning, execution, lifecycle and simulation.
//!
//! Continuous mechanical coordinates are distinct from sky bearings. A route
//! planner must choose those coordinates before the following controller runs.
//! The incremental simulator exercises that controller through the production
//! actuator, using explicit, unmeasured motor assumptions. Its sampled results
//! are development diagnostics, not permission to track a pass on hardware.
//! [`TrackingLifecycle`] binds a single selected event, revisioned plans and
//! executor generations to an exclusive operator lease. It owns frozen input
//! snapshots independently of browser connections.
//! [`TrackingRuntime`] adds validated input publication and execution gating;
//! firmware uses it to supervise the frozen timeline and device network through
//! the same coordination/actuator boundary as manual commands. The dashboard
//! exposes leased selection/arming and a bounded read-only status projection.

mod clock;
mod controller;
mod deviation;
mod event;
mod inputs;
mod lifecycle;
mod mailbox;
mod model;
mod planner;
mod reception;
mod recovery;
mod route;
mod runtime;
mod simulation;

pub use clock::{execution_window, ExecutionClock, TimingError, TimingPolicy};
pub use controller::FollowingController;
pub use deviation::{
    DeviationReading, DeviationStatus, REACQUISITION_DWELL_MS, REACQUISITION_ENTER_MS,
    REACQUISITION_LIMIT_MS, REACQUISITION_TOLERANCE_DEG,
};
pub use event::{reconcile_event, EventError, EventId, PassEvent};
pub use inputs::{ExecutionInputs, InputError, InputSettings};
pub use lifecycle::{
    BoundTrackingPlan, LifecycleError, PlanProblem, PlanRevisions, PlanStatus, PlanTicket,
    RecoveryCause, RecoveryToken, RunToken, Selection, TrackingAction, TrackingLifecycle,
    TrackingMode, TrackingPhase, TrackingRequest, TrackingSnapshot, TrackingStop,
    TrackingTermination,
};
pub use model::MotorModel;
pub use planner::{
    Assessment, CandidateFailure, EvaluationPhase, PlannedGap, Planner, PlanningConfig,
    PlanningError, PlanningReport, PlanningWindow, PreparationTiming, TrackingPlan, MAX_GAPS,
    MAX_PLANNING_STEPS, MAX_SCENARIOS,
};
pub use reception::{ReceptionCoverage, ReceptionGap, ReceptionWindow, PARTIAL_ADMISSION_LEAD_MS};
pub use recovery::{
    RecoveryHold, RecoveryStatus, INTERCEPT_GRID_MS, INTERCEPT_PLANNING_LEAD_MS, INTERCEPT_RETRY_MS,
};
pub use route::{Route, RouteChoice, RouteKind};
pub use runtime::{RuntimeError, RuntimeObservation, TrackingRuntime, TrackingSummary};
pub use simulation::{
    RestingStart, Scenario, Simulation, SimulationSample, SimulationSettings, SimulationSummary,
};

use crate::duty::DutyConfig;
use crate::feedback::FeedbackHealth;
use crate::motion::MotionFault;
use crate::position::Pointing;

/// Mechanical axis named in simulation failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Continuous azimuth, including any installed overlap.
    Azimuth,
    /// Mechanical elevation, including installed over-the-top travel.
    Elevation,
}

/// A chosen mechanical route position and the actual satellite direction.
///
/// Keep both when a keyhole route deliberately departs from the exact pointing.
/// Otherwise a follower could track the motor route perfectly while its report
/// concealed the planned loss of sky alignment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackingTarget {
    /// Continuous motor angles, already selected within the installed travel.
    pub mechanical: Pointing,
    /// Satellite boresight direction; an equivalent over-the-top representation
    /// is accepted as well as conventional sky azimuth/elevation.
    pub sky: Pointing,
}

impl TrackingTarget {
    /// A mechanical route position exactly aligned with the satellite.
    pub const fn exact(pointing: Pointing) -> Self {
        Self {
            mechanical: pointing,
            sky: pointing,
        }
    }
}

/// One axis's assumed response to the applied direction relays.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AxisProfile {
    /// Constant motor rate after startup. Zero models a stalled motor.
    pub rate_deg_per_s: f32,
    /// Dead time on each activation before motor travel starts.
    pub startup_ms: u64,
    /// Constant-rate travel after release; deliberately conservative, not a
    /// measured deceleration curve. No coast follows an activation that never moved.
    pub coast_ms: u64,
    /// Motor travel between the two engaged sides of the gearing.
    pub backlash_deg: f32,
}

impl AxisProfile {
    fn is_valid(self) -> bool {
        self.rate_deg_per_s.is_finite()
            && self.rate_deg_per_s >= 0.0
            && self.backlash_deg.is_finite()
            && (0.0..=180.0).contains(&self.backlash_deg)
    }
}

/// Assumed plant behavior, independent of the actuator's relay timing.
///
/// These AC profiles come from the provisional values in `docs/HARDWARE.md`.
/// They do not identify the installed hardware or replace commissioning.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotorProfile {
    /// Azimuth response, the same in both directions.
    pub azimuth: AxisProfile,
    /// Elevation response, the same in both directions.
    pub elevation: AxisProfile,
    /// Per-axis energized allowance and the full rest that replenishes it.
    pub duty: DutyConfig,
}

impl MotorProfile {
    /// Original AC G-5500, 50 Hz, with provisional transients and confirmed-AC duty.
    pub const AC_50HZ: Self = Self {
        azimuth: AxisProfile {
            rate_deg_per_s: 360.0 / 70.0,
            startup_ms: 300,
            coast_ms: 500,
            backlash_deg: 1.0,
        },
        elevation: AxisProfile {
            rate_deg_per_s: 180.0 / 80.0,
            startup_ms: 300,
            coast_ms: 500,
            backlash_deg: 1.0,
        },
        duty: DutyConfig::CONFIRMED_AC,
    };

    /// Slower loaded-rate scenario used for preparation/intercept estimates.
    pub const AC_SLOW: Self = Self {
        azimuth: AxisProfile {
            rate_deg_per_s: 4.0,
            ..Self::AC_50HZ.azimuth
        },
        elevation: AxisProfile {
            rate_deg_per_s: 1.8,
            ..Self::AC_50HZ.elevation
        },
        ..Self::AC_50HZ
    };

    /// AC upper-rate assumptions; they do not bound a G-5500DC.
    pub const AC_FAST: Self = Self {
        azimuth: AxisProfile {
            rate_deg_per_s: 7.0,
            ..Self::AC_50HZ.azimuth
        },
        elevation: AxisProfile {
            rate_deg_per_s: 3.0,
            ..Self::AC_50HZ.elevation
        },
        ..Self::AC_50HZ
    };

    /// Reject non-finite physics and empty duty/rest allowances.
    pub fn is_valid(self) -> bool {
        self.azimuth.is_valid() && self.elevation.is_valid() && self.duty.is_valid()
    }
}

/// Why a development simulation could not continue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimulationError {
    /// Starting actuator/feedback snapshot was not coherently serviced and settled.
    InvalidStart,
    /// Motor assumptions or initial duty accounting were invalid.
    InvalidProfile,
    /// The simulated installation was not observable by the configured ADC.
    InvalidCalibration,
    /// Controller thresholds were non-finite or outside their supported range.
    InvalidController,
    /// Sampling cadence, acquisition, output policy or consumed duty was invalid.
    InvalidSettings,
    /// Initial/commanded mechanical coordinates were outside installed travel.
    InvalidPointing,
    /// Actual model travel, including coast, crossed a mechanical endpoint.
    TravelLimit(Axis),
    /// An axis exhausted its energized-time allowance without a full rest.
    DutyLimit(Axis),
    /// The production feedback validator rejected a simulated acquisition.
    Feedback(FeedbackHealth),
    /// The production actuator latched a physical motion fault.
    Motion(MotionFault),
    /// The simulation clock could not represent its next service instant.
    TimeOverflow,
}

/// True angular separation of two boresight directions in degrees.
///
/// Inputs are mechanical angles, not differences in az/el coordinates. This
/// handles azimuth overlap, zenith and `(A + 180°, 180° - E)` equivalence. It
/// does not include alignment, orbital or clock uncertainty. Invalid mechanical
/// coordinates return `None` rather than an apparently small deviation.
pub fn pointing_deviation_deg(left: Pointing, right: Pointing) -> Option<f32> {
    if !left.is_valid() || !right.is_valid() {
        return None;
    }
    let vector = |p: Pointing| {
        let az = f64::from(p.azimuth_deg).to_radians();
        let el = f64::from(p.elevation_deg).to_radians();
        let (sin_az, cos_az) = libm::sincos(az);
        let (sin_el, cos_el) = libm::sincos(el);
        [cos_el * sin_az, cos_el * cos_az, sin_el]
    };
    let a = vector(left);
    let b = vector(right);
    let cross = [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ];
    let sine = libm::sqrt(cross.iter().map(|v| v * v).sum::<f64>());
    let cosine = a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>();
    Some(libm::atan2(sine, cosine).to_degrees() as f32)
}
