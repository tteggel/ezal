use super::{AxisProfile, MotorProfile, SimulationError};
use crate::drive::{AzimuthDirection, Command, DriveCommand, ElevationDirection, RelayConfig};
use crate::feedback::{FeedbackSample, FEEDBACK_MAX_AGE_MS};
use crate::position::Pointing;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct AxisFollower {
    active: Option<bool>,
    started_ms: u64,
    stopped_ms: Option<u64>,
    releasing: bool,
}

impl AxisFollower {
    fn observe(&mut self, active: Option<bool>, now_ms: u64) {
        if active != self.active {
            if self.active.is_some() {
                self.stopped_ms = Some(now_ms);
            }
            if active.is_some() {
                self.started_ms = now_ms;
            }
            self.releasing = false;
            self.active = active;
        }
    }

    fn request(
        &mut self,
        error_and_velocity: [f32; 2],
        sample_ms: u64,
        now_ms: u64,
        profile: AxisProfile,
        pulse_ms: u64,
        deadband: f32,
    ) -> Option<bool> {
        let [error, velocity] = error_and_velocity;
        let rate = profile.rate_deg_per_s;
        if let Some(increasing) = self.active {
            // A requested release must finish its minimum pulse without a newer
            // target repeatedly undoing the actuator's pending release.
            if self.releasing {
                return None;
            }
            let stop_ms = now_ms.max(self.started_ms.saturating_add(pulse_ms));
            let moving_from = sample_ms.max(self.started_ms.saturating_add(profile.startup_ms));
            let travel_ms = stop_ms
                .saturating_sub(moving_from)
                .saturating_add(profile.coast_ms);
            let sign = if increasing { 1.0 } else { -1.0 };
            let future_error = error
                + velocity * (stop_ms - now_ms).saturating_add(profile.coast_ms) as f32 / 1000.0;
            let remaining = future_error * sign - rate * travel_ms as f32 / 1000.0;
            if remaining > deadband {
                return Some(increasing);
            }
            self.releasing = true;
            return None;
        }
        // Do not reuse a pre-release measurement to start the next correction.
        if self
            .stopped_ms
            .is_some_and(|stopped| sample_ms < stopped.saturating_add(profile.coast_ms))
        {
            return None;
        }
        // A fixed-speed minimum pulse should improve the predicted residual.
        // Do not subtract uncertain backlash and thereby encourage reversals.
        let pulse_travel = rate
            * pulse_ms
                .saturating_sub(profile.startup_ms)
                .saturating_add(profile.coast_ms) as f32
            / 1000.0;
        let future_error =
            error + velocity * pulse_ms.saturating_add(profile.coast_ms) as f32 / 1000.0;
        (future_error.abs() > 0.5 * pulse_travel + deadband).then_some(future_error > 0.0)
    }
}

/// Stateful feedback follower for a continuous mechanical route.
///
/// Accounts for minimum pulse size, sample age, startup and coast, and waits
/// for post-coast feedback before another correction. Target velocity comes
/// from consecutive route positions, never from simulated plant ground truth.
/// Use identical settings and output observations in simulation and execution.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FollowingController {
    reference: MotorProfile,
    deadband_deg: f32,
    minimum_pulse_ms: u64,
    axes: [AxisFollower; 2],
    previous_target: Option<(Pointing, u64)>,
    previous_ms: Option<u64>,
}

impl FollowingController {
    /// Build against the output policy used by the simulation/execution owner.
    pub fn new(
        reference: MotorProfile,
        deadband_deg: f32,
        timing: RelayConfig,
    ) -> Result<Self, SimulationError> {
        if !reference.is_valid() {
            return Err(SimulationError::InvalidProfile);
        }
        if !deadband_deg.is_finite()
            || !(0.0..=3.0).contains(&deadband_deg)
            || !timing.is_valid()
            || reference.azimuth.rate_deg_per_s <= 0.0
            || reference.elevation.rate_deg_per_s <= 0.0
            || reference.azimuth.startup_ms >= timing.min_active_ms
            || reference.elevation.startup_ms >= timing.min_active_ms
        {
            return Err(SimulationError::InvalidController);
        }
        Ok(Self {
            reference,
            deadband_deg,
            minimum_pulse_ms: timing.min_active_ms,
            axes: [AxisFollower::default(); 2],
            previous_target: None,
            previous_ms: None,
        })
    }

    /// Record actual output at its original transition instant after every
    /// actuator service, including cancellation and faults.
    pub fn observe_output(&mut self, applied: DriveCommand, now_ms: u64) {
        self.axes[0].observe(applied.azimuth.map(|d| d.increases_angle()), now_ms);
        self.axes[1].observe(applied.elevation.map(|d| d.increases_angle()), now_ms);
    }

    /// Discard target velocity while retaining physical output/coast history.
    pub fn clear_target(&mut self) {
        self.previous_target = None;
    }

    pub(super) const fn minimum_pulse_ms(&self) -> u64 {
        self.minimum_pulse_ms
    }

    /// Follow with current feedback. Invalid/backwards time inhibits output;
    /// cancellation and LOS must also send an immediate `Inhibit` directly.
    pub fn command(
        &mut self,
        target: Pointing,
        feedback: Option<FeedbackSample>,
        applied: DriveCommand,
        now_ms: u64,
    ) -> Command {
        self.observe_output(applied, now_ms);
        let valid_clock = self.previous_ms.is_none_or(|previous| now_ms >= previous);
        self.previous_ms = Some(now_ms);
        let Some(sample) = feedback.filter(|sample| {
            now_ms
                .checked_sub(sample.sampled_at_ms())
                .is_some_and(|age| age <= FEEDBACK_MAX_AGE_MS)
        }) else {
            self.clear_target();
            return Command::Inhibit;
        };
        if !valid_clock || !target.is_valid() {
            self.clear_target();
            return Command::Inhibit;
        }
        let mut velocity = [0.0; 2];
        if let Some((previous, time)) = self.previous_target {
            if now_ms > time && now_ms - time <= FEEDBACK_MAX_AGE_MS {
                let dt = (now_ms - time) as f32 / 1000.0;
                velocity = [
                    (target.azimuth_deg - previous.azimuth_deg) / dt,
                    (target.elevation_deg - previous.elevation_deg) / dt,
                ];
                // A discontinuous caller target is a slew, not a velocity estimate.
                if velocity.iter().any(|rate| rate.abs() > 20.0) {
                    velocity = [0.0; 2];
                }
            }
        }
        self.previous_target = Some((target, now_ms));
        let position = sample.position();
        let request = [
            self.axes[0].request(
                [target.azimuth_deg - position.azimuth_deg, velocity[0]],
                sample.sampled_at_ms(),
                now_ms,
                self.reference.azimuth,
                self.minimum_pulse_ms,
                self.deadband_deg,
            ),
            self.axes[1].request(
                [target.elevation_deg - position.elevation_deg, velocity[1]],
                sample.sampled_at_ms(),
                now_ms,
                self.reference.elevation,
                self.minimum_pulse_ms,
                self.deadband_deg,
            ),
        ];
        Command::Drive(DriveCommand {
            azimuth: request[0].map(|up| {
                if up {
                    AzimuthDirection::Clockwise
                } else {
                    AzimuthDirection::CounterClockwise
                }
            }),
            elevation: request[1].map(|up| {
                if up {
                    ElevationDirection::Up
                } else {
                    ElevationDirection::Down
                }
            }),
        })
    }
}
