# ezal

> A satellite-tracking antenna rotator controller, written in Rust for the
> Raspberry Pi Pico 2.

**Status: hardware-driving walking skeleton.** The default firmware boots with
a deterministic, horizon-to-horizon **METOP-C** target source, POSTs the real
ADS1015, drives the dish to the next pass's start position, and settles before
starting the 180-second pass clock. It then stops for exactly 30 seconds and
reacquires the next pass in the opposite direction. Real feedback, calibration,
controller, movement lease, relay timing, GPIO outputs, rotator, shared state,
and dashboard telemetry are all exercised end to end.

## What this project will be

`ezal` (read it "ez/al", for *azimuth/elevation*) drives a Yaesu **G-5500**
az/el rotator to track a low-earth-orbit (LEO) satellite as it passes
overhead. A separate ground-control system will compute the satellite's az/el
position from a TLE; ezal will receive those targets over a serial link,
drive the G-5500's direction inputs through transistor switches, and
read the controller's position feedback via an Adafruit ADS1015 I²C
ADC to close the loop. See [HARDWARE.md](docs/HARDWARE.md) and the
[design/](design/) folder for the interface details and schematic.

The firmware runs on a **Raspberry Pi Pico 2** (RP2350). The chip has
plenty of headroom for this job — async I/O, two Cortex-M33 cores, 520 KiB
of SRAM — so the project also serves as a *clean, principled example* of
professional embedded Rust development. The code is deliberately
over-commented for educational purposes.

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
│   │       ├── simulation.rs      # METOP-C source + pass sequencer
│   │       └── drive.rs           # command lease + relay timing
│   └── ezal-firmware/              # the on-chip application
│       ├── build.rs                # stages memory.x; bakes .env creds in
│       ├── memory.x                # linker memory layout
│       ├── cyw43-firmware/         # vendored CYW43439 radio blobs
│       └── src/
│           ├── main.rs            # boot graph + simulator/feedback tasks
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

# Run the host-side controller, calibration, and sequencer tests.
./scripts/test-host.sh

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

ADS1015 POST and the autonomous tracking task start before WiFi bring-up, so
acquisition runs from boot even while networking connects. Once DHCP completes,
the dashboard shows the pass number, acquire/track/pause phase and countdown,
target and calibrated position, raw A0/A1 millivolts, controller health, and
guarded drive state. Acquisition requires the dish to remain within 4° on both
axes for one second; failure to get there within 120 seconds stops the outputs
in an `acquire-timeout` fault.

Manual ADC/drive mode disables the synthetic target source:

```bash
cargo run -p ezal-firmware --release --no-default-features
```

Both default and manual modes enable physical outputs and require a correctly
wired ADS1015. Complete [CALIBRATION.md](docs/CALIBRATION.md) and verify motor
direction with an accessible power disconnect before flashing either one.

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
