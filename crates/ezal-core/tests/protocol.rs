//! Tests for the browser-facing wire protocol.

mod common;

use common::TEST_CALIBRATION;
use ezal_core::dashboard::INDEX_HTML;
use ezal_core::drive::{AzimuthDirection, DriveCommand, ElevationDirection, OUTPUT_MIN_ACTIVE_MS};
use ezal_core::feedback::{FeedbackHealth, FeedbackMonitor, FeedbackReadError, FeedbackSnapshot};
use ezal_core::position::Pointing;
use ezal_core::protocol::{
    ClientMessage, ControlStatus, FeedbackTelemetry, MovementRequest, PositionTelemetry,
    COMMAND_MAX_AGE_MS, COMMAND_REFRESH_MS, COMMAND_TIMEOUT_MS, TAKE_CONTROL_MESSAGE,
};

#[test]
fn parses_direction_commands() {
    assert_eq!(
        parse_command("drive:cw"),
        Some(DriveCommand {
            azimuth: Some(AzimuthDirection::Clockwise),
            elevation: None,
        })
    );
    assert_eq!(
        parse_command("drive:ccw"),
        Some(DriveCommand {
            azimuth: Some(AzimuthDirection::CounterClockwise),
            elevation: None,
        })
    );
    assert_eq!(
        parse_command("drive:up"),
        Some(DriveCommand {
            azimuth: None,
            elevation: Some(ElevationDirection::Up),
        })
    );
    assert_eq!(
        parse_command("drive:down"),
        Some(DriveCommand {
            azimuth: None,
            elevation: Some(ElevationDirection::Down),
        })
    );
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
    assert_eq!(ClientMessage::parse("stop"), Some(ClientMessage::Stop));
}

#[test]
fn parses_simultaneous_axis_commands() {
    assert_eq!(
        parse_command("drive:cw+up"),
        Some(DriveCommand {
            azimuth: Some(AzimuthDirection::Clockwise),
            elevation: Some(ElevationDirection::Up),
        })
    );
    assert_eq!(
        parse_command("drive:down+ccw"),
        Some(DriveCommand {
            azimuth: Some(AzimuthDirection::CounterClockwise),
            elevation: Some(ElevationDirection::Down),
        })
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
fn formats_calibrated_feedback_without_demo_fields() {
    let mut json = String::new();
    FeedbackTelemetry {
        azimuth_tenths: 4247,
        elevation_tenths: 3,
        health: FeedbackHealth::Healthy,
    }
    .write_json(&mut json)
    .unwrap();
    assert_eq!(
        json,
        r#"{"type":"feedback","azimuth_tenths":4247,"elevation_tenths":3,"control_state":"idle"}"#
    );
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

#[test]
fn domain_feedback_projects_retained_diagnostics_and_legacy_wire_health() {
    let raw = TEST_CALIBRATION
        .position_to_feedback(Pointing::new(100.0, 20.0))
        .unwrap();
    let mut monitor = FeedbackMonitor::new(TEST_CALIBRATION, None);
    monitor.update(100, 98, Ok(raw));
    let failed = monitor
        .update(200, 198, Err(FeedbackReadError::TimedOut))
        .snapshot;
    let mut json = String::new();
    FeedbackTelemetry::from(failed)
        .write_json(&mut json)
        .unwrap();
    assert_eq!(
        json,
        r#"{"type":"feedback","azimuth_tenths":1000,"elevation_tenths":200,"control_state":"feedback-stale"}"#
    );
    assert_eq!(
        PositionTelemetry::from(failed),
        PositionTelemetry {
            a0_mv: raw.a0_elevation_mv,
            a1_mv: raw.a1_azimuth_mv
        }
    );

    json.clear();
    FeedbackTelemetry::from(FeedbackSnapshot::INITIAL)
        .write_json(&mut json)
        .unwrap();
    assert_eq!(
        json,
        r#"{"type":"feedback","azimuth_tenths":0,"elevation_tenths":0,"control_state":"no-feedback"}"#
    );
}

fn parse_command(text: &str) -> Option<DriveCommand> {
    match ClientMessage::parse(&format!("press:1:1:{text}"))? {
        ClientMessage::Movement(request) => Some(request.drive),
        _ => None,
    }
}
