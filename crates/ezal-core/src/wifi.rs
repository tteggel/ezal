//! # WiFi station credentials (pure, host-testable)
//!
//! The Pico 2 W joins an access point in **station (STA) mode** using an SSID
//! and password that [`build.rs`] bakes into the firmware from `.env` at
//! compile time (there is no filesystem on the chip to read them from at
//! runtime). This module is the *pure* half of that story: it holds the
//! [`Credentials`] the build supplies and the rules that decide whether they
//! are join-able, with no dependency on the radio or the HAL. The bus-facing
//! half — powering the CYW43439 and issuing the join — lives in the firmware
//! crate's `wifi` module, which calls [`Credentials::validate`] as the first
//! step of its power-on self-test.
//!
//! Keeping the rules here means they are checked with a plain `cargo test` on
//! the host, and — more importantly — that a mistyped `.env` (an empty SSID,
//! a five-character password) is caught with a clear message at boot rather
//! than as an opaque `JoinError` from deep inside the driver.
//!
//! The constraints are the 802.11 / WPA ones:
//!
//! * an SSID is 1–32 octets ([`SSID_MAX_LEN`]);
//! * a WPA/WPA2/WPA3 passphrase is 8–63 octets
//!   ([`PSK_MIN_LEN`]–[`PSK_MAX_LEN`]);
//! * an empty password selects an **open** (unencrypted) network.
//!
//! (The 64-hex-character "raw PSK" form that some stacks accept in place of a
//! passphrase is deliberately not modelled — ezal takes a passphrase.)
//!
//! [`build.rs`]: https://github.com/tteggel/ezal/blob/main/crates/ezal-firmware/build.rs

/// Maximum length of an SSID, in octets (802.11). An SSID must be non-empty.
pub const SSID_MAX_LEN: usize = 32;

/// Minimum length of a WPA/WPA2/WPA3 passphrase, in octets.
pub const PSK_MIN_LEN: usize = 8;

/// Maximum length of a WPA/WPA2/WPA3 passphrase, in octets.
pub const PSK_MAX_LEN: usize = 63;

/// The kind of network a valid set of [`Credentials`] describes — i.e. what
/// the firmware should hand the driver when it joins.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Security {
    /// No password: an open, unencrypted network (join with no passphrase).
    Open,
    /// A WPA/WPA2/WPA3 network protected by the [`Credentials::password`]
    /// passphrase.
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
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Credentials<'a> {
    /// The network name (1–[`SSID_MAX_LEN`] octets).
    pub ssid: &'a str,
    /// The passphrase (8–[`PSK_MAX_LEN`] octets), or `""` for an open network.
    pub password: &'a str,
}

impl Credentials<'_> {
    /// Check the credentials against the 802.11 / WPA length rules.
    ///
    /// On success, returns the [`Security`] the firmware should join with:
    /// [`Security::Open`] when [`password`](Credentials::password) is empty,
    /// [`Security::Protected`] otherwise. On failure, returns the specific
    /// [`CredentialError`] — the firmware's WiFi POST turns this into a loud
    /// boot-time log line instead of powering the radio to no purpose.
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
