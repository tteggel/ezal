# Development guide

This document walks through setting up a working development environment
on a fresh machine, and explains the day-to-day workflow.

## One-time setup

### 1. Install `rustup`

[`rustup`](https://rustup.rs) manages Rust toolchains. The first time you
`cd` into the workspace, rustup reads `rust-toolchain.toml` and
automatically downloads the pinned compiler, components, and ARM target.

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

After installation, `cargo --version` should print the pinned version
from inside the repo.

### 2. Install host-side tooling

```bash
./scripts/install-tools.sh
```

This installs:

- **`probe-rs-tools`** — flashes our firmware via a debug probe and
  streams `defmt` logs back to the terminal. Used by `cargo run`.
- **`flip-link`** — *optional but recommended*. A linker wrapper that
  inverts the stack so overflows trap (MPU fault) instead of silently
  corrupting statics. Not wired into the build by default; uncomment
  the `linker = "flip-link"` line in `.cargo/config.toml` to enable it.
- **`picotool`** — *optional*. Drag-and-drop UF2 flashing without a
  debug probe.

### 3. Linux only: udev rules

If you're on Linux, the debug probe and the Pico 2 in BOOTSEL mode need
non-root access. Install the rules:

```bash
sudo curl -L https://probe.rs/files/69-probe-rs.rules \
    -o /etc/udev/rules.d/69-probe-rs.rules
sudo udevadm control --reload-rules
sudo udevadm trigger
```

Then unplug and replug the probe.

### 4. macOS / Windows

No extra setup beyond rustup. On macOS, `probe-rs` works without drivers.
On Windows, install Zadig and assign WinUSB to the debug probe interface
(see the probe-rs install instructions).

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
take an elf with `-t elf`.)

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
