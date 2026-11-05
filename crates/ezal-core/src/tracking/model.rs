use super::{Axis, AxisProfile, MotorProfile, SimulationError};
use crate::ads1015::FEEDBACK_FULL_SCALE;
use crate::drive::DriveCommand;
use crate::duty::{DutyConfig, DutyHistory};
use crate::position::{InstalledCalibration, Pointing, PositionCalibration};

#[derive(Clone, Copy, Debug, PartialEq)]
struct AxisModel {
    position_deg: f64,
    minimum_deg: f64,
    maximum_deg: f64,
    applied: Option<bool>,
    moving: Option<bool>,
    startup_ms: u64,
    coast_ms: u64,
    // Position within the gearbox's slack; partial take-up survives pulse gaps.
    engagement_deg: Option<f64>,
    used_ms: u64,
    idle_ms: u64,
}

impl AxisModel {
    fn new(position: f32, minimum: f32, maximum: f32, used_ms: u64) -> Self {
        Self {
            position_deg: f64::from(position),
            minimum_deg: f64::from(minimum),
            maximum_deg: f64::from(maximum),
            applied: None,
            moving: None,
            startup_ms: 0,
            coast_ms: 0,
            engagement_deg: None,
            used_ms,
            idle_ms: 0,
        }
    }

    fn advance(
        &mut self,
        direction: Option<bool>,
        elapsed_ms: u64,
        profile: AxisProfile,
        duty: DutyConfig,
        axis: Axis,
    ) -> Result<(), SimulationError> {
        if direction != self.applied {
            if self.applied.is_some() && self.coast_ms == 0 && self.moving.is_some() {
                self.coast_ms = profile.coast_ms;
            }
            if self.coast_ms == 0 {
                self.moving = None;
            }
            self.startup_ms = if direction.is_some() {
                profile.startup_ms
            } else {
                0
            };
            self.applied = direction;
        }

        if direction.is_some() {
            if elapsed_ms > duty.energized_ms - self.used_ms {
                return Err(SimulationError::DutyLimit(axis));
            }
            self.used_ms += elapsed_ms;
            self.idle_ms = 0;
        } else {
            self.idle_ms = self.idle_ms.saturating_add(elapsed_ms);
            if self.idle_ms >= duty.rest_ms {
                self.used_ms = 0;
            }
        }

        let coasting_ms = elapsed_ms.min(self.coast_ms);
        if let Some(previous_direction) = self.moving {
            self.travel(previous_direction, coasting_ms, profile, axis)?;
        }
        self.coast_ms -= coasting_ms;
        // A requested reversal waits for old-direction coast before startup.
        // This conservative serial model also handles off-times shorter than coast.
        let remaining_ms = elapsed_ms - coasting_ms;
        if self.coast_ms == 0 && (coasting_ms > 0 || direction.is_none()) {
            self.moving = None;
        }
        if let Some(direction) = direction {
            let settling_ms = remaining_ms.min(self.startup_ms);
            self.startup_ms -= settling_ms;
            let powered_travel_ms = remaining_ms - settling_ms;
            if powered_travel_ms > 0 && profile.rate_deg_per_s > 0.0 {
                self.travel(direction, powered_travel_ms, profile, axis)?;
                self.moving = Some(direction);
            }
        }
        Ok(())
    }

    fn travel(
        &mut self,
        increasing: bool,
        elapsed_ms: u64,
        profile: AxisProfile,
        axis: Axis,
    ) -> Result<(), SimulationError> {
        if elapsed_ms == 0 {
            return Ok(());
        }
        let distance = f64::from(profile.rate_deg_per_s) * elapsed_ms as f64 / 1_000.0;
        let backlash = f64::from(profile.backlash_deg);
        // Assume the first direction is already engaged; subsequent reversals
        // traverse the explicit slack. Initial gear engagement is a model assumption.
        let engagement = self
            .engagement_deg
            .get_or_insert(if increasing { backlash } else { 0.0 });
        let take_up = if increasing {
            (backlash - *engagement).min(distance)
        } else {
            (*engagement).min(distance)
        };
        let sign = if increasing { 1.0 } else { -1.0 };
        *engagement += sign * take_up;
        self.position_deg += sign * (distance - take_up);
        if !self.position_deg.is_finite()
            || self.position_deg < self.minimum_deg
            || self.position_deg > self.maximum_deg
        {
            return Err(SimulationError::TravelLimit(axis));
        }
        Ok(())
    }

    fn settled(self) -> bool {
        self.applied.is_none() && self.coast_ms == 0
    }
}

/// Constant-rate two-axis motor model with startup, coast, backlash and duty.
///
/// Position is accumulated in `f64` so changing the simulation step does not
/// introduce large `f32` integration drift. Direction leads are supplied by
/// the caller; use [`super::Simulation`] to obtain them from the actual actuator.
/// Relay timing and physical supervision are deliberately not duplicated here.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotorModel {
    profile: MotorProfile,
    azimuth: AxisModel,
    elevation: AxisModel,
    fault: Option<SimulationError>,
}

impl MotorModel {
    pub(super) fn set_initial_rest(&mut self, idle_ms: [u64; 2], engagement: [Option<bool>; 2]) {
        self.azimuth.idle_ms = idle_ms[0];
        self.elevation.idle_ms = idle_ms[1];
        self.azimuth.engagement_deg = engagement[0].map(|positive| {
            if positive {
                f64::from(self.profile.azimuth.backlash_deg)
            } else {
                0.0
            }
        });
        self.elevation.engagement_deg = engagement[1].map(|positive| {
            if positive {
                f64::from(self.profile.elevation.backlash_deg)
            } else {
                0.0
            }
        });
    }

    /// Start at rest with explicit previously consumed azimuth/elevation duty.
    /// `[0, 0]` asserts a fully rested model; it is not a reboot recovery policy.
    pub fn new(
        profile: MotorProfile,
        calibration: PositionCalibration,
        initial: Pointing,
        used_ms: [u64; 2],
    ) -> Result<Self, SimulationError> {
        if !profile.is_valid() || !DutyHistory::Known(used_ms).is_valid(profile.duty) {
            return Err(SimulationError::InvalidProfile);
        }
        let installed = InstalledCalibration::new(calibration, FEEDBACK_FULL_SCALE)
            .map_err(|_| SimulationError::InvalidCalibration)?;
        if !initial.is_valid_for(calibration) {
            return Err(SimulationError::InvalidPointing);
        }
        let minimum = installed.minimum();
        let maximum = installed.maximum();
        Ok(Self {
            profile,
            azimuth: AxisModel::new(
                initial.azimuth_deg,
                minimum.azimuth_deg,
                maximum.azimuth_deg,
                used_ms[0],
            ),
            elevation: AxisModel::new(
                initial.elevation_deg,
                minimum.elevation_deg,
                maximum.elevation_deg,
                used_ms[1],
            ),
            fault: None,
        })
    }

    /// Apply the supplied relay state for an elapsed interval. `elapsed_ms = 0`
    /// records a transition without moving time forward. A limit/duty failure
    /// latches; later calls cannot quietly resume a partially advanced model.
    pub fn advance(
        &mut self,
        applied: DriveCommand,
        elapsed_ms: u64,
    ) -> Result<(), SimulationError> {
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        let result = self
            .azimuth
            .advance(
                applied.azimuth.map(|d| d.increases_angle()),
                elapsed_ms,
                self.profile.azimuth,
                self.profile.duty,
                Axis::Azimuth,
            )
            .and_then(|()| {
                self.elevation.advance(
                    applied.elevation.map(|d| d.increases_angle()),
                    elapsed_ms,
                    self.profile.elevation,
                    self.profile.duty,
                    Axis::Elevation,
                )
            });
        self.fault = result.err();
        result
    }

    /// Actual continuous mechanical pose, including unobserved coast.
    pub fn position(&self) -> Pointing {
        Pointing::new(
            self.azimuth.position_deg as f32,
            self.elevation.position_deg as f32,
        )
    }

    /// Energized time since each axis's last complete rest.
    pub const fn used_duty_ms(&self) -> [u64; 2] {
        [self.azimuth.used_ms, self.elevation.used_ms]
    }

    /// Energized time each axis may still accumulate before its duty limit.
    pub(super) const fn remaining_duty_ms(&self) -> [u64; 2] {
        [
            self.profile.duty.energized_ms - self.azimuth.used_ms,
            self.profile.duty.energized_ms - self.elevation.used_ms,
        ]
    }

    /// Whether neither axis has active drive or remaining coast.
    pub fn is_settled(&self) -> bool {
        self.azimuth.settled() && self.elevation.settled()
    }
}
