#!/usr/bin/env bash
# test-host.sh — run the host-side unit tests for ezal-core.
#
# Why this script exists:
#   The workspace .cargo/config.toml sets a *default* build target of
#   thumbv8m.main-none-eabihf so the common case (building the firmware)
#   is one command. But the ezal-core tests need to run on the host, not
#   on the chip — so we must override the target with the host triple.
#
# rustc's `-vV` output contains a line like "host: x86_64-unknown-linux-gnu"
# (or whatever your platform is). We extract it with sed.

set -euo pipefail

HOST_TRIPLE=$(rustc -vV | sed -n 's/host: //p')
if [[ -z "${HOST_TRIPLE}" ]]; then
    echo "✗ could not determine host triple from rustc -vV" >&2
    exit 1
fi

echo "→ running ezal-core tests for ${HOST_TRIPLE}"
exec cargo test -p ezal-core --target "${HOST_TRIPLE}" --all-features "$@"
