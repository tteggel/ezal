//! Pure SNTP packet validation and UTC anchored to an injected monotonic clock.
//!
//! Neither the packet codec nor the clock reads time or performs I/O. UTC is
//! for orbit predictions only: movement leases must continue using uptime.

/// Basic NTP packet length, excluding optional extensions.
pub const PACKET_LEN: usize = 48;
/// Stop predicting after six hours without a successful synchronization.
pub const MAX_SYNC_AGE_MS: u64 = 6 * 60 * 60 * 1000;
const NTP_UNIX_OFFSET: u64 = 2_208_988_800;
const MIN_UNIX_MS: u64 = 1_577_836_800_000; // 2020-01-01
const MAX_UNIX_MS: u64 = 4_102_444_800_000; // 2100-01-01

/// Reasons a reply cannot establish time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Truncated packet or unsupported server mode/version.
    Packet,
    /// Unsynchronized server or kiss-of-death reply.
    Server,
    /// Reply does not echo this request's random transmit timestamp.
    Origin,
    /// Invalid server timestamps or excessive network delay.
    Time,
}

/// Build a version-4 client request. A random, nonzero cookie is echoed in the
/// server's origin field, allowing cold-start synchronization without UTC.
pub fn request(cookie: [u8; 8]) -> [u8; PACKET_LEN] {
    let mut packet = [0; PACKET_LEN];
    packet[0] = 0x23;
    packet[40..48].copy_from_slice(&cookie);
    packet
}

/// Validate a server reply and estimate Unix milliseconds at receipt. The
/// caller also checks the source IP/port. Server processing time is removed
/// from RTT before estimating the return journey (symmetric-delay assumption).
pub fn reply(packet: &[u8], cookie: [u8; 8], round_trip_ms: u64) -> Result<u64, Error> {
    if packet.len() < PACKET_LEN || !matches!((packet[0] >> 3) & 7, 3 | 4) || packet[0] & 7 != 4 {
        return Err(Error::Packet);
    }
    if packet[0] >> 6 == 3 || !(1..=15).contains(&packet[1]) {
        return Err(Error::Server);
    }
    if cookie == [0; 8] || packet[24..32] != cookie {
        return Err(Error::Origin);
    }
    let receive_ms = timestamp_ms(&packet[32..40])?;
    let transmit_ms = timestamp_ms(&packet[40..48])?;
    let processing_ms = transmit_ms.checked_sub(receive_ms).ok_or(Error::Time)?;
    // Permit one millisecond of timestamp quantization.
    if round_trip_ms > 2000 || processing_ms > round_trip_ms + 1 {
        return Err(Error::Time);
    }
    Ok(transmit_ms + round_trip_ms.saturating_sub(processing_ms) / 2)
}

fn timestamp_ms(bytes: &[u8]) -> Result<u64, Error> {
    if bytes == [0; 8] {
        return Err(Error::Time);
    }
    let seconds = u32::from_be_bytes(bytes[..4].try_into().map_err(|_| Error::Packet)?) as u64;
    let fraction = u32::from_be_bytes(bytes[4..].try_into().map_err(|_| Error::Packet)?) as u64;
    // Unfold the 2036 rollover into era 1. The explicit supported date range
    // makes the otherwise ambiguous 32-bit wire timestamp unambiguous.
    let seconds = if seconds < NTP_UNIX_OFFSET {
        seconds + (1u64 << 32)
    } else {
        seconds
    };
    let unix_ms = (seconds - NTP_UNIX_OFFSET) * 1000 + ((fraction * 1000) >> 32);
    if !(MIN_UNIX_MS..MAX_UNIX_MS).contains(&unix_ms) {
        return Err(Error::Time);
    }
    Ok(unix_ms)
}

/// A successful UTC sample and the monotonic instant at which it was received.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UtcClock {
    anchor: Option<(u64, u64)>,
}

impl UtcClock {
    /// An unsynchronized clock never fabricates a UTC timestamp.
    pub const fn new() -> Self {
        Self { anchor: None }
    }

    /// Apply a validated sample. Wall time may step; uptime remains untouched.
    pub fn synchronize(&mut self, monotonic_ms: u64, unix_ms: u64) {
        self.anchor = Some((monotonic_ms, unix_ms));
    }

    /// Milliseconds since the last good sample, even when it has expired.
    pub fn age_ms(self, monotonic_ms: u64) -> Option<u64> {
        monotonic_ms.checked_sub(self.anchor?.0)
    }

    /// Current UTC, only while the sample is fresh and uptime has not reversed.
    pub fn unix_ms(self, monotonic_ms: u64) -> Option<u64> {
        let age = self.age_ms(monotonic_ms)?;
        if age >= MAX_SYNC_AGE_MS {
            return None;
        }
        self.anchor?.1.checked_add(age)
    }
}
