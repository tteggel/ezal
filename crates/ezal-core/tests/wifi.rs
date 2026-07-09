//! Host unit tests for the pure WiFi credential logic.
//!
//! These run on the host with a plain `cargo test` (see
//! `scripts/test-host.sh`). They pin down the boundaries of the 802.11 / WPA
//! length rules — the empty SSID, the 32-octet SSID, the 8-and-63-octet
//! passphrase edges, and the "empty password means open network" carve-out —
//! so a mistyped `.env` is caught here, in seconds, with a clear message
//! rather than as an opaque `JoinError` from inside the radio driver at boot.

use ezal_core::wifi::*;

/// Build a placeholder string of exactly `n` ASCII octets.
fn n_chars(n: usize) -> String {
    "a".repeat(n)
}

/// A normal WPA network: non-empty SSID, 8–63 octet passphrase → protected.
#[test]
fn typical_wpa_credentials_are_protected() {
    let creds = Credentials {
        ssid: "ground-station",
        password: "hunter2!!",
    };
    assert_eq!(creds.validate(), Ok(Security::Protected));
}

/// An empty password is the open-network signal, not an error.
#[test]
fn empty_password_selects_open_network() {
    let creds = Credentials {
        ssid: "cafe-wifi",
        password: "",
    };
    assert_eq!(creds.validate(), Ok(Security::Open));
}

/// An empty SSID is never join-able, and that check comes first — even a
/// present password can't rescue it (this is the "no .env" boot case).
#[test]
fn empty_ssid_is_rejected_before_the_password() {
    assert_eq!(
        Credentials {
            ssid: "",
            password: ""
        }
        .validate(),
        Err(CredentialError::SsidEmpty)
    );
    assert_eq!(
        Credentials {
            ssid: "",
            password: "a-perfectly-fine-password"
        }
        .validate(),
        Err(CredentialError::SsidEmpty)
    );
}

/// The SSID boundary: 32 octets is the longest allowed; 33 is too long.
#[test]
fn ssid_length_boundary_is_32_octets() {
    assert_eq!(SSID_MAX_LEN, 32);

    let ssid_32 = n_chars(32);
    assert_eq!(
        Credentials {
            ssid: &ssid_32,
            password: ""
        }
        .validate(),
        Ok(Security::Open)
    );

    let ssid_33 = n_chars(33);
    assert_eq!(
        Credentials {
            ssid: &ssid_33,
            password: ""
        }
        .validate(),
        Err(CredentialError::SsidTooLong { len: 33 })
    );
}

/// The passphrase boundaries: 8 and 63 octets are the inclusive edges; 7 is
/// too short and 64 is too long.
#[test]
fn password_length_boundaries_are_8_and_63_octets() {
    assert_eq!((PSK_MIN_LEN, PSK_MAX_LEN), (8, 63));

    let ssid = "ground-station";
    let check = |pw: &str| Credentials { ssid, password: pw }.validate();

    assert_eq!(
        check(&n_chars(7)),
        Err(CredentialError::PasswordTooShort { len: 7 })
    );
    assert_eq!(check(&n_chars(8)), Ok(Security::Protected));
    assert_eq!(check(&n_chars(63)), Ok(Security::Protected));
    assert_eq!(
        check(&n_chars(64)),
        Err(CredentialError::PasswordTooLong { len: 64 })
    );
}

/// SSID length is counted in octets, not `char`s: a multi-byte SSID that fits
/// in 32 *characters* but not 32 *bytes* must be rejected. "é" is 2 UTF-8
/// bytes, so 17 of them = 34 octets > 32.
#[test]
fn ssid_length_is_measured_in_octets_not_chars() {
    let ssid = "é".repeat(17); // 17 chars, 34 octets
    assert_eq!(ssid.chars().count(), 17);
    assert_eq!(ssid.len(), 34);
    assert_eq!(
        Credentials {
            ssid: &ssid,
            password: ""
        }
        .validate(),
        Err(CredentialError::SsidTooLong { len: 34 })
    );
}

/// Every error carries a non-empty static message the firmware can log.
#[test]
fn every_error_has_a_message() {
    for err in [
        CredentialError::SsidEmpty,
        CredentialError::SsidTooLong { len: 33 },
        CredentialError::PasswordTooShort { len: 7 },
        CredentialError::PasswordTooLong { len: 64 },
    ] {
        assert!(!err.message().is_empty());
    }
}

/// `validate` is a `const fn`, so it must be usable in a const context.
#[test]
fn validate_works_in_const_context() {
    const CREDS: Credentials = Credentials {
        ssid: "compile-time",
        password: "baked-in-at-build",
    };
    const RESULT: Result<Security, CredentialError> = CREDS.validate();
    assert_eq!(RESULT, Ok(Security::Protected));
}
