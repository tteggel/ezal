//! Pure coordination of manual ownership, feedback and actuator delivery.
//!
//! Time is supplied to transitions explicitly. Reading a snapshot never
//! expires permissions, revokes ownership or drains pending commands.

use crate::actuator::{ActuatorInput, ActuatorStatus};
use crate::authority::ControlAuthority;
use crate::drive::DriveCommand;
use crate::duty::DutyStatus;
use crate::feedback::FeedbackSnapshot;
use crate::interlock::FeedbackInterlock;
use crate::mailbox::{CommandEnvelope, CommandMailbox};
use crate::motion::MotionFault;
use crate::network::NetworkGate;
use crate::position::FeedbackVoltages;

/// Manual ownership and the output actually applied by the GPIO task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlStatus {
    /// Whether the receiving connection owns current manual control.
    pub controller: bool,
    /// Last output applied to GPIOs.
    pub drive: DriveCommand,
    /// Whether a usable network observation is still current.
    pub network_ready: bool,
    /// Latched physical feedback fault, cleared only by restart.
    pub motion_fault: Option<MotionFault>,
    /// Shared per-axis allowance, including any rest required before movement.
    pub duty: DutyStatus,
}

/// One coherent observation for a dashboard connection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TelemetrySnapshot {
    /// Raw diagnostics and last calibrated feedback.
    pub feedback: FeedbackSnapshot,
    /// Ownership and applied output.
    pub control: ControlStatus,
    /// Permission generation used to bind this observation's freshness proof.
    pub network: NetworkGate,
}

/// Runtime-independent state; firmware supplies synchronization and wakeups.
pub struct CoordinationState {
    feedback: FeedbackSnapshot,
    interlock: FeedbackInterlock,
    authority: ControlAuthority,
    applied_drive: DriveCommand,
    actuator_status: ActuatorStatus,
    commands: CommandMailbox,
    network: NetworkGate,
}

impl Default for CoordinationState {
    fn default() -> Self {
        Self::new()
    }
}

impl CoordinationState {
    /// Boot idle, with no feedback, network permission or owner.
    pub const fn new() -> Self {
        Self {
            feedback: FeedbackSnapshot::INITIAL,
            interlock: FeedbackInterlock::INHIBITED,
            authority: ControlAuthority::new(),
            applied_drive: DriveCommand::IDLE,
            actuator_status: ActuatorStatus {
                motion_fault: None,
                duty: DutyStatus::INITIAL,
            },
            commands: CommandMailbox::new(),
            network: NetworkGate::new(),
        }
    }

    fn revoke_network_motion(&mut self) {
        self.authority.revoke_manual();
        self.commands.submit(CommandEnvelope::INHIBIT);
    }

    /// Apply time-dependent coordination transitions before admission/drain.
    pub fn advance(&mut self, now_ms: u64) {
        if self.network.expire(now_ms) {
            self.revoke_network_motion();
        }
    }

    /// Publish readiness, returning whether motion was revoked.
    pub fn publish_network(&mut self, ready: bool, now_ms: u64) -> bool {
        let lost = self.network.observe(now_ms, ready);
        if lost {
            self.revoke_network_motion();
        }
        lost
    }

    /// Read permission without changing coordination state.
    pub const fn network(&self) -> NetworkGate {
        self.network
    }

    /// Retain the ADC POST readings before recurring acquisition starts.
    pub fn store_raw_feedback(&mut self, raw: FeedbackVoltages) {
        self.feedback.raw = Some(raw);
    }

    /// Publish one acquisition atomically; report whether active motion needs a wakeup.
    pub fn publish_feedback(
        &mut self,
        feedback: FeedbackSnapshot,
        interlock: FeedbackInterlock,
    ) -> bool {
        self.feedback = feedback;
        self.interlock = interlock;
        self.applied_drive.is_active()
    }

    /// Read the latest feedback diagnostics.
    pub const fn feedback(&self) -> FeedbackSnapshot {
        self.feedback
    }

    /// Allocate a connection, granting unclaimed control only while online.
    pub fn register_client(&mut self, now_ms: u64) -> u32 {
        self.advance(now_ms);
        let id = self.authority.register_client();
        if !self.network.ready(now_ms) {
            self.authority.revoke_manual();
        }
        id
    }

    /// Observe one coherent snapshot without performing transitions.
    pub fn telemetry(&self, client_id: u32, now_ms: u64) -> TelemetrySnapshot {
        let ready = self.network.ready(now_ms);
        TelemetrySnapshot {
            feedback: self.feedback,
            control: ControlStatus {
                controller: ready && self.authority.is_controller(client_id),
                drive: self.applied_drive,
                network_ready: ready,
                motion_fault: self.actuator_status.motion_fault,
                duty: self.actuator_status.duty,
            },
            network: self.network,
        }
    }

    /// Transfer ownership and queue inhibition before any replacement movement.
    pub fn take_control(&mut self, client_id: u32, now_ms: u64) -> bool {
        self.advance(now_ms);
        if !self.network.ready(now_ms) || !self.authority.take_control(client_id) {
            return false;
        }
        self.commands.submit(CommandEnvelope::INHIBIT);
        true
    }

    /// Releasing an old viewer cannot interrupt the current owner.
    pub fn release_client(&mut self, client_id: u32) -> bool {
        if !self.authority.release_client(client_id) {
            return false;
        }
        self.commands.submit(CommandEnvelope::INHIBIT);
        true
    }

    /// Admit a source-validated envelope without restarting its expiry.
    pub fn accept_command(
        &mut self,
        client_id: u32,
        envelope: CommandEnvelope,
        now_ms: u64,
    ) -> bool {
        self.advance(now_ms);
        if !self.network.ready(now_ms) || !self.authority.is_controller(client_id) {
            return false;
        }
        self.commands.submit(envelope);
        true
    }

    /// Record output only after the GPIO adapter has applied it.
    pub fn record_actuator(&mut self, drive: DriveCommand, status: ActuatorStatus) {
        self.applied_drive = drive;
        self.actuator_status = status;
    }

    /// Atomically drain pending work with the feedback and network it will use.
    pub fn take_actuator_input(&mut self, now_ms: u64) -> ActuatorInput {
        self.advance(now_ms);
        ActuatorInput {
            interlock: self.interlock,
            network: self.network,
            command: self.commands.take(),
        }
    }
}
