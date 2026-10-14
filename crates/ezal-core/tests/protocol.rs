//! Tests for the browser-facing wire protocol.

mod common;

use common::TEST_CALIBRATION;
use ezal_core::coordination::{ControlStatus, CoordinationState};
use ezal_core::drive::{AzimuthDirection, DriveCommand, ElevationDirection};
use ezal_core::duty::DutyStatus;
use ezal_core::feedback::{FeedbackHealth, FeedbackMonitor, FeedbackReadError, FeedbackSnapshot};
use ezal_core::position::{FeedbackVoltages, Pointing};
use ezal_core::protocol::{ClientMessage, TelemetryMessage, TAKE_CONTROL_MESSAGE};
use ezal_core::session::MovementRequest;

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

/// The position and feedback members of the telemetry frame carrying `feedback`.
fn feedback_frame(feedback: FeedbackSnapshot) -> String {
    let mut snapshot = CoordinationState::new().telemetry(1, 0);
    snapshot.feedback = feedback;
    let message = TelemetryMessage {
        snapshot,
        token: None,
        tracking: CoordinationState::new().tracking_telemetry(1, 0),
    };
    let mut json = String::new();
    message.write_json(&mut json).unwrap();
    json.truncate(json.find(",\"control\":").unwrap());
    json
}

#[test]
fn telemetry_nulls_each_absent_measurement_independently() {
    let rejected = FeedbackSnapshot {
        raw: Some(FeedbackVoltages {
            a0_elevation_mv: 901,
            a1_azimuth_mv: -12,
        }),
        last_valid: None,
        health: FeedbackHealth::Invalid,
    };
    assert_eq!(
        feedback_frame(rejected),
        r#"{"type":"telemetry","position":{"a0_mv":901,"a1_mv":-12},"feedback":{"azimuth_tenths":null,"elevation_tenths":null,"health":"feedback-invalid"}"#
    );
}

#[test]
fn formats_control_status_as_compact_json() {
    let duty = CoordinationState::new().telemetry(1, 0).control.duty;
    let mut json = String::new();
    ControlStatus {
        controller: true,
        drive: DriveCommand {
            azimuth: Some(AzimuthDirection::CounterClockwise),
            elevation: Some(ElevationDirection::Up),
        },
        network_ready: true,
        motion_fault: None,
        duty,
    }
    .write_json(&mut json)
    .unwrap();

    assert_eq!(
        json,
        r#"{"type":"control","controller":true,"azimuth":"ccw","elevation":"up","network_ready":true,"motion_fault":null,"duty_remaining_ms":[0,0],"duty_cooldown_ms":[900000,900000]}"#
    );

    json.clear();
    ControlStatus {
        controller: false,
        drive: DriveCommand::IDLE,
        network_ready: false,
        motion_fault: None,
        duty,
    }
    .write_json(&mut json)
    .unwrap();

    assert_eq!(
        json,
        r#"{"type":"control","controller":false,"azimuth":null,"elevation":null,"network_ready":false,"motion_fault":null,"duty_remaining_ms":[0,0],"duty_cooldown_ms":[900000,900000]}"#
    );
}

#[test]
fn telemetry_projects_healthy_feedback_with_a0_elevation_and_a1_azimuth() {
    let raw = TEST_CALIBRATION
        .position_to_feedback(Pointing::new(100.0, 20.0))
        .unwrap();
    assert_ne!(raw.a0_elevation_mv, raw.a1_azimuth_mv);
    let mut monitor = FeedbackMonitor::new(TEST_CALIBRATION, None);
    let healthy = monitor.update(100, 98, Ok(raw)).snapshot;
    assert_eq!(
        feedback_frame(healthy),
        format!(
            r#"{{"type":"telemetry","position":{{"a0_mv":{},"a1_mv":{}}},"feedback":{{"azimuth_tenths":1000,"elevation_tenths":200,"health":"healthy"}}"#,
            raw.a0_elevation_mv, raw.a1_azimuth_mv
        )
    );
}

#[test]
fn telemetry_retains_diagnostics_after_a_failed_read() {
    let raw = TEST_CALIBRATION
        .position_to_feedback(Pointing::new(100.0, 20.0))
        .unwrap();
    let mut monitor = FeedbackMonitor::new(TEST_CALIBRATION, None);
    monitor.update(100, 98, Ok(raw));
    let failed = monitor
        .update(200, 198, Err(FeedbackReadError::TimedOut))
        .snapshot;
    assert_eq!(
        feedback_frame(failed),
        format!(
            r#"{{"type":"telemetry","position":{{"a0_mv":{},"a1_mv":{}}},"feedback":{{"azimuth_tenths":1000,"elevation_tenths":200,"health":"read-timeout"}}"#,
            raw.a0_elevation_mv, raw.a1_azimuth_mv
        )
    );
}

fn parse_command(text: &str) -> Option<DriveCommand> {
    match ClientMessage::parse(&format!("press:1:1:{text}"))? {
        ClientMessage::Movement(request) => Some(request.drive),
        _ => None,
    }
}

#[test]
fn telemetry_encodes_one_observation_with_its_proof() {
    use ezal_core::session::CommandToken;

    let message = TelemetryMessage {
        snapshot: CoordinationState::new().telemetry(1, 0),
        tracking: CoordinationState::new().tracking_telemetry(1, 0),
        token: Some(CommandToken {
            token: 42,
            expires_ms: 1000,
        }),
    };
    let mut json = String::new();
    message.write_json(&mut json).unwrap();
    assert_eq!(
        json,
        r#"{"type":"telemetry","position":{"a0_mv":null,"a1_mv":null},"feedback":{"azimuth_tenths":null,"elevation_tenths":null,"health":"no-feedback"},"control":{"type":"control","controller":false,"azimuth":null,"elevation":null,"network_ready":false,"motion_fault":null,"duty_remaining_ms":[0,0],"duty_cooldown_ms":[900000,900000]},"freshness":{"token":"42"},"tracking":{"revision":"0","operator":null,"mode":"manual","phase":"idle","run":null,"selection":null,"plan":{"status":"unplanned","reason":null,"preview":null},"feedback_stale":true,"feedback_age_ms":null,"deviation":{"run":null,"warning":false,"maximum_deg":null,"outside_ms":0,"current":null},"recovery":null,"termination":null,"summary":null}}"#
    );
}

#[test]
fn telemetry_fits_its_fixed_buffer_and_reports_formatting_failure() {
    use ezal_core::motion::MotionFault;
    use ezal_core::protocol::TELEMETRY_CAPACITY;
    use ezal_core::session::CommandToken;

    let mut snapshot = CoordinationState::new().telemetry(1, 0);
    snapshot.feedback.raw = Some(FeedbackVoltages {
        a0_elevation_mv: i32::MIN,
        a1_azimuth_mv: i32::MIN,
    });
    snapshot.feedback.health = FeedbackHealth::CalibrationInvalid;
    snapshot.feedback.last_valid = Some(common::sample(Pointing::new(360.0, 90.0), 0));
    snapshot.control.motion_fault = Some(MotionFault::ElevationImplausibleSpeed);
    let maximum = ezal_core::duty::AxisDutyStatus {
        remaining_ms: u64::MAX,
        cooldown_ms: u64::MAX,
    };
    snapshot.control.duty = DutyStatus {
        azimuth: maximum,
        elevation: maximum,
    };
    snapshot.control.drive = DriveCommand {
        azimuth: Some(AzimuthDirection::CounterClockwise),
        elevation: Some(ElevationDirection::Down),
    };
    let mut control: heapless::String<{ ezal_core::protocol::CONTROL_STATUS_CAPACITY }> =
        heapless::String::new();
    snapshot.control.write_json(&mut control).unwrap();
    let mut message = TelemetryMessage {
        snapshot,
        tracking: CoordinationState::new().tracking_telemetry(1, 0),
        token: Some(CommandToken {
            token: u64::MAX,
            expires_ms: u64::MAX,
        }),
    };
    let mut payload: heapless::String<TELEMETRY_CAPACITY> = heapless::String::new();
    message.write_json(&mut payload).unwrap();
    assert!(payload.contains(r#""token":"18446744073709551615"}"#));
    let mut undersized: heapless::String<16> = heapless::String::new();
    assert!(message.write_json(&mut undersized).is_err());
    payload.clear();
    message.token = None;
    message.write_json(&mut payload).unwrap();
    assert!(payload.contains(r#""freshness":null,"tracking":"#));
}

#[test]
fn tracking_replies_fit_their_fixed_buffer() {
    use ezal_core::operator::LeaseError;
    use ezal_core::protocol::{TrackingRejection, TrackingReply, TRACKING_REPLY_CAPACITY};
    use ezal_core::tracking::{LifecycleError, RuntimeError};

    // Maximum-width identifiers and the longest rejection name.
    let widest = [
        (
            Ok(u64::MAX),
            r#"{"type":"tracking_result","sequence":"18446744073709551615","ok":true,"revision":"18446744073709551615","error":null}"#,
        ),
        (
            Err(TrackingRejection::Rejected(RuntimeError::Lifecycle(
                LifecycleError::Lease(LeaseError::ClockOrCounter),
            ))),
            r#"{"type":"tracking_result","sequence":"18446744073709551615","ok":false,"revision":null,"error":"Rejected(Lifecycle(Lease(ClockOrCounter)))"}"#,
        ),
    ];
    for (result, expected) in widest {
        let mut json: heapless::String<TRACKING_REPLY_CAPACITY> = heapless::String::new();
        TrackingReply {
            sequence: u64::MAX,
            result,
        }
        .write_json(&mut json)
        .unwrap();
        assert_eq!(json, expected);
    }
}
