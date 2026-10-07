//! Fail-safe closed-loop azimuth/elevation control.
//!
//! [`TrackingController`] is deliberately transport- and hardware-agnostic.
//! It accepts timestamped targets and feedback positions, validates their
//! freshness and mechanical bounds, and produces a bang-bang drive command
//! with separate engage/release thresholds to avoid relay chatter.

use crate::drive::{AzimuthDirection, Command, DriveCommand, ElevationDirection};
use crate::position::{Pointing, PositionCalibration};

/// Maximum age of feedback used by the controller and final output interlock.
/// A feedback read still unfinished after this long is cancelled, and the
/// cancelled bus is not reused until reset.
pub const FEEDBACK_TIMEOUT_MS: u64 = 500;

/// Delay between publishing one feedback sample and starting the next read.
pub const FEEDBACK_PERIOD_MS: u64 = 100;

/// Longest accepted time from starting a feedback read to validating it. A
/// slower sample is rejected as stale rather than published with too little
/// remaining life for its replacement to arrive.
pub const FEEDBACK_MAX_LATENCY_MS: u64 = 150;

/// Executor delay allowed between publishing a sample and starting the next.
const FEEDBACK_SCHEDULING_MARGIN_MS: u64 = 100;

// An accepted sample must be replaced before the output interlock expires it:
// its own latency, the period, scheduling delay, and the replacement's latency
// all fit inside the feedback lifetime.
const _: () = assert!(
    2 * FEEDBACK_MAX_LATENCY_MS + FEEDBACK_PERIOD_MS + FEEDBACK_SCHEDULING_MARGIN_MS
        <= FEEDBACK_TIMEOUT_MS
);

/// Timestamped position supplied to the control loop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimedPointing {
    /// Target or measured position.
    pub position: Pointing,
    /// Monotonic production timestamp in milliseconds.
    pub timestamp_ms: u64,
}

impl TimedPointing {
    /// Attach a monotonic timestamp to a position.
    pub const fn new(position: Pointing, timestamp_ms: u64) -> Self {
        Self {
            position,
            timestamp_ms,
        }
    }
}

/// Tunable control and watchdog thresholds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControlConfig {
    /// Error at which an idle axis starts moving.
    pub engage_error_deg: f32,
    /// Error below which a moving axis stops.
    pub release_error_deg: f32,
    /// Maximum accepted target age.
    pub target_timeout_ms: u64,
    /// Maximum accepted feedback age.
    pub feedback_timeout_ms: u64,
    /// Installed azimuth lower mechanical endpoint.
    pub azimuth_min_deg: f32,
    /// Installed azimuth upper mechanical endpoint.
    pub azimuth_max_deg: f32,
    /// Installed elevation lower mechanical endpoint.
    pub elevation_min_deg: f32,
    /// Installed elevation upper mechanical endpoint.
    pub elevation_max_deg: f32,
}

impl ControlConfig {
    /// Use an installed calibration's angle endpoints as controller limits.
    pub fn for_calibration(calibration: PositionCalibration) -> Self {
        Self {
            azimuth_min_deg: calibration.azimuth.angle_min_deg,
            azimuth_max_deg: calibration.azimuth.angle_max_deg,
            elevation_min_deg: calibration.elevation.angle_min_deg,
            elevation_max_deg: calibration.elevation.angle_max_deg,
            ..Self::default()
        }
    }

    /// Whether the thresholds define hysteresis and both mechanical ranges
    /// support finite position errors. A zero release threshold is valid, but
    /// engagement must require a strictly larger error.
    pub fn is_valid(self) -> bool {
        self.release_error_deg.is_finite()
            && self.engage_error_deg.is_finite()
            && self.release_error_deg >= 0.0
            && self.engage_error_deg > self.release_error_deg
            && [
                (self.azimuth_min_deg, self.azimuth_max_deg),
                (self.elevation_min_deg, self.elevation_max_deg),
            ]
            .into_iter()
            .all(|(min, max)| {
                min.is_finite() && max.is_finite() && max > min && (max - min).is_finite()
            })
    }

    fn contains(self, position: Pointing) -> bool {
        position.azimuth_deg.is_finite()
            && position.elevation_deg.is_finite()
            && position.azimuth_deg >= self.azimuth_min_deg
            && position.azimuth_deg <= self.azimuth_max_deg
            && position.elevation_deg >= self.elevation_min_deg
            && position.elevation_deg <= self.elevation_max_deg
    }
}

impl Default for ControlConfig {
    fn default() -> Self {
        Self {
            engage_error_deg: 4.0,
            release_error_deg: 1.5,
            target_timeout_ms: 500,
            feedback_timeout_ms: FEEDBACK_TIMEOUT_MS,
            azimuth_min_deg: 0.0,
            azimuth_max_deg: 450.0,
            elevation_min_deg: 0.0,
            elevation_max_deg: 180.0,
        }
    }
}

/// State of the controller after one update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlState {
    /// Fresh, valid target and feedback are driving the loop.
    Tracking,
    /// The pass scheduler intentionally supplied no target (pause/idle).
    Idle,
    /// No usable feedback has arrived yet.
    FeedbackUnavailable,
    /// Target timestamp exceeded its watchdog deadline.
    TargetStale,
    /// Feedback timestamp exceeded its watchdog deadline.
    FeedbackStale,
    /// Target was non-finite or outside the mechanical envelope.
    TargetInvalid,
    /// Feedback was non-finite or outside the mechanical envelope.
    FeedbackInvalid,
    /// A timestamp was in the future, indicating a broken time source.
    TimestampInvalid,
    /// Dish did not reach and settle at the pass start before the deadline.
    AcquisitionTimeout,
    /// The installed calibration cannot be supervised with the feedback ADC.
    CalibrationInvalid,
    /// Controller thresholds or mechanical limits violate their invariants.
    ConfigurationInvalid,
}

impl ControlState {
    /// Stable token for logs and telemetry.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tracking => "tracking",
            Self::Idle => "idle",
            Self::FeedbackUnavailable => "no-feedback",
            Self::TargetStale => "target-stale",
            Self::FeedbackStale => "feedback-stale",
            Self::TargetInvalid => "target-invalid",
            Self::FeedbackInvalid => "feedback-invalid",
            Self::TimestampInvalid => "timestamp-invalid",
            Self::AcquisitionTimeout => "acquire-timeout",
            Self::CalibrationInvalid => "calibration-invalid",
            Self::ConfigurationInvalid => "configuration-invalid",
        }
    }

    /// Whether motion is permitted in this state.
    pub const fn permits_motion(self) -> bool {
        matches!(self, Self::Tracking)
    }
}

/// One fail-safe control-loop decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlDecision {
    /// Leased command to submit to the actuator boundary.
    pub command: Command,
    /// Health/inhibit state explaining the command.
    pub state: ControlState,
}

/// Hysteretic two-axis position controller with input watchdogs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackingController {
    config: ControlConfig,
    azimuth: Option<AzimuthDirection>,
    elevation: Option<ElevationDirection>,
}

impl TrackingController {
    /// Create an idle controller. An invalid configuration is retained for
    /// diagnosis, and every update inhibits motion with
    /// [`ControlState::ConfigurationInvalid`].
    pub const fn new(config: ControlConfig) -> Self {
        Self {
            config,
            azimuth: None,
            elevation: None,
        }
    }

    /// Evaluate one control tick. Any invalid or stale input resets both
    /// hysteresis latches and returns an immediate stop request.
    pub fn update(
        &mut self,
        now_ms: u64,
        target: Option<TimedPointing>,
        feedback: Option<TimedPointing>,
    ) -> ControlDecision {
        // Configuration is an input too: NaN or negative thresholds can
        // otherwise turn a zero position error into a latched drive request.
        if !self.config.is_valid() {
            return self.stop(ControlState::ConfigurationInvalid);
        }
        let target = match target {
            Some(target) => target,
            None => return self.stop(ControlState::Idle),
        };
        let feedback = match feedback {
            Some(feedback) => feedback,
            None => return self.stop(ControlState::FeedbackUnavailable),
        };

        if target.timestamp_ms > now_ms || feedback.timestamp_ms > now_ms {
            return self.stop(ControlState::TimestampInvalid);
        }
        if now_ms - target.timestamp_ms > self.config.target_timeout_ms {
            return self.stop(ControlState::TargetStale);
        }
        if now_ms - feedback.timestamp_ms > self.config.feedback_timeout_ms {
            return self.stop(ControlState::FeedbackStale);
        }
        if !self.config.contains(target.position) {
            return self.stop(ControlState::TargetInvalid);
        }
        if !self.config.contains(feedback.position) {
            return self.stop(ControlState::FeedbackInvalid);
        }

        self.azimuth = update_azimuth(
            self.azimuth,
            target.position.azimuth_deg - feedback.position.azimuth_deg,
            feedback.position.azimuth_deg,
            self.config.azimuth_min_deg,
            self.config.azimuth_max_deg,
            self.config,
        );
        self.elevation = update_elevation(
            self.elevation,
            target.position.elevation_deg - feedback.position.elevation_deg,
            feedback.position.elevation_deg,
            self.config.elevation_min_deg,
            self.config.elevation_max_deg,
            self.config,
        );

        let drive = DriveCommand {
            azimuth: self.azimuth,
            elevation: self.elevation,
        };
        ControlDecision {
            command: if drive.is_active() {
                Command::Drive(drive)
            } else {
                Command::Stop
            },
            state: ControlState::Tracking,
        }
    }

    fn stop(&mut self, state: ControlState) -> ControlDecision {
        self.azimuth = None;
        self.elevation = None;
        ControlDecision {
            command: if state == ControlState::Idle {
                Command::Stop
            } else {
                Command::Inhibit
            },
            state,
        }
    }
}

impl Default for TrackingController {
    fn default() -> Self {
        Self::new(ControlConfig::default())
    }
}

fn update_azimuth(
    current: Option<AzimuthDirection>,
    error_deg: f32,
    position_deg: f32,
    position_min_deg: f32,
    position_max_deg: f32,
    config: ControlConfig,
) -> Option<AzimuthDirection> {
    let desired = if error_deg > 0.0 {
        AzimuthDirection::Clockwise
    } else {
        AzimuthDirection::CounterClockwise
    };
    hysteretic_direction(current, desired, error_deg.abs(), config).filter(|direction| {
        before_endpoint(
            direction.increases_angle(),
            position_deg,
            position_min_deg,
            position_max_deg,
        )
    })
}

fn update_elevation(
    current: Option<ElevationDirection>,
    error_deg: f32,
    position_deg: f32,
    position_min_deg: f32,
    position_max_deg: f32,
    config: ControlConfig,
) -> Option<ElevationDirection> {
    let desired = if error_deg > 0.0 {
        ElevationDirection::Up
    } else {
        ElevationDirection::Down
    };
    hysteretic_direction(current, desired, error_deg.abs(), config).filter(|direction| {
        before_endpoint(
            direction.increases_angle(),
            position_deg,
            position_min_deg,
            position_max_deg,
        )
    })
}

/// Whether a position still has travel left toward the endpoint a direction
/// approaches. The output interlock separately allows for sampling and travel.
fn before_endpoint(increases_angle: bool, position_deg: f32, min_deg: f32, max_deg: f32) -> bool {
    if increases_angle {
        position_deg < max_deg
    } else {
        position_deg > min_deg
    }
}

fn hysteretic_direction<T: Copy + PartialEq>(
    current: Option<T>,
    desired: T,
    absolute_error_deg: f32,
    config: ControlConfig,
) -> Option<T> {
    if absolute_error_deg <= config.release_error_deg {
        None
    } else if let Some(current) = current {
        (current == desired).then_some(current)
    } else if absolute_error_deg >= config.engage_error_deg {
        Some(desired)
    } else {
        None
    }
}
