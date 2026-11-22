//! Motion settings for the installed rotator. Its feedback calibration is
//! [`HARD_CODED_CALIBRATION`](crate::position::HARD_CODED_CALIBRATION). The
//! firmware runs these values, and the end-to-end host test rehearses them.

use crate::actuator::ActuatorConfig;
use crate::drive::RelayConfig;
use crate::interlock::{AxisTravel, TravelProfile};
use crate::tracking::{AxisProfile, MotorProfile, PlanningConfig};

/// Relay timing, travel bounds, motion supervision and duty for the output task.
pub const ACTUATOR: ActuatorConfig = ActuatorConfig {
    relays: RelayConfig {
        min_active_ms: 500,
        same_direction_off_ms: 500,
        reversal_off_ms: 1_000,
    },
    // Retain the conservative stopping allowance until it is measured under load.
    travel: TravelProfile {
        azimuth: AxisTravel {
            max_speed_deg_per_s: 7.7,
            ..TravelProfile::DEFAULT.azimuth
        },
        elevation: AxisTravel {
            max_speed_deg_per_s: 3.8,
            ..TravelProfile::DEFAULT.elevation
        },
    },
    ..ActuatorConfig::DEFAULT
};

const _: () = assert!(ACTUATOR.is_valid());

/// The follower uses the measured steady rates. The other scenarios bracket
/// those rates and keep the commissioning assumptions for startup and coast.
pub fn planning_config() -> PlanningConfig {
    let azimuth = AxisProfile {
        rate_deg_per_s: 6.2,
        startup_ms: 300,
        coast_ms: 100,
        backlash_deg: 1.0,
    };
    let elevation = AxisProfile {
        rate_deg_per_s: 3.1,
        ..azimuth
    };
    let reference = MotorProfile {
        azimuth,
        elevation,
        duty: ACTUATOR.duty,
    };
    let slow = MotorProfile {
        azimuth: AxisProfile {
            rate_deg_per_s: 5.5,
            ..azimuth
        },
        elevation: AxisProfile {
            rate_deg_per_s: 2.8,
            ..elevation
        },
        ..reference
    };
    let fast = MotorProfile {
        azimuth: AxisProfile {
            rate_deg_per_s: 7.0,
            ..azimuth
        },
        elevation: AxisProfile {
            rate_deg_per_s: 3.4,
            ..elevation
        },
        ..reference
    };
    PlanningConfig::with_motor_profiles(reference, slow, fast)
}
