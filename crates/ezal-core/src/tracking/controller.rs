use super::{MotorProfile, SimulationError};
use crate::drive::{AzimuthDirection, Command, DriveCommand, ElevationDirection};
use crate::feedback::{FeedbackSample, FEEDBACK_MAX_AGE_MS};
use crate::position::Pointing;

/// Initial feedback follower for an already chosen continuous mechanical route.
///
/// Release anticipates travel during feedback age and the assumed coast time.
/// Ordinary `Drive` requests preserve the production actuator's on/off timing;
/// the controller cannot bypass that timing to obtain a shorter pulse. This
/// initial policy has no route search, velocity estimator or reacquisition.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FollowingController {
    reference: MotorProfile,
    deadband_deg: f32,
}

impl FollowingController {
    /// Build a follower with its own reference profile. Keep it fixed when
    /// comparing slower/faster plant scenarios so the test is not omniscient.
    pub fn new(reference: MotorProfile, deadband_deg: f32) -> Result<Self, SimulationError> {
        if !reference.is_valid() {
            return Err(SimulationError::InvalidProfile);
        }
        if !deadband_deg.is_finite() || !(0.0..=3.0).contains(&deadband_deg) {
            return Err(SimulationError::InvalidController);
        }
        Ok(Self {
            reference,
            deadband_deg,
        })
    }

    /// Request following at monotonic `now_ms`, or inhibit on missing/stale
    /// feedback or invalid coordinates. Cancellation/LOS must send `Inhibit`
    /// directly; they must not be implemented as an ordinary target change.
    pub fn command(
        &self,
        target: Pointing,
        feedback: Option<FeedbackSample>,
        applied: DriveCommand,
        now_ms: u64,
    ) -> Command {
        let Some(sample) = feedback else {
            return Command::Inhibit;
        };
        let Some(age_ms) = now_ms.checked_sub(sample.sampled_at_ms()) else {
            return Command::Inhibit;
        };
        if age_ms > FEEDBACK_MAX_AGE_MS || !target.is_valid() {
            return Command::Inhibit;
        }
        let position = sample.position();
        let direction = |error: f32, active: Option<bool>, rate: f32, coast_ms: u64| {
            if let Some(increasing) = active {
                let signed_error = if increasing { error } else { -error };
                let stopping_travel = f64::from(rate) * (age_ms as f64 + coast_ms as f64) / 1_000.0;
                (f64::from(signed_error) > stopping_travel + f64::from(self.deadband_deg))
                    .then_some(increasing)
            } else {
                (error.abs() > self.deadband_deg).then_some(error > 0.0)
            }
        };
        Command::Drive(DriveCommand {
            azimuth: direction(
                target.azimuth_deg - position.azimuth_deg,
                applied.azimuth.map(|d| d.increases_angle()),
                self.reference.azimuth.rate_deg_per_s,
                self.reference.azimuth.coast_ms,
            )
            .map(|increasing| {
                if increasing {
                    AzimuthDirection::Clockwise
                } else {
                    AzimuthDirection::CounterClockwise
                }
            }),
            elevation: direction(
                target.elevation_deg - position.elevation_deg,
                applied.elevation.map(|d| d.increases_angle()),
                self.reference.elevation.rate_deg_per_s,
                self.reference.elevation.coast_ms,
            )
            .map(|increasing| {
                if increasing {
                    ElevationDirection::Up
                } else {
                    ElevationDirection::Down
                }
            }),
        })
    }
}
