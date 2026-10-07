use ezal_core::coordination::CoordinationState;
use ezal_core::drive::{AzimuthDirection, Command, DriveCommand};
use ezal_core::mailbox::CommandEnvelope;

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
    let viewer = state.register_client(0);
    assert!(!state.accept_command(viewer, MOVE, 0));
    assert!(state.accept_command(owner, MOVE, 0));
    assert_eq!(state.take_actuator_input(200).command, Some(MOVE));
    assert_eq!(state.take_actuator_input(200).command, None);
}

#[test]
fn takeover_inhibition_survives_new_owner_refreshes_and_old_socket_close() {
    let mut state = CoordinationState::new();
    state.publish_network(true, 0);
    let old = state.register_client(0);
    let new = state.register_client(0);
    state.accept_command(old, MOVE, 0);
    assert!(state.take_control(new, 10));
    assert!(!state.release_client(old));
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
    let viewer = state.register_client(0);
    state.accept_command(owner, MOVE, 0);
    assert!(!state.release_client(viewer));
    assert!(state.release_client(owner));
    assert!(!state.telemetry(viewer, 10).control.controller);
    assert!(!state.accept_command(viewer, MOVE, 10));
    assert_eq!(
        state.take_actuator_input(10).command,
        Some(CommandEnvelope::INHIBIT)
    );
    assert!(state.take_control(viewer, 10));
}
