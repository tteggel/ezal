//! Pure SNTP packet validation and UTC anchored to an injected monotonic clock.
//!
//! Neither the packet codec nor the clock reads time or performs I/O. UTC is
//! for orbital time only: movement leases must continue using uptime. SNTP is
//! unauthenticated; the error estimate assumes an honest server/network and
//! does not protect against an attacker changing the time.

/// Basic NTP packet length, excluding optional extensions.
pub const PACKET_LEN: usize = 48;
/// Stop predicting after six hours without a successful synchronization.
pub const MAX_SYNC_AGE_MS: u64 = 6 * 60 * 60 * 1000;
const NTP_UNIX_OFFSET: u64 = 2_208_988_800;
const MIN_UNIX_MS: u64 = 1_577_836_800_000; // 2020-01-01
const MAX_UNIX_MS: u64 = 4_102_444_800_000; // 2100-01-01

/// A validated SNTP estimate at receipt, with a conservative network/server
/// error estimate. Local oscillator drift is added by the execution clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeSample {
    unix_ms: u64,
    uncertainty_ms: u64,
    leap_pending: bool,
}

impl TimeSample {
    /// UTC estimate at the monotonic receipt instant.
    pub const fn unix_ms(self) -> u64 {
        self.unix_ms
    }
    /// Network asymmetry, advertised root distance/precision and quantization.
    pub const fn uncertainty_ms(self) -> u64 {
        self.uncertainty_ms
    }
    /// An announced leap requires a new sample before autonomous execution.
    pub const fn leap_pending(self) -> bool {
        self.leap_pending
    }
}

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

/// Validate a server reply and estimate Unix milliseconds at receipt, with its
/// uncertainty. The caller also checks the source IP/port. Server processing
/// time is removed from RTT before estimating the return journey
/// (symmetric-delay assumption).
/// Root delay is signed 16.16 seconds; negative values cannot reduce the bound.
/// Dispersion is unsigned 16.16 seconds. Round upwards throughout.
pub fn reply_sample(
    packet: &[u8],
    cookie: [u8; 8],
    round_trip_ms: u64,
) -> Result<TimeSample, Error> {
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
    let network_ms = round_trip_ms.saturating_sub(processing_ms);
    let unix_ms = transmit_ms + network_ms / 2;
    if !(MIN_UNIX_MS..MAX_UNIX_MS).contains(&unix_ms) {
        return Err(Error::Time);
    }
    let root_delay = i32::from_be_bytes(packet[4..8].try_into().unwrap()).max(0) as u64;
    let dispersion = u32::from_be_bytes(packet[8..12].try_into().unwrap()) as u64;
    let precision = packet[3] as i8;
    // Absurd precision is an invalid clock, not a shift/overflow opportunity.
    let precision_ms = match precision {
        ..=-10 => 1,
        -9..=-1 => 1000u64.div_ceil(1 << -precision),
        0..=20 => 1000u64 << precision,
        _ => return Err(Error::Time),
    };
    Ok(TimeSample {
        unix_ms,
        uncertainty_ms: network_ms.div_ceil(2)
            + (root_delay * 1000).div_ceil(2 * 65_536)
            + (dispersion * 1000).div_ceil(65_536)
            + precision_ms
            + 2,
        leap_pending: packet[0] >> 6 != 0,
    })
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
    anchor: Option<UtcAnchor>,
    revision: u64,
    failed: bool,
}

/// Immutable, revisioned UTC mapping. A new synchronization never edits a copy
/// already owned by an executing pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UtcAnchor {
    monotonic_ms: u64,
    revision: u64,
    sample: TimeSample,
}

impl UtcAnchor {
    /// Monotonic receipt instant, in the same domain as actuator deadlines.
    pub const fn monotonic_ms(self) -> u64 {
        self.monotonic_ms
    }
    /// Never-reused boot-local synchronization revision.
    pub const fn revision(self) -> u64 {
        self.revision
    }
    /// The quantified sample received at `monotonic_ms`.
    pub const fn sample(self) -> TimeSample {
        self.sample
    }
    /// Fractional UTC mapping; never truncates to whole seconds internally.
    pub fn unix_ms(self, monotonic_ms: u64) -> Option<u64> {
        let elapsed = monotonic_ms.checked_sub(self.monotonic_ms)?;
        let unix = self.sample.unix_ms.checked_add(elapsed)?;
        (unix < MAX_UNIX_MS).then_some(unix)
    }
    /// Convert an absolute UTC deadline using this exact anchor, with checked
    /// arithmetic in either direction. No live clock or integer seconds involved.
    pub fn monotonic_at(self, unix_ms: u64) -> Option<u64> {
        if !(MIN_UNIX_MS..MAX_UNIX_MS).contains(&unix_ms) {
            return None;
        }
        if unix_ms >= self.sample.unix_ms {
            self.monotonic_ms.checked_add(unix_ms - self.sample.unix_ms)
        } else {
            self.monotonic_ms.checked_sub(self.sample.unix_ms - unix_ms)
        }
    }
}

/// A reversed uptime or an exhausted revision counter disarms the clock until
/// reboot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockDisarmed;

impl UtcClock {
    /// An unsynchronized clock never fabricates a UTC timestamp.
    pub const fn new() -> Self {
        Self {
            anchor: None,
            revision: 0,
            failed: false,
        }
    }

    /// Accept a quantified sample. Failed network refreshes must not call this
    /// method: they neither renew source age nor clear the previous good sample.
    /// Wall time may step; uptime remains untouched.
    pub fn synchronize(
        &mut self,
        monotonic_ms: u64,
        sample: TimeSample,
    ) -> Result<(), ClockDisarmed> {
        if self.failed
            || self
                .anchor
                .is_some_and(|anchor| monotonic_ms < anchor.monotonic_ms)
            || self.revision == u64::MAX
        {
            self.anchor = None;
            self.failed = true;
            return Err(ClockDisarmed);
        }
        self.revision += 1;
        self.anchor = Some(UtcAnchor {
            monotonic_ms,
            revision: self.revision,
            sample,
        });
        Ok(())
    }

    /// Quantified anchor for a coherent source snapshot.
    pub const fn anchor(self) -> Option<UtcAnchor> {
        self.anchor
    }

    /// Milliseconds since the last good sample, even when it has expired.
    pub fn age_ms(self, monotonic_ms: u64) -> Option<u64> {
        monotonic_ms.checked_sub(self.anchor?.monotonic_ms)
    }

    /// Current UTC, only while the sample is fresh and uptime has not reversed.
    pub fn unix_ms(self, monotonic_ms: u64) -> Option<u64> {
        let age = self.age_ms(monotonic_ms)?;
        if age >= MAX_SYNC_AGE_MS {
            return None;
        }
        self.anchor?.unix_ms(monotonic_ms)
    }
}
