//! Direction-output GPIOs and the leased drive-control loop.
//!
//! The bus-facing half of [`ezal_core::drive`]: it owns the four transistor
//! switch GPIOs, feeds every requested command through the shared
//! [`ActuatorGuard`] so relays never chatter, and enforces the movement lease.
//! Whether commands come from autonomous tracking or held dashboard buttons,
//! this task drops the outputs back to all-low if their refresh stream stops.

use embassy_rp::gpio::Output;
use embassy_time::{with_deadline, Instant};

use ezal_core::drive::{ActuatorGuard, AzimuthDirection, DriveCommand, ElevationDirection};

use crate::state::SharedState;

/// The four GPIO outputs that drive the G-5500 direction switch transistors.
pub struct DirectionOutputs {
    cw: Output<'static>,
    ccw: Output<'static>,
    up: Output<'static>,
    down: Output<'static>,
    applied: DriveCommand,
}

impl DirectionOutputs {
    /// Wrap the four idle-low direction GPIOs, ordered CW, CCW, UP, DOWN to
    /// match the schematic. Starts with no axis energised.
    pub fn new(
        cw: Output<'static>,
        ccw: Output<'static>,
        up: Output<'static>,
        down: Output<'static>,
    ) -> Self {
        Self {
            cw,
            ccw,
            up,
            down,
            applied: DriveCommand::IDLE,
        }
    }

    fn apply_drive(&mut self, drive: DriveCommand) {
        if self.applied.azimuth != drive.azimuth {
            match self.applied.azimuth {
                Some(AzimuthDirection::Clockwise) => self.cw.set_low(),
                Some(AzimuthDirection::CounterClockwise) => self.ccw.set_low(),
                None => {}
            }

            match drive.azimuth {
                Some(AzimuthDirection::Clockwise) => self.cw.set_high(),
                Some(AzimuthDirection::CounterClockwise) => self.ccw.set_high(),
                None => {}
            }
        }

        if self.applied.elevation != drive.elevation {
            match self.applied.elevation {
                Some(ElevationDirection::Up) => self.up.set_low(),
                Some(ElevationDirection::Down) => self.down.set_low(),
                None => {}
            }

            match drive.elevation {
                Some(ElevationDirection::Up) => self.up.set_high(),
                Some(ElevationDirection::Down) => self.down.set_high(),
                None => {}
            }
        }

        self.applied = drive;
    }

    fn stop(&mut self) {
        self.cw.set_low();
        self.ccw.set_low();
        self.up.set_low();
        self.down.set_low();
        self.applied = DriveCommand::IDLE;
    }
}

/// Owns and applies direction GPIO commands. Movement is lease-based: a command
/// source must repeat an active state, and this task drops back to all-low if
/// that refresh stream stops.
#[embassy_executor::task]
pub async fn direction_task(mut outputs: DirectionOutputs, state: &'static SharedState) -> ! {
    outputs.stop();
    let mut guard = ActuatorGuard::new();
    state.record_applied_drive(guard.actual());

    loop {
        let now = Instant::now();
        apply_guarded_drive(&mut outputs, state, &mut guard, now.as_millis());

        let next_deadline = guard
            .next_transition_ms(now.as_millis())
            .map(Instant::from_millis);
        let command = match next_deadline {
            Some(deadline) => with_deadline(deadline, state.wait_command()).await.ok(),
            None => Some(state.wait_command().await),
        };

        let now = Instant::now();
        if let Some(command) = command {
            if guard.command(command, now.as_millis()) {
                outputs.apply_drive(guard.actual());
                state.record_applied_drive(guard.actual());
            }
            continue;
        }
    }
}

fn apply_guarded_drive(
    outputs: &mut DirectionOutputs,
    state: &'static SharedState,
    guard: &mut ActuatorGuard,
    now_ms: u64,
) {
    if guard.update(now_ms) {
        outputs.apply_drive(guard.actual());
        state.record_applied_drive(guard.actual());
    }
}
