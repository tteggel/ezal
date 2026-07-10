//! Dashboard WebSocket wire protocol.
//!
//! The browser-facing half of the firmware: the text the dashboard sends
//! ([`ClientMessage`], parsed from strings like `drive:cw+up`) and the compact
//! JSON frames it receives ([`PositionTelemetry`], [`ControlStatus`]). The
//! HTTP server and socket transport that carry these live in `ezal-firmware`;
//! the drive semantics they wrap live in [`crate::drive`].

use core::fmt;

use crate::drive::{
    AzimuthDirection, Command, DriveCommand, ElevationDirection, OUTPUT_MIN_ACTIVE_MS,
    OUTPUT_MIN_INACTIVE_MS,
};

/// WebSocket route used by the dashboard.
pub const WS_PATH: &str = "/ws";

/// WebSocket message used by a viewer to become the controller.
pub const TAKE_CONTROL_MESSAGE: &str = "control:take";

/// How often the firmware samples feedback and emits position telemetry.
pub const TELEMETRY_PERIOD_MS: u64 = 500;

/// How often the browser repeats a held drive state.
pub const COMMAND_REFRESH_MS: u64 = 150;

/// How long the firmware allows a movement command to live without refresh.
pub const COMMAND_TIMEOUT_MS: u64 = 750;

// The browser's refresh must beat the firmware lease, and neither actuator
// minimum (defined in `crate::drive`) may outlast that lease — otherwise a
// held control could expire before its debounced output is even applied.
const _: () = assert!(COMMAND_REFRESH_MS < COMMAND_TIMEOUT_MS);
const _: () = assert!(OUTPUT_MIN_ACTIVE_MS <= COMMAND_TIMEOUT_MS);
const _: () = assert!(OUTPUT_MIN_INACTIVE_MS <= COMMAND_TIMEOUT_MS);

/// Parse a dashboard drive/stop text message into a [`Command`].
///
/// This is the browser's wire encoding — `stop`, or `drive:` followed by one
/// or two `+`-joined axis tokens (`cw`/`ccw`/`up`/`down`). A future serial
/// transport would provide its own parser over the same [`Command`] type.
pub fn parse_command(message: &str) -> Option<Command> {
    if message == "stop" {
        return Some(Command::Stop);
    }

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

    command.is_active().then_some(Command::Drive(command))
}

/// A client-originated WebSocket message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientMessage {
    /// Request controller ownership for this connection.
    TakeControl,
    /// Drive-state command for the active controller.
    Command(Command),
}

impl ClientMessage {
    /// Parse a browser-originated WebSocket text message.
    pub fn parse(message: &str) -> Option<Self> {
        if message == TAKE_CONTROL_MESSAGE {
            Some(Self::TakeControl)
        } else {
            parse_command(message).map(Self::Command)
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

/// Controller ownership and active drive state reported to each browser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlStatus {
    /// Whether the receiving connection currently owns control.
    pub controller: bool,
    /// Server-confirmed drive state.
    pub drive: DriveCommand,
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
        fmt::Write::write_str(out, "}")
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
