//! Feedback and mechanical-limit checks at the final actuator boundary.
//!
//! Command admission alone cannot stop a held browser control after its last
//! feedback sample becomes stale. This gate also removes active and delayed
//! output requests whenever current feedback no longer permits them.
//!
//! A sample reports where the dish was when its read began, and an energised
//! axis keeps moving until the next sample. Each outward direction is
//! therefore refused inside an endpoint band of one ADC code, the fastest
//! travel possible since that sample began, and post-release coast.
//! [`FeedbackInterlock::deadline_ms`]
//! reports when that band reaches an energised axis, so the actuator cuts it
//! without waiting for another sample. The release/coast allowance is an
//! explicit engineering assumption until measured on the installed mechanism.

use crate::drive::{Command, DriveCommand};
use crate::feedback::{FeedbackSample, FEEDBACK_MAX_AGE_MS};
use crate::position::{InstalledCalibration, Pointing};

/// Added to one ADC code so a reading exactly one code inside an endpoint is
/// refused without depending on floating-point rounding.
const CODE_ROUNDING_MARGIN_MV: f32 = 0.5;

/// Upper travel bounds for one axis, shared by endpoint and motion supervision.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AxisTravel {
    /// Fastest supported physical rate while moving or coasting.
    pub max_speed_deg_per_s: f32,
    /// Assumed worst-case relay release and mechanical coast duration.
    pub coast_ms: u64,
}

impl AxisTravel {
    /// A finite positive rate and a representable stopping-distance allowance.
    pub const fn is_valid(self) -> bool {
        self.max_speed_deg_per_s.is_finite()
            && self.max_speed_deg_per_s > 0.0
            && (self.max_speed_deg_per_s * (self.coast_ms as f32 / 1_000.0)).is_finite()
    }
}

/// The output owner's installed upper bounds, distinct from a nominal motor model.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TravelProfile {
    /// Continuous azimuth travel bounds.
    pub azimuth: AxisTravel,
    /// Mechanical elevation travel bounds.
    pub elevation: AxisTravel,
}

impl TravelProfile {
    /// AC upper rates with margin, plus the provisional coast allowance.
    pub const DEFAULT: Self = Self {
        azimuth: AxisTravel {
            // The G-5500 manual's 58 s per 360° at 60 Hz (6.2°/s), plus margin.
            max_speed_deg_per_s: 7.0,
            coast_ms: 500,
        },
        elevation: AxisTravel {
            // The G-5500 manual's 67 s per 180° at 60 Hz (2.7°/s), plus margin.
            max_speed_deg_per_s: 3.0,
            coast_ms: 500,
        },
    };

    /// Whether both axes have usable upper travel bounds.
    pub const fn is_valid(self) -> bool {
        self.azimuth.is_valid() && self.elevation.is_valid()
    }

    /// Longest assumed release/coast interval of either axis.
    pub const fn maximum_coast_ms(self) -> u64 {
        if self.azimuth.coast_ms > self.elevation.coast_ms {
            self.azimuth.coast_ms
        } else {
            self.elevation.coast_ms
        }
    }
}

/// Latest validated feedback and the limits required for any physical motion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FeedbackInterlock {
    limits: Option<Limits>,
    feedback: Option<FeedbackSample>,
    profile: TravelProfile,
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
}

impl AxisLimits {
    fn new(min_deg: f32, max_deg: f32, code_deg: f32) -> Self {
        Self {
            min_deg,
            max_deg,
            code_deg,
        }
    }

    fn contains(self, position_deg: f32) -> bool {
        position_deg >= self.min_deg && position_deg <= self.max_deg
    }

    /// Sample age from which travel in one direction is refused.
    fn refusal_age_ms(self, position_deg: f32, increases_angle: bool, travel: AxisTravel) -> u64 {
        let room_deg = if increases_angle {
            self.max_deg - self.code_deg - position_deg
        } else {
            position_deg - self.min_deg - self.code_deg
        };
        let speed_deg_per_ms = travel.max_speed_deg_per_s / 1_000.0;
        let room_deg = room_deg - speed_deg_per_ms * travel.coast_ms as f32;
        // Truncation refuses up to a millisecond early, never late.
        if room_deg > 0.0 {
            (room_deg / speed_deg_per_ms) as u64
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
        profile: TravelProfile::DEFAULT,
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
            ),
            elevation: AxisLimits::new(
                minimum.elevation_deg,
                maximum.elevation_deg,
                resolution.elevation_deg,
            ),
        });
        Self {
            limits,
            feedback: None,
            profile: TravelProfile::DEFAULT,
        }
    }

    /// Apply the actuator's travel policy to this calibrated observation.
    /// Invalid bounds inhibit; callers cannot recover missing calibration here.
    pub fn with_travel_profile(mut self, profile: TravelProfile) -> Self {
        if profile.is_valid() {
            self.profile = profile;
        } else {
            self.limits = None;
        }
        self
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
            refusal_age_ms = refusal_age_ms.min(limits.azimuth.refusal_age_ms(
                position.azimuth_deg,
                direction.increases_angle(),
                self.profile.azimuth,
            ));
        }
        if let Some(direction) = active.elevation {
            refusal_age_ms = refusal_age_ms.min(limits.elevation.refusal_age_ms(
                position.elevation_deg,
                direction.increases_angle(),
                self.profile.elevation,
            ));
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
                    < limits.azimuth.refusal_age_ms(
                        position.azimuth_deg,
                        direction.increases_angle(),
                        self.profile.azimuth,
                    )
            }),
            elevation: drive.elevation.filter(|direction| {
                age_ms
                    < limits.elevation.refusal_age_ms(
                        position.elevation_deg,
                        direction.increases_angle(),
                        self.profile.elevation,
                    )
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
            Command::Inhibit => command,
        }
    }
}
