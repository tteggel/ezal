//! Rotator drive model and actuator-safety timing.
//!
//! The two independent axes of the G-5500 rotator — azimuth and elevation —
//! and the [`Command`]s that move them, expressed independently of any
//! transport. Manual input uses [`DriveCommand`] without knowing anything
//! about GPIOs. A [`CommandEnvelope`] carries a command with the expiry its
//! source assigned.
//!
//! Private relay timing enforces ordinary minimum on/off intervals.
//! The actuator's lease guard adds movement expiry and safety inhibition, which can
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

/// Ordinary relay timing, independent of source leases and revocable permissions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RelayConfig {
    /// Minimum ordinary energized interval; inhibition still cuts immediately.
    pub min_active_ms: u64,
    /// Minimum off-time before reusing the last energized direction.
    pub same_direction_off_ms: u64,
    /// Minimum off-time before energizing the opposite direction.
    pub reversal_off_ms: u64,
}

impl RelayConfig {
    /// Installed relay timing, which the firmware runs. Commissioning must
    /// validate any change.
    pub const DEFAULT: Self = Self {
        min_active_ms: OUTPUT_MIN_ACTIVE_MS,
        same_direction_off_ms: OUTPUT_MIN_INACTIVE_MS,
        reversal_off_ms: OUTPUT_MIN_INACTIVE_MS,
    };

    /// Shorter tracking timing, used only in simulation and commissioning.
    pub const TRACKING_CANDIDATE: Self = Self {
        min_active_ms: 500,
        same_direction_off_ms: 500,
        reversal_off_ms: 1_000,
    };

    /// The movement lease must cover an ordinary pulse, with a real off-time.
    pub const fn is_valid(self) -> bool {
        self.min_active_ms > 0
            && self.min_active_ms <= MOVEMENT_LEASE_MS
            && self.same_direction_off_ms > 0
            && self.reversal_off_ms >= self.same_direction_off_ms
    }
}

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
    /// Immediately remove motion for a fault, expired lease, or operator stop.
    /// Unlike ordinary release, safety inhibition bypasses minimum active time.
    Inhibit,
}

/// A request and the original exclusive expiry assigned at admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandEnvelope {
    /// Operator jog or immediate safety inhibition.
    pub command: Command,
    /// Monotonic source deadline; unused for inhibition.
    pub valid_until_ms: u64,
}

impl CommandEnvelope {
    /// Immediate inhibition has no movement lease.
    pub const INHIBIT: Self = Self {
        command: Command::Inhibit,
        valid_until_ms: 0,
    };
}

/// One axis's relay state. Only the applied direction owns a minimum-on
/// deadline; both directions share the inactive hold before reactivation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AxisRelay<D> {
    config: RelayConfig,
    target: Option<D>,
    actual: Option<D>,
    last_direction: Option<D>,
    on_until_ms: u64,
    off_until_ms: u64,
    reverse_until_ms: u64,
}

impl<D: Copy + Eq> AxisRelay<D> {
    const fn new(config: RelayConfig) -> Self {
        Self {
            config,
            target: None,
            actual: None,
            last_direction: None,
            on_until_ms: 0,
            off_until_ms: 0,
            reverse_until_ms: 0,
        }
    }

    fn release(&mut self, now_ms: u64) {
        if let Some(direction) = self.actual.take() {
            self.last_direction = Some(direction);
            self.off_until_ms = now_ms.saturating_add(self.config.same_direction_off_ms);
            self.reverse_until_ms = now_ms.saturating_add(self.config.reversal_off_ms);
        }
    }

    fn inactive_until_ms(&self) -> u64 {
        if self.last_direction.is_some() && self.target != self.last_direction {
            self.reverse_until_ms
        } else {
            self.off_until_ms
        }
    }

    fn inhibit(&mut self, now_ms: u64) {
        self.target = None;
        self.release(now_ms);
    }

    fn update(&mut self, now_ms: u64, activation_limit_ms: u64) {
        if self.actual == self.target {
            return;
        }
        if self.actual.is_some() {
            if now_ms < self.on_until_ms {
                return;
            }
            self.release(now_ms);
        }
        if now_ms < self.inactive_until_ms() {
            return;
        }
        let Some(on_until_ms) = now_ms.checked_add(self.config.min_active_ms) else {
            return;
        };
        if self.target.is_some() && on_until_ms <= activation_limit_ms {
            self.actual = self.target;
            self.on_until_ms = on_until_ms;
        }
    }

    fn next_transition_ms(&self, now_ms: u64, activation_limit_ms: u64) -> Option<u64> {
        if self.actual == self.target {
            None
        } else if self.actual.is_some() {
            Some(self.on_until_ms.max(now_ms))
        } else {
            activation_ms(
                self.inactive_until_ms(),
                now_ms,
                activation_limit_ms,
                self.config.min_active_ms,
            )
        }
    }
}

/// Relay timing for two independent axes. Ordinary release respects minimum
/// on-time; inhibition and operator release cut at once. Every activation
/// respects the shared off-time for its axis and the available command lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RelayTiming {
    azimuth: AxisRelay<AzimuthDirection>,
    elevation: AxisRelay<ElevationDirection>,
}

impl RelayTiming {
    const fn new(config: RelayConfig) -> Self {
        Self {
            azimuth: AxisRelay::new(config),
            elevation: AxisRelay::new(config),
        }
    }

    const fn target(&self) -> DriveCommand {
        DriveCommand {
            azimuth: self.azimuth.target,
            elevation: self.elevation.target,
        }
    }

    const fn actual(&self) -> DriveCommand {
        DriveCommand {
            azimuth: self.azimuth.actual,
            elevation: self.elevation.actual,
        }
    }

    fn set_target(&mut self, command: Command, now_ms: u64) -> bool {
        self.set_target_until(command, now_ms, u64::MAX)
    }

    fn set_target_until(
        &mut self,
        command: Command,
        now_ms: u64,
        activation_limit_ms: u64,
    ) -> bool {
        let before = self.actual();
        let drive = match command {
            Command::Inhibit => {
                return self.inhibit_axes(true, true, now_ms);
            }
            Command::Jog(drive) => {
                self.inhibit_axes(
                    self.azimuth.actual.is_some() && self.azimuth.actual != drive.azimuth,
                    self.elevation.actual.is_some() && self.elevation.actual != drive.elevation,
                    now_ms,
                );
                drive
            }
            Command::Drive(drive) => drive,
        };
        self.azimuth.target = drive.azimuth;
        self.elevation.target = drive.elevation;
        self.update_until(now_ms, activation_limit_ms);
        self.actual() != before
    }

    fn inhibit_axes(&mut self, azimuth: bool, elevation: bool, now_ms: u64) -> bool {
        let before = self.actual();
        if azimuth {
            self.azimuth.inhibit(now_ms);
        }
        if elevation {
            self.elevation.inhibit(now_ms);
        }
        self.actual() != before
    }

    fn update_until(&mut self, now_ms: u64, activation_limit_ms: u64) -> bool {
        let before = self.actual();
        self.azimuth.update(now_ms, activation_limit_ms);
        self.elevation.update(now_ms, activation_limit_ms);
        self.actual() != before
    }

    fn next_transition_until(&self, now_ms: u64, activation_limit_ms: u64) -> Option<u64> {
        [
            self.azimuth.next_transition_ms(now_ms, activation_limit_ms),
            self.elevation
                .next_transition_ms(now_ms, activation_limit_ms),
        ]
        .into_iter()
        .flatten()
        .min()
    }
}

/// Fail-safe movement lease around the two-axis relay timing.
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
pub(crate) struct ActuatorGuard {
    relays: RelayTiming,
    lease_deadline_ms: Option<u64>,
}

impl ActuatorGuard {
    /// Create an idle guard with validated installation timing.
    pub const fn new(config: RelayConfig) -> Self {
        Self {
            relays: RelayTiming::new(config),
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
        self.relays
            .set_target_until(command, now_ms, self.activation_limit_ms());
        // Ordinary release may leave an axis on until minimum-on ends. Its
        // existing movement lease must still bound that remaining motion.
        self.clear_idle_lease();
        self.actual() != before
    }

    /// Advance lease and relay timers.
    ///
    /// Lease expiry is handled before relay timing advances, so an
    /// unrefreshed output can never be re-energised by a pending transition.
    pub fn update(&mut self, now_ms: u64) -> bool {
        let changed = self.expire_lease(now_ms)
            || self.relays.update_until(now_ms, self.activation_limit_ms());
        self.clear_idle_lease();
        changed
    }

    fn expire_lease(&mut self, now_ms: u64) -> bool {
        let expired = self
            .lease_deadline_ms
            .is_some_and(|deadline| deadline <= now_ms);
        if expired {
            self.lease_deadline_ms = None;
            self.relays.set_target(Command::Inhibit, now_ms)
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
        let changed = self.relays.inhibit_axes(azimuth, elevation, now_ms);
        self.clear_idle_lease();
        changed
    }

    /// Drive state currently permitted at the physical output boundary.
    pub const fn actual(&self) -> DriveCommand {
        self.relays.actual()
    }

    /// Drive state most recently requested, after any lease expiry.
    pub const fn target(&self) -> DriveCommand {
        self.relays.target()
    }

    /// Current movement-lease deadline, if an active command owns one.
    pub const fn lease_deadline_ms(&self) -> Option<u64> {
        self.lease_deadline_ms
    }

    /// Next instant at which the guard may change its output state. An
    /// activation the lease cannot cover waits for a refresh or the expiry.
    pub fn next_transition_ms(&self, now_ms: u64) -> Option<u64> {
        let transition = self
            .relays
            .next_transition_until(now_ms, self.activation_limit_ms());
        match (transition, self.lease_deadline_ms) {
            (Some(a), Some(b)) => Some(if a <= b { a } else { b }),
            (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
            (None, None) => None,
        }
    }
}

/// When a pending activation may start, if the limit covers its on-time.
fn activation_ms(
    off_until_ms: u64,
    now_ms: u64,
    activation_limit_ms: u64,
    min_active_ms: u64,
) -> Option<u64> {
    let start_ms = off_until_ms.max(now_ms);
    (start_ms.checked_add(min_active_ms)? <= activation_limit_ms).then_some(start_ms)
}

#[cfg(test)]
mod tests;
