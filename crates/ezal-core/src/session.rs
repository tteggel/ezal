//! Manual-session timing and operator-intent admission, independent of transport.

use crate::drive::{DriveCommand, MOVEMENT_LEASE_MS, OUTPUT_MIN_ACTIVE_MS};

/// How often the firmware sends a complete telemetry observation to
/// each dashboard connection. Feedback sampling has its own faster period,
/// [`crate::feedback::FEEDBACK_PERIOD_MS`].
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

/// Per-WebSocket freshness and operator-intent admission, independent of
/// controller ownership. This is a bounded age check, not authentication.
///
/// Sending a token with each complete telemetry observation ties movement to
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
