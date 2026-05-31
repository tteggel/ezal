# Development guide

This document walks through setting up a working development environment
on a fresh machine, and explains the day-to-day workflow.

## One-time setup

ezal pins every host-side tool — Rust, probe-rs, flip-link, picotool,
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

### macOS / Windows

macOS needs no extra setup beyond the steps above — probe-rs uses
IOKit on Darwin and works without drivers.

Windows is not officially supported by this flake (Nix on Windows runs
under WSL2, which works fine, but plain-Windows users would need a
non-Nix path — see below).

### Without Nix (alternative path)

If you'd rather not install Nix, you can still build the project the
old way: rustup reads `rust-toolchain.toml` automatically, and the
embedded tools are all `cargo install`able.

```bash
rustup show                                # installs the pinned toolchain
cargo install --locked probe-rs-tools
cargo install --locked flip-link           # optional
# picotool: https://github.com/raspberrypi/picotool
# Only needed if you're re-rendering the schematic:
pip install --user schemdraw matplotlib
```

You're then responsible for keeping versions roughly in sync with what
the flake pins. CI runs every job inside the flake's lean `.#ci` shell
(`nix develop .#ci`), so `flake.lock` is the authoritative pin for the
toolchain — `picotool` and all — that the build is checked against.

## Day-to-day workflow

### Build the firmware

```bash
cargo build -p ezal-firmware --release
```

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

You should see something like:

```
0.000123 INFO  ezal hello-morse: starting (dit = 150 ms)
0.000456 INFO  Transmitting: HELLO WORLD
```

…and the onboard LED blinking.

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
valid RP2350 flash. `picotool` comes from the dev shell.

The trade-off versus `cargo run` is that there's no probe attached, so you
won't see the live defmt log stream — the firmware still emits it over RTT,
there's just nothing reading it. If you *do* have the Pico 2 attached over
USB in BOOTSEL mode, `picotool load <file>.uf2` flashes the same image
straight from the command line.

### Run the host tests

```bash
./scripts/test-host.sh
```

This is equivalent to `cargo test -p ezal-core` with the right `--target`
override (since the workspace default target is the Pico's ARM core,
running tests for the host needs an explicit triple — the script computes
it from `rustc -vV`).

### Format and lint

Before pushing:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets
```

CI runs both in `-D warnings` mode, so a clippy warning will fail the
build.

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

Requires the `llvm-tools` rustup component, which is in our toolchain
file. The `text` section is the flash usage; `data + bss` is the RAM
usage at startup.

### Inspecting the disassembly

```bash
cargo objdump -p ezal-firmware --release -- --disassemble --no-show-raw-insn
```

Same component requirement.

### Running a debugger session

```bash
probe-rs gdb --chip RP235x \
    target/thumbv8m.main-none-eabihf/release/ezal-firmware
```

…then connect arm-none-eabi-gdb to it. For most bugs `defmt` is enough
and a debugger is overkill, but breakpoints can save you when you're
hunting a startup-time issue.

## Debugging tips

- **No logs?** Make sure your debug probe is connected to SWCLK + SWDIO +
  GND, and that probe-rs can see it: `probe-rs list`.
- **`error: failed to find ROM resource for RP235x`** → update probe-rs.
  Older versions don't know about the RP2350 chip ID.
- **Chip won't boot at all** → the `IMAGE_DEF` block is probably missing
  from your binary. The macro in `src/main.rs` only takes effect with
  the `#[link_section = ".start_block"] #[used] pub static` attributes
  intact; don't trim them as "unused".
- **Stack overflow** → either bump the stack in `memory.x` *carefully*,
  or enable `flip-link` so the next overflow traps loudly.

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
