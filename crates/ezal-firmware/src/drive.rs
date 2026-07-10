//! Direction-output GPIOs and the leased drive-control loop.
//!
//! The bus-facing half of [`ezal_core::drive`]: it owns the four transistor
//! switch GPIOs, feeds every requested [`Command`] through a [`Debouncer`] so
//! the relays never chatter, and enforces the movement *lease*. The browser
//! repeats the active two-axis drive state while buttons are held, and this
//! task drops the outputs back to all-low if that refresh stream stops.

use embassy_rp::gpio::Output;
use embassy_time::{with_deadline, Duration, Instant};

use ezal_core::drive::{AzimuthDirection, Command, Debouncer, DriveCommand, ElevationDirection};
use ezal_core::protocol::COMMAND_TIMEOUT_MS;

use crate::state::SharedState;

/// Maximum lifetime of an unrefreshed drive-state command.
const DIRECTION_TIMEOUT: Duration = Duration::from_millis(COMMAND_TIMEOUT_MS);

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

/// Owns and applies direction GPIO commands. Movement is lease-based: the
/// browser repeats the active two-axis drive state while buttons are held, and
/// this task drops back to all-low if that refresh stream stops.
#[embassy_executor::task]
pub async fn direction_task(mut outputs: DirectionOutputs, state: &'static SharedState) -> ! {
    outputs.stop();
    let mut debouncer = Debouncer::new();
    let mut lease_deadline = None;
    state.record_applied_drive(debouncer.actual());

    loop {
        let now = Instant::now();
        apply_debounced_drive(&mut outputs, state, &mut debouncer, now.as_millis());

        let next_deadline = next_direction_deadline(&debouncer, lease_deadline, now);
        let command = match next_deadline {
            Some(deadline) => with_deadline(deadline, state.wait_command()).await.ok(),
            None => Some(state.wait_command().await),
        };

        let now = Instant::now();
        if let Some(command) = command {
            lease_deadline = match command {
                Command::Drive(drive) if drive.is_active() => Some(now + DIRECTION_TIMEOUT),
                Command::Drive(_) | Command::Stop => None,
            };
            if debouncer.set_target(command, now.as_millis()) {
                outputs.apply_drive(debouncer.actual());
                state.record_applied_drive(debouncer.actual());
            }
            continue;
        }

        if let Some(deadline) = lease_deadline {
            if deadline <= now {
                lease_deadline = None;
                if debouncer.set_target(Command::Stop, now.as_millis()) {
                    outputs.apply_drive(debouncer.actual());
                    state.record_applied_drive(debouncer.actual());
                }
            }
        }
    }
}

fn apply_debounced_drive(
    outputs: &mut DirectionOutputs,
    state: &'static SharedState,
    debouncer: &mut Debouncer,
    now_ms: u64,
) {
    if debouncer.update(now_ms) {
        outputs.apply_drive(debouncer.actual());
        state.record_applied_drive(debouncer.actual());
    }
}

fn next_direction_deadline(
    debouncer: &Debouncer,
    lease_deadline: Option<Instant>,
    now: Instant,
) -> Option<Instant> {
    let debounce_deadline = debouncer
        .next_transition_ms(now.as_millis())
        .map(Instant::from_millis);

    match (debounce_deadline, lease_deadline) {
        (Some(a), Some(b)) => Some(if a <= b { a } else { b }),
        (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
        (None, None) => None,
    }
}
