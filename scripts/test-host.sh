#!/usr/bin/env bash
# Run ezal-core's tests on the host. The workspace's default target is the
# Pico's (.cargo/config.toml), so the host tuple is named explicitly.
set -euo pipefail

REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "${REPO_ROOT}"
HOST=$(rustc --print host-tuple)
exec cargo test --locked -p ezal-core --target "$HOST" "$@"
