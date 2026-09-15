//! Shared inter-task state — the coordination point between the web transport,
//! the feedback sensor, and the drive actuator.
//!
//! [`SharedState`] is the one place three otherwise-independent tasks meet:
//! the simulator/controller and dashboard sockets publish [`Command`]s; web
//! clients read control ownership and telemetry; feedback tasks store the
//! latest sample; and the drive loop waits on commands and records what it
//! actually applied. Housing it here keeps those modules independent.

use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU8, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;

use ezal_core::control::ControlState;
use ezal_core::drive::{AzimuthDirection, Command, DriveCommand, ElevationDirection};
use ezal_core::protocol::{ControlStatus, PositionTelemetry, TrackingMode, TrackingTelemetry};
use ezal_core::simulation::PassPhase;

/// The one shared-state instance, wired to every task from `main`.
pub static STATE: SharedState = SharedState::new();

/// Coordination state shared safely between Embassy tasks.
pub struct SharedState {
    positions: SharedPositions,
    tracking: SharedTracking,
    commands: Signal<CriticalSectionRawMutex, Command>,
    next_client_id: AtomicU32,
    controller_id: AtomicU32,
    drive: AtomicU8,
    autonomous: AtomicBool,
}

impl SharedState {
    /// Create empty shared state.
    pub const fn new() -> Self {
        Self {
            positions: SharedPositions::new(),
            tracking: SharedTracking::new(),
            commands: Signal::new(),
            next_client_id: AtomicU32::new(1),
            controller_id: AtomicU32::new(0),
            drive: AtomicU8::new(encode_drive(DriveCommand::IDLE)),
            autonomous: AtomicBool::new(false),
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

    /// Store an internally coherent autonomous-tracking snapshot.
    pub fn store_tracking(&self, tracking: TrackingTelemetry) {
        self.tracking.store(tracking);
    }

    /// Read the latest autonomous-tracking snapshot.
    pub fn tracking(&self) -> TrackingTelemetry {
        self.tracking.load()
    }

    /// Enable or disable autonomous ownership of the actuator command stream.
    #[cfg(feature = "simulator")]
    pub fn set_autonomous(&self, autonomous: bool) {
        self.autonomous.store(autonomous, Ordering::Release);
        self.controller_id.store(0, Ordering::Release);
        self.apply_command(Command::Stop);
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
        if !self.autonomous.load(Ordering::Acquire) {
            let _ = self
                .controller_id
                .compare_exchange(0, id, Ordering::AcqRel, Ordering::Acquire);
        }
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
        if self.autonomous.load(Ordering::Acquire) {
            return;
        }
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
        if self.autonomous.load(Ordering::Acquire) || !self.is_controller(client_id) {
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

/// Sequence-locked autonomous telemetry. The writer is one tracking task;
/// readers retry if they overlap a write, avoiding mixed-pass snapshots.
struct SharedTracking {
    sequence: AtomicU32,
    mode: AtomicU8,
    pass_index: AtomicU32,
    phase: AtomicU8,
    phase_remaining_ms: AtomicU32,
    azimuth_tenths: AtomicI32,
    elevation_tenths: AtomicI32,
    target_azimuth_tenths: AtomicI32,
    target_elevation_tenths: AtomicI32,
    control_state: AtomicU8,
}

const NO_TARGET: i32 = i32::MIN;

impl SharedTracking {
    const fn new() -> Self {
        Self {
            sequence: AtomicU32::new(0),
            mode: AtomicU8::new(encode_tracking_mode(TrackingMode::Manual)),
            pass_index: AtomicU32::new(0),
            phase: AtomicU8::new(encode_phase(PassPhase::Pause)),
            phase_remaining_ms: AtomicU32::new(0),
            azimuth_tenths: AtomicI32::new(0),
            elevation_tenths: AtomicI32::new(0),
            target_azimuth_tenths: AtomicI32::new(NO_TARGET),
            target_elevation_tenths: AtomicI32::new(NO_TARGET),
            control_state: AtomicU8::new(encode_control_state(ControlState::Idle)),
        }
    }

    fn store(&self, value: TrackingTelemetry) {
        self.sequence.fetch_add(1, Ordering::AcqRel);
        self.mode
            .store(encode_tracking_mode(value.mode), Ordering::Relaxed);
        self.pass_index.store(value.pass_index, Ordering::Relaxed);
        self.phase
            .store(encode_phase(value.phase), Ordering::Relaxed);
        self.phase_remaining_ms.store(
            value.phase_remaining_ms.min(u32::MAX as u64) as u32,
            Ordering::Relaxed,
        );
        self.azimuth_tenths
            .store(value.azimuth_tenths, Ordering::Relaxed);
        self.elevation_tenths
            .store(value.elevation_tenths, Ordering::Relaxed);
        self.target_azimuth_tenths.store(
            value.target_azimuth_tenths.unwrap_or(NO_TARGET),
            Ordering::Relaxed,
        );
        self.target_elevation_tenths.store(
            value.target_elevation_tenths.unwrap_or(NO_TARGET),
            Ordering::Relaxed,
        );
        self.control_state
            .store(encode_control_state(value.control_state), Ordering::Relaxed);
        self.sequence.fetch_add(1, Ordering::Release);
    }

    fn load(&self) -> TrackingTelemetry {
        loop {
            let before = self.sequence.load(Ordering::Acquire);
            if before & 1 != 0 {
                core::hint::spin_loop();
                continue;
            }
            let target_azimuth = self.target_azimuth_tenths.load(Ordering::Relaxed);
            let target_elevation = self.target_elevation_tenths.load(Ordering::Relaxed);
            let value = TrackingTelemetry {
                mode: decode_tracking_mode(self.mode.load(Ordering::Relaxed)),
                pass_index: self.pass_index.load(Ordering::Relaxed),
                phase: decode_phase(self.phase.load(Ordering::Relaxed)),
                phase_remaining_ms: self.phase_remaining_ms.load(Ordering::Relaxed) as u64,
                azimuth_tenths: self.azimuth_tenths.load(Ordering::Relaxed),
                elevation_tenths: self.elevation_tenths.load(Ordering::Relaxed),
                target_azimuth_tenths: (target_azimuth != NO_TARGET).then_some(target_azimuth),
                target_elevation_tenths: (target_elevation != NO_TARGET)
                    .then_some(target_elevation),
                control_state: decode_control_state(self.control_state.load(Ordering::Relaxed)),
            };
            if before == self.sequence.load(Ordering::Acquire) {
                return value;
            }
        }
    }
}

const fn encode_tracking_mode(mode: TrackingMode) -> u8 {
    match mode {
        TrackingMode::Manual => 0,
        TrackingMode::HardwareWalkingSkeleton => 1,
    }
}

const fn decode_tracking_mode(value: u8) -> TrackingMode {
    match value {
        1 => TrackingMode::HardwareWalkingSkeleton,
        _ => TrackingMode::Manual,
    }
}

const fn encode_phase(phase: PassPhase) -> u8 {
    match phase {
        PassPhase::Pause => 0,
        PassPhase::Tracking => 1,
        PassPhase::Acquiring => 2,
        PassPhase::Fault => 3,
    }
}

const fn decode_phase(value: u8) -> PassPhase {
    match value {
        1 => PassPhase::Tracking,
        2 => PassPhase::Acquiring,
        3 => PassPhase::Fault,
        _ => PassPhase::Pause,
    }
}

const fn encode_control_state(state: ControlState) -> u8 {
    match state {
        ControlState::Tracking => 0,
        ControlState::Idle => 1,
        ControlState::FeedbackUnavailable => 2,
        ControlState::TargetStale => 3,
        ControlState::FeedbackStale => 4,
        ControlState::TargetInvalid => 5,
        ControlState::FeedbackInvalid => 6,
        ControlState::TimestampInvalid => 7,
        ControlState::AcquisitionTimeout => 8,
    }
}

const fn decode_control_state(value: u8) -> ControlState {
    match value {
        0 => ControlState::Tracking,
        1 => ControlState::Idle,
        2 => ControlState::FeedbackUnavailable,
        3 => ControlState::TargetStale,
        4 => ControlState::FeedbackStale,
        5 => ControlState::TargetInvalid,
        6 => ControlState::FeedbackInvalid,
        7 => ControlState::TimestampInvalid,
        _ => ControlState::AcquisitionTimeout,
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
