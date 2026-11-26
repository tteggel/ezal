mod common;

use ezal_core::actuator::Actuator;
use ezal_core::coordination::CoordinationState;
use ezal_core::drive::{AzimuthDirection, Command, CommandEnvelope, DriveCommand};
use ezal_core::feedback::{FeedbackHealth, FeedbackSample, FeedbackSnapshot};
use ezal_core::interlock::FeedbackInterlock;
use ezal_core::operator::{OperatorGrant, OPERATOR_LEASE_MS};
use ezal_core::position::{FeedbackVoltages, Pointing};

const MOVE: CommandEnvelope = CommandEnvelope {
    command: Command::Jog(DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: None,
    }),
    valid_until_ms: 750,
};

#[test]
fn admission_and_delivery_preserve_source_expiry() {
    let mut state = CoordinationState::new();
    state.publish_network(true, 0);
    let owner = state.register_client(0);
    assert_eq!(
        state.take_actuator_input(0).command,
        Some(CommandEnvelope::INHIBIT)
    );
    let viewer = state.register_client(0);
    assert!(!state.accept_command(viewer, MOVE, 0));
    assert!(state.accept_command(owner, MOVE, 0));
    assert_eq!(state.take_actuator_input(200).command, Some(MOVE));
    assert_eq!(state.take_actuator_input(200).command, None);
}

#[test]
fn exclusive_lease_and_release_preserve_stop_before_new_owner_movement() {
    let mut state = CoordinationState::new();
    state.publish_network(true, 0);
    let old = state.register_client(0);
    let new = state.register_client(0);
    state.accept_command(old, MOVE, 0);
    assert!(!state.take_control(new, 10));
    assert!(state.release_client(old, 10));
    assert!(state.take_control(new, 10));
    assert!(!state.release_client(old, 10));
    assert!(!state.accept_command(old, MOVE, 10));
    assert!(state.accept_command(new, MOVE, 10));
    assert_eq!(
        state.take_actuator_input(10).command,
        Some(CommandEnvelope::INHIBIT)
    );
    assert!(state.accept_command(new, MOVE, 11));
    assert_eq!(state.take_actuator_input(11).command, Some(MOVE));
}

#[test]
fn reads_do_not_expire_state_but_actuator_drain_does() {
    let mut state = CoordinationState::new();
    state.publish_network(true, 0);
    let owner = state.register_client(0);
    assert_eq!(
        state.take_actuator_input(0).command,
        Some(CommandEnvelope::INHIBIT)
    );
    state.accept_command(owner, MOVE, 0);
    assert!(!state.telemetry(owner, 300).control.controller);
    assert!(!state.telemetry(owner, 300).control.network_ready);
    // A read did not mutate stored permission or ownership.
    assert_eq!(state.network().deadline_ms(), Some(300));
    assert!(state.telemetry(owner, 299).control.controller);
    let input = state.take_actuator_input(300);
    assert_eq!(input.command, Some(CommandEnvelope::INHIBIT));
    assert_eq!(input.network.deadline_ms(), None);
}

#[test]
fn recovery_requires_takeover_and_preserves_an_unconsumed_stop() {
    let mut state = CoordinationState::new();
    state.publish_network(true, 0);
    let owner = state.register_client(0);
    assert_eq!(
        state.take_actuator_input(0).command,
        Some(CommandEnvelope::INHIBIT)
    );
    state.accept_command(owner, MOVE, 0);
    assert!(state.publish_network(true, 300));
    assert_eq!(state.network().generation(), 2);
    assert!(!state.accept_command(owner, MOVE, 300));
    assert!(state.take_control(owner, 300));
    assert!(state.accept_command(owner, MOVE, 300));
    assert_eq!(
        state.take_actuator_input(300).command,
        Some(CommandEnvelope::INHIBIT)
    );
}

#[test]
fn boot_offline_cannot_grant_control() {
    let mut state = CoordinationState::new();
    let client = state.register_client(0);
    assert!(!state.take_control(client, 0));
    assert!(!state.accept_command(client, MOVE, 0));
    state.publish_network(true, 1);
    assert!(!state.telemetry(client, 1).control.controller);
    assert!(state.take_control(client, 1));
}

#[test]
fn owner_disconnect_stops_without_promoting_a_viewer() {
    let mut state = CoordinationState::new();
    state.publish_network(true, 0);
    let owner = state.register_client(0);
    assert_eq!(
        state.take_actuator_input(0).command,
        Some(CommandEnvelope::INHIBIT)
    );
    let viewer = state.register_client(0);
    state.accept_command(owner, MOVE, 0);
    assert!(!state.release_client(viewer, 0));
    assert!(state.release_client(owner, 0));
    assert!(!state.telemetry(viewer, 10).control.controller);
    assert!(!state.accept_command(viewer, MOVE, 10));
    assert_eq!(
        state.take_actuator_input(10).command,
        Some(CommandEnvelope::INHIBIT)
    );
    assert!(state.take_control(viewer, 10));
}

#[test]
fn readers_and_repeated_claims_do_not_renew_an_operator_lease() {
    let mut state = CoordinationState::new();
    state.publish_network(true, 0);
    let owner = state.register_client(0);
    let viewer = state.register_client(0);
    let token = state.operator(owner, 0).unwrap().token;
    for now in (100..=5000).step_by(100) {
        state.publish_network(true, now);
        for _ in 0..8 {
            state.telemetry(viewer, now);
        }
        if now < 5000 {
            assert!(state.take_control(owner, now));
        }
    }
    assert!(state.operator(owner, 5000).is_none());
    assert!(!state.renew_operator(token, 5000));
    assert!(state.take_control(viewer, 5000));
    assert!(!state.release_client(owner, 5000));
}

#[test]
fn renewal_extends_the_same_grant_only_while_the_network_is_current() {
    let mut state = CoordinationState::new();
    state.publish_network(true, 0);
    let owner = state.register_client(0);
    let token = state.operator(owner, 0).unwrap().token;
    state.publish_network(true, 200);
    assert!(state.renew_operator(token, 400));
    assert_eq!(
        state.operator(owner, 400),
        Some(OperatorGrant {
            token,
            expires_at_ms: 400 + OPERATOR_LEASE_MS,
        })
    );
    // The observation from 200 expires at 500 and revokes the renewed lease with it.
    assert!(!state.renew_operator(token, 700));
    assert!(state.operator(owner, 700).is_none());
}

/// One resting acquisition in firmware order: publish, service, record.
fn rest_at(state: &mut CoordinationState, actuator: &mut Actuator, mv: FeedbackVoltages, now: u64) {
    let sample = FeedbackSample::from_voltages(common::installed(), mv, now).unwrap();
    let mut interlock = FeedbackInterlock::new(common::installed());
    interlock.publish(Some(sample));
    state.publish_network(true, now);
    state.publish_feedback(
        FeedbackSnapshot {
            raw: None,
            last_valid: Some(sample),
            health: FeedbackHealth::Healthy,
        },
        interlock,
        now,
    );
    state.service_actuator(actuator, now);
    state.record_actuator(actuator, now);
}

/// Five-code feedback noise fits with headroom; larger motion is measured
/// from the fixed planning start, independently of endpoint rounding margins.
#[test]
fn idle_feedback_noise_keeps_the_planning_start_and_drift_from_it_replaces_it() {
    let mut state = CoordinationState::new();
    let mut actuator = common::rested_actuator();
    let rest = common::TEST_CALIBRATION
        .position_to_feedback(Pointing::new(180.0, 45.0))
        .unwrap();
    let offset = |codes: i32| FeedbackVoltages {
        a0_elevation_mv: rest.a0_elevation_mv + 2 * codes,
        a1_azimuth_mv: rest.a1_azimuth_mv - 2 * codes,
    };
    rest_at(&mut state, &mut actuator, rest, 0);
    rest_at(&mut state, &mut actuator, rest, 2000);
    let start = state.start_revision();
    for (now, codes) in (2100..4000).step_by(100).zip([5, -5].into_iter().cycle()) {
        rest_at(&mut state, &mut actuator, offset(codes), now);
    }
    assert_eq!(state.start_revision(), start);
    // Creeping a code at a time is measured from the start, not the last sample.
    for (now, codes) in [(4000, 2), (4100, 3), (4200, 4)] {
        rest_at(&mut state, &mut actuator, offset(codes), now);
    }
    assert_eq!(state.start_revision(), start);
    rest_at(&mut state, &mut actuator, offset(7), 4300);
    let moved = state.start_revision();
    assert!(moved > start);
    // The moved pose is the new start.
    rest_at(&mut state, &mut actuator, offset(3), 4400);
    assert_eq!(state.start_revision(), moved);
}
