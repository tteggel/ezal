//! Exercise the complete output step used by firmware, including permissions.

mod common;

use common::{installed, sample, TEST_CALIBRATION};
use ezal_core::actuator::{Actuator, ActuatorConfig, ActuatorInput, ActuatorUpdate};
use ezal_core::coordination::CoordinationState;
use ezal_core::drive::{AzimuthDirection, Command, DriveCommand, MOVEMENT_LEASE_MS};
use ezal_core::duty::DutyHistory;
use ezal_core::feedback::FeedbackMonitor;
use ezal_core::interlock::FeedbackInterlock;
use ezal_core::mailbox::CommandEnvelope;
use ezal_core::motion::{MotionConfig, MotionFault};
use ezal_core::network::NetworkGate;
use ezal_core::position::Pointing;

const DRIVE: DriveCommand = DriveCommand {
    azimuth: Some(AzimuthDirection::Clockwise),
    elevation: None,
};
const MOVE: Command = Command::Jog(DRIVE);

fn feedback(sampled_at_ms: u64) -> FeedbackInterlock {
    let mut interlock = FeedbackInterlock::new(installed());
    interlock.publish(Some(sample(Pointing::new(180.0, 45.0), sampled_at_ms)));
    interlock
}

// Each snapshot represents a fresh observation from the same healthy network
// generation. Network transition tests use a persistent NetworkGate instead.
fn ready(now_ms: u64) -> NetworkGate {
    let mut network = NetworkGate::new();
    network.observe(now_ms, true);
    network
}

fn command(command: Command, now_ms: u64) -> CommandEnvelope {
    CommandEnvelope {
        command,
        valid_until_ms: now_ms + MOVEMENT_LEASE_MS,
    }
}

fn step(
    actuator: &mut Actuator,
    now_ms: u64,
    network: NetworkGate,
    command: Option<CommandEnvelope>,
) -> ActuatorUpdate {
    actuator.step(
        ActuatorInput {
            interlock: feedback(now_ms),
            network,
            command,
        },
        now_ms,
    )
}

#[test]
fn boot_and_offline_permission_refuse_movement_without_a_past_wakeup() {
    let mut actuator = common::rested_actuator();
    let update = step(&mut actuator, 0, NetworkGate::new(), Some(command(MOVE, 0)));
    assert_eq!(update.output, DriveCommand::IDLE);
    assert_eq!(actuator.target(), DriveCommand::IDLE);
    assert_eq!(update.status.motion_fault, None);
    assert_eq!(update.next_deadline_ms, None);

    let mut stale = ready(0);
    let update = step(&mut actuator, 300, stale, Some(command(MOVE, 300)));
    assert_eq!(update.output, DriveCommand::IDLE);
    assert_eq!(update.next_deadline_ms, None);
    stale.expire(300);
    assert_eq!(step(&mut actuator, 300, stale, None).next_deadline_ms, None);
}

#[test]
fn network_permission_does_not_shorten_the_command_activation_lifetime() {
    let mut actuator = common::rested_actuator();
    let update = step(&mut actuator, 0, ready(0), Some(command(MOVE, 0)));
    // A 300 ms permission can be renewed or revoked independently; it must
    // not prevent an activation requiring 500 ms of command lifetime.
    assert_eq!(update.output, DRIVE);
    assert_eq!(actuator.lease_deadline_ms(), Some(750));
    assert_eq!(update.next_deadline_ms, Some(300));
}

#[test]
fn stalled_network_cuts_even_while_feedback_and_commands_keep_arriving() {
    let mut actuator = common::rested_actuator();
    let network = ready(0);
    for now_ms in (0..300).step_by(50) {
        assert_eq!(
            step(&mut actuator, now_ms, network, Some(command(MOVE, now_ms))).output,
            DRIVE
        );
    }
    let update = step(&mut actuator, 300, network, Some(command(MOVE, 300)));
    assert_eq!(update.output, DriveCommand::IDLE);
    assert_eq!(actuator.target(), DriveCommand::IDLE);
    assert_eq!(actuator.lease_deadline_ms(), None);
    assert_eq!(update.next_deadline_ms, None);
}

#[test]
fn recovery_between_steps_clears_active_motion_without_a_new_command() {
    let mut actuator = common::rested_actuator();
    let mut network = ready(0);
    step(&mut actuator, 0, network, Some(command(MOVE, 0)));
    network.observe(100, false);
    network.observe(101, true);
    let update = step(&mut actuator, 150, network, None);
    assert_eq!(update.output, DriveCommand::IDLE);
    assert_eq!(actuator.target(), DriveCommand::IDLE);
    assert_eq!(actuator.lease_deadline_ms(), None);
}

#[test]
fn recovery_between_steps_cancels_an_activation_due_in_the_new_generation() {
    let mut actuator = common::rested_actuator();
    step(&mut actuator, 0, ready(0), Some(command(MOVE, 0)));
    step(
        &mut actuator,
        100,
        ready(100),
        Some(command(Command::Inhibit, 100)),
    );
    let mut network = ready(1_900);
    step(&mut actuator, 1_900, network, Some(command(MOVE, 1_900)));
    assert_eq!(actuator.target(), DRIVE);
    assert_eq!(actuator.lease_deadline_ms(), Some(2_650));

    network.observe(1_901, false);
    network.observe(1_902, true);
    network.observe(2_100, true);
    let update = step(&mut actuator, 2_100, network, None);
    assert_eq!(update.output, DriveCommand::IDLE);
    assert_eq!(actuator.target(), DriveCommand::IDLE);
    assert_eq!(actuator.lease_deadline_ms(), None);
}

#[test]
fn an_arriving_release_cancels_a_same_instant_pending_activation() {
    for release in [Command::Inhibit, Command::Jog(DriveCommand::IDLE)] {
        let mut actuator = common::rested_actuator();
        step(&mut actuator, 0, ready(0), Some(command(MOVE, 0)));
        step(
            &mut actuator,
            100,
            ready(100),
            Some(command(Command::Inhibit, 100)),
        );
        step(
            &mut actuator,
            1_900,
            ready(1_900),
            Some(command(MOVE, 1_900)),
        );
        assert_eq!(actuator.target(), DRIVE);
        let update = step(
            &mut actuator,
            2_100,
            ready(2_100),
            Some(command(release, 2_100)),
        );
        assert_eq!(update.output, DriveCommand::IDLE);
        assert_eq!(actuator.target(), DriveCommand::IDLE);
        assert_eq!(actuator.lease_deadline_ms(), None);
        // No hidden activation occurred at 2100: it would have started a new
        // off-hold, preventing this fresh command from starting immediately.
        assert_eq!(
            step(
                &mut actuator,
                2_101,
                ready(2_101),
                Some(command(MOVE, 2_101)),
            )
            .output,
            DRIVE
        );
    }
}

#[test]
fn feedback_expires_independently_of_fresh_network_and_command_refreshes() {
    let mut actuator = common::rested_actuator();
    let interlock = feedback(0);
    for now_ms in [0, 150, 300, 450] {
        let update = actuator.step(
            ActuatorInput {
                interlock,
                network: ready(now_ms),
                command: Some(command(MOVE, now_ms)),
            },
            now_ms,
        );
        assert_eq!(update.output, DRIVE);
    }
    let update = actuator.step(
        ActuatorInput {
            interlock,
            network: ready(500),
            command: None,
        },
        500,
    );
    assert_eq!(update.output, DRIVE);
    assert_eq!(update.next_deadline_ms, Some(501));
    let update = actuator.step(
        ActuatorInput {
            interlock,
            network: ready(501),
            command: Some(command(MOVE, 501)),
        },
        501,
    );
    assert_eq!(update.output, DriveCommand::IDLE);
    assert_eq!(actuator.target(), DriveCommand::IDLE);
}

#[test]
fn delayed_delivery_never_renews_the_original_command_expiry() {
    let mut actuator = common::rested_actuator();
    let envelope = CommandEnvelope {
        command: MOVE,
        valid_until_ms: 700,
    };
    assert_eq!(
        step(&mut actuator, 200, ready(200), Some(envelope)).output,
        DRIVE
    );
    assert_eq!(actuator.lease_deadline_ms(), Some(700));
    assert_eq!(
        step(&mut actuator, 650, ready(650), Some(envelope)).output,
        DRIVE
    );
    assert_eq!(actuator.lease_deadline_ms(), Some(700));
    assert_eq!(
        step(&mut actuator, 700, ready(700), Some(envelope)).output,
        DriveCommand::IDLE
    );
    assert_eq!(actuator.target(), DriveCommand::IDLE);

    let mut too_late = common::rested_actuator();
    assert_eq!(
        step(&mut too_late, 201, ready(201), Some(envelope)).output,
        DriveCommand::IDLE
    );
}

#[test]
fn a_late_refresh_expires_the_previous_lease_before_receiving_a_new_one() {
    let mut actuator = common::rested_actuator();
    step(&mut actuator, 0, ready(0), Some(command(MOVE, 0)));
    let update = step(&mut actuator, 750, ready(750), Some(command(MOVE, 750)));
    assert_eq!(update.output, DriveCommand::IDLE);
    assert_eq!(actuator.target(), DRIVE);
    assert_eq!(actuator.lease_deadline_ms(), Some(1_500));
}

#[test]
fn network_loss_cannot_erase_elapsed_motion_or_its_latched_fault() {
    let mut actuator = Actuator::with_config(
        ActuatorConfig {
            motion: MotionConfig {
                progress_timeout_ms: 1_000,
                progress_codes: 3,
                noise_codes: 2,
            },
            ..ActuatorConfig::DEFAULT
        },
        DutyHistory::Known([0, 0]),
    );
    for now_ms in (0..1_000).step_by(100) {
        let update = step(
            &mut actuator,
            now_ms,
            ready(now_ms),
            Some(command(MOVE, now_ms)),
        );
        assert_eq!(update.output, DRIVE);
        assert_eq!(update.status.motion_fault, None);
    }
    let update = step(&mut actuator, 1_000, NetworkGate::new(), None);
    assert_eq!(update.output, DriveCommand::IDLE);
    assert_eq!(
        update.status.motion_fault,
        Some(MotionFault::AzimuthNoProgress)
    );
    let recovered = step(
        &mut actuator,
        1_100,
        ready(1_100),
        Some(command(MOVE, 1_100)),
    );
    assert_eq!(recovered.output, DriveCommand::IDLE);
    assert_eq!(recovered.status, update.status);
}

fn coordinated() -> (CoordinationState, Actuator, u32) {
    let mut state = CoordinationState::new();
    state.publish_network(true, 0);
    let owner = state.register_client(0);
    let mut monitor = FeedbackMonitor::new(TEST_CALIBRATION, None);
    let voltages = TEST_CALIBRATION
        .position_to_feedback(Pointing::new(180.0, 45.0))
        .unwrap();
    let update = monitor.update(0, 0, Ok(voltages));
    state.publish_feedback(update.snapshot, update.interlock);
    (state, common::rested_actuator(), owner)
}

fn consume(state: &mut CoordinationState, actuator: &mut Actuator, now_ms: u64) -> ActuatorUpdate {
    let update = actuator.step(state.take_actuator_input(now_ms), now_ms);
    state.record_actuator(update.output, update.status);
    update
}

#[test]
fn ownership_and_network_changes_preserve_duty_and_publish_it_to_every_viewer() {
    let (mut state, mut actuator, owner) = coordinated();
    assert!(state.accept_command(owner, command(MOVE, 0), 0));
    consume(&mut state, &mut actuator, 0);
    let viewer = state.register_client(100);
    assert!(state.take_control(viewer, 100));
    let stopped = consume(&mut state, &mut actuator, 100);
    assert_eq!(stopped.output, DriveCommand::IDLE);
    assert_eq!(stopped.status.duty.azimuth.remaining_ms, 179_900);
    assert_eq!(
        state.telemetry(owner, 100).control.duty,
        stopped.status.duty
    );
    assert_eq!(
        state.telemetry(viewer, 100).control.duty,
        stopped.status.duty
    );
    state.publish_network(false, 150);
    consume(&mut state, &mut actuator, 150);
    state.publish_network(true, 200);
    assert!(state.take_control(owner, 200));
    let recovered = consume(&mut state, &mut actuator, 200);
    assert_eq!(recovered.status.duty, stopped.status.duty);
    assert_eq!(recovered.output, DriveCommand::IDLE);
}

#[test]
fn coalesced_submissions_deliver_inhibition_before_another_jog_can_resume() {
    let (mut state, mut actuator, owner) = coordinated();
    assert!(state.accept_command(owner, command(MOVE, 0), 0));
    assert_eq!(consume(&mut state, &mut actuator, 0).output, DRIVE);

    assert!(state.accept_command(owner, CommandEnvelope::INHIBIT, 100));
    assert!(state.accept_command(owner, command(MOVE, 101), 101));
    assert!(state.accept_command(owner, command(MOVE, 102), 102));
    let update = consume(&mut state, &mut actuator, 150);
    assert_eq!(update.output, DriveCommand::IDLE);
    assert_eq!(actuator.target(), DriveCommand::IDLE);
    assert_eq!(actuator.lease_deadline_ms(), None);
    assert_eq!(
        state.telemetry(owner, 150).control.drive,
        DriveCommand::IDLE
    );

    assert_eq!(
        consume(&mut state, &mut actuator, 151).output,
        DriveCommand::IDLE
    );
    assert_eq!(actuator.target(), DriveCommand::IDLE);
    // Only a request submitted after consumption can become pending again.
    assert!(state.accept_command(owner, command(MOVE, 152), 152));
    assert_eq!(
        consume(&mut state, &mut actuator, 152).output,
        DriveCommand::IDLE
    );
    assert_eq!(actuator.target(), DRIVE);
    assert_eq!(actuator.lease_deadline_ms(), Some(902));
}

#[test]
fn coordinated_outage_and_recovery_supersede_queued_movement() {
    let (mut state, mut actuator, owner) = coordinated();
    assert!(state.accept_command(owner, command(MOVE, 0), 0));
    assert_eq!(consume(&mut state, &mut actuator, 0).output, DRIVE);
    assert!(state.accept_command(owner, command(MOVE, 100), 100));

    assert!(state.publish_network(false, 101));
    state.publish_network(true, 102);
    assert!(!state.accept_command(owner, command(MOVE, 103), 103));
    assert!(state.take_control(owner, 104));
    assert!(state.accept_command(owner, command(MOVE, 105), 105));
    // All of those notifications may coalesce into one wake. The complete
    // production path must still consume the outage's inhibition first.
    let update = consume(&mut state, &mut actuator, 150);
    assert_eq!(update.output, DriveCommand::IDLE);
    assert_eq!(actuator.target(), DriveCommand::IDLE);
    assert_eq!(actuator.lease_deadline_ms(), None);
    assert_eq!(
        consume(&mut state, &mut actuator, 200).output,
        DriveCommand::IDLE
    );
    assert_eq!(actuator.target(), DriveCommand::IDLE);
}
