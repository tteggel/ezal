//! The safety-ordered actuator sequence shared by firmware and host tests.
//!
//! Feedback and motion supervision always precede relay timing, and every
//! command passes the [`FeedbackInterlock`] before reaching [`ActuatorGuard`]. The
//! firmware owns the GPIOs and applies [`Actuator::actual`] whenever a call
//! reports a change; it holds no ordering rules of its own.

use crate::drive::{ActuatorGuard, Command, DriveCommand};
use crate::interlock::FeedbackInterlock;
use crate::motion::{MotionConfig, MotionFault, MotionMonitor};

/// Leased, debounced, feedback-interlocked output state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Actuator {
    guard: ActuatorGuard,
    motion: MotionMonitor,
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
        }
    }

    /// Enforce feedback and motion supervision, then advance relay/lease timers.
    ///
    /// Returns `true` when the applicable output changed.
    pub fn service(&mut self, interlock: &FeedbackInterlock, now_ms: u64) -> bool {
        let before = self.guard.actual();
        self.enforce(interlock, now_ms);
        // Inhibition precedes pending transitions, so an activation due now
        // is checked against current feedback before it can energise.
        self.guard.update(now_ms);
        self.motion.applied(self.guard.actual(), now_ms);
        self.guard.actual() != before
    }

    /// Admit a command through the interlock.
    ///
    /// Timers are not advanced first: the command may cancel an activation
    /// that falls due at this instant. Returns `true` when the output changed.
    pub fn command(
        &mut self,
        command: Command,
        interlock: &FeedbackInterlock,
        now_ms: u64,
    ) -> bool {
        self.command_until(command, interlock, now_ms, u64::MAX)
    }

    /// Admit a command whose source already established an absolute expiry.
    /// Queueing and transport delays consume that lifetime; receiving it here
    /// cannot grant it a fresh lease. Ordinary local callers use [`Self::command`].
    pub fn command_until(
        &mut self,
        command: Command,
        interlock: &FeedbackInterlock,
        now_ms: u64,
        valid_until_ms: u64,
    ) -> bool {
        let before = self.guard.actual();
        self.enforce(interlock, now_ms);
        let permitted = if self.motion.fault().is_some() {
            Command::Inhibit
        } else {
            interlock.guard_command(command, now_ms)
        };
        self.guard.command_until(permitted, now_ms, valid_until_ms);
        self.motion.applied(self.guard.actual(), now_ms);
        self.guard.actual() != before
    }

    fn enforce(&mut self, interlock: &FeedbackInterlock, now_ms: u64) {
        // Observe the output that actually existed throughout the elapsed
        // interval, before any freshness check or incoming stop changes it.
        self.motion.observe(interlock, now_ms);
        interlock.enforce(&mut self.guard, now_ms);
        if self.motion.fault().is_some() {
            self.guard.command(Command::Inhibit, now_ms);
        }
    }

    /// Next instant at which relay timing, the lease, feedback, or lack of
    /// motion progress may change the output. Call [`Self::service`] at or after it.
    pub fn next_deadline_ms(&self, interlock: &FeedbackInterlock, now_ms: u64) -> Option<u64> {
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
