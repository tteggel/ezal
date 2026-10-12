//! Fixtures shared by the integration binaries and in-crate tests. They never
//! use `HARD_CODED_CALIBRATION`, so replacing the installed calibration cannot
//! break a policy test.

#![allow(dead_code)]

use ezal_core::actuator::{Actuator, ActuatorConfig, ActuatorInput};
use ezal_core::ads1015::FEEDBACK_FULL_SCALE;
use ezal_core::duty::{DutyConfig, DutyHistory};
use ezal_core::feedback::FeedbackSample;
use ezal_core::interlock::FeedbackInterlock;
use ezal_core::network::NetworkGate;
use ezal_core::position::{AxisCalibration, InstalledCalibration, Pointing, PositionCalibration};
use ezal_core::tracking::{PlanningConfig, RestingStart, Scenario};

/// A synthetic installation with round numbers: azimuth 0–360° over
/// 200–2000 mV (5 mV per degree, 0.4° per 2 mV ADC code) and a reversed
/// elevation scale, 0–90° over 1300–400 mV (10 mV per degree, 0.2° per code).
pub const TEST_CALIBRATION: PositionCalibration = PositionCalibration {
    azimuth: AxisCalibration {
        angle_min_deg: 0.0,
        angle_max_deg: 360.0,
        voltage_min_mv: 200,
        voltage_max_mv: 2_000,
    },
    elevation: AxisCalibration {
        angle_min_deg: 0.0,
        angle_max_deg: 90.0,
        voltage_min_mv: 1_300,
        voltage_max_mv: 400,
    },
};

/// A synthetic installation spanning the whole G-5500 envelope: azimuth
/// 0–450° over 200–2900 mV and a reversed elevation scale, 0–180° over
/// 2800–1000 mV. Tracking tests need the azimuth overlap and over-the-top
/// elevation that [`TEST_CALIBRATION`]'s 360°/90° stops exclude.
pub const FULL_TRAVEL_CALIBRATION: PositionCalibration = PositionCalibration {
    azimuth: AxisCalibration {
        angle_min_deg: 0.0,
        angle_max_deg: 450.0,
        voltage_min_mv: 200,
        voltage_max_mv: 2_900,
    },
    elevation: AxisCalibration {
        angle_min_deg: 0.0,
        angle_max_deg: 180.0,
        voltage_min_mv: 2_800,
        voltage_max_mv: 1_000,
    },
};

pub fn installed() -> InstalledCalibration {
    InstalledCalibration::new(TEST_CALIBRATION, FEEDBACK_FULL_SCALE).unwrap()
}

pub fn full_travel_installed() -> InstalledCalibration {
    InstalledCalibration::new(FULL_TRAVEL_CALIBRATION, FEEDBACK_FULL_SCALE).unwrap()
}

/// Policy fixtures begin with explicitly known rested motors. Production boot
/// uses unknown history and must complete a full rest before movement.
pub fn rested_actuator() -> Actuator {
    Actuator::with_config(ActuatorConfig::DEFAULT, DutyHistory::Known([0, 0]))
}

/// Build test observations through the same checked conversion as hardware.
pub fn sample(position: Pointing, sampled_at_ms: u64) -> FeedbackSample {
    let voltages = TEST_CALIBRATION.position_to_feedback(position).unwrap();
    FeedbackSample::from_voltages(installed(), voltages, sampled_at_ms).unwrap()
}

/// Build a [`FULL_TRAVEL_CALIBRATION`] observation through the same checked
/// conversion as hardware, like [`sample`].
pub fn full_travel_sample(position: Pointing, sampled_at_ms: u64) -> FeedbackSample {
    FeedbackSample::from_voltages(
        full_travel_installed(),
        FULL_TRAVEL_CALIBRATION
            .position_to_feedback(position)
            .unwrap(),
        sampled_at_ms,
    )
    .unwrap()
}

/// The planner's start at 2000 ms: an idle confirmed-AC actuator with `used`
/// duty, serviced at 0 and 2000 ms with healthy network and feedback at `pose`.
pub fn resting_start(
    calibration: PositionCalibration,
    pose: Pointing,
    used: [u64; 2],
) -> RestingStart {
    let installed = InstalledCalibration::new(calibration, FEEDBACK_FULL_SCALE).unwrap();
    let mut actuator = Actuator::with_config(
        ActuatorConfig {
            duty: DutyConfig::CONFIRMED_AC,
            ..ActuatorConfig::DEFAULT
        },
        DutyHistory::Known(used),
    );
    let mut network = NetworkGate::new();
    let mut feedback = None;
    for at_ms in [0, 2000] {
        feedback = Some(
            FeedbackSample::from_voltages(
                installed,
                calibration.position_to_feedback(pose).unwrap(),
                at_ms,
            )
            .unwrap(),
        );
        let mut interlock = FeedbackInterlock::new(installed);
        interlock.publish(feedback);
        network.observe(at_ms, true);
        actuator.step(
            ActuatorInput {
                interlock,
                network,
                command: None,
            },
            at_ms,
        );
    }
    RestingStart {
        at_ms: 2000,
        feedback: feedback.unwrap(),
        actuator,
        network,
    }
}

/// Planning against the nominal motor scenario alone.
pub fn nominal() -> PlanningConfig {
    let mut config = PlanningConfig::default();
    config.scenarios.clear();
    config.scenarios.push(Scenario::NOMINAL).unwrap();
    config
}
