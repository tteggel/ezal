//! The safety-ordered actuator sequence shared by firmware and host tests.
//!
//! [`Actuator::step`] consumes one coherent observation and command snapshot.
//! Motion accounting and permissions precede relay timing, and a queued stop
//! is consumed before a pending activation. Firmware supplies time, applies
//! the resulting GPIO state, and wakes for the returned deadline.

use crate::drive::{ActuatorGuard, Command, CommandEnvelope, DriveCommand, RelayConfig};
use crate::duty::{DutyConfig, DutyHistory, DutyMonitor, DutyStatus};
use crate::feedback::FEEDBACK_MAX_AGE_MS;
use crate::interlock::{FeedbackInterlock, TravelProfile};
use crate::motion::{MotionConfig, MotionFault, MotionMonitor};
use crate::network::NetworkGate;

/// Installation policy owned by the final output boundary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActuatorConfig {
    /// Ordinary relay timing; source expiry and inhibition remain independent.
    pub relays: RelayConfig,
    /// Common rate/coast bounds for endpoint and physical-motion supervision.
    pub travel: TravelProfile,
    /// Progress and measurement-noise thresholds.
    pub motion: MotionConfig,
    /// Per-axis energized/rest accounting shared by every source.
    pub duty: DutyConfig,
}

impl ActuatorConfig {
    /// Installed timing with provisional coast bounds and unidentified-hardware duty.
    pub const DEFAULT: Self = Self {
        relays: RelayConfig::DEFAULT,
        travel: TravelProfile::DEFAULT,
        motion: MotionConfig::DEFAULT,
        duty: DutyConfig::DEFAULT,
    };

    /// Reversal must allow old-direction coast and old feedback to finish before
    /// establishing the new direction's baseline. No per-pulse blind window is used.
    pub const fn is_valid(self) -> bool {
        let Some(reversal_settle_ms) = self
            .travel
            .maximum_coast_ms()
            .checked_add(FEEDBACK_MAX_AGE_MS)
        else {
            return false;
        };
        self.relays.is_valid()
            && self.travel.is_valid()
            && self.motion.is_valid()
            && self.duty.is_valid()
            && self.duty.energized_ms >= self.relays.min_active_ms
            && self.relays.reversal_off_ms >= reversal_settle_ms
    }
}

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
    /// Remaining per-axis allowance and any required cooldown.
    pub duty: DutyStatus,
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
    config: ActuatorConfig,
    configuration_valid: bool,
    guard: ActuatorGuard,
    motion: MotionMonitor,
    duty: DutyMonitor,
    network_generation: u64,
}

impl Actuator {
    /// Boot idle with unknown prior motor duty; a full rest is required.
    pub const fn new() -> Self {
        Self::with_config(ActuatorConfig::DEFAULT, DutyHistory::Unknown)
    }

    /// Create an idle actuator with explicit policy and trustworthy duty history.
    /// Invalid settings/history latch inhibition before any output can energize.
    pub const fn with_config(config: ActuatorConfig, history: DutyHistory) -> Self {
        Self {
            config,
            configuration_valid: config.is_valid() && history.is_valid(config.duty),
            guard: ActuatorGuard::new(config.relays),
            motion: MotionMonitor::new(config.motion, config.travel),
            duty: DutyMonitor::new(config.duty, history, config.relays.min_active_ms),
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
        self.duty.observe(self.guard.actual(), now_ms);
        let interlock = input.interlock.with_travel_profile(self.config.travel);
        self.enforce(&interlock, now_ms);
        let network_ready = input.network.ready(now_ms);
        if !network_ready || self.network_generation != input.network.generation() {
            self.guard.command(Command::Inhibit, now_ms);
        }
        self.network_generation = input.network.generation();

        if let Some(envelope) = input.command {
            let permitted = if !network_ready || self.fault().is_some() {
                Command::Inhibit
            } else {
                match interlock.guard_command(envelope.command, now_ms) {
                    Command::Drive(drive) => {
                        Command::Drive(self.duty.guard_drive(drive, self.guard.actual()))
                    }
                    Command::Jog(drive) => {
                        Command::Jog(self.duty.guard_drive(drive, self.guard.actual()))
                    }
                    other => other,
                }
            };
            // command_until expires the old lease before a late refresh and
            // consumes a release before advancing its pending activation.
            self.guard
                .command_until(permitted, now_ms, envelope.valid_until_ms);
        } else {
            self.guard.update(now_ms);
        }
        self.motion.applied(self.guard.actual(), now_ms);
        self.duty.applied(self.guard.actual());

        let network_deadline = network_ready.then(|| input.network.deadline_ms()).flatten();
        ActuatorUpdate {
            output: self.guard.actual(),
            status: self.status(),
            next_deadline_ms: [self.next_deadline_ms(&interlock, now_ms), network_deadline]
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
        let permitted_actual = self
            .duty
            .guard_drive(interlock.guard_drive(actual, now_ms), actual);
        let permitted_target = self
            .duty
            .guard_drive(interlock.guard_drive(target, now_ms), actual);
        self.guard.inhibit_axes(
            actual.azimuth != permitted_actual.azimuth
                || target.azimuth != permitted_target.azimuth,
            actual.elevation != permitted_actual.elevation
                || target.elevation != permitted_target.elevation,
            now_ms,
        );
        if self.fault().is_some() {
            self.guard.command(Command::Inhibit, now_ms);
        }
    }

    /// Physical deadlines, combined with operating permission by `step`. The
    /// interlock is the one `step` bound to this actuator's travel profile.
    fn next_deadline_ms(&self, interlock: &FeedbackInterlock, now_ms: u64) -> Option<u64> {
        let transition = self.guard.next_transition_ms(now_ms);
        let feedback = interlock.deadline_ms(self.guard.actual(), now_ms);
        let duty = if self.fault().is_none() {
            self.duty.deadline_ms(self.guard.actual(), now_ms)
        } else {
            None
        };
        [transition, feedback, self.motion.deadline_ms(now_ms), duty]
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
        if self.configuration_valid {
            self.motion.fault()
        } else {
            Some(MotionFault::InvalidConfiguration)
        }
    }

    /// Coherent health and duty state at the most recent service instant.
    pub const fn status(&self) -> ActuatorStatus {
        ActuatorStatus {
            motion_fault: self.fault(),
            duty: self.duty.status(),
        }
    }

    /// Settings copied into a plan; replacing them during a run requires replanning.
    pub const fn configuration(&self) -> ActuatorConfig {
        self.config
    }

    /// Require a serviced, inhibited, physically settled starting snapshot.
    /// Copying the entire actuator then preserves relay holds, fault/progress
    /// state and partially completed duty rest in offline evaluation.
    pub(crate) fn resting_history(&self, now_ms: u64) -> Option<([u64; 2], [u64; 2])> {
        if self.actual().is_active() || self.target().is_active() || self.fault().is_some() {
            return None;
        }
        let (used, idle) = self.duty.history(now_ms)?;
        (idle[0] >= self.config.travel.azimuth.coast_ms
            && idle[1] >= self.config.travel.elevation.coast_ms)
            .then_some((used, idle))
    }
}

impl Default for Actuator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
