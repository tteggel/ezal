//! Expiring permission to move while the network supervisor reports readiness.
//!
//! A cached `connected` flag is insufficient: if the task maintaining it stops,
//! the actuator would keep treating its last successful observation as current.
//! This small gate gives every observation an absolute deadline. The GPIO owner
//! evaluates that deadline independently, including while a relay transition is
//! pending. Link/address recovery grants a new generation, so command sources
//! can discard work prepared before an interruption.

use crate::drive::Command;

/// Expected interval between link/address observations in the firmware.
pub const NETWORK_POLL_MS: u64 = 100;

/// Maximum age of a successful network observation, exclusive at the deadline.
/// This allows scheduler jitter across two missed polls without giving an
/// indefinitely blocked network task permission to keep driving.
pub const NETWORK_HEALTH_TIMEOUT_MS: u64 = 300;

/// Last positive link/address observation and its recovery generation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NetworkGate {
    observed_ms: Option<u64>,
    generation: u64,
}

impl NetworkGate {
    /// Start offline; boot must observe both link and usable addressing.
    pub const fn new() -> Self {
        Self {
            observed_ms: None,
            generation: 0,
        }
    }

    /// Whether the most recent positive observation is still valid.
    pub fn ready(&self, now_ms: u64) -> bool {
        self.observed_ms
            .and_then(|observed| now_ms.checked_sub(observed))
            .is_some_and(|age| age < NETWORK_HEALTH_TIMEOUT_MS)
    }

    /// Deadline at which a current positive observation expires.
    pub fn deadline_ms(&self) -> Option<u64> {
        self.observed_ms?.checked_add(NETWORK_HEALTH_TIMEOUT_MS)
    }

    /// Recovery generation; changes on the first ready observation after boot,
    /// a reported outage, or an expired observation. Steady refreshes retain it.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Discard expired readiness and report whether an active permit was lost.
    /// Callers use this transition to revoke ownership and queued commands.
    pub fn expire(&mut self, now_ms: u64) -> bool {
        if self.observed_ms.is_some() && !self.ready(now_ms) {
            self.observed_ms = None;
            true
        } else {
            false
        }
    }

    /// Publish link/address readiness. Returns whether an existing permission
    /// was lost, even if a delayed positive observation immediately recovers it.
    pub fn observe(&mut self, now_ms: u64, ready: bool) -> bool {
        let expired = self.expire(now_ms);
        if !ready {
            return self.observed_ms.take().is_some() || expired;
        }
        if self.observed_ms.is_none() {
            self.generation = self.generation.saturating_add(1);
        }
        // Treat an unrepresentable deadline as offline, never as an immortal
        // permit. Real hardware cannot approach u64 milliseconds of uptime.
        if now_ms.checked_add(NETWORK_HEALTH_TIMEOUT_MS).is_none() {
            return self.observed_ms.take().is_some() || expired;
        }
        self.observed_ms = Some(now_ms);
        expired
    }

    /// Gate every command at consumption, retaining immediate stop semantics.
    pub fn guard_command(&self, command: Command, now_ms: u64) -> Command {
        if self.ready(now_ms) {
            command
        } else {
            Command::Inhibit
        }
    }
}
