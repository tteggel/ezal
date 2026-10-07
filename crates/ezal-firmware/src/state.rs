//! Embassy synchronization around pure manual-control coordination.
//!
//! No lock crosses an await or performs I/O. The signal is only a wake hint;
//! every actuator iteration drains a coherent input from the stored state.

use core::cell::RefCell;
use embassy_sync::blocking_mutex::{raw::CriticalSectionRawMutex, Mutex};
use embassy_sync::signal::Signal;
use embassy_time::Instant;
use ezal_core::actuator::ActuatorInput;
use ezal_core::coordination::{ControlStatus, CoordinationState, TelemetrySnapshot};
use ezal_core::drive::DriveCommand;
use ezal_core::feedback::FeedbackSnapshot;
use ezal_core::interlock::FeedbackInterlock;
use ezal_core::mailbox::CommandEnvelope;
use ezal_core::motion::MotionFault;
use ezal_core::network::NetworkGate;
use ezal_core::position::FeedbackVoltages;

pub static STATE: SharedState = SharedState::new();

pub struct SharedState {
    inner: Mutex<CriticalSectionRawMutex, RefCell<CoordinationState>>,
    changed: Signal<CriticalSectionRawMutex, ()>,
}

impl SharedState {
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(RefCell::new(CoordinationState::new())),
            changed: Signal::new(),
        }
    }

    fn with<R>(&self, operation: impl FnOnce(&mut CoordinationState) -> R) -> R {
        self.inner.lock(|state| operation(&mut state.borrow_mut()))
    }

    fn wake_if(&self, changed: bool) -> bool {
        if changed {
            self.changed.signal(());
        }
        changed
    }

    pub fn publish_network(&self, ready: bool) {
        self.wake_if(self.with(|state| state.publish_network(ready, Instant::now().as_millis())));
    }

    pub fn network(&self) -> NetworkGate {
        self.with(|state| state.network())
    }

    pub fn store_raw_feedback(&self, raw: FeedbackVoltages) {
        self.with(|state| state.store_raw_feedback(raw));
    }

    pub fn publish_feedback(&self, feedback: FeedbackSnapshot, interlock: FeedbackInterlock) {
        self.wake_if(self.with(|state| state.publish_feedback(feedback, interlock)));
    }

    pub fn feedback(&self) -> FeedbackSnapshot {
        self.with(|state| state.feedback())
    }

    pub fn register_client(&self) -> u32 {
        let id = self.with(|state| state.register_client(Instant::now().as_millis()));
        self.changed.signal(());
        id
    }

    pub fn telemetry(&self, client_id: u32, now_ms: u64) -> TelemetrySnapshot {
        self.with(|state| state.telemetry(client_id, now_ms))
    }

    pub fn control_status(&self, client_id: u32) -> ControlStatus {
        self.telemetry(client_id, Instant::now().as_millis())
            .control
    }

    pub fn take_control(&self, client_id: u32) {
        self.with(|state| state.take_control(client_id, Instant::now().as_millis()));
        self.changed.signal(());
    }

    pub fn release_client(&self, client_id: u32) {
        self.wake_if(self.with(|state| state.release_client(client_id)));
    }

    pub fn accept_command(&self, client_id: u32, command: CommandEnvelope) -> bool {
        let accepted =
            self.with(|state| state.accept_command(client_id, command, Instant::now().as_millis()));
        // A rejected admission can still have expired a network permission.
        self.changed.signal(());
        accepted
    }

    pub fn record_actuator(&self, drive: DriveCommand, fault: Option<MotionFault>) {
        self.with(|state| state.record_actuator(drive, fault));
    }

    pub fn take_actuator_input(&self, now_ms: u64) -> ActuatorInput {
        self.with(|state| state.take_actuator_input(now_ms))
    }

    pub async fn wait_changed(&self) {
        self.changed.wait().await;
    }
}
