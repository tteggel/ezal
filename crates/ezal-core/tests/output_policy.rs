//! Shared output policy for manual holds and future tracking/preparation pulses.

mod common;

use ezal_core::actuator::{Actuator, ActuatorConfig, ActuatorInput, ActuatorUpdate};
use ezal_core::drive::{
    AzimuthDirection, Command, DriveCommand, ElevationDirection, RelayConfig, MOVEMENT_LEASE_MS,
};
use ezal_core::duty::{DutyConfig, DutyHistory};
use ezal_core::interlock::FeedbackInterlock;
use ezal_core::mailbox::CommandEnvelope;
use ezal_core::motion::MotionFault;
use ezal_core::network::NetworkGate;
use ezal_core::position::Pointing;

const BOTH: DriveCommand = DriveCommand {
    azimuth: Some(AzimuthDirection::Clockwise),
    elevation: Some(ElevationDirection::Up),
};
const AZ: DriveCommand = DriveCommand {
    azimuth: BOTH.azimuth,
    elevation: None,
};
const EL: DriveCommand = DriveCommand {
    azimuth: None,
    elevation: BOTH.elevation,
};

#[test]
fn short_pulse_progress_includes_first_post_coast_feedback_without_forgiving_stalls() {
    let service = |actuator: &mut Actuator, now, sampled, elevation, command| {
        let mut interlock = FeedbackInterlock::new(common::installed());
        interlock.publish(Some(common::sample(
            Pointing::new(180.0, elevation),
            sampled,
        )));
        let mut network = NetworkGate::new();
        network.observe(now, true);
        actuator.step(
            ActuatorInput {
                interlock,
                network,
                command: Some(CommandEnvelope {
                    command,
                    valid_until_ms: now + MOVEMENT_LEASE_MS,
                }),
            },
            now,
        )
    };
    for (movement, delay, expected) in [
        (1.0, 1100, None),
        (0.0, 1100, Some(MotionFault::ElevationNoProgress)),
        (-1.0, 1100, Some(MotionFault::ElevationWrongDirection)),
        // Movement observed long after coast cannot replenish a stalled pulse budget.
        (1.0, 1600, Some(MotionFault::ElevationNoProgress)),
    ] {
        let mut actuator = common::rested_actuator();
        for pulse in 0..12 {
            let start = pulse * 2500;
            let position = 30.0 + pulse as f32 * movement;
            service(&mut actuator, start, start, position, Command::Drive(EL));
            service(
                &mut actuator,
                start + 250,
                start,
                position,
                Command::Drive(EL),
            );
            service(
                &mut actuator,
                start + 500,
                start,
                position,
                Command::Inhibit,
            );
            service(
                &mut actuator,
                start + delay,
                start + delay,
                position + movement,
                Command::Inhibit,
            );
            if actuator.fault().is_some() {
                break;
            }
        }
        assert_eq!(
            actuator.fault(),
            expected,
            "movement={movement}, delay={delay}"
        );
    }
}

fn step_at(
    actuator: &mut Actuator,
    now_ms: u64,
    sampled_ms: u64,
    command: Option<Command>,
) -> ActuatorUpdate {
    let mut interlock = FeedbackInterlock::new(common::installed());
    interlock.publish(Some(common::sample(Pointing::new(180.0, 45.0), sampled_ms)));
    let mut network = NetworkGate::new();
    network.observe(now_ms, true);
    actuator.step(
        ActuatorInput {
            interlock,
            network,
            command: command.map(|command| CommandEnvelope {
                command,
                valid_until_ms: now_ms + MOVEMENT_LEASE_MS,
            }),
        },
        now_ms,
    )
}

fn step(actuator: &mut Actuator, now_ms: u64, command: Option<Command>) -> ActuatorUpdate {
    step_at(actuator, now_ms, now_ms, command)
}

fn policy() -> ActuatorConfig {
    ActuatorConfig {
        relays: RelayConfig::TRACKING_CANDIDATE,
        duty: DutyConfig {
            energized_ms: 1_200,
            rest_ms: 6_000,
        },
        ..ActuatorConfig::DEFAULT
    }
}

#[test]
fn unknown_boot_requires_full_rest_and_never_replays_a_refused_request() {
    let mut actuator = Actuator::new();
    let boot = step(&mut actuator, 100, Some(Command::Jog(BOTH)));
    assert_eq!(boot.output, DriveCommand::IDLE);
    assert_eq!(boot.status.duty.azimuth.remaining_ms, 0);
    assert_eq!(boot.status.duty.azimuth.cooldown_ms, 900_000);
    assert_eq!(actuator.target(), DriveCommand::IDLE);
    let almost = step(&mut actuator, 900_099, Some(Command::Drive(BOTH)));
    assert_eq!(almost.output, DriveCommand::IDLE);
    assert_eq!(almost.status.duty.azimuth.cooldown_ms, 1);
    assert_eq!(almost.next_deadline_ms, Some(900_100));
    let rested = step(&mut actuator, 900_100, None);
    assert_eq!(rested.output, DriveCommand::IDLE);
    assert_eq!(rested.status.duty.azimuth.remaining_ms, 180_000);
    assert_eq!(rested.status.duty.elevation.cooldown_ms, 0);
    assert_eq!(
        step(&mut actuator, 900_101, Some(Command::Jog(BOTH))).output,
        BOTH
    );
}

#[test]
fn duty_survives_manual_controller_pulses_and_cancellation_and_recovers_per_axis() {
    let mut actuator = Actuator::with_config(policy(), DutyHistory::Known([0, 0]));
    assert_eq!(
        step(&mut actuator, 0, Some(Command::Jog(BOTH))).output,
        BOTH
    );
    step(&mut actuator, 500, Some(Command::Inhibit));
    assert_eq!(
        step(&mut actuator, 1_000, Some(Command::Drive(AZ))).output,
        AZ
    );
    let cancelled = step(&mut actuator, 1_500, Some(Command::Inhibit));
    assert_eq!(cancelled.status.duty.azimuth.remaining_ms, 200);
    assert_eq!(cancelled.status.duty.azimuth.cooldown_ms, 6_000);
    assert_eq!(cancelled.status.duty.elevation.remaining_ms, 700);
    // An unusably short remaining allowance cannot start a new minimum pulse.
    assert_eq!(
        step(&mut actuator, 2_000, Some(Command::Jog(BOTH))).output,
        EL
    );
    assert_eq!(actuator.target(), EL);
    let released = step(&mut actuator, 2_500, Some(Command::Inhibit));
    assert_eq!(released.status.duty.azimuth.cooldown_ms, 5_000);
    assert_eq!(released.status.duty.elevation.cooldown_ms, 6_000);
    let azimuth_rested = step(&mut actuator, 7_500, None);
    assert_eq!(azimuth_rested.output, DriveCommand::IDLE);
    assert_eq!(azimuth_rested.status.duty.azimuth.remaining_ms, 1_200);
    assert_eq!(azimuth_rested.status.duty.elevation.remaining_ms, 200);
    assert_eq!(azimuth_rested.status.duty.elevation.cooldown_ms, 1_000);
    let both_rested = step(&mut actuator, 8_500, None);
    assert_eq!(both_rested.status.duty.elevation.remaining_ms, 1_200);
    assert_eq!(both_rested.output, DriveCommand::IDLE);
    assert_eq!(both_rested.status.motion_fault, None);
}

#[test]
fn exhaustion_has_an_exact_deadline_and_only_cuts_the_exhausted_axis() {
    let mut actuator = Actuator::with_config(policy(), DutyHistory::Known([600, 0]));
    step(&mut actuator, 0, Some(Command::Drive(BOTH)));
    let nearing = step(&mut actuator, 400, Some(Command::Drive(BOTH)));
    assert_eq!(nearing.output, BOTH);
    assert_eq!(nearing.next_deadline_ms, Some(600));
    let exhausted = step(&mut actuator, 600, Some(Command::Drive(BOTH)));
    assert_eq!(exhausted.output, EL);
    assert_eq!(actuator.target(), EL);
    assert_eq!(exhausted.status.duty.azimuth.remaining_ms, 0);
    assert_eq!(exhausted.status.duty.elevation.remaining_ms, 600);
    assert_eq!(
        step(&mut actuator, 630, Some(Command::Drive(BOTH))).output,
        EL
    );
    assert_eq!(actuator.fault(), None);
}

#[test]
fn candidate_timing_distinguishes_repeated_direction_from_reversal() {
    let mut stopped = Actuator::with_config(
        ActuatorConfig {
            relays: RelayConfig::TRACKING_CANDIDATE,
            ..ActuatorConfig::DEFAULT
        },
        DutyHistory::Known([0, 0]),
    );
    step(&mut stopped, 0, Some(Command::Drive(AZ)));
    // Cancellation during preparation must cut even before the minimum on-time.
    step(&mut stopped, 200, Some(Command::Inhibit));
    let mut same = stopped;
    let mut reverse = stopped;
    let opposite = DriveCommand {
        azimuth: Some(AzimuthDirection::CounterClockwise),
        elevation: None,
    };
    for (actuator, drive) in [(&mut same, AZ), (&mut reverse, opposite)] {
        assert_eq!(
            step(actuator, 600, Some(Command::Drive(drive))).output,
            DriveCommand::IDLE
        );
    }
    // Release at an activation deadline always wins, without a brief pulse.
    for release in [
        Command::Drive(DriveCommand::IDLE),
        Command::Inhibit,
        Command::Jog(DriveCommand::IDLE),
    ] {
        let mut cancelled = same;
        assert_eq!(
            step(&mut cancelled, 700, Some(release)).output,
            DriveCommand::IDLE
        );
        assert_eq!(cancelled.target(), DriveCommand::IDLE);
    }
    let mut stale = same;
    assert_eq!(
        step_at(&mut stale, 700, 0, Some(Command::Drive(AZ))).output,
        DriveCommand::IDLE
    );
    assert_eq!(stale.target(), DriveCommand::IDLE);
    assert_eq!(step(&mut same, 700, Some(Command::Drive(AZ))).output, AZ);
    assert_eq!(
        step(&mut reverse, 700, Some(Command::Drive(opposite))).output,
        DriveCommand::IDLE
    );
    assert_eq!(
        step(&mut reverse, 1_199, Some(Command::Drive(opposite))).output,
        DriveCommand::IDLE
    );
    assert_eq!(
        step(&mut reverse, 1_200, Some(Command::Drive(opposite))).output,
        opposite
    );
    assert_eq!(reverse.status().duty.azimuth.remaining_ms, 179_800);
}

#[test]
fn invalid_policy_or_history_inhibits_before_any_activation() {
    let base = ActuatorConfig::DEFAULT;
    let mut bad_speed = base.travel;
    bad_speed.azimuth.max_speed_deg_per_s = f32::NAN;
    let mut bad_coast = base.travel;
    bad_coast.elevation.coast_ms = u64::MAX;
    for config in [
        ActuatorConfig {
            relays: RelayConfig {
                min_active_ms: 0,
                ..base.relays
            },
            ..base
        },
        ActuatorConfig {
            relays: RelayConfig {
                min_active_ms: 751,
                ..base.relays
            },
            ..base
        },
        ActuatorConfig {
            relays: RelayConfig {
                same_direction_off_ms: 0,
                ..base.relays
            },
            ..base
        },
        ActuatorConfig {
            relays: RelayConfig {
                same_direction_off_ms: 500,
                reversal_off_ms: 999,
                ..base.relays
            },
            ..base
        },
        ActuatorConfig {
            duty: DutyConfig {
                energized_ms: 499,
                ..base.duty
            },
            ..base
        },
        ActuatorConfig {
            duty: DutyConfig {
                rest_ms: 0,
                ..base.duty
            },
            ..base
        },
        ActuatorConfig {
            travel: bad_speed,
            ..base
        },
        ActuatorConfig {
            travel: bad_coast,
            ..base
        },
    ] {
        assert!(!config.is_valid());
        let mut actuator = Actuator::with_config(config, DutyHistory::Known([0, 0]));
        let update = step(&mut actuator, 0, Some(Command::Drive(BOTH)));
        assert_eq!(update.output, DriveCommand::IDLE);
        assert_eq!(
            update.status.motion_fault,
            Some(MotionFault::InvalidConfiguration)
        );
    }
    for history in [[180_001, 0], [0, 180_001]] {
        let mut actuator = Actuator::with_config(base, DutyHistory::Known(history));
        assert_eq!(
            step(&mut actuator, 0, Some(Command::Jog(BOTH)))
                .status
                .motion_fault,
            Some(MotionFault::InvalidConfiguration)
        );
        assert_eq!(actuator.actual(), DriveCommand::IDLE);
    }
}
