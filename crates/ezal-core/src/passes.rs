//! Pure, incremental pass-window search, forecast assembly and dashboard
//! serialization.
//!
//! The shell supplies time and decides when to yield. Peak bracketing also
//! finds short grazing passes whose entire visible portion can fall between
//! scan samples. Crossings are refined to better than one second.
//!
//! A [`Forecast`] covers a fixed twelve-hour span, so it remains complete for
//! every six-hour dashboard window that fits inside it. Validity is therefore
//! a coverage question, not an age limit: the shell recomputes when coverage
//! runs low or new elements arrive, and a [`Report`] serves only what the
//! current forecast fully covers.

use crate::orbit::{
    fresh_epoch, within, Catalog, GroundStation, LookAngle, Observer, ObserverError, Orbit,
    Satellite, SATELLITES, SATELLITE_COUNT,
};
use core::fmt::{self, Write};
use heapless::Vec;

/// The dashboard lists passes acquired within the next six hours.
pub const WINDOW_S: i64 = 6 * 60 * 60;
/// Each forecast searches twelve hours, so one computation keeps the full
/// window available for about six hours.
pub const HORIZON_S: i64 = 12 * 60 * 60;
/// A served list stays complete for this long while the dashboard advances
/// time between polls. The client reads the bound from `valid_until_unix_s`.
pub const SERVE_AHEAD_S: i64 = 60;
/// Start a replacement forecast this long before coverage runs out.
pub const REFRESH_LEAD_S: i64 = 10 * 60;
/// A LEO (period of at least 80 minutes) makes at most one pass per orbit,
/// and a search, including its margins, spans fewer than eleven orbits.
pub const MAX_PASSES_PER_SATELLITE: usize = 12;
/// Room for every candidate's quota, so assembly never drops a pass.
pub const MAX_PASSES: usize = SATELLITE_COUNT * MAX_PASSES_PER_SATELLITE;
/// A served window, including a pass in progress, spans under five orbits.
pub const MAX_SERVED: usize = SATELLITE_COUNT * 6;
const STEP_S: i64 = 30;
const MARGIN_S: i64 = 30 * 60;
const DEFAULT_MIN_PEAK_DEG: f64 = 10.0;
const LATEST_START_UNIX_S: i64 = 4_102_444_800; // 2100-01-01

/// Ground station and the geometric definition of a useful reception window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    /// Receiver position.
    pub observer: Observer,
    /// AOS/LOS occur where elevation crosses this mask.
    pub min_elevation_deg: f64,
    /// Omit passes that never reach this elevation.
    pub min_peak_elevation_deg: f64,
    /// Include legacy Fengyun candidates, explicitly marked experimental.
    pub include_experimental: bool,
}

/// The first invalid setting in a [`Config`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// An invalid station coordinate.
    Observer(ObserverError),
    /// Elevation mask outside 0–89°.
    MinElevation,
    /// Minimum peak below the mask or above 90°.
    MinPeakElevation,
}

impl Config {
    /// All Saints Church, Clevedon; 5° mask, 10° minimum peak.
    pub const DEFAULT: Self = Self {
        observer: Observer::CLEVEDON,
        min_elevation_deg: 5.0,
        min_peak_elevation_deg: DEFAULT_MIN_PEAK_DEG,
        include_experimental: false,
    };

    /// The default minimum peak for a mask: 10°, raised to the mask when the
    /// mask is higher, so configuring only the mask is always valid.
    pub const fn default_min_peak(min_elevation_deg: f64) -> f64 {
        if min_elevation_deg > DEFAULT_MIN_PEAK_DEG {
            min_elevation_deg
        } else {
            DEFAULT_MIN_PEAK_DEG
        }
    }

    /// Validate all floats before they reach propagation or JSON output.
    /// `const`, so firmware rejects a bad baked-in configuration at compile time.
    pub const fn validate(self) -> Result<(), ConfigError> {
        if let Err(error) = self.observer.validate() {
            Err(ConfigError::Observer(error))
        } else if !within(self.min_elevation_deg, 0.0, 89.0) {
            Err(ConfigError::MinElevation)
        } else if !within(self.min_peak_elevation_deg, self.min_elevation_deg, 90.0) {
            Err(ConfigError::MinPeakElevation)
        } else {
            Ok(())
        }
    }

    /// Whether [`validate`](Self::validate) accepts this configuration.
    pub const fn is_valid(self) -> bool {
        self.validate().is_ok()
    }

    /// Whether a catalog service is requested by this configuration.
    pub const fn includes(self, satellite: &Satellite) -> bool {
        !satellite.experimental || self.include_experimental
    }
}

/// One complete reception window. Times remain actual crossings even if a pass
/// is underway at the search start or ends beyond the search boundary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pass {
    /// Candidate whose orbit produced this window.
    pub satellite: &'static Satellite,
    /// Acquisition of signal at the configured elevation mask, Unix seconds.
    pub aos_unix_s: i64,
    /// Loss of signal at the same mask, Unix seconds.
    pub los_unix_s: i64,
    /// Maximum geometric elevation.
    pub peak_elevation_deg: f64,
}

/// Explicit search failure, never confused with a successful empty forecast.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Invalid station, mask or time span.
    Config,
    /// Failed/non-finite propagation, or a crossing outside the LEO bound.
    Propagation,
}

/// A topocentric direction source; `None` when propagation fails.
pub trait Ephemeris {
    /// Direction from the station at a Unix time.
    fn look_at(&self, unix_s: f64) -> Option<LookAngle>;
}

impl<F: Fn(f64) -> Option<LookAngle>> Ephemeris for F {
    fn look_at(&self, unix_s: f64) -> Option<LookAngle> {
        self(unix_s)
    }
}

/// One catalog orbit seen from one station: a nameable [`Ephemeris`], so a
/// [`Job`] can hold its search between steps.
#[derive(Clone, Copy)]
pub struct Track<'a> {
    /// Propagated orbit.
    pub orbit: &'a Orbit,
    /// Observing station.
    pub station: &'a GroundStation,
}

impl Ephemeris for Track<'_> {
    fn look_at(&self, unix_s: f64) -> Option<LookAngle> {
        self.orbit.look_at(self.station, unix_s)
    }
}

/// A deterministic search with no runtime, clock, networking or global state.
/// Each `step` performs at most two propagations, including peak/crossing
/// refinement, so the shell can yield without starving actuator tasks.
pub struct Search<E> {
    ephemeris: E,
    config: Config,
    satellite: &'static Satellite,
    start: i64,
    end: i64,
    next: i64,
    previous: [Option<f64>; 2],
    refinement: Option<Refinement>,
    finished: bool,
}

#[derive(Clone, Copy)]
struct Sample {
    time: f64,
    angle: LookAngle,
}

enum Refinement {
    Peak {
        low: f64,
        high: f64,
    },
    Crossing {
        peak: Sample,
        aos: Option<Sample>,
        inside: f64,
        outside: f64,
        bracketed: bool,
    },
}

impl<E: Ephemeris> Search<E> {
    /// Search for passes acquired before `end_unix_s` that are still visible
    /// after `start_unix_s`, including one already in progress at the start.
    /// The span may not exceed [`HORIZON_S`]. The ephemeris must describe a
    /// low-eccentricity HRPT LEO.
    pub fn new(
        start_unix_s: i64,
        end_unix_s: i64,
        config: Config,
        satellite: &'static Satellite,
        ephemeris: E,
    ) -> Result<Self, Error> {
        if !config.is_valid()
            || !(0..=LATEST_START_UNIX_S).contains(&start_unix_s)
            || !(1..=HORIZON_S).contains(&(end_unix_s - start_unix_s))
        {
            return Err(Error::Config);
        }
        Ok(Self {
            ephemeris,
            config,
            satellite,
            start: start_unix_s,
            end: end_unix_s,
            next: start_unix_s - MARGIN_S,
            previous: [None; 2],
            refinement: None,
            finished: false,
        })
    }

    /// True after every sample has been considered or propagation failed.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Advance by a bounded amount of CPU work. `None` means no result at this
    /// step; check `is_finished` separately. Discard this satellite's partial
    /// results on error, since its full forecast could not be established.
    pub fn step(&mut self) -> Result<Option<Pass>, Error> {
        if self.finished {
            return Ok(None);
        }
        let result = self.advance();
        if result.is_err() {
            self.finished = true;
        }
        result
    }

    fn at(&self, time: f64) -> Result<LookAngle, Error> {
        self.ephemeris
            .look_at(time)
            .filter(|angle| angle.elevation_deg.is_finite() && angle.azimuth_deg.is_finite())
            .ok_or(Error::Propagation)
    }

    fn advance(&mut self) -> Result<Option<Pass>, Error> {
        if let Some(refinement) = self.refinement.take() {
            return self.refine(refinement);
        }
        if self.next > self.end + MARGIN_S {
            self.finished = true;
            return Ok(None);
        }
        let time = self.next;
        self.next += STEP_S;
        let elevation = self.at(time as f64)?.elevation_deg;
        let [left, center] = self.previous;
        self.previous = [center, Some(elevation)];
        if let (Some(left), Some(center)) = (left, center) {
            if center > left && center >= elevation {
                self.refinement = Some(Refinement::Peak {
                    low: (time - 2 * STEP_S) as f64,
                    high: time as f64,
                });
            }
        }
        Ok(None)
    }

    fn refine(&mut self, refinement: Refinement) -> Result<Option<Pass>, Error> {
        self.refinement = match refinement {
            Refinement::Peak { mut low, mut high } => {
                if high - low > 0.25 {
                    let a = low + (high - low) / 3.0;
                    let b = high - (high - low) / 3.0;
                    if self.at(a)?.elevation_deg < self.at(b)?.elevation_deg {
                        low = a;
                    } else {
                        high = b;
                    }
                    Some(Refinement::Peak { low, high })
                } else {
                    let time = (low + high) / 2.0;
                    let peak = Sample {
                        time,
                        angle: self.at(time)?,
                    };
                    // A tangency at the mask has zero duration.
                    if peak.angle.elevation_deg >= self.config.min_peak_elevation_deg
                        && peak.angle.elevation_deg > self.config.min_elevation_deg
                    {
                        Some(Refinement::Crossing {
                            peak,
                            aos: None,
                            inside: time,
                            outside: time - 60.0,
                            bracketed: false,
                        })
                    } else {
                        None
                    }
                }
            }
            Refinement::Crossing {
                peak,
                aos,
                mut inside,
                mut outside,
                mut bracketed,
            } => {
                let time = if bracketed {
                    (inside + outside) / 2.0
                } else {
                    outside
                };
                let sample = Sample {
                    time,
                    angle: self.at(time)?,
                };
                if bracketed && (inside - outside).abs() <= 0.25 {
                    if let Some(aos) = aos {
                        let aos_unix_s = libm::round(aos.time) as i64;
                        let los_unix_s = libm::round(time) as i64;
                        if time > self.start as f64
                            && aos.time < self.end as f64
                            && los_unix_s > aos_unix_s
                        {
                            return Ok(Some(Pass {
                                satellite: self.satellite,
                                aos_unix_s,
                                los_unix_s,
                                peak_elevation_deg: peak.angle.elevation_deg,
                            }));
                        }
                        None
                    } else {
                        Some(Refinement::Crossing {
                            peak,
                            aos: Some(sample),
                            inside: peak.time,
                            outside: peak.time + 60.0,
                            bracketed: false,
                        })
                    }
                } else {
                    if sample.angle.elevation_deg < self.config.min_elevation_deg {
                        outside = time;
                        bracketed = true;
                    } else {
                        inside = time;
                        if !bracketed {
                            outside += if aos.is_some() { 60.0 } else { -60.0 };
                            if (outside - peak.time).abs() > MARGIN_S as f64 {
                                return Err(Error::Propagation);
                            }
                        }
                    }
                    Some(Refinement::Crossing {
                        peak,
                        aos,
                        inside,
                        outside,
                        bracketed,
                    })
                }
            }
        };
        Ok(None)
    }
}

/// What a forecast established for one catalog entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Not requested, or no elements fresh for the whole span when computed.
    Skipped,
    /// Every qualifying pass in the span is present.
    Predicted,
    /// Propagation failed or the pass quota overflowed; no rows are kept.
    Failed,
}

/// A completed forecast: every pass acquired in `[start, end)` for each
/// [`Outcome::Predicted`] entry, sorted by acquisition.
#[derive(Clone, Debug, PartialEq)]
pub struct Forecast {
    /// Search start, Unix seconds.
    pub start_unix_s: i64,
    /// Search end (exclusive for acquisition), Unix seconds.
    pub end_unix_s: i64,
    /// Chronologically sorted by acquisition, then catalog number.
    pub passes: Vec<Pass, MAX_PASSES>,
    /// Per-catalog-entry result, in `SATELLITES` order.
    pub outcomes: [Outcome; SATELLITE_COUNT],
}

impl Forecast {
    /// The last instant whose served window, `[now, now + WINDOW_S +
    /// SERVE_AHEAD_S]`, lies inside this forecast's span.
    pub fn valid_until_unix_s(&self) -> i64 {
        self.end_unix_s - WINDOW_S - SERVE_AHEAD_S
    }

    /// Whether this forecast holds every pass a report at `now` may serve.
    pub fn covers(&self, now_unix_s: i64) -> bool {
        (self.start_unix_s..=self.valid_until_unix_s()).contains(&now_unix_s)
    }
}

/// Whether the shell should compute a new forecast at `now`: none exists, the
/// clock has moved outside its span, or coverage ends within
/// [`REFRESH_LEAD_S`]. New elements are a separate trigger the shell tracks.
pub fn needs_forecast(current: Option<&Forecast>, now_unix_s: i64) -> bool {
    current.is_none_or(|forecast| {
        !(forecast.start_unix_s..=forecast.valid_until_unix_s() - REFRESH_LEAD_S)
            .contains(&now_unix_s)
    })
}

/// Computes a complete [`Forecast`] in bounded steps so a cooperative shell
/// can yield between them. Each step performs at most two propagations. A
/// candidate whose search fails or overflows its `QUOTA` of passes is
/// discarded whole. Tests lower the quota to exercise that rule.
pub struct Job<'a, const QUOTA: usize = MAX_PASSES_PER_SATELLITE> {
    catalog: &'a Catalog,
    station: &'a GroundStation,
    config: Config,
    forecast: Forecast,
    next: usize,
    active: Option<Active<'a, QUOTA>>,
}

struct Active<'a, const QUOTA: usize> {
    index: usize,
    search: Search<Track<'a>>,
    passes: Vec<Pass, QUOTA>,
}

impl<'a, const QUOTA: usize> Job<'a, QUOTA> {
    const QUOTA_FITS: () = assert!(
        QUOTA <= MAX_PASSES_PER_SATELLITE,
        "the forecast reserves MAX_PASSES_PER_SATELLITE rows per candidate"
    );

    /// Prepare a forecast spanning [`HORIZON_S`] from `start_unix_s`.
    pub fn new(
        catalog: &'a Catalog,
        station: &'a GroundStation,
        config: Config,
        start_unix_s: i64,
    ) -> Result<Self, Error> {
        let () = Self::QUOTA_FITS;
        if !config.is_valid() || !(0..=LATEST_START_UNIX_S).contains(&start_unix_s) {
            return Err(Error::Config);
        }
        Ok(Self {
            catalog,
            station,
            config,
            forecast: Forecast {
                start_unix_s,
                end_unix_s: start_unix_s + HORIZON_S,
                passes: Vec::new(),
                outcomes: [Outcome::Skipped; SATELLITE_COUNT],
            },
            next: 0,
            active: None,
        })
    }

    /// Perform at most one search step. Returns `true` once every candidate
    /// is complete; then take the result with [`finish`](Self::finish).
    pub fn step(&mut self) -> bool {
        let Some(active) = &mut self.active else {
            return !self.start_next();
        };
        let outcome = match active.search.step() {
            Err(_) => Outcome::Failed,
            Ok(Some(_)) if active.passes.is_full() => Outcome::Failed,
            Ok(pass) => {
                if let Some(pass) = pass {
                    let _ = active.passes.push(pass); // not full: checked above
                }
                if !active.search.is_finished() {
                    return false;
                }
                // Capacity is reserved for every candidate's full quota.
                for &pass in &active.passes {
                    let _ = self.forecast.passes.push(pass);
                }
                Outcome::Predicted
            }
        };
        self.forecast.outcomes[active.index] = outcome;
        self.active = None;
        false
    }

    /// Begin the next requested candidate with fresh elements. Returns
    /// `false` when none remain. Skipping entries performs no propagation.
    fn start_next(&mut self) -> bool {
        let (start, end) = (self.forecast.start_unix_s, self.forecast.end_unix_s);
        while self.next < SATELLITE_COUNT {
            let index = self.next;
            self.next += 1;
            let satellite = &SATELLITES[index];
            let Some(orbit) = self.catalog.orbit(index) else {
                continue;
            };
            if !self.config.includes(satellite) || !orbit.is_fresh(start, end) {
                continue;
            }
            let track = Track {
                orbit,
                station: self.station,
            };
            match Search::new(start, end, self.config, satellite, track) {
                Ok(search) => {
                    self.active = Some(Active {
                        index,
                        search,
                        passes: Vec::new(),
                    });
                    return true;
                }
                Err(_) => self.forecast.outcomes[index] = Outcome::Failed,
            }
        }
        false
    }

    /// The completed forecast, sorted by acquisition then catalog number.
    pub fn finish(mut self) -> Forecast {
        self.forecast
            .passes
            .sort_unstable_by_key(|pass| (pass.aos_unix_s, pass.satellite.norad_id));
        self.forecast
    }
}

/// Element bookkeeping the shell keeps beside its forecast.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Elements {
    /// Catalog epochs in `SATELLITES` order (see [`Catalog::epochs`]).
    pub epochs: [Option<f64>; SATELLITE_COUNT],
    /// UTC of the last successful download.
    pub updated_unix_s: Option<i64>,
    /// The most recent download attempt failed.
    pub download_failed: bool,
}

impl Elements {
    /// No download yet.
    pub const EMPTY: Self = Self {
        epochs: [None; SATELLITE_COUNT],
        updated_unix_s: None,
        download_failed: false,
    };
}

/// Forecast readiness, in order of precedence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// No fresh UTC.
    WaitingForTime,
    /// No requested candidate has elements fresh for the next window.
    WaitingForTles,
    /// The current forecast does not cover now, or predates new candidates.
    Predicting,
    /// Rows are complete for the served window.
    Ready,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Self::WaitingForTime => "waiting_for_time",
            Self::WaitingForTles => "waiting_for_tles",
            Self::Predicting => "predicting",
            Self::Ready => "ready",
        }
    }
}

/// Dashboard state at one instant, small enough to copy out of shared
/// storage so that serialization holds no lock. Formats (via `Display`) as
/// the `/api/passes` JSON document; no allocation and no clock reads.
#[derive(Clone, Debug)]
pub struct Report {
    config: Config,
    status: Status,
    now_unix_s: Option<i64>,
    sync_age_ms: Option<u64>,
    valid_until_unix_s: Option<i64>,
    elements: Elements,
    available: [bool; SATELLITE_COUNT],
    failed: [bool; SATELLITE_COUNT],
    passes: Vec<Pass, MAX_SERVED>,
    truncated: bool,
}

impl Report {
    /// Select what may be shown at `now`. A candidate is available when its
    /// elements are fresh for the next window, independently of whether a
    /// forecast is current. Rows are served only when the forecast covers
    /// `[now, now + WINDOW_S + SERVE_AHEAD_S]` and includes every available
    /// candidate.
    pub fn new(
        config: Config,
        forecast: Option<&Forecast>,
        elements: Elements,
        now_unix_s: Option<i64>,
        sync_age_ms: Option<u64>,
    ) -> Self {
        let available: [bool; SATELLITE_COUNT] = core::array::from_fn(|index| {
            config.includes(&SATELLITES[index])
                && now_unix_s
                    .zip(elements.epochs[index])
                    .is_some_and(|(now, epoch)| fresh_epoch(epoch, now, now + WINDOW_S))
        });
        let current = now_unix_s.zip(forecast).and_then(|(now, forecast)| {
            let complete = (0..SATELLITE_COUNT)
                .all(|index| !available[index] || forecast.outcomes[index] != Outcome::Skipped);
            (forecast.covers(now) && complete).then_some((now, forecast))
        });
        let status = if now_unix_s.is_none() {
            Status::WaitingForTime
        } else if !available.contains(&true) {
            Status::WaitingForTles
        } else if current.is_none() {
            Status::Predicting
        } else {
            Status::Ready
        };
        let mut report = Self {
            config,
            status,
            now_unix_s,
            sync_age_ms,
            valid_until_unix_s: None,
            elements,
            available,
            failed: [false; SATELLITE_COUNT],
            passes: Vec::new(),
            truncated: false,
        };
        if let (Status::Ready, Some((now, forecast))) = (status, current) {
            report.valid_until_unix_s = Some(now + SERVE_AHEAD_S);
            report.failed = forecast.outcomes.map(|outcome| outcome == Outcome::Failed);
            for &pass in forecast.passes.iter().filter(|pass| {
                pass.los_unix_s > now && pass.aos_unix_s < now + WINDOW_S + SERVE_AHEAD_S
            }) {
                // Sorted by acquisition, so a full list keeps the earliest.
                if report.passes.push(pass).is_err() {
                    report.truncated = true;
                    break;
                }
            }
        }
        report
    }

    /// Readiness at the report's instant.
    pub fn status(&self) -> Status {
        self.status
    }

    /// Rows served, sorted by acquisition.
    pub fn passes(&self) -> &[Pass] {
        &self.passes
    }

    /// Serialized length, for a `Content-Length` header.
    pub fn json_len(&self) -> usize {
        struct Count(usize);
        impl Write for Count {
            fn write_str(&mut self, text: &str) -> fmt::Result {
                self.0 += text.len();
                Ok(())
            }
        }
        let mut count = Count(0);
        // Formatting a report cannot fail; it writes only numbers and constants.
        let _ = write!(count, "{self}");
        count.0
    }
}

impl fmt::Display for Report {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        let observer = self.config.observer;
        write!(
            out,
            "{{\"status\":\"{}\",\"now_unix_s\":{},\"sync_age_ms\":{},\"valid_until_unix_s\":{},\
             \"tle_updated_unix_s\":{},\"download_failed\":{},\"truncated\":{},\"window_s\":{WINDOW_S},\
             \"observer\":{{\"latitude_deg\":{},\"longitude_deg\":{},\"altitude_m\":{}}},\
             \"min_elevation_deg\":{},\"min_peak_elevation_deg\":{},\"satellites\":[",
            self.status.as_str(),
            Json(self.now_unix_s),
            Json(self.sync_age_ms),
            Json(self.valid_until_unix_s),
            Json(self.elements.updated_unix_s),
            self.elements.download_failed,
            self.truncated,
            observer.latitude_deg,
            observer.longitude_deg,
            observer.altitude_m,
            self.config.min_elevation_deg,
            self.config.min_peak_elevation_deg,
        )?;
        let mut separator = "";
        for (index, satellite) in SATELLITES.iter().enumerate() {
            if !self.config.includes(satellite) {
                continue;
            }
            write!(
                out,
                "{separator}{{\"name\":\"{}\",\"available\":{},\"failed\":{},\"experimental\":{}}}",
                satellite.name, self.available[index], self.failed[index], satellite.experimental
            )?;
            separator = ",";
        }
        out.write_str("],\"passes\":[")?;
        separator = "";
        for pass in &self.passes {
            write!(
                out,
                "{separator}{{\"name\":\"{}\",\"frequency_mhz\":\"{}\",\"experimental\":{},\
                 \"aos_unix_s\":{},\"los_unix_s\":{},\"peak_elevation_deg\":{:.1}}}",
                pass.satellite.name,
                pass.satellite.frequency_mhz,
                pass.satellite.experimental,
                pass.aos_unix_s,
                pass.los_unix_s,
                pass.peak_elevation_deg,
            )?;
            separator = ",";
        }
        out.write_str("]}")
    }
}

/// A JSON number or `null`.
struct Json<T>(Option<T>);

impl<T: fmt::Display> fmt::Display for Json<T> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(value) => write!(out, "{value}"),
            None => out.write_str("null"),
        }
    }
}
