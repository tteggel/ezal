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
unset EZAL_LATITUDE_DEG EZAL_LONGITUDE_DEG EZAL_ALTITUDE_M
unset EZAL_MIN_ELEVATION_DEG EZAL_MIN_PEAK_ELEVATION_DEG EZAL_INCLUDE_EXPERIMENTAL
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
[[ "$output" == *$'\ncargo:rustc-env=EZAL_WIFI_PASSWORD=' ]] # credentials print last
[[ "$output" != *fixture-password* ]]
output=$(EZAL_WIFI_SSID= EZAL_WIFI_PASSWORD= "$TEST_ROOT/build-script")
[[ "$output" != *fixture-network* && "$output" != *fixture-password* ]]

# Station defaults are ezal-core's, by name, and the peak default follows the
# mask; .env provisioning and process overrides produce literals.
config=$(cat "$OUT_DIR/prediction_config.rs")
[[ "$config" == *'latitude_deg: DEFAULT.observer.latitude_deg'* ]]
[[ "$config" == *'min_peak_elevation_deg: ezal_core::passes::Config::default_min_peak(min_elevation_deg)'* ]]
cat >> "$TEST_ROOT/workspace/.env" <<'EOF'
EZAL_LATITUDE_DEG=-33.86
EZAL_LONGITUDE_DEG=151.2
EZAL_ALTITUDE_M=30
EZAL_MIN_ELEVATION_DEG=8
EZAL_MIN_PEAK_ELEVATION_DEG=20
EZAL_INCLUDE_EXPERIMENTAL=true
EOF
"$TEST_ROOT/build-script" > /dev/null
config=$(cat "$OUT_DIR/prediction_config.rs")
[[ "$config" == *'latitude_deg: -33.86'* && "$config" == *'let min_elevation_deg = 8.0;'* ]]
[[ "$config" == *'min_peak_elevation_deg: 20.0'* && "$config" == *'include_experimental: true'* ]]
EZAL_LATITUDE_DEG=40 "$TEST_ROOT/build-script" > /dev/null
[[ "$(cat "$OUT_DIR/prediction_config.rs")" == *'latitude_deg: 40.0'* ]]
# Ranges are checked once, by the firmware's compile-time Config::validate.
EZAL_LONGITUDE_DEG=181 EZAL_MIN_PEAK_ELEVATION_DEG=7 "$TEST_ROOT/build-script" > /dev/null
config=$(cat "$OUT_DIR/prediction_config.rs")
[[ "$config" == *'longitude_deg: 181.0'* && "$config" == *'min_peak_elevation_deg: 7.0'* ]]
grep -q 'match CONFIG.validate()' "$REPO_ROOT/crates/ezal-firmware/src/prediction.rs"

# Text that is not a finite number or boolean fails in the build script, and
# the failure must not make Cargo echo either credential from .env.
for invalid in EZAL_LATITUDE_DEG=NaN EZAL_ALTITUDE_M=inf EZAL_LONGITUDE_DEG=east \
    EZAL_INCLUDE_EXPERIMENTAL=yes EZAL_MIN_ELEVATION_DEG='5 degrees'; do
    if env -u EZAL_WIFI_SSID -u EZAL_WIFI_PASSWORD "$invalid" "$TEST_ROOT/build-script" \
        > "$TEST_ROOT/output" 2> "$TEST_ROOT/error"; then
        echo "FAIL: accepted invalid station setting $invalid" >&2
        exit 1
    fi
    output=$(cat "$TEST_ROOT/output" "$TEST_ROOT/error")
    if [[ "$output" == *fixture-network* || "$output" == *fixture-password* \
        || "$output" == *cargo:rustc-env=* ]]; then
        echo "FAIL: $invalid echoed a credential before failing" >&2
        exit 1
    fi
done

# The last line for a key wins, and an unquoted value ends at ' #'. Quoted
# values keep '#', and '#' without preceding whitespace is part of a value.
cat >> "$TEST_ROOT/workspace/.env" <<'EOF'
EZAL_LATITUDE_DEG=40.5
EZAL_ALTITUDE_M=12 # measured with GPS
EZAL_WIFI_PASSWORD="quoted # secret" # trailing comment
EZAL_WIFI_SSID=hash#inside
EOF
output=$(env -u EZAL_WIFI_SSID -u EZAL_WIFI_PASSWORD "$TEST_ROOT/build-script")
config=$(cat "$OUT_DIR/prediction_config.rs")
[[ "$config" == *'latitude_deg: 40.5'* && "$config" == *'altitude_m: 12.0'* ]]
[[ "$output" == *$'\ncargo:rustc-env=EZAL_WIFI_PASSWORD=quoted # secret' ]]
[[ "$output" == *$'cargo:rustc-env=EZAL_WIFI_SSID=hash#inside\n'* ]]
printf '%s\n' "EZAL_WIFI_SSID='closed' then text" >> "$TEST_ROOT/workspace/.env"
if env -u EZAL_WIFI_SSID -u EZAL_WIFI_PASSWORD "$TEST_ROOT/build-script" \
    > /dev/null 2> "$TEST_ROOT/error"; then
    echo 'FAIL: accepted text after a quoted value' >&2
    exit 1
fi
cat > "$TEST_ROOT/workspace/.env" <<'EOF'
EZAL_WIFI_SSID=fixture-network
EZAL_WIFI_PASSWORD=fixture-password
EOF

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

echo 'Build-tool regressions passed (credentials, station configuration, and UF2 artifact selection).'
