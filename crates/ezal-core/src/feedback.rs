//! Calibrated observations and acquisition health, independent of telemetry.

use crate::ads1015::FEEDBACK_FULL_SCALE;
use crate::interlock::FeedbackInterlock;
use crate::position::{
    CalibrationError, FeedbackVoltages, InstalledCalibration, Pointing, PositionCalibration,
};

/// Maximum lifetime of a sample at the physical output boundary.
pub const FEEDBACK_MAX_AGE_MS: u64 = 500;
/// Deadline for completing a paired ADC transaction before cancelling it.
pub const FEEDBACK_READ_TIMEOUT_MS: u64 = 500;
/// Delay between publishing a sample and starting the next acquisition.
pub const FEEDBACK_PERIOD_MS: u64 = 100;
/// Maximum completed-read latency that leaves time for a replacement sample.
pub const FEEDBACK_MAX_LATENCY_MS: u64 = 150;
const FEEDBACK_SCHEDULING_MARGIN_MS: u64 = 100;

const _: () = assert!(
    2 * FEEDBACK_MAX_LATENCY_MS + FEEDBACK_PERIOD_MS + FEEDBACK_SCHEDULING_MARGIN_MS
        <= FEEDBACK_MAX_AGE_MS
);

/// A calibrated, bounded observation. Freshness is checked separately at use.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FeedbackSample {
    position: Pointing,
    sampled_at_ms: u64,
}

impl FeedbackSample {
    /// Calibrate a raw pair without changing its original acquisition timestamp.
    pub fn from_voltages(
        calibration: InstalledCalibration,
        voltages: FeedbackVoltages,
        sampled_at_ms: u64,
    ) -> Result<Self, CalibrationError> {
        Ok(Self {
            position: calibration.feedback_to_position(voltages)?,
            sampled_at_ms,
        })
    }

    /// Calibrated position inside the installation's mechanical bounds.
    pub const fn position(self) -> Pointing {
        self.position
    }

    /// Monotonic time before reading the oldest channel in this observation.
    pub const fn sampled_at_ms(self) -> u64 {
        self.sampled_at_ms
    }
}

/// Outcome of the latest acquisition, separate from permission to move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackHealth {
    /// A usable observation was published.
    Healthy,
    /// The bus returned an error without cancelling its transaction.
    Unavailable,
    /// A transaction was cancelled; its resource owner requires reset.
    ReadTimedOut,
    /// A completed acquisition took too long to publish safely.
    Stale,
    /// Feedback was outside the observable calibrated range.
    Invalid,
    /// Acquisition began in the future relative to the supplied clock.
    TimestampInvalid,
    /// Installation settings cannot be supervised by this ADC.
    CalibrationInvalid,
}

impl FeedbackHealth {
    /// Stable diagnostic token for logs.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Unavailable => "no-feedback",
            Self::ReadTimedOut => "read-timeout",
            Self::Stale => "feedback-stale",
            Self::Invalid => "feedback-invalid",
            Self::TimestampInvalid => "timestamp-invalid",
            Self::CalibrationInvalid => "calibration-invalid",
        }
    }
}

/// Failure reported by the hardware adapter for a paired read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackReadError {
    /// An uncancelled transaction failed and may be retried.
    Unavailable,
    /// Cancellation prohibits reusing this bus until hardware reset.
    TimedOut,
}

/// Read lifecycle held by the owner of the ADC, never by the feedback monitor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AcquisitionState {
    cancelled: bool,
}

impl AcquisitionState {
    /// Whether the resource owner may begin another transaction.
    pub const fn read_permitted(self) -> bool {
        !self.cancelled
    }

    /// Record cancellation and reject subsequent results from the same resource.
    pub fn complete(
        &mut self,
        result: Result<FeedbackVoltages, FeedbackReadError>,
    ) -> Result<FeedbackVoltages, FeedbackReadError> {
        self.cancelled |= matches!(result, Err(FeedbackReadError::TimedOut));
        if self.cancelled {
            Err(FeedbackReadError::TimedOut)
        } else {
            result
        }
    }
}

/// Last available diagnostics and current health; neither grants movement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FeedbackSnapshot {
    /// Most recent completed raw pair, including rejected observations.
    pub raw: Option<FeedbackVoltages>,
    /// Last accepted sample, retained for display after a failed acquisition.
    pub last_valid: Option<FeedbackSample>,
    /// Outcome of the latest acquisition.
    pub health: FeedbackHealth,
}

impl FeedbackSnapshot {
    /// No acquisition has completed yet.
    pub const INITIAL: Self = Self {
        raw: None,
        last_valid: None,
        health: FeedbackHealth::Unavailable,
    };
}

/// One atomic publication from the feedback monitor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FeedbackUpdate {
    /// Diagnostic observations and acquisition health.
    pub snapshot: FeedbackSnapshot,
    /// The latest usable sample with the same installation's output limits.
    pub interlock: FeedbackInterlock,
}

/// Validates observations without owning the ADC or generating movement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FeedbackMonitor {
    calibration: Result<InstalledCalibration, CalibrationError>,
    snapshot: FeedbackSnapshot,
    interlock: FeedbackInterlock,
}

impl FeedbackMonitor {
    /// Validate the installation once and retain optional startup diagnostics.
    /// Rejected calibration still allows raw readings to remain visible.
    pub fn new(calibration: PositionCalibration, initial_raw: Option<FeedbackVoltages>) -> Self {
        let calibration = InstalledCalibration::new(calibration, FEEDBACK_FULL_SCALE);
        Self {
            calibration,
            snapshot: FeedbackSnapshot {
                raw: initial_raw,
                ..FeedbackSnapshot::INITIAL
            },
            interlock: calibration
                .map(FeedbackInterlock::new)
                .unwrap_or(FeedbackInterlock::INHIBITED),
        }
    }

    /// Why the installation was rejected, if it was.
    pub fn calibration_error(&self) -> Option<CalibrationError> {
        self.calibration.err()
    }

    /// Validate one completed acquisition while retaining the last diagnostics.
    pub fn update(
        &mut self,
        now_ms: u64,
        sampled_at_ms: u64,
        readings: Result<FeedbackVoltages, FeedbackReadError>,
    ) -> FeedbackUpdate {
        let sample = match readings {
            Ok(raw) => {
                self.snapshot.raw = Some(raw);
                self.sample(now_ms, sampled_at_ms, raw)
            }
            Err(FeedbackReadError::Unavailable) => Err(FeedbackHealth::Unavailable),
            Err(FeedbackReadError::TimedOut) => Err(FeedbackHealth::ReadTimedOut),
        };
        self.snapshot.health = sample.err().unwrap_or(FeedbackHealth::Healthy);
        if let Ok(sample) = sample {
            self.snapshot.last_valid = Some(sample);
        }
        self.interlock.publish(sample.ok());
        FeedbackUpdate {
            snapshot: self.snapshot,
            interlock: self.interlock,
        }
    }

    fn sample(
        &self,
        now_ms: u64,
        sampled_at_ms: u64,
        raw: FeedbackVoltages,
    ) -> Result<FeedbackSample, FeedbackHealth> {
        let calibration = self
            .calibration
            .map_err(|_| FeedbackHealth::CalibrationInvalid)?;
        let sample = FeedbackSample::from_voltages(calibration, raw, sampled_at_ms)
            .map_err(|_| FeedbackHealth::Invalid)?;
        match now_ms.checked_sub(sampled_at_ms) {
            None => Err(FeedbackHealth::TimestampInvalid),
            Some(latency_ms) if latency_ms > FEEDBACK_MAX_LATENCY_MS => Err(FeedbackHealth::Stale),
            Some(_) => Ok(sample),
        }
    }
}
