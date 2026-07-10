//! Tests for the browser-facing wire protocol.

use ezal_core::web::{
    AzimuthDirection, ClientMessage, Command, ControlStatus, DriveCommand, DriveDebouncer,
    ElevationDirection, PositionTelemetry, COMMAND_REFRESH_MS, COMMAND_TIMEOUT_MS, INDEX_HTML,
    OUTPUT_MIN_ACTIVE_MS, OUTPUT_MIN_INACTIVE_MS, TAKE_CONTROL_MESSAGE, WS_PATH,
};

#[test]
fn parses_direction_commands() {
    assert_eq!(
        Command::parse("drive:cw"),
        Some(Command::Drive(DriveCommand {
            azimuth: Some(AzimuthDirection::Clockwise),
            elevation: None,
        }))
    );
    assert_eq!(
        Command::parse("drive:ccw"),
        Some(Command::Drive(DriveCommand {
            azimuth: Some(AzimuthDirection::CounterClockwise),
            elevation: None,
        }))
    );
    assert_eq!(
        Command::parse("drive:up"),
        Some(Command::Drive(DriveCommand {
            azimuth: None,
            elevation: Some(ElevationDirection::Up),
        }))
    );
    assert_eq!(
        Command::parse("drive:down"),
        Some(Command::Drive(DriveCommand {
            azimuth: None,
            elevation: Some(ElevationDirection::Down),
        }))
    );
    assert_eq!(Command::parse("stop"), Some(Command::Stop));
}

#[test]
fn parses_client_messages() {
    assert_eq!(
        ClientMessage::parse(TAKE_CONTROL_MESSAGE),
        Some(ClientMessage::TakeControl)
    );
    assert_eq!(
        ClientMessage::parse("drive:cw"),
        Some(ClientMessage::Command(Command::Drive(DriveCommand {
            azimuth: Some(AzimuthDirection::Clockwise),
            elevation: None,
        })))
    );
}

#[test]
fn parses_simultaneous_axis_commands() {
    assert_eq!(
        Command::parse("drive:cw+up"),
        Some(Command::Drive(DriveCommand {
            azimuth: Some(AzimuthDirection::Clockwise),
            elevation: Some(ElevationDirection::Up),
        }))
    );
    assert_eq!(
        Command::parse("drive:down+ccw"),
        Some(Command::Drive(DriveCommand {
            azimuth: Some(AzimuthDirection::CounterClockwise),
            elevation: Some(ElevationDirection::Down),
        }))
    );
}

#[test]
fn rejects_unknown_commands() {
    assert_eq!(Command::parse(""), None);
    assert_eq!(Command::parse("cw"), None);
    assert_eq!(Command::parse("drive:left"), None);
    assert_eq!(Command::parse("drive:"), None);
    assert_eq!(Command::parse("stop\n"), None);
}

#[test]
fn rejects_conflicting_axis_commands() {
    assert_eq!(Command::parse("drive:cw+ccw"), None);
    assert_eq!(Command::parse("drive:ccw+cw"), None);
    assert_eq!(Command::parse("drive:up+down"), None);
    assert_eq!(Command::parse("drive:down+up"), None);
    assert_eq!(Command::parse("drive:cw+up+down"), None);
    assert_eq!(Command::parse("drive:cw+cw"), None);
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
    }
    .write_json(&mut json)
    .unwrap();

    assert_eq!(
        json,
        r#"{"type":"control","controller":true,"azimuth":"ccw","elevation":"up"}"#
    );

    json.clear();
    ControlStatus {
        controller: false,
        drive: DriveCommand::IDLE,
    }
    .write_json(&mut json)
    .unwrap();

    assert_eq!(
        json,
        r#"{"type":"control","controller":false,"azimuth":null,"elevation":null}"#
    );
}

#[test]
fn drive_debouncer_holds_active_outputs_for_the_minimum_time() {
    let mut debouncer = DriveDebouncer::new();
    let cw = DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: None,
    };

    assert!(debouncer.set_target(Command::Drive(cw), 10));
    assert_eq!(debouncer.actual(), cw);
    assert_eq!(debouncer.target(), cw);

    assert!(!debouncer.set_target(Command::Stop, 100));
    assert_eq!(debouncer.actual(), cw);
    assert_eq!(debouncer.next_transition_ms(100), Some(510));

    assert!(!debouncer.update(509));
    assert_eq!(debouncer.actual(), cw);

    assert!(debouncer.update(510));
    assert_eq!(debouncer.actual(), DriveCommand::IDLE);
    assert_eq!(debouncer.next_transition_ms(510), None);
}

#[test]
fn drive_debouncer_delays_reversals_without_conflicting_outputs() {
    let mut debouncer = DriveDebouncer::new();
    let cw = DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: None,
    };
    let ccw = DriveCommand {
        azimuth: Some(AzimuthDirection::CounterClockwise),
        elevation: None,
    };

    assert!(debouncer.set_target(Command::Drive(cw), 0));
    assert!(!debouncer.set_target(Command::Drive(ccw), 125));
    assert_eq!(debouncer.target(), ccw);
    assert_eq!(debouncer.actual(), cw);
    assert_eq!(
        debouncer.next_transition_ms(125),
        Some(OUTPUT_MIN_ACTIVE_MS)
    );

    assert!(debouncer.update(OUTPUT_MIN_ACTIVE_MS));
    assert_eq!(debouncer.actual(), DriveCommand::IDLE);
    assert_eq!(
        debouncer.next_transition_ms(OUTPUT_MIN_ACTIVE_MS),
        Some(OUTPUT_MIN_ACTIVE_MS + OUTPUT_MIN_INACTIVE_MS)
    );

    assert!(!debouncer.update(OUTPUT_MIN_ACTIVE_MS + OUTPUT_MIN_INACTIVE_MS - 1));
    assert_eq!(debouncer.actual(), DriveCommand::IDLE);

    assert!(debouncer.update(OUTPUT_MIN_ACTIVE_MS + OUTPUT_MIN_INACTIVE_MS));
    assert_eq!(debouncer.actual(), ccw);
    assert_eq!(
        debouncer.next_transition_ms(OUTPUT_MIN_ACTIVE_MS + OUTPUT_MIN_INACTIVE_MS),
        None
    );

    assert!(!debouncer.set_target(
        Command::Stop,
        OUTPUT_MIN_ACTIVE_MS + OUTPUT_MIN_INACTIVE_MS + 1
    ));
    assert_eq!(debouncer.actual(), ccw);
    assert_eq!(
        debouncer.next_transition_ms(OUTPUT_MIN_ACTIVE_MS + OUTPUT_MIN_INACTIVE_MS + 1),
        Some(OUTPUT_MIN_ACTIVE_MS * 2 + OUTPUT_MIN_INACTIVE_MS)
    );
}

#[test]
fn drive_debouncer_holds_inactive_outputs_for_the_minimum_time() {
    let mut debouncer = DriveDebouncer::new();
    let cw = DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: None,
    };

    assert!(debouncer.set_target(Command::Drive(cw), 0));
    assert!(debouncer.set_target(Command::Stop, OUTPUT_MIN_ACTIVE_MS));
    assert_eq!(debouncer.actual(), DriveCommand::IDLE);

    assert!(!debouncer.set_target(Command::Drive(cw), OUTPUT_MIN_ACTIVE_MS + 100));
    assert_eq!(debouncer.actual(), DriveCommand::IDLE);
    assert_eq!(
        debouncer.next_transition_ms(OUTPUT_MIN_ACTIVE_MS + 100),
        Some(OUTPUT_MIN_ACTIVE_MS + OUTPUT_MIN_INACTIVE_MS)
    );

    assert!(!debouncer.update(OUTPUT_MIN_ACTIVE_MS + OUTPUT_MIN_INACTIVE_MS - 1));
    assert_eq!(debouncer.actual(), DriveCommand::IDLE);

    assert!(debouncer.update(OUTPUT_MIN_ACTIVE_MS + OUTPUT_MIN_INACTIVE_MS));
    assert_eq!(debouncer.actual(), cw);
}

#[test]
fn drive_debouncer_keeps_axis_timing_independent() {
    let mut debouncer = DriveDebouncer::new();
    let cw = DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: None,
    };
    let cw_up = DriveCommand {
        azimuth: Some(AzimuthDirection::Clockwise),
        elevation: Some(ElevationDirection::Up),
    };
    let up = DriveCommand {
        azimuth: None,
        elevation: Some(ElevationDirection::Up),
    };

    assert!(debouncer.set_target(Command::Drive(cw), 0));
    assert!(debouncer.set_target(Command::Drive(cw_up), 100));
    assert_eq!(debouncer.actual(), cw_up);

    assert!(!debouncer.set_target(Command::Drive(up), 200));
    assert_eq!(debouncer.actual(), cw_up);
    assert_eq!(
        debouncer.next_transition_ms(200),
        Some(OUTPUT_MIN_ACTIVE_MS)
    );

    assert!(debouncer.update(OUTPUT_MIN_ACTIVE_MS));
    assert_eq!(debouncer.actual(), up);
    assert_eq!(debouncer.next_transition_ms(OUTPUT_MIN_ACTIVE_MS), None);
}

#[test]
fn dashboard_uses_the_firmware_websocket_path() {
    assert_eq!(WS_PATH, "/ws");
    assert!(INDEX_HTML.contains("new WebSocket"));
    assert!(INDEX_HTML.contains("`${proto}://${location.host}/ws`"));
    assert!(INDEX_HTML.contains(TAKE_CONTROL_MESSAGE));
    assert!(INDEX_HTML.contains(r#"id="take""#));
    assert!(INDEX_HTML.contains(r#"message.type === "control""#));
    assert!(INDEX_HTML.contains("drive[meta.axis] === cmd"));
    assert!(INDEX_HTML.contains("axisPointers[meta.axis] !== null"));
    assert!(INDEX_HTML.contains("axisPointers[claim.axis] = null"));
    assert!(INDEX_HTML.contains(".dir:active:not(.active)"));
}

#[test]
fn dashboard_embeds_a_favicon() {
    assert!(INDEX_HTML.contains(r#"<link rel="icon" href="data:image/svg+xml,"#));
    assert!(INDEX_HTML.contains("viewBox='0 0 64 64'"));
}

#[test]
fn command_refresh_stays_inside_firmware_lease() {
    const {
        assert!(COMMAND_REFRESH_MS < COMMAND_TIMEOUT_MS);
        assert!(OUTPUT_MIN_ACTIVE_MS <= COMMAND_TIMEOUT_MS);
        assert!(OUTPUT_MIN_INACTIVE_MS <= COMMAND_TIMEOUT_MS);
    }
    assert!(INDEX_HTML.contains("const refreshMs = 150;"));
}
