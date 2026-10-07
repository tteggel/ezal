//! Direction-output GPIOs and the leased drive-control loop.
//!
//! The bus-facing half of [`ezal_core::actuator`]: it owns the four transistor
//! switch GPIOs and applies whatever the shared [`Actuator`] permits. That type
//! holds the permission, command, and timer ordering rules. This task copies
//! coherent inputs, steps that policy, applies GPIOs, then feeds the hardware
//! watchdog. Its wait is bounded by the next policy deadline and a fixed
//! service period.

use embassy_rp::gpio::Output;
use embassy_rp::watchdog::Watchdog;
use embassy_time::{with_deadline, Duration, Instant};

use ezal_core::actuator::Actuator;
use ezal_core::drive::{AzimuthDirection, DriveCommand, ElevationDirection};

use crate::state::SharedState;

/// Longest wait between service passes when nothing else is pending. Each pass
/// re-checks feedback and feeds the watchdog, so it must stay well inside
/// [`crate::watchdog::WATCHDOG_TIMEOUT`].
const SERVICE_PERIOD: Duration = Duration::from_millis(100);

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
pub async fn direction_task(
    mut outputs: DirectionOutputs,
    state: &'static SharedState,
    mut watchdog: Watchdog,
) -> ! {
    outputs.stop();
    let mut actuator = Actuator::new();
    state.record_actuator(actuator.actual(), actuator.fault());
    crate::watchdog::start(&mut watchdog);
    let mut next_service = Instant::now();

    loop {
        let now = Instant::now();
        let input = state.take_actuator_input(now.as_millis());
        let update = actuator.step(input, now.as_millis());
        outputs.apply_drive(update.output);
        state.record_actuator(update.output, update.status.motion_fault);

        // Feeding from this loop proves the actuator task ran and serviced the
        // output boundary; a separate heartbeat task could hide a stall here.
        watchdog.feed(crate::watchdog::WATCHDOG_TIMEOUT);

        // A fixed service schedule, rather than one re-armed from every wake,
        // keeps an idle board at a known small number of passes per second.
        while next_service <= now {
            next_service += SERVICE_PERIOD;
        }
        let deadline = update
            .next_deadline_ms
            .map(Instant::from_millis)
            .map_or(next_service, |deadline| deadline.min(next_service));

        let _ = with_deadline(deadline, state.wait_changed()).await;
    }
}
