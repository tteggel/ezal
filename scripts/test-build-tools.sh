#!/usr/bin/env bash
# Build tooling runs on the host even when its package targets the Pico. Test
# its public process interface with synthetic credentials and fake build tools;
# no test reads a contributor's .env, flashes hardware, or needs a network.
set -euo pipefail

REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
TEST_ROOT=$(mktemp -d)
trap 'rm -rf -- "$TEST_ROOT"' EXIT
mkdir -p "$TEST_ROOT/workspace/crates/firmware" "$TEST_ROOT/out" "$TEST_ROOT/bin"

rustc --edition=2021 "$REPO_ROOT/crates/ezal-firmware/build.rs" -o "$TEST_ROOT/build-script"
export CARGO_MANIFEST_DIR="$TEST_ROOT/workspace/crates/firmware"
export OUT_DIR="$TEST_ROOT/out"
cat > "$TEST_ROOT/workspace/.env" <<'EOF'
EZAL_WIFI_SSID=fixture-network
EZAL_WIFI_PASSWORD=fixture-password
EOF

# Absent process values use .env; explicitly empty values must override it.
output=$(env -u EZAL_WIFI_SSID -u EZAL_WIFI_PASSWORD "$TEST_ROOT/build-script")
[[ "$output" == *'cargo:rustc-env=EZAL_WIFI_SSID=fixture-network'* ]]
[[ "$output" == *'cargo:rustc-env=EZAL_WIFI_PASSWORD=fixture-password'* ]]
output=$(EZAL_WIFI_SSID=override EZAL_WIFI_PASSWORD= "$TEST_ROOT/build-script")
[[ "$output" == *$'cargo:rustc-env=EZAL_WIFI_SSID=override\n'* ]]
[[ "$output" == *$'cargo:rustc-env=EZAL_WIFI_PASSWORD=\n'* ]]
[[ "$output" != *fixture-password* ]]
output=$(EZAL_WIFI_SSID= EZAL_WIFI_PASSWORD= "$TEST_ROOT/build-script")
[[ "$output" != *fixture-network* && "$output" != *fixture-password* ]]

# A line break must fail before either credential reaches Cargo's stdout.
if EZAL_WIFI_SSID=fixture EZAL_WIFI_PASSWORD=$'secret\ncargo:warning=injected' \
    "$TEST_ROOT/build-script" > "$TEST_ROOT/output" 2> "$TEST_ROOT/error"; then
    echo 'FAIL: accepted a multiline credential' >&2
    exit 1
fi
output=$(cat "$TEST_ROOT/output" "$TEST_ROOT/error")
[[ "$output" != *secret* && "$output" != *injected* ]]
[[ "$output" != *cargo:rustc-env=* ]]

# The single-line dotenv format rejects quoted multiline credentials too;
# splitting lines must not silently turn a malformed secret into a new value.
cat > "$TEST_ROOT/workspace/.env" <<'EOF'
EZAL_WIFI_SSID=fixture-network
EZAL_WIFI_PASSWORD="first-secret-line
second-secret-line"
EOF
if env -u EZAL_WIFI_SSID -u EZAL_WIFI_PASSWORD "$TEST_ROOT/build-script" \
    > "$TEST_ROOT/output" 2> "$TEST_ROOT/error"; then
    echo 'FAIL: truncated a multiline dotenv credential' >&2
    exit 1
fi
output=$(cat "$TEST_ROOT/output" "$TEST_ROOT/error")
[[ "$output" != *secret-line* && "$output" != *cargo:rustc-env=* ]]

# A missing file is a supported CI case; invalid UTF-8 is a configuration error.
rm "$TEST_ROOT/workspace/.env"
env -u EZAL_WIFI_SSID -u EZAL_WIFI_PASSWORD "$TEST_ROOT/build-script" > /dev/null
printf '\377' > "$TEST_ROOT/workspace/.env"
if env -u EZAL_WIFI_SSID -u EZAL_WIFI_PASSWORD "$TEST_ROOT/build-script" \
    > /dev/null 2> "$TEST_ROOT/error"; then
    echo 'FAIL: silently ignored an unreadable dotenv file' >&2
    exit 1
fi

# Simulate Cargo reporting an artifact outside the conventional target path,
# including spaces. This catches accidentally converting a previous local ELF.
export FIXTURE_ELF="$TEST_ROOT/custom output/ezal-firmware"
export CONVERTED_ELF="$TEST_ROOT/converted"
mkdir -p "$(dirname "$FIXTURE_ELF")"
touch "$FIXTURE_ELF"
cat > "$TEST_ROOT/bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ " $* " == *' --locked '* ]]
[[ " $* " == *' --message-format=json-render-diagnostics '* ]]
if [[ "${NO_ARTIFACT:-}" != 1 ]]; then
    jq -n --arg executable "$FIXTURE_ELF" '{
        reason: "compiler-artifact", target: {name: "ezal-firmware", kind: ["bin"]},
        executable: $executable
    }'
fi
EOF
cat > "$TEST_ROOT/bin/picotool" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == uf2 && "$2" == convert ]]
[[ "$3" == "$FIXTURE_ELF" && "$6" == "$FIXTURE_ELF.uf2" ]]
printf '%s' "$3" > "$CONVERTED_ELF"
EOF
chmod +x "$TEST_ROOT/bin/cargo" "$TEST_ROOT/bin/picotool"
PATH="$TEST_ROOT/bin:$PATH" "$REPO_ROOT/scripts/build-uf2.sh" \
    --target-dir "$TEST_ROOT/custom output" > /dev/null
[[ "$(cat "$CONVERTED_ELF")" == "$FIXTURE_ELF" ]]
rm "$CONVERTED_ELF"
if NO_ARTIFACT=1 PATH="$TEST_ROOT/bin:$PATH" "$REPO_ROOT/scripts/build-uf2.sh" \
    > /dev/null 2> "$TEST_ROOT/error"; then
    echo 'FAIL: packaged without a Cargo executable artifact' >&2
    exit 1
fi
[[ ! -e "$CONVERTED_ELF" ]]

echo 'Build-tool regressions passed (credential resolution and UF2 artifact selection).'
