//! Fixtures shared by policy tests. They never use `HARD_CODED_CALIBRATION`,
//! so replacing the installed calibration cannot break a policy test.

#![allow(dead_code)]

use ezal_core::position::{AxisCalibration, PositionCalibration};

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
