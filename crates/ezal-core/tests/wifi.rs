//! Host tests for WiFi credentials and the network supervisor's retry schedule.
//! They pin the 802.11 / WPA length boundaries: the empty SSID, the 32-octet
//! SSID, the 8- and 63-octet passphrase edges, and the empty password that
//! selects an open network.

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
        Err(CredentialError::SsidTooLong)
    );
}

/// The passphrase boundaries: 8 and 63 octets are the inclusive edges; 7 is
/// too short and 64 is too long.
#[test]
fn password_length_boundaries_are_8_and_63_octets() {
    let ssid = "ground-station";
    let check = |pw: &str| Credentials { ssid, password: pw }.validate();

    assert_eq!(check(&n_chars(7)), Err(CredentialError::PasswordTooShort));
    assert_eq!(check(&n_chars(8)), Ok(Security::Protected));
    assert_eq!(check(&n_chars(63)), Ok(Security::Protected));
    assert_eq!(check(&n_chars(64)), Err(CredentialError::PasswordTooLong));
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
        Err(CredentialError::SsidTooLong)
    );
}

/// Every error carries a non-empty static message the firmware can log.
#[test]
fn every_error_has_a_message() {
    for err in [
        CredentialError::SsidEmpty,
        CredentialError::SsidTooLong,
        CredentialError::PasswordTooShort,
        CredentialError::PasswordTooLong,
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

/// Debugging startup configuration must never disclose its baked-in secret.
#[test]
fn debug_redacts_the_password() {
    let creds = Credentials {
        ssid: "ground-station",
        password: "secret-test-passphrase",
    };
    for rendered in [format!("{creds:?}"), format!("{creds:#?}")] {
        assert!(rendered.contains(creds.ssid));
        assert!(rendered.contains("[REDACTED]"));
        assert!(!rendered.contains(creds.password));
    }
}

#[test]
fn timed_out_joins_get_two_prompt_retries_before_backoff() {
    let mut retries = ConnectionRetries::new();
    assert_eq!(retries.association_timeout_ms(), 2_750);
    assert!(retries.retry_timed_out_join());
    assert_eq!(retries.association_timeout_ms(), 3_500);
    assert!(retries.retry_timed_out_join());
    assert_eq!(retries.association_timeout_ms(), 3_500);
    assert!(!retries.retry_timed_out_join());
    assert_eq!(retries.backoff_secs(), 1);

    // Backoff begins a fresh group, including its shorter initial deadline.
    assert_eq!(retries.association_timeout_ms(), 2_750);
    assert!(retries.retry_timed_out_join());
}

#[test]
fn cleanup_join_or_dhcp_failure_ends_the_prompt_retry_group() {
    let mut retries = ConnectionRetries::new();
    assert!(retries.retry_timed_out_join());
    assert_eq!(retries.association_timeout_ms(), 3_500);

    // A cleanup failure or a rejected join backs off instead of spending the
    // remaining prompt retry. A join without DHCP uses the same failure path.
    assert_eq!(retries.backoff_secs(), 1);
    assert_eq!(retries.association_timeout_ms(), 2_750);
    assert!(retries.retry_timed_out_join());
    assert!(retries.retry_timed_out_join());
    assert!(!retries.retry_timed_out_join());
    assert_eq!(retries.backoff_secs(), 2);
}

#[test]
fn repeated_failures_back_off_to_thirty_seconds_until_network_recovers() {
    let mut retries = ConnectionRetries::new();
    for expected in [1, 2, 4, 8, 16, 30, 30, 30] {
        assert_eq!(retries.backoff_secs(), expected);
    }
    assert!(retries.retry_timed_out_join());

    // The supervisor only calls this after both association and DHCP succeed.
    retries.connected();
    assert_eq!(retries.association_timeout_ms(), 2_750);
    assert_eq!(retries.backoff_secs(), 1);
    assert_eq!(retries.backoff_secs(), 2);
}
