use ezal_core::ntp::{self, Error, TimeSample, UtcClock, MAX_SYNC_AGE_MS};

const COOKIE: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
const NOW: u64 = 1_791_385_200_000;

fn timestamp(unix_ms: u64) -> [u8; 8] {
    let seconds = (unix_ms / 1000 + 2_208_988_800) as u32;
    let fraction = (((unix_ms % 1000) << 32) / 1000) as u32;
    let mut bytes = [0; 8];
    bytes[..4].copy_from_slice(&seconds.to_be_bytes());
    bytes[4..].copy_from_slice(&fraction.to_be_bytes());
    bytes
}

fn response(receive: u64, transmit: u64) -> [u8; 48] {
    let mut packet = [0; 48];
    packet[0] = 0x24;
    packet[1] = 2;
    packet[24..32].copy_from_slice(&COOKIE);
    packet[32..40].copy_from_slice(&timestamp(receive));
    packet[40..48].copy_from_slice(&timestamp(transmit));
    packet
}

fn sample(unix_ms: u64) -> TimeSample {
    ntp::reply_sample(&response(unix_ms, unix_ms), COOKIE, 0).unwrap()
}

#[test]
fn cold_start_request_and_response_account_for_network_and_server_delay() {
    let request = ntp::request(COOKIE);
    assert_eq!(request[0], 0x23);
    assert_eq!(&request[40..], &COOKIE);
    // 100 ms RTT, 20 ms spent at server -> 40 ms estimated return journey.
    let result = ntp::reply_sample(&response(NOW, NOW + 20), COOKIE, 100)
        .unwrap()
        .unix_ms();
    assert!(result.abs_diff(NOW + 60) <= 1);
}

#[test]
fn rejects_wrong_origin_unsynchronized_kiss_of_death_and_invalid_packets() {
    let packet = response(NOW, NOW);
    assert_eq!(
        ntp::reply_sample(&packet[..47], COOKIE, 20),
        Err(Error::Packet)
    );
    assert_eq!(ntp::reply_sample(&packet, [9; 8], 20), Err(Error::Origin));
    for byte in [0x23, 0x25, 0x14] {
        let mut bad = packet;
        bad[0] = byte;
        assert_eq!(ntp::reply_sample(&bad, COOKIE, 20), Err(Error::Packet));
    }
    let mut bad = packet;
    bad[0] |= 0xc0;
    assert_eq!(ntp::reply_sample(&bad, COOKIE, 20), Err(Error::Server));
    for stratum in [0, 16, 255] {
        let mut bad = packet;
        bad[1] = stratum;
        assert_eq!(ntp::reply_sample(&bad, COOKIE, 20), Err(Error::Server));
    }
}

#[test]
fn rejects_zero_reversed_implausible_and_delayed_timestamps() {
    let mut packet = response(NOW, NOW);
    packet[40..48].fill(0);
    assert_eq!(ntp::reply_sample(&packet, COOKIE, 20), Err(Error::Time));
    assert_eq!(
        ntp::reply_sample(&response(NOW + 100, NOW), COOKIE, 20),
        Err(Error::Time)
    );
    assert_eq!(
        ntp::reply_sample(&response(NOW, NOW + 100), COOKIE, 20),
        Err(Error::Time)
    );
    assert_eq!(
        ntp::reply_sample(&response(NOW, NOW), COOKIE, 2001),
        Err(Error::Time)
    );
    assert_eq!(
        ntp::reply_sample(&response(1_000_000, 1_000_000), COOKIE, 20),
        Err(Error::Time)
    );
}

#[test]
fn unfolds_ntp_era_rollover_in_2036() {
    let rollover = 2_085_978_496_000;
    let result = ntp::reply_sample(&response(rollover - 10, rollover + 10), COOKIE, 100)
        .unwrap()
        .unix_ms();
    assert!(result.abs_diff(rollover + 50) <= 1);
}

#[test]
fn utc_uses_monotonic_elapsed_time_expires_and_accepts_corrections() {
    let mut clock = UtcClock::new();
    assert_eq!(clock.unix_ms(0), None);
    clock.synchronize(10_000, sample(NOW)).unwrap();
    assert_eq!(clock.unix_ms(11_234), Some(NOW + 1234));
    assert_eq!(clock.unix_ms(9999), None);
    assert_eq!(clock.unix_ms(10_000 + MAX_SYNC_AGE_MS), None);
    clock.synchronize(20_000, sample(NOW - 2000)).unwrap();
    assert_eq!(clock.unix_ms(21_000), Some(NOW - 1000));
    assert_eq!(clock.age_ms(21_000), Some(1000));
}
