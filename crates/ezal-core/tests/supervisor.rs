//! Composed feedback, sequencing, controller, interlock, and telemetry behavior.

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
use ezal_core::simulation::{
    PassPhase, ACQUIRE_TIMEOUT_MS, BETWEEN_PASS_PAUSE_MS, METOP_PASS_DURATION_MS,
};
use ezal_core::supervisor::{FeedbackReadError, ManualSupervisor, TrackingSupervisor};

fn feedback(azimuth: f32, elevation: f32) -> Result<FeedbackVoltages, FeedbackReadError> {
    Ok(TEST_CALIBRATION
        .position_to_feedback(Pointing::new(azimuth, elevation))
        .unwrap())
}

#[test]
fn offline_reacquisition_preserves_the_time_budget_and_requires_new_settling() {
    let mut supervisor = TrackingSupervisor::new(TEST_CALIBRATION, 0);
    for now_ms in [0, ACQUIRE_TIMEOUT_MS, ACQUIRE_TIMEOUT_MS * 2] {
        supervisor.restart_tracking(now_ms);
        let update = supervisor.update(now_ms, now_ms, feedback(20.0, 0.0));
        assert_eq!(update.tracking.phase, PassPhase::Acquiring);
        assert_eq!(update.tracking.phase_remaining_ms, ACQUIRE_TIMEOUT_MS);
    }
    let recovered_ms = ACQUIRE_TIMEOUT_MS * 3;
    supervisor.restart_tracking(recovered_ms);
    let update = supervisor.update(recovered_ms, recovered_ms, feedback(20.0, 0.0));
    assert_eq!(update.tracking.phase, PassPhase::Acquiring);
    let settled_ms = recovered_ms + ezal_core::simulation::ACQUIRE_SETTLE_MS;
    let update = supervisor.update(settled_ms, settled_ms, feedback(20.0, 0.0));
    assert_eq!(update.tracking.phase, PassPhase::Tracking);
}

#[test]
fn network_recovery_preserves_adc_and_acquisition_fault_latches() {
    let mut supervisor = TrackingSupervisor::new(TEST_CALIBRATION, 0);
    supervisor.update(0, 0, Err(FeedbackReadError::TimedOut));
    supervisor.restart_tracking(10_000);
    let update = supervisor.update(10_000, 10_000, feedback(100.0, 20.0));
    assert!(!supervisor.feedback_read_permitted());
    assert_eq!(update.decision.state, ControlState::FeedbackStale);
    assert_eq!(update.decision.command, Command::Inhibit);

    let mut supervisor = TrackingSupervisor::new(TEST_CALIBRATION, 0);
    supervisor.update(
        ACQUIRE_TIMEOUT_MS,
        ACQUIRE_TIMEOUT_MS,
        feedback(100.0, 20.0),
    );
    let now_ms = ACQUIRE_TIMEOUT_MS + 10_000;
    supervisor.restart_tracking(now_ms);
    let update = supervisor.update(now_ms, now_ms, feedback(20.0, 0.0));
    assert_eq!(update.tracking.phase, PassPhase::Fault);
    assert_eq!(update.decision.state, ControlState::AcquisitionTimeout);
    assert_eq!(update.decision.command, Command::Inhibit);
}

#[test]
fn fresh_feedback_acquires_tracks_pauses_then_acquires_the_reversed_pass() {
    let mut supervisor = TrackingSupervisor::new(TEST_CALIBRATION, 0);
    let slewing = supervisor.update(0, 0, feedback(100.0, 20.0));
    assert_eq!(slewing.tracking.phase, PassPhase::Acquiring);
    assert_eq!(slewing.tracking.target_azimuth_tenths, Some(200));
    assert!(matches!(slewing.decision.command, Command::Drive(_)));
    // Every movement refresh is submitted to renew the actuator lease.
    assert_eq!(slewing.command, Some(slewing.decision.command));
    let refresh = supervisor.update(50, 50, feedback(100.0, 20.0));
    assert_eq!(refresh.command, Some(refresh.decision.command));

    let settling = supervisor.update(100, 100, feedback(20.0, 0.0));
    assert_eq!(settling.tracking.phase, PassPhase::Acquiring);
    assert_eq!(settling.command, Some(Command::Stop));
    let started = supervisor.update(1_100, 1_100, feedback(20.0, 0.0));
    assert_eq!(started.tracking.phase, PassPhase::Tracking);
    assert_eq!(started.tracking.phase_remaining_ms, METOP_PASS_DURATION_MS);
    assert_eq!(started.decision.command, Command::Stop);
    // An unchanged release is not resubmitted every tick.
    assert_eq!(started.command, None);

    let midpoint_ms = 1_100 + METOP_PASS_DURATION_MS / 2;
    let midpoint = supervisor.update(midpoint_ms, midpoint_ms, feedback(20.0, 0.0));
    assert_eq!(midpoint.tracking.target_azimuth_tenths, Some(1_800));
    assert_eq!(midpoint.tracking.target_elevation_tenths, Some(820));
    assert!(matches!(midpoint.decision.command, Command::Drive(_)));

    let pause_ms = 1_100 + METOP_PASS_DURATION_MS;
    let pause = supervisor.update(pause_ms, pause_ms, feedback(340.0, 0.0));
    assert_eq!(pause.tracking.phase, PassPhase::Pause);
    assert_eq!(pause.tracking.phase_remaining_ms, BETWEEN_PASS_PAUSE_MS);
    assert_eq!(pause.tracking.target_azimuth_tenths, None);
    assert_eq!(pause.decision.state, ControlState::Idle);
    assert_eq!(pause.decision.command, Command::Stop);
    let next_ms = pause_ms + BETWEEN_PASS_PAUSE_MS;
    let next = supervisor.update(next_ms, next_ms, feedback(340.0, 0.0));
    assert_eq!(next.tracking.phase, PassPhase::Acquiring);
    assert_eq!(next.tracking.pass_index, 1);
    assert_eq!(next.tracking.target_azimuth_tenths, Some(3_400));
}

#[test]
fn an_old_sample_cannot_complete_settling_or_replace_the_reported_position() {
    let mut supervisor = TrackingSupervisor::new(TEST_CALIBRATION, 0);
    let fresh = supervisor.update(0, 0, feedback(20.0, 0.0));
    let stale = supervisor.update(1_000, 499, feedback(22.0, 1.0));
    assert_eq!(stale.tracking.phase, PassPhase::Acquiring);
    assert_eq!(stale.tracking.azimuth_tenths, fresh.tracking.azimuth_tenths);
    assert_eq!(stale.tracking.elevation_tenths, 0);
    assert_eq!(stale.decision.state, ControlState::FeedbackStale);
    assert_eq!(stale.decision.command, Command::Inhibit);
    assert_eq!(stale.interlock.feedback(), None);
    assert_eq!(
        supervisor
            .update(1_100, 1_100, feedback(20.0, 0.0))
            .tracking
            .phase,
        PassPhase::Acquiring
    );
    assert_eq!(
        supervisor
            .update(2_100, 2_100, feedback(20.0, 0.0))
            .tracking
            .phase,
        PassPhase::Tracking
    );
}

#[test]
fn read_timeout_latches_bus_inhibition_and_rejects_later_successes() {
    let mut supervisor = TrackingSupervisor::new(TEST_CALIBRATION, 0);
    supervisor.update(0, 0, feedback(100.0, 20.0));
    let timeout = supervisor.update(600, 100, Err(FeedbackReadError::TimedOut));
    assert!(!supervisor.feedback_read_permitted());
    assert_eq!(timeout.decision.state, ControlState::FeedbackStale);
    assert_eq!(timeout.decision.command, Command::Inhibit);
    assert_eq!(timeout.raw_feedback, None);
    assert!(!timeout.interlock.motion_permitted(600));
    let resumed = supervisor.update(700, 700, feedback(20.0, 0.0));
    assert_eq!(resumed.decision, timeout.decision);
    assert_eq!(resumed.raw_feedback, None);
    assert_eq!(
        resumed.tracking.azimuth_tenths,
        timeout.tracking.azimuth_tenths
    );
}

#[test]
fn calibration_fault_keeps_raw_diagnostics_and_resets_controller_hysteresis() {
    let mut supervisor = TrackingSupervisor::new(TEST_CALIBRATION, 0);
    let moving = supervisor.update(0, 0, feedback(10.0, 0.0));
    assert!(matches!(moving.decision.command, Command::Drive(_)));
    let invalid = supervisor.update(
        100,
        100,
        Ok(FeedbackVoltages {
            a0_elevation_mv: 1_281,
            a1_azimuth_mv: 0,
        }),
    );
    assert_eq!(invalid.raw_feedback.unwrap().a1_mv, 0);
    assert_eq!(
        invalid.tracking.azimuth_tenths,
        moving.tracking.azimuth_tenths
    );
    assert_eq!(invalid.decision.state, ControlState::FeedbackInvalid);
    assert_eq!(invalid.decision.command, Command::Inhibit);
    // A three-degree error is within the hysteresis band: after the fault,
    // the prior clockwise latch must not cause movement to resume.
    let recovered = supervisor.update(200, 200, feedback(17.0, 0.0));
    assert_eq!(recovered.decision.state, ControlState::Tracking);
    assert_eq!(recovered.decision.command, Command::Stop);
    assert!(supervisor.feedback_read_permitted());
}

#[test]
fn acquisition_timeout_takes_precedence_and_remains_latched_after_recovery() {
    let mut supervisor = TrackingSupervisor::new(TEST_CALIBRATION, 0);
    let timeout = supervisor.update(
        ACQUIRE_TIMEOUT_MS,
        ACQUIRE_TIMEOUT_MS,
        Err(FeedbackReadError::Unavailable),
    );
    assert_eq!(timeout.tracking.phase, PassPhase::Fault);
    assert_eq!(timeout.tracking.target_azimuth_tenths, None);
    assert_eq!(timeout.decision.state, ControlState::AcquisitionTimeout);
    assert_eq!(timeout.decision.command, Command::Inhibit);
    let recovered = supervisor.update(
        ACQUIRE_TIMEOUT_MS + 100,
        ACQUIRE_TIMEOUT_MS + 100,
        feedback(20.0, 0.0),
    );
    assert_eq!(recovered.decision, timeout.decision);
    assert_eq!(recovered.command, None);
}

#[test]
fn a_future_feedback_timestamp_is_rejected_without_counting_toward_settling() {
    let mut supervisor = TrackingSupervisor::new(TEST_CALIBRATION, 0);
    let future = supervisor.update(100, 101, feedback(20.0, 0.0));
    assert_eq!(future.decision.state, ControlState::TimestampInvalid);
    assert_eq!(future.decision.command, Command::Inhibit);
    assert_eq!(future.tracking.azimuth_tenths, 0);
    assert_eq!(
        supervisor
            .update(1_100, 1_100, feedback(20.0, 0.0))
            .tracking
            .phase,
        PassPhase::Acquiring
    );
}

#[test]
fn feedback_faults_clear_applied_outputs_during_the_minimum_on_period() {
    let cases = [
        (
            Err(FeedbackReadError::TimedOut),
            ControlState::FeedbackStale,
        ),
        (
            Err(FeedbackReadError::Unavailable),
            ControlState::FeedbackUnavailable,
        ),
        (
            Ok(FeedbackVoltages {
                a0_elevation_mv: 1_281,
                a1_azimuth_mv: 0,
            }),
            ControlState::FeedbackInvalid,
        ),
        (
            Ok(FeedbackVoltages {
                a0_elevation_mv: 1_281,
                a1_azimuth_mv: FEEDBACK_FULL_SCALE.positive_saturation_mv(),
            }),
            ControlState::FeedbackInvalid,
        ),
    ];
    for (readings, expected_state) in cases {
        let mut supervisor = TrackingSupervisor::new(TEST_CALIBRATION, 2_000);
        let mut actuator = Actuator::new();
        let moving = supervisor.update(2_000, 2_000, feedback(100.0, 20.0));
        actuator.command(moving.command.unwrap(), &moving.interlock, 2_000);
        assert!(actuator.actual().is_active());

        // The relay has been on for 100 ms of its ordinary 500 ms hold.
        let fault = supervisor.update(2_100, 2_050, readings);
        assert_eq!(fault.decision.state, expected_state);
        assert_eq!(fault.interlock.feedback(), None);
        // The published interlock alone removes the output at once.
        assert!(actuator.service(&fault.interlock, 2_100));
        assert_eq!(actuator.actual(), DriveCommand::IDLE);
        assert_eq!(actuator.target(), DriveCommand::IDLE);
        assert_eq!(fault.command, Some(Command::Inhibit));
        actuator.command(Command::Inhibit, &fault.interlock, 2_100);
        actuator.service(&fault.interlock, 2_500);
        assert_eq!(actuator.actual(), DriveCommand::IDLE);
    }
}

#[test]
fn manual_feedback_errors_remove_readiness_and_only_uncancelled_reads_can_recover() {
    let mut supervisor = ManualSupervisor::new(TEST_CALIBRATION);
    let initial = supervisor.update(100, 98, feedback(100.0, 20.0));
    assert_eq!(initial.decision.state, ControlState::Idle);
    assert_eq!(initial.interlock.feedback().unwrap().timestamp_ms, 98);
    assert_eq!(initial.command, None);
    let missing = supervisor.update(200, 198, Err(FeedbackReadError::Unavailable));
    assert_eq!(missing.decision.state, ControlState::FeedbackUnavailable);
    assert_eq!(missing.decision.command, Command::Inhibit);
    assert_eq!(missing.interlock.feedback(), None);
    assert_eq!(missing.command, None);
    assert_eq!(
        missing.tracking.azimuth_tenths,
        initial.tracking.azimuth_tenths
    );
    assert!(supervisor.feedback_read_permitted());
    assert!(supervisor
        .update(300, 298, feedback(110.0, 20.0))
        .interlock
        .feedback()
        .is_some());
    let timeout = supervisor.update(900, 400, Err(FeedbackReadError::TimedOut));
    assert_eq!(timeout.decision.state, ControlState::FeedbackStale);
    assert_eq!(timeout.decision.command, Command::Inhibit);
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
        assert_eq!(update.decision.state, state);
        assert_eq!(update.decision.command, Command::Inhibit);
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
    assert_eq!(update.decision.state, ControlState::Idle);
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
    assert_eq!(update.decision.state, ControlState::CalibrationInvalid);
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
            .decision
            .state,
        ControlState::FeedbackUnavailable
    );

    let mut tracking = TrackingSupervisor::new(rejected, 0);
    assert_eq!(
        tracking.calibration_error(),
        Some(CalibrationError::AdcRangeTooNarrow)
    );
    let first = tracking.update(10, 5, readings);
    assert_eq!(first.raw_feedback.unwrap().a1_mv, 1_100);
    assert_eq!(first.tracking.phase, PassPhase::Fault);
    assert_eq!(first.decision.state, ControlState::CalibrationInvalid);
    assert_eq!(first.command, Some(Command::Inhibit));
    assert!(!first.interlock.motion_permitted(10));
    assert_eq!(tracking.update(110, 105, readings).command, None);
}
