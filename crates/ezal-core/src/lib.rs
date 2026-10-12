//! Deterministic, hardware-independent policy for ezal.
//!
//! This crate is `no_std`; policy receives values and explicit monotonic times.
//! Firmware owns peripherals, synchronization and asynchronous I/O. Run
//! `./scripts/test-host.sh` to test the same policy on the host.
//!
//! The control path has four owners:
//! * [`feedback`] validates acquisition results using [`position::InstalledCalibration`].
//! * [`session`] admits fresh, ordered operator intent.
//! * [`coordination`] serializes inputs and delivers commands coherently; its
//!   [`tracking::TrackingLifecycle`] owns the Manual/Tracking mode, operator lease and mailbox.
//! * [`actuator::Actuator::step`] is the complete physical-output decision.
//!
//! [`interlock`], [`motion`], [`network`] and private [`drive`] timing helpers
//! support that boundary. [`drive::CommandEnvelope`] preserves a source's
//! absolute deadline. [`protocol`] supplies wire parsing and telemetry encoding;
//! [`dashboard`] supplies the page and firmware-owned client configuration.
//!
//! [`ads1015`] models ADC registers and decoding; [`wifi`] contains credential
//! validation and reconnect timing. Advisory prediction is separate: [`ntp`]
//! supplies a revisioned UTC anchor and error estimate, [`http`] frames streamed downloads, and
//! [`orbit`]/[`passes`] validate elements and calculate reception windows.
//! [`tracking`] supplies bounded planning, a device-owned lifecycle, following
//! and offline simulation. Its execution-input boundary owns orbital/clock
//! snapshots and enforces source/timing/network validity. [`operator`] supplies
//! its exclusive controller lease.
//! Firmware services tracking independently through that same output boundary.

#![cfg_attr(not(test), no_std)]
#![deny(unsafe_code)]
#![warn(missing_docs)]

// Unit and integration tests share the same calibrated observation fixtures.
#[cfg(test)]
extern crate self as ezal_core;
#[cfg(test)]
#[path = "../tests/common/mod.rs"]
mod test_support;

pub mod actuator;
pub mod ads1015;
pub mod coordination;
pub mod dashboard;
pub mod drive;
pub mod duty;
pub mod feedback;
pub mod http;
pub mod interlock;
pub mod motion;
pub mod network;
pub mod ntp;
pub mod operator;
pub mod orbit;
pub mod passes;
pub mod position;
pub mod protocol;
pub mod session;
pub mod tracking;
pub mod wifi;
