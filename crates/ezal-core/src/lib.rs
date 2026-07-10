//! # ezal-core
//!
//! Hardware-independent logic for the **ezal** antenna-tracker project.
//!
//! This crate is `no_std` by default so it can be compiled into the
//! [`ezal-firmware`](../ezal_firmware/index.html) binary that runs on the
//! Raspberry Pi Pico 2, but it also builds cleanly for the host so its
//! contents can be unit-tested with a plain `cargo test`. The only escape
//! hatch is `cfg(test)`, which enables `std` *only* when the test harness
//! is being built.
//!
//! ## Modules
//!
//! * [`ads1015`] — the pure register model for the ADS1015 position-feedback
//!   ADC: Config-register packing, conversion-result decoding, and the
//!   power-on self-test predicates. The bus transactions that use it live in
//!   the firmware crate.
//! * [`wifi`] — validation of the station-mode WiFi credentials that
//!   `build.rs` bakes in from `.env`. Pure predicates the firmware's WiFi
//!   POST checks before it powers the radio.
//! * [`drive`] — the rotator's transport-independent motion vocabulary
//!   (azimuth/elevation directions, the combined [`drive::DriveCommand`], the
//!   [`drive::Command`] surface) and [`drive::Debouncer`], the pure state
//!   machine that keeps the direction outputs from chattering. The GPIO layer
//!   that applies it lives in the firmware crate's `drive` module.
//! * [`protocol`] — the dashboard's WebSocket wire protocol: parsing browser
//!   messages ([`protocol::ClientMessage`]) and framing telemetry and control
//!   state ([`protocol::PositionTelemetry`], [`protocol::ControlStatus`]) as
//!   compact JSON.
//! * [`dashboard`] — the single static HTML/CSS/JS page served at `/`.
//!
//! ## What goes in this crate
//!
//! Anything that is:
//!   * pure (no global state, no I/O),
//!   * deterministic (same input → same output),
//!   * `no_std` (no `Box`, `Vec`, `HashMap`, etc., unless `heapless`).
//!
//! ## What does **not** go in this crate
//!
//! Anything that:
//!   * touches a peripheral (GPIO, SPI, UART, USB, …),
//!   * needs `embassy-*`, `embedded-hal`, or any chip-specific crate,
//!   * blocks on real time (use `embassy_time::Timer` in the firmware).
//!
//! Pushing those concerns into `ezal-firmware` keeps this crate trivially
//! testable on the host — exactly the property we want as the satellite
//! pointing maths get more elaborate.

// `no_std` for embedded use, but allow `std` when the test harness builds
// us. `cargo test` defines `cfg(test)` on this crate (but not on its
// dependents), so the firmware always sees the `no_std` version.
#![cfg_attr(not(test), no_std)]
// Hard-line safety lints: this crate is pure logic and has no business
// reaching for unsafe. If we ever need it, we'll add a justified `#[allow]`.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::all)]

pub mod ads1015;
pub mod dashboard;
pub mod drive;
pub mod protocol;
pub mod wifi;
