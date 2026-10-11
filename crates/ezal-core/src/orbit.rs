//! Allocator-free orbit catalog and WGS84 observer geometry.
//!
//! SGP4 owns TLE field parsing, propagation and sidereal time. This module
//! checks every downloaded line before the library sees it, selects our
//! reception candidates, and rotates TEME positions into local east/north/up
//! coordinates. All times and input data are supplied by callers.
//!
//! The calculation follows these steps:
//!
//! 1. A Two-Line Element set (TLE) describes mean orbital elements at an epoch.
//!    SGP4 (Simplified General Perturbations 4) evolves those elements to the
//!    requested time, accounting for Earth's non-spherical gravity and drag.
//! 2. Its position is geocentric TEME (True Equator, Mean Equinox), in km.
//!    This frame does not rotate with the receiver on Earth's surface.
//! 3. Sidereal time supplies Earth's rotation angle, so the satellite can be
//!    expressed in the same Earth-fixed axes as the WGS84 ground station.
//! 4. Subtracting the station position gives the line of sight. Projecting it
//!    onto local east/north/up axes gives azimuth and elevation.
//!
//! Angles supplied by callers are degrees; trigonometry uses radians. Every
//! Cartesian position and displacement here is in kilometres. The WGS72
//! gravity model used by SGP4 and the WGS84 ellipsoid used for the station
//! serve different purposes: one evolves an orbit, the other locates a receiver.
//! See `docs/PASS_PREDICTION.md` for the equations, assumptions and references.

use heapless::Vec;
use libm::{atan2, cos, sin, sqrt};

/// Reject TLE epochs more than seven days from the prediction window's end.
pub const MAX_TLE_AGE_S: i64 = 7 * 86_400;
/// Small future epochs are normal in distributed element sets.
pub const MAX_FUTURE_EPOCH_S: i64 = 86_400;
/// Number of curated reception candidates.
pub const SATELLITE_COUNT: usize = 7;
/// Every TLE data line is exactly this many ASCII characters.
pub const TLE_LINE_LEN: usize = 69;

/// A curated L-band receiver candidate; status is not inferred from its TLE.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Satellite {
    /// Stable NORAD catalog identifier (names in feeds can change).
    pub norad_id: u64,
    /// Display name.
    pub name: &'static str,
    /// Nominal downlink MHz, including alternate channels where applicable.
    pub frequency_mhz: &'static str,
    /// Legacy services with no confirmed current usable broadcast.
    pub experimental: bool,
}

/// L-band catalog checked against WMO OSCAR on 2026-10-07. See
/// `docs/PASS_PREDICTION.md` for sources and the legacy Fengyun limitations.
/// A `static`, so passes can borrow their entry for the life of the program.
pub static SATELLITES: [Satellite; SATELLITE_COUNT] = [
    Satellite {
        norad_id: 38771,
        name: "METOP-B",
        frequency_mhz: "1701.3",
        experimental: false,
    },
    Satellite {
        norad_id: 43689,
        name: "METOP-C",
        frequency_mhz: "1701.3",
        experimental: false,
    },
    Satellite {
        norad_id: 57166,
        name: "METEOR-M N2-3",
        frequency_mhz: "1700 / 1705",
        experimental: false,
    },
    Satellite {
        norad_id: 59051,
        name: "METEOR-M N2-4",
        frequency_mhz: "1700 / 1705",
        experimental: false,
    },
    Satellite {
        norad_id: 32958,
        name: "FENGYUN-3A",
        frequency_mhz: "1704.5",
        experimental: true,
    },
    Satellite {
        norad_id: 37214,
        name: "FENGYUN-3B",
        frequency_mhz: "1704.5",
        experimental: true,
    },
    Satellite {
        norad_id: 39260,
        name: "FENGYUN-3C",
        frequency_mhz: "1706.7",
        experimental: true,
    },
];

/// Geodetic receiver coordinates. Longitude is east-positive, height in metres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Observer {
    /// Latitude, degrees north.
    pub latitude_deg: f64,
    /// Longitude, degrees east.
    pub longitude_deg: f64,
    /// Height above the WGS84 ellipsoid (zero is the unmeasured default).
    pub altitude_m: f64,
}

/// The first out-of-range or non-finite station coordinate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObserverError {
    /// Latitude outside ±90°.
    Latitude,
    /// Longitude outside ±180°.
    Longitude,
    /// Height outside −500 m to 10 km.
    Altitude,
}

impl Observer {
    /// All Saints Church, East Clevedon; height has not been surveyed.
    pub const CLEVEDON: Self = Self {
        latitude_deg: 51.44266,
        longitude_deg: -2.84053,
        altitude_m: 0.0,
    };

    /// Reject non-finite or out-of-range ground station coordinates. `const`,
    /// so firmware can reject its baked-in station at compile time.
    pub const fn validate(self) -> Result<(), ObserverError> {
        if !within(self.latitude_deg, -90.0, 90.0) {
            Err(ObserverError::Latitude)
        } else if !within(self.longitude_deg, -180.0, 180.0) {
            Err(ObserverError::Longitude)
        } else if !within(self.altitude_m, -500.0, 10_000.0) {
            Err(ObserverError::Altitude)
        } else {
            Ok(())
        }
    }

    /// Whether [`validate`](Self::validate) accepts these coordinates.
    pub const fn is_valid(self) -> bool {
        self.validate().is_ok()
    }
}

/// A finite value within an inclusive range (NaN is never within).
pub(crate) const fn within(value: f64, min: f64, max: f64) -> bool {
    value.is_finite() && value >= min && value <= max
}

/// The topocentric geometric direction, without terrain/refraction corrections.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LookAngle {
    /// Degrees clockwise from true north, in [0, 360).
    pub azimuth_deg: f64,
    /// Degrees above the local horizontal.
    pub elevation_deg: f64,
}

/// Prepared station geometry, reused across every propagation in a search.
#[derive(Clone, Debug)]
pub struct GroundStation {
    // Earth-fixed (ECEF) position in km: x through latitude/longitude (0°, 0°),
    // y through (0°, 90°E), z through the north pole; origin at Earth's centre.
    position: [f64; 3],
    // The station is fixed during a search, so both its position and the
    // trigonometric terms defining its local axes can be computed just once.
    sin_lat: f64,
    cos_lat: f64,
    sin_lon: f64,
    cos_lon: f64,
}

impl GroundStation {
    /// Precompute WGS84 Earth-fixed position and local basis.
    pub fn new(observer: Observer) -> Option<Self> {
        if !observer.is_valid() {
            return None;
        }
        // Latitude is geodetic: the angle of the ellipsoid's outward normal,
        // not the angle of a radius drawn from Earth's centre. That distinction
        // matters both for the position below and for the local "up" direction.
        let lat = observer.latitude_deg.to_radians();
        let lon = observer.longitude_deg.to_radians();
        let (sin_lat, cos_lat) = (sin(lat), cos(lat));
        let (sin_lon, cos_lon) = (sin(lon), cos(lon));
        // WGS84 equatorial radius a = 6378.137 km and squared eccentricity
        // e² = f(2 - f), with flattening f = 1 / 298.257223563. This e² describes
        // Earth's ellipsoid, independently of the satellite's orbital eccentricity.
        let e2 = 6.694_379_990_14e-3;
        // N = a / sqrt(1 - e² sin²(latitude)) is the prime-vertical radius of
        // curvature. It is not the station's distance from Earth's centre.
        let n = 6378.137 / sqrt(1.0 - e2 * sin_lat * sin_lat);
        // SGP4 returns km, whereas configuration stores metres. Height is
        // measured along the ellipsoid normal; sea-level height is different.
        let height = observer.altitude_m / 1000.0;
        Some(Self {
            // Geodetic -> ECEF:
            //   X = (N + h) cos(latitude) cos(longitude)
            //   Y = (N + h) cos(latitude) sin(longitude)
            //   Z = (N(1 - e²) + h) sin(latitude)
            // The smaller polar term accounts for Earth's flattening. Adding
            // h this way displaces the station along its local normal.
            position: [
                (n + height) * cos_lat * cos_lon,
                (n + height) * cos_lat * sin_lon,
                (n * (1.0 - e2) + height) * sin_lat,
            ],
            sin_lat,
            cos_lat,
            sin_lon,
            cos_lon,
        })
    }

    /// Rotate a TEME position in kilometres into local azimuth/elevation.
    /// UT1 is approximated by UTC; polar motion is negligible for this rotator.
    /// Elevation is geometric: no atmospheric bending or terrain mask is added.
    /// At the zenith/nadir, azimuth is mathematically undefined because the
    /// horizontal projection vanishes; the returned value is not a bearing there.
    pub fn look_at(&self, teme_km: [f64; 3], unix_s: f64) -> LookAngle {
        // The sidereal routine expects Julian years since J2000, not Unix
        // seconds: J2000 is 2000-01-01 12:00 UTC = Unix 946_728_000, and one
        // Julian year is exactly 365.25 * 86_400 = 31_557_600 seconds.
        // Its result θ is the Greenwich sidereal angle in radians. Sidereal
        // rotation is measured relative to the equinox, rather than the Sun,
        // so simply using the fraction of a 24-hour UTC day would be wrong.
        let sidereal = sgp4::afspc_epoch_to_sidereal_time((unix_s - 946_728_000.0) / 31_557_600.0);
        let (s, c) = (sin(sidereal), cos(sidereal));
        // Undo Earth's rotation about z to express TEME in rotating axes:
        //   X_fixed =  cos(θ) X_teme + sin(θ) Y_teme
        //   Y_fixed = -sin(θ) X_teme + cos(θ) Y_teme
        //   Z_fixed = Z_teme
        // Strictly these are pseudo-Earth-fixed (PEF) coordinates; neglecting
        // polar motion lets us use them as ECEF. Subtract the station only
        // after this rotation: vectors must share a frame before subtraction.
        // x/y/z below are receiver-to-satellite displacements, not positions.
        let x = c * teme_km[0] + s * teme_km[1] - self.position[0];
        let y = -s * teme_km[0] + c * teme_km[1] - self.position[1];
        let z = teme_km[2] - self.position[2];
        // Dot that displacement with three orthonormal local unit vectors,
        // written in ECEF axes (φ = geodetic latitude, λ = longitude):
        //   east  = (-sin λ,          cos λ,         0)
        //   north = (-sin φ cos λ,   -sin φ sin λ,   cos φ)
        //   up    = ( cos φ cos λ,    cos φ sin λ,   sin φ)
        // East/north span the ellipsoid's tangent plane. Up is its normal,
        // not a geocentric radius, which is why we retain geodetic latitude.
        let east = -self.sin_lon * x + self.cos_lon * y;
        let north =
            -self.sin_lat * self.cos_lon * x - self.sin_lat * self.sin_lon * y + self.cos_lat * z;
        let up =
            self.cos_lat * self.cos_lon * x + self.cos_lat * self.sin_lon * y + self.sin_lat * z;
        // atan2(y, x) preserves the quadrant and avoids dividing by zero.
        // With (east, north) as its arguments, 0° is north and 90° is east.
        // Normalize its signed result to the receiver's [0°, 360°) convention.
        let azimuth = atan2(east, north).to_degrees();
        LookAngle {
            azimuth_deg: if azimuth < 0.0 {
                azimuth + 360.0
            } else {
                azimuth
            },
            // Horizontal range H = sqrt(east² + north²); elevation = atan2(up, H).
            // Positive is above the local horizon, negative below. Normalizing
            // the full line-of-sight vector is unnecessary because a common
            // scale cancels in atan2; this also avoids an asin near ±1 at zenith.
            elevation_deg: atan2(up, sqrt(east * east + north * north)).to_degrees(),
        }
    }
}

// Fixed-width column layouts of the two TLE data lines: a literal line number
// or space, `i` catalog number (digits, or an Alpha-5 letter), `c`
// classification, `d` international designator, `f` numeric field and `k`
// the checksum digit. The array types pin both layouts to 69 columns.
const LINE1_LAYOUT: &[u8; TLE_LINE_LEN] =
    b"1 iiiiic dddddddd ffffffffffffff ffffffffff ffffffff ffffffff f ffffk";
const LINE2_LAYOUT: &[u8; TLE_LINE_LEN] =
    b"2 iiiii ffffffff ffffffff fffffff ffffffff ffffffff ffffffffffffffffk";

/// Check one line's layout and checksum before any byte reaches SGP4. The
/// library's checksum test underflows (a panic with overflow checks) on a
/// non-digit final column, it converts fields with unchecked UTF-8, and Rust
/// float parsing accepts `nan`/`inf`, so this is the boundary for untrusted
/// downloads. Numeric fields admit only digits, spaces, `.`, `+` and `-`.
fn well_formed(line: &[u8], layout: &[u8; TLE_LINE_LEN]) -> bool {
    let digit = |byte: u8| byte.is_ascii_digit().then(|| u32::from(byte - b'0'));
    if line.len() != TLE_LINE_LEN
        || !line
            .iter()
            .zip(layout)
            .all(|(&byte, &column)| match column {
                b'i' | b'd' => byte.is_ascii_digit() || byte.is_ascii_uppercase() || byte == b' ',
                b'c' => matches!(byte, b'U' | b'C' | b'S'),
                b'f' => byte.is_ascii_digit() || matches!(byte, b' ' | b'.' | b'+' | b'-'),
                b'k' => byte.is_ascii_digit(),
                literal => byte == literal,
            })
    {
        return false;
    }
    // Modulo-10 sum of the digits, with each minus sign counting as one.
    let sum: u32 = line[..TLE_LINE_LEN - 1]
        .iter()
        .map(|&byte| {
            if byte == b'-' {
                1
            } else {
                digit(byte).unwrap_or(0)
            }
        })
        .sum();
    digit(line[TLE_LINE_LEN - 1]) == Some(sum % 10)
}

/// Whether elements with this epoch may be used throughout `[start, end]`.
pub fn fresh_epoch(epoch_unix_s: f64, start_unix_s: i64, end_unix_s: i64) -> bool {
    // The epoch is the reference time of the fitted elements, not the download
    // time. Test the future allowance at the window's earliest instant and the
    // age limit at its latest. These are operational limits, not an accuracy
    // guarantee: real error also depends on drag, maneuvers and the element fit.
    epoch_unix_s <= (start_unix_s + MAX_FUTURE_EPOCH_S) as f64
        && epoch_unix_s >= (end_unix_s - MAX_TLE_AGE_S) as f64
}

/// Validated elements plus immutable SGP4 constants for one LEO satellite.
#[derive(Clone, Debug)]
pub struct Orbit {
    /// Exact catalog entry, matched by number rather than feed-provided name.
    pub satellite: &'static Satellite,
    /// Epoch in Unix seconds, retaining the fractional TLE epoch.
    pub epoch_unix_s: f64,
    constants: sgp4::Constants,
}

impl Orbit {
    /// Check both lines' layout and checksums, parse them with the library
    /// and reject physically impossible angles. Periods are limited to 80–150
    /// minutes: the bounded pass search is for HRPT LEOs.
    pub fn from_tle(satellite: &'static Satellite, line1: &[u8], line2: &[u8]) -> Option<Self> {
        if !well_formed(line1, LINE1_LAYOUT) || !well_formed(line2, LINE2_LAYOUT) {
            return None;
        }
        let elements = sgp4::Elements::from_tle(line1, line2).ok()?;
        // TLE angles are in degrees: inclination tilts the orbital plane;
        // right ascension locates its ascending node; argument of perigee
        // locates closest approach within that plane; mean anomaly gives
        // orbital phase at the epoch. Mean anomaly is not the geometric angle
        // of the satellite about Earth (the true anomaly).
        let angle = |degrees: f64| within(degrees, 0.0, 360.0);
        // Eccentricity <= 0.05 and mean motion 9.6..=18 revolutions/day are
        // search-policy restrictions, not universal validity limits for TLEs.
        // Period in minutes is 1440 / mean_motion, hence 80..=150 minutes.
        // The peak-search shape and crossing margins in passes.rs rely on this
        // nearly circular LEO domain; valid deep-space elements are rejected.
        if elements.norad_id != satellite.norad_id
            || !within(elements.inclination, 0.0, 180.0)
            || !angle(elements.right_ascension)
            || !angle(elements.argument_of_perigee)
            || !angle(elements.mean_anomaly)
            || !within(elements.eccentricity, 0.0, 0.05)
            || !within(elements.mean_motion, 9.6, 18.0)
            || !elements.mean_motion_dot.is_finite()
            || !elements.mean_motion_ddot.is_finite()
            || !elements.drag_term.is_finite()
        {
            return None;
        }
        // Keep fractional seconds (to milliseconds), rather than rounding the
        // epoch to an integer: any epoch shift also shifts every propagation.
        let epoch_unix_s = elements.datetime.and_utc().timestamp_millis() as f64 / 1000.0;
        // A TLE contains model-specific mean elements, not an instantaneous
        // Kepler ellipse. Let SGP4 perform the mean-element conversion and
        // prepare its gravity/drag coefficients once per accepted element set.
        // WGS72/AFSPC initialization preserves the conventional TLE model;
        // WGS84 remains appropriate for locating the physical ground station.
        let constants = sgp4::Constants::from_elements_afspc_compatibility_mode(&elements).ok()?;
        Some(Self {
            satellite,
            epoch_unix_s,
            constants,
        })
    }

    /// Require a fresh epoch throughout the entire requested window.
    pub fn is_fresh(&self, start_unix_s: i64, end_unix_s: i64) -> bool {
        fresh_epoch(self.epoch_unix_s, start_unix_s, end_unix_s)
    }

    /// Propagate using the library and apply only the observer transformation.
    pub fn look_at(&self, station: &GroundStation, unix_s: f64) -> Option<LookAngle> {
        // SGP4 takes elapsed minutes since the TLE epoch. Negative offsets are
        // allowed (a fresh distributed TLE can have a slightly future epoch).
        // Each call starts from the same epoch constants, so peak refinement
        // can evaluate times in either order without accumulated step error.
        // The propagation compatibility switch differs only for certain
        // deep-space orbits; the accepted 80..=150 minute LEOs use the same path.
        let prediction = self
            .constants
            .propagate(sgp4::MinutesSinceEpoch((unix_s - self.epoch_unix_s) / 60.0))
            .ok()?;
        // Position is in TEME kilometres. Velocity is also returned by SGP4,
        // but is unnecessary for geometric pointing and these pass windows.
        let look = station.look_at(prediction.position, unix_s);
        // A numerical failure must not become a direction or an empty forecast.
        (look.azimuth_deg.is_finite() && look.elevation_deg.is_finite()).then_some(look)
    }
}

/// Longest line kept while streaming. Anything longer is not TLE data.
const LINE_CAPACITY: usize = 80;

/// Incremental 2LE/3LE parser for a streamed download, so no response-sized
/// buffer is needed. Tolerates unrelated satellites, CRLF, blank lines and
/// line breaks split across reads. Accepted element sets are only staged: they
/// reach a [`Catalog`] through [`Catalog::commit`], once the caller knows the
/// transfer completed.
pub struct Update {
    now_unix_s: i64,
    line: Vec<u8, LINE_CAPACITY>,
    overlong: bool,
    first: Option<[u8; TLE_LINE_LEN]>,
    staged: [Option<Orbit>; SATELLITE_COUNT],
}

impl Update {
    /// Start parsing; entries must be fresh at `now_unix_s`.
    pub fn new(now_unix_s: i64) -> Self {
        Self {
            now_unix_s,
            line: Vec::new(),
            overlong: false,
            first: None,
            staged: [const { None }; SATELLITE_COUNT],
        }
    }

    /// Feed the next body bytes. Line breaks may fall anywhere.
    pub fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\n' {
                self.end_line();
            } else if self.line.push(byte).is_err() {
                self.overlong = true;
            }
        }
    }

    fn end_line(&mut self) {
        let line = core::mem::take(&mut self.line);
        if core::mem::replace(&mut self.overlong, false) {
            self.first = None;
            return;
        }
        let line = line.trim_ascii_end();
        if line.starts_with(b"1 ") {
            self.first = line.try_into().ok();
        } else if line.starts_with(b"2 ") {
            if let Some(first) = self.first.take() {
                self.stage(&first, line);
            }
        } else if !line.is_empty() {
            self.first = None;
        }
    }

    fn stage(&mut self, line1: &[u8; TLE_LINE_LEN], line2: &[u8]) {
        let id = core::str::from_utf8(&line1[2..7])
            .ok()
            .and_then(|id| id.trim().parse::<u64>().ok());
        let Some(index) = SATELLITES.iter().position(|sat| Some(sat.norad_id) == id) else {
            return;
        };
        let Some(orbit) = Orbit::from_tle(&SATELLITES[index], line1, line2) else {
            return;
        };
        if orbit.is_fresh(self.now_unix_s, self.now_unix_s)
            && self.staged[index]
                .as_ref()
                .is_none_or(|staged| orbit.epoch_unix_s >= staged.epoch_unix_s)
        {
            self.staged[index] = Some(orbit);
        }
    }
}

/// Bounded in-memory catalog. Failed or older downloads never replace good data.
#[derive(Clone, Debug)]
pub struct Catalog {
    orbits: [Option<Orbit>; SATELLITE_COUNT],
}

impl Default for Catalog {
    fn default() -> Self {
        Self::new()
    }
}

impl Catalog {
    /// Empty at boot: no baked-in orbital elements silently masquerade as fresh.
    pub const fn new() -> Self {
        Self {
            orbits: [const { None }; SATELLITE_COUNT],
        }
    }

    /// Apply a completed download. Staged entries replace current ones unless
    /// their epoch is older. Returns the number of entries updated.
    pub fn commit(&mut self, mut update: Update) -> usize {
        self.commit_mask(&mut update).count_ones() as usize
    }

    /// Exact accepted slots, for attaching source metadata only to entries
    /// actually published by a completed download.
    pub(crate) fn commit_mask(&mut self, update: &mut Update) -> u8 {
        // A final line without a trailing newline is still a line.
        if !update.line.is_empty() || update.overlong {
            update.end_line();
        }
        let mut updated = 0u8;
        for (index, (slot, staged)) in self
            .orbits
            .iter_mut()
            .zip(update.staged.iter_mut())
            .enumerate()
        {
            if let Some(orbit) = staged.take() {
                if slot
                    .as_ref()
                    .is_none_or(|old| orbit.epoch_unix_s >= old.epoch_unix_s)
                {
                    *slot = Some(orbit);
                    updated |= 1 << index;
                }
            }
        }
        updated
    }

    /// Parse and commit a complete 2LE/3LE text in one call. Malformed,
    /// wrong-ID, stale or regressive entries are ignored.
    pub fn update(&mut self, text: &str, now_unix_s: i64) -> usize {
        let mut update = Update::new(now_unix_s);
        update.push(text.as_bytes());
        self.commit(update)
    }

    /// The validated entry for `SATELLITES[index]`, if any.
    pub fn orbit(&self, index: usize) -> Option<&Orbit> {
        self.orbits.get(index)?.as_ref()
    }

    /// Validated entries; callers select services and enforce current epoch age.
    pub fn orbits(&self) -> impl Iterator<Item = &Orbit> {
        self.orbits.iter().flatten()
    }

    /// Epoch of each catalog entry, in `SATELLITES` order, so availability can
    /// be judged later without holding the catalog.
    pub fn epochs(&self) -> [Option<f64>; SATELLITE_COUNT] {
        core::array::from_fn(|index| self.orbit(index).map(|orbit| orbit.epoch_unix_s))
    }
}
