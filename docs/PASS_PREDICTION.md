# Pass prediction

The firmware displays a rolling six-hour forecast at `/api/passes` and
in the dashboard. NTP synchronization starts after DHCP. TLE download waits
for valid UTC, then the firmware computes SGP4 reception windows independently
of dashboard connections. These windows are advisory and never authorize
movement. The firmware boots idle and moves only in response to manual
dashboard input; real tracking is deferred.

The [planned tracking behavior](#planned-tracking) below records the agreed
workflow and provisional commissioning values. It is a design for future
firmware, separate from the advisory behavior described in the other sections.

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
  a ten-second total timeout. Good samples refresh hourly; failures retry in
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
  Checksums, ID matching and age checks catch corruption, not an active network
  attacker. This advisory implementation must gain an authenticated data path
  before its predictions are used as a trusted autonomous command source.
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
per step so it cannot delay actuator/feedback servicing. Add an authenticated
orbital-data path as required by the current advisory-data contract above.

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
- [ ] **T3 — Bounded trajectory planner.** Generate continuous conventional,
  azimuth-overlap and over-the-top candidates; refine near zenith and limits.
  Evaluate preparation and the entire pass with the shared controller across
  motion uncertainty. Report supported feasibility, peak deviation, gaps and
  preparation time. Include relay availability at AOS; bound memory/work per
  step and check between trajectory samples, including feedback noise/channel
  skew in the uncertainty budget. Tune follower hysteresis/lead
  against the slow/nominal/fast scenarios to reduce the baseline controller's
  reversals without hiding sky error. Depends on T1 and T2.
- [ ] **T4 — Single-pass lifecycle and exclusive operator lease.** Implement
  device-owned selection, arm/prepare/follow/complete, stable pass identity,
  revisioned updates until preparation, atomic plan freeze and idempotent
  cancellation. Tracking survives browser/lease expiry; only the current
  lease holder can mutate it. Test takeover refusal, transfer, stale commands
  and update/cancel/preparation races. Depends on T3's plan contract.
- [ ] **T5 — Execution clock and trusted orbital inputs.** Add an authenticated
  orbital-data path and a fractional UTC anchor tied to monotonic uptime.
  Freeze execution inputs, enforce source validity through LOS and account
  for timing uncertainty. Test clock steps, stale sources, failed refreshes,
  boot disarming and the initial stop-on-device-network-loss policy.
- [ ] **T6 — Bounded reacquisition.** Apply deviation warning/hysteresis,
  search reachable future intercepts on the frozen pass and enforce the
  original 30 s episode deadline/LOS. Retain fault inhibition, cancellation
  and duty state; report recovery ETA and terminal reasons. Test recoverable
  lag, impossible intercepts, repeated replans and feedback failure. Depends
  on T2–T5.
- [ ] **T7 — Independent firmware execution.** Wire tracking into its own
  serviced task and the existing output boundary with short internal command
  leases. Keep downloads and incremental planning out of the live control
  task. Check Pico memory, execution time and watchdog behavior with prediction,
  downloads and multiple viewers active. Depends on T2–T6.
- [ ] **T8 — Tracking API and dashboard.** Add selection, feasibility preview,
  Manual/Tracking mode, scheduled preparation/Prepare now and Cancel. Hide
  manual controls while tracking and reject their commands on the device.
  Show phase, desired/measured pointing, timestamp-matched spherical deviation,
  stale feedback, recovery progress and the final summary. Test owner/viewer
  permissions and reconnects. Depends on T4, T6 and T7.
- [ ] **T9 — Complete-pass acceptance.** Run recorded and synthetic exact-
  zenith/near-zenith passes, azimuth wraps, extreme preparation positions,
  slow/fast motor profiles, cancellations, faults, duty exhaustion and LOS.
  Confirm holding the final position, no automatic next pass, and external
  recording only. Compare the planner's predictions with the execution path.
  Depends on T3–T8.
- [ ] **T10 — Installation commissioning.** Follow [CALIBRATION.md](CALIBRATION.md)
  to identify hardware, calibrate full travel/sky pointing and measure loaded
  rates, useful pulses, coast, backlash and RF pointing tolerance. Replace
  provisional values, validate GPIO/watchdog release and perform supervised
  passes before enabling routine automatic tracking. Measurement work can
  start alongside T1; final acceptance depends on T9.

### Current simulation milestone

[`tracking`](../crates/ezal-core/src/tracking.rs) now supplies a pure-core
feedback follower and a bounded simulator. The simulator feeds quantized,
delayed observations through the production feedback validator and actuator;
its default uses the current 500 ms minimum on-time and 2 s off-time. It records
actual energized time, relay transitions, reversals, sampled peak sky deviation and
time outside the modeled allowance. Invalid inputs, duty exhaustion, physical
travel violations and actuator motion faults stop a run with explicit reasons.
Cancellation inhibits pending as well as active output; further idle steps
model coast to the final position.

Run the offline [replay example](../crates/ezal-core/examples/tracking-sim.rs)
inside the development shell:

```bash
TRACKING_HOST=$(rustc -vV | sed -n 's/host: //p')
cargo run --locked -p ezal-core --target "$TRACKING_HOST" --example tracking-sim
# Compare proposed timing offline; this does not configure the device:
cargo run --locked -p ezal-core --target "$TRACKING_HOST" --example tracking-sim -- --candidate-timing
```

The example compares slow/nominal/fast AC profiles on synthetic exact-zenith
and 88° peak passes, then replays orbital geometry from the recorded TLE
fixture. It starts at the first target with a synthetic full-travel calibration
and a rested, confirmed-AC 300 s duty allowance. Live firmware uses the
unidentified-hardware 180 s policy and requires full rest at boot. The first
motor direction is assumed engaged;
reversals consume explicit backlash, including across short pulses. Motor
startup is a dead time, and coast is constant-rate travel for the assumed
release duration. Paired feedback is quantized directly to ADC codes and
delayed by 100 ms; channel skew, electrical noise and alignment uncertainty
are not yet modeled. The baseline follower uses a 0.5° mechanical deadband
and estimates release travel from feedback age and the assumed coast rate.
It keeps its nominal reference profile while the simulated motor rates vary.

Route position and satellite direction are separate inputs. An intentional
keyhole offset therefore contributes to sky deviation even when the motor
route is followed exactly. The default simulation threshold is the 2° modeled
allocation, leaving the agreed 1° uncertainty reserve within the 3° total
target. Samples are at most 20 ms apart, with earlier actuator/acquisition
deadlines honored. This milestone does not bound unsampled pointing error,
plan preparation or reacquisition, or certify a pass as feasible.

The initial nominal-profile replay produces approximately 0.91° sampled peak
deviation for the synthetic over-the-top zenith pass, versus 7.12° for the
conventional zenith route. The recorded 70.75° peak pass reaches 3.11°, exceeding
the modeled allocation. Faster scenarios show frequent reversals, and the
fast conventional zenith case is now held inside the endpoint by the coast
guard, reaching 5.83° peak deviation. Some near-zenith scenarios stop with an
azimuth no-progress fault after repeated reversals. These
are results from provisional models, not measured antenna performance; they
provide concrete cases for T3's controller/planner
tuning. A fixed 2° keyhole offset also consumes the entire modeled allocation
before following error is added, so it is not automatically an acceptable route.

The shorter candidate timings are not an improvement with the baseline
follower: the nominal over-the-top zenith case rises to 2.28° peak deviation,
203 elevation reversals and 232 s of elevation on-time, compared with 0.91°,
zero reversals and 51 s using installed timing. The recorded nominal case
rises from 3.11° to 4.38°. T3 must account for pulse completion, coast and
backlash in its hysteresis/lead tuning before adopting shorter timings.

The [tracking tests](../crates/ezal-core/tests/tracking.rs) cover geometry,
startup/coast, partial backlash take-up, duty/rest, delayed quantized feedback,
invalid inputs, ordinary relay timing, cancellation racing a pending activation,
stall/speed faults, full synthetic/recorded passes and intentional sky offsets.
The shared output safeguards are wired into the firmware's existing manual
control path. They include pulse-aware motion checks, configured coast bounds,
validated same-direction/reversal timing, and per-axis duty preserved through
command/owner changes. Unknown history requires 900 s rest at boot. The
dashboard displays allowance/rest and requires fresh input after a block;
an unaffected axis remains usable. [Output-policy tests](../crates/ezal-core/tests/output_policy.rs)
exercise these rules, with coordination, motion and dashboard regressions.

The tracking follower and simulator remain offline; pass selection/execution
is not yet available. Next implementation task: **T3 — Bounded trajectory planner**.
