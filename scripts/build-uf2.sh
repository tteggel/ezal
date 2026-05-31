#!/usr/bin/env bash
# build-uf2.sh — build the ezal firmware and package it as a UF2 image
# for drag-and-drop BOOTSEL flashing (no debug probe required).
#
# A UF2 is the file format the RP2350's built-in USB mass-storage
# bootloader accepts. Hold the Pico 2's BOOTSEL button while plugging it
# in; it mounts as a drive named "RP2350"; copy the .uf2 onto that drive
# and the chip reboots straight into the new firmware. This path needs no
# probe-rs, no debug probe, and nothing installed on the machine doing the
# copy — handy for handing a build to someone else to flash.
#
# The trade-off versus `cargo run` / scripts/flash.sh (which flash over a
# debug probe): you lose the live defmt log stream. The firmware still
# emits logs over RTT, there's just no probe attached to read them.
#
# Defaults to a *release* build — smaller, and what you'd actually deploy.
# Pass `--debug` for a debug build instead. Any further arguments are
# forwarded to `cargo build` (e.g. `--features ...`).
#
# Output: target/thumbv8m.main-none-eabihf/<profile>/ezal-firmware.uf2

set -euo pipefail

# Resolve the workspace root from this script's own location and work from
# there, so the script can be run from any working directory.
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "${ROOT}"

# The firmware's build target is fixed in .cargo/config.toml. Keep this in
# lock-step with that file (and the paths quoted throughout the docs).
TARGET="thumbv8m.main-none-eabihf"

# Release unless --debug is passed. PROFILE_DIR is the matching cargo
# output subdirectory — the dev profile lands in target/.../debug/.
PROFILE_FLAG="--release"
PROFILE_DIR="release"
if [[ "${1:-}" == "--debug" ]]; then
    PROFILE_FLAG=""
    PROFILE_DIR="debug"
    shift
fi

# picotool does the ELF→UF2 conversion. It's provided by the Nix dev
# shell; fail early with a pointer rather than a confusing error part-way
# through a build.
if ! command -v picotool >/dev/null 2>&1; then
    echo "✗ picotool not found on PATH." >&2
    echo "  It's provided by the Nix dev shell — see flake.nix / docs/DEVELOPMENT.md." >&2
    echo "  Without Nix: https://github.com/raspberrypi/picotool" >&2
    exit 1
fi

# 1. Build the firmware ELF (forwarding any extra cargo args).
cargo build -p ezal-firmware ${PROFILE_FLAG} "$@"

# Honour CARGO_TARGET_DIR if the caller set one; default to ./target.
TARGET_DIR="${CARGO_TARGET_DIR:-${ROOT}/target}"
ELF="${TARGET_DIR}/${TARGET}/${PROFILE_DIR}/ezal-firmware"
UF2="${ELF}.uf2"

if [[ ! -f "${ELF}" ]]; then
    echo "✗ expected firmware ELF not found at ${ELF}" >&2
    exit 1
fi

# 2. Convert ELF → UF2.
#      -t elf            the ELF has no file extension, so name its type.
#      --family rp2350-arm-s
#                        tag the blocks as an Arm secure image — the right
#                        family for our `imagedef-secure-exe` build. Left
#                        to itself picotool falls back to the generic
#                        `absolute` family (the embassy ELF carries no
#                        binary-info metadata to key off); that also boots,
#                        but isn't what RP2350 tooling expects to see.
#      --platform rp2350 verify every block lands in valid RP2350 flash.
#    The .uf2 extension on the output is enough for picotool to infer the
#    output type, so no second -t is needed.
picotool uf2 convert "${ELF}" -t elf "${UF2}" \
    --family rp2350-arm-s --platform rp2350

echo
echo "✓ UF2 written to ${UF2}"
echo "  To flash: hold BOOTSEL, plug in the Pico 2, then copy the file onto"
echo "  the 'RP2350' drive that appears — or run: picotool load \"${UF2}\""
