//! Core-0 thread-executor synchronization around the device owner.
//! No core-1 or interrupt-executor task may access this state. Thread-mode
//! locking leaves IRQs enabled during bounded propagation/planning steps.
//!
//! No lock crosses an await or performs I/O. The signal is only a wake hint;
//! every actuator iteration drains a coherent input from the stored state.

use core::cell::RefCell;
use embassy_sync::blocking_mutex::{raw::ThreadModeRawMutex, Mutex};
use embassy_sync::signal::Signal;
use embassy_time::Instant;
use ezal_core::actuator::{Actuator, ActuatorInput};
use ezal_core::coordination::{ControlStatus, CoordinationState, TelemetrySnapshot};
use ezal_core::feedback::FeedbackSnapshot;
use ezal_core::interlock::FeedbackInterlock;
use ezal_core::mailbox::CommandEnvelope;
use ezal_core::network::NetworkGate;
use ezal_core::ntp::UtcClock;
use ezal_core::operator::{OperatorGrant, OperatorToken};
use ezal_core::position::FeedbackVoltages;
use ezal_core::protocol::{TrackingIntent, TrackingReply, TrackingTelemetry};
use ezal_core::session::CommandFreshness;

pub static STATE: SharedState = SharedState::new();

pub struct SharedState {
    inner: Mutex<ThreadModeRawMutex, RefCell<CoordinationState>>,
    changed: Signal<ThreadModeRawMutex, ()>,
}

impl SharedState {
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(RefCell::new(CoordinationState::new())),
            changed: Signal::new(),
        }
    }

    pub(crate) fn with<R>(&self, operation: impl FnOnce(&mut CoordinationState) -> R) -> R {
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
        self.wake_if(
            self.with(|state| {
                state.publish_feedback(feedback, interlock, Instant::now().as_millis())
            }),
        );
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

    pub fn dashboard_telemetry(
        &self,
        client_id: u32,
        now_ms: u64,
    ) -> (TelemetrySnapshot, TrackingTelemetry) {
        self.with(|state| {
            (
                state.telemetry(client_id, now_ms),
                state.tracking_telemetry(client_id, now_ms),
            )
        })
    }

    pub fn request_tracking(
        &self,
        client_id: u32,
        intent: TrackingIntent,
        freshness: &CommandFreshness,
    ) -> TrackingReply {
        let reply = self.with(|state| {
            state.request_tracking(
                client_id,
                intent,
                freshness,
                Instant::now().as_millis(),
                |pass| crate::prediction::report().offers(pass),
            )
        });
        self.changed.signal(());
        reply
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
        self.wake_if(
            self.with(|state| state.release_client(client_id, Instant::now().as_millis())),
        );
    }

    pub fn accept_command(&self, client_id: u32, command: CommandEnvelope) -> bool {
        let accepted =
            self.with(|state| state.accept_command(client_id, command, Instant::now().as_millis()));
        // A rejected admission can still have expired a network permission.
        self.changed.signal(());
        accepted
    }

    pub fn record_actuator(&self, actuator: &Actuator, now_ms: u64) {
        self.with(|state| state.record_actuator(actuator, now_ms));
    }

    pub fn execute_tracking(&self, now_ms: u64) {
        self.with(|state| state.execute_tracking(now_ms));
    }

    pub fn execution_deadline_ms(&self) -> Option<u64> {
        self.with(|state| state.tracking().execution_deadline_ms())
    }

    pub fn publish_clock(&self, clock: UtcClock) {
        self.with(|state| state.publish_clock(clock, Instant::now().as_millis()));
        self.changed.signal(());
    }

    pub fn operator(&self, client_id: u32, now_ms: u64) -> Option<OperatorGrant> {
        self.with(|state| state.operator(client_id, now_ms))
    }

    pub fn renew_operator(&self, token: OperatorToken, now_ms: u64) {
        self.with(|state| state.renew_operator(token, now_ms));
        self.changed.signal(());
    }

    pub fn take_actuator_input(&self, now_ms: u64) -> ActuatorInput {
        self.with(|state| state.take_actuator_input(now_ms))
    }

    pub async fn wait_changed(&self) {
        self.changed.wait().await;
    }
}
