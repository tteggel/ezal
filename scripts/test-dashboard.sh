#!/usr/bin/env bash
# Execute the JavaScript embedded in the served dashboard with Node's built-in
# test runner. No npm packages or browser installation are required.
set -euo pipefail

REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
exec node "$@" "${REPO_ROOT}/crates/ezal-core/tests/dashboard.test.mjs"
