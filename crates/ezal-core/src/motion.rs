//! Motion supervision at the physical output boundary.
//!
//! A fresh in-range ADC value proves neither that a motor moved nor that its
//! feedback wiring is correct. This monitor counts time for **applied** output
//! directions, requires displacement larger than feedback quantization, and
//! checks direction and speed across pulse trains, including their coast.
//! Lease refreshes, normal stops, inactive relay holds, and feedback recovery
//! cannot erase a fault or repeatedly spend a new measurement-noise allowance.
//!
//! The default thresholds are conservative engineering starting points, not
//! installation measurements. Commission the timeout, code tolerances, and
//! the travel profile on the installed
//! mechanism. A fault requires inspection and a firmware reset; there is no
//! command or automatically recovering sample that clears it.

use crate::drive::DriveCommand;
use crate::interlock::{AxisTravel, FeedbackInterlock, TravelProfile};
use crate::position::Pointing;

/// Keep a longer comparison alongside consecutive samples. Otherwise the
/// noise allowance could be spent again at every sample to disguise a
/// consistently excessive rate as a series of individually small changes.
const SPEED_WINDOW_MS: u64 = 1_000;

/// Installation-tunable thresholds shared by the two axis monitors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MotionConfig {
    /// Maximum accumulated energised time without a resolvable displacement.
    /// Includes motor startup; idle time pauses this budget, and even reversal
    /// does not reset it. Repeated short jogs cannot hide frozen feedback.
    pub progress_timeout_ms: u64,
    /// Minimum displacement, in calibrated ADC codes, that proves progress.
    /// Must exceed `noise_codes`; a lone code transition is not sufficient.
    pub progress_codes: u16,
    /// Allowed feedback noise/quantization in direction and speed comparisons.
    /// Each code includes the interlock's millivolt rounding allowance.
    pub noise_codes: u16,
}

impl MotionConfig {
    /// Initial commissioning values: 5 s without movement, three codes to
    /// prove progress, and two codes of measurement tolerance.
    pub const DEFAULT: Self = Self {
        progress_timeout_ms: 5_000,
        progress_codes: 3,
        noise_codes: 2,
    };

    pub(crate) const fn is_valid(self) -> bool {
        self.progress_timeout_ms > 0
            && self.noise_codes > 0
            && self.progress_codes > self.noise_codes
    }
}

/// First motion-supervision failure; latches both physical outputs off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotionFault {
    /// Output configuration or history cannot authorize supervised movement.
    InvalidConfiguration,
    /// The monotonic output-owner clock moved backwards.
    ClockWentBackwards,
    /// Azimuth exhausted its energised-time budget without movement.
    AzimuthNoProgress,
    /// Elevation exhausted its energised-time budget without movement.
    ElevationNoProgress,
    /// Azimuth feedback moved against its applied direction or expected coast.
    AzimuthWrongDirection,
    /// Elevation feedback moved against its applied direction or expected coast.
    ElevationWrongDirection,
    /// Azimuth feedback changed faster than the configured physical bound.
    AzimuthImplausibleSpeed,
    /// Elevation feedback changed faster than the configured physical bound.
    ElevationImplausibleSpeed,
}

impl MotionFault {
    /// Stable diagnostic token for firmware logs and dashboard telemetry.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "invalid_configuration",
            Self::ClockWentBackwards => "clock_went_backwards",
            Self::AzimuthNoProgress => "azimuth_no_progress",
            Self::ElevationNoProgress => "elevation_no_progress",
            Self::AzimuthWrongDirection => "azimuth_wrong_direction",
            Self::ElevationWrongDirection => "elevation_wrong_direction",
            Self::AzimuthImplausibleSpeed => "azimuth_implausible_speed",
            Self::ElevationImplausibleSpeed => "elevation_implausible_speed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AxisFault {
    NoProgress,
    WrongDirection,
    ImplausibleSpeed,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Sample {
    position_deg: f32,
    timestamp_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Progress {
    anchor_deg: f32,
    energized_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct AxisMonitor {
    applied: Option<bool>,
    last_direction: Option<bool>,
    run_started_ms: u64,
    stopped_ms: Option<u64>,
    // Include the first post-coast observation: a short pulse can do most of
    // its observable travel after the electrical output has been released.
    coast_observation_pending: bool,
    /// Furthest position in the same direction, retained across pulse gaps.
    direction_extreme_deg: Option<f32>,
    speed_anchor: Option<Sample>,
    sample: Option<Sample>,
    progress: Option<Progress>,
}

impl AxisMonitor {
    const fn new() -> Self {
        Self {
            applied: None,
            last_direction: None,
            run_started_ms: 0,
            stopped_ms: None,
            coast_observation_pending: false,
            direction_extreme_deg: None,
            speed_anchor: None,
            sample: None,
            progress: None,
        }
    }

    fn observe(
        &mut self,
        elapsed_ms: u64,
        sample: Option<Sample>,
        code_deg: f32,
        travel: AxisTravel,
        config: MotionConfig,
    ) -> Option<AxisFault> {
        if self.applied.is_some() {
            if let Some(progress) = &mut self.progress {
                progress.energized_ms = progress.energized_ms.saturating_add(elapsed_ms);
                // A late feedback delivery cannot forgive a deadline already
                // missed by the output task. At the exact deadline a fresh
                // observation may still establish timely movement.
                if progress.energized_ms > config.progress_timeout_ms {
                    return Some(AxisFault::NoProgress);
                }
            }
        }

        if let Some(sample) = sample.filter(|sample| {
            self.sample
                .is_none_or(|last| sample.timestamp_ms > last.timestamp_ms)
        }) {
            let noise_deg = code_deg * f32::from(config.noise_codes);
            if self.last_direction.is_some() {
                // Physical speed remains bounded during startup, coast and idle.
                // Preserve the longer baseline across every output transition.
                for baseline in [self.sample, self.speed_anchor].into_iter().flatten() {
                    let elapsed_s = (sample.timestamp_ms - baseline.timestamp_ms) as f32 / 1_000.0;
                    let travelled = (sample.position_deg - baseline.position_deg).abs();
                    if travelled > travel.max_speed_deg_per_s * elapsed_s + noise_deg {
                        return Some(AxisFault::ImplausibleSpeed);
                    }
                }
            }
            if self
                .speed_anchor
                .is_none_or(|anchor| sample.timestamp_ms - anchor.timestamp_ms >= SPEED_WINDOW_MS)
            {
                self.speed_anchor = Some(sample);
            }
            let expected_motion = self.applied.is_some()
                || self.stopped_ms.is_some_and(|stopped| {
                    sample.timestamp_ms <= stopped.saturating_add(travel.coast_ms)
                });
            let coast_observation = self.coast_observation_pending
                && self.stopped_ms.is_some_and(|stopped| {
                    sample.timestamp_ms
                        <= stopped
                            .saturating_add(travel.coast_ms)
                            .saturating_add(crate::feedback::FEEDBACK_MAX_AGE_MS)
                });
            if expected_motion || coast_observation {
                if let Some(increases_angle) = self.last_direction {
                    let sign = if increases_angle { 1.0 } else { -1.0 };
                    if let Some(extreme) = self.direction_extreme_deg {
                        if (sample.position_deg - extreme) * sign < -noise_deg {
                            return Some(AxisFault::WrongDirection);
                        }
                    }
                    if self
                        .direction_extreme_deg
                        .is_none_or(|extreme| (sample.position_deg - extreme) * sign > 0.0)
                    {
                        self.direction_extreme_deg = Some(sample.position_deg);
                    }
                }
            }
            if expected_motion || coast_observation {
                // Absolute displacement is intentional: reversal must not
                // erase accumulated stalled-motor time. Wrong-direction
                // displacement is checked independently above, without a per-pulse blind window.
                if sample.timestamp_ms >= self.run_started_ms {
                    if let Some(progress) = &mut self.progress {
                        let moved_deg = (sample.position_deg - progress.anchor_deg).abs();
                        if moved_deg >= code_deg * f32::from(config.progress_codes) {
                            progress.anchor_deg = sample.position_deg;
                            progress.energized_ms = 0;
                        }
                    }
                }
            }
            if self.stopped_ms.is_some_and(|stopped| {
                sample.timestamp_ms >= stopped.saturating_add(travel.coast_ms)
            }) {
                self.coast_observation_pending = false;
            }
            self.sample = Some(sample);
        }

        (self.applied.is_some()
            && self
                .progress
                .is_some_and(|p| p.energized_ms >= config.progress_timeout_ms))
        .then_some(AxisFault::NoProgress)
    }

    fn applied(&mut self, direction: Option<bool>, now_ms: u64) {
        if self.applied == direction {
            return;
        }
        if self.applied.is_some() {
            self.stopped_ms = Some(now_ms);
            self.coast_observation_pending = true;
        }
        self.applied = direction;
        if direction.is_some() {
            self.run_started_ms = now_ms;
            if direction != self.last_direction {
                // Actuator configuration requires reversal hold >= coast plus
                // maximum feedback age, so this baseline cannot contain motion
                // in the old direction after its timestamp.
                self.last_direction = direction;
                self.direction_extreme_deg = self.sample.map(|sample| sample.position_deg);
            }
            if self.progress.is_none() {
                self.progress = self.sample.map(|sample| Progress {
                    anchor_deg: sample.position_deg,
                    energized_ms: 0,
                });
            }
        }
    }

    fn deadline_ms(self, now_ms: u64, config: MotionConfig) -> Option<u64> {
        self.applied?;
        let remaining_ms = config
            .progress_timeout_ms
            .saturating_sub(self.progress?.energized_ms);
        Some(now_ms.saturating_add(remaining_ms))
    }
}

/// Output-owner state; kept private to the actuator so callers cannot forget
/// to account for a GPIO transition or accidentally reset just one monitor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MotionMonitor {
    config: MotionConfig,
    travel: TravelProfile,
    azimuth: AxisMonitor,
    elevation: AxisMonitor,
    serviced_ms: Option<u64>,
    fault: Option<MotionFault>,
}

impl MotionMonitor {
    pub(crate) const fn new(config: MotionConfig, travel: TravelProfile) -> Self {
        Self {
            config,
            travel,
            azimuth: AxisMonitor::new(),
            elevation: AxisMonitor::new(),
            serviced_ms: None,
            fault: if config.is_valid() && travel.is_valid() {
                None
            } else {
                Some(MotionFault::InvalidConfiguration)
            },
        }
    }

    /// Account for the previously applied output before changing any relays.
    pub(crate) fn observe(&mut self, interlock: &FeedbackInterlock, now_ms: u64) {
        if self.fault.is_some() {
            return;
        }
        let Some(elapsed_ms) = now_ms.checked_sub(self.serviced_ms.unwrap_or(now_ms)) else {
            self.fault = Some(MotionFault::ClockWentBackwards);
            return;
        };
        self.serviced_ms = Some(now_ms);
        let feedback = interlock
            .feedback()
            .filter(|_| interlock.motion_permitted(now_ms));
        let resolution = interlock
            .feedback_resolution_deg()
            .unwrap_or(Pointing::new(0.0, 0.0));
        let sample = |axis: fn(Pointing) -> f32| {
            feedback.map(|feedback| Sample {
                position_deg: axis(feedback.position()),
                timestamp_ms: feedback.sampled_at_ms(),
            })
        };
        let azimuth = self.azimuth.observe(
            elapsed_ms,
            sample(|p| p.azimuth_deg),
            resolution.azimuth_deg,
            self.travel.azimuth,
            self.config,
        );
        let elevation = self.elevation.observe(
            elapsed_ms,
            sample(|p| p.elevation_deg),
            resolution.elevation_deg,
            self.travel.elevation,
            self.config,
        );
        self.fault = match (azimuth, elevation) {
            (Some(AxisFault::NoProgress), _) => Some(MotionFault::AzimuthNoProgress),
            (Some(AxisFault::WrongDirection), _) => Some(MotionFault::AzimuthWrongDirection),
            (Some(AxisFault::ImplausibleSpeed), _) => Some(MotionFault::AzimuthImplausibleSpeed),
            (None, Some(AxisFault::NoProgress)) => Some(MotionFault::ElevationNoProgress),
            (None, Some(AxisFault::WrongDirection)) => Some(MotionFault::ElevationWrongDirection),
            (None, Some(AxisFault::ImplausibleSpeed)) => {
                Some(MotionFault::ElevationImplausibleSpeed)
            }
            (None, None) => None,
        };
    }

    pub(crate) fn applied(&mut self, actual: DriveCommand, now_ms: u64) {
        self.azimuth.applied(
            actual.azimuth.map(|direction| direction.increases_angle()),
            now_ms,
        );
        self.elevation.applied(
            actual
                .elevation
                .map(|direction| direction.increases_angle()),
            now_ms,
        );
    }

    pub(crate) const fn fault(&self) -> Option<MotionFault> {
        self.fault
    }

    pub(crate) fn deadline_ms(&self, now_ms: u64) -> Option<u64> {
        if self.fault.is_some() {
            return None;
        }
        // Axis budgets are current as of the last observation, not as of the
        // caller's clock. Asking for a deadline never extends that deadline.
        let observed_ms = self.serviced_ms?;
        [self.azimuth, self.elevation]
            .into_iter()
            .filter_map(|axis| axis.deadline_ms(observed_ms, self.config))
            .min()
            .map(|deadline| deadline.max(now_ms))
    }
}
