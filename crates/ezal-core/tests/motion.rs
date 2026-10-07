//! Exercise motion supervision through the same boundary used by firmware.

mod common;

use common::TEST_CALIBRATION;
use ezal_core::actuator::Actuator;
use ezal_core::control::TimedPointing;
use ezal_core::drive::{
    AzimuthDirection, Command, DriveCommand, ElevationDirection, OUTPUT_MIN_INACTIVE_MS,
};
use ezal_core::interlock::FeedbackInterlock;
use ezal_core::motion::{MotionConfig, MotionFault};
use ezal_core::position::Pointing;

const START: Pointing = Pointing::new(180.0, 45.0);
const BOTH: DriveCommand = DriveCommand {
    azimuth: Some(AzimuthDirection::Clockwise),
    elevation: Some(ElevationDirection::Up),
};
const REVERSE: DriveCommand = DriveCommand {
    azimuth: Some(AzimuthDirection::CounterClockwise),
    elevation: Some(ElevationDirection::Down),
};

fn gate(position: Pointing, timestamp_ms: u64) -> FeedbackInterlock {
    let mut gate = FeedbackInterlock::new(TEST_CALIBRATION);
    gate.publish(Some(TimedPointing::new(position, timestamp_ms)));
    gate
}

fn refresh(actuator: &mut Actuator, now_ms: u64, position: Pointing, drive: DriveCommand) {
    let gate = gate(position, now_ms);
    actuator.service(&gate, now_ms);
    actuator.command(Command::Jog(drive), &gate, now_ms);
}

#[test]
fn fresh_identical_feedback_cannot_sustain_either_axis_for_120_seconds() {
    for (drive, expected) in [
        (
            DriveCommand {
                azimuth: BOTH.azimuth,
                elevation: None,
            },
            MotionFault::AzimuthNoProgress,
        ),
        (
            DriveCommand {
                azimuth: None,
                elevation: BOTH.elevation,
            },
            MotionFault::ElevationNoProgress,
        ),
    ] {
        let mut actuator = Actuator::new();
        for now_ms in (0..=120_000).step_by(100) {
            refresh(&mut actuator, now_ms, START, drive);
            if now_ms < MotionConfig::DEFAULT.progress_timeout_ms {
                assert_eq!(actuator.actual(), drive);
                assert_eq!(actuator.fault(), None);
            } else {
                assert_eq!(actuator.actual(), DriveCommand::IDLE);
                assert_eq!(actuator.target(), DriveCommand::IDLE);
                assert_eq!(actuator.fault(), Some(expected));
            }
        }
    }
}

#[test]
fn a_stalled_axis_latches_both_outputs_off_even_when_the_other_moves() {
    for stalled_azimuth in [true, false] {
        let mut actuator = Actuator::new();
        for now_ms in (0..=5_000).step_by(100) {
            let seconds = now_ms as f32 / 1_000.0;
            let position = if stalled_azimuth {
                Pointing::new(START.azimuth_deg, START.elevation_deg + seconds)
            } else {
                Pointing::new(START.azimuth_deg + seconds * 3.0, START.elevation_deg)
            };
            refresh(&mut actuator, now_ms, position, BOTH);
        }
        assert_eq!(actuator.actual(), DriveCommand::IDLE);
        assert_eq!(
            actuator.fault(),
            Some(if stalled_azimuth {
                MotionFault::AzimuthNoProgress
            } else {
                MotionFault::ElevationNoProgress
            })
        );
    }
}

#[test]
fn quantized_healthy_motion_and_reversal_continue_without_fault() {
    let mut actuator = Actuator::new();
    // Realistic quantized samples include repeated codes on the slower axis.
    let mut position = START;
    for now_ms in (0..=10_000).step_by(100) {
        position = Pointing::new(
            180.0 + (now_ms / 200) as f32 * 0.4,
            45.0 + (now_ms / 400) as f32 * 0.2,
        );
        refresh(&mut actuator, now_ms, position, BOTH);
        assert_eq!(actuator.actual(), BOTH);
        assert_eq!(actuator.fault(), None);
    }

    // Reversal stops both outputs at once and includes a real inactive hold.
    refresh(&mut actuator, 10_100, position, REVERSE);
    assert_eq!(actuator.actual(), DriveCommand::IDLE);
    for now_ms in (10_200..12_100).step_by(100) {
        refresh(&mut actuator, now_ms, position, REVERSE);
        assert_eq!(actuator.actual(), DriveCommand::IDLE);
        assert_eq!(actuator.fault(), None);
    }
    let turnaround = position;
    for now_ms in (12_100..=22_100).step_by(100) {
        let elapsed = now_ms - 12_100;
        position = Pointing::new(
            turnaround.azimuth_deg - (elapsed / 200) as f32 * 0.4,
            turnaround.elevation_deg - (elapsed / 400) as f32 * 0.2,
        );
        refresh(&mut actuator, now_ms, position, REVERSE);
        assert_eq!(actuator.actual(), REVERSE);
        assert_eq!(actuator.fault(), None);
    }
}

#[test]
fn idle_and_minimum_off_time_do_not_consume_the_progress_budget() {
    let mut actuator = Actuator::new();
    for now_ms in (0..=1_000).step_by(100) {
        refresh(&mut actuator, now_ms, START, BOTH);
    }
    actuator.command(Command::Inhibit, &gate(START, 1_000), 1_000);
    for now_ms in (1_100..=21_000).step_by(100) {
        actuator.service(&gate(START, now_ms), now_ms);
        assert_eq!(actuator.fault(), None);
    }
    // The previous 1 s of actual energisation is retained; the 20 s idle
    // interval contributes nothing, and another 4 s of stalled motion trips.
    for now_ms in (21_000..=25_000).step_by(100) {
        refresh(&mut actuator, now_ms, START, BOTH);
        if now_ms < 25_000 {
            assert_eq!(actuator.actual(), BOTH);
        }
    }
    assert_eq!(actuator.fault(), Some(MotionFault::AzimuthNoProgress));
    assert_eq!(actuator.actual(), DriveCommand::IDLE);
}

#[test]
fn short_reversing_jogs_cannot_repeatedly_reset_a_stalled_motor_budget() {
    let mut actuator = Actuator::new();
    let mut now_ms = 0;
    for run in 0..5 {
        let drive = if run % 2 == 0 { BOTH } else { REVERSE };
        let started_ms = now_ms;
        for _ in 0..=10 {
            refresh(&mut actuator, now_ms, START, drive);
            now_ms += 100;
        }
        actuator.command(Command::Inhibit, &gate(START, now_ms), now_ms);
        now_ms = started_ms + 1_100 + OUTPUT_MIN_INACTIVE_MS;
    }
    assert_eq!(actuator.actual(), DriveCommand::IDLE);
    assert_eq!(actuator.fault(), Some(MotionFault::AzimuthNoProgress));
}

#[test]
fn harmless_code_jitter_is_not_progress_or_a_direction_fault() {
    let mut actuator = Actuator::new();
    for now_ms in (0..=5_000).step_by(100) {
        let noise = if now_ms % 200 == 0 { 0.0 } else { 1.0 };
        refresh(
            &mut actuator,
            now_ms,
            Pointing::new(180.0 + noise * 0.4, 45.0 + noise * 0.2),
            BOTH,
        );
        if now_ms < 5_000 {
            assert_eq!(actuator.fault(), None);
        }
    }
    assert_eq!(actuator.fault(), Some(MotionFault::AzimuthNoProgress));
}

#[test]
fn timestamps_must_advance_before_a_changed_value_can_prove_progress() {
    let mut actuator = Actuator::new();
    for now_ms in (0..=4_900).step_by(100) {
        refresh(&mut actuator, now_ms, START, BOTH);
    }
    // Repeated publication with the same acquisition stamp is not a new
    // physical observation, even if a caller mutates its coordinates.
    let duplicate = gate(Pointing::new(190.0, 50.0), 4_900);
    actuator.service(&duplicate, 5_000);
    assert_eq!(actuator.fault(), Some(MotionFault::AzimuthNoProgress));
    assert_eq!(actuator.actual(), DriveCommand::IDLE);
}

#[test]
fn motion_deadline_cuts_without_waiting_for_another_sample_or_refresh() {
    let mut actuator = Actuator::new();
    for now_ms in (0..=4_900).step_by(100) {
        refresh(&mut actuator, now_ms, START, BOTH);
    }
    let feedback = gate(START, 4_900);
    assert_eq!(actuator.next_deadline_ms(&feedback, 4_950), Some(5_000));
    assert!(actuator.service(&feedback, 5_000));
    assert_eq!(actuator.actual(), DriveCommand::IDLE);
    assert_eq!(actuator.fault(), Some(MotionFault::AzimuthNoProgress));
    assert_eq!(actuator.next_deadline_ms(&feedback, 5_000), None);
}

#[test]
fn actuator_preserves_source_expiry_through_feedback_and_motion_checks() {
    let mut actuator = Actuator::new();
    actuator.command_until(Command::Jog(BOTH), &gate(START, 100), 100, 700);
    assert_eq!(actuator.actual(), BOTH);
    assert_eq!(actuator.lease_deadline_ms(), Some(700));
    let feedback = gate(START, 600);
    actuator.command_until(Command::Jog(BOTH), &feedback, 600, 700);
    assert_eq!(actuator.next_deadline_ms(&feedback, 600), Some(700));
    actuator.service(&feedback, 700);
    assert_eq!(actuator.actual(), DriveCommand::IDLE);
    assert_eq!(actuator.target(), DriveCommand::IDLE);
    assert_eq!(actuator.fault(), None);
}

#[test]
fn feedback_recovery_and_new_commands_never_clear_a_motion_fault() {
    let mut actuator = Actuator::new();
    for now_ms in (0..=5_000).step_by(100) {
        refresh(&mut actuator, now_ms, START, BOTH);
    }
    let mut feedback = gate(START, 5_100);
    feedback.publish(None);
    actuator.service(&feedback, 5_100);
    for (index, command) in [
        Command::Stop,
        Command::Inhibit,
        Command::Drive(REVERSE),
        Command::Jog(BOTH),
    ]
    .into_iter()
    .enumerate()
    {
        let now_ms = 10_000 + index as u64 * 100;
        actuator.command(command, &gate(Pointing::new(190.0, 50.0), now_ms), now_ms);
        assert_eq!(actuator.actual(), DriveCommand::IDLE);
        assert_eq!(actuator.target(), DriveCommand::IDLE);
        assert_eq!(actuator.fault(), Some(MotionFault::AzimuthNoProgress));
    }
}

#[test]
fn slow_motion_against_either_applied_direction_latches_a_direction_fault() {
    for (axis, drive, expected) in [
        (0, BOTH, MotionFault::AzimuthWrongDirection),
        (0, REVERSE, MotionFault::AzimuthWrongDirection),
        (1, BOTH, MotionFault::ElevationWrongDirection),
        (1, REVERSE, MotionFault::ElevationWrongDirection),
    ] {
        let mut actuator = Actuator::new();
        let sign = if drive == BOTH { -1.0 } else { 1.0 };
        for now_ms in (0..=3_000).step_by(100) {
            let offset = now_ms as f32 / 1_000.0 * sign;
            let position = if axis == 0 {
                Pointing::new(180.0 + offset, 45.0 - offset)
            } else {
                Pointing::new(180.0 - offset, 45.0 + offset)
            };
            refresh(&mut actuator, now_ms, position, drive);
        }
        assert_eq!(actuator.fault(), Some(expected));
        assert_eq!(actuator.actual(), DriveCommand::IDLE);
    }
}

#[test]
fn impossible_feedback_jumps_latch_each_axis_after_startup_settles() {
    for (position, fault) in [
        (
            Pointing::new(200.0, 45.0),
            MotionFault::AzimuthImplausibleSpeed,
        ),
        (
            Pointing::new(180.0, 60.0),
            MotionFault::ElevationImplausibleSpeed,
        ),
    ] {
        let mut actuator = Actuator::new();
        for now_ms in (0..=1_500).step_by(100) {
            refresh(&mut actuator, now_ms, START, BOTH);
        }
        refresh(&mut actuator, 1_600, position, BOTH);
        assert_eq!(actuator.fault(), Some(fault));
        assert_eq!(actuator.actual(), DriveCommand::IDLE);
    }
}

#[test]
fn small_fast_samples_cannot_reuse_the_noise_allowance_indefinitely() {
    for azimuth in [true, false] {
        let mut actuator = Actuator::new();
        for now_ms in (0..=2_000).step_by(100) {
            let seconds = now_ms as f32 / 1_000.0;
            // Each individual change fits below the per-sample speed+noise
            // allowance. The longer comparison still detects the excess rate.
            let position = if azimuth {
                Pointing::new(180.0 + seconds * 9.0, 45.0 + seconds)
            } else {
                Pointing::new(180.0 + seconds * 3.0, 45.0 + seconds * 4.0)
            };
            refresh(&mut actuator, now_ms, position, BOTH);
        }
        assert_eq!(
            actuator.fault(),
            Some(if azimuth {
                MotionFault::AzimuthImplausibleSpeed
            } else {
                MotionFault::ElevationImplausibleSpeed
            })
        );
        assert_eq!(actuator.actual(), DriveCommand::IDLE);
    }
}

#[test]
fn startup_can_be_stationary_before_quantized_movement_begins() {
    let mut actuator = Actuator::new();
    for now_ms in (0_u64..=4_000).step_by(100) {
        let elapsed = now_ms.saturating_sub(900);
        let position = Pointing::new(
            180.0 + (elapsed / 200) as f32 * 0.4,
            45.0 + (elapsed / 400) as f32 * 0.2,
        );
        refresh(&mut actuator, now_ms, position, BOTH);
        assert_eq!(actuator.fault(), None);
        assert_eq!(actuator.actual(), BOTH);
    }
}

#[test]
fn unusable_thresholds_inhibit_before_activation() {
    for config in [
        MotionConfig {
            progress_timeout_ms: 1_000,
            ..MotionConfig::DEFAULT
        },
        MotionConfig {
            progress_codes: 2,
            ..MotionConfig::DEFAULT
        },
        MotionConfig {
            noise_codes: 0,
            ..MotionConfig::DEFAULT
        },
    ] {
        let mut actuator = Actuator::with_motion_config(config);
        refresh(&mut actuator, 0, START, BOTH);
        assert_eq!(actuator.actual(), DriveCommand::IDLE);
        assert_eq!(actuator.fault(), Some(MotionFault::InvalidConfiguration));
    }
}

#[test]
fn a_backwards_clock_inhibits_instead_of_renewing_the_progress_budget() {
    let mut actuator = Actuator::new();
    refresh(&mut actuator, 100, START, BOTH);
    refresh(&mut actuator, 99, START, BOTH);
    assert_eq!(actuator.fault(), Some(MotionFault::ClockWentBackwards));
    assert_eq!(actuator.actual(), DriveCommand::IDLE);
}
