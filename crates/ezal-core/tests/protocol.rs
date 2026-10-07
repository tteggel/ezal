//! Tests for the browser-facing wire protocol.

use ezal_core::control::ControlState;
use ezal_core::dashboard::INDEX_HTML;
use ezal_core::drive::{
    AzimuthDirection, Command, DriveCommand, ElevationDirection, OUTPUT_MIN_ACTIVE_MS,
};
use ezal_core::protocol::{
    parse_command, ClientMessage, CommandFreshness, ControlStatus, MovementRequest,
    PositionTelemetry, TrackingMode, TrackingTelemetry, COMMAND_MAX_AGE_MS, COMMAND_REFRESH_MS,
    COMMAND_TIMEOUT_MS, TAKE_CONTROL_MESSAGE,
};
use ezal_core::simulation::PassPhase;

#[test]
fn parses_direction_commands() {
    assert_eq!(
        parse_command("drive:cw"),
        Some(Command::Drive(DriveCommand {
            azimuth: Some(AzimuthDirection::Clockwise),
            elevation: None,
        }))
    );
    assert_eq!(
        parse_command("drive:ccw"),
        Some(Command::Drive(DriveCommand {
            azimuth: Some(AzimuthDirection::CounterClockwise),
            elevation: None,
        }))
    );
    assert_eq!(
        parse_command("drive:up"),
        Some(Command::Drive(DriveCommand {
            azimuth: None,
            elevation: Some(ElevationDirection::Up),
        }))
    );
    assert_eq!(
        parse_command("drive:down"),
        Some(Command::Drive(DriveCommand {
            azimuth: None,
            elevation: Some(ElevationDirection::Down),
        }))
    );
    assert_eq!(parse_command("stop"), Some(Command::Stop));
}

#[test]
fn parses_client_messages() {
    assert_eq!(
        ClientMessage::parse(TAKE_CONTROL_MESSAGE),
        Some(ClientMessage::TakeControl)
    );
    assert_eq!(
        ClientMessage::parse("press:4294967297:1:drive:cw"),
        Some(ClientMessage::Movement(MovementRequest {
            token: 4294967297,
            sequence: 1,
            pressed: true,
            drive: DriveCommand {
                azimuth: Some(AzimuthDirection::Clockwise),
                elevation: None,
            },
        }))
    );
    assert_eq!(ClientMessage::parse("drive:cw"), None);
    assert_eq!(
        ClientMessage::parse("stop"),
        Some(ClientMessage::Command(Command::Stop))
    );
}

#[test]
fn parses_simultaneous_axis_commands() {
    assert_eq!(
        parse_command("drive:cw+up"),
        Some(Command::Drive(DriveCommand {
            azimuth: Some(AzimuthDirection::Clockwise),
            elevation: Some(ElevationDirection::Up),
        }))
    );
    assert_eq!(
        parse_command("drive:down+ccw"),
        Some(Command::Drive(DriveCommand {
            azimuth: Some(AzimuthDirection::CounterClockwise),
            elevation: Some(ElevationDirection::Down),
        }))
    );
}

#[test]
fn rejects_unknown_commands() {
    assert_eq!(parse_command(""), None);
    assert_eq!(parse_command("cw"), None);
    assert_eq!(parse_command("drive:left"), None);
    assert_eq!(parse_command("drive:"), None);
    assert_eq!(parse_command("stop\n"), None);
}

#[test]
fn rejects_conflicting_axis_commands() {
    assert_eq!(parse_command("drive:cw+ccw"), None);
    assert_eq!(parse_command("drive:ccw+cw"), None);
    assert_eq!(parse_command("drive:up+down"), None);
    assert_eq!(parse_command("drive:down+up"), None);
    assert_eq!(parse_command("drive:cw+up+down"), None);
    assert_eq!(parse_command("drive:cw+cw"), None);
}

#[test]
fn formats_position_telemetry_as_compact_json() {
    let mut json = String::new();
    PositionTelemetry {
        a0_mv: 901,
        a1_mv: -12,
    }
    .write_json(&mut json)
    .unwrap();

    assert_eq!(json, r#"{"type":"position","a0_mv":901,"a1_mv":-12}"#);
}

#[test]
fn formats_control_status_as_compact_json() {
    let mut json = String::new();
    ControlStatus {
        controller: true,
        drive: DriveCommand {
            azimuth: Some(AzimuthDirection::CounterClockwise),
            elevation: Some(ElevationDirection::Up),
        },
        network_ready: true,
        motion_fault: None,
    }
    .write_json(&mut json)
    .unwrap();

    assert_eq!(
        json,
        r#"{"type":"control","controller":true,"azimuth":"ccw","elevation":"up","network_ready":true,"motion_fault":null}"#
    );

    json.clear();
    ControlStatus {
        controller: false,
        drive: DriveCommand::IDLE,
        network_ready: false,
        motion_fault: None,
    }
    .write_json(&mut json)
    .unwrap();

    assert_eq!(
        json,
        r#"{"type":"control","controller":false,"azimuth":null,"elevation":null,"network_ready":false,"motion_fault":null}"#
    );
}

#[test]
fn formats_tracking_telemetry_with_nullable_pause_targets() {
    let mut json = String::new();
    TrackingTelemetry {
        mode: TrackingMode::HardwareWalkingSkeleton,
        pass_index: 1,
        phase: PassPhase::Pause,
        phase_remaining_ms: 29_500,
        azimuth_tenths: 4_247,
        elevation_tenths: 3,
        target_azimuth_tenths: None,
        target_elevation_tenths: None,
        control_state: ControlState::Idle,
    }
    .write_json(&mut json)
    .unwrap();

    assert_eq!(
        json,
        r#"{"type":"tracking","mode":"hardware-walking-skeleton","satellite":"METOP-C","phase":"pause","pass":2,"remaining_ms":29500,"azimuth_tenths":4247,"elevation_tenths":3,"target_azimuth_tenths":null,"target_elevation_tenths":null,"control_state":"idle"}"#
    );
}

#[test]
fn formats_the_largest_pass_index_without_overflow() {
    let mut json = String::new();
    TrackingTelemetry {
        pass_index: u32::MAX,
        ..TrackingTelemetry::ZERO
    }
    .write_json(&mut json)
    .unwrap();

    assert!(json.contains(r#""pass":4294967296,"#));
}

#[test]
fn command_refresh_stays_inside_firmware_lease() {
    const {
        assert!(COMMAND_REFRESH_MS < COMMAND_TIMEOUT_MS);
        assert!(OUTPUT_MIN_ACTIVE_MS <= COMMAND_TIMEOUT_MS);
    }
    assert!(INDEX_HTML.contains("const refreshMs = 150;"));
    assert_eq!(COMMAND_MAX_AGE_MS, 1000);
    assert!(INDEX_HTML.contains("const telemetryTimeoutMs = 1000;"));
}

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
    assert_eq!(json, r#"{"type":"freshness","token":"12884901889"}"#);
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
