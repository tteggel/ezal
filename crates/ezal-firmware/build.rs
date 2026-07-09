//! Build script for `ezal-firmware`.
//!
//! Three responsibilities:
//!
//!  1. Stage `memory.x` into Cargo's `OUT_DIR` so the linker can find it.
//!     The reason we don't keep `memory.x` next to the elf is that the
//!     linker searches its `-L` paths for `INCLUDE`d scripts, and Cargo's
//!     `OUT_DIR` is the conventional place to drop generated artefacts.
//!
//!  2. Bake the WiFi credentials from `.env` into the build. The Pico has no
//!     filesystem, so there is nowhere to read an SSID/password from at
//!     runtime — they have to be compile-time constants. We read them here
//!     and re-export them as `EZAL_WIFI_SSID` / `EZAL_WIFI_PASSWORD` so
//!     `src/wifi.rs` can pick them up with `env!(...)`. See `.env.example`.
//!
//!     Precedence for each value: a real process environment variable wins
//!     (so CI or `direnv`'s `dotenv` can inject them), otherwise the `.env`
//!     file at the workspace root, otherwise empty. Empty is deliberately
//!     *not* a hard error: the firmware must still cross-compile in CI, which
//!     has no `.env` — instead the on-chip WiFi POST validates the baked-in
//!     credentials and fails loudly at boot if they're unset. We do emit a
//!     build warning so it's not silent.
//!
//!     Note: the credentials end up in the flashed image in the clear. That's
//!     inherent to a device with no secure element; keep `.env` out of git
//!     (it is `.gitignore`d) and treat built artefacts as secrets.
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

fn main() {
    stage_memory_x();
    bake_wifi_credentials();

    // Re-run only when these change. Without the explicit list, *any*
    // change in the crate triggers a rerun.
    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rerun-if-changed=build.rs");
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

/// Responsibility 2: resolve the WiFi credentials and hand them to rustc as
/// compile-time environment variables.
fn bake_wifi_credentials() {
    // The `.env` lives at the workspace root, two levels up from this crate's
    // manifest directory (crates/ezal-firmware/ → ../../).
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"));
    let env_path = manifest_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or(&manifest_dir)
        .join(".env");

    // Re-run if the file appears / changes, and if either var is passed in
    // the process environment directly (CI, direnv's `dotenv`, a one-off
    // `EZAL_WIFI_SSID=... cargo build`).
    println!("cargo:rerun-if-changed={}", env_path.display());
    for var in WIFI_VARS {
        println!("cargo:rerun-if-env-changed={var}");
    }

    let dotenv = parse_dotenv(&env_path);
    let lookup = |name: &str| -> String {
        // A process env var wins, but only if non-empty — an empty inherited
        // value shouldn't mask a real one in `.env`.
        match env::var(name) {
            Ok(v) if !v.is_empty() => v,
            _ => dotenv
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .unwrap_or_default(),
        }
    };

    let ssid = lookup("EZAL_WIFI_SSID");
    let password = lookup("EZAL_WIFI_PASSWORD");

    // An empty SSID means the radio can't join anything; warn (don't fail —
    // CI needs the cross-compile to succeed without a `.env`). The runtime
    // POST is the real gate. An empty *password* is legitimate: it selects an
    // open network, so it never warns.
    if ssid.is_empty() {
        println!(
            "cargo:warning=EZAL_WIFI_SSID is unset (no .env at {} and not in the environment); \
             the WiFi POST will fail at boot until you copy .env.example to .env and fill it in.",
            env_path.display()
        );
    }

    // These reach `src/wifi.rs` as `env!(\"EZAL_WIFI_SSID\")` etc. We always
    // set them (possibly to \"\") so `env!` never fails the compile.
    println!("cargo:rustc-env=EZAL_WIFI_SSID={ssid}");
    println!("cargo:rustc-env=EZAL_WIFI_PASSWORD={password}");
}

/// A deliberately small `.env` parser: `KEY=VALUE` per line, `#` comments and
/// blank lines skipped, an optional leading `export`, and optional matching
/// single or double quotes around the value (whose interior is kept verbatim;
/// unquoted values are trimmed). This is all the `.env` format ezal needs —
/// it is not a general dotenv implementation. Returns `[]` if the file is
/// absent (the not-yet-configured case), so callers fall back to defaults.
fn parse_dotenv(path: &Path) -> Vec<(String, String)> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
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
        let value = value.trim();
        let value = strip_matching_quotes(value).unwrap_or_else(|| value.trim().to_string());
        out.push((key, value));
    }
    out
}

/// If `value` is wrapped in a matching pair of single or double quotes, return
/// its unquoted interior verbatim (no trimming inside); otherwise `None`.
fn strip_matching_quotes(value: &str) -> Option<String> {
    for q in ['"', '\''] {
        if value.len() >= 2 && value.starts_with(q) && value.ends_with(q) {
            return Some(value[1..value.len() - 1].to_string());
        }
    }
    None
}
