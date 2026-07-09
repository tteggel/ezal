//! Tests for the browser-facing wire protocol.

use ezal_core::web::{
    AzimuthDirection, ClientMessage, Command, ControlStatus, DriveCommand, ElevationDirection,
    PositionTelemetry, COMMAND_REFRESH_MS, COMMAND_TIMEOUT_MS, INDEX_HTML, TAKE_CONTROL_MESSAGE,
    WS_PATH,
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
    assert!(COMMAND_REFRESH_MS < COMMAND_TIMEOUT_MS);
    assert!(INDEX_HTML.contains("const refreshMs = 150;"));
}
