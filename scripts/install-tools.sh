#!/usr/bin/env bash
# install-tools.sh — bootstrap the host-side tooling needed to build, flash,
# and debug the ezal firmware.
#
# Idempotent: re-running is safe. Designed to work on Linux, macOS, and
# WSL2; on Windows please use the bash script under Git Bash or WSL.
#
# This script does NOT install anything that requires sudo. The two
# privileged steps (udev rules on Linux, drivers on Windows) are linked
# from docs/DEVELOPMENT.md and left to the user.

set -euo pipefail

bold()  { printf '\033[1m%s\033[0m\n' "$*"; }
log()   { printf '  • %s\n' "$*"; }
warn()  { printf '\033[33m  ! %s\033[0m\n' "$*"; }
die()   { printf '\033[31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

# 1. rustup --------------------------------------------------------------
bold "Checking rustup is installed..."
command -v rustup >/dev/null || die \
    "rustup not found. Install from https://rustup.rs and re-run this script."

# Pull the toolchain version, components, and targets pinned in
# rust-toolchain.toml. `rustup show` triggers the install if absent.
bold "Installing pinned toolchain from rust-toolchain.toml..."
rustup show

# 2. probe-rs ------------------------------------------------------------
#
# probe-rs is the modern Rust replacement for openocd. It speaks SWD/JTAG
# over a debug probe (Raspberry Pi Debug Probe, J-Link, CMSIS-DAP, …),
# flashes our firmware, and streams defmt logs back to the host. The cargo
# `runner = "probe-rs run ..."` line in .cargo/config.toml is what calls it.
bold "Installing probe-rs-tools..."
if command -v probe-rs >/dev/null; then
    log "probe-rs already installed — $(probe-rs --version 2>&1 | head -1)"
else
    cargo install --locked probe-rs-tools
fi

# 3. flip-link (optional but recommended) --------------------------------
#
# flip-link is a linker wrapper that places the stack at the *bottom* of
# RAM rather than the top, so a stack overflow triggers an MPU fault
# instead of silently corrupting your statics. We don't enable it by
# default (so the build works without it), but every contributor should
# install it locally and uncomment the `linker = "flip-link"` line in
# .cargo/config.toml when working on stack-heavy code.
bold "Installing flip-link (recommended)..."
if command -v flip-link >/dev/null; then
    log "flip-link already installed"
else
    cargo install --locked flip-link
fi

# 4. picotool (optional) -------------------------------------------------
#
# picotool talks to the Pico 2's USB BOOTSEL bootloader. You only need it
# if you're flashing without a debug probe (drag-and-drop UF2 style).
# Building it requires CMake + a recent libusb; we just print a hint and
# leave it to the user.
bold "picotool (optional)"
if command -v picotool >/dev/null; then
    log "picotool installed — $(picotool version 2>&1 | head -1)"
else
    warn "picotool not installed. Optional — only needed for UF2 flashing."
    warn "  See: https://github.com/raspberrypi/picotool"
fi

bold "Done."
cat <<'EOF'

Next steps:
  1. Plug a Raspberry Pi Debug Probe (or compatible) into the Pico 2's
     SWD pins (SWCLK, SWDIO, GND).
  2. Connect both to your host over USB.
  3. From the workspace root:

         cargo run -p ezal-firmware --release

     should build, flash, and start streaming defmt logs.

  4. To run the host-side ezal-core tests:

         ./scripts/test-host.sh

See docs/DEVELOPMENT.md for the full setup, including Linux udev rules.
EOF
