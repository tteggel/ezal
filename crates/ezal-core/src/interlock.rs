//! Feedback and mechanical-limit checks at the final actuator boundary.
//!
//! Command admission alone cannot stop a held browser control after its last
//! feedback sample becomes stale. This gate also removes active and delayed
//! output requests whenever current feedback no longer permits them.
//!
//! A sample reports where the dish was when its read began, and an energised
//! axis keeps moving until the next sample. Each outward direction is
//! therefore refused inside an endpoint band of one ADC code plus the fastest
//! travel possible since that sample began. [`FeedbackInterlock::deadline_ms`]
//! reports when that band reaches an energised axis, so the actuator cuts it
//! without waiting for another sample. Relay release time and coasting after
//! the cut are outside this model.

use crate::drive::{
    Command, DriveCommand, AZIMUTH_MAX_SPEED_DEG_PER_S, ELEVATION_MAX_SPEED_DEG_PER_S,
};
use crate::feedback::{FeedbackSample, FEEDBACK_MAX_AGE_MS};
use crate::position::{InstalledCalibration, Pointing};

/// Added to one ADC code so a reading exactly one code inside an endpoint is
/// refused without depending on floating-point rounding.
const CODE_ROUNDING_MARGIN_MV: f32 = 0.5;

/// Latest validated feedback and the limits required for any physical motion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FeedbackInterlock {
    limits: Option<Limits>,
    feedback: Option<FeedbackSample>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Limits {
    azimuth: AxisLimits,
    elevation: AxisLimits,
}

/// One axis's endpoint band, derived once from a validated calibration.
#[derive(Clone, Copy, Debug, PartialEq)]
struct AxisLimits {
    min_deg: f32,
    max_deg: f32,
    /// One ADC code plus the rounding margin, in degrees.
    code_deg: f32,
    speed_deg_per_ms: f32,
}

impl AxisLimits {
    fn new(min_deg: f32, max_deg: f32, code_deg: f32, speed_deg_per_s: f32) -> Self {
        Self {
            min_deg,
            max_deg,
            code_deg,
            speed_deg_per_ms: speed_deg_per_s / 1_000.0,
        }
    }

    fn contains(self, position_deg: f32) -> bool {
        position_deg >= self.min_deg && position_deg <= self.max_deg
    }

    /// Sample age from which travel in one direction is refused.
    fn refusal_age_ms(self, position_deg: f32, increases_angle: bool) -> u64 {
        let room_deg = if increases_angle {
            self.max_deg - self.code_deg - position_deg
        } else {
            position_deg - self.min_deg - self.code_deg
        };
        // Truncation refuses up to a millisecond early, never late.
        if room_deg > 0.0 {
            (room_deg / self.speed_deg_per_ms) as u64
        } else {
            0
        }
    }
}

impl FeedbackInterlock {
    /// A gate that permits no motion, used before usable feedback is published.
    pub const INHIBITED: Self = Self {
        limits: None,
        feedback: None,
    };

    /// Derive output limits from the same validated model used to calibrate samples.
    pub fn new(calibration: InstalledCalibration) -> Self {
        let minimum = calibration.minimum();
        let maximum = calibration.maximum();
        let resolution = calibration.resolution_deg(CODE_ROUNDING_MARGIN_MV);
        let limits = Some(Limits {
            azimuth: AxisLimits::new(
                minimum.azimuth_deg,
                maximum.azimuth_deg,
                resolution.azimuth_deg,
                AZIMUTH_MAX_SPEED_DEG_PER_S,
            ),
            elevation: AxisLimits::new(
                minimum.elevation_deg,
                maximum.elevation_deg,
                resolution.elevation_deg,
                ELEVATION_MAX_SPEED_DEG_PER_S,
            ),
        });
        Self {
            limits,
            feedback: None,
        }
    }

    /// Publish validated feedback, or invalidate the gate after a read fault.
    pub fn publish(&mut self, feedback: Option<FeedbackSample>) {
        self.feedback = feedback;
    }

    /// Latest published feedback, whether or not it is still fresh.
    pub const fn feedback(&self) -> Option<FeedbackSample> {
        self.feedback
    }

    /// Conservative angular size of one feedback code, including the same
    /// rounding allowance used by the endpoint gate. Motion supervision uses
    /// this calibration-derived tolerance instead of treating ADC quantization
    /// as either proof of movement or a direction fault.
    pub(crate) fn feedback_resolution_deg(&self) -> Option<Pointing> {
        let limits = self.limits?;
        Some(Pointing::new(
            limits.azimuth.code_deg,
            limits.elevation.code_deg,
        ))
    }

    /// Limits, position, and age of feedback that currently permits motion.
    fn current(&self, now_ms: u64) -> Option<(Limits, Pointing, u64)> {
        let limits = self.limits?;
        let feedback = self.feedback?;
        let age_ms = now_ms.checked_sub(feedback.sampled_at_ms())?;
        let position = feedback.position();
        (age_ms <= FEEDBACK_MAX_AGE_MS
            && limits.azimuth.contains(position.azimuth_deg)
            && limits.elevation.contains(position.elevation_deg))
        .then_some((limits, position, age_ms))
    }

    /// Whether feedback is fresh, valid, and inside the calibrated endpoints.
    pub fn motion_permitted(&self, now_ms: u64) -> bool {
        self.current(now_ms).is_some()
    }

    /// First instant at which current feedback stops permitting an `active`
    /// output: the sample's expiry or an outward endpoint band, if earlier.
    ///
    /// Idle outputs need no deadline because every activation is checked when
    /// it happens. No deadline is returned once feedback forbids motion, so an
    /// actuator that has processed the inhibition can sleep.
    pub fn deadline_ms(&self, active: DriveCommand, now_ms: u64) -> Option<u64> {
        if !active.is_active() {
            return None;
        }
        let (limits, position, _) = self.current(now_ms)?;
        let mut refusal_age_ms = FEEDBACK_MAX_AGE_MS + 1;
        if let Some(direction) = active.azimuth {
            refusal_age_ms = refusal_age_ms.min(
                limits
                    .azimuth
                    .refusal_age_ms(position.azimuth_deg, direction.increases_angle()),
            );
        }
        if let Some(direction) = active.elevation {
            refusal_age_ms = refusal_age_ms.min(
                limits
                    .elevation
                    .refusal_age_ms(position.elevation_deg, direction.increases_angle()),
            );
        }
        self.feedback?.sampled_at_ms().checked_add(refusal_age_ms)
    }

    /// Remove directions that require faulty feedback or that could already
    /// have reached a calibrated endpoint band since the sample began.
    pub fn guard_drive(&self, drive: DriveCommand, now_ms: u64) -> DriveCommand {
        let Some((limits, position, age_ms)) = self.current(now_ms) else {
            return DriveCommand::IDLE;
        };
        DriveCommand {
            azimuth: drive.azimuth.filter(|direction| {
                age_ms
                    < limits
                        .azimuth
                        .refusal_age_ms(position.azimuth_deg, direction.increases_angle())
            }),
            elevation: drive.elevation.filter(|direction| {
                age_ms
                    < limits
                        .elevation
                        .refusal_age_ms(position.elevation_deg, direction.increases_angle())
            }),
        }
    }

    /// Admit permitted movement; stale or invalid feedback inhibits it.
    ///
    /// A request whose directions are all refused remains an ordinary release
    /// of its kind, so a released axis's relay timing never depends on the
    /// other axis. The actuator separately cuts refused energised axes.
    pub fn guard_command(&self, command: Command, now_ms: u64) -> Command {
        match command {
            Command::Drive(_) | Command::Jog(_) if !self.motion_permitted(now_ms) => {
                Command::Inhibit
            }
            Command::Drive(drive) => Command::Drive(self.guard_drive(drive, now_ms)),
            Command::Jog(drive) => Command::Jog(self.guard_drive(drive, now_ms)),
            Command::Stop | Command::Inhibit => command,
        }
    }
}
