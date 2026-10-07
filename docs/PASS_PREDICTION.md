# Pass prediction

Both firmware modes display a rolling six-hour forecast at `/api/passes` and
in the dashboard. NTP synchronization starts after DHCP. TLE download waits
for valid UTC, then the firmware computes SGP4 reception windows independently
of dashboard connections. These windows do not command the motors; the default
hardware-driving simulator and the manual mode retain their existing behavior.

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

Propagation uses the library's WGS72/AFSPC initialization. TEME positions are
rotated with the library's sidereal time into Earth-fixed coordinates, then
projected onto a WGS84 station's east/north/up basis. UTC approximates UT1;
polar motion and atmospheric refraction are omitted. The search scans at
thirty-second spacing, refines local peaks (including short grazing passes),
then brackets and bisects AOS/LOS to 0.25 seconds before rounding. The ±30 minute
search margin is specific to the supported low-eccentricity HRPT LEOs.

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
cargo build --locked -p ezal-firmware --release --no-default-features
```
