//! Deterministic supervision of feedback, pass sequencing, and motion control.
//!
//! The firmware supplies one paired ADC result and timestamps. This module
//! owns calibration, acquisition settling, fault precedence, the output
//! interlock's feedback, and telemetry so their interactions can be exercised
//! without an executor or peripherals.
//!
//! Construction never fails. With a calibration the feedback ADC cannot
//! supervise, a supervisor keeps reporting raw readings so the endpoints can
//! be measured again, while its [`FeedbackInterlock`] refuses all motion.

use crate::ads1015::FEEDBACK_FULL_SCALE;
use crate::control::{
    ControlConfig, ControlDecision, ControlState, TimedPointing, TrackingController,
    FEEDBACK_MAX_LATENCY_MS,
};
use crate::drive::Command;
use crate::interlock::FeedbackInterlock;
use crate::position::{CalibrationError, FeedbackVoltages, Pointing, PositionCalibration};
use crate::protocol::{PositionTelemetry, TrackingMode, TrackingTelemetry};
use crate::simulation::{MetopPassScheduler, PassPhase, PassSample, TrackingSequence};

/// Failure reported by the hardware adapter while reading a feedback pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackReadError {
    /// The bus transaction failed without being cancelled.
    Unavailable,
    /// The read deadline elapsed; the cancelled bus must not be reused.
    TimedOut,
}

/// Publications and actuator decision from one complete supervision tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SupervisorUpdate {
    /// Raw readings, including pairs rejected by calibration; absent on read failure.
    pub raw_feedback: Option<PositionTelemetry>,
    /// Output interlock for this supervisor's calibration, holding the fresh
    /// calibrated position, if any. Publish it to the actuator unchanged.
    pub interlock: FeedbackInterlock,
    /// Coherent target, position, phase, and fault report.
    pub tracking: TrackingTelemetry,
    /// Control policy and the health state explaining it.
    pub decision: ControlDecision,
    /// Command to submit to the actuator this tick. Every movement refresh is
    /// submitted, but an unchanged release is not repeated. Manual supervision
    /// never submits: its interlock enforces faults and the browser owns motion.
    pub command: Option<Command>,
}

/// Autonomous feedback and tracking policy, with no hardware dependencies.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackingSupervisor {
    feedback: FeedbackSupervisor,
    /// Absent after a rejected calibration: no pass is ever attempted.
    sequence: Option<TrackingSequence>,
    controller: TrackingController,
    last_command: Option<Command>,
}

impl TrackingSupervisor {
    /// Begin acquiring the first pass, or report a rejected calibration.
    pub fn new(calibration: PositionCalibration, now_ms: u64) -> Self {
        let feedback = FeedbackSupervisor::new(calibration);
        let sequence = MetopPassScheduler::for_calibration(calibration)
            .ok()
            .filter(|_| feedback.calibration_error.is_none())
            .map(|scheduler| TrackingSequence::new(scheduler, now_ms));
        Self {
            feedback,
            sequence,
            controller: TrackingController::new(ControlConfig::for_calibration(calibration)),
            last_command: None,
        }
    }

    /// Why the installed calibration was rejected, if it was.
    pub const fn calibration_error(&self) -> Option<CalibrationError> {
        self.feedback.calibration_error
    }

    /// Whether the firmware may start another transaction on the feedback bus.
    /// A timed-out transaction inhibits further reads and motion until reset.
    pub const fn feedback_read_permitted(&self) -> bool {
        !self.feedback.timed_out
    }

    /// Restart acquisition following a network interruption without resetting
    /// feedback health or any latched fault. Call while offline and once on
    /// recovery; otherwise offline settling could be mistaken for a completed
    /// acquisition. The pass index and installed calibration stay intact.
    pub fn restart_tracking(&mut self, now_ms: u64) {
        if let Some(sequence) = self.sequence.as_mut() {
            sequence.reacquire(now_ms);
        }
        self.controller =
            TrackingController::new(ControlConfig::for_calibration(self.feedback.calibration));
        self.last_command = None;
    }

    /// Process a feedback pair using the time after both reads completed.
    ///
    /// Only fresh, valid feedback contributes to acquisition settling or
    /// replaces the reported position. A read timeout latches until reset;
    /// acquisition timeout takes precedence over other faults in telemetry.
    pub fn update(
        &mut self,
        now_ms: u64,
        sample_started_ms: u64,
        readings: Result<FeedbackVoltages, FeedbackReadError>,
    ) -> SupervisorUpdate {
        let feedback = self.feedback.update(now_ms, sample_started_ms, readings);
        let (pass, decision) = match self.sequence.as_mut() {
            None => (
                FAULT_SAMPLE,
                ControlDecision {
                    command: Command::Inhibit,
                    state: feedback.fault.unwrap_or(ControlState::CalibrationInvalid),
                },
            ),
            Some(sequence) => {
                let pass =
                    sequence.update(now_ms, feedback.validated.map(|sample| sample.position));
                let target = pass.target.map(|target| TimedPointing::new(target, now_ms));
                let decision = if pass.phase == PassPhase::Fault {
                    ControlDecision {
                        command: Command::Inhibit,
                        state: ControlState::AcquisitionTimeout,
                    }
                } else if let Some(fault) = feedback.fault {
                    // Reset controller hysteresis on a fault, including during a pause.
                    let _ = self.controller.update(now_ms, target, None);
                    ControlDecision {
                        command: Command::Inhibit,
                        state: fault,
                    }
                } else {
                    self.controller.update(now_ms, target, feedback.validated)
                };
                (pass, decision)
            }
        };
        SupervisorUpdate {
            raw_feedback: feedback.raw,
            interlock: self.feedback.interlock,
            tracking: tracking_telemetry(
                TrackingMode::HardwareWalkingSkeleton,
                pass,
                self.feedback.reported_position,
                decision.state,
            ),
            command: self.submission(decision.command),
            decision,
        }
    }

    /// Refresh every movement lease; do not resubmit an unchanged release.
    fn submission(&mut self, command: Command) -> Option<Command> {
        let refresh = matches!(
            command,
            Command::Drive(drive) | Command::Jog(drive) if drive.is_active()
        );
        let changed = self.last_command.replace(command) != Some(command);
        (refresh || changed).then_some(command)
    }
}

/// Manual-mode feedback health, with the same calibration and read deadlines
/// as autonomous tracking. Browser ownership and endpoint checks are applied
/// by the actuator using the returned interlock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ManualSupervisor {
    feedback: FeedbackSupervisor,
}

impl ManualSupervisor {
    /// Supervise feedback for browser motion, or report a rejected calibration.
    pub fn new(calibration: PositionCalibration) -> Self {
        Self {
            feedback: FeedbackSupervisor::new(calibration),
        }
    }

    /// Why the installed calibration was rejected, if it was.
    pub const fn calibration_error(&self) -> Option<CalibrationError> {
        self.feedback.calibration_error
    }

    /// Whether another ADC read is safe after any previous cancellation.
    pub const fn feedback_read_permitted(&self) -> bool {
        !self.feedback.timed_out
    }

    /// Validate the raw pair and publish feedback readiness. A healthy manual
    /// tick reports idle control policy; the browser supplies actual commands.
    pub fn update(
        &mut self,
        now_ms: u64,
        sample_started_ms: u64,
        readings: Result<FeedbackVoltages, FeedbackReadError>,
    ) -> SupervisorUpdate {
        let feedback = self.feedback.update(now_ms, sample_started_ms, readings);
        let decision = ControlDecision {
            command: if feedback.fault.is_some() {
                Command::Inhibit
            } else {
                Command::Stop
            },
            state: feedback.fault.unwrap_or(ControlState::Idle),
        };
        SupervisorUpdate {
            raw_feedback: feedback.raw,
            interlock: self.feedback.interlock,
            tracking: tracking_telemetry(
                TrackingMode::Manual,
                PassSample {
                    pass_index: 0,
                    phase: PassPhase::Pause,
                    phase_remaining_ms: 0,
                    target: None,
                },
                self.feedback.reported_position,
                decision.state,
            ),
            decision,
            command: None,
        }
    }
}

const FAULT_SAMPLE: PassSample = PassSample {
    pass_index: 0,
    phase: PassPhase::Fault,
    phase_remaining_ms: 0,
    target: None,
};

/// Common hardware feedback policy for both modes.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FeedbackSupervisor {
    calibration: PositionCalibration,
    calibration_error: Option<CalibrationError>,
    timed_out: bool,
    reported_position: Pointing,
    interlock: FeedbackInterlock,
}

struct FeedbackUpdate {
    raw: Option<PositionTelemetry>,
    validated: Option<TimedPointing>,
    fault: Option<ControlState>,
}

impl FeedbackSupervisor {
    fn new(calibration: PositionCalibration) -> Self {
        Self {
            calibration,
            calibration_error: calibration.validate_for_adc(FEEDBACK_FULL_SCALE).err(),
            timed_out: false,
            reported_position: Pointing::new(0.0, 0.0),
            interlock: FeedbackInterlock::new(calibration),
        }
    }

    fn update(
        &mut self,
        now_ms: u64,
        sample_started_ms: u64,
        readings: Result<FeedbackVoltages, FeedbackReadError>,
    ) -> FeedbackUpdate {
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
        FeedbackUpdate {
            raw,
            validated,
            fault: position.err(),
        }
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

fn tracking_telemetry(
    mode: TrackingMode,
    pass: PassSample,
    position: Pointing,
    state: ControlState,
) -> TrackingTelemetry {
    TrackingTelemetry {
        mode,
        pass_index: pass.pass_index,
        phase: pass.phase,
        phase_remaining_ms: pass.phase_remaining_ms,
        azimuth_tenths: degrees_to_tenths(position.azimuth_deg),
        elevation_tenths: degrees_to_tenths(position.elevation_deg),
        target_azimuth_tenths: pass
            .target
            .map(|target| degrees_to_tenths(target.azimuth_deg)),
        target_elevation_tenths: pass
            .target
            .map(|target| degrees_to_tenths(target.elevation_deg)),
        control_state: state,
    }
}

fn degrees_to_tenths(degrees: f32) -> i32 {
    (degrees * 10.0 + 0.5) as i32
}
