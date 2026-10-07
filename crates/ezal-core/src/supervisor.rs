//! Feedback supervision for manual movement.

use crate::ads1015::FEEDBACK_FULL_SCALE;
use crate::control::{ControlState, TimedPointing, FEEDBACK_MAX_LATENCY_MS};
use crate::interlock::FeedbackInterlock;
use crate::position::{CalibrationError, FeedbackVoltages, Pointing, PositionCalibration};
use crate::protocol::{FeedbackTelemetry, PositionTelemetry};

/// Failure reported by the hardware adapter while reading a feedback pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackReadError {
    /// The bus transaction failed without being cancelled.
    Unavailable,
    /// The read deadline elapsed; the cancelled bus must not be reused.
    TimedOut,
}

/// Publications from one acquisition.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SupervisorUpdate {
    /// Latest raw readings, including rejected measurements.
    pub raw_feedback: Option<PositionTelemetry>,
    /// Current motion gate.
    pub interlock: FeedbackInterlock,
    /// Last good calibrated position and current acquisition health.
    pub feedback: FeedbackTelemetry,
    /// Acquisition health for logging.
    pub state: ControlState,
}

/// Validates feedback without generating movement requests.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ManualSupervisor {
    calibration: PositionCalibration,
    calibration_error: Option<CalibrationError>,
    timed_out: bool,
    reported_position: Pointing,
    interlock: FeedbackInterlock,
}

impl ManualSupervisor {
    /// Start with no usable feedback.
    pub fn new(calibration: PositionCalibration) -> Self {
        Self {
            calibration,
            calibration_error: calibration.validate_for_adc(FEEDBACK_FULL_SCALE).err(),
            timed_out: false,
            reported_position: Pointing::new(0.0, 0.0),
            interlock: FeedbackInterlock::new(calibration),
        }
    }

    /// Validate a completed acquisition and publish feedback health.
    pub fn update(
        &mut self,
        now_ms: u64,
        sample_started_ms: u64,
        readings: Result<FeedbackVoltages, FeedbackReadError>,
    ) -> SupervisorUpdate {
        self.timed_out |= matches!(readings, Err(FeedbackReadError::TimedOut));
        let readings = if self.timed_out {
            Err(FeedbackReadError::TimedOut)
        } else {
            readings
        };
        let (raw, position) = match readings {
            Ok(voltages) => (
                Some(PositionTelemetry {
                    a0_mv: voltages.a0_elevation_mv,
                    a1_mv: voltages.a1_azimuth_mv,
                }),
                self.position(now_ms, sample_started_ms, voltages),
            ),
            Err(FeedbackReadError::Unavailable) => (None, Err(ControlState::FeedbackUnavailable)),
            Err(FeedbackReadError::TimedOut) => (None, Err(ControlState::FeedbackStale)),
        };
        let validated = position
            .ok()
            .map(|position| TimedPointing::new(position, sample_started_ms));
        if let Some(sample) = validated {
            self.reported_position = sample.position;
        }
        self.interlock.publish(validated);
        let state = position.err().unwrap_or(ControlState::Idle);
        SupervisorUpdate {
            raw_feedback: raw,
            interlock: self.interlock,
            feedback: FeedbackTelemetry {
                azimuth_tenths: degrees_to_tenths(self.reported_position.azimuth_deg),
                elevation_tenths: degrees_to_tenths(self.reported_position.elevation_deg),
                control_state: state,
            },
            state,
        }
    }

    /// Why installation calibration was rejected.
    pub const fn calibration_error(&self) -> Option<CalibrationError> {
        self.calibration_error
    }

    /// A cancelled transaction requires a hardware reset before bus reuse.
    pub const fn feedback_read_permitted(&self) -> bool {
        !self.timed_out
    }

    /// Calibrate a completed read, rejecting samples too late to publish.
    fn position(
        &self,
        now_ms: u64,
        sample_started_ms: u64,
        voltages: FeedbackVoltages,
    ) -> Result<Pointing, ControlState> {
        if self.calibration_error.is_some() {
            return Err(ControlState::CalibrationInvalid);
        }
        let position = self
            .calibration
            .feedback_to_position_for_adc(voltages, FEEDBACK_FULL_SCALE)
            .map_err(|_| ControlState::FeedbackInvalid)?;
        match now_ms.checked_sub(sample_started_ms) {
            None => Err(ControlState::TimestampInvalid),
            // The output interlock would expire a slower sample before its
            // replacement could be read and published.
            Some(latency_ms) if latency_ms > FEEDBACK_MAX_LATENCY_MS => {
                Err(ControlState::FeedbackStale)
            }
            Some(_) => Ok(position),
        }
    }
}

fn degrees_to_tenths(degrees: f32) -> i32 {
    (degrees * 10.0 + 0.5) as i32
}
