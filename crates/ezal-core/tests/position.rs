//! Tests for calibrated position/voltage conversion.

use ezal_core::position::{
    AxisCalibration, CalibrationError, FeedbackVoltages, Pointing, FEEDBACK_ENDPOINT_MARGIN_MV,
    HARD_CODED_CALIBRATION,
};

#[test]
fn hard_coded_calibration_maps_both_physical_channels() {
    HARD_CODED_CALIBRATION.validate().unwrap();

    let low = Pointing::new(
        HARD_CODED_CALIBRATION.azimuth.angle_min_deg,
        HARD_CODED_CALIBRATION.elevation.angle_min_deg,
    );
    let low_feedback = HARD_CODED_CALIBRATION.position_to_feedback(low).unwrap();
    assert_eq!(
        low_feedback.a0_elevation_mv,
        HARD_CODED_CALIBRATION.elevation.voltage_min_mv
    );
    assert_eq!(
        low_feedback.a1_azimuth_mv,
        HARD_CODED_CALIBRATION.azimuth.voltage_min_mv
    );

    let high = Pointing::new(
        HARD_CODED_CALIBRATION.azimuth.angle_max_deg,
        HARD_CODED_CALIBRATION.elevation.angle_max_deg,
    );
    let high_feedback = HARD_CODED_CALIBRATION.position_to_feedback(high).unwrap();
    assert_eq!(
        high_feedback.a0_elevation_mv,
        HARD_CODED_CALIBRATION.elevation.voltage_max_mv
    );
    assert_eq!(
        high_feedback.a1_azimuth_mv,
        HARD_CODED_CALIBRATION.azimuth.voltage_max_mv
    );

    assert_eq!(
        HARD_CODED_CALIBRATION
            .feedback_to_position(low_feedback)
            .unwrap(),
        low
    );
    assert_eq!(
        HARD_CODED_CALIBRATION
            .feedback_to_position(high_feedback)
            .unwrap(),
        high
    );
}

#[test]
fn quantised_round_trip_error_stays_below_one_adc_code() {
    let original = Pointing::new(317.2, 47.6);
    let feedback = HARD_CODED_CALIBRATION
        .position_to_feedback(original)
        .unwrap();
    let decoded = HARD_CODED_CALIBRATION
        .feedback_to_position(feedback)
        .unwrap();

    assert!((decoded.azimuth_deg - original.azimuth_deg).abs() <= 0.41);
    assert!((decoded.elevation_deg - original.elevation_deg).abs() <= 0.17);
}

#[test]
fn endpoint_noise_clamps_but_disconnected_feedback_fails() {
    let axis = AxisCalibration {
        angle_min_deg: 0.0,
        angle_max_deg: 90.0,
        voltage_min_mv: 100,
        voltage_max_mv: 1_100,
    };
    assert_eq!(
        axis.voltage_to_angle(axis.voltage_min_mv - FEEDBACK_ENDPOINT_MARGIN_MV),
        Ok(0.0)
    );
    assert_eq!(
        axis.voltage_to_angle(axis.voltage_min_mv - FEEDBACK_ENDPOINT_MARGIN_MV - 1),
        Err(CalibrationError::VoltageOutOfRange)
    );

    let bad = FeedbackVoltages {
        a0_elevation_mv: 0,
        a1_azimuth_mv: HARD_CODED_CALIBRATION.azimuth.voltage_min_mv,
    };
    assert_eq!(
        HARD_CODED_CALIBRATION.feedback_to_position(bad),
        Err(CalibrationError::VoltageOutOfRange)
    );
}

#[test]
fn malformed_and_out_of_range_calibrations_are_rejected() {
    let malformed = AxisCalibration {
        angle_min_deg: 0.0,
        angle_max_deg: 180.0,
        voltage_min_mv: 1_000,
        voltage_max_mv: 1_000,
    };
    assert_eq!(
        malformed.validate(),
        Err(CalibrationError::InvalidCalibration)
    );
    assert_eq!(
        HARD_CODED_CALIBRATION.azimuth.angle_to_voltage(451.0),
        Err(CalibrationError::AngleOutOfRange)
    );
}

#[test]
fn reversed_voltage_scale_maps_both_directions() {
    let reversed = AxisCalibration {
        angle_min_deg: 0.0,
        angle_max_deg: 90.0,
        voltage_min_mv: 1_281,
        voltage_max_mv: 58,
    };

    assert_eq!(reversed.validate(), Ok(()));
    assert_eq!(reversed.angle_to_voltage(0.0), Ok(1_281));
    assert_eq!(reversed.angle_to_voltage(45.0), Ok(670));
    assert_eq!(reversed.angle_to_voltage(90.0), Ok(58));
    assert_eq!(reversed.voltage_to_angle(1_281), Ok(0.0));
    assert_eq!(reversed.voltage_to_angle(58), Ok(90.0));
    assert!((reversed.voltage_to_angle(670).unwrap() - 45.0).abs() < 0.05);
}

#[test]
fn reversed_voltage_scale_clamps_noise_at_the_correct_angle_endpoint() {
    let reversed = AxisCalibration {
        angle_min_deg: 0.0,
        angle_max_deg: 90.0,
        voltage_min_mv: 1_281,
        voltage_max_mv: 58,
    };

    assert_eq!(
        reversed.voltage_to_angle(58 - FEEDBACK_ENDPOINT_MARGIN_MV),
        Ok(90.0)
    );
    assert_eq!(
        reversed.voltage_to_angle(1_281 + FEEDBACK_ENDPOINT_MARGIN_MV),
        Ok(0.0)
    );
    assert_eq!(
        reversed.voltage_to_angle(58 - FEEDBACK_ENDPOINT_MARGIN_MV - 1),
        Err(CalibrationError::VoltageOutOfRange)
    );
    assert_eq!(
        reversed.voltage_to_angle(1_281 + FEEDBACK_ENDPOINT_MARGIN_MV + 1),
        Err(CalibrationError::VoltageOutOfRange)
    );
}
