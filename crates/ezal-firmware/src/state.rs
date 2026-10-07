//! Typed state shared by the feedback, dashboard, and actuator tasks.
//!
//! Snapshots and command ownership changes use a short critical section. No
//! lock crosses an await or performs I/O; the GPIO task alone applies commands.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::{raw::CriticalSectionRawMutex, Mutex};
use embassy_sync::signal::Signal;
use embassy_time::Instant;

use ezal_core::authority::ControlAuthority;
use ezal_core::drive::{Command, DriveCommand, MOVEMENT_LEASE_MS};
use ezal_core::interlock::FeedbackInterlock;
use ezal_core::mailbox::CommandMailbox;
use ezal_core::motion::MotionFault;
use ezal_core::network::NetworkGate;
use ezal_core::protocol::{ControlStatus, PositionTelemetry, TrackingTelemetry};

/// The one shared-state instance, wired to every task from `main`.
pub static STATE: SharedState = SharedState::new();

/// Values read or changed together while the critical section is held.
struct Snapshot {
    position: PositionTelemetry,
    tracking: TrackingTelemetry,
    authority: ControlAuthority,
    applied_drive: DriveCommand,
    interlock: FeedbackInterlock,
    commands: CommandMailbox,
    command_valid_until_ms: Option<u64>,
    network: NetworkGate,
    motion_fault: Option<MotionFault>,
}

/// One command plus its original expiry. Queueing or consuming a command must
/// never restart the time budget assigned when its source was validated.
pub struct QueuedCommand {
    pub command: Command,
    pub valid_until_ms: Option<u64>,
}

/// Coordination state shared safely between Embassy tasks.
pub struct SharedState {
    snapshot: Mutex<CriticalSectionRawMutex, RefCell<Snapshot>>,
    changed: Signal<CriticalSectionRawMutex, ()>,
}

impl SharedState {
    /// Create empty shared state. Motion stays inhibited until a supervisor
    /// publishes an interlock built from the installed calibration.
    pub const fn new() -> Self {
        Self {
            snapshot: Mutex::new(RefCell::new(Snapshot {
                position: PositionTelemetry::ZERO,
                tracking: TrackingTelemetry::ZERO,
                authority: ControlAuthority::new(),
                applied_drive: DriveCommand::IDLE,
                interlock: FeedbackInterlock::INHIBITED,
                commands: CommandMailbox::new(),
                command_valid_until_ms: None,
                network: NetworkGate::new(),
                motion_fault: None,
            })),
            changed: Signal::new(),
        }
    }

    /// Run `change` inside the shared state's critical section.
    fn with<R>(&self, change: impl FnOnce(&mut Snapshot) -> R) -> R {
        self.snapshot.lock(|snapshot| {
            let mut snapshot = snapshot.borrow_mut();
            // Expiration is independent of the network task that renews the
            // permit. In particular, the GPIO task reaches this path even if
            // join, DHCP, or supervision stops making progress.
            if snapshot.network.expire(Instant::now().as_millis()) {
                self.revoke_network_motion(&mut snapshot);
            }
            change(&mut snapshot)
        })
    }

    fn revoke_network_motion(&self, snapshot: &mut Snapshot) {
        snapshot.authority.revoke_manual();
        snapshot.commands.submit(Command::Inhibit);
        snapshot.command_valid_until_ms = None;
        self.changed.signal(());
    }

    /// Queue a command and wake the actuator in the same critical section, so
    /// no wake-up can be lost between submitting and signalling.
    fn submit(&self, snapshot: &mut Snapshot, command: Command) {
        let now_ms = Instant::now().as_millis();
        self.submit_until(snapshot, command, now_ms.saturating_add(MOVEMENT_LEASE_MS));
    }

    fn submit_until(&self, snapshot: &mut Snapshot, command: Command, valid_until_ms: u64) {
        let command = snapshot
            .network
            .guard_command(command, Instant::now().as_millis());
        snapshot.commands.submit(command);
        // An unconsumed inhibition wins in CommandMailbox. Its expiry is
        // ignored, so carrying this later movement deadline cannot weaken it.
        snapshot.command_valid_until_ms = Some(valid_until_ms);
        self.changed.signal(());
    }

    /// Publish the latest link and IPv4-address check. Any loss immediately
    /// revokes manual ownership and overrides pending movement with inhibition.
    pub fn publish_network(&self, ready: bool) {
        self.with(|snapshot| {
            if snapshot.network.observe(Instant::now().as_millis(), ready) {
                self.revoke_network_motion(snapshot);
            }
        });
    }

    /// Expiring readiness snapshot for command admission and output servicing.
    pub fn network(&self) -> NetworkGate {
        self.with(|snapshot| snapshot.network)
    }

    /// Store the latest raw ADC millivolt readings.
    pub fn store_position(&self, position: PositionTelemetry) {
        self.with(|snapshot| snapshot.position = position);
    }

    /// Read the latest coherent pair of ADC readings.
    pub fn position(&self) -> PositionTelemetry {
        self.with(|snapshot| snapshot.position)
    }

    /// Publish one supervision tick: raw readings, tracking status, and the
    /// output interlock built from the supervisor's own calibration.
    pub fn publish_feedback(
        &self,
        position: Option<PositionTelemetry>,
        tracking: TrackingTelemetry,
        interlock: FeedbackInterlock,
    ) {
        self.with(|snapshot| {
            if let Some(position) = position {
                snapshot.position = position;
            }
            snapshot.tracking = tracking;
            snapshot.interlock = interlock;
            // Only an energised output can travel toward an endpoint or
            // outlive its sample, so only then must the actuator re-evaluate
            // at once. It checks a pending activation when that falls due.
            if snapshot.applied_drive.is_active() {
                self.changed.signal(());
            }
        });
    }

    /// Read the latest internally coherent tracking snapshot.
    pub fn tracking(&self) -> TrackingTelemetry {
        self.with(|snapshot| snapshot.tracking)
    }

    /// Enable or disable autonomous ownership of the actuator command stream.
    #[cfg(feature = "simulator")]
    pub fn set_autonomous(&self, autonomous: bool) {
        self.with(|snapshot| {
            let command = snapshot.authority.set_autonomous(autonomous);
            self.submit(snapshot, command);
        });
    }

    /// Allocate a WebSocket id and assign unclaimed manual control.
    pub fn register_client(&self) -> u32 {
        self.with(|snapshot| {
            let id = snapshot.authority.register_client();
            if !snapshot.network.ready(Instant::now().as_millis()) {
                snapshot.authority.revoke_manual();
            }
            id
        })
    }

    /// Return ownership and applied output from the same snapshot.
    pub fn control_status(&self, client_id: u32) -> ControlStatus {
        self.with(|snapshot| ControlStatus {
            controller: snapshot.authority.is_controller(client_id),
            drive: snapshot.applied_drive,
            network_ready: snapshot.network.ready(Instant::now().as_millis()),
            motion_fault: snapshot.motion_fault,
        })
    }

    /// Make a connection the controller and stop any stale drive lease.
    pub fn take_control(&self, client_id: u32) {
        self.with(|snapshot| {
            if !snapshot.network.ready(Instant::now().as_millis()) {
                return;
            }
            if let Some(command) = snapshot.authority.take_control(client_id) {
                self.submit(snapshot, command);
            }
        });
    }

    /// Release control if the disconnecting connection still owns it.
    pub fn release_client(&self, client_id: u32) {
        self.with(|snapshot| {
            if let Some(command) = snapshot.authority.release_client(client_id) {
                self.submit(snapshot, command);
            }
        });
    }

    /// Queue the current controller's command. The actuator applies the
    /// feedback interlock when it consumes it, so there is one such gate.
    pub fn accept_command(&self, client_id: u32, command: Command) -> bool {
        self.accept_command_until(
            client_id,
            command,
            Instant::now().as_millis().saturating_add(MOVEMENT_LEASE_MS),
        )
    }

    /// Admit a validated browser command with its source's absolute expiry.
    /// The GPIO task enforces this same deadline after mailbox consumption.
    pub fn accept_command_until(
        &self,
        client_id: u32,
        command: Command,
        valid_until_ms: u64,
    ) -> bool {
        self.with(|snapshot| {
            if !snapshot.network.ready(Instant::now().as_millis()) {
                return false;
            }
            match snapshot.authority.accept_command(client_id, command) {
                Some(command) => {
                    self.submit_until(snapshot, command, valid_until_ms);
                    true
                }
                None => false,
            }
        })
    }

    /// Queue a command from the firmware's own supervision.
    pub fn submit_command(&self, command: Command) {
        self.with(|snapshot| self.submit(snapshot, command));
    }

    /// Record the drive state currently applied to the output GPIOs.
    pub fn record_actuator(&self, drive: DriveCommand, fault: Option<MotionFault>) {
        self.with(|snapshot| {
            snapshot.applied_drive = drive;
            snapshot.motion_fault = fault;
        });
    }

    /// The interlock the actuator must satisfy, as last published.
    pub fn interlock(&self) -> FeedbackInterlock {
        self.with(|snapshot| snapshot.interlock)
    }

    /// Wait for a command or feedback change that needs actuator reevaluation.
    pub async fn wait_command(&self) -> Option<QueuedCommand> {
        self.changed.wait().await;
        self.with(|snapshot| {
            snapshot.commands.take().map(|command| QueuedCommand {
                command,
                valid_until_ms: snapshot.command_valid_until_ms.take(),
            })
        })
    }
}
