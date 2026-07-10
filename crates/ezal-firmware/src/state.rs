//! Shared inter-task state — the coordination point between the web transport,
//! the feedback sensor, and the drive actuator.
//!
//! [`SharedState`] is the one place three otherwise-independent tasks meet:
//! the dashboard sockets in [`crate::web`] publish [`Command`]s and read back
//! control ownership and telemetry; the feedback task stores the latest ADC
//! sample; and the drive loop in [`crate::drive`] waits on those commands and
//! records what it actually applied. Housing it here — rather than inside any
//! one of those modules — keeps them from depending on one another just to
//! reach the channel between them.

use core::sync::atomic::{AtomicI32, AtomicU32, AtomicU8, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;

use ezal_core::drive::{AzimuthDirection, Command, DriveCommand, ElevationDirection};
use ezal_core::protocol::{ControlStatus, PositionTelemetry};

/// The one shared-state instance, wired to every task from `main`.
pub static STATE: SharedState = SharedState::new();

/// Coordination state shared safely between Embassy tasks.
pub struct SharedState {
    positions: SharedPositions,
    commands: Signal<CriticalSectionRawMutex, Command>,
    next_client_id: AtomicU32,
    controller_id: AtomicU32,
    drive: AtomicU8,
}

impl SharedState {
    /// Create empty shared state.
    pub const fn new() -> Self {
        Self {
            positions: SharedPositions::new(),
            commands: Signal::new(),
            next_client_id: AtomicU32::new(1),
            controller_id: AtomicU32::new(0),
            drive: AtomicU8::new(encode_drive(DriveCommand::IDLE)),
        }
    }

    /// Store the latest raw ADC millivolt readings.
    pub fn store_position(&self, position: PositionTelemetry) {
        self.positions.store(position);
    }

    /// Read the latest raw ADC millivolt readings.
    pub fn position(&self) -> PositionTelemetry {
        self.positions.load()
    }

    /// Allocate a WebSocket connection id and assign initial control when no
    /// controller is active.
    pub fn register_client(&self) -> u32 {
        let id = self.next_client_id.fetch_add(1, Ordering::Relaxed);
        let id = if id == 0 {
            self.next_client_id.fetch_add(1, Ordering::Relaxed)
        } else {
            id
        };
        let _ = self
            .controller_id
            .compare_exchange(0, id, Ordering::AcqRel, Ordering::Acquire);
        id
    }

    /// Whether a WebSocket connection currently owns drive control.
    pub fn is_controller(&self, client_id: u32) -> bool {
        self.controller_id.load(Ordering::Acquire) == client_id
    }

    /// Return the control status as seen by one connection.
    pub fn control_status(&self, client_id: u32) -> ControlStatus {
        ControlStatus {
            controller: self.is_controller(client_id),
            drive: decode_drive(self.drive.load(Ordering::Acquire)),
        }
    }

    /// Make a connection the controller and stop any stale drive lease.
    pub fn take_control(&self, client_id: u32) {
        self.controller_id.store(client_id, Ordering::Release);
        self.apply_command(Command::Stop);
    }

    /// Release control if the disconnecting connection still owns it.
    pub fn release_client(&self, client_id: u32) {
        if self
            .controller_id
            .compare_exchange(client_id, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.apply_command(Command::Stop);
        }
    }

    /// Accept a client command only from the current controller.
    pub fn accept_command(&self, client_id: u32, command: Command) -> bool {
        if !self.is_controller(client_id) {
            return false;
        }

        self.apply_command(command);
        true
    }

    /// Signal a requested drive-state command to the task that owns the GPIOs.
    pub fn apply_command(&self, command: Command) {
        self.commands.signal(command);
    }

    /// Record the drive state currently applied to the output GPIOs.
    pub fn record_applied_drive(&self, drive: DriveCommand) {
        self.drive.store(encode_drive(drive), Ordering::Release);
    }

    /// Wait for the next drive-state command.
    pub async fn wait_command(&self) -> Command {
        self.commands.wait().await
    }
}

const fn encode_drive(drive: DriveCommand) -> u8 {
    let azimuth = match drive.azimuth {
        Some(AzimuthDirection::Clockwise) => 1,
        Some(AzimuthDirection::CounterClockwise) => 2,
        None => 0,
    };
    let elevation = match drive.elevation {
        Some(ElevationDirection::Up) => 1,
        Some(ElevationDirection::Down) => 2,
        None => 0,
    };

    azimuth | (elevation << 2)
}

const fn decode_drive(bits: u8) -> DriveCommand {
    DriveCommand {
        azimuth: match bits & 0b11 {
            1 => Some(AzimuthDirection::Clockwise),
            2 => Some(AzimuthDirection::CounterClockwise),
            _ => None,
        },
        elevation: match (bits >> 2) & 0b11 {
            1 => Some(ElevationDirection::Up),
            2 => Some(ElevationDirection::Down),
            _ => None,
        },
    }
}

/// Atomic position snapshot shared between the ADC task and WebSocket clients.
struct SharedPositions {
    a0_mv: AtomicI32,
    a1_mv: AtomicI32,
}

impl SharedPositions {
    const fn new() -> Self {
        Self {
            a0_mv: AtomicI32::new(PositionTelemetry::ZERO.a0_mv),
            a1_mv: AtomicI32::new(PositionTelemetry::ZERO.a1_mv),
        }
    }

    fn store(&self, position: PositionTelemetry) {
        self.a0_mv.store(position.a0_mv, Ordering::Relaxed);
        self.a1_mv.store(position.a1_mv, Ordering::Relaxed);
    }

    fn load(&self) -> PositionTelemetry {
        PositionTelemetry {
            a0_mv: self.a0_mv.load(Ordering::Relaxed),
            a1_mv: self.a1_mv.load(Ordering::Relaxed),
        }
    }
}
