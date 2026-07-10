//! Rotator drive model and actuator-safety timing.
//!
//! The two independent axes of the G-5500 rotator — azimuth and elevation —
//! and the [`Command`]s that move them, expressed independently of any
//! transport. The dashboard WebSocket is the only command source today, but
//! the eventual USB-serial link will speak the same [`DriveCommand`] without
//! knowing anything about the web.
//!
//! Alongside the vocabulary sits [`Debouncer`], the pure state machine that
//! stops the relays and geartrain from seeing sub-[`OUTPUT_MIN_ACTIVE_MS`]
//! on/off chatter. The GPIO layer that actually energises the switch
//! transistors — the bus-facing half — lives in the firmware crate's `drive`
//! module and feeds every command through this filter at the pin boundary.

/// Minimum time a direction output stays active once energised.
pub const OUTPUT_MIN_ACTIVE_MS: u64 = 500;

/// Minimum time an axis stays inactive before it may be energised again.
pub const OUTPUT_MIN_INACTIVE_MS: u64 = OUTPUT_MIN_ACTIVE_MS;

/// Azimuth motion carried by a drive command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AzimuthDirection {
    /// Clockwise azimuth movement.
    Clockwise,
    /// Counter-clockwise azimuth movement.
    CounterClockwise,
}

/// Elevation motion carried by a drive command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElevationDirection {
    /// Upward elevation movement.
    Up,
    /// Downward elevation movement.
    Down,
}

/// Simultaneous drive state for the two independent rotator axes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DriveCommand {
    /// Azimuth direction, or `None` when azimuth is idle.
    pub azimuth: Option<AzimuthDirection>,
    /// Elevation direction, or `None` when elevation is idle.
    pub elevation: Option<ElevationDirection>,
}

impl DriveCommand {
    /// No axis is moving.
    pub const IDLE: Self = Self {
        azimuth: None,
        elevation: None,
    };

    /// Whether either axis is moving.
    pub const fn is_active(&self) -> bool {
        self.azimuth.is_some() || self.elevation.is_some()
    }
}

/// A command to the rotator drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Move either or both axes. The firmware treats this as a lease that must
    /// be refreshed by the client until it sends [`Stop`](Self::Stop).
    Drive(DriveCommand),
    /// Stop all direction outputs.
    Stop,
}

/// Minimum active/inactive-time filter for the four physical direction outputs.
///
/// The command stream is intentionally responsive, but the relay and geartrain
/// should not see sub-[`OUTPUT_MIN_ACTIVE_MS`] on/off or off/on chatter. This
/// pure state machine lets firmware enforce that rule at the GPIO boundary
/// while tests exercise the timing behaviour on the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Debouncer {
    target: DriveCommand,
    actual: DriveCommand,
    cw_until_ms: u64,
    ccw_until_ms: u64,
    azimuth_off_until_ms: u64,
    up_until_ms: u64,
    down_until_ms: u64,
    elevation_off_until_ms: u64,
}

impl Debouncer {
    /// Create an idle debouncer.
    pub const fn new() -> Self {
        Self {
            target: DriveCommand::IDLE,
            actual: DriveCommand::IDLE,
            cw_until_ms: 0,
            ccw_until_ms: 0,
            azimuth_off_until_ms: 0,
            up_until_ms: 0,
            down_until_ms: 0,
            elevation_off_until_ms: 0,
        }
    }

    /// The currently requested drive state.
    pub const fn target(&self) -> DriveCommand {
        self.target
    }

    /// The drive state that is allowed to be physically applied now.
    pub const fn actual(&self) -> DriveCommand {
        self.actual
    }

    /// Set a new target command and advance the debounced output state.
    ///
    /// Returns `true` when [`actual`](Self::actual) changed.
    pub fn set_target(&mut self, command: Command, now_ms: u64) -> bool {
        self.target = match command {
            Command::Drive(drive) => drive,
            Command::Stop => DriveCommand::IDLE,
        };
        self.update(now_ms)
    }

    /// Advance the debounced output state at `now_ms`.
    ///
    /// Returns `true` when [`actual`](Self::actual) changed.
    pub fn update(&mut self, now_ms: u64) -> bool {
        let before = self.actual;
        self.update_azimuth(now_ms);
        self.update_elevation(now_ms);
        self.actual != before
    }

    /// Next millisecond instant where a delayed target may become applicable.
    pub fn next_transition_ms(&self, now_ms: u64) -> Option<u64> {
        let mut next = None;

        if self.actual.azimuth != self.target.azimuth {
            let deadline = match self.actual.azimuth {
                Some(direction) => self.azimuth_until_ms(direction),
                None => self.azimuth_off_until_ms,
            };
            next = Some(min_option(next, deadline.max(now_ms)));
        }

        if self.actual.elevation != self.target.elevation {
            let deadline = match self.actual.elevation {
                Some(direction) => self.elevation_until_ms(direction),
                None => self.elevation_off_until_ms,
            };
            next = Some(min_option(next, deadline.max(now_ms)));
        }

        next
    }

    fn update_azimuth(&mut self, now_ms: u64) {
        if self.actual.azimuth == self.target.azimuth {
            return;
        }

        if let Some(current) = self.actual.azimuth {
            if now_ms < self.azimuth_until_ms(current) {
                return;
            }
            self.actual.azimuth = None;
            self.azimuth_off_until_ms = now_ms.saturating_add(OUTPUT_MIN_INACTIVE_MS);
        }

        if now_ms < self.azimuth_off_until_ms {
            return;
        }

        if let Some(target) = self.target.azimuth {
            self.actual.azimuth = Some(target);
            self.set_azimuth_until_ms(target, now_ms.saturating_add(OUTPUT_MIN_ACTIVE_MS));
        }
    }

    fn update_elevation(&mut self, now_ms: u64) {
        if self.actual.elevation == self.target.elevation {
            return;
        }

        if let Some(current) = self.actual.elevation {
            if now_ms < self.elevation_until_ms(current) {
                return;
            }
            self.actual.elevation = None;
            self.elevation_off_until_ms = now_ms.saturating_add(OUTPUT_MIN_INACTIVE_MS);
        }

        if now_ms < self.elevation_off_until_ms {
            return;
        }

        if let Some(target) = self.target.elevation {
            self.actual.elevation = Some(target);
            self.set_elevation_until_ms(target, now_ms.saturating_add(OUTPUT_MIN_ACTIVE_MS));
        }
    }

    const fn azimuth_until_ms(&self, direction: AzimuthDirection) -> u64 {
        match direction {
            AzimuthDirection::Clockwise => self.cw_until_ms,
            AzimuthDirection::CounterClockwise => self.ccw_until_ms,
        }
    }

    fn set_azimuth_until_ms(&mut self, direction: AzimuthDirection, until_ms: u64) {
        match direction {
            AzimuthDirection::Clockwise => self.cw_until_ms = until_ms,
            AzimuthDirection::CounterClockwise => self.ccw_until_ms = until_ms,
        }
    }

    const fn elevation_until_ms(&self, direction: ElevationDirection) -> u64 {
        match direction {
            ElevationDirection::Up => self.up_until_ms,
            ElevationDirection::Down => self.down_until_ms,
        }
    }

    fn set_elevation_until_ms(&mut self, direction: ElevationDirection, until_ms: u64) {
        match direction {
            ElevationDirection::Up => self.up_until_ms = until_ms,
            ElevationDirection::Down => self.down_until_ms = until_ms,
        }
    }
}

impl Default for Debouncer {
    fn default() -> Self {
        Self::new()
    }
}

const fn min_option(current: Option<u64>, candidate: u64) -> u64 {
    match current {
        Some(current) if current < candidate => current,
        _ => candidate,
    }
}
