//! Background catalog refresh and cooperative prediction shell. HTTP framing,
//! TLE validation, orbit propagation, pass search, forecast assembly and JSON
//! policy all live in ezal-core; this task only performs I/O and yields.

use core::cell::RefCell;
use embassy_net::{dns::DnsQueryType, tcp::TcpSocket, Stack};
use embassy_sync::blocking_mutex::{raw::CriticalSectionRawMutex, Mutex};
use embassy_time::{with_timeout, Duration, Instant, Timer};
use ezal_core::{
    orbit::{GroundStation, ObserverError},
    passes::{needs_forecast, ConfigError, Elements, Forecast, Job, Report},
    tracking::{CompletedOrbitalDownload, OrbitalCatalog, OrbitalDownload},
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

// HTTP is the explicitly accepted transport for this installation. Parser/age
// checks validate data, without authenticating a potentially hostile network.
// Predictions currently remain advisory. Every byte of
// the response passes through ezal-core's bounded decoders, so a broken or
// hostile reply can fail the download but cannot panic or stall the task.
const TLE_HOST: &str = "celestrak.org";
const TLE_REQUEST: &[u8] = b"GET /NORAD/elements/gp.php?GROUP=weather&FORMAT=tle HTTP/1.1\r\n\
Host: celestrak.org\r\nUser-Agent: ezal\r\nAccept: text/plain\r\nConnection: close\r\n\r\n";
const REFRESH: Duration = Duration::from_secs(6 * 3600);
const RETRY: Duration = Duration::from_secs(15 * 60);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);

struct Shared {
    forecast: Option<Forecast>,
    elements: Elements,
}

static SHARED: Mutex<CriticalSectionRawMutex, RefCell<Shared>> = Mutex::new(RefCell::new(Shared {
    forecast: None,
    elements: Elements::EMPTY,
}));

/// The dashboard's view at this instant. Selecting rows copies a few hundred
/// bytes inside the critical section; formatting happens later, outside it,
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
pub async fn run(stack: Stack<'static>) -> ! {
    let station = GroundStation::new(CONFIG.observer).expect("validated at compile time");
    let mut catalog = OrbitalCatalog::new();
    let mut next_fetch = Instant::now();
    // New elements make the published forecast outdated even while it still
    // covers the window.
    let mut elements_changed = false;
    loop {
        let Some(now) = crate::time::unix_s() else {
            Timer::after_secs(1).await;
            continue;
        };
        if Instant::now() >= next_fetch && stack.is_config_up() && stack.is_link_up() {
            let updated = match with_timeout(DOWNLOAD_TIMEOUT, download(stack, now)).await {
                Ok(Ok(update)) => catalog
                    .commit(update, Instant::now().as_millis())
                    .unwrap_or(0),
                _ => 0,
            };
            let succeeded = updated > 0;
            SHARED.lock(|cell| {
                let elements = &mut cell.borrow_mut().elements;
                if succeeded {
                    elements.epochs = catalog.catalog().epochs();
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
            let job: Result<Job, _> = Job::new(catalog.catalog(), &station, CONFIG, now);
            if let Ok(mut job) = job {
                // A timer yields even if every socket and CPU operation is
                // immediately ready. ADC, network and actuator tasks keep running.
                while !job.step() {
                    Timer::after_millis(1).await;
                }
                let forecast = job.finish();
                SHARED.lock(|cell| cell.borrow_mut().forecast = Some(forecast));
                elements_changed = false;
            }
        }
        // Coverage lasts hours; this only bounds how soon a due download,
        // clock step or lost synchronization is noticed.
        Timer::after_secs(5).await;
    }
}

/// Fetch and stage the weather group's elements. Body bytes stream from a
/// small read buffer through the HTTP decoder into the TLE parser, so no
/// response-sized buffer exists. Staged entries are committed by the caller
/// only after the response is known to be complete.
async fn download(stack: Stack<'_>, now_unix_s: i64) -> Result<CompletedOrbitalDownload, ()> {
    let addresses = stack
        .dns_query(TLE_HOST, DnsQueryType::A)
        .await
        .map_err(|_| ())?;
    let address = *addresses.first().ok_or(())?;
    let mut rx_buffer = [0; 2048];
    let mut tx_buffer = [0; 512];
    let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
    socket.set_timeout(Some(Duration::from_secs(10)));
    socket.connect((address, 80)).await.map_err(|_| ())?;
    let result = exchange(&mut socket, now_unix_s).await;
    socket.abort();
    result
}

async fn exchange(
    socket: &mut TcpSocket<'_>,
    now_unix_s: i64,
) -> Result<CompletedOrbitalDownload, ()> {
    let mut request = TLE_REQUEST;
    while !request.is_empty() {
        let written = socket.write(request).await.map_err(|_| ())?;
        if written == 0 {
            return Err(());
        }
        request = &request[written..];
    }
    socket.flush().await.map_err(|_| ())?;
    let mut download = OrbitalDownload::new(now_unix_s);
    let mut buffer = [0; 512];
    loop {
        // Every read awaits the socket, so the deadline can always fire.
        let read = socket.read(&mut buffer).await.map_err(|_| ())?;
        if read == 0 {
            return download.finish().map_err(|_| ());
        }
        download.push(&buffer[..read]).map_err(|_| ())?;
        if download.is_complete() {
            return download.finish().map_err(|_| ());
        }
    }
}
