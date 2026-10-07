//! Build script for `ezal-firmware`.
//!
//! Three responsibilities:
//!
//!  1. Stage `memory.x` into Cargo's `OUT_DIR` so the linker can find it.
//!     The reason we don't keep `memory.x` next to the elf is that the
//!     linker searches its `-L` paths for `INCLUDE`d scripts, and Cargo's
//!     `OUT_DIR` is the conventional place to drop generated artefacts.
//!
//!  2. Bake WiFi credentials and station configuration from `.env` into the build. The Pico has no
//!     filesystem, so there is nowhere to read an SSID/password from at
//!     runtime — they have to be compile-time constants. We read them here
//!     and re-export them as `EZAL_WIFI_SSID` / `EZAL_WIFI_PASSWORD` so
//!     `src/wifi.rs` can pick them up with `env!(...)`. See `.env.example`.
//!     Station settings become `OUT_DIR/prediction_config.rs`; unset ones
//!     name `ezal_core`'s defaults, and the firmware checks their ranges at
//!     compile time with `Config::validate`, so neither is duplicated here.
//!
//!     Precedence for each value: a real process environment variable wins
//!     (so CI or a one-off command can inject it), otherwise the `.env`
//!     file at the workspace root, otherwise empty. Empty is deliberately
//!     *not* a hard error: the firmware must still cross-compile in CI, which
//!     has no `.env` — instead the on-chip WiFi POST validates the baked-in
//!     credentials and fails loudly at boot if they're unset. We do emit a
//!     build warning so it's not silent.
//!
//!     Note: this build-time provisioning scheme stores credentials in the
//!     flashed image in the clear. Keep `.env` out of git (it is `.gitignore`d)
//!     and treat built artefacts and Cargo's build-script output as secrets.
//!     Cargo prints a failed build script's stdout, so every check that can
//!     fail runs before the credentials are printed, as the very last step.
//!
//!  3. Tell Cargo when to re-run this script. Without these hints Cargo
//!     would re-run `build.rs` on every build (slow) or never (wrong);
//!     the `rerun-if-changed` / `rerun-if-env-changed` lines hit the right
//!     balance.
//!
//! The link chain — for the curious — is:
//!
//!   rustc → linker (`rust-lld`)
//!         → `link.x`            (from `cortex-m-rt`, see .cargo/config)
//!             → `INCLUDE memory.x`
//!         → `defmt.x`           (from `defmt`, also in .cargo/config)
//!
//! `memory.x` defines `MEMORY` (FLASH and RAM origin/length) and
//! `_stack_start`; `cortex-m-rt`'s `link.x` uses those symbols to lay out
//! sections.

use std::env;
use std::fs;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Names of the credential variables we surface to the crate via `env!`.
const WIFI_VARS: [&str; 2] = ["EZAL_WIFI_SSID", "EZAL_WIFI_PASSWORD"];
const PREDICTION_VARS: [&str; 6] = [
    "EZAL_LATITUDE_DEG",
    "EZAL_LONGITUDE_DEG",
    "EZAL_ALTITUDE_M",
    "EZAL_MIN_ELEVATION_DEG",
    "EZAL_MIN_PEAK_ELEVATION_DEG",
    "EZAL_INCLUDE_EXPERIMENTAL",
];

fn main() {
    stage_memory_x();
    bake_configuration();
}

/// Responsibility 1: put `memory.x` where the linker will find it.
fn stage_memory_x() {
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR not set by cargo"));

    File::create(out.join("memory.x"))
        .expect("failed to create OUT_DIR/memory.x")
        .write_all(include_bytes!("memory.x"))
        .expect("failed to write OUT_DIR/memory.x");

    // Add OUT_DIR to the linker search path so `INCLUDE memory.x` resolves.
    println!("cargo:rustc-link-search={}", out.display());
}

/// Responsibilities 2 and 3: resolve the configuration, generate the station
/// settings, and hand the WiFi credentials to rustc as compile-time
/// environment variables.
fn bake_configuration() {
    // The `.env` lives at the workspace root, two levels up from this crate's
    // manifest directory (crates/ezal-firmware/ → ../../).
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"));
    let env_path = manifest_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or(&manifest_dir)
        .join(".env");

    // Re-run only when these change. Without the explicit list, *any*
    // change in the crate triggers a rerun. That includes the `.env` file
    // appearing or changing, and any variable passed in the process
    // environment directly (CI, a one-off `EZAL_WIFI_SSID=... cargo build`).
    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", env_path.display());
    for var in WIFI_VARS.into_iter().chain(PREDICTION_VARS) {
        println!("cargo:rerun-if-env-changed={var}");
    }

    let dotenv = parse_dotenv(&env_path);
    let lookup = |name: &str| -> String {
        // Presence, not non-emptiness, determines precedence. An explicit
        // empty password selects an open network; an empty SSID lets an
        // artifact build opt out of embedding a developer's local credentials.
        match env::var(name) {
            Ok(v) => v,
            Err(env::VarError::NotPresent) => dotenv
                .iter()
                .rev()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .unwrap_or_default(),
            Err(env::VarError::NotUnicode(_)) => panic!("{name} must be valid UTF-8"),
        }
    };

    let ssid = lookup("EZAL_WIFI_SSID");
    let password = lookup("EZAL_WIFI_PASSWORD");

    // Cargo reads stdout one line at a time. Reject line breaks before printing
    // either value, or a credential could be truncated or interpreted as a new
    // build instruction. Diagnostics name the field without exposing its value.
    for (name, value) in [(WIFI_VARS[0], &ssid), (WIFI_VARS[1], &password)] {
        assert!(
            !value.contains(['\r', '\n']),
            "{name} must not contain line breaks"
        );
    }
    write_prediction_config(lookup);

    // An empty SSID means the radio can't join anything; warn (don't fail —
    // CI needs the cross-compile to succeed without a `.env`). The runtime
    // POST is the real gate. An empty *password* is legitimate: it selects an
    // open network, so it never warns.
    if ssid.is_empty() {
        println!(
            "cargo:warning=EZAL_WIFI_SSID is empty after environment/.env resolution ({}); \
             the WiFi POST will fail at boot until you copy .env.example to .env and fill it in.",
            env_path.display()
        );
    }

    // These reach `src/wifi.rs` as `env!("EZAL_WIFI_SSID")` etc. We always
    // set them (possibly to "") so `env!` never fails the compile. Nothing
    // may follow that can fail, or Cargo would echo the credentials.
    println!("cargo:rustc-env=EZAL_WIFI_SSID={ssid}");
    println!("cargo:rustc-env=EZAL_WIFI_PASSWORD={password}");
}

/// Generate the station configuration as Rust source. A set value must be a
/// finite decimal number (or `true`/`false`); an unset one refers to
/// `ezal_core`'s default by name. Ranges are the firmware's compile-time
/// `Config::validate` check, so a value can only be rejected in one place.
fn write_prediction_config(lookup: impl Fn(&str) -> String) {
    let number = |name: &str, default: &str| -> String {
        let value = lookup(name);
        if value.is_empty() {
            return default.to_string();
        }
        let parsed = value
            .parse::<f64>()
            .ok()
            .filter(|parsed| parsed.is_finite())
            .unwrap_or_else(|| panic!("{name} must be a finite decimal number"));
        format!("{parsed:?}")
    };
    let latitude = number(PREDICTION_VARS[0], "DEFAULT.observer.latitude_deg");
    let longitude = number(PREDICTION_VARS[1], "DEFAULT.observer.longitude_deg");
    let altitude = number(PREDICTION_VARS[2], "DEFAULT.observer.altitude_m");
    let mask = number(PREDICTION_VARS[3], "DEFAULT.min_elevation_deg");
    let peak = number(
        PREDICTION_VARS[4],
        "ezal_core::passes::Config::default_min_peak(min_elevation_deg)",
    );
    let experimental = match lookup(PREDICTION_VARS[5]).as_str() {
        "" => "DEFAULT.include_experimental",
        "false" => "false",
        "true" => "true",
        _ => panic!("EZAL_INCLUDE_EXPERIMENTAL must be true or false"),
    };
    let config = format!(
        "// Generated by build.rs from the process environment or `.env`.\n\
         const CONFIG: ezal_core::passes::Config = {{\n\
         \x20   #[allow(dead_code)] // unused when every setting is provided\n\
         \x20   const DEFAULT: ezal_core::passes::Config = ezal_core::passes::Config::DEFAULT;\n\
         \x20   let min_elevation_deg = {mask};\n\
         \x20   ezal_core::passes::Config {{\n\
         \x20       observer: ezal_core::orbit::Observer {{\n\
         \x20           latitude_deg: {latitude},\n\
         \x20           longitude_deg: {longitude},\n\
         \x20           altitude_m: {altitude},\n\
         \x20       }},\n\
         \x20       min_elevation_deg,\n\
         \x20       min_peak_elevation_deg: {peak},\n\
         \x20       include_experimental: {experimental},\n\
         \x20   }}\n\
         }};\n"
    );
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR not set"));
    fs::write(out.join("prediction_config.rs"), config).expect("write prediction configuration");
}

/// A deliberately small `.env` parser: `KEY=VALUE` per line, `#` comments and
/// blank lines skipped, and an optional leading `export`. A value may be
/// wrapped in single or double quotes, whose interior is kept verbatim (the
/// first matching quote closes it; only a comment may follow). An unquoted
/// value is trimmed and ends at a `#` that follows whitespace, so
/// `KEY=12 # measured` is `12` while `KEY=pa#ss` keeps its `#`. As in other
/// dotenv tools, the last line for a key wins. This is all the `.env` format
/// ezal needs — it is not a general dotenv implementation. Returns `[]` if the
/// file is absent (the not-yet-configured case), so callers fall back to
/// defaults.
fn parse_dotenv(path: &Path) -> Vec<(String, String)> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => panic!("failed to read {}: {error}", path.display()),
    };

    let mut out = Vec::new();
    for line in contents.lines() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_string();
        if !WIFI_VARS.contains(&key.as_str()) && !PREDICTION_VARS.contains(&key.as_str()) {
            continue;
        }
        let value = parse_value(&key, value);
        out.push((key, value));
    }
    out
}

/// Interpret the text after `=` (see [`parse_dotenv`]).
fn parse_value(key: &str, raw: &str) -> String {
    let value = raw.trim_start();
    if let Some(quote) = value.chars().next().filter(|c| matches!(c, '"' | '\'')) {
        // This deliberately single-line format must reject multiline quoted
        // values, rather than baking only their first line.
        let inner = &value[1..];
        let Some(end) = inner.find(quote) else {
            panic!("unterminated quoted value for {key}");
        };
        let rest = inner[end + 1..].trim_start();
        assert!(
            rest.is_empty() || rest.starts_with('#'),
            "unexpected text after the quoted value for {key}"
        );
        return inner[..end].to_string();
    }
    let comment = raw
        .char_indices()
        .find(|&(index, c)| c == '#' && raw[..index].ends_with(char::is_whitespace))
        .map_or(raw.len(), |(index, _)| index);
    raw[..comment].trim().to_string()
}
