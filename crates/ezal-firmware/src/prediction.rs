//! Background catalog refresh and cooperative prediction shell. N2YO requests
//! and replies, HTTP framing, TLE validation, orbit propagation, pass search,
//! forecast assembly and JSON policy all live in ezal-core; this task only
//! performs DNS, TCP and TLS I/O and yields.

use core::cell::RefCell;
use embassy_futures::yield_now;
use embassy_net::{dns::DnsQueryType, tcp::TcpSocket, Stack};
use embassy_rp::{peripherals::TRNG, trng::Trng};
use embassy_sync::blocking_mutex::{raw::ThreadModeRawMutex, Mutex};
use embassy_time::{with_timeout, Duration, Instant, Timer};
use embedded_tls::{Aes128GcmSha256, TlsConfig, TlsConnection, TlsContext, UnsecureProvider};
use ezal_core::{
    catalog::{Catalog, CompletedOrbitalDownload, DownloadError, OrbitalDownload, OrbitalSnapshot},
    n2yo::{self, ReplyError},
    orbit::{GroundStation, ObserverError, SATELLITES, SATELLITE_COUNT},
    passes::{needs_forecast, ConfigError, Elements, Forecast, Job, Report},
    tracking::{PassEvent, TrackingMode},
};

include!(concat!(env!("OUT_DIR"), "/prediction_config.rs"));

// The station configuration is baked in by build.rs; reject it at compile
// time rather than at boot. Ranges live only in `Config::validate`.
const _: () = match CONFIG.validate() {
    Ok(()) => {}
    Err(ConfigError::Observer(ObserverError::Latitude)) => {
        panic!("EZAL_LATITUDE_DEG must be between -90 and 90")
    }
    Err(ConfigError::Observer(ObserverError::Longitude)) => {
        panic!("EZAL_LONGITUDE_DEG must be between -180 and 180")
    }
    Err(ConfigError::Observer(ObserverError::Altitude)) => {
        panic!("EZAL_ALTITUDE_M must be between -500 and 10000")
    }
    Err(ConfigError::MinElevation) => panic!("EZAL_MIN_ELEVATION_DEG must be between 0 and 89"),
    Err(ConfigError::MinPeakElevation) => {
        panic!("EZAL_MIN_PEAK_ELEVATION_DEG must be between EZAL_MIN_ELEVATION_DEG and 90")
    }
};

// Elements come from N2YO over TLS without certificate verification, the
// explicitly accepted transport for this installation: private on the way but,
// like plain HTTP, unauthenticated. Parser/age checks validate data, without
// authenticating a potentially hostile network. Forecast rows remain advisory
// until explicitly selected and armed. Every byte of each reply passes through
// ezal-core's bounded decoders, so a broken or hostile reply can fail the
// download but cannot panic or stall the task.

/// N2YO API key, baked in from `.env` by `build.rs`.
const N2YO_KEY: &str = env!("EZAL_N2YO_KEY");
const _: () = assert!(
    n2yo::valid_key(N2YO_KEY),
    "EZAL_N2YO_KEY is not a usable N2YO key; see ezal_core::n2yo::valid_key"
);
/// The largest TLS record, 2^14 plaintext bytes plus 256 of overhead.
const TLS_READ_RECORD: usize = 16_640;
/// Requests are short; the handshake is encoded in the larger read buffer.
const TLS_WRITE_RECORD: usize = 1024;
/// A refresh costs one N2YO request per satellite, of 1000 an hour.
const REFRESH: Duration = Duration::from_secs(6 * 3600);
const RETRY: Duration = Duration::from_secs(15 * 60);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);

struct Shared {
    forecast: Option<Forecast>,
    elements: Elements,
    sources: [Option<OrbitalSnapshot>; SATELLITE_COUNT],
}

static SHARED: Mutex<ThreadModeRawMutex, RefCell<Shared>> = Mutex::new(RefCell::new(Shared {
    forecast: None,
    elements: Elements::EMPTY,
    sources: [const { None }; SATELLITE_COUNT],
}));

/// Atomically pair the complete forecast with the exact orbital publication
/// used to compute it. Failed/in-progress downloads never replace this pair.
pub fn tracking_source(
    norad_id: u64,
) -> Option<(
    OrbitalSnapshot,
    heapless::Vec<PassEvent, { ezal_core::passes::MAX_PASSES }>,
)> {
    SHARED.lock(|cell| {
        let shared = cell.borrow();
        let source = shared
            .sources
            .iter()
            .flatten()
            .find(|s| s.norad_id() == norad_id)?
            .clone();
        let events = shared
            .forecast
            .as_ref()?
            .passes
            .iter()
            .copied()
            .map(PassEvent::from_pass)
            .collect();
        Some((source, events))
    })
}

pub fn observer() -> ezal_core::orbit::Observer {
    CONFIG.observer
}

/// The dashboard's view at this instant. Selecting rows copies a few hundred
/// bytes inside the thread-mode borrow; formatting happens later, outside it,
/// so float formatting cannot delay IRQs and slow clients hold no lock.
pub fn report() -> Report {
    let clock = crate::time::clock();
    let uptime = Instant::now().as_millis();
    let now = clock.unix_ms(uptime).map(|ms| (ms / 1000) as i64);
    let sync_age_ms = clock.age_ms(uptime);
    SHARED.lock(|cell| {
        let shared = cell.borrow();
        Report::new(
            CONFIG,
            shared.forecast.as_ref(),
            shared.elements,
            now,
            sync_age_ms,
        )
    })
}

#[embassy_executor::task]
pub async fn run(stack: Stack<'static>, mut rng: Trng<'static, TRNG>) -> ! {
    let station = GroundStation::new(CONFIG.observer).expect("validated at compile time");
    let mut catalog = Catalog::new();
    let mut next_fetch = Instant::now();
    // New elements make the published forecast outdated even while it still
    // covers the window.
    let mut elements_changed = false;
    loop {
        let Some(now) = crate::time::unix_s() else {
            Timer::after_secs(1).await;
            continue;
        };
        // The TLS key exchange runs for tens of milliseconds without yielding,
        // too long for a pass's control steps. An armed or running plan holds
        // its own elements, so a due download waits for Manual mode.
        let tracking = crate::state::STATE
            .with(|state| state.tracking().snapshot().mode == TrackingMode::Tracking);
        if Instant::now() >= next_fetch && stack.is_config_up() && stack.is_link_up() && !tracking {
            let updated = match with_timeout(DOWNLOAD_TIMEOUT, download(stack, &mut rng, now)).await
            {
                Ok(Ok(update)) => catalog
                    .commit(update, Instant::now().as_millis())
                    .unwrap_or(0),
                Ok(Err(stage)) => {
                    defmt::warn!("TLE download failed: {=str}", stage);
                    0
                }
                Err(_) => {
                    defmt::warn!("TLE download timed out");
                    0
                }
            };
            let succeeded = updated > 0;
            SHARED.lock(|cell| {
                let elements = &mut cell.borrow_mut().elements;
                if succeeded {
                    elements.epochs = catalog.epochs();
                    elements.updated_unix_s = Some(now);
                }
                elements.download_failed = !succeeded;
            });
            if succeeded {
                defmt::info!("TLE catalog refreshed ({=usize} entries)", updated);
                next_fetch = Instant::now() + REFRESH;
                elements_changed = true;
            } else {
                defmt::warn!("TLE refresh failed; retaining last valid elements");
                next_fetch = Instant::now() + RETRY;
            }
        }
        // Download latency and NTP corrections must not move the span back.
        let Some(now) = crate::time::unix_s() else {
            continue;
        };
        let due = SHARED.lock(|cell| needs_forecast(cell.borrow().forecast.as_ref(), now));
        if elements_changed || due {
            // NTP only yields 2020–2099 UTC, which a job always accepts.
            if let Ok(mut job) = Job::new(&catalog, &station, CONFIG, now) {
                // A timer yields even if every socket and CPU operation is
                // immediately ready. ADC, network and actuator tasks keep running.
                while !job.step() {
                    Timer::after_millis(1).await;
                }
                let forecast = job.finish();
                SHARED.lock(|cell| {
                    let mut shared = cell.borrow_mut();
                    shared.sources =
                        core::array::from_fn(|i| catalog.snapshot(SATELLITES[i].norad_id));
                    shared.forecast = Some(forecast);
                });
                crate::tracking::refresh_inputs(&crate::state::STATE);
                elements_changed = false;
            }
        }
        // Coverage lasts hours; this only bounds how soon a due download,
        // clock step or lost synchronization is noticed.
        Timer::after_secs(5).await;
    }
}

/// Fetch every catalogued satellite's elements on one TLS connection. Body
/// bytes stream from a small read buffer through the HTTP decoder and N2YO's
/// `tle` reader into the TLE parser, so no reply-sized buffer exists. Staged
/// entries are committed by the caller only after every reply is complete.
/// A failure names the stage that failed.
async fn download(
    stack: Stack<'_>,
    rng: &mut Trng<'static, TRNG>,
    now_unix_s: i64,
) -> Result<CompletedOrbitalDownload, &'static str> {
    let addresses = stack
        .dns_query(n2yo::HOST, DnsQueryType::A)
        .await
        .map_err(|_| "DNS lookup")?;
    let address = *addresses.first().ok_or("DNS lookup")?;
    let mut rx_buffer = [0; 2048];
    let mut tx_buffer = [0; 512];
    let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
    socket.set_timeout(Some(Duration::from_secs(10)));
    socket
        .connect((address, n2yo::PORT))
        .await
        .map_err(|_| "TCP connect")?;
    let mut read_record = [0; TLS_READ_RECORD];
    let mut write_record = [0; TLS_WRITE_RECORD];
    let mut tls = TlsConnection::new(socket, &mut read_record, &mut write_record);
    let result = exchange(&mut tls, rng, now_unix_s).await;
    // Nothing more is needed from the connection, whatever happened.
    let (Ok(mut socket) | Err((mut socket, _))) = tls.close().await;
    socket.abort();
    result
}

async fn exchange(
    tls: &mut TlsConnection<'_, TcpSocket<'_>, Aes128GcmSha256>,
    rng: &mut Trng<'static, TRNG>,
    now_unix_s: i64,
) -> Result<CompletedOrbitalDownload, &'static str> {
    // N2YO's certificate is RSA, so RSA signature schemes must be offered even
    // though nothing here checks the signature.
    let config = TlsConfig::new()
        .with_server_name(n2yo::HOST)
        .enable_rsa_signatures();
    tls.open(TlsContext::new(
        &config,
        UnsecureProvider::new::<Aes128GcmSha256>(rng),
    ))
    .await
    .map_err(|_| "TLS handshake")?;
    let mut download = OrbitalDownload::new(now_unix_s);
    let mut buffer = [0; 512];
    for (index, satellite) in SATELLITES.iter().enumerate() {
        if index > 0 {
            download.next_reply().map_err(reason)?;
        }
        // The key was checked at compile time, so the request always exists.
        let request = n2yo::request(satellite.norad_id, N2YO_KEY).ok_or("request")?;
        let mut request = request.as_bytes();
        while !request.is_empty() {
            let written = tls.write(request).await.map_err(|_| "TLS write")?;
            if written == 0 {
                return Err("TLS write");
            }
            request = &request[written..];
        }
        tls.flush().await.map_err(|_| "TLS write")?;
        loop {
            // Every read awaits the socket, so the deadline can always fire.
            let read = tls.read(&mut buffer).await.map_err(|_| "TLS read")?;
            if read == 0 {
                // Closed: `next_reply` or `finish` judges the reply so far.
                break;
            }
            download.push(&buffer[..read]).map_err(reason)?;
            if download.is_complete() {
                break;
            }
            // Buffered records can make every read immediately ready. Parsing
            // is bounded per chunk and yields even in that case.
            yield_now().await;
        }
    }
    download.finish().map_err(reason)
}

fn reason(error: DownloadError) -> &'static str {
    match error {
        DownloadError::Http(_) => "HTTP framing",
        DownloadError::Reply(ReplyError::NoElements) => {
            "reply without elements; check EZAL_N2YO_KEY"
        }
        DownloadError::Reply(ReplyError::Malformed) => "malformed reply",
    }
}
