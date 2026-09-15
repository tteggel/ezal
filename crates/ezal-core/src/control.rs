//! Fail-safe closed-loop azimuth/elevation control.
//!
//! [`TrackingController`] is deliberately transport- and hardware-agnostic.
//! It accepts timestamped targets and feedback positions, validates their
//! freshness and mechanical bounds, and produces a bang-bang drive command
//! with separate engage/release thresholds to avoid relay chatter.

use crate::drive::{AzimuthDirection, Command, DriveCommand, ElevationDirection};
use crate::position::{Pointing, PositionCalibration};

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
            feedback_timeout_ms: 500,
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
    /// Create an idle controller.
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
            command: Command::Stop,
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
    let next = hysteretic_direction(current, desired, error_deg.abs(), config);

    match next {
        Some(AzimuthDirection::CounterClockwise) if position_deg <= position_min_deg => None,
        Some(AzimuthDirection::Clockwise) if position_deg >= position_max_deg => None,
        other => other,
    }
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
    let next = hysteretic_direction(current, desired, error_deg.abs(), config);

    match next {
        Some(ElevationDirection::Down) if position_deg <= position_min_deg => None,
        Some(ElevationDirection::Up) if position_deg >= position_max_deg => None,
        other => other,
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
