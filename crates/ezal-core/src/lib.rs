//! # ezal-core
//!
//! Hardware-independent logic for the **ezal** antenna-tracker project.
//!
//! This crate is `no_std` by default so it can be compiled into the
//! [`ezal-firmware`](../ezal_firmware/index.html) binary that runs on the
//! Raspberry Pi Pico 2 W, but it also builds for the host. Run
//! `./scripts/test-host.sh` to select the host target; the workspace's default
//! target is embedded. `cfg(test)` enables `std` only for the test harness.
//!
//! ## Modules
//!
//! * [`ads1015`] — the pure register model for the ADS1015 position-feedback
//!   ADC: Config-register packing, conversion-result decoding, and the
//!   power-on self-test predicates. The bus transactions that use it live in
//!   the firmware crate.
//! * [`position`] — azimuth/elevation units, hard-coded installation
//!   calibration, and calibrated angle ↔ feedback-millivolt conversion.
//! * [`control`] — timestamp watchdogs, bounds checks, and the hysteretic
//!   two-axis fail-safe controller.
//! * [`simulation`] — the repeating METOP-C pass profile and its
//!   acquire → track → pause sequencer.
//! * [`supervisor`] — shared ADC validation, read-timeout latching, autonomous
//!   sequencing, and telemetry assembly for autonomous and manual modes.
//! * [`interlock`] — feedback freshness and travel-aware directional endpoint
//!   checks at the final actuator boundary.
//! * [`actuator`] — the safety-ordered sequence that enforces the interlock
//!   before relay timing and admits every command through it.
//! * [`motion`] — latched motion-progress and plausibility supervision using
//!   applied outputs, feedback resolution, and accumulated energised time.
//! * [`authority`] — exclusive browser or autonomous command ownership;
//!   browser movement becomes operator-held jogs.
//! * [`mailbox`] — a pending command slot where safety inhibition takes
//!   precedence over movement refreshes.
//! * [`wifi`] — validation of the station-mode WiFi credentials that
//!   `build.rs` bakes in from `.env`. Pure predicates the firmware's WiFi
//!   POST checks after radio initialization and before association.
//! * [`drive`] — the rotator's transport-independent motion vocabulary
//!   (azimuth/elevation directions, the combined [`drive::DriveCommand`], the
//!   [`drive::Command`] surface), relay timing, and [`drive::ActuatorGuard`].
//!   Ordinary controller stops obey relay timing; safety inhibition, operator
//!   release, and lease expiry immediately clear applicable outputs. The GPIO
//!   adapter lives in firmware.
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

pub mod actuator;
pub mod ads1015;
pub mod authority;
pub mod control;
pub mod dashboard;
pub mod drive;
pub mod interlock;
pub mod mailbox;
pub mod motion;
pub mod network;
pub mod position;
pub mod protocol;
pub mod simulation;
pub mod supervisor;
pub mod wifi;
