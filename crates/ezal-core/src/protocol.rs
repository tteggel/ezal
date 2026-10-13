//! Dashboard WebSocket wire protocol.
//!
//! The browser-facing half of the firmware: the text the dashboard sends
//! ([`ClientMessage`], including freshness envelopes around `drive:cw+up`) and the compact
//! JSON frames it receives ([`TelemetryMessage`], [`ControlStatus`]). The
//! HTTP server and socket transport that carry these live in `ezal-firmware`;
//! the drive semantics they wrap live in [`crate::drive`].
//!
//! Each wire type this module defines ([`TelemetryMessage`],
//! [`TrackingTelemetry`], [`TrackingReply`]) encodes itself with `write_json`. A
//! type from another module gets an inherent `write_json` here only when two or
//! more messages write it ([`ControlStatus`]: the acknowledgement and
//! telemetry); a type written by one message is encoded inline in that message
//! ([`CommandToken`]).

use crate::coordination::TelemetrySnapshot;
use core::fmt;

mod tracking;
pub use tracking::{
    TrackingIntent, TrackingRejection, TrackingReply, TrackingTelemetry, TRACKING_REPLY_CAPACITY,
};

use crate::coordination::ControlStatus;
use crate::drive::{AzimuthDirection, DriveCommand, ElevationDirection};
use crate::motion::MotionFault;

/// WebSocket route used by the dashboard.
pub const WS_PATH: &str = "/ws";

/// WebSocket message used by a viewer to become the controller.
pub const TAKE_CONTROL_MESSAGE: &str = "control:take";

/// Server message that clears the browser's held input; motion needs a new press.
pub const INPUT_REQUIRED_MESSAGE: &str = "{\"type\":\"input_required\"}";

/// Fixed acknowledgement buffer, including maximum-width duty counters.
pub const CONTROL_STATUS_CAPACITY: usize = 320;

use crate::session::{CommandToken, MovementRequest};

/// Decode only movement directions; freshness and ownership are separate policy.
fn parse_drive(message: &str) -> Option<DriveCommand> {
    let drive = message.strip_prefix("drive:")?;
    if drive.is_empty() {
        return None;
    }

    let mut command = DriveCommand::IDLE;

    for token in drive.split('+') {
        match token {
            "cw" if command.azimuth.is_none() => {
                command.azimuth = Some(AzimuthDirection::Clockwise);
            }
            "ccw" if command.azimuth.is_none() => {
                command.azimuth = Some(AzimuthDirection::CounterClockwise);
            }
            "up" if command.elevation.is_none() => {
                command.elevation = Some(ElevationDirection::Up);
            }
            "down" if command.elevation.is_none() => {
                command.elevation = Some(ElevationDirection::Down);
            }
            _ => return None,
        }
    }

    command.is_active().then_some(command)
}

/// A client-originated WebSocket message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientMessage {
    /// Request controller ownership for this connection.
    TakeControl,
    /// A stop needs no freshness proof; making an old stop harmlessly stop
    /// again is preferable to rejecting it during a network interruption.
    Stop,
    /// Movement requires both a recent server token and ordered operator input.
    Movement(MovementRequest),
    /// Single-pass intent; the server supplies connection identity and inputs.
    Tracking(TrackingIntent),
}

impl ClientMessage {
    /// Parse a browser-originated WebSocket text message.
    pub fn parse(message: &str) -> Option<Self> {
        if message == TAKE_CONTROL_MESSAGE {
            Some(Self::TakeControl)
        } else if message == "stop" {
            Some(Self::Stop)
        } else if let Some(message) = message.strip_prefix("track:") {
            TrackingIntent::parse(message).map(Self::Tracking)
        } else {
            let mut fields = message.splitn(4, ':');
            let pressed = match fields.next()? {
                "press" => true,
                "hold" => false,
                _ => return None,
            };
            let token = fields.next()?.parse().ok()?;
            let sequence = fields.next()?.parse().ok()?;
            let drive = parse_drive(fields.next()?)?;
            Some(Self::Movement(MovementRequest {
                token,
                sequence,
                pressed,
                drive,
            }))
        }
    }
}

/// Browser token for an azimuth direction, or `None` when azimuth is idle.
const fn azimuth_token(direction: Option<AzimuthDirection>) -> Option<&'static str> {
    match direction {
        Some(AzimuthDirection::Clockwise) => Some("cw"),
        Some(AzimuthDirection::CounterClockwise) => Some("ccw"),
        None => None,
    }
}

/// Browser token for an elevation direction, or `None` when elevation is idle.
const fn elevation_token(direction: Option<ElevationDirection>) -> Option<&'static str> {
    match direction {
        Some(ElevationDirection::Up) => Some("up"),
        Some(ElevationDirection::Down) => Some("down"),
        None => None,
    }
}

impl ControlStatus {
    /// Write this status as a compact WebSocket JSON message.
    pub fn write_json(&self, out: &mut impl fmt::Write) -> fmt::Result {
        fn write_option(out: &mut impl fmt::Write, value: Option<&str>) -> fmt::Result {
            match value {
                Some(value) => write!(out, "\"{}\"", value),
                None => fmt::Write::write_str(out, "null"),
            }
        }

        write!(
            out,
            "{{\"type\":\"control\",\"controller\":{},\"azimuth\":",
            if self.controller { "true" } else { "false" }
        )?;
        write_option(out, azimuth_token(self.drive.azimuth))?;
        fmt::Write::write_str(out, ",\"elevation\":")?;
        write_option(out, elevation_token(self.drive.elevation))?;
        write!(
            out,
            ",\"network_ready\":{},\"motion_fault\":",
            self.network_ready
        )?;
        write_option(out, self.motion_fault.map(MotionFault::as_str))?;
        write!(
            out,
            ",\"duty_remaining_ms\":[{},{}],\"duty_cooldown_ms\":[{},{}]}}",
            self.duty.azimuth.remaining_ms,
            self.duty.elevation.remaining_ms,
            self.duty.azimuth.cooldown_ms,
            self.duty.elevation.cooldown_ms
        )
    }
}

fn degrees_to_tenths(degrees: f32) -> i32 {
    (degrees * 10.0 + 0.5) as i32
}

/// Fixed maximum payload allocation for one periodic observation.
pub const TELEMETRY_CAPACITY: usize = 4096;

/// One coherent domain observation and its original freshness proof.
pub struct TelemetryMessage {
    /// Values copied together from coordination state.
    pub snapshot: TelemetrySnapshot,
    /// Proof stamped before formatting and any socket write.
    pub token: Option<CommandToken>,
    /// Small projection of the same device observation, never a route copy.
    pub tracking: TrackingTelemetry,
}

impl TelemetryMessage {
    /// Encode an entire periodic observation in one bounded WebSocket message.
    /// An absent measurement is `null`: raw millivolts until an ADC pair
    /// completes, tenths of a degree until a sample is accepted.
    pub fn write_json(&self, out: &mut impl fmt::Write) -> fmt::Result {
        fn write_number(out: &mut impl fmt::Write, value: Option<i32>) -> fmt::Result {
            match value {
                Some(value) => write!(out, "{value}"),
                None => out.write_str("null"),
            }
        }

        let feedback = self.snapshot.feedback;
        let position = feedback.last_valid.map(|sample| sample.position());
        out.write_str("{\"type\":\"telemetry\",\"position\":{\"a0_mv\":")?;
        write_number(out, feedback.raw.map(|raw| raw.a0_elevation_mv))?;
        out.write_str(",\"a1_mv\":")?;
        write_number(out, feedback.raw.map(|raw| raw.a1_azimuth_mv))?;
        out.write_str("},\"feedback\":{\"azimuth_tenths\":")?;
        write_number(out, position.map(|p| degrees_to_tenths(p.azimuth_deg)))?;
        out.write_str(",\"elevation_tenths\":")?;
        write_number(out, position.map(|p| degrees_to_tenths(p.elevation_deg)))?;
        write!(
            out,
            ",\"health\":\"{}\"}},\"control\":",
            feedback.health.as_str()
        )?;
        self.snapshot.control.write_json(out)?;
        out.write_str(",\"freshness\":")?;
        match self.token {
            Some(token) => write!(out, "{{\"token\":\"{}\"}}", token.token)?,
            None => out.write_str("null")?,
        }
        out.write_str(",\"tracking\":")?;
        self.tracking.write_json(out)?;
        out.write_str("}")
    }
}
