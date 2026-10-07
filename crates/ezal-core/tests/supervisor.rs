//! Feedback validation and its effect on manual actuation.

mod common;

use common::TEST_CALIBRATION;
use ezal_core::actuator::Actuator;
use ezal_core::ads1015::FEEDBACK_FULL_SCALE;
use ezal_core::control::{
    ControlState, TimedPointing, FEEDBACK_MAX_LATENCY_MS, FEEDBACK_PERIOD_MS,
};
use ezal_core::drive::{AzimuthDirection, Command, DriveCommand};
use ezal_core::interlock::FeedbackInterlock;
use ezal_core::position::{CalibrationError, FeedbackVoltages, Pointing};
use ezal_core::protocol::PositionTelemetry;
use ezal_core::supervisor::{FeedbackReadError, ManualSupervisor};

fn feedback(azimuth: f32, elevation: f32) -> Result<FeedbackVoltages, FeedbackReadError> {
    Ok(TEST_CALIBRATION
        .position_to_feedback(Pointing::new(azimuth, elevation))
        .unwrap())
}

#[test]
fn feedback_faults_cut_manual_motion_during_the_minimum_on_time() {
    for readings in [
        Err(FeedbackReadError::Unavailable),
        Err(FeedbackReadError::TimedOut),
        Ok(FeedbackVoltages {
            a0_elevation_mv: 700,
            a1_azimuth_mv: 0,
        }),
    ] {
        let mut supervisor = ManualSupervisor::new(TEST_CALIBRATION);
        let mut actuator = Actuator::new();
        let initial = supervisor.update(2_000, 2_000, feedback(100.0, 20.0));
        actuator.command(
            Command::Jog(DriveCommand {
                azimuth: Some(AzimuthDirection::Clockwise),
                elevation: None,
            }),
            &initial.interlock,
            2_000,
        );
        assert!(actuator.actual().is_active());
        let fault = supervisor.update(2_100, 2_050, readings);
        assert_eq!(
            fault.feedback.azimuth_tenths,
            initial.feedback.azimuth_tenths
        );
        actuator.service(&fault.interlock, 2_100);
        assert_eq!(actuator.actual(), DriveCommand::IDLE);
        assert_eq!(actuator.target(), DriveCommand::IDLE);
    }
}

#[test]
fn manual_feedback_errors_remove_readiness_and_only_uncancelled_reads_can_recover() {
    let mut supervisor = ManualSupervisor::new(TEST_CALIBRATION);
    let initial = supervisor.update(100, 98, feedback(100.0, 20.0));
    assert_eq!(initial.state, ControlState::Idle);
    assert_eq!(initial.interlock.feedback().unwrap().timestamp_ms, 98);
    let missing = supervisor.update(200, 198, Err(FeedbackReadError::Unavailable));
    assert_eq!(missing.state, ControlState::FeedbackUnavailable);
    assert_eq!(missing.interlock.feedback(), None);
    assert_eq!(
        missing.feedback.azimuth_tenths,
        initial.feedback.azimuth_tenths
    );
    assert!(supervisor.feedback_read_permitted());
    assert!(supervisor
        .update(300, 298, feedback(110.0, 20.0))
        .interlock
        .feedback()
        .is_some());
    let timeout = supervisor.update(900, 400, Err(FeedbackReadError::TimedOut));
    assert_eq!(timeout.state, ControlState::FeedbackStale);
    assert!(!supervisor.feedback_read_permitted());
    assert_eq!(
        supervisor
            .update(1_000, 998, feedback(100.0, 20.0))
            .interlock
            .feedback(),
        None
    );
}

#[test]
fn manual_mode_rejects_late_future_and_saturated_feedback_before_publishing_readiness() {
    for (now_ms, started_ms, readings, state) in [
        (601, 100, feedback(100.0, 20.0), ControlState::FeedbackStale),
        (
            100 + FEEDBACK_MAX_LATENCY_MS + 1,
            100,
            feedback(100.0, 20.0),
            ControlState::FeedbackStale,
        ),
        (
            100,
            101,
            feedback(100.0, 20.0),
            ControlState::TimestampInvalid,
        ),
        (
            100,
            98,
            Ok(FeedbackVoltages {
                a0_elevation_mv: FEEDBACK_FULL_SCALE.positive_saturation_mv(),
                a1_azimuth_mv: 1_000,
            }),
            ControlState::FeedbackInvalid,
        ),
    ] {
        let mut supervisor = ManualSupervisor::new(TEST_CALIBRATION);
        let update = supervisor.update(now_ms, started_ms, readings);
        assert_eq!(update.state, state);
        assert_eq!(update.interlock.feedback(), None);
        assert!(update.raw_feedback.is_some());
        // Only a cancelled read latches; a late or rejected one does not.
        assert!(supervisor.feedback_read_permitted());
    }
}

#[test]
fn an_accepted_sample_outlives_the_slowest_schedule_that_replaces_it() {
    let mut supervisor = ManualSupervisor::new(TEST_CALIBRATION);
    let started_ms = 300;
    let published_ms = started_ms + FEEDBACK_MAX_LATENCY_MS;
    let update = supervisor.update(published_ms, started_ms, feedback(100.0, 20.0));
    assert_eq!(update.state, ControlState::Idle);
    let replaced_ms = published_ms + FEEDBACK_PERIOD_MS + FEEDBACK_MAX_LATENCY_MS;
    assert!(update.interlock.motion_permitted(replaced_ms));
}

#[test]
fn the_published_interlock_uses_the_supervisors_own_calibration() {
    let mut narrower = TEST_CALIBRATION;
    narrower.azimuth.angle_max_deg = 350.0;
    let mut supervisor = ManualSupervisor::new(narrower);
    let at_end = narrower
        .position_to_feedback(Pointing::new(350.0, 45.0))
        .unwrap();
    let update = supervisor.update(1, 0, Ok(at_end));
    let position = update.interlock.feedback().unwrap().position;
    assert_eq!(position.azimuth_deg, 350.0);
    let clockwise = DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: None,
    };
    let counter_clockwise = DriveCommand {
        azimuth: Some(AzimuthDirection::CounterClockwise),
        elevation: None,
    };
    assert_eq!(
        update.interlock.guard_drive(clockwise, 1),
        DriveCommand::IDLE
    );
    assert_eq!(
        update.interlock.guard_drive(counter_clockwise, 1),
        counter_clockwise
    );

    // A separately configured 360° gate would have kept driving outward.
    let mut separate = FeedbackInterlock::new(TEST_CALIBRATION);
    separate.publish(Some(TimedPointing::new(position, 0)));
    assert_eq!(separate.guard_drive(clockwise, 1), clockwise);
}

#[test]
fn a_rejected_calibration_keeps_raw_readings_live_and_never_permits_motion() {
    let mut rejected = TEST_CALIBRATION;
    // A 90° reading of 20 mV leaves no room for the 25 mV fault margin.
    rejected.elevation.voltage_max_mv = 20;
    let readings = Ok(FeedbackVoltages {
        a0_elevation_mv: 700,
        a1_azimuth_mv: 1_100,
    });

    let mut manual = ManualSupervisor::new(rejected);
    assert_eq!(
        manual.calibration_error(),
        Some(CalibrationError::AdcRangeTooNarrow)
    );
    let update = manual.update(10, 5, readings);
    assert_eq!(
        update.raw_feedback,
        Some(PositionTelemetry {
            a0_mv: 700,
            a1_mv: 1_100,
        })
    );
    assert_eq!(update.state, ControlState::CalibrationInvalid);
    assert!(!update.interlock.motion_permitted(10));
    let moved = manual.update(
        110,
        105,
        Ok(FeedbackVoltages {
            a0_elevation_mv: 710,
            a1_azimuth_mv: 1_100,
        }),
    );
    assert_eq!(moved.raw_feedback.unwrap().a0_mv, 710);
    // A bus fault still explains why readings stopped changing.
    assert_eq!(
        manual
            .update(210, 205, Err(FeedbackReadError::Unavailable))
            .state,
        ControlState::FeedbackUnavailable
    );
}
