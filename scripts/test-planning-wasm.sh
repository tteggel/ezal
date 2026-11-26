#!/usr/bin/env bash
# Compare the actual browser WASM module against native Rust, including the
# exact lazy device ephemeris and independent uploaded-route verification.
set -euo pipefail
REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$REPO_ROOT"
PLANNING_CORPUS=$(mktemp -d "${TMPDIR:-/tmp}/ezal-planning.XXXXXX")
trap 'rm -rf "$PLANNING_CORPUS"' EXIT
HOST=$(rustc --print host-tuple)
cargo build --locked --release -p ezal-wasm --target wasm32-unknown-unknown
cargo run --locked --quiet --release -p ezal-core --example planning-equivalence --target "$HOST" -- "$PLANNING_CORPUS"
node crates/ezal-core/tests/planning-wasm.test.mjs "${CARGO_TARGET_DIR:-target}/wasm32-unknown-unknown/release/ezal_wasm.wasm" "$PLANNING_CORPUS"
