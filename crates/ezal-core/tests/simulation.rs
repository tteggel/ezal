//! Tests for the deterministic METOP-C scheduler and acquisition sequencer.

mod common;

use common::TEST_CALIBRATION;
use ezal_core::simulation::{
    MetopPassScheduler, PassPhase, TrackingSequence, ACQUIRE_SETTLE_MS, ACQUIRE_TIMEOUT_MS,
    BETWEEN_PASS_PAUSE_MS, METOP_PASS_DURATION_MS, SIMULATED_SATELLITE,
};

#[test]
fn schedule_starts_with_metop_c_then_pauses_for_exactly_thirty_seconds() {
    assert_eq!(SIMULATED_SATELLITE, "METOP-C");
    let scheduler = MetopPassScheduler::new();

    let boot = scheduler.sample(0);
    assert_eq!(boot.pass_index, 0);
    assert_eq!(boot.phase, PassPhase::Tracking);
    assert!(boot.target.is_some());

    let pause = scheduler.sample(METOP_PASS_DURATION_MS);
    assert_eq!(pause.phase, PassPhase::Pause);
    assert_eq!(pause.phase_remaining_ms, BETWEEN_PASS_PAUSE_MS);
    assert_eq!(pause.target, None);

    let last_pause_ms = METOP_PASS_DURATION_MS + BETWEEN_PASS_PAUSE_MS - 1;
    assert_eq!(scheduler.sample(last_pause_ms).phase, PassPhase::Pause);

    let second = scheduler.sample(METOP_PASS_DURATION_MS + BETWEEN_PASS_PAUSE_MS);
    assert_eq!(second.pass_index, 1);
    assert_eq!(second.phase, PassPhase::Tracking);
    assert!(second.target.is_some());
}

#[test]
fn sequence_acquires_and_settles_before_starting_the_pass_clock() {
    let scheduler = MetopPassScheduler::for_calibration(TEST_CALIBRATION).unwrap();
    let start = scheduler.target_for_pass(0, 0);
    let mut sequence = TrackingSequence::new(scheduler, 0);

    assert_eq!(sequence.update(0, None).phase, PassPhase::Acquiring);
    assert_eq!(
        sequence.update(100, Some(start)).phase,
        PassPhase::Acquiring
    );
    assert_eq!(
        sequence
            .update(100 + ACQUIRE_SETTLE_MS - 1, Some(start))
            .phase,
        PassPhase::Acquiring
    );

    let tracking = sequence.update(100 + ACQUIRE_SETTLE_MS, Some(start));
    assert_eq!(tracking.phase, PassPhase::Tracking);
    assert_eq!(tracking.phase_remaining_ms, METOP_PASS_DURATION_MS);
}

#[test]
fn sequence_fails_safe_when_start_position_cannot_be_acquired() {
    let scheduler = MetopPassScheduler::for_calibration(TEST_CALIBRATION).unwrap();
    let mut sequence = TrackingSequence::new(scheduler, 0);

    let fault = sequence.update(ACQUIRE_TIMEOUT_MS, None);
    assert_eq!(fault.phase, PassPhase::Fault);
    assert_eq!(fault.target, None);
}

#[test]
fn the_calibration_bounds_the_entire_pass_profile() {
    let scheduler = MetopPassScheduler::for_calibration(TEST_CALIBRATION).unwrap();
    for elapsed_ms in (0..METOP_PASS_DURATION_MS).step_by(100) {
        let target = scheduler.sample(elapsed_ms).target.unwrap();
        assert!(target.is_valid_for(TEST_CALIBRATION));
    }
}
