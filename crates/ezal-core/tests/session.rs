//! Session freshness and operator-intent tests.
use ezal_core::drive::{AzimuthDirection, DriveCommand};
use ezal_core::protocol::ClientMessage;
use ezal_core::session::{CommandFreshness, MovementRequest};

fn movement(token: u64, sequence: u64, pressed: bool) -> MovementRequest {
    MovementRequest {
        token,
        sequence,
        pressed,
        drive: DriveCommand {
            azimuth: Some(AzimuthDirection::Clockwise),
            elevation: None,
        },
    }
}

#[test]
fn command_age_is_bounded_by_observation_even_when_received_late() {
    let mut freshness = CommandFreshness::new(1);
    let token = freshness.issue(0).unwrap();
    // A late command does not receive 750 new milliseconds. This deadline is
    // carried through the mailbox and relay scheduler by the firmware.
    assert_eq!(
        freshness.admit(movement(token.token, 1, true), 900),
        Some(1000)
    );
    assert_eq!(freshness.admit(movement(token.token, 2, false), 1000), None);
    let next = freshness.issue(1000).unwrap();
    // A resumed timer cannot revive the rejected operator hold.
    assert_eq!(freshness.admit(movement(next.token, 3, false), 1001), None);
    let next = freshness.issue(1500).unwrap();
    assert_eq!(
        freshness.admit(movement(next.token, 4, true), 1501),
        Some(2251)
    );
}

#[test]
fn replacement_tokens_do_not_renew_queued_commands_or_allow_cross_session_replay() {
    let mut freshness = CommandFreshness::new(1);
    let old = freshness.issue(0).unwrap();
    let new = freshness.issue(500).unwrap();
    assert_eq!(
        freshness.admit(movement(old.token, 1, true), 500),
        Some(1000)
    );
    assert_eq!(
        freshness.admit(movement(new.token, 2, false), 700),
        Some(1450)
    );
    // The old telemetry generation expires even though its replacement lives.
    assert_eq!(freshness.admit(movement(old.token, 3, false), 1000), None);
    // Rejecting a stale command invalidates all already issued tokens, so
    // queued presses cannot undo the stop before a newer telemetry batch.
    assert_eq!(freshness.admit(movement(new.token, 4, true), 1001), None);
    let mut other_session = CommandFreshness::new(2);
    other_session.issue(0);
    assert_eq!(other_session.admit(movement(old.token, 1, true), 1), None);
}

#[test]
fn replay_out_of_order_and_expired_holds_require_new_operator_input() {
    for repeated_sequence in [1, 2] {
        let mut freshness = CommandFreshness::new(4);
        let token = freshness.issue(0).unwrap();
        assert_eq!(
            freshness.admit(movement(token.token, 2, true), 0),
            Some(750)
        );
        assert_eq!(
            freshness.admit(movement(token.token, repeated_sequence, false), 10),
            None
        );
    }
    let mut freshness = CommandFreshness::new(4);
    let first = freshness.issue(0).unwrap();
    assert_eq!(
        freshness.admit(movement(first.token, 1, true), 0),
        Some(750)
    );
    let second = freshness.issue(500).unwrap();
    // Token two remains valid, but the *operator hold* already lost its lease.
    assert_eq!(freshness.admit(movement(second.token, 2, false), 750), None);
    let third = freshness.issue(1000).unwrap();
    assert_eq!(
        freshness.admit(movement(third.token, 3, true), 1000),
        Some(1750)
    );
    freshness.disarm();
    assert_eq!(freshness.admit(movement(third.token, 4, false), 1001), None);
}

#[test]
fn command_envelopes_require_complete_ordered_movement_fields() {
    for invalid in [
        "press:1:1:stop",
        "hold:1:1:stop",
        "drive:cw",
        "press::1:drive:cw",
        "press:1::drive:cw",
        "press:1:1:drive:cw:extra",
        "press:1:1:drive:cw+ccw",
        "hold:18446744073709551616:1:drive:cw",
        "press:1:18446744073709551616:drive:cw",
    ] {
        assert_eq!(ClientMessage::parse(invalid), None, "{invalid}");
    }
    assert!(matches!(
        ClientMessage::parse("hold:1:2:drive:ccw+up"),
        Some(ClientMessage::Movement(MovementRequest {
            pressed: false,
            ..
        }))
    ));
    let mut freshness = CommandFreshness::new(3);
    let token = freshness.issue(0).unwrap();
    let mut json = String::new();
    token.write_json(&mut json).unwrap();
    assert_eq!(json, r#"{"token":"12884901889"}"#);
}

#[test]
fn network_recovery_invalidates_even_unexpired_queued_presses() {
    let mut freshness = CommandFreshness::new(1);
    freshness.observe_network(1, true);
    let old = freshness.issue(0).unwrap();
    assert_eq!(freshness.admit(movement(old.token, 1, true), 0), Some(750));
    // A brief outage can recover entirely between two WebSocket receives.
    // Its new generation must still invalidate the original token.
    freshness.observe_network(2, true);
    assert_eq!(freshness.admit(movement(old.token, 2, true), 100), None);
    let new = freshness.issue(100).unwrap();
    assert_eq!(
        freshness.admit(movement(new.token, 3, true), 100),
        Some(850)
    );
    freshness.observe_network(2, false);
    assert_eq!(freshness.admit(movement(new.token, 4, true), 110), None);
}
