//! Feedback timing and health shared by sampling and the output boundary.

use crate::position::Pointing;

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

/// Health of the latest feedback acquisition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlState {
    /// A usable sample was published.
    Idle,
    /// No sample could be read.
    FeedbackUnavailable,
    /// The sample or read is too old.
    FeedbackStale,
    /// The sample is outside the observable calibrated range.
    FeedbackInvalid,
    /// The acquisition timestamp is in the future.
    TimestampInvalid,
    /// The installed calibration cannot be observed by this ADC.
    CalibrationInvalid,
}

impl ControlState {
    /// Stable diagnostic name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::FeedbackUnavailable => "no-feedback",
            Self::FeedbackStale => "feedback-stale",
            Self::FeedbackInvalid => "feedback-invalid",
            Self::TimestampInvalid => "timestamp-invalid",
            Self::CalibrationInvalid => "calibration-invalid",
        }
    }
}
