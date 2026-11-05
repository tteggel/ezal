use ezal_core::ntp::{self, ClockDisarmed, UtcClock};
use ezal_core::tracking::{
    execution_window, ExecutionClock, PlanningConfig, TimingError, TimingPolicy,
};

const UTC: u64 = 1_791_385_200_125;
const COOKIE: [u8; 8] = [1; 8];

fn timestamp(utc: u64) -> [u8; 8] {
    let mut bytes = [0; 8];
    bytes[..4].copy_from_slice(&((utc / 1000 + 2_208_988_800) as u32).to_be_bytes());
    bytes[4..].copy_from_slice(&((((utc % 1000) << 32).div_ceil(1000)) as u32).to_be_bytes());
    bytes
}

fn packet(utc: u64) -> [u8; 48] {
    let mut packet = [0; 48];
    packet[0] = 0x24;
    packet[1] = 2;
    packet[3] = (-20i8) as u8;
    packet[24..32].copy_from_slice(&COOKIE);
    packet[32..40].copy_from_slice(&timestamp(utc));
    packet[40..48].copy_from_slice(&timestamp(utc));
    packet
}

fn clock(at: u64, utc: u64) -> UtcClock {
    let mut clock = UtcClock::new();
    clock
        .synchronize(at, ntp::reply_sample(&packet(utc), COOKIE, 0).unwrap())
        .unwrap();
    clock
}

#[test]
fn sntp_uncertainty_includes_asymmetry_root_delay_dispersion_and_precision() {
    let mut reply = packet(UTC);
    reply[40..48].copy_from_slice(&timestamp(UTC + 20));
    reply[4..8].copy_from_slice(&65_536i32.to_be_bytes()); // one second round trip to root
    reply[8..12].copy_from_slice(&32_768u32.to_be_bytes()); // half second dispersion
    let sample = ntp::reply_sample(&reply, COOKIE, 100).unwrap();
    assert_eq!(sample.unix_ms(), UTC + 60);
    assert_eq!(sample.uncertainty_ms(), 40 + 500 + 500 + 1 + 2);
    reply[4..8].copy_from_slice(&(-65_536i32).to_be_bytes());
    assert_eq!(
        ntp::reply_sample(&reply, COOKIE, 100)
            .unwrap()
            .uncertainty_ms(),
        543
    );
    reply[3] = 127;
    assert_eq!(
        ntp::reply_sample(&reply, COOKIE, 100),
        Err(ntp::Error::Time)
    );
}

#[test]
fn fractional_mapping_preserves_original_anchor_across_positive_and_negative_corrections() {
    for correction in [-100i64, 100] {
        let mut live = clock(2000, UTC);
        let frozen = ExecutionClock::new(
            live.anchor().unwrap(),
            TimingPolicy::default(),
            &PlanningConfig::default(),
            152_000,
            2000,
        )
        .unwrap();
        assert_eq!(frozen.unix_s(2001).unwrap(), (UTC + 1) as f64 / 1000.0);
        let window =
            execution_window(live.anchor().unwrap(), UTC + 120_003, UTC + 150_007).unwrap();
        assert_eq!(window.aos_at_ms, 122_003);
        assert_eq!(window.duration_ms, 30_004);
        assert_eq!(window.aos_unix_s, (UTC + 120_003) as f64 / 1000.0);
        let sample =
            ntp::reply_sample(&packet((UTC as i64 + 1000 + correction) as u64), COOKIE, 0).unwrap();
        live.synchronize(3000, sample).unwrap();
        assert_eq!(frozen.validate(live, 3100), Ok(()));
        assert_eq!(frozen.unix_s(3100).unwrap(), (UTC + 1100) as f64 / 1000.0);
        assert_eq!(frozen.anchor().revision(), 1);
        assert_eq!(live.anchor().unwrap().revision(), 2);
        assert_eq!(frozen.validate(live, 152_001), Err(TimingError::Expired));
    }
}

#[test]
fn corrections_must_fit_remaining_error_through_los_and_never_ratchet_the_anchor() {
    let mut live = clock(2000, UTC);
    let frozen = ExecutionClock::new(
        live.anchor().unwrap(),
        TimingPolicy::default(),
        &PlanningConfig::default(),
        152_000,
        2000,
    )
    .unwrap();
    for (at, correction, expected) in [
        (3000, 100, Ok(())),
        (4000, 200, Ok(())),
        (5000, 300, Err(TimingError::Correction)),
    ] {
        live.synchronize(
            at,
            ntp::reply_sample(&packet(UTC + at - 2000 + correction), COOKIE, 0).unwrap(),
        )
        .unwrap();
        assert_eq!(frozen.validate(live, at), expected);
    }
    let mut early = clock(2000, UTC);
    early
        .synchronize(
            3000,
            ntp::reply_sample(&packet(UTC + 700), COOKIE, 0).unwrap(),
        )
        .unwrap();
    assert_eq!(frozen.validate(early, 3000), Err(TimingError::Correction));
    // A small correction plus a noisy new sample also exceeds the allowance.
    let mut noisy = clock(2000, UTC);
    noisy
        .synchronize(
            3000,
            ntp::reply_sample(&packet(UTC + 1000), COOKIE, 400).unwrap(),
        )
        .unwrap();
    assert_eq!(frozen.validate(noisy, 3000), Err(TimingError::Correction));
}

#[test]
fn full_horizon_uncertainty_and_source_age_are_checked_at_exclusive_boundaries() {
    let current = clock(2000, UTC);
    let anchor = current.anchor().unwrap();
    // 3 ms initial error + 100 ppm * 2,470 seconds = the 250 ms allowance.
    let frozen = ExecutionClock::new(
        anchor,
        TimingPolicy::default(),
        &PlanningConfig::default(),
        2_472_000,
        2000,
    )
    .unwrap();
    assert_eq!(frozen.admission_deadline_ms().unwrap(), 2_472_001);
    assert_eq!(
        ExecutionClock::new(
            anchor,
            TimingPolicy::default(),
            &PlanningConfig::default(),
            2_472_001,
            2000
        ),
        Err(TimingError::Uncertainty)
    );
    let policy = TimingPolicy {
        max_sample_age_ms: 1000,
        ..TimingPolicy::default()
    };
    assert!(ExecutionClock::new(anchor, policy, &PlanningConfig::default(), 2999, 2000).is_ok());
    assert_eq!(
        ExecutionClock::new(anchor, policy, &PlanningConfig::default(), 3000, 2000),
        Err(TimingError::Expired)
    );
    assert_eq!(
        current.anchor(),
        Some(anchor),
        "a failed refresh does not replace/renew a sample"
    );
}

#[test]
fn leap_pending_and_missing_clocks_cannot_authorize_execution() {
    for leap in [1, 2] {
        let mut reply = packet(UTC);
        reply[0] |= leap << 6;
        let mut live = UtcClock::new();
        live.synchronize(2000, ntp::reply_sample(&reply, COOKIE, 0).unwrap())
            .unwrap();
        assert_eq!(live.unix_ms(2100), Some(UTC + 100));
        assert_eq!(
            ExecutionClock::new(
                live.anchor().unwrap(),
                TimingPolicy::default(),
                &PlanningConfig::default(),
                3000,
                2000
            ),
            Err(TimingError::LeapPending)
        );
    }
    let live = clock(2000, UTC);
    let frozen = ExecutionClock::new(
        live.anchor().unwrap(),
        TimingPolicy::default(),
        &PlanningConfig::default(),
        3000,
        2000,
    )
    .unwrap();
    assert_eq!(
        frozen.validate(UtcClock::new(), 2100),
        Err(TimingError::Unavailable)
    );
}

#[test]
fn reversed_time_latches_and_deadline_conversions_never_wrap() {
    let mut live = clock(2000, UTC);
    let sample = ntp::reply_sample(&packet(UTC), COOKIE, 0).unwrap();
    assert_eq!(live.synchronize(1999, sample), Err(ClockDisarmed));
    assert_eq!(live.synchronize(3000, sample), Err(ClockDisarmed));
    assert!(live.anchor().is_none());
    let anchor = clock(2000, UTC).anchor().unwrap();
    assert_eq!(anchor.monotonic_at(UTC - 1999), Some(1));
    assert_eq!(anchor.monotonic_at(UTC - 2001), None);
    assert_eq!(
        execution_window(anchor, UTC + 1000, UTC + 999),
        Err(TimingError::Configuration)
    );
    assert_eq!(
        execution_window(anchor, UTC, u64::MAX),
        Err(TimingError::Configuration)
    );
    let anchor = clock(u64::MAX - 1000, UTC).anchor().unwrap();
    assert_eq!(anchor.monotonic_at(UTC + 1001), None);
    assert_eq!(anchor.unix_ms(u64::MAX), Some(UTC + 1000));
}

#[test]
fn clock_cannot_spend_alignment_reserve_or_accept_invalid_policy() {
    let anchor = clock(2000, UTC).anchor().unwrap();
    for policy in [
        TimingPolicy {
            drift_ppm: 0,
            ..TimingPolicy::default()
        },
        TimingPolicy {
            non_timing_uncertainty_deg: 1.0,
            ..TimingPolicy::default()
        },
        TimingPolicy {
            non_timing_uncertainty_deg: f32::NAN,
            ..TimingPolicy::default()
        },
    ] {
        assert_eq!(
            ExecutionClock::new(anchor, policy, &PlanningConfig::default(), 3000, 2000),
            Err(TimingError::Configuration)
        );
    }
    let config = PlanningConfig {
        sky_rate_bound_deg_per_s: f32::INFINITY,
        ..PlanningConfig::default()
    };
    assert_eq!(
        ExecutionClock::new(anchor, TimingPolicy::default(), &config, 3000, 2000),
        Err(TimingError::Configuration)
    );
}
