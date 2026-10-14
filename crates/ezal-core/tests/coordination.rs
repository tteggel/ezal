use ezal_core::coordination::CoordinationState;
use ezal_core::drive::{AzimuthDirection, Command, CommandEnvelope, DriveCommand};
use ezal_core::operator::{OperatorGrant, OPERATOR_LEASE_MS};

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
