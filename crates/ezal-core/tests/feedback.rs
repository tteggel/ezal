//! Feedback validation and its effect on manual actuation.

mod common;

use common::{installed, sample, TEST_CALIBRATION};
use ezal_core::actuator::{Actuator, ActuatorInput};
use ezal_core::ads1015::FEEDBACK_FULL_SCALE;
use ezal_core::drive::{AzimuthDirection, Command, DriveCommand};
use ezal_core::feedback::{
    AcquisitionState, FeedbackHealth, FeedbackMonitor, FeedbackReadError, FeedbackSample,
    FEEDBACK_MAX_AGE_MS, FEEDBACK_MAX_LATENCY_MS, FEEDBACK_PERIOD_MS,
};
use ezal_core::interlock::FeedbackInterlock;
use ezal_core::mailbox::CommandEnvelope;
use ezal_core::network::NetworkGate;
use ezal_core::position::{CalibrationError, FeedbackVoltages, Pointing};

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
        let mut monitor = FeedbackMonitor::new(TEST_CALIBRATION, None);
        let mut actuator = Actuator::new();
        let mut network = NetworkGate::new();
        network.observe(2_000, true);
        let initial = monitor.update(2_000, 2_000, feedback(100.0, 20.0));
        actuator.step(
            ActuatorInput {
                interlock: initial.interlock,
                network,
                command: Some(CommandEnvelope {
                    command: Command::Jog(DriveCommand {
                        azimuth: Some(AzimuthDirection::Clockwise),
                        elevation: None,
                    }),
                    valid_until_ms: 2_750,
                }),
            },
            2_000,
        );
        assert!(actuator.actual().is_active());
        let fault = monitor.update(2_100, 2_050, readings);
        assert_eq!(fault.snapshot.last_valid, initial.snapshot.last_valid);
        assert_eq!(fault.interlock.feedback(), None);
        actuator.step(
            ActuatorInput {
                interlock: fault.interlock,
                network,
                command: None,
            },
            2_100,
        );
        assert_eq!(actuator.actual(), DriveCommand::IDLE);
        assert_eq!(actuator.target(), DriveCommand::IDLE);
    }
}

#[test]
fn read_errors_remove_readiness_but_keep_last_diagnostics() {
    let mut monitor = FeedbackMonitor::new(TEST_CALIBRATION, None);
    let initial = monitor.update(100, 98, feedback(100.0, 20.0));
    assert_eq!(initial.snapshot.health, FeedbackHealth::Healthy);
    assert_eq!(initial.interlock.feedback().unwrap().sampled_at_ms(), 98);
    for (error, health) in [
        (FeedbackReadError::Unavailable, FeedbackHealth::Unavailable),
        (FeedbackReadError::TimedOut, FeedbackHealth::ReadTimedOut),
    ] {
        let missing = monitor.update(200, 198, Err(error));
        assert_eq!(missing.snapshot.health, health);
        assert_eq!(missing.interlock.feedback(), None);
        assert_eq!(missing.snapshot.last_valid, initial.snapshot.last_valid);
        assert_eq!(missing.snapshot.raw, initial.snapshot.raw);
    }
    // The monitor does not own resource recovery. A successful result from a
    // usable ADC may restore readiness; the reader separately owns cancellation.
    let recovered = monitor.update(300, 298, feedback(110.0, 20.0));
    assert_eq!(recovered.snapshot.health, FeedbackHealth::Healthy);
    assert!(recovered.interlock.feedback().is_some());
}

#[test]
fn acquisition_owner_latches_cancellation_and_only_uncancelled_errors_can_recover() {
    let mut acquisition = AcquisitionState::default();
    assert!(acquisition.read_permitted());
    assert_eq!(
        acquisition.complete(Err(FeedbackReadError::Unavailable)),
        Err(FeedbackReadError::Unavailable)
    );
    assert!(acquisition.read_permitted());
    assert!(acquisition.complete(feedback(100.0, 20.0)).is_ok());
    assert_eq!(
        acquisition.complete(Err(FeedbackReadError::TimedOut)),
        Err(FeedbackReadError::TimedOut)
    );
    assert!(!acquisition.read_permitted());
    let mut monitor = FeedbackMonitor::new(TEST_CALIBRATION, None);
    for readings in [feedback(100.0, 20.0), Err(FeedbackReadError::Unavailable)] {
        let refused = acquisition.complete(readings);
        assert_eq!(refused, Err(FeedbackReadError::TimedOut));
        let update = monitor.update(100, 100, refused);
        assert_eq!(update.snapshot.health, FeedbackHealth::ReadTimedOut);
        assert!(!update.interlock.motion_permitted(100));
    }
}

#[test]
fn late_future_and_saturated_feedback_cannot_replace_last_valid_or_publish_readiness() {
    for (now_ms, started_ms, readings, health) in [
        (601, 100, feedback(110.0, 30.0), FeedbackHealth::Stale),
        (
            100 + FEEDBACK_MAX_LATENCY_MS + 1,
            100,
            feedback(110.0, 30.0),
            FeedbackHealth::Stale,
        ),
        (
            100,
            101,
            feedback(110.0, 30.0),
            FeedbackHealth::TimestampInvalid,
        ),
        (
            100,
            98,
            Ok(FeedbackVoltages {
                a0_elevation_mv: FEEDBACK_FULL_SCALE.positive_saturation_mv(),
                a1_azimuth_mv: 1_000,
            }),
            FeedbackHealth::Invalid,
        ),
    ] {
        let mut monitor = FeedbackMonitor::new(TEST_CALIBRATION, None);
        let initial = monitor.update(0, 0, feedback(100.0, 20.0));
        let update = monitor.update(now_ms, started_ms, readings);
        assert_eq!(update.snapshot.health, health);
        assert_eq!(update.snapshot.last_valid, initial.snapshot.last_valid);
        assert_eq!(update.interlock.feedback(), None);
        assert_eq!(update.snapshot.raw, readings.ok());
        assert_eq!(
            monitor
                .update(now_ms + 1, now_ms + 1, feedback(100.0, 20.0))
                .snapshot
                .health,
            FeedbackHealth::Healthy
        );
    }
}

#[test]
fn an_accepted_sample_outlives_its_replacement_schedule_but_expires_without_a_producer() {
    let mut monitor = FeedbackMonitor::new(TEST_CALIBRATION, None);
    let started_ms = 300;
    let published_ms = started_ms + FEEDBACK_MAX_LATENCY_MS;
    let update = monitor.update(published_ms, started_ms, feedback(100.0, 20.0));
    assert_eq!(update.snapshot.health, FeedbackHealth::Healthy);
    let replaced_ms = published_ms + FEEDBACK_PERIOD_MS + FEEDBACK_MAX_LATENCY_MS;
    assert!(update.interlock.motion_permitted(replaced_ms));
    assert!(!update
        .interlock
        .motion_permitted(started_ms + FEEDBACK_MAX_AGE_MS + 1));
    assert_eq!(
        update.snapshot.last_valid.unwrap().sampled_at_ms(),
        started_ms
    );
}

#[test]
fn the_published_interlock_uses_the_monitors_installed_calibration() {
    let mut narrower = TEST_CALIBRATION;
    narrower.azimuth.angle_max_deg = 350.0;
    let mut monitor = FeedbackMonitor::new(narrower, None);
    let at_end = narrower
        .position_to_feedback(Pointing::new(350.0, 45.0))
        .unwrap();
    let update = monitor.update(1, 0, Ok(at_end));
    let position = update.interlock.feedback().unwrap().position();
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
    let mut separate = FeedbackInterlock::new(installed());
    separate.publish(Some(sample(position, 0)));
    assert_eq!(separate.guard_drive(clockwise, 1), clockwise);
}

#[test]
fn a_rejected_calibration_keeps_raw_readings_live_and_never_permits_motion() {
    let mut rejected = TEST_CALIBRATION;
    rejected.elevation.voltage_max_mv = 20;
    let raw = FeedbackVoltages {
        a0_elevation_mv: 700,
        a1_azimuth_mv: 1_100,
    };
    let mut monitor = FeedbackMonitor::new(rejected, None);
    assert_eq!(
        monitor.calibration_error(),
        Some(CalibrationError::AdcRangeTooNarrow)
    );
    let update = monitor.update(10, 5, Ok(raw));
    assert_eq!(update.snapshot.raw, Some(raw));
    assert_eq!(update.snapshot.last_valid, None);
    assert_eq!(update.snapshot.health, FeedbackHealth::CalibrationInvalid);
    assert!(!update.interlock.motion_permitted(10));
    let raw = FeedbackVoltages {
        a0_elevation_mv: 710,
        ..raw
    };
    let moved = monitor.update(110, 105, Ok(raw));
    assert_eq!(moved.snapshot.raw, Some(raw));
    let failed = monitor.update(210, 205, Err(FeedbackReadError::Unavailable));
    assert_eq!(failed.snapshot.health, FeedbackHealth::Unavailable);
    assert_eq!(failed.snapshot.raw, Some(raw));
}

#[test]
fn startup_raw_diagnostics_do_not_grant_motion_or_fabricate_a_calibrated_position() {
    let raw = feedback(100.0, 20.0).unwrap();
    let mut monitor = FeedbackMonitor::new(TEST_CALIBRATION, Some(raw));
    let update = monitor.update(10, 5, Err(FeedbackReadError::Unavailable));
    assert_eq!(update.snapshot.raw, Some(raw));
    assert_eq!(update.snapshot.last_valid, None);
    assert!(!update.interlock.motion_permitted(10));
}

#[test]
fn a_feedback_sample_cannot_be_constructed_from_rail_or_out_of_range_readings() {
    for raw in [
        FeedbackVoltages {
            a0_elevation_mv: 0,
            a1_azimuth_mv: 1100,
        },
        FeedbackVoltages {
            a0_elevation_mv: 700,
            a1_azimuth_mv: 0,
        },
        FeedbackVoltages {
            a0_elevation_mv: 700,
            a1_azimuth_mv: 2500,
        },
    ] {
        assert!(FeedbackSample::from_voltages(installed(), raw, 0).is_err());
    }
}
