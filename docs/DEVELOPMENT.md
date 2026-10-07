# Development guide

This document walks through setting up a working development environment
on a fresh machine, and explains the day-to-day workflow.

## One-time setup

ezal pins the regular build and test tools — Rust, probe-rs, flip-link, picotool, Node.js,
the Python + schemdraw stack used to re-render the schematic — into a
single `flake.nix` so every contributor gets the same toolchain on every
platform. The flake is loaded automatically by [direnv](https://direnv.net)
the moment you `cd` into the workspace; you don't run any "install
this, then that" script.

### 1. Install Nix, direnv, and nix-direnv

If you don't already have them:

- **Nix with flakes enabled** — the
  [Determinate Systems installer](https://install.determinate.systems/)
  is the smoothest path on Linux and macOS and enables flakes by default.
  On NixOS use your system config as usual.
- **direnv** — from your distribution's package manager, or
  [direnv.net/docs/installation.html](https://direnv.net/docs/installation.html).
- **nix-direnv** — the glue that lets direnv load Nix flakes;
  installation guide at
  [github.com/nix-community/nix-direnv](https://github.com/nix-community/nix-direnv#installation).
  Make sure you've hooked direnv into your shell (the install docs cover
  bash / zsh / fish).

### 2. Activate the dev shell

```bash
cd ezal/
direnv allow
```

The first activation takes a few minutes — Nix is downloading the Rust
toolchain, probe-rs, and so on. After that, activation is instant and
cached.

When the shell is loaded you have:

- `cargo`, `rustc`, `rustfmt`, `clippy` pinned to the version in
  `rust-toolchain.toml`,
- `probe-rs` — used by `cargo run` to flash and stream defmt logs,
- `flip-link` — optional linker wrapper for stack-overflow trapping
  (off by default in `.cargo/config.toml`; uncomment the `linker =`
  line to enable it),
- `picotool` — optional, for UF2 / BOOTSEL flashing without a probe,
- `jq` — reads Cargo's executable artifact path during UF2 packaging,
- `node` — runs dashboard behavior tests with its built-in test runner,
- `python3` with `schemdraw` and `matplotlib` ready to go for
  `python3 design/circuit.py`.

### 3. Linux only: udev rules

If you're on a non-NixOS Linux distro, the debug probe and the Pico 2
in BOOTSEL mode need non-root access. Probe-rs's udev rules have to
live in `/etc/udev/rules.d` to take effect, so they aren't (and can't
be) installed by the flake — you do this one-off, system-wide:

```bash
sudo curl -L https://probe.rs/files/69-probe-rs.rules \
    -o /etc/udev/rules.d/69-probe-rs.rules
sudo udevadm control --reload-rules
sudo udevadm trigger
```

Then unplug and replug the probe.

On NixOS, add `services.udev.packages = [ pkgs.probe-rs-tools ];` to
your system configuration instead and rebuild.

### 4. WiFi credentials

The firmware joins your WiFi in station mode, and — since the Pico has no
filesystem — the SSID and password are baked in at build time. Copy the
template and fill it in:

```bash
cp .env.example .env
$EDITOR .env          # set EZAL_WIFI_SSID and EZAL_WIFI_PASSWORD
```

`.env` is git-ignored. `crates/ezal-firmware/build.rs` reads it at compile
time (and the dev shell also exports it into your environment via direnv's
`dotenv`, so a fresh `direnv allow` picks it up). Leave the password blank
for an open network; protected joins use WPA3/SAE. Building *without* a `.env` still compiles — CI does
exactly that — but the firmware's WiFi POST then fails at boot until the
credentials are set. The CYW43439's own firmware blobs are vendored in-tree
(`crates/ezal-firmware/cyw43-firmware/`), so there's nothing else to fetch.

An explicitly set environment variable overrides `.env`, including an empty
value. To build an artifact without local network credentials, set both
`EZAL_WIFI_SSID=` and `EZAL_WIFI_PASSWORD=` on the build command. Build images
and Cargo's cached build-script output contain any supplied credentials;
handle them accordingly. Unreadable files and credentials that cannot fit the
single-line build format fail the build instead of being silently truncated.

### macOS / Windows

macOS needs no extra setup beyond the steps above — probe-rs uses
IOKit on Darwin and works without drivers.

The Nix inputs follow current unstable packages on Linux and Apple Silicon.
Intel macOS uses the newest supported 26.05 Darwin branch because Nixpkgs
26.11 removed that platform. Rust remains pinned to the same 1.99.0 release
on every platform, and CI action releases are pinned by commit.

Windows is not officially supported by this flake (Nix on Windows runs
under WSL2, which works fine, but plain-Windows users would need a
non-Nix path — see below).

### Without Nix (alternative path)

If you'd rather not install Nix, you can still build the project the
old way: rustup reads `rust-toolchain.toml` automatically, and the
Rust tools can be installed with Cargo. Install Node.js 18 or newer separately
for the dashboard tests, and picotool and jq separately for UF2 packaging.

```bash
rustup show                                # installs the pinned toolchain
cargo install --locked probe-rs-tools
cargo install --locked flip-link           # optional
node --version                            # Node.js 18+ for dashboard tests
# picotool: https://github.com/raspberrypi/picotool
# Only needed if you're re-rendering the schematic:
pip install --user schemdraw matplotlib
```

You're then responsible for keeping versions roughly in sync with what
the flake pins. CI runs its Rust jobs inside the flake's lean `.#ci` shell
(`nix develop .#ci`) and the dashboard job inside `.#dashboard` (Node only),
so `flake.lock` is the authoritative pin for the toolchain — `picotool`,
Node.js and all — that the build is checked against.

## Day-to-day workflow

### Build the firmware

```bash
cargo build -p ezal-firmware --release
```

That command builds the default hardware-driving walking skeleton. It POSTs the
ADS1015 and autonomously drives the rotator to a synthetic pass start. Manual
hardware mode disables the autonomous target source:

```bash
cargo build -p ezal-firmware --release --no-default-features
```

Do not flash either mode onto a connected rotator until completing
[CALIBRATION.md](CALIBRATION.md).

The output lands at:

```
target/thumbv8m.main-none-eabihf/release/ezal-firmware
```

(That's an ELF, not a UF2. probe-rs flashes the elf directly; picotool can
take an elf with `-t elf`. To get a drag-and-drop `.uf2` instead, see
[Build a UF2 for BOOTSEL flashing](#build-a-uf2-for-bootsel-flashing) below.)

### Flash and watch logs

```bash
cargo run -p ezal-firmware --release
```

This is the *one command you'll run most*. It:

1. Builds the firmware (incrementally — the second run is fast).
2. Calls `probe-rs run` with the elf and chip name from `.cargo/config.toml`.
3. probe-rs flashes the chip, asserts a reset, attaches to RTT, and prints
   defmt log frames as they arrive.
4. Ctrl-C exits, leaving the chip running whatever you last flashed.

Illustrative logs (timings and the ADC config's conversion-ready bit vary):

```
0.000123 INFO  ezal adc-bringup: POSTing ADS1015 at 0x48
0.002456 INFO  ADS1015 POST OK: config=0xc383, A0=1280 mV, A1=114 mV
0.002789 INFO  ezal: autonomous walking skeleton; hardware drive enabled
0.003012 INFO  tracking: METOP-C acquire 1, 120000 ms remaining
0.003234 INFO  ezal wifi-bringup: joining "my-network" in STA mode
2.310456 INFO  WiFi POST OK: joined "my-network" (secured)
2.320789 INFO  ezal net: waiting for DHCP
3.100123 INFO  ezal net: DHCP OK
3.101234 INFO  ezal web: listening on http://<dhcp-address>/
```

Feedback sampling starts before the network, but outputs wait for a station
link and usable DHCP address. The tracker then acquires and settles at the
pass start, runs a 180-second pass, stops for 30 seconds, and reacquires the
next start. Network loss inhibits the four direction outputs and invalidates
pending commands. On recovery, autonomous mode reacquires the pass start;
manual mode requires control ownership and fresh operator input. Existing
ADC, acquisition, and motion fault latches survive network recovery.

With `--no-default-features`, the ADS1015 remains active but the dashboard
exposes manual leased direction controls instead of the autonomous pass. Both
modes require fresh, valid calibrated feedback at the final GPIO boundary.
Initial calibration uses the G-5500 front-panel controls with the Pico's
direction leads disconnected; see [CALIBRATION.md](CALIBRATION.md).

The output task arms a 500 ms hardware watchdog, serviced on a fixed 100 ms
schedule while the executor runs normally. It continues counting during a
debugger halt. A timeout resets the output hardware and boot then inhibits
motion before ADC or network startup. Inspect the fault before clearing that
state with a board reset, debugger reset, or power cycle. Flashing/debugging
can reset the board and therefore re-enable the default autonomous startup
behavior; a UF2 or `picotool` reboot also goes through the watchdog, and boot
distinguishes it from a firmware-armed timeout, so a freshly flashed board
starts normally instead of looking dead.

Network supervision retries association and 30-second DHCP attempts with
exponential backoff capped at 30 seconds. HTTP acceptors are started without
waiting for the first address, so a late AP or DHCP recovery exposes the
dashboard without resetting the board. Successful link/address observations
renew a 300 ms motion permit every 100 ms; the output task expires that permit
independently if supervision stalls. Internet access is not required.

### Build a UF2 for BOOTSEL flashing

If you don't have a debug probe, you can flash over the Pico 2's built-in
USB bootloader instead. Build a UF2 image:

```bash
./scripts/build-uf2.sh                 # release; --debug for a debug build
./scripts/build-uf2.sh --no-default-features  # manual mode
```

The UF2 lands next to the elf:

```
target/thumbv8m.main-none-eabihf/release/ezal-firmware.uf2
```

Then flash it without any extra tooling on the target machine:

1. Hold the **BOOTSEL** button while plugging the Pico 2 into USB.
2. It mounts as a mass-storage drive named `RP2350`.
3. Copy the `.uf2` onto that drive. The chip flashes it and reboots into
   the new firmware automatically.

The script just runs `cargo build` and then `picotool uf2 convert` on the
resulting elf, tagging the image with the `rp2350-arm-s` family (the
correct one for our secure-Arm build) and verifying every block lands in
valid RP2350 flash. It uses Cargo's JSON artifact report to locate the ELF,
including when `--target-dir` or Cargo configuration changes its directory;
it never falls back to an older image at the default path. `picotool` and `jq`
come from the dev shell. Build and test helpers use `--locked` so dependency
changes require an explicit lockfile update.

The trade-off versus `cargo run` is that there's no probe attached, so you
won't see the live defmt log stream — the firmware still emits it over RTT,
there's just nothing reading it. If you *do* have the Pico 2 attached over
USB in BOOTSEL mode, `picotool load <file>.uf2` flashes the same image
straight from the command line.

### Run the host tests

```bash
./scripts/test-host.sh
./scripts/test-dashboard.sh
./scripts/test-build-tools.sh
```

This is equivalent to `cargo test -p ezal-core` with the right `--target`
override (since the workspace default target is the Pico's ARM core,
running tests for the host needs an explicit triple — the script computes
it from `rustc -vV`). Host tests cover the composed supervisor, the feedback
gate and its travel-aware endpoint band, ownership, the command mailbox, and
the actuator sequence the firmware itself runs, as well as the actual pinned
picoserve HTTP upgrade/WebSocket parser with deterministic timeouts. Picoserve
is a test-only dependency of the otherwise dependency-free core, sharing the
workspace's version pin with the firmware so both exercise one parser.

Apart from one sanity test of the installed calibration, the policy tests use
their own fixture in `crates/ezal-core/tests/common/`, so replacing
`HARD_CODED_CALIBRATION` with your measurements cannot break them.

The dashboard script runs the actual embedded JavaScript against simulated
DOM, sockets and timers using Node's built-in runner. It checks pointer
ownership, release/cancellation, command refresh, control takeover, reconnect,
telemetry expiry, fresh-press recovery, and rendered telemetry without
installing npm packages. Rust tests exercise stuck feedback on both axes,
progress across short jogs/reversals, directional/speed faults, network health
expiry, stale tokens and replays, and legal control frames inside fragmented
WebSocket messages without extending their deadline.

The build-tool script tests the build script as a host executable with synthetic
configuration, then substitutes Cargo and picotool to verify artifact selection.
It needs Rust, a host linker, Bash, and jq. It never reads local credentials or
flashes hardware. CI runs it alongside formatting. The workspace's supported
minimum Rust version matches the tested pin in `rust-toolchain.toml`.

### Format and lint

Before pushing:

```bash
cargo fmt --all -- --check
HOST_TRIPLE=$(rustc -vV | sed -n 's/host: //p')
cargo clippy -p ezal-core --target "$HOST_TRIPLE" --all-targets --all-features -- -D warnings
cargo clippy -p ezal-firmware --bins -- -D warnings
cargo clippy -p ezal-firmware --bins --no-default-features -- -D warnings
```

Use `cargo fmt --all` to apply formatting. Firmware has no embedded `libtest`,
so do not select its test targets with `--workspace --all-targets`.

CI runs these checks, both test scripts, and one firmware job that builds,
lints and packages UF2 images for autonomous and manual modes, uploading them
under `dist/<mode>/`. Both feature sets write the same ELF/UF2 path, so the
last build — locally, or within that job — determines which mode that file
contains.

### Watching for file changes

If you have `cargo-watch` installed:

```bash
cargo watch -x 'build -p ezal-firmware'
```

…will recompile on every save without any other ceremony.

## Common tasks

### Cleaning the build

```bash
cargo clean
```

Wipes `target/`. Rare; cargo handles incremental builds well.

### Looking at the firmware size

```bash
cargo size -p ezal-firmware --release -- -A
```

Requires `cargo-binutils` (`cargo install --locked cargo-binutils`) as well as
the `llvm-tools` component already in our toolchain file. The optional Cargo
subcommands are not installed by the Nix shell. `.text` is executable code;
flash also contains read-only data and initial values for `.data`. `.data` and
`.bss` describe static RAM usage, excluding runtime stack use.

### Inspecting the disassembly

```bash
cargo objdump -p ezal-firmware --release -- --disassemble --no-show-raw-insn
```

Uses the same `cargo-binutils` and `llvm-tools` prerequisites.

### Running a debugger session

```bash
probe-rs gdb --chip RP235x \
    target/thumbv8m.main-none-eabihf/release/ezal-firmware
```

…then connect arm-none-eabi-gdb to it. For most bugs `defmt` is enough
and a debugger is overkill, but breakpoints can save you when you're
hunting a startup-time issue.

The watchdog remains active during breakpoints once the output task starts;
the board resets after 500 ms without output service. Disconnect motor power
for interactive debugging, and expect the next boot to inhibit motion after a
watchdog timeout. A debugger reset clears that inhibition.

## Debugging tips

- **No logs?** Make sure your debug probe is connected to SWCLK + SWDIO +
  GND, and that probe-rs can see it: `probe-rs list`.
- **`error: failed to find ROM resource for RP235x`** → update probe-rs.
  Older versions don't know about the RP2350 chip ID.
- **Chip won't boot / drops back to BOOTSEL** → the boot ROM didn't find a
  valid `IMAGE_DEF` block in the first 4 KiB of flash. Two causes: the block
  is *missing* (the `imagedef-secure-exe` feature must be on, and a custom
  `static` would need `#[link_section = ".start_block"] #[used]`), or — the
  subtler one — it's *misplaced*. cortex-m-rt's `link.x` doesn't position
  `.start_block`, so the `SECTIONS … INSERT AFTER` block in `memory.x` is
  what pins it after the vector table; without it the linker dumps it at the
  end of the image, out of the boot ROM's reach. Verify placement with:

  ```bash
  cargo objdump -p ezal-firmware --release -- -h | grep start_block
  ```

  The address should be just past `.vector_table` (≈ `0x10000114`), not
  several KiB in.
- **Stack overflow** → inspect stack usage and large local buffers; enable
  `flip-link` so an overflow traps before corrupting static state.

### Hardware verification after safety changes

Cross-compilation and host tests cannot verify GPIO reset behavior or motor
coasting. With motor power disconnected and a logic analyzer on the direction
outputs, verify:

1. Holding the CPU at a breakpoint while a direction output is active causes
   watchdog reset and output removal within the configured 500 ms interval
   (allow for oscillator tolerance); the next boot stays inhibited. Capture
   the pin *through* the reset, not only after the reboot: an RP2350 pad
   isolation latch can hold the last output state until boot claims the pin,
   which is how long the base pull-downs take to win.
2. Loss of command refresh removes output at the 750 ms lease deadline. When
   activation is delayed by the 2-second off interval, a lease with less than
   500 ms left leaves the relay alone entirely — check that no brief click
   occurs, and that a refreshed lease then starts the output normally.
3. Invalid, missing, or stalled ADC feedback inhibits both operating modes;
   a cancelled read is not retried until reset. A read slower than 150 ms
   reports `feedback-stale` without latching.
4. At each measured endpoint, an outward jog is inhibited and an inward jog
   is permitted. Approaching an endpoint under power, the output is cut
   before reaching it rather than at the next sample. Check intermediate
   positions after the change to 2 mV ADC resolution, then measure
   coast/brake distance with motor power restored.
5. Flashing a CI UF2 by BOOTSEL boots into normal operation: the boot ROM's
   own watchdog reboot is not reported as a firmware watchdog fault.

## Editor setup

`rust-analyzer` works out of the box, but for the firmware crate it
needs to know about the cross-compile target. The `.cargo/config.toml`
already covers this — VS Code's rust-analyzer extension reads it.

If you find rust-analyzer is checking against the host target and
flagging `embassy-rp` errors, set the `rust-analyzer.cargo.target` setting
to `thumbv8m.main-none-eabihf` in `.vscode/settings.json`:

```json
{
    "rust-analyzer.cargo.target": "thumbv8m.main-none-eabihf",
    "rust-analyzer.check.allTargets": false
}
```

(Not committed to keep editor preferences out of the repo.)
