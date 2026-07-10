//! Tests for the browser-facing wire protocol.

use ezal_core::dashboard::INDEX_HTML;
use ezal_core::drive::{
    AzimuthDirection, Command, DriveCommand, ElevationDirection, OUTPUT_MIN_ACTIVE_MS,
    OUTPUT_MIN_INACTIVE_MS,
};
use ezal_core::protocol::{
    parse_command, ClientMessage, ControlStatus, PositionTelemetry, COMMAND_REFRESH_MS,
    COMMAND_TIMEOUT_MS, TAKE_CONTROL_MESSAGE,
};

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
fn command_refresh_stays_inside_firmware_lease() {
    const {
        assert!(COMMAND_REFRESH_MS < COMMAND_TIMEOUT_MS);
        assert!(OUTPUT_MIN_ACTIVE_MS <= COMMAND_TIMEOUT_MS);
        assert!(OUTPUT_MIN_INACTIVE_MS <= COMMAND_TIMEOUT_MS);
    }
    assert!(INDEX_HTML.contains("const refreshMs = 150;"));
}
