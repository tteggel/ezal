# ezal

> A satellite-tracking antenna rotator controller, written in Rust for the
> Raspberry Pi Pico 2 W.

**Status: hardware-driving walking skeleton.** The default firmware boots with
a deterministic, horizon-to-horizon **METOP-C** target source, POSTs the real
ADS1015, waits for WiFi and DHCP, drives the dish to the next pass's start position, and settles before
starting the 180-second pass clock. It then supplies no target for a 30-second
pause and reacquires the next pass in the opposite direction. Real feedback,
calibration, controller, movement lease, relay timing, GPIO outputs, rotator,
shared state, and dashboard telemetry participate in the same runtime path.

## What this project will be

`ezal` (read it "ez/al", for *azimuth/elevation*) drives a Yaesu **G-5500**
az/el rotator to track a low-earth-orbit (LEO) satellite as it passes
overhead. A separate ground-control system will compute the satellite's az/el
position from a TLE; ezal will receive those targets over a serial link,
drive the G-5500's direction inputs through transistor switches, and
read the controller's position feedback via an Adafruit ADS1015 I²C
ADC to close the loop. See [HARDWARE.md](docs/HARDWARE.md) and the
[design/](design/) folder for the interface details and schematic.

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
├── .envrc                          # `use flake` + `.env` — direnv loads the shell
├── .env.example                    # template for WiFi credentials (copy to .env)
├── .cargo/config.toml              # default target, linker flags, runner
├── crates/
│   ├── ezal-core/                  # pure no_std logic, host-testable
│   │   └── src/
│   │       ├── position.rs        # calibrated degrees ↔ ADC millivolts
│   │       ├── control.rs         # hysteretic fail-safe controller
│   │       ├── supervisor.rs      # feedback, fault, sequencing, telemetry policy
│   │       ├── authority.rs       # manual ownership and command routing
│   │       ├── interlock.rs       # fresh feedback and endpoint motion gate
│   │       ├── mailbox.rs         # pending inhibition has priority
│   │       ├── actuator.rs        # interlock first, then lease and relay timing
│   │       ├── motion.rs          # latched progress, direction, and speed checks
│   │       ├── network.rs         # expiring network permission for motion
│   │       ├── simulation.rs      # METOP-C source + pass sequencer
│   │       └── drive.rs           # command lease + relay timing
│   └── ezal-firmware/              # the on-chip application
│       ├── build.rs                # stages memory.x; bakes .env creds in
│       ├── memory.x                # linker memory layout
│       ├── cyw43-firmware/         # vendored CYW43439 radio blobs
│       └── src/
│           ├── main.rs            # boot graph + simulator/feedback tasks
│           ├── state.rs           # typed snapshots, commands, and wake signal
│           ├── drive.rs           # guarded direction GPIOs, watchdog feeding
│           ├── watchdog.rs        # independent hardware reset fallback
│           ├── web.rs             # dashboard, ownership, and telemetry
│           ├── ws_receive.rs      # complete-message receive deadline
│           ├── wifi.rs            # CYW43439 bring-up + STA-join POST
│           └── ads1015.rs         # ADS1015 I²C driver + POST
├── docs/
│   ├── ARCHITECTURE.md             # how the pieces fit together
│   ├── CALIBRATION.md              # installed-system calibration procedure
│   ├── HARDWARE.md                 # G-5500 wiring, debug probe, ...
│   └── DEVELOPMENT.md              # local toolchain setup
├── design/
│   ├── pinout.md                   # G-5500 8-pin DIN pinout
│   ├── circuit.py                  # schemdraw source for the schematic
│   ├── circuit.svg                 # rendered schematic (vector)
│   └── circuit.png                 # rendered schematic (raster)
├── scripts/                        # build, test, flash helpers
└── .github/workflows/ci.yml        # CI (Nix flake): fmt, clippy, tests, UF2
```

## Quick start

Prerequisites: [Nix with flakes](https://install.determinate.systems/),
[direnv](https://direnv.net/), and
[nix-direnv](https://github.com/nix-community/nix-direnv). The flake
provides the pinned Rust toolchain, `probe-rs`, `flip-link`, `picotool`,
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

# WARNING: default mode drives the connected rotator automatically.
# Build/flash it and stream defmt logs.
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
outage cuts motion and recovery restarts acquisition. Once DHCP completes,
the dashboard shows the pass number, acquire/track/pause phase and countdown,
target and calibrated position, raw A0/A1 millivolts, controller health, and
guarded drive state. Acquisition requires the dish to remain within 4° on both
axes for one second; failure to get there within 120 seconds stops the outputs
in an `acquire-timeout` fault.

Manual ADC/drive mode disables the synthetic target source:

```bash
cargo run -p ezal-firmware --release --no-default-features
```

Both modes enable physical outputs and require current network permission and
fresh, valid calibrated ADS1015 feedback before permitting movement. They read both feedback channels under a
500 ms deadline, reject a pair that took longer than 150 ms to read, and wait
100 ms after each one. The ADC uses ±4.096 V with 2 mV steps; the final output
gate stops outward motion one ADC code before a calibrated endpoint, widened by
the travel the rotator could have made since that sample began. Follow
[CALIBRATION.md](docs/CALIBRATION.md), including its disconnected-direction-lead
measurement procedure, before enabling motor commands.

Faults, stale feedback, expired movement leases, and released dashboard
controls inhibit the affected outputs immediately when the actuator task
handles them — releasing one of two held buttons stops that axis at once.
Ordinary controller stops respect relay timing. A separate 500 ms hardware
watchdog resets a stalled or panicked system; after such a timeout, boot holds
outputs low until an explicit reset or power cycle, while a flashing tool's
reboot through the same watchdog is not mistaken for it. These safeguards still
require on-board verification of output release and motor stopping behavior. The
[architecture](docs/ARCHITECTURE.md#safety-layers-and-their-limits) describes the
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
successful check. Internet access and a connected dashboard are not required.

## Documentation

- **[ARCHITECTURE.md](docs/ARCHITECTURE.md)** — simulator/hardware data flow,
  core/firmware boundary, control tick, and safety layers.
- **[HARDWARE.md](docs/HARDWARE.md)** — the Pico 2, the G-5500 rotator,
  the debug probe, expected wiring.
- **[CALIBRATION.md](docs/CALIBRATION.md)** — safe endpoint measurement,
  hard-coded value location, replacement, and verification.
- **[DEVELOPMENT.md](docs/DEVELOPMENT.md)** — setting up a working
  development environment from scratch.

## Roadmap

| step | description                                                     | status   |
|------|-----------------------------------------------------------------|----------|
| 1    | Direction outputs + ADS1015 feedback                            | ✅ done  |
| 2    | Calibrated az/el ↔ voltage mapping                              | ✅ done  |
| 3    | Hysteretic controller, watchdog lease, limits, relay protection | ✅ done  |
| 4    | Repeating METOP-C simulator + 30-second inter-pass pause        | ✅ done  |
| 5    | Autonomous dashboard telemetry                                 | ✅ done  |
| 6    | Persisted calibration + authenticated calibration UI           | planned  |
| 7    | Production target transport / real TLE source                   | planned  |

Simulator mode deliberately uses a representative target profile, not an
orbital propagator or current TLE. Replacing that source is the next production
boundary; the downstream control and safety chain does not depend on where a
valid timestamped target came from.

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  http://opensource.org/licenses/MIT)

at your option. This is the Rust ecosystem convention; it lets the code
be reused in either MIT-only or Apache-preferring downstream projects.
