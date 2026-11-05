# Pass prediction

The firmware shows a rolling six-hour forecast at `/api/passes` and in the
dashboard. NTP synchronization starts after DHCP, the TLE download waits for
valid UTC, and the firmware then computes SGP4 reception windows independently
of dashboard connections. Forecast rows alone never authorize movement, and
the firmware boots idle. [TRACKING.md](TRACKING.md) describes how one selected
pass is planned, armed and followed.

## Station configuration

Set these values in the workspace `.env` (or process environment) and rebuild.
Process variables take precedence; in `.env` the last line for a key wins and
` #` starts a comment. Configuration persists in the firmware image; there is
no runtime configuration or flash-write path.

| Variable | Default | Meaning |
|---|---:|---|
| `EZAL_LATITUDE_DEG` | `51.44266` | Geodetic latitude, north positive |
| `EZAL_LONGITUDE_DEG` | `-2.84053` | Longitude, east positive |
| `EZAL_ALTITUDE_M` | `0` | Antenna height above the WGS84 ellipsoid, metres |
| `EZAL_MIN_ELEVATION_DEG` | `5` | AOS and LOS elevation mask |
| `EZAL_MIN_PEAK_ELEVATION_DEG` | `10`, or the mask if higher | Minimum maximum elevation for including a pass |
| `EZAL_INCLUDE_EXPERIMENTAL` | `false` | Add inactive/unconfirmed legacy Fengyun candidates |

The default horizontal coordinates identify
[All Saints Church, East Clevedon](https://dove.cccbr.org.uk/tower/16163).
The antenna height is unmeasured; zero is an explicit assumption. Latitude must
be within ±90°, longitude within ±180°, height within -500–10,000 m, cutoff
within 0–89°, and minimum peak between the cutoff and 90°. Text that is not a
finite number fails the build script; an out-of-range value fails compilation
through a constant check of `passes::Config::validate`, the single place the
ranges are defined, naming the variable. The build script refers to unset
settings by name from `Config::DEFAULT`, so defaults are not copied either.
Dashboard times are explicitly UTC, including the date so passes through
midnight are unambiguous.

“Acquisition” and “loss” are geometric crossings of the configured elevation
mask, rounded to seconds. They are estimates, not measured receiver locks.
Terrain, buildings, RF link budget, Doppler tuning, transmitter schedules and
rotator slew/cable-wrap limits are not modeled. A pass underway at the start
retains its actual AOS, and a pass acquired within six hours retains its full
LOS even if that is later than the forecast boundary. Overlapping windows are
listed independently; there is no automatic pass arbitration or prepositioning.

## Satellite catalog

The catalog matches NORAD IDs, never feed-provided names. Service information
was checked on 2026-10-07; TLE availability does not establish transmitter health.

| Satellite | NORAD | Nominal MHz | Default |
|---|---:|---|---|
| METOP-B | 38771 | 1701.3 | Enabled |
| METOP-C | 43689 | 1701.3 | Enabled |
| METEOR-M N2-3 | 57166 | 1700 / 1705 | Enabled |
| METEOR-M N2-4 | 59051 | 1700 / 1705 | Enabled |
| FENGYUN-3A | 32958 | 1704.5 | Experimental only |
| FENGYUN-3B | 37214 | 1704.5 | Experimental only |
| FENGYUN-3C | 39260 | 1706.7 | Experimental only |

[EUMETSAT](https://www.eumetsat.int/our-satellites/metop-series) identifies
METOP-B/C as operational and METOP-A as retired. WMO supplies
[METOP-B's AHRPT frequency](https://space.oscar.wmo.int/satellites/view/metop_b)
and lists [METEOR-M N2-3](https://space.oscar.wmo.int/satellites/view/meteor_m_n2_3)
and [N2-4](https://space.oscar.wmo.int/satellites/view/meteor_m_n2_4) as operational
with L-band downlinks. Both Meteor channel assignments are shown because
orbital elements cannot tell us which transmitter is enabled on a given pass.

WMO lists [FY-3A](https://space.oscar.wmo.int/satellites/view/fy_3a),
[FY-3B](https://space.oscar.wmo.int/satellites/view/fy_3b) and
[FY-3C](https://space.oscar.wmo.int/satellites/view/fy_3c) as inactive.
FY-3C also stopped HRPT outside China in 2020. Enabling experimental candidates
shows their geometric windows, clearly marked unconfirmed; it does not claim
they can be received in Clevedon. Newer Fengyun direct broadcasts require
X-band equipment; [SatDump's decoder catalog](https://github.com/SatDump/SatDump.Docs/blob/master/plugins.rst)
distinguishes the A/B/C L-band feeds from D/E X-band AHRPT.
[NOAA-15/18/19 are retired](https://www.ospo.noaa.gov/operations/poes/status.html)
and excluded. Extend `orbit::SATELLITES` when new usable L-band services are
confirmed; check forecast capacity if significantly enlarging it.

## Time, data and failure behavior

- NTP uses `pool.ntp.org`, with `time.cloudflare.com` on failure. Queries have
  a ten-second total timeout. Good samples refresh every five minutes; failures retry in
  one minute. Replies must match the source endpoint and random request cookie,
  be synchronized server replies, and have sane timestamps and RTT ≤2 seconds.
  UTC advances from Embassy uptime; six hours without a good sample invalidates
  it. The codec handles the 2036 NTP era rollover, for dates in 2020–2099.
- The public [CelesTrak weather feed](https://celestrak.org/NORAD/elements/gp.php?GROUP=weather&FORMAT=tle)
  is downloaded once at boot and every six hours, with a fifteen-minute retry
  after failure. DNS, connection and response share a thirty-second deadline,
  and every socket read awaits, so the deadline always fires. The response
  streams from a 512-byte read buffer through `ezal_core::http` (Content-Length,
  chunked or close-delimited framing, at most 64 KiB) straight into the TLE
  parser; there is no response buffer. Truncated, oversized, ambiguously framed
  or non-200 responses fail the download without touching the catalog, and
  bytes after a complete body are ignored rather than counted.
- The Pico uses the feed's HTTP endpoint without TLS. NTP is also unauthenticated.
  On 2026-10-11 the operator explicitly accepted this transport risk for tracking.
  Checksums, ID matching and age checks validate data but cannot establish its
  authenticity. Tracking requires no TLS stack or additional gateway service.
- Each line's fixed-width layout and checksum are checked before the `sgp4`
  library sees it: lines must be 69 ASCII characters, and numeric columns admit
  only digits, spaces, `.`, `+` and `-`, so `nan`/`inf` cannot parse and the
  library's checksum arithmetic never meets a non-digit. Parsed sets require
  matching IDs, physical angles and finite terms, and must describe a
  low-eccentricity LEO with an 80–150 minute period. An epoch must be no more
  than one day in the future and remain within seven days of the end of the
  forecast. Entries are staged while streaming and committed only after the
  whole response arrives. Failed, malformed or older updates retain the last valid entry.
  Missing/stale/unusable entries reduce coverage explicitly, rather than
  presenting a misleading complete empty forecast. Nothing is persisted to flash.
- Each forecast searches twelve hours from its start, so it stays complete for
  every six-hour window that fits inside it. The firmware computes a
  replacement when less than ten minutes of that coverage remain (about every
  six hours), when new elements arrive, or when the clock leaves the forecast's
  span; the previous forecast stays on display meanwhile. Validity is coverage,
  not age, so calculation time, download time and poll phase cannot hide rows.
  Each search step performs at most two propagations and the shell yields for
  one millisecond between steps. A failed propagation, or more passes than one
  per orbit allows, discards that satellite's results and is reported as a
  prediction failure. Results are sorted by AOS; capacity is reserved for every
  satellite's quota. Offline forecasts remain possible with fresh cached TLEs and UTC.
- The dashboard polls every fifteen seconds over a separate HTTP path. Each
  reply lists every pass in the next six hours plus one minute and states, as
  `valid_until_unix_s`, how long that list stays complete; the browser applies
  no age limit of its own. No NTP, a forecast that does not cover the window,
  or HTTP failures hide predicted rows. Satellite availability reflects fresh
  elements even while a forecast is being calculated, and failed predictions
  are listed separately. Successful empty forecasts, incomplete coverage, time
  wait and TLE wait have distinct messages. The page rewrites its live status
  and rows only when they change. The UTC display advances from the device's
  timestamp using browser elapsed time; it does not trust the browser's wall
  clock. The station line shows only settings the device has reported.

## Calculation and verification

`ezal-core` stays `no_std`, allocator-free, deterministic, and free of I/O:
`ntp` owns packet validation and UTC anchoring; `orbit` adapts
[`sgp4` 2.4](https://docs.rs/sgp4/2.4.0/sgp4/) with its `libm` feature;
`http` owns response framing; `passes` owns the incremental search, forecast
assembly and serialization. Firmware provides DNS, UDP/TCP sockets, timers and
coherent shared snapshots. Each of the eight HTTP tasks copies a report of
about 1.5 KiB into its own slot and formats JSON from it directly into the
socket, so no buffer or lock is shared between requests and a client that
stops acknowledging delays only its own reply. Response futures hold only a
reference to the slot, which keeps per-task RAM small. No critical section
spans an await or serialization.

### From orbital elements to a satellite position

The calculation in `crates/ezal-core/src/orbit.rs` starts with a validated
Two-Line Element set (TLE). Its epoch is the reference time of the orbit fit,
which can differ from the time we downloaded it. The principal elements are
inclination (tilt of the orbital plane), right ascension of the ascending node
(orientation of that plane), eccentricity (shape), argument of perigee
(orientation of closest approach within the plane), mean anomaly (phase at
the epoch), and mean motion (revolutions per day). Mean anomaly advances
uniformly in an ideal two-body orbit; it differs from the satellite's actual
angular position, called true anomaly, on an eccentric orbit.

These are mean elements fitted for the SGP4 model. Treating them as an
instantaneous Kepler ellipse and advancing only the mean anomaly would lose
the perturbations required to reconstruct the orbit.
[CelesTrak's model documentation](https://celestrak.org/NORAD/documentation/)
explains this connection between the elements and their propagator. SGP4
(Simplified General Perturbations 4) handles the gravity and drag corrections;
our code prepares its constants once per TLE and supplies elapsed time as:

```text
elapsed_minutes = (requested_unix_seconds - tle_epoch_unix_seconds) / 60
```

Fractional epoch seconds are retained to milliseconds, and refinement uses
fractional requested seconds. Negative elapsed times are permitted for a
slightly future epoch. Predictions start from the same epoch constants, so
evaluating times out of order during refinement does not accumulate an
integration error from one query to the next. The result used here is a
geocentric position in kilometres in TEME (True Equator, Mean Equinox).
SGP4 also returns velocity, which this geometric window calculation does not
need.

We use [WGS72/AFSPC initialization](https://docs.rs/sgp4/2.4.0/sgp4/struct.Constants.html#method.from_elements_afspc_compatibility_mode)
for the TLE propagator and WGS84 for the receiver geometry. Those models have
separate roles: SGP4 needs its conventional gravity constants, while station
coordinates refer to the WGS84 reference ellipsoid. The ordinary `propagate`
method and the library's AFSPC propagation variant differ only for certain
deep-space orbits, outside the supported LEO range.

The application accepts eccentricity from 0 to 0.05 and mean motion from 9.6
to 18 revolutions/day. Since `period_minutes = 1440 / mean_motion`, that
corresponds to 80–150 minutes. These restrictions establish the domain of the
pass search below; they are narrower than the orbits SGP4 itself can handle.

### Locating the receiver on the ellipsoid

`GroundStation::new` converts configured geodetic latitude `φ`, east-positive
longitude `λ` and ellipsoid height `h` to Earth-centred, Earth-fixed (ECEF)
coordinates. Latitude and longitude are converted to radians and height to
kilometres. ECEF has its origin at Earth's centre, its x axis through the
equator at Greenwich, its y axis through the equator at 90° east, and its z
axis through the north pole.

[WGS84's defining ellipsoid parameters](https://earth-info.nga.mil/index.php?action=wgs84&dir=wgs84)
are equatorial radius `a = 6378.137 km` and flattening
`f = 1 / 298.257223563`. The squared ellipsoid eccentricity is
`e² = f(2 - f) = 0.00669437999014`; it is unrelated to the satellite's orbital
eccentricity. Using the
[ellipsoidal-to-Cartesian equations](https://gssc.esa.int/navipedia/index.php/Ellipsoidal_and_Cartesian_Coordinates_Conversion):

```text
N = a / sqrt(1 - e² sin²φ)

X_station = (N + h) cosφ cosλ
Y_station = (N + h) cosφ sinλ
Z_station = (N(1 - e²) + h) sinφ
```

`N` is a radius of curvature, not the distance from Earth's centre. Geodetic
latitude measures the ellipsoid's surface normal relative to the equatorial
plane; geocentric latitude instead measures a radius from Earth's centre.
The formulas use geodetic latitude throughout. Height adds displacement along
that normal, and the `1 - e²` factor accounts for the flattened polar shape.
The resulting station position and its latitude/longitude trigonometric terms
are reused for every satellite and time in the search.

### Rotating the satellite into the receiver's frame

We cannot subtract the station's ECEF coordinates directly from SGP4's TEME
coordinates: the two sets of axes have different orientations. At the
requested instant, `GroundStation::look_at` obtains Greenwich sidereal angle
`θ` from the library's AFSPC sidereal routine. The argument to that routine is
Julian years since J2000:

```text
J2000 = 2000-01-01 12:00 UTC = Unix 946728000 seconds
one Julian year = 365.25 × 86400 = 31557600 seconds
years_since_J2000 = (unix_seconds - 946728000) / 31557600

X_fixed =  cosθ X_TEME + sinθ Y_TEME
Y_fixed = -sinθ X_TEME + cosθ Y_TEME
Z_fixed = Z_TEME

(dx, dy, dz) = (X_fixed, Y_fixed, Z_fixed)
             - (X_station, Y_station, Z_station)
```

The rotation undoes Earth's rotation about its polar axis. Sidereal time
measures that rotation relative to the equinox, so UTC's fraction of a
24-hour solar day cannot replace `θ`. The signs matter: the coordinate axes
rotate with Earth while the satellite vector is being expressed in them.
Subtraction then gives the displacement from receiver to satellite, in km.

Strictly, the rotation produces pseudo-Earth-fixed (PEF) coordinates; treating
them as ECEF omits polar motion. We also approximate Earth-rotation time UT1
with UTC rather than ingesting Earth orientation parameters. This follows the
TEME-to-PEF approach described in
[Vallado et al., *Revisiting Spacetrack Report #3*](https://celestrak.org/publications/AIAA/2006-6753/AIAA-2006-6753.pdf),
with those approximations explicitly applied.

### Turning the displacement into azimuth and elevation

The local east/north/up (ENU) axes form an orthonormal basis at the receiver.
Dot products with those unit vectors project the displacement onto that basis:

```text
east  = -sinλ dx + cosλ dy
north = -sinφ cosλ dx - sinφ sinλ dy + cosφ dz
up    =  cosφ cosλ dx + cosφ sinλ dy + sinφ dz

horizontal_range = sqrt(east² + north²)
azimuth_radians   = atan2(east, north)
elevation_radians = atan2(up, horizontal_range)
```

The basis comes from the
[ECEF-to-ENU transformation](https://gssc.esa.int/navipedia/index.php/Transformations_between_ECEF_and_ENU_coordinates).
East and north lie in the ellipsoid's tangent plane; up points along its
normal. `atan2` retains quadrant information. Supplying east first and north
second makes azimuth increase clockwise from true north: north is 0°, east
90°, south 180°, and west 270°. A negative result is shifted by 360° before
returning degrees. Positive elevation is above the local horizontal and
negative elevation is below it. The common vector scale cancels in `atan2`,
so normalizing to a unit line of sight is unnecessary. At an exact zenith or
nadir the horizontal projection vanishes and azimuth is undefined.

### Searching for complete pass windows

`crates/ezal-core/src/passes.rs` treats elevation as a scalar function `e(t)`.
For elevation mask `m`, define `g(t) = e(t) - m`. AOS is the rising root
`g(t) = 0`, LOS is the falling root, and the interval between them is at or
above the mask. A separate minimum peak requirement decides whether to keep
the interval. For example, a pass that peaks at 8° crosses a 5° mask but is
omitted when the minimum peak is 10°.

The search proceeds as follows:

1. **Bracket a peak.** Sample elevation every 30 seconds. For consecutive
   samples at `t - 60`, `t - 30`, and `t`, a middle sample higher than the left
   and at least as high as the right starts refinement on `[t - 60, t]`.
   Looking for maxima does not require any sampled point to exceed the mask.
   This allows a short grazing interval between samples to be found, provided
   the coarse samples bracket its peak. Checking only sampled mask crossings
   would miss such an interval.
2. **Refine the peak by ternary search.** Evaluate at the two interior thirds
   `a = low + (high - low)/3` and `b = high - (high - low)/3`. Assuming a single
   smooth maximum in the bracket, `e(a) < e(b)` rules out the portion left of
   `a`; otherwise discard the portion right of `b`. Each iteration retains
   two thirds of the time interval. A 60-second bracket reaches at most 0.25
   seconds after 14 iterations. Evaluate its midpoint for the peak elevation.
   Reject a peak below the minimum peak or one that merely touches the mask,
   because a tangency has zero reception duration.
3. **Bracket AOS and LOS separately.** Start at the above-mask peak and probe
   backward in one-minute increments for AOS, then forward from the peak for
   LOS. The first below-mask probe and the last at/above-mask probe enclose a
   root. The state names `inside` and `outside` describe elevation relative
   to the mask, not chronological order: outside is earlier for AOS and later
   for LOS.
4. **Bisect each root.** Evaluate the midpoint of the bracket and replace the
   endpoint on the same side of the mask. Equality belongs to `inside`.
   Continuity and the assumed single crossing on that flank keep the root
   enclosed while each step halves the interval. Stop when its width is at
   most 0.25 seconds and use the midpoint. A bracket initially 60 seconds wide
   needs eight halvings. Round only the final crossing times to whole seconds;
   omit an interval if its rounded endpoints coincide.
5. **Keep overlapping windows.** Return a complete pass when `LOS > start`
   and `AOS < end`, using the refined times for those comparisons. Neither
   endpoint is clipped to the requested span. The coarse scan extends 30
   minutes on either side to include peaks of boundary passes. A crossing
   walk that cannot find its below-mask endpoint within 30 minutes of the
   peak reports a propagation/search failure.

The ternary and bisection iterations are stored in `Refinement`, rather than
run in one long loop. A `Search::step` performs at most two ephemeris queries;
firmware yields between calls. This keeps orbital work from monopolizing the
cooperative runtime while actuator and network tasks need to run.

The shape assumptions and ±30 minute margins apply to the accepted nearly
circular HRPT LEOs. They do not prove completeness for arbitrary direction
functions, highly eccentric orbits, or multiple maxima inside one coarse
bracket. Increasing the tolerances' precision does not fix a violated shape
assumption.

### Numerical precision and forecast coverage

A 0.25-second crossing bracket gives a midpoint error of at most 0.125
seconds relative to the model's enclosed root. Rounding adds at most 0.5
seconds. These are numerical bounds under the search assumptions, not a
claim that measured RF acquisition agrees within one second. TLE fitting
error, drag changes, maneuvers, clock error, unmeasured antenna height, terrain
and atmospheric refraction can shift actual reception. The seven-day epoch
age limit is a data policy, not an error bound. No refraction or terrain
correction is included in `e(t)`.

A forecast's rolling-window validity is also a time calculation. To serve
every acquisition in the next six hours plus the one-minute polling allowance,
the searched span must satisfy:

```text
forecast_start <= now
now + WINDOW_S + SERVE_AHEAD_S <= forecast_end

latest_covered_now = forecast_end - WINDOW_S - SERVE_AHEAD_S
```

This is why the forecast searches twelve hours and refreshes as remaining
coverage falls below ten minutes. Age alone cannot establish completeness:
even a recently computed forecast would miss acquisitions outside its searched
span. A satellite's rows are added only after its entire search succeeds;
failure or quota overflow discards its partial results and records `Failed`.

### Verification

Host tests use the committed `weather-2026-10-07.tle` fixture, downloaded from
CelesTrak on 2026-10-07. Expected directions and crossings were independently
generated with satellite.js 6.0.1 using WGS84 `eciToEcf` / `ecfToLookAngles` and
a one-second scan from 2026-10-07 15:00 UTC at the default station. Agreement
tolerances are 0.001° for directions, 0.01° for peak elevation and one second
for AOS/LOS. Synthetic curves exercise grazing passes and window boundaries;
packet fixtures exercise NTP rejection, delay compensation and era rollover.
HTTP framing is fed whole and at every split point, including bytes beyond
Content-Length, chunked bodies closed mid-chunk and conflicting framing. TLE
tests try every byte value in the checksum column and `nan`/`inf` fields with
valid checksums. The `/api/passes` JSON is pinned in
`tests/fixtures/passes-report.json`, which the dashboard tests also render.
These tests need no network. Hardware NTP reachability, RF acquisition and
execution timing still require validation on the installed Pico/receiver.

```bash
./scripts/test-host.sh
./scripts/test-dashboard.sh
./scripts/test-build-tools.sh
cargo build --locked -p ezal-firmware --release
```
