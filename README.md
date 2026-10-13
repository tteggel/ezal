# ezal

`ezal` (read it "ez/al", for *azimuth/elevation*) points a Yaesu **G-5500**
az/el rotator at low-earth-orbit weather satellites. It runs on a
**Raspberry Pi Pico 2 W**: four GPIO-driven transistor switches press the
rotator's direction inputs, and an Adafruit **ADS1015** I²C ADC reads its
calibrated position feedback. The firmware predicts L-band HRPT passes from
TLEs with SGP4 and serves a dashboard on the local network for manual control
and for following one explicitly armed pass. The policy lives in `ezal-core`, a
`no_std` crate tested on the host; `ezal-firmware` is the Embassy shell that
owns the peripherals. [HARDWARE.md](docs/HARDWARE.md) and [design/](design/)
describe the wiring and the interface board.

The control and tracking policy, including full-pass tracking, is validated by
host tests. Validation on the board and the installed rotator follows the
[bench verification checklist](docs/CALIBRATION.md#bench-verification) and is
still pending.

The dashboard assumes a trusted local network: controller ownership
coordinates clients but does not authenticate them.

## Quick start

Prerequisites: [Nix with flakes](https://install.determinate.systems/),
[direnv](https://direnv.net/), and
[nix-direnv](https://github.com/nix-community/nix-direnv). The flake
provides the pinned Rust toolchain, `probe-rs`, `picotool`,
and a Python+schemdraw environment so you don't have to install any of
them yourself. See
[DEVELOPMENT.md](docs/DEVELOPMENT.md#without-nix-alternative-path) for a
rustup-only path if you'd rather not use Nix.

```bash
cd ezal/
direnv allow                        # one-time: load the dev shell

# Run Rust policy/transport tests and dashboard JavaScript behavior tests.
./scripts/test-host.sh
./scripts/test-dashboard.sh

# One-time: set your WiFi credentials and station settings.
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

## Documentation

- **[ARCHITECTURE.md](docs/ARCHITECTURE.md)**: the core/firmware boundary,
  which owner holds each resource, the output loop, where tests live and the
  comment conventions.
- **[CONTROL_CONTRACT.md](docs/CONTROL_CONTRACT.md)**: every safeguard, its
  owner, effect, recovery and timing; the reference for control-path changes.
- **[TRACKING.md](docs/TRACKING.md)**: single-pass planning, execution,
  reacquisition and the tracking wire contract.
- **[PASS_PREDICTION.md](docs/PASS_PREDICTION.md)**: station settings, the
  satellite catalog, advisory forecasts and the orbital maths.
- **[HARDWARE.md](docs/HARDWARE.md)**: the Pico 2 W, the interface board, the
  ADS1015 feedback path and the G-5500.
- **[CALIBRATION.md](docs/CALIBRATION.md)**: endpoint calibration, the motion
  profile, the bench verification checklist and known model limits.
- **[DEVELOPMENT.md](docs/DEVELOPMENT.md)**: toolchain setup, building,
  flashing, testing and debugging.
- **[`crates/ezal-core/src/lib.rs`](crates/ezal-core/src/lib.rs)**: the crate
  doc lists every core module and its role.

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  http://opensource.org/licenses/MIT)

at your option. This is the Rust ecosystem convention; it lets the code
be reused in either MIT-only or Apache-preferring downstream projects.
