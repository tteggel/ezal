#!/usr/bin/env bash
# Run the static checks: formatting, clippy for both crates, and rustdoc with
# warnings denied so a broken intra-doc link fails like a compile error.
set -euo pipefail

REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "${REPO_ROOT}"
HOST=$(rustc --print host-tuple)

cargo fmt --all -- --check
cargo clippy --locked -p ezal-core --target "$HOST" --all-targets -- -D warnings
# The firmware has no embedded libtest, so clippy checks its binary only.
cargo clippy --locked -p ezal-firmware --bins -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps -p ezal-core --target "$HOST"
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps -p ezal-firmware
