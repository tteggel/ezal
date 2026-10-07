//! WiFi station credentials and the reconnect schedule.
//!
//! The Pico 2 W joins an access point in **station (STA) mode** using an SSID
//! and password that [`build.rs`] bakes into the firmware from `.env` at
//! compile time (there is no filesystem on the chip to read them from at
//! runtime). This module is the *pure* half of that story: it holds the
//! [`Credentials`] the build supplies and the rules that decide whether they
//! are join-able, with no dependency on the radio or the HAL. The bus-facing
//! half — powering the CYW43439 and issuing the join — lives in the firmware
//! crate's `wifi` module. Its supervisor validates credentials once and
//! uses [`ConnectionRetries`] to schedule retries after association or DHCP fails.
//!
//! Keeping the rules here means they are checked by `./scripts/test-host.sh`,
//! and a mistyped `.env` (an empty SSID or five-character password) is caught
//! with a clear message at boot rather
//! than as an opaque `JoinError` from deep inside the driver.
//!
//! ezal accepts these credential lengths:
//!
//! * an SSID is 1–32 octets ([`SSID_MAX_LEN`]);
//! * a protected-network passphrase is 8–63 octets
//!   ([`PSK_MIN_LEN`]–[`PSK_MAX_LEN`]);
//! * an empty password selects an **open** (unencrypted) network.
//!
//! (The 64-hex-character "raw PSK" form that some stacks accept in place of a
//! passphrase is deliberately not modelled — ezal takes a passphrase.)
//! The firmware currently requests WPA3/SAE for protected networks; this
//! module validates strings and does not negotiate a security protocol.
//!
//! [`build.rs`]: https://github.com/tteggel/ezal/blob/main/crates/ezal-firmware/build.rs

/// Maximum length of an SSID, in octets (802.11). An SSID must be non-empty.
pub const SSID_MAX_LEN: usize = 32;

/// Minimum protected-network passphrase length accepted by ezal, in octets.
pub const PSK_MIN_LEN: usize = 8;

/// Maximum protected-network passphrase length accepted by ezal, in octets.
pub const PSK_MAX_LEN: usize = 63;

/// The kind of network a valid set of [`Credentials`] describes — i.e. what
/// the firmware should hand the driver when it joins.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Security {
    /// No password: an open, unencrypted network (join with no passphrase).
    Open,
    /// A network protected by the [`Credentials::password`] passphrase.
    /// The hardware adapter chooses the authentication protocol.
    Protected,
}

/// Why a set of [`Credentials`] is not join-able. Each variant carries the
/// offending length where one applies, and [`CredentialError::message`] gives
/// a short static description suitable for a boot-time log line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CredentialError {
    /// The SSID was empty — nothing to join. Usually means `.env` is missing
    /// or `EZAL_WIFI_SSID` was left blank.
    SsidEmpty,
    /// The SSID was longer than [`SSID_MAX_LEN`] octets.
    SsidTooLong {
        /// The offending SSID length, in octets.
        len: usize,
    },
    /// A non-empty password was shorter than [`PSK_MIN_LEN`] octets. (An
    /// *empty* password is not an error — it selects an open network.)
    PasswordTooShort {
        /// The offending password length, in octets.
        len: usize,
    },
    /// The password was longer than [`PSK_MAX_LEN`] octets.
    PasswordTooLong {
        /// The offending password length, in octets.
        len: usize,
    },
}

impl CredentialError {
    /// A short, static, human-readable description — no allocation, so the
    /// firmware can log it directly over defmt.
    pub const fn message(self) -> &'static str {
        match self {
            CredentialError::SsidEmpty => "SSID is empty (set EZAL_WIFI_SSID in .env)",
            CredentialError::SsidTooLong { .. } => "SSID is longer than 32 octets",
            CredentialError::PasswordTooShort { .. } => {
                "WPA passphrase is shorter than 8 octets (use 8-63, or leave blank for open)"
            }
            CredentialError::PasswordTooLong { .. } => "WPA passphrase is longer than 63 octets",
        }
    }
}

/// Station-mode WiFi credentials: an access point's SSID and its passphrase.
///
/// These borrow the compile-time strings `build.rs` injects, so a
/// `Credentials` is normally built right at the call site:
///
/// ```
/// use ezal_core::wifi::{Credentials, Security};
///
/// let creds = Credentials { ssid: "my-network", password: "hunter2!!" };
/// assert_eq!(creds.validate(), Ok(Security::Protected));
/// ```
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Credentials<'a> {
    /// The network name (1–[`SSID_MAX_LEN`] octets).
    pub ssid: &'a str,
    /// The passphrase (8–[`PSK_MAX_LEN`] octets), or `""` for an open network.
    pub password: &'a str,
}

// Credentials are configuration, but their password must not become a log
// field when a caller prints that configuration while diagnosing startup.
impl core::fmt::Debug for Credentials<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Credentials")
            .field("ssid", &self.ssid)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

impl Credentials<'_> {
    /// Check the credentials against ezal's accepted SSID/passphrase lengths.
    ///
    /// On success, returns the [`Security`] the firmware should join with:
    /// [`Security::Open`] when [`password`](Credentials::password) is empty,
    /// [`Security::Protected`] otherwise. On failure, returns the specific
    /// [`CredentialError`] — the firmware logs this before attempting
    /// association. Radio initialization has already completed at that point.
    ///
    /// Lengths are measured in octets (bytes), matching how the SSID and
    /// passphrase go out on the air.
    pub const fn validate(&self) -> Result<Security, CredentialError> {
        let ssid_len = self.ssid.len();
        if ssid_len == 0 {
            return Err(CredentialError::SsidEmpty);
        }
        if ssid_len > SSID_MAX_LEN {
            return Err(CredentialError::SsidTooLong { len: ssid_len });
        }

        let pw_len = self.password.len();
        if pw_len == 0 {
            // No password → open network. That's a valid choice, not an error.
            return Ok(Security::Open);
        }
        if pw_len < PSK_MIN_LEN {
            return Err(CredentialError::PasswordTooShort { len: pw_len });
        }
        if pw_len > PSK_MAX_LEN {
            return Err(CredentialError::PasswordTooLong { len: pw_len });
        }
        Ok(Security::Protected)
    }
}

/// The reconnect supervisor's retry schedule.
///
/// A group starts with a short join deadline to recover promptly from the
/// CYW43's stale initial WPA3 handshake. Two more timed-out joins may retry
/// after hardware cleanup alone. Other failures, or three timed-out joins,
/// end the group and use exponential backoff. Only usable networking resets
/// that backoff; association without DHCP is not recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectionRetries {
    join_attempt: u8,
    backoff_secs: u64,
}

impl ConnectionRetries {
    /// Start with the initial join deadline and one second of failure backoff.
    pub const fn new() -> Self {
        Self {
            join_attempt: 1,
            backoff_secs: 1,
        }
    }

    /// Deadline for the next bounded association attempt.
    pub const fn association_timeout_ms(&self) -> u64 {
        if self.join_attempt == 1 {
            2_750
        } else {
            3_500
        }
    }

    /// Whether a timed-out join may retry after cleanup without backoff.
    /// Cleanup itself must succeed before that next join can start.
    pub fn retry_timed_out_join(&mut self) -> bool {
        if self.join_attempt < 3 {
            self.join_attempt += 1;
            true
        } else {
            false
        }
    }

    /// Finish this group and return its delay before another first attempt.
    pub fn backoff_secs(&mut self) -> u64 {
        self.join_attempt = 1;
        let delay = self.backoff_secs;
        self.backoff_secs = (delay * 2).min(30);
        delay
    }

    /// A current station link and usable address restore the initial schedule.
    pub fn connected(&mut self) {
        *self = Self::new();
    }
}

impl Default for ConnectionRetries {
    fn default() -> Self {
        Self::new()
    }
}
