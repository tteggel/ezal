#!/usr/bin/env bash
# Execute the JavaScript embedded in the served dashboard with Node's built-in
# test runner, with the exact configuration emitted by Rust. No npm packages
# or browser installation are required.
set -euo pipefail

REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "${REPO_ROOT}"
HOST=$(rustc --print host-tuple)
CONFIG_FILE=$(mktemp "${TMPDIR:-/tmp}/ezal-dashboard-config.XXXXXX.js")
trap 'rm -f "$CONFIG_FILE"' EXIT
cargo run --quiet --locked -p ezal-core --example dashboard-config --target "$HOST" > "$CONFIG_FILE"
EZAL_DASHBOARD_CONFIG="$CONFIG_FILE" node "$@" "${REPO_ROOT}/crates/ezal-core/tests/dashboard.test.mjs"
