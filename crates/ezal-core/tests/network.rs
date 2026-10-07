//! Network outages must cut applied and pending motion independently of
//! command/feedback freshness, and recovery must not replay a previous request.

mod common;

use common::TEST_CALIBRATION;
use ezal_core::actuator::Actuator;
use ezal_core::control::TimedPointing;
use ezal_core::drive::{AzimuthDirection, Command, DriveCommand};
use ezal_core::interlock::FeedbackInterlock;
use ezal_core::network::{NetworkGate, NETWORK_HEALTH_TIMEOUT_MS};
use ezal_core::position::Pointing;

const MOVE: Command = Command::Drive(DriveCommand {
    azimuth: Some(AzimuthDirection::Clockwise),
    elevation: None,
});

fn feedback(now_ms: u64) -> FeedbackInterlock {
    let mut interlock = FeedbackInterlock::new(TEST_CALIBRATION);
    interlock.publish(Some(TimedPointing::new(Pointing::new(100.0, 40.0), now_ms)));
    interlock
}

#[test]
fn boot_is_inhibited_and_positive_observations_have_exact_deadlines() {
    let mut network = NetworkGate::new();
    assert!(!network.ready(0));
    assert_eq!(network.guard_command(MOVE, 0), Command::Inhibit);
    assert_eq!(network.deadline_ms(), None);
    assert!(!network.observe(100, true));
    assert_eq!(network.generation(), 1);
    assert_eq!(network.deadline_ms(), Some(100 + NETWORK_HEALTH_TIMEOUT_MS));
    assert!(network.ready(100 + NETWORK_HEALTH_TIMEOUT_MS - 1));
    assert!(!network.ready(100 + NETWORK_HEALTH_TIMEOUT_MS));
    assert!(!network.ready(99));
    assert!(network.expire(100 + NETWORK_HEALTH_TIMEOUT_MS));
    assert!(!network.expire(100 + NETWORK_HEALTH_TIMEOUT_MS));
}

#[test]
fn stalled_network_supervision_cuts_fresh_feedback_and_command_streams() {
    let mut network = NetworkGate::new();
    let mut actuator = Actuator::new();
    network.observe(0, true);
    for now_ms in (0..=NETWORK_HEALTH_TIMEOUT_MS).step_by(50) {
        let interlock = feedback(now_ms);
        actuator.command(network.guard_command(MOVE, now_ms), &interlock, now_ms);
        actuator.service(&interlock, now_ms);
        assert_eq!(
            actuator.actual().is_active(),
            now_ms < NETWORK_HEALTH_TIMEOUT_MS
        );
    }
    assert_eq!(actuator.target(), DriveCommand::IDLE);
    assert_eq!(actuator.lease_deadline_ms(), None);
}

#[test]
fn loss_clears_motion_and_recovery_waits_for_a_new_command() {
    let mut network = NetworkGate::new();
    let mut actuator = Actuator::new();
    network.observe(0, true);
    actuator.command(network.guard_command(MOVE, 0), &feedback(0), 0);
    assert!(actuator.actual().is_active());
    assert!(network.observe(100, false));
    actuator.command(network.guard_command(MOVE, 100), &feedback(100), 100);
    assert_eq!(actuator.actual(), DriveCommand::IDLE);
    assert_eq!(actuator.target(), DriveCommand::IDLE);
    network.observe(200, true);
    assert_eq!(network.generation(), 2);
    actuator.service(&feedback(200), 200);
    assert_eq!(actuator.actual(), DriveCommand::IDLE);
    // Network recovery alone does not recreate a movement request.
    actuator.service(&feedback(1_000), 1_000);
    assert_eq!(actuator.actual(), DriveCommand::IDLE);
}

#[test]
fn network_loss_cancels_a_relay_transition_waiting_to_activate() {
    let mut network = NetworkGate::new();
    let mut actuator = Actuator::new();
    network.observe(0, true);
    actuator.command(MOVE, &feedback(0), 0);
    actuator.command(Command::Inhibit, &feedback(100), 100);
    actuator.command(network.guard_command(MOVE, 101), &feedback(101), 101);
    assert_eq!(actuator.actual(), DriveCommand::IDLE);
    assert!(actuator.target().is_active());

    network.observe(102, false);
    actuator.command(network.guard_command(MOVE, 102), &feedback(102), 102);
    assert_eq!(actuator.target(), DriveCommand::IDLE);
    network.observe(600, true);
    actuator.service(&feedback(600), 600);
    assert_eq!(actuator.actual(), DriveCommand::IDLE);
    assert_eq!(actuator.lease_deadline_ms(), None);
}

#[test]
fn late_positive_observation_reports_outage_before_recovering() {
    let mut network = NetworkGate::new();
    network.observe(0, true);
    assert!(!network.observe(100, true));
    assert_eq!(network.generation(), 1);
    assert!(network.observe(100 + NETWORK_HEALTH_TIMEOUT_MS, true));
    assert_eq!(network.generation(), 2);
    assert!(network.ready(100 + NETWORK_HEALTH_TIMEOUT_MS));
}

#[test]
fn unrepresentable_deadline_cannot_grant_permission() {
    let mut network = NetworkGate::new();
    network.observe(u64::MAX, true);
    assert!(!network.ready(u64::MAX));
    assert_eq!(network.deadline_ms(), None);
}
