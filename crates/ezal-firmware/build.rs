//! Build script for `ezal-firmware`.
//!
//! Three responsibilities:
//!
//!  1. Put this crate's directory on the linker search path, where the
//!     `INCLUDE memory.x` in cortex-m-rt's `link.x` finds `memory.x`.
//!
//!  2. Bake WiFi credentials, the N2YO API key and station settings from
//!     `.env` into the build (`docs/DEVELOPMENT.md` describes provisioning).
//!     The credentials become `EZAL_WIFI_SSID` / `EZAL_WIFI_PASSWORD` for
//!     `env!` in `src/main.rs`, and the key `EZAL_N2YO_KEY` for
//!     `src/prediction.rs`, which checks its characters at compile time.
//!     Station settings become `OUT_DIR/prediction_config.rs`; unset ones
//!     name `ezal_core`'s defaults, and the firmware checks their ranges at
//!     compile time with `Config::validate`, so neither is duplicated here.
//!
//!     For each value a process environment variable wins, then the `.env`
//!     file at the workspace root, then empty. An empty SSID or key only
//!     warns: CI cross-compiles without a `.env`, network supervision parks
//!     offline at boot, and N2YO refuses a missing key. Cargo prints a
//!     failed build script's stdout, so every check that can fail runs
//!     before the credentials are printed, as the very last step.
//!
//!  3. Declare its inputs with `rerun-if-changed` / `rerun-if-env-changed`, so
//!     Cargo reruns it only when one of them changes.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Names of the credential variables we surface to the crate via `env!`.
const WIFI_VARS: [&str; 2] = ["EZAL_WIFI_SSID", "EZAL_WIFI_PASSWORD"];
/// The N2YO API key, surfaced the same way.
const N2YO_KEY: &str = "EZAL_N2YO_KEY";

// Names of the station settings generated into `prediction_config.rs`.
const LATITUDE: &str = "EZAL_LATITUDE_DEG";
const LONGITUDE: &str = "EZAL_LONGITUDE_DEG";
const ALTITUDE: &str = "EZAL_ALTITUDE_M";
const MIN_ELEVATION: &str = "EZAL_MIN_ELEVATION_DEG";
const MIN_PEAK_ELEVATION: &str = "EZAL_MIN_PEAK_ELEVATION_DEG";
const INCLUDE_EXPERIMENTAL: &str = "EZAL_INCLUDE_EXPERIMENTAL";
const PREDICTION_VARS: [&str; 6] = [
    LATITUDE,
    LONGITUDE,
    ALTITUDE,
    MIN_ELEVATION,
    MIN_PEAK_ELEVATION,
    INCLUDE_EXPERIMENTAL,
];

fn main() {
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"));
    // Responsibility 1: `link.x` resolves `INCLUDE memory.x` against this path.
    println!("cargo:rustc-link-search={}", manifest_dir.display());
    if env::var_os("CARGO_FEATURE_BROWSER_PLANNER").is_some() {
        build_planner(&manifest_dir);
    }
    bake_configuration(&manifest_dir);
}

/// Responsibilities 2 and 3: resolve the configuration, generate the station
/// settings, and hand the WiFi credentials to rustc as compile-time
/// environment variables.
fn bake_configuration(manifest_dir: &Path) {
    // The `.env` lives at the workspace root, two levels up from this crate's
    // manifest directory (crates/ezal-firmware/ → ../../).
    let env_path = manifest_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or(manifest_dir)
        .join(".env");

    // Re-run only when these change. Without the explicit list, *any*
    // change in the crate triggers a rerun. That includes the `.env` file
    // appearing or changing, and any variable passed in the process
    // environment directly (CI, a one-off `EZAL_WIFI_SSID=... cargo build`).
    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", env_path.display());
    for var in WIFI_VARS
        .into_iter()
        .chain([N2YO_KEY])
        .chain(PREDICTION_VARS)
    {
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
    let key = lookup(N2YO_KEY);

    // Cargo reads stdout one line at a time. Reject line breaks before printing
    // any value, or a credential could be truncated or interpreted as a new
    // build instruction. Diagnostics name the field without exposing its value.
    for (name, value) in [
        (WIFI_VARS[0], &ssid),
        (WIFI_VARS[1], &password),
        (N2YO_KEY, &key),
    ] {
        assert!(
            !value.contains(['\r', '\n']),
            "{name} must not contain line breaks"
        );
    }
    write_prediction_config(lookup);

    // An empty SSID means the radio can't join anything; warn (don't fail —
    // CI needs the cross-compile to succeed without a `.env`). The runtime
    // supervisor validates them before association. An empty password selects
    // an open network, so it never warns.
    if ssid.is_empty() {
        println!(
            "cargo:warning=EZAL_WIFI_SSID is empty after environment/.env resolution ({}); \
             networking will stay offline until you copy .env.example to .env and fill it in.",
            env_path.display()
        );
    }
    if key.is_empty() {
        println!(
            "cargo:warning=EZAL_N2YO_KEY is empty after environment/.env resolution ({}); \
             orbital-element downloads will fail until you add an N2YO API key.",
            env_path.display()
        );
    }

    // These reach `src/main.rs` as `env!("EZAL_WIFI_SSID")` etc. We always
    // set them (possibly to "") so `env!` never fails the compile. Nothing
    // may follow that can fail, or Cargo would echo the credentials.
    println!("cargo:rustc-env=EZAL_WIFI_SSID={ssid}");
    println!("cargo:rustc-env=EZAL_WIFI_PASSWORD={password}");
    println!("cargo:rustc-env=EZAL_N2YO_KEY={key}");
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
    let latitude = number(LATITUDE, "DEFAULT.observer.latitude_deg");
    let longitude = number(LONGITUDE, "DEFAULT.observer.longitude_deg");
    let altitude = number(ALTITUDE, "DEFAULT.observer.altitude_m");
    let mask = number(MIN_ELEVATION, "DEFAULT.min_elevation_deg");
    let peak = number(
        MIN_PEAK_ELEVATION,
        "ezal_core::passes::Config::default_min_peak(min_elevation_deg)",
    );
    let experimental = match lookup(INCLUDE_EXPERIMENTAL).as_str() {
        "" => "DEFAULT.include_experimental",
        "false" => "false",
        "true" => "true",
        _ => panic!("{INCLUDE_EXPERIMENTAL} must be true or false"),
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
        if !WIFI_VARS.contains(&key.as_str())
            && key != N2YO_KEY
            && !PREDICTION_VARS.contains(&key.as_str())
        {
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

/// Build the browser module from this checkout, in a distinct target directory
/// so nested Cargo never contends with the firmware's build-directory lock.
fn build_planner(manifest_dir: &Path) {
    let root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("workspace");
    for input in [
        "Cargo.toml",
        "Cargo.lock",
        ".cargo/config.toml",
        "rust-toolchain.toml",
        "crates/ezal-core/src",
        "crates/ezal-core/Cargo.toml",
        "crates/ezal-wasm",
    ] {
        println!("cargo:rerun-if-changed={}", root.join(input).display());
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let target = out.join("planner-build");
    let status = std::process::Command::new(env::var_os("CARGO").expect("CARGO"))
        .current_dir(root)
        .args([
            "build",
            "--locked",
            "--release",
            "-p",
            "ezal-wasm",
            "--target",
            "wasm32-unknown-unknown",
            "--target-dir",
        ])
        .arg(&target)
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        // A firmware clippy invocation must still produce an ordinary WASM
        // artifact, without inheriting its workspace lint-driver wrapper.
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("CLIPPY_ARGS")
        .status()
        .expect("build browser planner");
    assert!(
        status.success(),
        "browser planner build failed; install the pinned wasm32-unknown-unknown Rust target"
    );
    fs::copy(
        target.join("wasm32-unknown-unknown/release/ezal_wasm.wasm"),
        out.join("planner.wasm"),
    )
    .expect("embed planner WASM");
}
