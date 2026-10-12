# Pass prediction

The firmware displays a rolling six-hour forecast at `/api/passes` and
in the dashboard. NTP synchronization starts after DHCP. TLE download waits
for valid UTC, then the firmware computes SGP4 reception windows independently
of dashboard connections. Forecast rows alone never authorize movement. The
firmware boots idle. T7/T8 integrate device-owned execution and a dashboard for
selecting, previewing and explicitly arming one pass. Board timing, complete-pass
acceptance and installation commissioning remain pending.

The [planned tracking behavior](#planned-tracking) below records the agreed
workflow and provisional commissioning values. Milestone sections below
distinguish implemented policy/integration from remaining acceptance and board validation.

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
  On 2026-10-11 the operator explicitly accepted this transport risk for tracking,
  superseding the earlier requirement for authenticated orbital downloads.
  Checksums, ID matching and age checks validate data but cannot establish its
  authenticity. No TLS stack or additional gateway service is required by T5.
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

## Planned tracking

Design decisions updated on 2026-10-11. The installation has an 80 cm
parabolic dish with a custom helical feed and can track over the top.
Calibration, loaded motor behavior and acceptable RF pointing loss remain
unmeasured. The numerical choices below are provisional engineering defaults;
they do not change the manual firmware's constants or establish measured
tracking performance.

### Selection, ownership and modes

- Keep one device-owned selected pass. Manual mode may have no selection;
  Tracking mode requires exactly one. Selecting a row previews its plan;
  entering Tracking arms it. There is no queue or automatic next-pass choice.
- Only the holder of the current exclusive controller lease may select,
  replace, arm, cancel, prepare immediately, or change mode. Other clients
  may observe position, deviation and tracking state. A valid lease cannot
  be displaced by another client's take-control request.
- Separate this operator lease from the tracking run. Browser disconnection,
  page suspension or lease expiry releases operator authority but leaves an
  armed or executing pass on the Pico. A new lease holder manages the existing
  run; obtaining that lease does not stop it or alter the selection. Manual
  movement still stops when its browser source expires.
- The local tracking task refreshes its own short actuator command leases.
  It must not depend on browser heartbeats to move. Source deadlines and
  hardware inhibition remain independently enforceable if that task stalls.
- Hide manual direction controls in Tracking mode and reject manual direction
  commands on the device as well. Keep Cancel and Switch to Manual available
  to the controller throughout planning, preparation and execution. Either
  action inhibits pending and active output and returns to idle Manual mode;
  the last selection/result may remain visible for inspection. A fresh arm
  action is required for another run.
- Bind commands to the operator lease generation; arm/replace requests also
  require the expected state revision. Cancel and Switch to Manual require
  the current controller lease but remain effective across plan revisions.
  Duplicate requests are idempotent. Replacing a pass cancels the old run
  before admitting its replacement; delayed work cannot revive an old run.
  Fault handling and physical power removal do not require an operator lease.

The initial interruption policy retains the existing network gate for a
device Wi-Fi/DHCP outage and boots disarmed after reset. Browser independence
is agreed; continuing through a device network outage is a separate policy
that remains to be decided. A failed internet refresh can use still-valid
cached orbital/time data while the local network remains available.

### Updating and freezing the plan

Automatically update the selected pass's plan at any point before preparation
starts, including while it is armed. Re-evaluate new TLEs, clock updates,
station/profile/calibration revisions and the latest measured starting
position. Show changes to acquisition time, feasibility and preparation time
without requiring the operator to select the same pass again.

Use a stable event identity associated with a satellite and pass, rather than
a table index or exact rounded AOS timestamp. Reconcile a refreshed forecast
with that event. If the event cannot be uniquely identified or its replacement
plan is invalid, prevent preparation and show the reason; never silently
select a different satellite or orbit.

Preparation begins only after atomically accepting the latest complete,
valid plan revision. This transition freezes its orbital elements, observer,
motion/calibration profile and chosen nominal mechanical route. Results from
older planning jobs are discarded. Subsequent TLE refreshes continue for the
forecast but do not replace this executing plan. Reacquisition computes a
reachable intercept using those same frozen inputs and permitted travel.
Changing installed calibration or mechanical limits during execution aborts
the run rather than applying incompatible coordinates.

Use fractional orbital time anchored to monotonic uptime for execution.
Account for UTC uncertainty in the pointing budget. A new clock sample must
not make the target jump: keep the execution anchor when the discrepancy
fits the reserved uncertainty, and stop with a timing reason when it does not.
Source age limits still apply through LOS even though the plan is frozen.

### Preparation and completion

Use scheduled pre-positioning, with an optional Prepare now action for an
armed pass from the leased controller:

```text
preparation_lead_s = max(60, estimated_movement_s + 20)
preparation_start = acquisition_time - preparation_lead_s
```

The 60 s floor and 20 s arrival margin are the initial choices. Estimate the
movement using the slower rates in the [motor profile](HARDWARE.md#provisional-tracking-profile),
current position, chosen azimuth route, backlash, startup, coast and relay
state. Account for concurrent axes and any sequential movement segments.
Recalculate the schedule with every valid pre-preparation plan update.
Selecting a pass by itself does not move the dish.

If the calculated start is already past, begin preparation when armed and
the path is valid. If arrival before AOS is no longer possible, show late
acquisition and apply the bounded intercept policy below. Do not report a
complete reception window when its start has been missed. Arriving at the
starting pose enters Ready; the simulation must include the relay availability
needed to start following at AOS, as well as the pose itself.

Normal phases are Armed, Preparing, Ready, Following, Reacquiring and Complete.
Cancellation and faults have explicit terminal reasons. At LOS, remove drive,
disarm and stay at the final position, allowing for normal mechanical coast.
There is no automatic park move. A cancellation holds the position reached
when it is serviced. Recording is always external; receiver tuning and
recording integration are outside this tracking change. Use the configured
elevation cutoff with no terrain/building obstruction model.

### Pointing tolerance and over-the-top planning

For initial estimates, `70 * wavelength / diameter` gives about 15.4° full
half-power beamwidth at 1.7 GHz for an 80 cm dish. The coefficient depends on
feed illumination; this is a model, not a measurement of the custom feed.
The corresponding simple main-lobe model gives about 0.45 dB loss at 3°
offset. Both approximations are described in
[JPL IPN report 42-216C, PDF page 9](https://www.nasa.gov/wp-content/uploads/2026/02/42-216c.pdf).

Start with a configurable 3° operational pointing-deviation target. Reserve
1° provisionally for alignment, orbital and clock uncertainty, and require
the combined modeled path offset and controller following error to fit within
the remaining 2°. Include feedback latency, quantization, backlash and
coasting in that controller simulation. Commissioning must establish whether
the uncertainty allocation is sufficient; mark predictions as provisional
while the relevant measurements are unknown. The budget is not a claim of
receiver lock or RF alignment inferred from motor feedback alone.

Compare complete conventional and over-the-top routes, including equivalent
azimuth positions within measured travel. Keep continuous mechanical angles
separate from sky bearings. The direction represented by `(A, E)` is also
represented by `(A + 180°, 180° - E)` when those motor positions are permitted.
Changing representation alone does not reduce the azimuth rate of a near
miss of zenith. An over-the-top transition must be a physically continuous
path whose actual sky error fits the available budget, including between
sample points. Refine sampling around zenith, reversals and travel limits;
do not decide feasibility from peak elevation alone. The keyhole rate
relationship is documented in [JPL antenna positioning, section 2.2](https://deepspace.jpl.nasa.gov/dsndocs/810-005/302/302E.pdf).

Evaluate the full preparation/following path using the same feedback
controller and actuator timing as execution, with bounded motion uncertainty
and duty accounting. Prefer smaller peak pointing error, then fewer reversals
and output transitions. A bounded planner that finds no supported trajectory
reports that result without claiming that every possible trajectory is
physically impossible. The firmware can develop this simulation using the
provisional motor profile while installation measurements are pending.

### Reacquisition and visible deviation

Attempt reacquisition when the measured dish direction departs from the
selected pass. The initial thresholds are engineering defaults:

| Condition | Behavior |
|---|---|
| Estimated pointing deviation exceeds 3° | Show the deviation warning immediately |
| Deviation remains above 3° for 1 s | Enter Reacquiring |
| Deviation is at most 2° for 2 s with fresh feedback | Return from Reacquiring to Following |
| An episode reaches 30 s without recovery, or LOS arrives | Stop/disarm and retain the reason |
| Feedback, motion supervision, time validity or another output permission fails | Inhibit and stop the run; reacquisition does not clear faults |

Search for the earliest reachable future pointing along the frozen pass,
within the earlier of the episode deadline and LOS, using current position,
relay state, remaining duty and the same travel constraints. Follow only a
validated intercept path; if no intercept is currently available, hold and
retry within that original deadline. Replanning an intercept does not extend
the episode deadline. Never chase an obsolete position or change to a
different pass. Planned gaps must be labelled with their predicted duration
and reacquisition opportunity before arming.

Always show the selected satellite/pass, phase, desired sky az/el, measured
mechanical az/el, current estimated pointing deviation in degrees and the
maximum recorded deviation. Derive sky deviation from the angle between
the measured boresight vector and the satellite direction, timestamp-matched
to the feedback sample; include paired-channel sampling skew in its uncertainty.
This remains meaningful through 360° azimuth wrapping and elevation above
90°. An optional secondary readout may show error against the planned motor
position so intentional keyhole offsets can be distinguished from lag.

During recovery, display Reacquiring, the intercept ETA and time spent outside
the deviation target. Use text as well as colour, and keep Cancel available
to the leased controller. Stale feedback shows an unavailable/stale deviation,
not a retained value presented as current. Preserve a final pass summary with
maximum deviation, time outside tolerance and completion/cancellation/fault
reason for every viewer. These values report pointing, not receiver lock.

### Implementation and verification

Keep planning, following control and the mode/lease state machine in the pure
core. Run live following on an independently serviced firmware task, separate
from TLE downloads and incremental planning. Bound planning memory and work
per step so it cannot delay actuator/feedback servicing. Use the validated
orbital-input boundary and the explicitly accepted HTTP transport above.

The shared output boundary now supervises direction/speed across short pulse
trains and reserves an explicit coast margin at endpoints. Retain those checks,
existing stop ordering, short movement leases and the hardware watchdog path
when evaluating shorter timings. Live timing changes still need commissioning.

Exercise actual controller/actuator behavior with synthetic and recorded
passes: exact zenith and near misses, azimuth overlap, pre-positioning from
both extremes, slower/faster motors, backlash, pulse supervision and stalls.
Cover bounded reacquisition success/failure and UI deviation, automatic plan
updates racing preparation/cancellation, stale client commands, exclusive
lease expiry/transfer with tracking continuing, clock steps, duty exhaustion
and LOS. Test with downloads, prediction and multiple viewers active. Compare
simulated and measured trajectories during commissioning using
[CALIBRATION.md](CALIBRATION.md).

## Tracking implementation tasks

These tasks implement the decisions above. A completed simulation task does
not enable automatic movement in firmware. Each task includes the tests or
measurements needed to close it; hardware-dependent tasks remain open until
commissioning supplies that evidence.

- [x] **T1 — Controller and simulation foundation.** Add checked
  motor profiles, continuous mechanical coordinates, spherical pointing
  deviation, a feedback-following controller and an incremental, allocation-free
  simulator using the production actuator. Model startup, coast, backlash,
  quantized/delayed feedback and energized duty. Exercise complete synthetic
  and recorded passes and report deviation, relay activity and explicit
  failures. Keep simulation assumptions visible and separate from a feasibility
  guarantee.
- [x] **T2 — Tracking-capable output supervision.** Extend direction/speed
  checks across short pulse trains; add coast-aware endpoints, configurable
  same-direction/reversal timing and shared per-axis duty accounting. Preserve
  existing manual stop ordering, leases and watchdog behavior. Test pulses,
  reversals, stale feedback, cancellation of preparation-style pulses and unknown
  duty on boot. Publish per-axis allowance/rest in telemetry and the dashboard.
  Depends on T1; relaxed live timing also requires T10 measurements.
- [x] **T3 — Bounded trajectory planner.** Generate continuous conventional,
  azimuth-overlap and over-the-top candidates; refine near zenith and limits.
  Evaluate preparation and the entire pass with the shared controller across
  motion uncertainty. Report supported feasibility, peak deviation, gaps and
  preparation time. Include relay availability at AOS; bound memory/work per
  step and check between trajectory samples, including feedback noise/channel
  skew in the uncertainty budget. Tune follower hysteresis/lead
  against the slow/nominal/fast scenarios to reduce the baseline controller's
  reversals without hiding sky error. Depends on T1 and T2.
- [x] **T4 — Single-pass lifecycle and exclusive operator lease.** Implement
  device-owned selection, arm/prepare/follow/complete, stable pass identity,
  revisioned updates until preparation, atomic plan freeze and idempotent
  cancellation. Tracking survives browser/lease expiry; only the current
  lease holder can mutate it. Test takeover refusal, transfer, stale commands
  and update/cancel/preparation races. Depends on T3's plan contract.
- [x] **T5 — Execution clock and validated orbital inputs.** Retain the explicitly
  accepted HTTP feed and add a fractional UTC anchor tied to monotonic uptime.
  Freeze execution inputs, enforce source validity through LOS and account
  for timing uncertainty. Test clock steps, stale sources, failed refreshes,
  boot disarming and the initial stop-on-device-network-loss policy.
- [x] **T6 — Bounded reacquisition.** Apply deviation warning/hysteresis,
  search reachable future intercepts on the frozen pass and enforce the
  original 30 s episode deadline/LOS. Retain fault inhibition, cancellation
  and duty state; report recovery ETA and terminal reasons. Test recoverable
  lag, impossible intercepts, repeated replans and feedback failure. Depends
  on T2–T5.
- [x] **T6a — Partial-pass admission.** Extend the planner/publication contract
  to permit a selection with validated late acquisition or predicted gaps,
  including gap duration, a bounded recovery opportunity and the remaining
  reception window. Reuse T6's intercept validation; never label partial
  reception as complete-window support or arm an unsupported diagnostic route.
  Test late arming and planned keyhole gaps. Depends on T3–T6; required before
  completing the agreed partial-pass behavior in T7/T8.
- [ ] **T7 — Independent firmware execution.** Software implemented; board
  validation pending. Wire tracking into its own
  serviced task and the existing output boundary with short internal command
  leases. Keep downloads and incremental planning out of the live control
  task. Check Pico memory, execution time and watchdog behavior with prediction,
  downloads and multiple viewers active. Depends on T2–T6a.
- [x] **T8 — Tracking API and dashboard.** Add selection, feasibility preview,
  Manual/Tracking mode, scheduled preparation/Prepare now and Cancel. Hide
  manual controls while tracking and reject their commands on the device.
  Show phase, desired/measured pointing, timestamp-matched spherical deviation,
  stale feedback, recovery progress and the final summary. Test owner/viewer
  permissions and reconnects. Depends on T4, T6/T6a and T7.
- [x] **T9 — Complete-pass acceptance.** Host acceptance complete; see the
  [results and prediction limits](#complete-pass-acceptance-t9). Ran recorded and
  synthetic exact-zenith/near-zenith passes, azimuth wraps, extreme preparation positions,
  slow/fast motor profiles, cancellations, faults, duty exhaustion and LOS.
  Confirmed holding the final position, no automatic next pass, and external
  recording only. Compared the planner's predictions with the execution path.
  Depends on T3–T8.
- [ ] **T10 — Installation commissioning.** Follow [CALIBRATION.md](CALIBRATION.md)
  to identify hardware, calibrate full travel/sky pointing and measure loaded
  rates, useful pulses, coast, backlash and RF pointing tolerance. Replace
  provisional values, validate GPIO/watchdog release and perform supervised
  passes before enabling routine automatic tracking. Measurement work can
  start alongside T1; final acceptance depends on T9.

### Current planning milestone

[`tracking`](../crates/ezal-core/src/tracking.rs) now supplies a pure-core
incremental planner, feedback follower and simulator. The planner searches 24
conventional, inverted and over-the-top/azimuth-overlap candidates. It unwraps
azimuth continuously and refines geometry near zenith and the azimuth wrap;
linear interpolation stays inside the measured mechanical range. Over-the-top
bridges at 80°, 85° and 88° can intentionally offset the sky direction. Every
following-error check includes that offset.

Run the offline [planning example](../crates/ezal-core/examples/tracking-plan.rs)
or the individual [controller replays](../crates/ezal-core/examples/tracking-sim.rs)
inside the development shell:

```bash
TRACKING_HOST=$(rustc --print host-tuple)
cargo run --locked -p ezal-core --target "$TRACKING_HOST" --example tracking-plan
cargo run --locked -p ezal-core --target "$TRACKING_HOST" --example tracking-sim
# Compare proposed timing offline; this does not configure the device:
cargo run --locked -p ezal-core --target "$TRACKING_HOST" --example tracking-plan -- --candidate-timing
cargo run --locked -p ezal-core --target "$TRACKING_HOST" --example tracking-sim -- --candidate-timing
```

The planner requires a serviced, settled copy of the output owner's actuator,
network generation and fresh feedback acquired after coast. It copies relay
availability, duty/rest and supervision history; it never mutates live outputs.
Preparation probes establish the worst readiness time, including a 2 s settled
dwell. The scheduled lead is `max(60 s, preparation + 20 s)`, clipped to the
current time. Each complete verification then simulates scheduled preparation
and the pass in one run, retaining on-time and relay holds at AOS. Far-future
idle waiting is skipped with its actual rest credit; no hours-long simulation
loop is needed. Late preparation, duty exhaustion, motion faults and travel
violations produce explicit rejections, including the scenario and phase.

Nine default scenarios cover nominal/slow/fast rates, mixed axis rates,
initial gearbox engagement, fixed/alternating one-code measurement offsets,
0–150 ms acquisition latency and up to 150 ms channel skew. A0 is captured
first; the feedback timestamp remains that oldest capture time. The fixed
nominal follower accounts for minimum pulse completion, target velocity,
startup and coast, requires an additional 0.15° predicted improvement, and
waits for post-coast feedback before another correction. The scenarios still
share provisional 300 ms startup, 500 ms coast and 1° backlash assumptions.
They do not cover every intermediate parameter combination or prove real
hardware performance. Loaded measurements must replace them.

The modeled allocation is 2° within the provisional 3° total target, reserving
1° for alignment/orbit/time uncertainty. At most 20 ms separates checks, with
earlier output/acquisition deadlines and exact AOS/LOS boundaries. Between
adjacent checks, the planner bounds error using the axis-rate vector and an
explicit assumed sky-rate bound (default 2°/s), plus 0.002° numerical allowance.
Observed violations of the sky-rate assumption reject the job; unobserved
violations remain outside the guarantee. This is conditional support across
the supplied finite scenarios. The execution-input milestone below applies
the separate timing reserve and whole-window source-validity checks.

Any interval not established within budget is a predicted gap. The report
retains the worst scenario's total gap time and up to 16 intervals, explicitly
flagging truncation. Unsupported attempts cannot become a `TrackingPlan`.
No-gap support requires every scenario to complete through final coast. T6
recovers a frozen run and handles preparation that misses AOS. T6a adds a
separate partial-reception claim only after validating a bounded recovery and
the remaining window; unsupported diagnostic attempts still cannot authorize
movement. Complete reception is preferred whenever this search finds it.
Cancellation or a source/work/storage failure invalidates the entire job,
including earlier candidates. Exhausting this finite search means no supported
route was found, not that every possible physical route is impossible.

The planner stores at most 2,048 route points, nine scenarios and 16 gap
intervals. One step performs at most one propagation and one simulator service,
or bounded route bookkeeping; the whole job stops at 20 million steps.
Preparation probes are limited to 300 s and reception windows to 30 minutes.
T6a adds a second simulation state for prefix-to-recovery validation; the
`tracking-plan` example prints current host planner storage (82,464 bytes
on x86-64 at T6a). The default synthetic zenith job takes about 2.75 million
steps. Embedded memory placement
and execution time under concurrent firmware load must be checked in T7
before integration.

Current development results, using synthetic full travel and confirmed-AC duty:

| Case | Result |
|---|---|
| Nominal zenith, over the top, either relay policy | 0.85° sampled peak, zero reversals, 51 s elevation drive |
| Planned zenith, installed relay timing, nine scenarios | Supported: 1.46° sampled / 1.53° bounded peak; 60 s preparation lead |
| Zenith with elevation limited to 90° | Unsupported keyhole gap |
| Planned 88° peak, nine scenarios | No supported route; reported gaps/faults depend on candidate |
| Recorded 70.75° peak, nominal controller replay | 2.08° sampled peak, one elevation reversal; exceeds modeled allowance |
| Recorded pass, all nine planning scenarios | Rejected during preparation by speed supervision under alternating noise |

The shorter-timing nominal zenith replay previously made 203 reversals and
used 232 s of elevation drive. Pulse-aware release removes that behavior;
it does not make every recorded pass fit the pointing allocation or justify
relaxing live limits. The examples never extend the installed 0–360°/0–90°
calibration. Live firmware retains 500 ms on / 2 s off timing, the unidentified
hardware 180 s duty policy, and 900 s initial rest.

[Planner tests](../crates/ezal-core/tests/planner.rs) exercise full zenith/recorded
passes, wrap continuity, one-propagation steps, preparation/duty carryover,
retained relay holds, far-future rest, late starts, cancellation and bounded
storage. [Controller tests](../crates/ezal-core/tests/tracking.rs) cover plant
physics, noisy/skewed feedback, relay timing and pulse hysteresis.
[Output-policy tests](../crates/ezal-core/tests/output_policy.rs) verify that
post-release coast can prove progress without forgiving stalls, reversed
feedback or movement first observed after a long idle.

This milestone established the offline planner/follower contract used by the
lifecycle below. T7/T8 subsequently integrated execution and the dashboard.

### Current lifecycle milestone

[`TrackingSession`](../crates/ezal-core/src/tracking/lifecycle.rs) implements
the device-owned selection and lifecycle in the pure core. It starts disarmed
in Manual mode, owns at most one selection and accepts only a supported plan
when arming. Selection alone previews a pass. Tracking rejects manual movement
and manual stop messages; the current operator uses Cancel or Manual to stop
the run. These actions inhibit active/pending output and invalidate the run
generation before fresh manual commands can be admitted.

[`OperatorLease`](../crates/ezal-core/src/operator.rs) gives one connection a
provisional five-second renewable grant. Another connection cannot displace
it. Expiry is exclusive; a heartbeat at the deadline cannot resurrect the
old grant. Repeated claims and read-only snapshots do not renew it. Release or
expiry inhibits manual motion, while an armed or executing tracking run keeps
its own authority. A new controller can manage that existing run; an old
socket's close or delayed request cannot release the new grant.

Every operator mutation carries the connection-bound lease generation and a
strictly increasing request number. Select, Arm and Prepare now also require
the observed state revision. Cancel and Manual deliberately ignore that
revision so automatic replanning cannot prevent a stop. An exact retry of the
last request returns its original outcome; older requests are rejected without
repeating their effect. The transport must bind the token to its actual
connection identity: lease serials provide ordering, not authentication.

The selected event gets an opaque identity that survives forecast changes.
Association requires the same catalog number, overlapping reception windows,
and AOS/LOS within 300 seconds of the **original** selected window. Repeated
refreshes cannot walk the association to another orbit. Missing, ambiguous,
invalid or over-capacity forecasts block preparation with explicit reasons.
The comparison uses a complete forecast and is independent of row order.

Each planning ticket binds the selection to orbital, observer, calibration,
motion, clock and starting-state revisions, plus the preparation schedule.
Publishing newer inputs immediately removes the preceding plan's permission
to prepare. Only the latest job can publish, even if an older successful job
finishes later. Manual movement also invalidates a preview's starting pose.
An unsupported or failed replacement never falls back to an older success.
Before preparation, jobs can update a selection in either mode. At preparation,
the latest accepted plan and its owned input snapshot become immutable in one
transition. Later forecast/observer/clock updates leave that snapshot intact;
an installed calibration or motion-policy change stops the run.

| Event | Core behavior |
|---|---|
| Scheduled preparation becomes due | Freeze the current supported plan and issue a new executor generation |
| Prepare now | Request a new `Immediate` plan; start only after that schedule passes evaluation |
| Executor confirms measured readiness | Enter Ready; retain the same frozen route and inputs |
| AOS while Ready | Enter Following independently of operator presence |
| AOS during frozen preparation without readiness | Enter bounded late acquisition, with its deadline counted from AOS |
| AOS without a valid frozen plan | Stop with a late-preparation reason; never execute an older/unsupported route |
| LOS while Following | Inhibit, return to Manual and retain the result/selection; no park or next pass |
| Cancel, replacement or interruption | Invalidate old jobs/callbacks/commands and preserve an unconsumed stop |

Prepare now evaluates duty and relay history at the early start; it cannot
spend rest that a scheduled plan would have gained later. A ready dish can
remain at the preparation pose beyond the 300-second acquisition probe limit;
the complete planning job still has its fixed work bound. Long early waits
can therefore return an explicit work-limit failure rather than run unbounded.

The input owner supplies an exclusive `freeze_before_ms` validity deadline.
T4 checks that deadline, complete-window plan consistency and remaining
preparation time. T5 now supplies the validated orbital/observer/clock snapshot,
checks it through LOS and interrupts a frozen run if its source or timing
becomes unusable. The executor must confirm readiness from real settled
feedback and relay state; the callback is not a browser operation.

The output mailbox keeps inhibition ahead of refreshes. Executor commands are
bound to the current run generation and retain their original short source
deadline, at most 750 ms; LOS/cancellation checks run again on drain. The same
production actuator remains responsible for feedback, network, motion, relay
and duty constraints, so changing owners never resets physical accounting.
[Lifecycle tests](../crates/ezal-core/tests/tracking_lifecycle.rs) cover browser
loss, lease transfer, stale generations, duplicate requests, forecast matching,
plan-update/freeze/cancel races, early preparation, manual admission and LOS.
A test feeds cancellation through the real actuator at a queued activation's
deadline and verifies that no pulse starts and consumed duty is retained.

T7 now embeds this runtime in `CoordinationState`, replacing the separate
manual authority/mailbox. Socket cleanup releases the operator lease and leaves
a device-owned run intact. Only the device output task applies GPIOs and feeds
the watchdog. T8 exposes selection and arming through the browser.

### Current execution-input milestone

T5 adds [`OrbitalDownload`, `OrbitalCatalog` and `ExecutionInputs`](../crates/ezal-core/src/tracking/inputs.rs).
The download wrapper composes the existing bounded HTTP decoder and TLE parser.
Only a complete framed response can produce a catalog publication; the first
framing error stays latched. Truncation, timeout or non-200 responses cannot
publish a partially received catalog. Malformed or older individual entries
retain both their preceding elements and their original publication metadata.
Each accepted entry has a device-owned revision and monotonic completion time.
Reading or failing to refresh a cache changes neither. Orbital freshness uses
the **TLE epoch**, not its download time.

A planning snapshot owns the selected satellite's validated SGP4 constants,
observer geometry, clock anchor, job ticket, window and planning assumptions.
There are no mutable orbit/clock accessors. Publishing a result requires the
same selection/revisions, satellite, fractional AOS/LOS mapping and configuration
as that snapshot. The internal planner uses the snapshot as its ephemeris;
the browser cannot supply an alternative orbit or planning report. The existing
one-day future-epoch and seven-day age limits must cover now through LOS,
expanded by the admitted timing error. These remain operational data limits,
not measured orbital accuracy.

[`ntp`](../crates/ezal-core/src/ntp.rs) retains millisecond UTC, the original
monotonic receipt instant, a never-reused synchronization revision and an error
estimate. That estimate adds half the measured network delay after server
processing, half the nonnegative advertised root delay, root dispersion,
server precision and a rounding allowance. The packet fields follow
[RFC 5905](https://www.rfc-editor.org/rfc/rfc5905.html#section-7.3).
It assumes honest time service and bounded local clock drift; SNTP provides no
cryptographic time authentication. Samples announcing a leap remain usable
for the advisory display but cannot authorize tracking.

[`ExecutionClock`](../crates/ezal-core/src/tracking/clock.rs) reserves an explicit
share of the planner's existing uncertainty allocation:

| Provisional assumption | Default |
|---|---|
| Local oscillator drift bound | 100 ppm, pending measurement |
| Maximum sample age | Six hours, exclusive; angular error usually limits it earlier |
| Alignment/orbit share of the existing 1° reserve | 0.5° |
| Remaining timing share | 0.5° |
| Planner's assumed maximum sky rate | 2°/s |
| Resulting maximum timing error | 250 ms |

Error grows from the original sample by `elapsed_ms × drift_ppm / 1,000,000`.
At 100 ppm that consumes 6 ms per minute. A sample with 50 ms initial error
therefore covers about 33 minutes from receipt to LOS under these defaults.
Successful NTP refreshes now run every five minutes; failed attempts retry in
one minute without renewing the old anchor. These defaults require hardware
and RF commissioning along with the motor/pointing assumptions.

Early previews check the sample's current validity so a pass can be selected
and armed hours ahead while source/clock updates trigger replacement planning.
Immediately before preparation, the full error and age bounds must fit through
LOS. An inadequate replacement blocks preparation with an input/timing reason;
an older successful plan cannot take its place. The input owner supplies the
exclusive admission deadline automatically.

After preparation begins, propagation always uses the same millisecond anchor.
A new NTP sample is compared with that frozen mapping; its discrepancy plus
its uncertainty/drift through LOS must fit the timing allocation. Acceptable
samples validate the run without shifting targets or deadlines. Excessive
positive or negative corrections, missing time, stale/reversed clock
publications and unusable source data inhibit the run with an explicit reason.
Repeated small corrections cannot cumulatively move the frozen anchor.

[`TrackingRuntime`](../crates/ezal-core/src/tracking/runtime.rs) owns the lifecycle
and enforces this input boundary before preparation, internal command admission
and mailbox drain. It accepts coherent device observations, with the existing
timestamped `NetworkGate`, rather than a cached connected flag. Link/address
loss, expired supervision, or a recovery generation change stops an armed or
executing run and invalidates queued output; recovery cannot resume it.
Browser loss and failed internet refreshes preserve a run while its cached
inputs and device network permission remain valid. Boot is always Manual,
without a selection, run or persisted permission.

[Clock tests](../crates/ezal-core/tests/tracking_clock.rs) exercise fractional
conversion, signed corrections, cumulative drift, expiry, leap announcements
and checked arithmetic. [Input/runtime tests](../crates/ezal-core/tests/tracking_inputs.rs)
use the recorded orbital fixture and real planner to test source binding,
atomic download publication, through-LOS age, refresh/freeze ordering,
browser-independent execution, network interruption, queued-output inhibition
and boot disarming.

Firmware now uses the framed catalog publication path and quantified NTP
samples. Following/output ownership is integrated in T7 and UI wiring in T8.
That integration must publish complete source revisions **before** servicing a
simultaneous preparation deadline, use `TrackingRuntime` rather than bypassing
it with the generic lifecycle, and provide current clock/network observations
on every output drain. Planning remains device-internal and must consume the
same `ExecutionInputs` snapshot that accompanies its completed result.

### Current reacquisition milestone

T6 adds [timestamp-matched deviation](../crates/ezal-core/src/tracking/deviation.rs)
and a [bounded intercept search](../crates/ezal-core/src/tracking/recovery.rs) to
the existing `TrackingRuntime`. T7/T8 subsequently connect this core policy to
firmware execution and the dashboard.

Fresh paired feedback is compared with the frozen ephemeris at the oldest
channel's acquisition timestamp. The spherical estimate handles azimuth
overlap and over-the-top equivalence. Telemetry includes an explicit uncertainty
estimate: the plan's source/alignment/time reserve, calibration-derived ADC
resolution, and worst-case azimuth travel during the 150 ms paired-read bound.
The warning/recovery thresholds apply to the estimated separation, not a claim
of measured RF pointing accuracy. Missing/stale/failed feedback makes the current
reading unavailable and stops execution; the last maximum remains in the summary.

Distinct fresh samples must establish more than the plan's warning tolerance
(default 3°) for one second before recovery begins. Recovery requires at most
2° for two seconds of fresh samples after the validated route join. Repeated
reads of one sample do not accumulate dwell, and unobserved intervals break it.
The runtime also retains observed time outside tolerance and a final summary
bound to the exact run, event and termination reason. Selecting or executing
another pass does not relabel the preceding final summary.

Each episode owns `min(entry + 30 s, LOS)`. Preparation that was frozen but
did not establish readiness at AOS enters a late-acquisition episode counted
from AOS, even if service occurs later. Retries and browser lease expiry never
move these deadlines. The lifecycle distinguishes recovery timeout, LOS during
recovery, feedback failure, motion fault, cancellation and input invalidation.

The intercept procedure is deliberately finite:

1. Inhibit and wait for coast to end and a post-coast feedback pair. Copy the
   coherently serviced actuator, retaining relay holds, supervision and duty.
2. Set a launch two seconds ahead while incremental computation runs. Search
   future joins at one-second intervals on the **same frozen mechanical route**.
   Slew toward each future point, then rejoin the route at its original time.
3. Evaluate every configured motor/feedback scenario through the remaining
   pass and final coast. Each step performs at most one propagation and one
   simulation service, with the existing 20-million-step ceiling. From the
   join onward, intersample pointing bounds must fit the smaller of 2° and the
   plan's modeled allocation. Leave time for the fresh-feedback recovery dwell.
4. Accept the earliest qualifying grid candidate. At launch, require the same
   measured pose, policy, network generation and uninterrupted idle history.
   A changed start, missed launch or missed control deadline discards the path.
5. Follow only that validated target using the existing follower and actuator.
   If measured recovery misses the predicted join plus two-second dwell and
   feedback allowance, inhibit and retry. Unsuccessful searches retry after
   one second within the original episode; they never reset duty or faults.

“Earliest” means earliest in this finite search. These results remain conditional
on the provisional scenarios; they do not prove every intermediate motor
response or exclude a better trajectory. T6a uses the same validator to admit
explicitly partial reception before arming, as described below.

Firmware integration must publish feedback/motion health before issuing or
draining commands, including during preparation. `observe_tracking` supervises
following; `reacquisition_command` generates recovery commands; the independently
yielding worker calls `step_reacquisition`. A validated launch must be generated
and drained at its scheduled millisecond, and subsequent recovery control must
meet the frozen cadence (currently at most 20 ms). Late work holds/retries;
it never blocks the physical output task or applies an obsolete intercept.
The output owner must also report every actual relay transition to the active
follower. On return to Following, clear the normal follower's target-velocity
history while retaining its observed relay/coast history. GPIO, live planning
latency, memory, watchdog behavior and the feasibility of the two-second
computation lead must be measured in T7 before enabling tracking.

[Core recovery tests](../crates/ezal-core/src/tracking/recovery/tests.rs) exercise
all nine scenarios, remaining-pass source failure, impossible/duty-limited
intercepts, preserved overlap/over-the-top routes, changed starting history and
immutable episode deadlines. [Runtime tests](../crates/ezal-core/tests/common/tracking_recovery.rs)
use the recorded TLE fixture, a separate motor model and delayed feedback to
exercise successful recovery, late preparation, missed measured intercepts,
late computation/output, repeated retries, cancellation, feedback/network/motion
failure and retained final summaries.

## Implemented partial-pass admission (T6a)

The [reception contract](../crates/ezal-core/src/tracking/reception.rs) separates
`Complete` from `Partial`. A partial plan retains the original pass identity,
AOS, LOS and mechanical route, and exposes:

- The predicted missed interval and its duration, relative to original AOS.
- A fixed recovery entry, its earlier-of-30-seconds-or-LOS deadline, and the
  earliest/latest validated route joins across the evaluated starting cases.
- A usable prefix when available, the remaining usable suffix, and total
  predicted reception time. The missed interval includes the two-second
  recovery dwell and 500 ms feedback allowance after the latest route join.

`AllowPartial` is the default admission policy; `CompleteOnly` preserves the
complete-window requirement. The [planner's second search](../crates/ezal-core/src/tracking/planner/partial.rs)
runs only after no complete route qualifies (or selection is already past
AOS). A nominal failed route does not become supported by changing its label.
For every starting scenario, the planner replays preparation and the supported
prefix, inhibits and waits for coast plus settled feedback, then copies that
actuator into T6's intercept search. Every recovery scenario must meet travel,
relay, duty, motion-supervision and pointing constraints through original LOS
and final coast. Prefix duty and relay history carry into recovery. A fault
in the nominal continuation can only be avoided if this different recovery
trajectory passes the unchanged checks.

This finite search admits **one consolidated missed interval**: initial late
acquisition, or a supported prefix followed by a bounded planned gap and a
supported suffix. Several nominal gaps may fit within that interval. Separate
gaps needing additional planned episodes remain unsupported. All complete
routes are preferred to partial reception; within the partial round the first
qualifying route wins. The nested work shares the original 20-million-step
job limit; each step still performs at most one propagation and one simulation
service. Cancellation and source/work/storage failure discard the whole job.

For initial late acquisition, the fixed entry is `max(original AOS, snapshot
time + 5 s)`. Selection only previews it. Explicit arming allows the device to
freeze and begin recovery at that entry; no preparation movement occurs first.
A missed entry expires the plan rather than shifting its validated schedule.
Automatic input/forecast revisions may replace it before freezing. The input
boundary now permits previews after AOS, but still checks source and timing
validity through original LOS. The five-second admission lead, like T6's
two-second intercept computation lead, needs Pico timing validation in T7.

Publication distinguishes `SupportedPartial`, validates the gap/reception
metadata and rejects stale or inconsistent results. Planned gaps enter
`Reacquiring` and clear queued following commands at their fixed entry. The
executor then revalidates a live intercept from actual settled feedback and
actuator history. An earlier unexpected recovery keeps its original token and
deadline when the planned gap arrives. Only the leased controller can cancel;
browser/lease loss does not end a run. Final summaries retain the original
window and complete/partial prediction alongside measured deviation and the
actual termination reason. Reaching LOS does not turn a partial forecast into
a complete-reception claim.

[Admission tests](../crates/ezal-core/tests/common/tracking_partial.rs) cover
late starts before/after AOS across all nine default cases, an explicitly
nominal-profile keyhole gap, complete OTT preference, insufficient remaining
time/duty, separated gaps, expired/invalid publication, fixed episode deadlines,
cancellation and one-propagation-per-step yielding. The same keyhole fixture
remains unsupported with all nine default cases because a noisy fast-axis
case trips motion supervision. Partial admission does not relax that policy.
The [real-orbit runtime test](../crates/ezal-core/tests/common/tracking_recovery.rs)
executes an admitted late plan through recovery to LOS with delayed feedback
and verifies that the final summary retains its partial prediction.
The offline example's static late-acquisition case qualifies across all nine
scenarios: 13.65 s missed and 76.35 s usable in the original 90 s window, with
a fixed 30 s recovery episode. These are model results, not RF measurements.

Validation: 284 Rust tests (including the doctest), 50 dashboard tests, host and
Pico Clippy, formatting and the Pico release build. Firmware was manual-only
at this milestone; no firmware was flashed.

The following milestone integrates this policy into firmware.

## Independent firmware execution (T7)

The firmware now has one device owner: `CoordinationState` contains
`TrackingRuntime`, and both manual requests and tracking use its exclusive
operator lease and sticky command mailbox. The old transferable manual authority
has been removed. A viewer cannot take an unexpired five-second lease. A
single-use, one-second WebSocket Ping/Pong challenge renews only the original
grant; reads, repeated claims, replayed pongs and expired grants cannot renew it.
Closing the browser releases its lease without stopping an armed pass.

The existing GPIO task services preparation, measured readiness, normal
following and bounded recovery independently of browsers. It first drains current
inhibition/permissions, services and physically applies the actuator, then generates
a tracking command from that coherent snapshot. A second drain uses the actual
current time. Every intermediate output transition reaches GPIO and follower
history. Preparation requires two seconds of distinct fresh, settled observations;
recovery hands its physical pulse/coast history to normal following on success.
Completion/cancellation hold the final position without parking or another pass.

Idle service remains 100 ms. Armed/live tracking aims for 10 ms, shortened by
relay, feedback, network, preparation, recovery-launch and LOS deadlines. The
750 ms internal lease still covers the required 500 ms relay activation. A
separate cadence gate stops normal tracking if service exceeds the plan's 20 ms
maximum; stale commands cannot start a pending relay on drain. Recovery discards
late work and retries within the original episode. A stopped whole executor
cannot apply a cut until it resumes or the 500 ms hardware watchdog resets it;
20 ms is an execution requirement, not a demonstrated physical release time.
Only the GPIO task feeds that watchdog, after both output services.

The new `tracking` task owns a statically bounded preview planner and steps both
preview and recovery work cooperatively. Each bounded unit yields without an
artificial millisecond sleep. Downloads yield after every bounded parsing chunk;
forecasting retains its own cooperative task. Complete forecasts and their exact
orbital snapshots publish atomically. Clock corrections and changed starting
pose/duty/history invalidate preview work before it can freeze; fresh ADC
*timestamps alone* do not restart planning. Installed calibration/motion settings
remain compile-time revisions. Noisy changing starting positions can prevent a
stable preview from completing; commissioning must check this with real feedback.
All shared tracking/forecast users run on the single core-0 thread executor.
Thread-mode borrows keep interrupts enabled and never span an await or I/O.

The production coordination/execution host tests run real-orbit preparation,
following and LOS with delayed feedback, eight viewers, incremental forecasts
and streamed download parsing. They also cover owner-only cancellation, browser
loss, changed-start/preparation races, stale control/drain inhibition, late
admission, and recovery-to-following handoff. These establish ordering and
bounded-work behavior, not Pico execution speed or RF reception.

The Pico release image uses approximately 262 KiB of static RAM, including an
approximately 82 KiB tracking worker pool and eight web sessions. Removing a
whole-planner move reduced the worker poll's local stack allocation from about
200 KiB to 62 KiB. Report publication has another roughly 53 KiB frame;
`memory.x` now requires at least 192 KiB of free shared stack capacity at link
time. This is a capacity/assembly check, not a measured stack high-water mark.

A once-per-minute diagnostic reports maximum output service duration, active
service gap, preview/recovery step times, whole worker poll time, and work count.
**No firmware was flashed and no board timing or watchdog-release measurement
was taken for this implementation.** The [board procedure](DEVELOPMENT.md#tracking-execution-load-check)
must still verify the 20 ms maximum interval, two-second recovery computation
lead, five-second late-admission lead, stack headroom and physical watchdog
release under concurrent network/viewer load. T7 remains open for those checks.
T8 below supplies the tracking API/dashboard and T9 records host acceptance;
routine hardware tracking still requires T10 commissioning and these board checks.

Software validation: 292 Rust tests (including the doctest), 50 dashboard tests,
host/Pico Clippy, formatting, build-tool checks and the Pico release build pass.
The final release has 40,792 bytes of `.data`, 226,756 bytes of `.bss` and
1,024 bytes of `.uninit`: 268,572 bytes of static RAM and 263,908 bytes remaining
for the shared stack. The 192 KiB linker reserve passes.

## Tracking API and dashboard (T8)

The dashboard selects exactly one device-offered pass by NORAD and its complete
AOS/LOS window. Selection stops any held manual input and previews feasibility;
it does not arm or reposition the dish. **Enter Tracking** explicitly arms the
supported full or partial plan. The preview identifies the route, modeled error
bound, provisional pointing allowance, predicted reception/missed intervals and
UTC preparation time. Partial reception is also named on the arming button.

While Armed, forecasts and input changes automatically replace the preview.
**Prepare now** requests a separately evaluated earlier schedule. Preparation
freezes the accepted route and source as before; the UI follows the device phase
and hides manual controls throughout Tracking mode. The device independently
rejects manual commands, including the old manual `stop` message. **Cancel
tracking** or **Return to Manual** uses the current controller's lease to inhibit
tracking; completion and cancellation hold the final position. A selection is
retained for inspection, and never automatically rearmed.

All viewers receive the same selection, preview, phase, measured/desired
pointing at the same ADC sample time, spherical error, uncertainty allowance,
peak error and observed time outside tolerance. Feedback/sample expiry removes
the current numeric deviation. Reacquisition shows attempt count, search/hold
reason, the original episode's remaining time and a predicted intercept ETA;
a predicted join is explicitly distinguished from measured recovery. The last
finished run's summary survives subsequent previews and runs. Reception windows
remain model predictions; receiver lock and recording are external.

Only the currently leased connection can mutate tracking. Disconnect, page
hiding and telemetry loss never generate an automatic tracking cancellation;
reconnect restores the device's state and never resends an earlier intent.
Selection remains visible even if its forecast row disappears or HTTP polling
fails. A rejected or unacknowledged action is visible, and the operator can
cancel across changing plan revisions, stale observations or a pending action.

### Wire contract

The existing `/ws` connection carries manual and tracking traffic. `/api/passes`
now includes each row's `norad_id` as a decimal string. New intent is bounded to
256 bytes including full-width identities:

```text
track:<proof>:<lease>:<sequence>:<revision>:select:<norad>:<aos_unix_s>:<los_unix_s>
track:<proof>:<lease>:<sequence>:<revision>:arm
track:<proof>:<lease>:<sequence>:<revision>:prepare
track:<proof>:<lease>:<sequence>:<revision>:cancel
track:<proof>:<lease>:<sequence>:<revision>:manual
```

`proof` comes from `telemetry.freshness.token`; `lease` and `revision` come from
`telemetry.tracking.operator` and `.revision`. All identities remain decimal
strings in JavaScript; intent sequences use `BigInt`. The server derives the
connection identity from the socket and checks the live lease generation.
Select, Arm and Prepare require a still-valid one-second observation and the
current revision. Select also checks the current device forecast before replacing
anything. Cancel/Manual ignore observation age and revision but still enforce
lease ownership and ordered, nonzero sequence numbers. Exact retries within the
valid admission boundary are idempotent; the dashboard never retries actions
automatically. Network recovery invalidates prior observation proofs.

A `tracking_result` acknowledgement contains `sequence`, `ok`, `revision` and
`error`. It carries no freshness proof. The subsequent coherent telemetry frame
is authoritative for mode, selection and execution state; the browser waits for
it before admitting further movement. The `tracking` projection includes:

- `revision`, `operator`, `mode`, `phase`, `run` and at most one `selection`;
- `plan.status`, an explicit blocked `reason`, and a small supported `preview`;
- `feedback_stale`, `feedback_age_ms`, `deviation`, `recovery`, `termination`
  and the retained `summary`.

Reception windows and missed intervals are milliseconds relative to their own
`aos_unix_s`. Recovery times are remaining milliseconds at the device observation.
Diagnostic reasons are closed Rust enum representations, rendered as text in the
browser. No browser-provided string is inserted into server JSON or HTML.

The projection copies less than 1 KiB, performs no propagation or transitions,
and cannot renew a lease. It is captured with feedback/output telemetry under one
thread-mode borrow, then encoded outside the borrow in a bounded 4096-byte buffer.
No route or orbital snapshot is copied per web client. Failed encoding closes the
session; it cannot revoke an independently running tracking execution.

Validation covers wire bounds and fragmentation, full-width identities, owner/
viewer and expired-generation rejection, stale proof/revision refusal, cancellation,
forecast-row validation, read-only observations, and reconnect behavior. The
production execution tests now select and arm through the browser mutation
boundary. A shared JSON fixture carries actual Armed, Preparing, Following and
Complete observations into dashboard tests, alongside partial, recovery, stale
feedback, failure and no-replay cases. No firmware was flashed for T8.

T9 host acceptance is recorded below. T7's board timing, watchdog and stack
measurements and **T10 — Installation commissioning** remain open; the current
installed calibration still limits travel to 360°/90°. Follow
[CALIBRATION.md](CALIBRATION.md) before powered movement or enabling measured
over-the-top travel.

T8 software validation: **298 Rust tests (including the doctest), 61 dashboard
tests, host/Pico Clippy, formatting, build-tool checks and the Pico release
build pass**. The release uses 40,792 bytes of `.data`, 262,788 bytes of `.bss`
and 1,024 bytes of `.uninit`: 304,604 bytes of static RAM, leaving 227,876 bytes
for the shared stack. The 192 KiB linker reserve passes. Assembly inspection
still shows roughly 62 KiB for the tracking worker poll and 53 KiB for plan
publication; the web task poll has a small local frame with its buffers in the
task pool. These are capacity checks, not measured board high-water or timing.

### Complete-pass acceptance (T9)

The host acceptance matrix is in
[`tracking_plan_acceptance.rs`](../crates/ezal-core/tests/common/tracking_plan_acceptance.rs)
and
[`tracking_execution_acceptance.rs`](../crates/ezal-core/tests/common/tracking_execution_acceptance.rs).
It runs automatically with the existing host suite. To print the comparisons:

```bash
nix develop .#ci --command cargo test --locked -p ezal-core \
  --target x86_64-unknown-linux-gnu --test planner --test tracking_inputs \
  acceptance -- --nocapture
```

Synthetic routes use a separate 10 ms driver around the production follower and
actuator, with quantized ADC pairs, latency/skew, startup, coast, backlash and
duty accounting. This driver does not call the planner's `Simulation`. It checks
the complete route and post-LOS coast against the 20 ms planning results. The
motor model is shared, so these are independent integration checks under shared
physical assumptions, not independent evidence for those assumptions.

The recorded run uses the full production coordination/runtime and leased API,
including actual measured preparation readiness and incremental recovery. It
checks the fixture's AOS/LOS against the pass search. Its complete 751-second
window is METOP-B/NORAD 38771 over Clevedon, Unix seconds 1791402086–1791402837,
using the recorded `weather-2026-10-07.tle` catalog and the normal 5° mask.
The browser disconnects after arming. ADC pairs arrive with 100 ms latency at
the firmware's 200 ms pair cadence; output runs every 10 ms.

| Case | Planning result | Independent execution result |
|---|---|---|
| Exact zenith, 600 s, nine qualification scenarios | Complete OTT; 1.533° bounded modeled peak | Nominal/slow/fast replay peaks 0.852° / 0.852° / 1.261°; no modeled-budget excursions |
| Near zenith, 88° peak, 600 s, nine scenarios | Unsupported; pointing-budget excursions and noisy speed supervision | No executable route issued |
| Azimuth 350°→370°, 120 s, nominal scenario | Complete continuous route; 1.784° bounded peak | 1.705° peak; no wrap reversal |
| Preparation from 1°/1° and 449°/179°, then 120 s static target, nominal scenario | Complete; 0.248° / 0.119° bounded peaks | 0.137° / 0.222° peaks; within the 2° modeled allocation; include slew and final coast |
| Full recorded METOP-B window, nominal scenario | Partial; 12.50 s predicted gap; 2.218° bounded peak | Completes at original LOS; one 22.46 s recovery; 2.345° true model peak / 2.370° peak reported from ADC; zero measured time above 3° |
| Same recorded window, all nine scenarios | Unsupported; includes noisy azimuth speed fault | Arm rejected, zero relay transitions through LOS |
| Same recorded window, duty already exhausted | Unsupported, no recovery route | Arm rejected; later rest does not arm it |
| Leased cancellation while Armed, Preparing, Following or Reacquiring | Operator interruption | Immediate output inhibition; duty retained; stays stopped through original LOS |
| Stalled motor / lost feedback | Unexpected execution failure | Motion / feedback termination; returning samples never restart the run |
| Recorded recovery with insufficient planning throughput | Repeated missed launches | 13 bounded attempts, final timeout exactly at original 30 s deadline; peak/outside-tolerance metrics retained for the dashboard |

All tests use explicitly synthetic 450°/180° travel and confirmed-AC duty for
the modeled plant. The nominal-only fixtures deliberately isolate integration;
they do not qualify a real pass under the firmware's nine scenarios. Installed
travel, motion limits, relay timing, tolerance and scenario settings are unchanged.
The existing conventional-only zenith, FENGYUN 3C full-window rejection, duty
deadline, source/network failure and partial-admission tests also remain in the
host suite. Successful and interrupted runs hold their final post-coast position,
with no park or automatic next pass. There is no receiver/recording integration;
recording remains external.

Two differences matter for commissioning:

- **Predictions are conditional on the simulated schedule.** Changing ADC/pulse
  phase between 20 ms planning and 10 ms execution can exceed the reported
  modeled peak, even for a stationary target. The separate synthetic runs still
  fit the 2° modeled allocation. The recorded nominal replay's gap lasts 9.96 s
  longer than predicted and its true peak is 0.127° higher. It stays within the
  original recovery deadline and the provisional 3° operational threshold, but
  this does not establish the 1° reserve or RF reception windows. The dashboard
  correctly retains the original windows as predictions and reports live
  recovery/deviation separately. Measure scheduling, plant and pointing errors
  before accepting the reserve or treating modeled reception as achievable RF
  coverage; broaden timing-phase qualification if those measurements require it.
- **Full-pass recovery has a substantial computation cost.** A successful
  recorded search uses 398,964 incremental work units. The host harness supplies
  up to 2,048 units per virtual 10 ms turn; this is an explicit test throughput,
  not a Pico benchmark. At 128 units per turn the same run misses its launch
  opportunities and stops safely, with 3.393° peak measured error and 2.44 s
  above tolerance. T7 must measure complete searches with long suffixes and the
  real concurrent load against the fixed two-second launch lead. If the board
  cannot meet it, optimize/rebudget recovery and revalidate admission before
  claiming usable partial-pass tracking; do not extend an active episode deadline.

T9 closes the host acceptance work. It does not close T7's board timing/watchdog/
stack checks or T10's loaded motor, full-travel and RF commissioning. No firmware
was flashed and no physical pointing or RF tracking was tested for this task.

T9 software validation: **306 Rust tests (including the doctest), 61 dashboard
tests, host/Pico Clippy, formatting, build-tool checks and the Pico release build
pass**. This task adds tests and documentation; production behavior and its
T8 memory layout are unchanged.
