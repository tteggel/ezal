# ezal

> A satellite-tracking antenna rotator controller, written in Rust for the
> Raspberry Pi Pico 2.

**Status: pre-alpha.** This is step one — a "hello world" blinky firmware
that exercises the toolchain end to end. The real tracking firmware is
still to be written; see [Roadmap](#roadmap) below.

## What this project will be

`ezal` (read it "ez/al", for *azimuth/elevation*) drives a Yaesu **G-5500**
az/el rotator to track a low-earth-orbit (LEO) satellite as it passes
overhead. A separate ground-control system computes the satellite's az/el
position from a TLE; ezal receives those targets over a serial link,
drives the G-5500's direction inputs through transistor switches, and
reads the controller's position feedback via an Adafruit ADS1015 I²C
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
├── .cargo/config.toml              # default target, linker flags, runner
├── crates/
│   ├── ezal-core/                  # pure no_std logic, host-testable
│   │   ├── src/lib.rs
│   │   ├── src/morse.rs            # Morse code encoder
│   │   └── tests/morse.rs          # behavioural tests
│   └── ezal-firmware/              # the on-chip application
│       ├── build.rs
│       ├── memory.x                # linker memory layout
│       └── src/main.rs             # blinks HELLO WORLD on GPIO 25
├── docs/
│   ├── ARCHITECTURE.md             # how the pieces fit together
│   ├── HARDWARE.md                 # G-5500 wiring, debug probe, ...
│   ├── DEVELOPMENT.md              # local toolchain setup
│   └── MORSE.md                    # walkthrough of the hello demo
├── design/
│   ├── pinout.md                   # G-5500 8-pin DIN pinout
│   ├── circuit.py                  # schemdraw source for the schematic
│   ├── circuit.svg                 # rendered schematic (vector)
│   └── circuit.png                 # rendered schematic (raster)
├── scripts/                        # build, test, flash helpers
└── .github/workflows/ci.yml        # CI: fmt + clippy + tests + build
```

## Quick start

Prerequisite: [`rustup`](https://rustup.rs) installed.

```bash
# 1. Install host-side tooling (probe-rs, flip-link).
./scripts/install-tools.sh

# 2. Run the host-side ezal-core tests. Should be green.
./scripts/test-host.sh

# 3. Build, flash, and stream defmt logs from a connected Pico 2.
cargo run -p ezal-firmware --release
```

If you don't have a debug probe handy, you can flash UF2-style by holding
BOOTSEL while plugging the Pico 2 in and dropping the elf via `picotool`:

```bash
cargo build -p ezal-firmware --release
picotool load -uvx -t elf \
    target/thumbv8m.main-none-eabihf/release/ezal-firmware
```

You should see the onboard LED blink **HELLO WORLD** in international
Morse code (∙∙∙∙ ∙ ∙−∙∙ ∙−∙∙ −−− / ∙−− −−− ∙−∙ ∙−∙∙ −∙∙), with a
two-second pause between repeats.

## Documentation

- **[ARCHITECTURE.md](docs/ARCHITECTURE.md)** — the workspace layout, the
  core/firmware split, where the eventual rotator-control modules go.
- **[HARDWARE.md](docs/HARDWARE.md)** — the Pico 2, the G-5500 rotator,
  the debug probe, expected wiring.
- **[DEVELOPMENT.md](docs/DEVELOPMENT.md)** — setting up a working
  development environment from scratch.
- **[MORSE.md](docs/MORSE.md)** — line-by-line walkthrough of the hello
  demo, aimed at someone new to embedded Rust.

## Roadmap

| step | description                                              | status   |
|------|----------------------------------------------------------|----------|
| 1    | "Hello LED" morse blinker — toolchain proof              | ✅ done  |
| 2    | USB-serial command interface (host → firmware az/el)     | planned  |
| 3    | Direction-switch outputs + I²C ADS1015 feedback          | planned  |
| 4    | Deadband position controller (close the loop on-chip)    | planned  |
| 5    | TLE-driven tracking (host-supplied az/el stream)         | planned  |
| 6    | On-board TLE propagation (stand-alone tracking)          | maybe    |

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  http://opensource.org/licenses/MIT)

at your option. This is the Rust ecosystem convention; it lets the code
be reused in either MIT-only or Apache-preferring downstream projects.
