#!/usr/bin/env bash
# flash.sh — build and flash the ezal firmware to a connected Pico 2.
#
# Defaults to a *release* build (much smaller binary, marginally faster)
# and uses the cargo runner from .cargo/config.toml, which is probe-rs.
#
# Pass `--debug` to flash a debug build (slower, much larger binary,
# but with all asserts and more verbose defmt detail).

set -euo pipefail

PROFILE="--release"
if [[ "${1:-}" == "--debug" ]]; then
    PROFILE=""
    shift
fi

exec cargo run -p ezal-firmware ${PROFILE} "$@"
