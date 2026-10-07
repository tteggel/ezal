//! NTP transport. Pure packet/clock policy lives in ezal-core; only this task
//! performs DNS/UDP I/O or samples Embassy uptime. Motion never uses UTC.

use core::cell::Cell;
use embassy_net::{
    dns::DnsQueryType,
    udp::{PacketMetadata, UdpSocket},
    IpEndpoint, Stack,
};
use embassy_rp::clocks::RoscRng;
use embassy_sync::blocking_mutex::{raw::CriticalSectionRawMutex, Mutex};
use embassy_time::{with_timeout, Duration, Instant, Timer};
use ezal_core::ntp::{self, UtcClock};

static CLOCK: Mutex<CriticalSectionRawMutex, Cell<UtcClock>> =
    Mutex::new(Cell::new(UtcClock::new()));

pub fn clock() -> UtcClock {
    CLOCK.lock(Cell::get)
}

pub fn unix_s() -> Option<i64> {
    clock()
        .unix_ms(Instant::now().as_millis())
        .map(|time| (time / 1000) as i64)
}

#[embassy_executor::task]
pub async fn synchronize(stack: Stack<'static>) -> ! {
    let servers = ["pool.ntp.org", "time.cloudflare.com"];
    let mut server = 0;
    loop {
        stack.wait_config_up().await;
        let result = with_timeout(Duration::from_secs(10), sample(stack, servers[server])).await;
        let delay = match result {
            Ok(Ok((uptime, utc))) => {
                CLOCK.lock(|clock| {
                    let mut updated = clock.get();
                    updated.synchronize(uptime, utc);
                    clock.set(updated);
                });
                defmt::info!("NTP synchronized: Unix seconds {=u64}", utc / 1000);
                Duration::from_secs(3600)
            }
            _ => {
                defmt::warn!("NTP synchronization failed; retrying in one minute");
                server = (server + 1) % servers.len();
                Duration::from_secs(60)
            }
        };
        Timer::after(delay).await;
    }
}

async fn sample(stack: Stack<'_>, server: &str) -> Result<(u64, u64), ()> {
    let addresses = stack
        .dns_query(server, DnsQueryType::A)
        .await
        .map_err(|_| ())?;
    let endpoint = IpEndpoint::new(*addresses.first().ok_or(())?, 123);
    let mut rx_meta = [PacketMetadata::EMPTY; 2];
    let mut tx_meta = [PacketMetadata::EMPTY; 1];
    let mut rx_buffer = [0; 256];
    let mut tx_buffer = [0; ntp::PACKET_LEN];
    let mut socket = UdpSocket::new(
        stack,
        &mut rx_meta,
        &mut rx_buffer,
        &mut tx_meta,
        &mut tx_buffer,
    );
    socket.bind(0).map_err(|_| ())?;
    let cookie = (RoscRng.next_u64() | 1).to_be_bytes();
    let started = Instant::now().as_millis();
    socket
        .send_to(&ntp::request(cookie), endpoint)
        .await
        .map_err(|_| ())?;
    let mut packet = [0; 256];
    // Ignore unrelated datagrams, under the enclosing deadline.
    loop {
        let (size, source) = socket.recv_from(&mut packet).await.map_err(|_| ())?;
        if source.endpoint != endpoint {
            continue;
        }
        let received = Instant::now().as_millis();
        let utc = ntp::reply(&packet[..size], cookie, received - started).map_err(|_| ())?;
        return Ok((received, utc));
    }
}
