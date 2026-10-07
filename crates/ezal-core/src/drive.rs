//! Rotator drive model and actuator-safety timing.
//!
//! The two independent axes of the G-5500 rotator — azimuth and elevation —
//! and the [`Command`]s that move them, expressed independently of any
//! transport. Autonomous control and the hardware dashboard both speak the
//! same [`DriveCommand`] without knowing anything about GPIOs.
//!
//! [`Debouncer`] enforces ordinary relay minimum on/off intervals.
//! [`ActuatorGuard`] adds movement leases and safety inhibition, which can
//! interrupt the minimum on-time. Reactivation still respects the minimum
//! off-time, and a leased output starts only when its lease covers the minimum
//! on-time. [`crate::actuator`] orders these checks with the feedback
//! interlock; the GPIO adapter lives in firmware's `drive` module.

/// Minimum ordinary on-time; safety inhibition and operator release bypass it.
pub const OUTPUT_MIN_ACTIVE_MS: u64 = 500;

/// Minimum time an axis stays inactive before it may be energised again.
pub const OUTPUT_MIN_INACTIVE_MS: u64 = 2_000;

/// Maximum lifetime of a movement command that has not been refreshed.
pub const MOVEMENT_LEASE_MS: u64 = 750;

/// Fastest azimuth travel assumed while an output is energised: the G-5500
/// manual's 58 s per 360° at 60 Hz (6.2°/s), plus margin.
pub const AZIMUTH_MAX_SPEED_DEG_PER_S: f32 = 7.0;

/// Fastest elevation travel assumed while an output is energised: the G-5500
/// manual's 67 s per 180° at 60 Hz (2.7°/s), plus margin.
pub const ELEVATION_MAX_SPEED_DEG_PER_S: f32 = 3.0;

/// Azimuth motion carried by a drive command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AzimuthDirection {
    /// Clockwise azimuth movement.
    Clockwise,
    /// Counter-clockwise azimuth movement.
    CounterClockwise,
}

impl AzimuthDirection {
    /// Whether this direction moves toward the calibrated maximum azimuth.
    pub const fn increases_angle(self) -> bool {
        matches!(self, Self::Clockwise)
    }
}

/// Elevation motion carried by a drive command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElevationDirection {
    /// Upward elevation movement.
    Up,
    /// Downward elevation movement.
    Down,
}

impl ElevationDirection {
    /// Whether this direction moves toward the calibrated maximum elevation.
    pub const fn increases_angle(self) -> bool {
        matches!(self, Self::Up)
    }
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
    /// Move either or both axes under controller timing. The command source
    /// must refresh the lease while requesting movement. An axis the command
    /// no longer requests is released after its minimum on-time.
    Drive(DriveCommand),
    /// Operator-held movement, leased like [`Command::Drive`]. An energised
    /// axis the operator releases or reverses stops immediately; reactivation
    /// still waits for the minimum inactive time.
    Jog(DriveCommand),
    /// Ordinary controller release, respecting relay minimum active time.
    Stop,
    /// Immediately remove motion for a fault, expired lease, or operator stop.
    /// Unlike ordinary release, safety inhibition bypasses minimum active time.
    Inhibit,
}

/// Minimum active/inactive-time filter for the four physical direction outputs.
///
/// Ordinary releases respect [`OUTPUT_MIN_ACTIVE_MS`]; [`Command::Inhibit`]
/// and operator releases through [`Command::Jog`] remove active outputs
/// immediately. All reactivation and reversal respects
/// [`OUTPUT_MIN_INACTIVE_MS`]. The firmware applies this pure state machine's
/// resulting outputs at the GPIO boundary.
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
        self.set_target_until(command, now_ms, u64::MAX)
    }

    /// Like [`set_target`](Self::set_target), but start an output only if it
    /// can stay on for its minimum active time by `activation_limit_ms`.
    fn set_target_until(
        &mut self,
        command: Command,
        now_ms: u64,
        activation_limit_ms: u64,
    ) -> bool {
        let before = self.actual;
        match command {
            Command::Inhibit => {
                self.inhibit_axes(true, true, now_ms);
            }
            Command::Jog(drive) => {
                // Releasing a held control cuts that output at once. Its
                // inactive hold still delays the next activation.
                self.inhibit_axes(
                    self.actual.azimuth.is_some() && self.actual.azimuth != drive.azimuth,
                    self.actual.elevation.is_some() && self.actual.elevation != drive.elevation,
                    now_ms,
                );
                self.target = drive;
                self.update_until(now_ms, activation_limit_ms);
            }
            Command::Drive(drive) => {
                self.target = drive;
                self.update_until(now_ms, activation_limit_ms);
            }
            Command::Stop => {
                self.target = DriveCommand::IDLE;
                self.update_until(now_ms, activation_limit_ms);
            }
        }
        self.actual != before
    }

    fn inhibit_axes(&mut self, azimuth: bool, elevation: bool, now_ms: u64) -> bool {
        let before = self.actual;
        if azimuth {
            self.target.azimuth = None;
            if self.actual.azimuth.take().is_some() {
                self.azimuth_off_until_ms = now_ms.saturating_add(OUTPUT_MIN_INACTIVE_MS);
            }
        }
        if elevation {
            self.target.elevation = None;
            if self.actual.elevation.take().is_some() {
                self.elevation_off_until_ms = now_ms.saturating_add(OUTPUT_MIN_INACTIVE_MS);
            }
        }
        self.actual != before
    }

    /// Advance the debounced output state at `now_ms`.
    ///
    /// Returns `true` when [`actual`](Self::actual) changed.
    pub fn update(&mut self, now_ms: u64) -> bool {
        self.update_until(now_ms, u64::MAX)
    }

    fn update_until(&mut self, now_ms: u64, activation_limit_ms: u64) -> bool {
        let before = self.actual;
        self.update_azimuth(now_ms, activation_limit_ms);
        self.update_elevation(now_ms, activation_limit_ms);
        self.actual != before
    }

    /// Next millisecond instant where a delayed target may become applicable.
    pub fn next_transition_ms(&self, now_ms: u64) -> Option<u64> {
        self.next_transition_until(now_ms, u64::MAX)
    }

    /// Next transition, excluding activations that `activation_limit_ms`
    /// would refuse. Such an activation waits for a later limit, not time.
    fn next_transition_until(&self, now_ms: u64, activation_limit_ms: u64) -> Option<u64> {
        let mut next = None;

        if self.actual.azimuth != self.target.azimuth {
            let deadline = match self.actual.azimuth {
                Some(direction) => Some(self.azimuth_until_ms(direction).max(now_ms)),
                None => activation_ms(self.azimuth_off_until_ms, now_ms, activation_limit_ms),
            };
            if let Some(deadline) = deadline {
                next = Some(min_option(next, deadline));
            }
        }

        if self.actual.elevation != self.target.elevation {
            let deadline = match self.actual.elevation {
                Some(direction) => Some(self.elevation_until_ms(direction).max(now_ms)),
                None => activation_ms(self.elevation_off_until_ms, now_ms, activation_limit_ms),
            };
            if let Some(deadline) = deadline {
                next = Some(min_option(next, deadline));
            }
        }

        next
    }

    fn update_azimuth(&mut self, now_ms: u64, activation_limit_ms: u64) {
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

        let until_ms = now_ms.saturating_add(OUTPUT_MIN_ACTIVE_MS);
        if let Some(target) = self.target.azimuth {
            if until_ms <= activation_limit_ms {
                self.actual.azimuth = Some(target);
                self.set_azimuth_until_ms(target, until_ms);
            }
        }
    }

    fn update_elevation(&mut self, now_ms: u64, activation_limit_ms: u64) {
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

        let until_ms = now_ms.saturating_add(OUTPUT_MIN_ACTIVE_MS);
        if let Some(target) = self.target.elevation {
            if until_ms <= activation_limit_ms {
                self.actual.elevation = Some(target);
                self.set_elevation_until_ms(target, until_ms);
            }
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

/// Fail-safe movement lease wrapped around the relay-protecting debouncer.
///
/// Every active command must be refreshed before [`MOVEMENT_LEASE_MS`]
/// elapses. Expiry clears both requested and applied movement immediately,
/// including a delayed activation. An output starts only when its lease leaves
/// at least [`OUTPUT_MIN_ACTIVE_MS`]; otherwise the activation waits for a
/// refresh. Expiry therefore never shortens a minimum on-time, and a stalled
/// command stream cannot produce a relay click too short to move the rotator.
/// The firmware services these deadlines; host tests exercise their
/// interaction with relay timing deterministically.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActuatorGuard {
    debouncer: Debouncer,
    lease_deadline_ms: Option<u64>,
}

impl ActuatorGuard {
    /// Create an idle guard with no active lease.
    pub const fn new() -> Self {
        Self {
            debouncer: Debouncer::new(),
            lease_deadline_ms: None,
        }
    }

    /// Accept a command and refresh its movement lease when it is active.
    ///
    /// Returns `true` when the physically applicable drive state changed.
    pub fn command(&mut self, command: Command, now_ms: u64) -> bool {
        self.command_until(command, now_ms, u64::MAX)
    }

    /// Accept a command bounded by its source's absolute expiry. Neither
    /// transport delivery nor time spent in a mailbox renews that expiry.
    /// Expired movement inhibits immediately, including pending activations;
    /// releases and inhibition are always admitted regardless of their age.
    pub fn command_until(&mut self, command: Command, now_ms: u64, valid_until_ms: u64) -> bool {
        let before = self.actual();
        // A late refresh must not erase an expiry. Do not advance pending
        // activation first: this command may cancel it at its deadline.
        self.expire_lease(now_ms);
        let mut command = command;
        if let Command::Drive(drive) | Command::Jog(drive) = command {
            if drive.is_active() {
                let deadline_ms = now_ms.saturating_add(MOVEMENT_LEASE_MS).min(valid_until_ms);
                if deadline_ms <= now_ms {
                    command = Command::Inhibit;
                } else {
                    self.lease_deadline_ms = Some(deadline_ms);
                }
            }
        }
        self.debouncer
            .set_target_until(command, now_ms, self.activation_limit_ms());
        // Ordinary release may leave an axis on until minimum-on ends. Its
        // existing movement lease must still bound that remaining motion.
        self.clear_idle_lease();
        self.actual() != before
    }

    /// Advance lease and debounce timers.
    ///
    /// Lease expiry is handled before the debouncer is advanced, so an
    /// unrefreshed output can never be re-energised by a pending transition.
    pub fn update(&mut self, now_ms: u64) -> bool {
        let changed = self.expire_lease(now_ms)
            || self
                .debouncer
                .update_until(now_ms, self.activation_limit_ms());
        self.clear_idle_lease();
        changed
    }

    fn expire_lease(&mut self, now_ms: u64) -> bool {
        let expired = self
            .lease_deadline_ms
            .is_some_and(|deadline| deadline <= now_ms);
        if expired {
            self.lease_deadline_ms = None;
            self.debouncer.set_target(Command::Inhibit, now_ms)
        } else {
            false
        }
    }

    /// Latest instant a newly started output may need; none without a lease.
    fn activation_limit_ms(&self) -> u64 {
        self.lease_deadline_ms.unwrap_or(0)
    }

    fn clear_idle_lease(&mut self) {
        if !self.target().is_active() && !self.actual().is_active() {
            self.lease_deadline_ms = None;
        }
    }

    /// Immediately inhibit selected axes, including any pending activation.
    /// The other axis keeps its command and lease. Repeated inhibition does not
    /// extend an already-inactive axis's hold; reactivation still waits 2 s.
    pub fn inhibit_axes(&mut self, azimuth: bool, elevation: bool, now_ms: u64) -> bool {
        let changed = self.debouncer.inhibit_axes(azimuth, elevation, now_ms);
        self.clear_idle_lease();
        changed
    }

    /// Drive state currently permitted at the physical output boundary.
    pub const fn actual(&self) -> DriveCommand {
        self.debouncer.actual()
    }

    /// Drive state most recently requested, after any lease expiry.
    pub const fn target(&self) -> DriveCommand {
        self.debouncer.target()
    }

    /// Current movement-lease deadline, if an active command owns one.
    pub const fn lease_deadline_ms(&self) -> Option<u64> {
        self.lease_deadline_ms
    }

    /// Next instant at which the guard may change its output state. An
    /// activation the lease cannot cover waits for a refresh or the expiry.
    pub fn next_transition_ms(&self, now_ms: u64) -> Option<u64> {
        let transition = self
            .debouncer
            .next_transition_until(now_ms, self.activation_limit_ms());
        match (transition, self.lease_deadline_ms) {
            (Some(a), Some(b)) => Some(if a <= b { a } else { b }),
            (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
            (None, None) => None,
        }
    }
}

impl Default for ActuatorGuard {
    fn default() -> Self {
        Self::new()
    }
}

/// When a pending activation may start, if the limit covers its on-time.
fn activation_ms(off_until_ms: u64, now_ms: u64, activation_limit_ms: u64) -> Option<u64> {
    let start_ms = off_until_ms.max(now_ms);
    (start_ms.saturating_add(OUTPUT_MIN_ACTIVE_MS) <= activation_limit_ms).then_some(start_ms)
}

const fn min_option(current: Option<u64>, candidate: u64) -> u64 {
    match current {
        Some(current) if current < candidate => current,
        _ => candidate,
    }
}
