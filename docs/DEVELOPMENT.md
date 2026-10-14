# Development guide

This document walks through setting up a working development environment
on a fresh machine, and explains the day-to-day workflow.

## One-time setup

ezal pins the regular build and test tools — Rust, probe-rs, picotool, Node.js,
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
- `cargo size` and `cargo objdump` (`cargo-binutils` over the toolchain's
  `llvm-tools`) for inspecting the firmware image,
- `probe-rs` — used by `cargo run` to flash and stream defmt logs,
- `picotool` — optional, for UF2 / BOOTSEL flashing without a probe,
- `jq` — reads Cargo's executable artifact path during UF2 packaging,
- `node` — runs dashboard behavior tests with its built-in test runner,
- `python3` with `schemdraw` ready to go for `python3 design/circuit.py`.

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
time and is its only parser; the dev shell does not export it, so a build
bakes the same values with or without direnv. Later lines override earlier
ones, and an unquoted value ends at a ` #` comment (quote a value to keep
` #` in it; `#` with no space before it is kept). Leave the password blank
for an open network; protected joins use WPA3/SAE. Building *without* a `.env` still compiles — CI does
exactly that — but the network supervisor parks with a credential error until the
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
cargo install --locked cargo-binutils      # cargo size / cargo objdump
node --version                            # Node.js 18+ for dashboard tests
# picotool: https://github.com/raspberrypi/picotool
# Only needed if you're re-rendering the schematic:
pip install --user schemdraw
```

You're then responsible for keeping versions roughly in sync with what
the flake pins. CI runs its Rust jobs inside the flake's lean `.#ci` shell
(`nix develop .#ci`) and the dashboard job inside `.#dashboard` (Rust
configuration exporter and Node), so `flake.lock` is the authoritative pin
for the toolchain — `picotool`,
Node.js and all — that the build is checked against.

## Day-to-day workflow

### Build the firmware

```bash
cargo build -p ezal-firmware --release
```

The firmware POSTs the ADS1015 and starts with all direction outputs idle.
Movement requires explicit dashboard input and current feedback and network
permission. Real orbital predictions remain advisory.

Complete [CALIBRATION.md](CALIBRATION.md), including its
[bench verification](CALIBRATION.md#bench-verification), before commanding a
connected rotator.

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
0.000123 INFO  ezal: POSTing ADS1015 at 0x48
0.002456 INFO  ADS1015 POST OK: config=0xc383, A0=1280 mV, A1=114 mV
0.003234 INFO  ezal: manual control; outputs idle until operator input
2.310456 INFO  WiFi associated (secured); waiting for DHCP
3.100123 INFO  network ready; motion permission available
```

Feedback sampling starts before the network, but movement waits for a station
link, usable DHCP address, and fresh operator input. Network loss inhibits
the four direction outputs and invalidates pending commands. Recovery requires
control ownership and fresh input. Existing ADC and motion fault latches
survive network recovery.

The dashboard exposes leased manual direction controls and single-pass tracking.
Select a forecast row to preview, then Enter Tracking to arm; Prepare now
re-evaluates an earlier start. Cancel or Return to Manual interrupts the run.
Manual controls are hidden while tracking, and browser closure does not cancel it.
Movement requires fresh, valid calibrated feedback at the final GPIO boundary.
Initial calibration uses the G-5500 front-panel controls with the Pico's
direction leads disconnected; see [CALIBRATION.md](CALIBRATION.md).

The output task arms the hardware watchdog and services it on the periods the
[control contract](CONTROL_CONTRACT.md#ordering-and-time) sets. It continues
counting during a debugger halt. A timeout resets the output hardware and boot
then inhibits motion before ADC or network startup; the contract's
[executor-failure row](CONTROL_CONTRACT.md#ownership-and-recovery) says which
reboots count as a timeout. Inspect the fault before clearing that state with a
board reset, debugger reset, or power cycle. A freshly flashed board starts
idle and waits for manual input or explicit tracking arming.

Network supervision retries association and 30-second DHCP attempts with
exponential backoff capped at 30 seconds. HTTP acceptors are started without
waiting for the first address, so a late AP or DHCP recovery exposes the
dashboard without resetting the board. Successful link/address observations
renew a short motion permit,
[`NETWORK_HEALTH_TIMEOUT_MS`](CONTROL_CONTRACT.md#ownership-and-recovery); the
output task expires that permit independently if supervision stalls. Manual
control does not require Internet access; tracking needs valid cached orbital
data and a quantified clock.

### Build a UF2 for BOOTSEL flashing

If you don't have a debug probe, you can flash over the Pico 2's built-in
USB bootloader instead. Build a UF2 image:

```bash
./scripts/build-uf2.sh                 # release; --debug for a debug build
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
it never falls back to an older image at the default path, and prints the
ELF and UF2 paths on stdout. `picotool` and `jq` come from the dev shell.
Build and test helpers use `--locked` so dependency changes require an
explicit lockfile update.

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

- `test-host.sh` runs `cargo test -p ezal-core` for the host tuple
  (`rustc --print host-tuple`), since the workspace's default target is the
  Pico's; extra arguments go to `cargo test`.
- `test-dashboard.sh` exports the production client configuration with Rust,
  then runs the served JavaScript against a simulated DOM, sockets and timers
  with Node's built-in runner, without npm packages.
- `test-build-tools.sh` runs the firmware build script as a host executable
  with synthetic configuration and substitutes Cargo and picotool to check
  artifact selection; it never reads local credentials or flashes hardware.

The tests under `crates/ezal-core/tests/` and the in-crate `tests` modules are
the inventory; [ARCHITECTURE.md](ARCHITECTURE.md#verification) gives the rule
for where a new test goes. The JSON fixtures in `crates/ezal-core/tests/fixtures/`
pin the wire formats that the dashboard tests render; set
`EZAL_UPDATE_FIXTURES=1` when running the host tests to regenerate them after
an intended change. The core's only runtime dependencies are `sgp4`, `libm` and
`heapless`; picoserve is test-only (see `crates/ezal-core/Cargo.toml`).

### Format and lint

Before pushing:

```bash
./scripts/lint.sh
```

The script checks formatting, runs clippy with warnings denied on the host
build of ezal-core (all targets) and on the firmware binary, and builds both
crates' rustdoc with warnings denied, so a broken intra-doc link fails. Use
`cargo fmt --all` to apply formatting. Firmware has no embedded `libtest`, so
do not select its test targets with `--workspace --all-targets`.

CI runs the same scripts: the lint script, the three test scripts, and
`build-uf2.sh`, uploading the firmware's ELF and UF2 under `dist/`.

To dump the CYW43 radio's own console log, enable the driver's
diagnostic feature with a debug defmt filter:

```bash
DEFMT_LOG=debug cargo run -p ezal-firmware --release --features cyw43/firmware-logs
```

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

`.text` is executable code; flash also contains read-only data and initial
values for `.data`. `.data` and `.bss` describe static RAM usage, excluding
runtime stack use.

### Inspecting the disassembly

```bash
cargo objdump -p ezal-firmware --release -- --disassemble --no-show-raw-insn
```

### Running a debugger session

```bash
probe-rs gdb --chip RP235x \
    target/thumbv8m.main-none-eabihf/release/ezal-firmware
```

…then connect arm-none-eabi-gdb to it. For most bugs `defmt` is enough
and a debugger is overkill, but breakpoints can save you when you're
hunting a startup-time issue.

The watchdog remains active during breakpoints once the output task starts; the
board resets once
[`WATCHDOG_TIMEOUT`](CONTROL_CONTRACT.md#ownership-and-recovery) passes without
output service. Disconnect motor power for interactive debugging, and expect
the next boot to inhibit motion after a watchdog timeout. A debugger reset
clears that inhibition.

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
- **Stack overflow** → inspect stack usage and large local buffers. Reset
  loads MSPLIM with `_stack_end` (cortex-m-rt's `set-msplim` feature), so a
  stack pointer below the reserved stack faults instead of overwriting static
  state; `memory.x` puts `_stack_end` on an 8-byte boundary because MSPLIM
  ignores the low three bits. Once `direction_task` has armed the watchdog,
  the fault stops the feeding and the board reboots with motion inhibited;
  before that, the core halts (in HardFault, or locked up when the handler's
  first push crosses the limit again) until a manual reset, with the
  direction outputs low or still in their reset state, so every switch stays
  off.

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
