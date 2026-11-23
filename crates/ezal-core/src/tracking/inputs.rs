//! Owned, validated execution inputs: the orbital, observer and clock snapshot
//! one plan is built and executed against. Elements come only from
//! [`crate::catalog`] publications.

use core::cell::Cell;

use super::{
    ExecutionClock, PlanTicket, PlanningConfig, PlanningWindow, TimingError, TimingPolicy,
    TrackingPlan,
};
use crate::catalog::OrbitalSnapshot;
use crate::ntp::UtcClock;
use crate::orbit::{
    GroundStation, LookAngle, Observer, OrbitalState, MAX_FUTURE_EPOCH_S, MAX_TLE_AGE_S,
};
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
    /// Ticket, satellite, clock mapping, window or assumptions disagree.
    Binding,
    /// The starting pose changed, so the preview needs replacing.
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
    /// early selection while NTP keeps synchronizing.
    pub fn capture(
        ticket: PlanTicket,
        orbit: OrbitalSnapshot,
        settings: InputSettings,
        current: UtcClock,
        mut window: PlanningWindow,
        config: &PlanningConfig,
        now_ms: u64,
    ) -> Result<Self, InputError> {
        let anchor = current.anchor().ok_or(TimingError::Unavailable)?;
        let aos_utc = anchor
            .unix_ms(window.aos_at_ms)
            .ok_or(InputError::Binding)? as f64
            / 1000.0;
        if ticket.pass().norad_id != orbit.norad_id()
            || ticket.revisions().orbit != orbit.publication().revision
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
        if ticket.is_rehearsal() {
            window.aos_at_ms = ticket
                .rehearsal_aos_at_ms(now_ms)
                .ok_or(InputError::Binding)?;
            if !ticket.rehearsal_fits(window) {
                return Err(InputError::Binding);
            }
        }
        let los_ms = window
            .aos_at_ms
            .checked_add(u64::from(window.duration_ms))
            .ok_or(InputError::Binding)?;
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
    /// Exclusive expiry: these inputs authorize nothing from LOS. Their clock
    /// stays valid only while the newest sample vouches for it.
    pub fn freeze_before_ms(&self) -> u64 {
        self.window.aos_at_ms + u64::from(self.window.duration_ms)
    }
    /// Verify the result used the snapshot's timeline and assumptions. The
    /// internal worker remains responsible for actually planning this ephemeris.
    pub fn matches(&self, plan: &TrackingPlan, ticket: PlanTicket) -> bool {
        self.ticket == ticket && self.window == plan.window && self.config == plan.config
    }
    /// Before preparation the newest sample must vouch for the frozen
    /// timeline now. A routine synchronization keeps the preview; one that
    /// moves time beyond the timing share, or a lapse in synchronization,
    /// invalidates it and requests automatic replanning.
    pub fn validate_preview(&self, current: UtcClock, now_ms: u64) -> Result<(), InputError> {
        self.clock.validate_now(current, now_ms)?;
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
        let mut end = self
            .clock
            .anchor()
            .unix_ms(self.clock.through_ms())
            .ok_or(TimingError::ClockOrder)? as f64
            / 1000.0;
        if self.ticket.is_rehearsal() {
            // The orbit is evaluated at the selected future pass's UTC, while
            // clock/output supervision still uses the actual rehearsal time.
            end = end.max(self.window.aos_unix_s + f64::from(self.window.duration_ms) / 1000.0);
        }
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
        if self.ticket.is_rehearsal() {
            let offset = now_ms.checked_sub(self.window.aos_at_ms)?;
            self.look_at(self.window.aos_unix_s + offset as f64 / 1000.0)
        } else {
            self.look_at(self.clock.unix_s(now_ms).ok()?)
        }
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

/// Spacing of the SGP4 states a [`PlanningTrack`] interpolates between.
const KNOT_MS: u32 = 60_000;
const MAX_KNOTS: usize = PlanningWindow::MAX_DURATION_MS.div_ceil(KNOT_MS) as usize + 1;

/// [`ExecutionInputs`] as a planner's ephemeris: the same orbit, station and
/// window, propagated once a minute instead of at every look-up. A plan
/// evaluates the sky hundreds of thousands of times, and on the Pico's
/// software floating point one SGP4 propagation costs about ten times the
/// observer transformation, so planning a pass would otherwise take ten
/// minutes rather than three. Between two states, cubic Hermite interpolation
/// of TEME position from position and velocity follows SGP4 to about 0.00001°
/// as seen from the station; the transformation itself stays exact. A run and
/// reacquisition propagate the inputs directly.
///
/// Each state is computed on the first look-up that needs it, so no planner
/// step propagates more than twice, and a value does not depend on the order
/// of look-ups.
#[derive(Clone, Debug)]
pub struct PlanningTrack {
    inputs: ExecutionInputs,
    knots: [Cell<Option<OrbitalState>>; MAX_KNOTS],
}

impl PlanningTrack {
    /// Plan against `inputs`, with no state propagated yet.
    pub fn new(inputs: ExecutionInputs) -> Self {
        Self {
            inputs,
            knots: [const { Cell::new(None) }; MAX_KNOTS],
        }
    }
    /// The exact inputs a plan from this track is published with.
    pub const fn inputs(&self) -> &ExecutionInputs {
        &self.inputs
    }
    /// Knot `index`: every whole minute from AOS, then LOS.
    fn knot(&self, index: usize) -> Option<(f64, OrbitalState)> {
        let window = self.inputs.window;
        let offset_ms = (index as u64 * u64::from(KNOT_MS)).min(u64::from(window.duration_ms));
        let unix_s = window.aos_unix_s + offset_ms as f64 / 1000.0;
        let cell = self.knots.get(index)?;
        let state = match cell.get() {
            Some(state) => state,
            None => {
                let state = self.inputs.orbit.orbit().state(unix_s)?;
                cell.set(Some(state));
                state
            }
        };
        Some((unix_s, state))
    }
}

impl Ephemeris for PlanningTrack {
    fn look_at(&self, unix_s: f64) -> Option<LookAngle> {
        let window = self.inputs.window;
        let offset_ms = (unix_s - window.aos_unix_s) * 1000.0;
        if !(0.0..=f64::from(window.duration_ms)).contains(&offset_ms) {
            return None;
        }
        let last = window.duration_ms.div_ceil(KNOT_MS) as usize - 1;
        let index = ((offset_ms / f64::from(KNOT_MS)) as usize).min(last);
        let ((t0, a), (t1, b)) = (self.knot(index)?, self.knot(index + 1)?);
        // Hermite basis on s in [0, 1]; velocities scale by the interval.
        let h = t1 - t0;
        let s = (unix_s - t0) / h;
        let (s2, s3) = (s * s, s * s * s);
        let [p0, m0, p1, m1] = [
            2.0 * s3 - 3.0 * s2 + 1.0,
            (s3 - 2.0 * s2 + s) * h,
            3.0 * s2 - 2.0 * s3,
            (s3 - s2) * h,
        ];
        let position_km = core::array::from_fn(|axis| {
            p0 * a.position_km[axis]
                + m0 * a.velocity_km_s[axis]
                + p1 * b.position_km[axis]
                + m1 * b.velocity_km_s[axis]
        });
        let look = self.inputs.station.look_at(position_km, unix_s);
        (look.azimuth_deg.is_finite() && look.elevation_deg.is_finite()).then_some(look)
    }
}
