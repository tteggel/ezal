//! The safety-ordered actuator sequence shared by firmware and host tests.
//!
//! [`Actuator::step`] consumes one coherent observation and command snapshot.
//! Motion accounting and permissions precede relay timing, and a queued stop
//! is consumed before a pending activation. Firmware supplies time, applies
//! the resulting GPIO state, and wakes for the returned deadline.

use crate::drive::{ActuatorGuard, Command, DriveCommand};
use crate::interlock::FeedbackInterlock;
use crate::mailbox::CommandEnvelope;
use crate::motion::{MotionConfig, MotionFault, MotionMonitor};
use crate::network::NetworkGate;

/// Coherent inputs copied by the coordination owner for one output step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActuatorInput {
    /// Latest feedback and installed mechanical limits.
    pub interlock: FeedbackInterlock,
    /// Expiring operating permission and its recovery generation.
    pub network: NetworkGate,
    /// One pending command carrying its original absolute expiry.
    pub command: Option<CommandEnvelope>,
}

/// Persistent actuator health, separate from feedback and connection health.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActuatorStatus {
    /// First motion fault, latched until the actuator is recreated on reset.
    pub motion_fault: Option<MotionFault>,
}

/// Output and next required service instant after an actuator step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActuatorUpdate {
    /// Drive state the GPIO owner must apply before feeding its watchdog.
    pub output: DriveCommand,
    /// Current latched actuator health.
    pub status: ActuatorStatus,
    /// Earliest permission, feedback, motion, lease, or relay deadline.
    pub next_deadline_ms: Option<u64>,
}

/// Leased output state with feedback, network and motion supervision.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Actuator {
    guard: ActuatorGuard,
    motion: MotionMonitor,
    network_generation: u64,
}

impl Actuator {
    /// Create an idle actuator.
    pub const fn new() -> Self {
        Self::with_motion_config(MotionConfig::DEFAULT)
    }

    /// Create an idle actuator with installation-specific motion thresholds.
    /// Invalid thresholds latch an inhibition before any output can energise.
    pub const fn with_motion_config(config: MotionConfig) -> Self {
        Self {
            guard: ActuatorGuard::new(),
            motion: MotionMonitor::new(config),
            network_generation: 0,
        }
    }

    /// Service the complete output boundary from one coherent snapshot.
    ///
    /// Network permission can revoke movement at any instant, independently
    /// of the command lease. It never shortens the lifetime used to decide
    /// whether a relay activation can cover its minimum active time.
    pub fn step(&mut self, input: ActuatorInput, now_ms: u64) -> ActuatorUpdate {
        // Account for the output that existed throughout the elapsed interval
        // before a network, feedback, or incoming command cut changes it.
        self.enforce(&input.interlock, now_ms);
        let network_ready = input.network.ready(now_ms);
        if !network_ready || self.network_generation != input.network.generation() {
            self.guard.command(Command::Inhibit, now_ms);
        }
        self.network_generation = input.network.generation();

        if let Some(envelope) = input.command {
            let permitted = if !network_ready || self.motion.fault().is_some() {
                Command::Inhibit
            } else {
                input.interlock.guard_command(envelope.command, now_ms)
            };
            // command_until expires the old lease before a late refresh and
            // consumes a release before advancing its pending activation.
            self.guard
                .command_until(permitted, now_ms, envelope.valid_until_ms);
        } else {
            self.guard.update(now_ms);
        }
        self.motion.applied(self.guard.actual(), now_ms);

        let network_deadline = network_ready.then(|| input.network.deadline_ms()).flatten();
        ActuatorUpdate {
            output: self.guard.actual(),
            status: ActuatorStatus {
                motion_fault: self.motion.fault(),
            },
            next_deadline_ms: [
                self.next_deadline_ms(&input.interlock, now_ms),
                network_deadline,
            ]
            .into_iter()
            .flatten()
            .min(),
        }
    }

    fn enforce(&mut self, interlock: &FeedbackInterlock, now_ms: u64) {
        // Observe the output that actually existed throughout the elapsed
        // interval, before any freshness check or incoming stop changes it.
        self.motion.observe(interlock, now_ms);
        let actual = self.guard.actual();
        let target = self.guard.target();
        let permitted_actual = interlock.guard_drive(actual, now_ms);
        let permitted_target = interlock.guard_drive(target, now_ms);
        self.guard.inhibit_axes(
            actual.azimuth != permitted_actual.azimuth
                || target.azimuth != permitted_target.azimuth,
            actual.elevation != permitted_actual.elevation
                || target.elevation != permitted_target.elevation,
            now_ms,
        );
        if self.motion.fault().is_some() {
            self.guard.command(Command::Inhibit, now_ms);
        }
    }

    /// Physical deadlines, combined with operating permission by `step`.
    fn next_deadline_ms(&self, interlock: &FeedbackInterlock, now_ms: u64) -> Option<u64> {
        let transition = self.guard.next_transition_ms(now_ms);
        let feedback = interlock.deadline_ms(self.guard.actual(), now_ms);
        [transition, feedback, self.motion.deadline_ms(now_ms)]
            .into_iter()
            .flatten()
            .min()
    }

    /// Drive state currently permitted at the physical output boundary.
    pub const fn actual(&self) -> DriveCommand {
        self.guard.actual()
    }

    /// Drive state most recently requested and still permitted.
    pub const fn target(&self) -> DriveCommand {
        self.guard.target()
    }

    /// Current movement-lease deadline, if an active command owns one.
    pub const fn lease_deadline_ms(&self) -> Option<u64> {
        self.guard.lease_deadline_ms()
    }

    /// First motion fault, latched until this actuator is recreated on reset.
    /// Both target and actual outputs remain inhibited while this is present.
    pub const fn fault(&self) -> Option<MotionFault> {
        self.motion.fault()
    }
}

impl Default for Actuator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
