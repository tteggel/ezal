//! Deterministic, hardware-independent policy for ezal.
//!
//! This crate is `no_std`; policy receives values and explicit monotonic times.
//! Firmware owns peripherals, synchronization and asynchronous I/O. Run
//! `./scripts/test-host.sh` to test the same policy on the host.
//!
//! The control path has four owners:
//! * [`feedback`] validates acquisition results using [`position::InstalledCalibration`].
//! * [`session`] admits fresh, ordered operator intent.
//! * [`coordination`] owns manual authority and coherent command delivery.
//! * [`actuator::Actuator::step`] is the complete physical-output decision.
//!
//! [`interlock`], [`motion`], [`network`] and private [`drive`] timing helpers
//! support that boundary. [`mailbox::CommandEnvelope`] preserves a source's
//! absolute deadline. [`protocol`] supplies wire parsing and telemetry encoding;
//! [`dashboard`] supplies the page and firmware-owned client configuration.
//!
//! [`ads1015`] models ADC registers and decoding; [`wifi`] contains credential
//! validation and reconnect timing. Advisory prediction is separate: [`ntp`]
//! supplies an expiring UTC anchor, [`http`] frames streamed downloads, and
//! [`orbit`]/[`passes`] validate elements and calculate reception windows.
//! [`tracking`] supplies bounded planning, a device-owned lifecycle, following
//! and offline simulation. [`operator`] supplies its exclusive controller lease.
//! This tracking policy is not yet connected to firmware output requests.

#![cfg_attr(not(test), no_std)]
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::all)]

// Unit and integration tests share the same calibrated observation fixtures.
#[cfg(test)]
extern crate self as ezal_core;

pub mod actuator;
pub mod ads1015;
mod authority;
pub mod coordination;
pub mod dashboard;
pub mod drive;
pub mod duty;
pub mod feedback;
pub mod http;
pub mod interlock;
pub mod mailbox;
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
