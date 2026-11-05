//! Owned, validated execution inputs: the orbital, observer and clock snapshot
//! one plan is built and executed against. Elements come only from
//! [`crate::catalog`] publications.

use super::{
    ExecutionClock, PlanTicket, PlanningConfig, PlanningWindow, TimingError, TimingPolicy,
    TrackingPlan,
};
use crate::catalog::OrbitalSnapshot;
use crate::ntp::UtcClock;
use crate::orbit::{GroundStation, LookAngle, Observer, MAX_FUTURE_EPOCH_S, MAX_TLE_AGE_S};
use crate::passes::Ephemeris;

/// Input rejection, suitable for both a blocked preview and a terminal run reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputError {
    /// No matching elements in the device's validated catalog.
    MissingOrbit,
    /// Orbital epoch cannot support the whole window plus the timing allowance.
    StaleOrbit,
    /// The snapshot's download completed after the current uptime.
    SourceOrder,
    /// Invalid station coordinates.
    Observer,
    /// Ticket, satellite, clock revision, window or assumptions disagree.
    Binding,
    /// A new clock sample requires a replacement preview before preparation.
    RevisionChanged,
    /// Clock validity/uncertainty failure.
    Timing(TimingError),
}

impl From<TimingError> for InputError {
    fn from(error: TimingError) -> Self {
        Self::Timing(error)
    }
}

/// Installation inputs captured by value, not through a shared mutable handle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InputSettings {
    /// Ground station used for both planning and live propagation.
    pub observer: Observer,
    /// Provisional oscillator and non-timing uncertainty assumptions.
    pub timing: TimingPolicy,
}

impl Default for InputSettings {
    fn default() -> Self {
        Self {
            observer: Observer::CLEVEDON,
            timing: TimingPolicy::default(),
        }
    }
}

/// Exact orbital, observer and clock snapshot used by one planning job. The
/// planner consumes this as its ephemeris; the accepted copy stays with its
/// plan for execution/reacquisition. New catalog data cannot alter it.
#[derive(Clone, Debug)]
pub struct ExecutionInputs {
    orbit: OrbitalSnapshot,
    station: GroundStation,
    clock: ExecutionClock,
    ticket: PlanTicket,
    window: PlanningWindow,
    config: PlanningConfig,
}

impl ExecutionInputs {
    /// Capture a preview. Whole-window orbital validity is required now; the
    /// clock's complete LOS error bound is checked at preparation, allowing
    /// early selection while automatic NTP/plan refreshes continue.
    pub fn capture(
        ticket: PlanTicket,
        orbit: OrbitalSnapshot,
        settings: InputSettings,
        current: UtcClock,
        window: PlanningWindow,
        config: &PlanningConfig,
        now_ms: u64,
    ) -> Result<Self, InputError> {
        let anchor = current.anchor().ok_or(TimingError::Unavailable)?;
        let los_ms = window
            .aos_at_ms
            .checked_add(u64::from(window.duration_ms))
            .ok_or(InputError::Binding)?;
        let aos_utc = anchor
            .unix_ms(window.aos_at_ms)
            .ok_or(InputError::Binding)? as f64
            / 1000.0;
        if ticket.pass().norad_id != orbit.norad_id()
            || ticket.revisions().orbit != orbit.publication().revision
            || ticket.revisions().clock != anchor.revision()
            || config.preparation_timing != ticket.preparation_timing()
            || !window.aos_unix_s.is_finite()
            || aos_utc != window.aos_unix_s
            || !(1..=PlanningWindow::MAX_DURATION_MS).contains(&window.duration_ms)
            || (window.aos_unix_s - ticket.pass().aos_unix_s as f64).abs() > 1.0
            || (window.aos_unix_s + f64::from(window.duration_ms) / 1000.0
                - ticket.pass().los_unix_s as f64)
                .abs()
                > 1.0
        {
            return Err(InputError::Binding);
        }
        let station = GroundStation::new(settings.observer).ok_or(InputError::Observer)?;
        let clock = ExecutionClock::preview(anchor, settings.timing, config, los_ms, now_ms)?;
        let inputs = Self {
            orbit,
            station,
            clock,
            ticket,
            window,
            config: config.clone(),
        };
        inputs.validate_preview(current, now_ms)?;
        Ok(inputs)
    }
    /// Clock used for every fractional orbital-time calculation.
    pub const fn clock(&self) -> ExecutionClock {
        self.clock
    }
    /// Complete execution window evaluated by the planner.
    pub const fn window(&self) -> PlanningWindow {
        self.window
    }
    /// Exact job identity; a snapshot cannot authorize a different selection.
    pub const fn ticket(&self) -> PlanTicket {
        self.ticket
    }
    /// Earliest exclusive expiry of the preview's current timing permission.
    pub fn freeze_before_ms(&self) -> Result<u64, InputError> {
        Ok(self
            .clock
            .admission_deadline_ms()?
            .min(self.window.aos_at_ms + u64::from(self.window.duration_ms)))
    }
    /// Verify the result used the snapshot's timeline and assumptions. The
    /// internal worker remains responsible for actually planning this ephemeris.
    pub fn matches(&self, plan: &TrackingPlan, ticket: PlanTicket) -> bool {
        self.ticket == ticket && self.window == plan.window && self.config == plan.config
    }
    /// Pre-preparation samples must stay current; even a small new clock
    /// correction invalidates the preview and requests automatic replanning.
    pub fn validate_preview(&self, current: UtcClock, now_ms: u64) -> Result<(), InputError> {
        let latest = current.anchor().ok_or(TimingError::Unavailable)?;
        if latest != self.clock.anchor() {
            return Err(InputError::RevisionChanged);
        }
        if now_ms >= self.freeze_before_ms()? {
            return Err(TimingError::Expired.into());
        }
        self.clock.unix_s(now_ms)?;
        self.validate_orbit(now_ms)
    }
    /// Required immediately before freezing, and on every serviced output
    /// admission thereafter. Full source validity extends through original LOS.
    pub fn validate_execution(&self, current: UtcClock, now_ms: u64) -> Result<(), InputError> {
        self.clock.validate(current, now_ms)?;
        self.validate_orbit(now_ms)
    }
    fn validate_orbit(&self, now_ms: u64) -> Result<(), InputError> {
        if now_ms < self.orbit.publication().received_at_ms {
            return Err(InputError::SourceOrder);
        }
        let now = self.clock.unix_s(now_ms)?;
        let end = self
            .clock
            .anchor()
            .unix_ms(self.clock.through_ms())
            .ok_or(TimingError::ClockOrder)? as f64
            / 1000.0;
        let allowance_s = self.clock.allowance_ms() / 1000.0;
        let epoch = self.orbit.epoch_unix_s();
        if !epoch.is_finite()
            || epoch > now - allowance_s + MAX_FUTURE_EPOCH_S as f64
            || epoch < end + allowance_s - MAX_TLE_AGE_S as f64
        {
            return Err(InputError::StaleOrbit);
        }
        Ok(())
    }
    /// Propagate the frozen orbit at fractional uptime-matched UTC. Execution
    /// services validate live clock/network state separately before output.
    pub fn look_at_uptime(&self, now_ms: u64) -> Option<LookAngle> {
        self.look_at(self.clock.unix_s(now_ms).ok()?)
    }
}

impl Ephemeris for ExecutionInputs {
    fn look_at(&self, unix_s: f64) -> Option<LookAngle> {
        let end = self.window.aos_unix_s + f64::from(self.window.duration_ms) / 1000.0;
        if !unix_s.is_finite() || unix_s < self.window.aos_unix_s || unix_s > end {
            return None;
        }
        self.orbit.orbit().look_at(&self.station, unix_s)
    }
}
