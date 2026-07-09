# ezal

> A satellite-tracking antenna rotator controller, written in Rust for the
> Raspberry Pi Pico 2.

**Status: pre-alpha.** This is early bring-up firmware: at boot it joins
WiFi in station mode and self-tests the ADS1015 position-feedback ADC, then
streams the feedback voltages — exercising the toolchain and the board's
peripherals end to end. The real tracking firmware is still to be written;
see [Roadmap](#roadmap) below.

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
├── flake.nix                       # pinned host-side toolchain (probe-rs, ...)
├── .envrc                          # `use flake` + `.env` — direnv loads the shell
├── .env.example                    # template for WiFi credentials (copy to .env)
├── .cargo/config.toml              # default target, linker flags, runner
├── crates/
│   ├── ezal-core/                  # pure no_std logic, host-testable
│   │   └── src/
│   │       ├── ads1015.rs         # ADS1015 register model + POST predicates
│   │       └── wifi.rs            # WiFi credential validation
│   └── ezal-firmware/              # the on-chip application
│       ├── build.rs                # stages memory.x; bakes .env creds in
│       ├── memory.x                # linker memory layout
│       ├── cyw43-firmware/         # vendored CYW43439 radio blobs
│       └── src/
│           ├── main.rs            # POSTs WiFi + ADS1015, then reads feedback
│           ├── wifi.rs            # CYW43439 bring-up + STA-join POST
│           └── ads1015.rs         # ADS1015 I²C driver + POST
├── docs/
│   ├── ARCHITECTURE.md             # how the pieces fit together
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

# Run the host-side ezal-core tests. Should be green.
./scripts/test-host.sh

# One-time: set your WiFi credentials for the station-mode POST.
cp .env.example .env && $EDITOR .env

# Build, flash, and stream defmt logs from a connected Pico 2 W.
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

Over a debug probe (`cargo run`) you'll see the two boot POSTs and then the
feedback readout: the firmware joins WiFi, self-tests the ADS1015, and logs
both G-5500 feedback channels twice a second (there's a sample in
[DEVELOPMENT.md](docs/DEVELOPMENT.md#flash-and-watch-logs)). The four
direction GPIOs are held low, so nothing moves and the interface board's
D1–D4 LEDs stay dark — this is a *sensing* bring-up. On a bare Pico 2 W with
no probe the firmware still runs; there's just nothing external to watch.

## Documentation

- **[ARCHITECTURE.md](docs/ARCHITECTURE.md)** — the workspace layout, the
  core/firmware split, where the eventual rotator-control modules go.
- **[HARDWARE.md](docs/HARDWARE.md)** — the Pico 2, the G-5500 rotator,
  the debug probe, expected wiring.
- **[DEVELOPMENT.md](docs/DEVELOPMENT.md)** — setting up a working
  development environment from scratch.

## Roadmap

| step | description                                              | status   |
|------|----------------------------------------------------------|----------|
| 1    | Direction-GPIO sweep bring-up — toolchain proof          | ✅ done  |
| 2    | USB-serial command interface (host → firmware az/el)     | planned  |
| 3    | Direction-switch outputs + I²C ADS1015 feedback          | planned  |
| 4    | Deadband position controller (close the loop on-chip)    | planned  |
| 5    | TLE-driven tracking (host-supplied az/el stream)         | planned  |
| 6    | On-board TLE propagation (stand-alone tracking)          | maybe    |

Alongside these steps, supporting infrastructure lands as it's needed: the
ADS1015 position-feedback POST and the Pico 2 W **WiFi station-mode bring-up**
(radio init + AP join, credentials from `.env`) are both in already. A
network transport for az/el targets would build on that WiFi link — see
[ARCHITECTURE.md](docs/ARCHITECTURE.md).

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  http://opensource.org/licenses/MIT)

at your option. This is the Rust ecosystem convention; it lets the code
be reused in either MIT-only or Apache-preferring downstream projects.
