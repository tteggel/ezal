//! Tests for calibrated position/voltage conversion.

mod common;

use common::TEST_CALIBRATION;
use ezal_core::ads1015::{count_to_mv, FullScale, FEEDBACK_FULL_SCALE, FEEDBACK_SUPPLY_RAIL_MV};
use ezal_core::position::{
    AxisCalibration, CalibrationError, FeedbackVoltages, Pointing, PositionCalibration,
    FEEDBACK_ENDPOINT_MARGIN_MV, HARD_CODED_CALIBRATION,
};

#[test]
fn the_installed_calibration_is_supervisable_and_maps_both_channels() {
    // The one test that reads the installed numbers. It must pass for any
    // correct calibration measured with docs/CALIBRATION.md, so it asserts
    // properties rather than the values currently checked in.
    HARD_CODED_CALIBRATION
        .validate_for_adc(FEEDBACK_FULL_SCALE)
        .expect("installed calibration must fit inside the feedback ADC range");

    for axis in [
        HARD_CODED_CALIBRATION.azimuth,
        HARD_CODED_CALIBRATION.elevation,
    ] {
        let span_deg = axis.angle_max_deg - axis.angle_min_deg;
        for angle_deg in [axis.angle_min_deg, axis.angle_max_deg] {
            let millivolts = axis.angle_to_voltage(angle_deg).unwrap();
            assert!((axis.voltage_to_angle(millivolts).unwrap() - angle_deg).abs() < 0.001);
        }
        // One ADC code must be a small fraction of the axis's travel.
        let span_mv = (axis.voltage_max_mv - axis.voltage_min_mv).unsigned_abs() as f32;
        let code_deg = span_deg * FEEDBACK_FULL_SCALE.code_mv() / span_mv;
        assert!(code_deg < span_deg / 100.0, "{code_deg}° per ADC code");
    }

    let low = Pointing::new(
        HARD_CODED_CALIBRATION.azimuth.angle_min_deg,
        HARD_CODED_CALIBRATION.elevation.angle_min_deg,
    );
    let feedback = HARD_CODED_CALIBRATION.position_to_feedback(low).unwrap();
    assert_eq!(
        feedback.a1_azimuth_mv,
        HARD_CODED_CALIBRATION.azimuth.voltage_min_mv
    );
    assert_eq!(
        feedback.a0_elevation_mv,
        HARD_CODED_CALIBRATION.elevation.voltage_min_mv
    );
    let decoded = HARD_CODED_CALIBRATION
        .feedback_to_position_for_adc(feedback, FEEDBACK_FULL_SCALE)
        .unwrap();
    assert!((decoded.azimuth_deg - low.azimuth_deg).abs() < 0.001);
    assert!((decoded.elevation_deg - low.elevation_deg).abs() < 0.001);
}

#[test]
fn quantised_round_trip_error_stays_below_one_adc_code() {
    let original = Pointing::new(317.2, 47.6);
    let feedback = TEST_CALIBRATION.position_to_feedback(original).unwrap();
    let decoded = TEST_CALIBRATION.feedback_to_position(feedback).unwrap();

    // 5 mV per azimuth degree and 10 mV per elevation degree, so millivolt
    // rounding costs at most 0.2° and 0.1°.
    assert!((decoded.azimuth_deg - original.azimuth_deg).abs() <= 0.2);
    assert!((decoded.elevation_deg - original.elevation_deg).abs() <= 0.1);
}

#[test]
fn endpoint_noise_clamps_but_disconnected_feedback_fails() {
    let axis = TEST_CALIBRATION.azimuth;
    assert_eq!(
        axis.voltage_to_angle(axis.voltage_min_mv - FEEDBACK_ENDPOINT_MARGIN_MV),
        Ok(axis.angle_min_deg)
    );
    assert_eq!(
        axis.voltage_to_angle(axis.voltage_min_mv - FEEDBACK_ENDPOINT_MARGIN_MV - 1),
        Err(CalibrationError::VoltageOutOfRange)
    );

    let disconnected = FeedbackVoltages {
        a0_elevation_mv: 0,
        a1_azimuth_mv: TEST_CALIBRATION.azimuth.voltage_min_mv,
    };
    assert_eq!(
        TEST_CALIBRATION.feedback_to_position(disconnected),
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
        TEST_CALIBRATION
            .azimuth
            .angle_to_voltage(TEST_CALIBRATION.azimuth.angle_max_deg + 1.0),
        Err(CalibrationError::AngleOutOfRange)
    );
    assert_eq!(
        PositionCalibration {
            elevation: malformed,
            ..TEST_CALIBRATION
        }
        .validate_for_adc(FEEDBACK_FULL_SCALE),
        Err(CalibrationError::InvalidCalibration)
    );
}

#[test]
fn extreme_voltage_endpoints_do_not_overflow_the_conversion_api() {
    // The mathematical conversion API accepts i32 endpoints independently of
    // the much narrower hardware ADC checks. Its Result must not conceal an
    // overflow panic when an endpoint or the span crosses an integer limit.
    for (voltage_min_mv, voltage_max_mv) in [(i32::MIN, i32::MAX), (i32::MAX, i32::MIN)] {
        let axis = AxisCalibration {
            angle_min_deg: 0.0,
            angle_max_deg: 180.0,
            voltage_min_mv,
            voltage_max_mv,
        };
        assert_eq!(axis.validate(), Ok(()));
        assert_eq!(axis.angle_to_voltage(0.0), Ok(voltage_min_mv));
        assert_eq!(axis.angle_to_voltage(180.0), Ok(voltage_max_mv));
        assert_eq!(axis.voltage_to_angle(voltage_min_mv), Ok(0.0));
        assert_eq!(axis.voltage_to_angle(voltage_max_mv), Ok(180.0));
        assert!((axis.voltage_to_angle(0).unwrap() - 90.0).abs() < 0.001);
    }
}

#[test]
fn finite_angle_endpoints_with_an_infinite_span_are_rejected() {
    let axis = AxisCalibration {
        angle_min_deg: -f32::MAX,
        angle_max_deg: f32::MAX,
        voltage_min_mv: 100,
        voltage_max_mv: 1_000,
    };
    assert_eq!(axis.validate(), Err(CalibrationError::InvalidCalibration));
    assert_eq!(
        axis.angle_to_voltage(0.0),
        Err(CalibrationError::InvalidCalibration)
    );
    assert_eq!(
        axis.voltage_to_angle(500),
        Err(CalibrationError::InvalidCalibration)
    );
}

#[test]
fn hardware_calibration_must_fit_the_interlocks_rotator_envelope() {
    let mut full_envelope = TEST_CALIBRATION;
    full_envelope.azimuth.angle_min_deg = 0.0;
    full_envelope.azimuth.angle_max_deg = 450.0;
    full_envelope.elevation.angle_min_deg = 0.0;
    full_envelope.elevation.angle_max_deg = 180.0;
    assert_eq!(full_envelope.validate_for_adc(FEEDBACK_FULL_SCALE), Ok(()));

    for (axis_index, min_deg, max_deg) in [
        (0, -1.0, 450.0),
        (0, 0.0, 451.0),
        (1, -1.0, 180.0),
        (1, 0.0, 181.0),
    ] {
        let mut calibration = full_envelope;
        let axis = if axis_index == 0 {
            &mut calibration.azimuth
        } else {
            &mut calibration.elevation
        };
        axis.angle_min_deg = min_deg;
        axis.angle_max_deg = max_deg;
        // Pure conversion remains available for diagnostics; the hardware
        // boundary must reject ranges that its final interlock cannot use.
        assert_eq!(calibration.validate(), Ok(()));
        assert_eq!(
            calibration.validate_for_adc(FEEDBACK_FULL_SCALE),
            Err(CalibrationError::InvalidCalibration)
        );
    }
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

#[test]
fn adc_range_validation_covers_both_axes_and_both_limits() {
    let axes: [fn(&mut PositionCalibration) -> &mut AxisCalibration; 2] = [
        |calibration| &mut calibration.azimuth,
        |calibration| &mut calibration.elevation,
    ];
    assert_eq!(
        TEST_CALIBRATION.validate_for_adc(FEEDBACK_FULL_SCALE),
        Ok(())
    );

    for axis_of in axes {
        // The fault margin below the lower endpoint must stay above ground.
        let mut at_ground = TEST_CALIBRATION;
        set_low_endpoint(axis_of(&mut at_ground), FEEDBACK_ENDPOINT_MARGIN_MV);
        assert_eq!(
            at_ground.validate_for_adc(FEEDBACK_FULL_SCALE),
            Err(CalibrationError::AdcRangeTooNarrow)
        );

        // The margin above the upper endpoint must stay below the supply rail,
        // which the ADS1015 reaches long before its ±4.096 V saturation code.
        let mut at_rail = TEST_CALIBRATION;
        set_high_endpoint(
            axis_of(&mut at_rail),
            FEEDBACK_SUPPLY_RAIL_MV - FEEDBACK_ENDPOINT_MARGIN_MV,
        );
        assert_eq!(
            at_rail.validate_for_adc(FEEDBACK_FULL_SCALE),
            Err(CalibrationError::AdcRangeTooNarrow)
        );
        let mut inside_rail = at_rail;
        set_high_endpoint(
            axis_of(&mut inside_rail),
            FEEDBACK_SUPPLY_RAIL_MV - FEEDBACK_ENDPOINT_MARGIN_MV - 1,
        );
        assert_eq!(inside_rail.validate_for_adc(FEEDBACK_FULL_SCALE), Ok(()));

        let mut absurd = TEST_CALIBRATION;
        set_high_endpoint(axis_of(&mut absurd), i32::MAX);
        assert_eq!(
            absurd.validate_for_adc(FEEDBACK_FULL_SCALE),
            Err(CalibrationError::AdcRangeTooNarrow)
        );
    }
    assert!(FEEDBACK_SUPPLY_RAIL_MV < FEEDBACK_FULL_SCALE.positive_saturation_mv());

    // A 2 V endpoint leaves no fault margin under the narrower ±2.048 V range.
    let mut near_full_scale = TEST_CALIBRATION;
    near_full_scale.azimuth.voltage_max_mv = 2_030;
    assert_eq!(
        near_full_scale.validate_for_adc(FullScale::V2_048),
        Err(CalibrationError::AdcRangeTooNarrow)
    );
    assert_eq!(
        near_full_scale.validate_for_adc(FEEDBACK_FULL_SCALE),
        Ok(())
    );
}

#[test]
fn adc_and_supply_rails_are_rejected_before_endpoint_noise_clamping() {
    // This was the failure: the purely mathematical endpoint clamp accepts a
    // saturation code as a valid mechanical limit.
    let mut near_full_scale = TEST_CALIBRATION;
    near_full_scale.azimuth.voltage_max_mv = 2_030;
    let saturated = FeedbackVoltages {
        a0_elevation_mv: 700,
        a1_azimuth_mv: 2_047,
    };
    assert_eq!(
        near_full_scale
            .feedback_to_position(saturated)
            .unwrap()
            .azimuth_deg,
        360.0
    );
    assert_eq!(
        near_full_scale.feedback_to_position_for_adc(saturated, FullScale::V2_048),
        Err(CalibrationError::AdcSaturated)
    );

    for invalid_mv in [
        0,
        -1,
        FEEDBACK_SUPPLY_RAIL_MV,
        FEEDBACK_FULL_SCALE.positive_saturation_mv(),
        5_000,
    ] {
        for invalid in [
            FeedbackVoltages {
                a0_elevation_mv: 700,
                a1_azimuth_mv: invalid_mv,
            },
            FeedbackVoltages {
                a0_elevation_mv: invalid_mv,
                a1_azimuth_mv: 1_000,
            },
        ] {
            assert_eq!(
                TEST_CALIBRATION.feedback_to_position_for_adc(invalid, FEEDBACK_FULL_SCALE),
                Err(CalibrationError::AdcSaturated)
            );
        }
    }
}

#[test]
fn wider_adc_quantisation_preserves_angles_and_exposes_overrange() {
    let expected = Pointing::new(317.2, 47.6);
    let raw = TEST_CALIBRATION.position_to_feedback(expected).unwrap();
    let quantised = FeedbackVoltages {
        a0_elevation_mv: count_to_mv((raw.a0_elevation_mv / 2) as i16, FEEDBACK_FULL_SCALE),
        a1_azimuth_mv: count_to_mv((raw.a1_azimuth_mv / 2) as i16, FEEDBACK_FULL_SCALE),
    };
    let measured = TEST_CALIBRATION
        .feedback_to_position_for_adc(quantised, FEEDBACK_FULL_SCALE)
        .unwrap();
    assert!((measured.azimuth_deg - expected.azimuth_deg).abs() < 0.4);
    assert!((measured.elevation_deg - expected.elevation_deg).abs() < 0.2);

    let overrange = FeedbackVoltages {
        a0_elevation_mv: raw.a0_elevation_mv,
        a1_azimuth_mv: TEST_CALIBRATION.azimuth.voltage_max_mv + FEEDBACK_ENDPOINT_MARGIN_MV + 1,
    };
    assert_eq!(
        TEST_CALIBRATION.feedback_to_position_for_adc(overrange, FEEDBACK_FULL_SCALE),
        Err(CalibrationError::VoltageOutOfRange)
    );
}

/// Move an axis's endpoint nearer ground, whichever end that is.
fn set_low_endpoint(axis: &mut AxisCalibration, millivolts: i32) {
    if axis.voltage_min_mv < axis.voltage_max_mv {
        axis.voltage_min_mv = millivolts;
    } else {
        axis.voltage_max_mv = millivolts;
    }
}

/// Move an axis's endpoint nearer the supply rail, whichever end that is.
fn set_high_endpoint(axis: &mut AxisCalibration, millivolts: i32) {
    if axis.voltage_min_mv < axis.voltage_max_mv {
        axis.voltage_max_mv = millivolts;
    } else {
        axis.voltage_min_mv = millivolts;
    }
}
