//! Dashboard WebSocket wire protocol.
//!
//! The browser-facing half of the firmware: the text the dashboard sends
//! ([`ClientMessage`], including freshness envelopes around `drive:cw+up`) and the compact
//! JSON frames it receives ([`PositionTelemetry`], [`ControlStatus`]). The
//! HTTP server and socket transport that carry these live in `ezal-firmware`;
//! the drive semantics they wrap live in [`crate::drive`].

use core::fmt;

use crate::control::ControlState;
use crate::drive::{
    AzimuthDirection, Command, DriveCommand, ElevationDirection, MOVEMENT_LEASE_MS,
    OUTPUT_MIN_ACTIVE_MS,
};
use crate::motion::MotionFault;
use crate::simulation::{PassPhase, SIMULATED_SATELLITE};

/// WebSocket route used by the dashboard.
pub const WS_PATH: &str = "/ws";

/// WebSocket message used by a viewer to become the controller.
pub const TAKE_CONTROL_MESSAGE: &str = "control:take";

/// How often the firmware sends position, control, and tracking telemetry to
/// each dashboard connection. Feedback sampling has its own faster period,
/// [`crate::control::FEEDBACK_PERIOD_MS`].
pub const TELEMETRY_PERIOD_MS: u64 = 500;

/// How often the browser repeats a held drive state.
pub const COMMAND_REFRESH_MS: u64 = 150;

/// How long the firmware allows a movement command to live without refresh.
pub const COMMAND_TIMEOUT_MS: u64 = MOVEMENT_LEASE_MS;

/// Maximum age of the server observation a dashboard movement can reference.
/// A token proves a recent round trip without trusting the browser's clock.
/// Its absolute expiry also bounds motion after a command waited in a TCP
/// buffer, a fragmented message, or the firmware's command mailbox.
pub const COMMAND_MAX_AGE_MS: u64 = 1_000;

// The browser's refresh must beat the firmware lease. A steadily held control
// must also leave the minimum on-time on its lease between refreshes, or the
// actuator would defer an activation until the following refresh.
const _: () = assert!(COMMAND_REFRESH_MS < COMMAND_TIMEOUT_MS);
const _: () = assert!(COMMAND_REFRESH_MS + OUTPUT_MIN_ACTIVE_MS <= COMMAND_TIMEOUT_MS);
const _: () = assert!(TELEMETRY_PERIOD_MS + COMMAND_REFRESH_MS < COMMAND_MAX_AGE_MS);

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
    /// A stop needs no freshness proof; making an old stop harmlessly stop
    /// again is preferable to rejecting it during a network interruption.
    Command(Command),
    /// Movement requires both a recent server token and ordered operator input.
    Movement(MovementRequest),
}

impl ClientMessage {
    /// Parse a browser-originated WebSocket text message.
    pub fn parse(message: &str) -> Option<Self> {
        if message == TAKE_CONTROL_MESSAGE {
            Some(Self::TakeControl)
        } else if message == "stop" {
            Some(Self::Command(Command::Stop))
        } else {
            let mut fields = message.splitn(4, ':');
            let pressed = match fields.next()? {
                "press" => true,
                "hold" => false,
                _ => return None,
            };
            let token = fields.next()?.parse().ok()?;
            let sequence = fields.next()?.parse().ok()?;
            let Command::Drive(drive) = parse_command(fields.next()?)? else {
                return None;
            };
            Some(Self::Movement(MovementRequest {
                token,
                sequence,
                pressed,
                drive,
            }))
        }
    }
}

/// Parsed `press:<token>:<sequence>:drive:cw` or `hold:…` movement envelope.
/// `press` is emitted only for new operator input; timer refreshes use `hold`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MovementRequest {
    /// Opaque server-issued identifier, scoped to this WebSocket connection.
    pub token: u64,
    /// Strictly increasing client input number; zero is never admitted.
    pub sequence: u64,
    /// Whether new operator input generated this request rather than a timer.
    pub pressed: bool,
    /// The operator's complete desired state for both axes.
    pub drive: DriveCommand,
}

/// One short-lived server challenge. Tokens are strings on the wire because
/// JavaScript numbers cannot represent every `u64` exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandToken {
    /// Server-generated session id and serial number, represented as a string
    /// in browser messages to avoid floating-point integer truncation.
    pub token: u64,
    /// Exclusive absolute expiry on the firmware's monotonic millisecond clock.
    pub expires_ms: u64,
}

impl CommandToken {
    /// Write the proof completing a periodic application telemetry batch.
    pub fn write_json(&self, out: &mut impl fmt::Write) -> fmt::Result {
        write!(
            out,
            "{{\"type\":\"freshness\",\"token\":\"{}\"}}",
            self.token
        )
    }
}

/// Per-WebSocket freshness and operator-intent admission, independent of
/// controller ownership. This is a bounded age check, not authentication.
///
/// Issuing a token only after each complete telemetry batch ties movement to
/// application traffic in both directions. Keep three recent tokens so the
/// preceding batch remains usable while its replacement is in flight. Every
/// token has its original deadline; issuing another never renews the old one.
pub struct CommandFreshness {
    session_id: u32,
    next_token: u32,
    tokens: [Option<CommandToken>; 3],
    next_slot: usize,
    last_sequence: u64,
    armed_until_ms: Option<u64>,
    network_generation: Option<u64>,
}

impl CommandFreshness {
    /// Create admission state for one newly allocated WebSocket session id.
    pub const fn new(session_id: u32) -> Self {
        Self {
            session_id,
            next_token: 0,
            tokens: [None; 3],
            next_slot: 0,
            last_sequence: 0,
            armed_until_ms: None,
            network_generation: None,
        }
    }

    /// Start a new telemetry batch, returning its proof and absolute expiry.
    /// Counter or clock exhaustion fails closed instead of reusing a token.
    pub fn issue(&mut self, now_ms: u64) -> Option<CommandToken> {
        self.next_token = self.next_token.checked_add(1)?;
        let token = CommandToken {
            token: (u64::from(self.session_id) << 32) | u64::from(self.next_token),
            expires_ms: now_ms.checked_add(COMMAND_MAX_AGE_MS)?,
        };
        self.tokens[self.next_slot] = Some(token);
        self.next_slot = (self.next_slot + 1) % self.tokens.len();
        Some(token)
    }

    /// Forget the held intent on an ordinary stop or control transfer.
    pub fn disarm(&mut self) {
        self.armed_until_ms = None;
    }

    /// Reject an unsafe input and invalidate its entire telemetry generation.
    /// Queued presses referencing that generation cannot restore movement;
    /// recovery requires a later telemetry batch and a new operator press.
    pub fn reject(&mut self) {
        self.disarm();
        self.tokens.fill(None);
    }

    /// Bind proofs to the network's current recovery generation. Even a brief
    /// outage invalidates old buffered presses if the TCP connection survives.
    /// Call before both telemetry issuance and input admission.
    pub fn observe_network(&mut self, generation: u64, ready: bool) {
        let observed = ready.then_some(generation);
        if observed.is_none() || observed != self.network_generation {
            self.reject();
        }
        self.network_generation = observed;
    }

    /// Return the absolute movement deadline, preserving the server token's
    /// age through all downstream queuing and relay timing.
    pub fn admit(&mut self, request: MovementRequest, now_ms: u64) -> Option<u64> {
        let expires_ms = self
            .tokens
            .iter()
            .flatten()
            .find(|token| token.token == request.token)
            .map(|token| token.expires_ms);
        let deadline = expires_ms.filter(|expires_ms| now_ms < *expires_ms);
        let fresh_press_or_hold = request.pressed
            || self
                .armed_until_ms
                .is_some_and(|deadline| now_ms < deadline);
        if request.sequence <= self.last_sequence || !fresh_press_or_hold || deadline.is_none() {
            self.reject();
            return None;
        }
        self.last_sequence = request.sequence;
        let deadline = deadline?.min(now_ms.saturating_add(COMMAND_TIMEOUT_MS));
        self.armed_until_ms = Some(deadline);
        Some(deadline)
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
    /// The firmware has a current usable network lease.
    pub network_ready: bool,
    /// A latched physical feedback fault; recovery requires a firmware restart
    /// after the mechanism and configured progress thresholds are inspected.
    pub motion_fault: Option<MotionFault>,
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

/// Target/control source and physical-I/O wiring for tracking telemetry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackingMode {
    /// Synthetic target source with real ADS1015 feedback and GPIO outputs.
    HardwareWalkingSkeleton,
    /// Real ADC feedback with dashboard-owned direction commands.
    Manual,
}

impl TrackingMode {
    /// Stable token used by dashboard telemetry.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HardwareWalkingSkeleton => "hardware-walking-skeleton",
            Self::Manual => "manual",
        }
    }

    /// Whether firmware, rather than a dashboard client, owns control.
    pub const fn is_autonomous(self) -> bool {
        !matches!(self, Self::Manual)
    }
}

/// Autonomous tracking state reported to the dashboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackingTelemetry {
    /// Target/control and hardware-I/O mode.
    pub mode: TrackingMode,
    /// Zero-based pass index since boot.
    pub pass_index: u32,
    /// Current pass/pause phase.
    pub phase: PassPhase,
    /// Milliseconds remaining in the current phase.
    pub phase_remaining_ms: u64,
    /// Calibrated current azimuth in tenths of a degree.
    pub azimuth_tenths: i32,
    /// Calibrated current elevation in tenths of a degree.
    pub elevation_tenths: i32,
    /// Target azimuth in tenths of a degree, absent during a pause.
    pub target_azimuth_tenths: Option<i32>,
    /// Target elevation in tenths of a degree, absent during a pause.
    pub target_elevation_tenths: Option<i32>,
    /// Controller health/inhibit state.
    pub control_state: ControlState,
}

impl TrackingTelemetry {
    /// Initial idle snapshot used before the autonomous task publishes.
    pub const ZERO: Self = Self {
        mode: TrackingMode::Manual,
        pass_index: 0,
        phase: PassPhase::Pause,
        phase_remaining_ms: 0,
        azimuth_tenths: 0,
        elevation_tenths: 0,
        target_azimuth_tenths: None,
        target_elevation_tenths: None,
        control_state: ControlState::Idle,
    };

    /// Write this snapshot as a compact WebSocket JSON message.
    pub fn write_json(&self, out: &mut impl fmt::Write) -> fmt::Result {
        fn write_option(out: &mut impl fmt::Write, value: Option<i32>) -> fmt::Result {
            match value {
                Some(value) => write!(out, "{}", value),
                None => fmt::Write::write_str(out, "null"),
            }
        }

        write!(
            out,
            "{{\"type\":\"tracking\",\"mode\":\"{}\",\"satellite\":\"{}\",\"phase\":\"{}\",\"pass\":{},\"remaining_ms\":{},\"azimuth_tenths\":{},\"elevation_tenths\":{},\"target_azimuth_tenths\":",
            self.mode.as_str(),
            if self.mode.is_autonomous() {
                SIMULATED_SATELLITE
            } else {
                "manual"
            },
            self.phase.as_str(),
            // Convert before adding: every u32 index has a valid one-based
            // display number, including u32::MAX. Formatting must not panic.
            u64::from(self.pass_index) + 1,
            self.phase_remaining_ms,
            self.azimuth_tenths,
            self.elevation_tenths,
        )?;
        write_option(out, self.target_azimuth_tenths)?;
        fmt::Write::write_str(out, ",\"target_elevation_tenths\":")?;
        write_option(out, self.target_elevation_tenths)?;
        write!(
            out,
            ",\"control_state\":\"{}\"}}",
            self.control_state.as_str()
        )
    }
}
