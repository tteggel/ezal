//! Dashboard WebSocket wire protocol.
//!
//! The browser-facing half of the firmware: the text the dashboard sends
//! ([`ClientMessage`], including freshness envelopes around `drive:cw+up`) and the compact
//! JSON frames it receives ([`PositionTelemetry`], [`ControlStatus`]). The
//! HTTP server and socket transport that carry these live in `ezal-firmware`;
//! the drive semantics they wrap live in [`crate::drive`].

use crate::coordination::TelemetrySnapshot;
use core::fmt;

mod tracking;
pub use tracking::{
    TrackingIntent, TrackingRejection, TrackingReply, TrackingTelemetry, TRACKING_REPLY_CAPACITY,
};

use crate::coordination::ControlStatus;
use crate::drive::{AzimuthDirection, DriveCommand, ElevationDirection};
use crate::feedback::{FeedbackHealth, FeedbackSnapshot};
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

impl CommandToken {
    /// Write a proof nested inside its coherent telemetry observation.
    pub fn write_json(&self, out: &mut impl fmt::Write) -> fmt::Result {
        write!(out, "{{\"token\":\"{}\"}}", self.token)
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

/// Raw position feedback reported to the browser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PositionTelemetry {
    /// ADS1015 channel A0 in millivolts.
    pub a0_mv: i32,
    /// ADS1015 channel A1 in millivolts.
    pub a1_mv: i32,
}

impl PositionTelemetry {
    /// A zeroed telemetry sample used before the first ADC read completes.
    pub const ZERO: Self = Self { a0_mv: 0, a1_mv: 0 };

    /// Write this telemetry sample as a compact WebSocket JSON message.
    pub fn write_json(&self, out: &mut impl fmt::Write) -> fmt::Result {
        write!(
            out,
            "{{\"type\":\"position\",\"a0_mv\":{},\"a1_mv\":{}}}",
            self.a0_mv, self.a1_mv
        )
    }
}

impl From<FeedbackSnapshot> for PositionTelemetry {
    fn from(snapshot: FeedbackSnapshot) -> Self {
        snapshot.raw.map_or(Self::ZERO, |raw| Self {
            a0_mv: raw.a0_elevation_mv,
            a1_mv: raw.a1_azimuth_mv,
        })
    }
}

/// Last good calibrated position and acquisition health.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeedbackTelemetry {
    /// Azimuth in tenths of a degree.
    pub azimuth_tenths: i32,
    /// Elevation in tenths of a degree.
    pub elevation_tenths: i32,
    /// Health of the latest acquisition.
    pub health: FeedbackHealth,
}

impl From<FeedbackSnapshot> for FeedbackTelemetry {
    fn from(snapshot: FeedbackSnapshot) -> Self {
        let position = snapshot.last_valid.map(|sample| sample.position());
        Self {
            azimuth_tenths: position.map_or(0, |position| degrees_to_tenths(position.azimuth_deg)),
            elevation_tenths: position
                .map_or(0, |position| degrees_to_tenths(position.elevation_deg)),
            health: snapshot.health,
        }
    }
}

impl FeedbackTelemetry {
    /// Encode calibrated feedback for the dashboard.
    pub fn write_json(&self, out: &mut impl fmt::Write) -> fmt::Result {
        // Preserve the dashboard's wire names while keeping domain health explicit.
        let state = match self.health {
            FeedbackHealth::Healthy => "idle",
            FeedbackHealth::ReadTimedOut => "feedback-stale",
            other => other.as_str(),
        };
        write!(out, "{{\"type\":\"feedback\",\"azimuth_tenths\":{},\"elevation_tenths\":{},\"control_state\":\"{}\"}}", self.azimuth_tenths, self.elevation_tenths, state)
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
    pub fn write_json(&self, out: &mut impl fmt::Write) -> fmt::Result {
        out.write_str("{\"type\":\"telemetry\",\"position\":")?;
        PositionTelemetry::from(self.snapshot.feedback).write_json(out)?;
        out.write_str(",\"feedback\":")?;
        FeedbackTelemetry::from(self.snapshot.feedback).write_json(out)?;
        out.write_str(",\"control\":")?;
        self.snapshot.control.write_json(out)?;
        out.write_str(",\"freshness\":")?;
        match self.token {
            Some(token) => token.write_json(out)?,
            None => out.write_str("null")?,
        }
        out.write_str(",\"tracking\":")?;
        self.tracking.write_json(out)?;
        out.write_str("}")
    }
}
