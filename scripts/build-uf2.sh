#!/usr/bin/env bash
# build-uf2.sh — build the ezal firmware and package it as a UF2 image for
# drag-and-drop BOOTSEL flashing without a debug probe. docs/DEVELOPMENT.md
# describes the flashing steps.
#
# Builds the release profile unless `--debug` is passed; further arguments go
# to `cargo build` (e.g. `--features ...` or `--target-dir ...`). The build
# target comes from .cargo/config.toml.
#
# Output: the ELF and UF2 paths on stdout, one per line; progress and
# diagnostics go to stderr.

set -euo pipefail

# Resolve the workspace root from this script's own location and work from
# there, so the script can be run from any working directory.
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "${ROOT}"

# Release unless --debug is passed. Cargo reports the actual output path,
# so profiles and target-directory overrides cannot select an older ELF.
PROFILE_FLAG="--release"
if [[ "${1:-}" == "--debug" ]]; then
    PROFILE_FLAG=""
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

if ! command -v jq >/dev/null 2>&1; then
    echo "✗ jq not found on PATH (provided by the Nix dev shell)." >&2
    exit 1
fi

# 1. Build the firmware ELF (forwarding any extra cargo args). Never guess
# its location: Cargo configuration, environment, and CLI flags can all change
# that path. A guessed path can silently package a previous firmware build.
# Diagnostics stay on stderr while compiler-artifact messages go to this file.
BUILD_MESSAGES=$(mktemp)
trap 'rm -f -- "$BUILD_MESSAGES"' EXIT
cargo build --locked -p ezal-firmware --bin ezal-firmware \
    ${PROFILE_FLAG} "$@" --message-format=json-render-diagnostics > "${BUILD_MESSAGES}"
ELF=$(jq -ers '
    [.[] | select(.reason == "compiler-artifact")
         | select(.target.name == "ezal-firmware")
         | select(.target.kind | index("bin"))
         | .executable | select(. != null)] | unique
    | if length == 1 then .[0] else error("expected one firmware executable") end
' "${BUILD_MESSAGES}")
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
    --family rp2350-arm-s --platform rp2350 >&2

{
    echo
    echo "✓ UF2 written to ${UF2}"
    echo "  To flash: hold BOOTSEL, plug in the Pico 2, then copy the file onto"
    echo "  the 'RP2350' drive that appears — or run: picotool load \"${UF2}\""
} >&2
printf '%s\n' "${ELF}" "${UF2}"
