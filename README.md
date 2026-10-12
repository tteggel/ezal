# ezal

> A satellite-tracking antenna rotator controller, written in Rust for the
> Raspberry Pi Pico 2 W.

**Status: manual control and device-owned single-pass tracking implemented;
hardware tracking validation pending.** The firmware
boots with all direction outputs idle, POSTs the real ADS1015, and waits for
WiFi and DHCP. Movement requires manual input or an explicitly armed pass, current calibrated
feedback, and network permission. Movement leases, relay timing, endpoint
checks, and motion supervision guard the physical GPIO outputs.

The firmware also synchronizes UTC with NTP, downloads weather-satellite
TLEs, and predicts the next six hours of L-band HRPT reception windows on the
dashboard. The default station is **All Saints Church, Clevedon**. Prediction
alone is advisory. The dashboard can select one pass, preview feasibility and
explicitly arm it for device-owned tracking. See
[pass prediction](docs/PASS_PREDICTION.md) for station settings, satellite coverage,
acquisition/loss thresholds, and limitations.

## What this project will be

`ezal` (read it "ez/al", for *azimuth/elevation*) drives a Yaesu **G-5500**
az/el rotator to track a low-earth-orbit (LEO) satellite as it passes
overhead. The pure core now computes satellite az/el and reception windows
from TLEs using SGP4. Manual control drives the G-5500's direction inputs
through transistor switches and reads calibrated position feedback via an
Adafruit ADS1015 I²C ADC. The device executor and dashboard arming are integrated;
full-pass host acceptance is complete. Board timing checks and installation
commissioning remain before routine automatic tracking.
See [HARDWARE.md](docs/HARDWARE.md) and the [design/](design/) folder for the
interface details and schematic.

The firmware runs on a **Raspberry Pi Pico 2 W** (RP2350 and CYW43439 radio). The chip has
plenty of headroom for this job — async I/O, two Cortex-M33 cores, 520 KiB
of SRAM. The core policy runs in host tests, while the Embassy shell supplies
the physical I/O. The code includes detailed comments for contributors learning
embedded Rust.

## What's in the repo today

```
.
├── Cargo.toml                      # workspace manifest
├── rust-toolchain.toml             # pinned compiler version
├── flake.nix                       # pinned host-side toolchain (probe-rs, ...)
├── .envrc                          # `use flake` — direnv loads the dev shell
├── .env.example                    # template for WiFi + station settings (copy to .env)
├── .cargo/config.toml              # default target, linker flags, runner
├── crates/
│   ├── ezal-core/                  # pure no_std logic, host-testable
│   │   └── src/
│   │       ├── position.rs        # calibrated degrees ↔ ADC millivolts
│   │       ├── feedback.rs        # validated observations and acquisition health
│   │       ├── coordination.rs    # ownership, coherent snapshots, command delivery
│   │       ├── session.rs         # source freshness and ordered manual intent
│   │       ├── interlock.rs       # fresh feedback and endpoint motion gate
│   │       ├── mailbox.rs         # pending inhibition has priority
│   │       ├── actuator.rs        # complete output decision in one step
│   │       ├── motion.rs          # latched progress, direction, and speed checks
│   │       ├── duty.rs            # per-axis energized allowance and continuous rest
│   │       ├── network.rs         # expiring network permission for motion
│   │       ├── operator.rs        # exclusive leases for the tracking lifecycle
│   │       ├── ntp.rs             # SNTP validation, uncertainty + revisioned UTC anchor
│   │       ├── http.rs            # streamed HTTP framing for TLE downloads
│   │       ├── orbit.rs           # TLE validation, catalog, SGP4 look angles
│   │       ├── passes.rs          # pass search, forecasts, /api/passes JSON
│   │       ├── tracking.rs        # planning, lifecycle, following + offline simulation
│   │       ├── protocol.rs        # wire parsing and coherent telemetry
│   │       ├── dashboard.rs       # browser asset and shared timing configuration
│   │       └── drive.rs           # vocabulary and private relay timing
│   └── ezal-firmware/              # the on-chip application
│       ├── build.rs                # memory.x link path; bakes .env creds + station in
│       ├── memory.x                # linker memory layout
│       ├── cyw43-firmware/         # vendored CYW43439 radio blobs
│       └── src/
│           ├── main.rs            # startup checks and task wiring
│           ├── feedback.rs        # ADC resource ownership and sampling
│           ├── state.rs           # synchronization and actuator wake hint
│           ├── drive.rs           # guarded direction GPIOs, watchdog feeding
│           ├── tracking.rs        # yielding planner/recovery worker + timing diagnostics
│           ├── watchdog.rs        # independent hardware reset fallback
│           ├── web.rs             # dashboard, ownership, telemetry, passes
│           ├── time.rs            # NTP over DNS/UDP
│           ├── prediction.rs      # TLE download + cooperative forecasts
│           ├── ws_receive.rs      # complete-message receive deadline
│           ├── wifi.rs            # radio initialization and reconnect supervision
│           └── ads1015.rs         # ADS1015 I²C driver + POST
├── docs/
│   ├── ARCHITECTURE.md             # layers and resource ownership
│   ├── CONTROL_CONTRACT.md         # safeguard effects, ordering, and recovery
│   ├── CALIBRATION.md              # installed-system calibration procedure
│   ├── HARDWARE.md                 # G-5500 wiring, debug probe, ...
│   ├── PASS_PREDICTION.md          # station settings, catalog, limits
│   └── DEVELOPMENT.md              # local toolchain setup
├── design/
│   ├── pinout.md                   # G-5500 8-pin DIN pinout
│   ├── circuit.py                  # schemdraw source for the schematic
│   └── circuit.svg                 # rendered schematic (vector)
├── scripts/                        # build, test, flash helpers
└── .github/workflows/ci.yml        # CI (Nix flake): fmt, clippy, tests, UF2
```

## Quick start

Prerequisites: [Nix with flakes](https://install.determinate.systems/),
[direnv](https://direnv.net/), and
[nix-direnv](https://github.com/nix-community/nix-direnv). The flake
provides the pinned Rust toolchain, `probe-rs`, `picotool`,
and a Python+schemdraw environment so you don't have to install any of
them yourself. See [DEVELOPMENT.md](docs/DEVELOPMENT.md) for a rustup-
only path if you'd rather not use Nix.

```bash
cd ezal/
direnv allow                        # one-time: load the dev shell

# Run Rust policy/transport tests and dashboard JavaScript behavior tests.
./scripts/test-host.sh
./scripts/test-dashboard.sh

# One-time: set your WiFi credentials for the station-mode POST.
cp .env.example .env && $EDITOR .env

# Build/flash firmware and stream defmt logs.
# Complete calibration before commanding the connected rotator.
cargo run -p ezal-firmware --release
```

If you don't have a debug probe handy, build a UF2 image and drag-and-drop
it onto the Pico 2's BOOTSEL drive instead — no probe, and nothing extra
installed on the machine doing the copy:

```bash
./scripts/build-uf2.sh              # → target/.../release/ezal-firmware.uf2
```

Then hold **BOOTSEL** while plugging the Pico 2 in, and copy the `.uf2`
onto the `RP2350` drive that appears. See
[DEVELOPMENT.md](docs/DEVELOPMENT.md#build-a-uf2-for-bootsel-flashing) for
the details.

ADS1015 POST and feedback sampling start before WiFi bring-up. Outputs remain
inhibited until the station link and a usable DHCP address are available; an
outage cuts motion and recovery requires ownership and fresh operator input.
The dashboard shows calibrated position, raw A0/A1 millivolts, feedback health,
applied drive state, and per-axis motor allowance/rest countdown alongside the
pass forecast and tracking status. Unknown duty history on boot requires 15 minutes of
continuous rest before jogging. The default allowance is 180 seconds per axis;
short pulse gaps and ownership changes do not reset it.

Manual movement requires current network permission and fresh, valid calibrated
ADS1015 feedback. The firmware reads both feedback channels under a
500 ms deadline, rejects a pair that took longer than 150 ms to read, and waits
100 ms after each one. The ADC uses ±4.096 V with 2 mV steps; the final output
gate stops outward motion one ADC code before a calibrated endpoint, widened by
the travel the rotator could have made since that sample began and an assumed
500 ms of release/coasting travel. Follow
[CALIBRATION.md](docs/CALIBRATION.md), including its disconnected-direction-lead
measurement procedure, before enabling motor commands.

Faults, stale feedback, expired movement leases, and released dashboard
controls inhibit the affected outputs immediately when the actuator task
handles them — releasing one of two held buttons stops that axis at once.
A separate 500 ms hardware watchdog resets a stalled or panicked system; after
such a timeout, boot holds
outputs low until an explicit reset or power cycle, while a flashing tool's
reboot through the same watchdog is not mistaken for it. These safeguards still
require on-board verification of output release and motor stopping behavior. The
[control contract](docs/CONTROL_CONTRACT.md) describes the
timing rules and their limits.

The dashboard assumes a trusted local network. Controller ownership coordinates
clients but does not authenticate them. Production target transport and
authentication remain planned work.

Motion supervision latches both outputs off if either axis fails to move after
five seconds of accumulated energized time, moves in the wrong direction after
startup settling, or exceeds its speed allowance. These configurable thresholds
need installation tuning; see [CALIBRATION.md](docs/CALIBRATION.md). Browser
movement carries an expiring server token, and missing telemetry clears held
controls. Network supervision retries association/DHCP with backoff, while the
output task independently expires network permission after 300 ms without a
successful check. Manual control requires the dashboard but not internet access.

## Documentation

- **[ARCHITECTURE.md](docs/ARCHITECTURE.md)** — manual control and prediction data flow,
  core/firmware boundary, control tick, and safety layers.
- **[CONTROL_CONTRACT.md](docs/CONTROL_CONTRACT.md)** — safeguard ownership,
  recovery, and output ordering.
- **[HARDWARE.md](docs/HARDWARE.md)** — the Pico 2, the G-5500 rotator,
  the debug probe, expected wiring.
- **[CALIBRATION.md](docs/CALIBRATION.md)** — safe endpoint measurement,
  full-travel and sky-pointing checks, replacement, and motion measurements.
- **[PASS_PREDICTION.md](docs/PASS_PREDICTION.md)** — current advisory forecasts
  and the [planned single-pass tracking behavior](docs/PASS_PREDICTION.md#planned-tracking),
  with [implementation tasks](docs/PASS_PREDICTION.md#tracking-implementation-tasks).
- **[DEVELOPMENT.md](docs/DEVELOPMENT.md)** — setting up a working
  development environment from scratch.

## Roadmap

| step | description                                                     | status   |
|------|-----------------------------------------------------------------|----------|
| 1    | Direction outputs + ADS1015 feedback                            | ✅ done  |
| 2    | Calibrated az/el ↔ voltage mapping                              | ✅ done  |
| 3    | Feedback validity, watchdog lease, limits, relay protection     | ✅ done  |
| 4    | Leased manual dashboard control and exclusive ownership        | ✅ done  |
| 5    | Calibrated feedback and applied-output dashboard telemetry     | ✅ done  |
| 6    | NTP time, TLE download, advisory HRPT pass prediction           | ✅ done  |
| 7    | Persisted calibration + authenticated calibration UI           | planned  |
| 8    | Validated orbital inputs and frozen execution time             | integrated |
| 9    | Single-pass tracking, over-the-top planning and reacquisition   | in progress |

The firmware lists passes and now services a browser-independent tracking executor. The
[tracking design](docs/PASS_PREDICTION.md#planned-tracking) specifies controller
ownership, automatic updates before preparation, local execution, visible
deviation and reacquisition. The [planning milestone](docs/PASS_PREDICTION.md#current-planning-milestone)
evaluates preparation and conventional/over-the-top routes through the production
actuator across provisional motor and feedback scenarios. The
[lifecycle core](docs/PASS_PREDICTION.md#current-lifecycle-milestone) adds exclusive
operator leases, one selected pass, plan freezing and browser-independent execution
state. The [execution-input boundary](docs/PASS_PREDICTION.md#current-execution-input-milestone)
binds validated orbital snapshots and fractional UTC to each plan, checks timing
uncertainty through LOS, and stops on device network loss. HTTP orbital downloads
are an explicit installation decision. The
[recovery core](docs/PASS_PREDICTION.md#current-reacquisition-milestone) adds
timestamp-matched deviation, bounded future intercepts and final pass summaries.
Validated partial-pass admission and firmware execution are integrated. The
[firmware milestone](docs/PASS_PREDICTION.md#independent-firmware-execution-t7)
records host validation, Pico memory checks and remaining board measurements.
The [tracking dashboard and API](docs/PASS_PREDICTION.md#tracking-api-and-dashboard-t8)
provide selection, full/partial feasibility, scheduled preparation, Prepare now,
Cancel and Return to Manual. Tracking continues without a browser; live deviation,
recovery and final metrics remain visible to viewers.
[T9 host acceptance](docs/PASS_PREDICTION.md#complete-pass-acceptance-t9) covers full
passes, rejection and interruption, and records prediction/actual recovery variance.
T10 commissioning and T7 board timing/watchdog checks remain.

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  http://opensource.org/licenses/MIT)

at your option. This is the Rust ecosystem convention; it lets the code
be reused in either MIT-only or Apache-preferring downstream projects.
