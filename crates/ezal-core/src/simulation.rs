//! Deterministic METOP-C pass source and acquisition sequencer.
//!
//! This is a walking skeleton, not an orbital propagator: it emits a
//! repeatable, METOP-C-labelled horizon-to-horizon target profile so every
//! downstream production component can be exercised with the real rotator and
//! ADC. A real target source can later replace [`MetopPassScheduler`] without
//! changing calibration, control, actuator safety, or telemetry.

use crate::position::{CalibrationError, Pointing, PositionCalibration};

/// Simulated satellite name reported by the scheduler.
pub const SIMULATED_SATELLITE: &str = "METOP-C";

/// Duration of each synthetic horizon-to-horizon run.
pub const METOP_PASS_DURATION_MS: u64 = 180_000;

/// Required idle interval between successive runs.
pub const BETWEEN_PASS_PAUSE_MS: u64 = 30_000;

/// Maximum time allowed to reach the next pass's starting position.
pub const ACQUIRE_TIMEOUT_MS: u64 = 120_000;

/// Time continuously inside the acquisition tolerance before a pass starts.
pub const ACQUIRE_SETTLE_MS: u64 = 1_000;

/// Per-axis error accepted while settling at the pass start.
///
/// This matches the controller's idle-to-moving engage threshold. Requiring
/// the tighter release threshold here could strand an already-idle axis in
/// the controller's intentional hysteresis band.
pub const ACQUIRE_TOLERANCE_DEG: f32 = 4.0;

/// Scheduler phase at a monotonic instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PassPhase {
    /// Dish is slewing to and settling at the next pass's first target.
    Acquiring,
    /// A target is currently crossing the simulated sky.
    Tracking,
    /// Outputs should be idle between runs.
    Pause,
    /// The start position was not acquired before its safety deadline.
    Fault,
}

impl PassPhase {
    /// Stable token for logs and telemetry.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Acquiring => "acquire",
            Self::Tracking => "pass",
            Self::Pause => "pause",
            Self::Fault => "fault",
        }
    }
}

/// One sample from the repeating pass scheduler.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PassSample {
    /// Zero-based run number since boot.
    pub pass_index: u32,
    /// Current pass/pause phase.
    pub phase: PassPhase,
    /// Time remaining in this phase.
    pub phase_remaining_ms: u64,
    /// Current pointing target; absent throughout the pause.
    pub target: Option<Pointing>,
}

/// Repeating deterministic synthetic METOP-C target source.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MetopPassScheduler {
    azimuth_start_deg: f32,
    azimuth_end_deg: f32,
    elevation_horizon_deg: f32,
    elevation_peak_deg: f32,
}

impl MetopPassScheduler {
    /// Create the stateless scheduler.
    pub const fn new() -> Self {
        Self {
            azimuth_start_deg: 25.0,
            azimuth_end_deg: 425.0,
            elevation_horizon_deg: 0.0,
            elevation_peak_deg: 82.0,
        }
    }

    /// Fit the synthetic pass inside an installed calibration's mechanical
    /// angle endpoints.
    pub fn for_calibration(calibration: PositionCalibration) -> Result<Self, CalibrationError> {
        calibration.validate()?;
        let azimuth_span_deg =
            calibration.azimuth.angle_max_deg - calibration.azimuth.angle_min_deg;
        let elevation_span_deg =
            calibration.elevation.angle_max_deg - calibration.elevation.angle_min_deg;
        let azimuth_inset_deg = (azimuth_span_deg / 18.0).min(25.0);

        Ok(Self {
            azimuth_start_deg: calibration.azimuth.angle_min_deg + azimuth_inset_deg,
            azimuth_end_deg: calibration.azimuth.angle_max_deg - azimuth_inset_deg,
            elevation_horizon_deg: calibration.elevation.angle_min_deg,
            elevation_peak_deg: calibration.elevation.angle_min_deg + elevation_span_deg.min(82.0),
        })
    }

    /// Sample the run schedule at milliseconds since boot.
    ///
    /// Successive profiles reverse azimuth direction. This starts each new
    /// run where the previous one ended, while still exercising both drive
    /// directions after the exact 30-second pause.
    pub fn sample(self, elapsed_ms: u64) -> PassSample {
        let cycle_ms = METOP_PASS_DURATION_MS + BETWEEN_PASS_PAUSE_MS;
        let pass_index_u64 = elapsed_ms / cycle_ms;
        let pass_index = pass_index_u64.min(u32::MAX as u64) as u32;
        let within_cycle_ms = elapsed_ms % cycle_ms;

        if within_cycle_ms >= METOP_PASS_DURATION_MS {
            return PassSample {
                pass_index,
                phase: PassPhase::Pause,
                phase_remaining_ms: cycle_ms - within_cycle_ms,
                target: None,
            };
        }

        PassSample {
            pass_index,
            phase: PassPhase::Tracking,
            phase_remaining_ms: METOP_PASS_DURATION_MS - within_cycle_ms,
            target: Some(self.target_for_pass(pass_index, within_cycle_ms)),
        }
    }

    /// Target at an elapsed instant within one numbered pass.
    pub fn target_for_pass(self, pass_index: u32, pass_elapsed_ms: u64) -> Pointing {
        let clamped_elapsed_ms = pass_elapsed_ms.min(METOP_PASS_DURATION_MS);
        let progress = clamped_elapsed_ms as f32 / METOP_PASS_DURATION_MS as f32;
        let rising = progress * 2.0;
        let elevation_fraction = if rising <= 1.0 { rising } else { 2.0 - rising };
        let (azimuth_start, azimuth_end) = if pass_index % 2 == 0 {
            (self.azimuth_start_deg, self.azimuth_end_deg)
        } else {
            (self.azimuth_end_deg, self.azimuth_start_deg)
        };

        Pointing::new(
            azimuth_start + (azimuth_end - azimuth_start) * progress,
            self.elevation_horizon_deg
                + (self.elevation_peak_deg - self.elevation_horizon_deg) * elevation_fraction,
        )
    }
}

impl Default for MetopPassScheduler {
    fn default() -> Self {
        Self::new()
    }
}

/// Stateful acquire → track → pause sequencing around a pass scheduler.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackingSequence {
    scheduler: MetopPassScheduler,
    pass_index: u32,
    phase: PassPhase,
    phase_started_ms: u64,
    within_tolerance_since_ms: Option<u64>,
}

impl TrackingSequence {
    /// Begin by acquiring pass zero's first target.
    pub const fn new(scheduler: MetopPassScheduler, now_ms: u64) -> Self {
        Self {
            scheduler,
            pass_index: 0,
            phase: PassPhase::Acquiring,
            phase_started_ms: now_ms,
            within_tolerance_since_ms: None,
        }
    }

    /// Advance sequencing using the latest calibrated dish position.
    pub fn update(&mut self, now_ms: u64, position: Option<Pointing>) -> PassSample {
        match self.phase {
            PassPhase::Acquiring => self.update_acquiring(now_ms, position),
            PassPhase::Tracking => self.update_tracking(now_ms),
            PassPhase::Pause => self.update_pause(now_ms),
            PassPhase::Fault => self.sample_fault(),
        }
    }

    fn update_acquiring(&mut self, now_ms: u64, position: Option<Pointing>) -> PassSample {
        let elapsed_ms = now_ms.saturating_sub(self.phase_started_ms);
        if elapsed_ms >= ACQUIRE_TIMEOUT_MS {
            self.phase = PassPhase::Fault;
            return self.sample_fault();
        }

        let target = self.scheduler.target_for_pass(self.pass_index, 0);
        let within_tolerance = position.is_some_and(|position| {
            (target.azimuth_deg - position.azimuth_deg).abs() <= ACQUIRE_TOLERANCE_DEG
                && (target.elevation_deg - position.elevation_deg).abs() <= ACQUIRE_TOLERANCE_DEG
        });

        if within_tolerance {
            let settled_since_ms = *self.within_tolerance_since_ms.get_or_insert(now_ms);
            if now_ms.saturating_sub(settled_since_ms) >= ACQUIRE_SETTLE_MS {
                self.phase = PassPhase::Tracking;
                self.phase_started_ms = now_ms;
                self.within_tolerance_since_ms = None;
                return self.update_tracking(now_ms);
            }
        } else {
            self.within_tolerance_since_ms = None;
        }

        PassSample {
            pass_index: self.pass_index,
            phase: PassPhase::Acquiring,
            phase_remaining_ms: ACQUIRE_TIMEOUT_MS - elapsed_ms,
            target: Some(target),
        }
    }

    fn update_tracking(&mut self, now_ms: u64) -> PassSample {
        let elapsed_ms = now_ms.saturating_sub(self.phase_started_ms);
        if elapsed_ms >= METOP_PASS_DURATION_MS {
            self.phase = PassPhase::Pause;
            self.phase_started_ms = now_ms;
            return self.update_pause(now_ms);
        }

        PassSample {
            pass_index: self.pass_index,
            phase: PassPhase::Tracking,
            phase_remaining_ms: METOP_PASS_DURATION_MS - elapsed_ms,
            target: Some(self.scheduler.target_for_pass(self.pass_index, elapsed_ms)),
        }
    }

    fn update_pause(&mut self, now_ms: u64) -> PassSample {
        let elapsed_ms = now_ms.saturating_sub(self.phase_started_ms);
        if elapsed_ms >= BETWEEN_PASS_PAUSE_MS {
            self.pass_index = self.pass_index.saturating_add(1);
            self.phase = PassPhase::Acquiring;
            self.phase_started_ms = now_ms;
            self.within_tolerance_since_ms = None;
            return self.update_acquiring(now_ms, None);
        }

        PassSample {
            pass_index: self.pass_index,
            phase: PassPhase::Pause,
            phase_remaining_ms: BETWEEN_PASS_PAUSE_MS - elapsed_ms,
            target: None,
        }
    }

    const fn sample_fault(&self) -> PassSample {
        PassSample {
            pass_index: self.pass_index,
            phase: PassPhase::Fault,
            phase_remaining_ms: 0,
            target: None,
        }
    }
}
