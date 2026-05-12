//! Build script for `ezal-firmware`.
//!
//! Two responsibilities:
//!
//!  1. Stage `memory.x` into Cargo's `OUT_DIR` so the linker can find it.
//!     The reason we don't keep `memory.x` next to the elf is that the
//!     linker searches its `-L` paths for `INCLUDE`d scripts, and Cargo's
//!     `OUT_DIR` is the conventional place to drop generated artefacts.
//!
//!  2. Tell Cargo when to re-run this script. Without these hints Cargo
//!     would re-run `build.rs` on every build (slow) or never (wrong);
//!     the `rerun-if-changed` lines hit the right balance.
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
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR not set by cargo"));

    File::create(out.join("memory.x"))
        .expect("failed to create OUT_DIR/memory.x")
        .write_all(include_bytes!("memory.x"))
        .expect("failed to write OUT_DIR/memory.x");

    // Add OUT_DIR to the linker search path so `INCLUDE memory.x` resolves.
    println!("cargo:rustc-link-search={}", out.display());

    // Re-run only when these change. Without the explicit list, *any*
    // change in the crate triggers a rerun.
    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rerun-if-changed=build.rs");
}
