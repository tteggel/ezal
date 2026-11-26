//! A frozen orbital clock with an explicit share of the pointing allowance.

use super::{PlanningConfig, PlanningWindow};
use crate::ntp::{UtcAnchor, UtcClock, MAX_SYNC_AGE_MS};

/// Provisional clock assumptions, subject to installation measurements.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimingPolicy {
    /// Maximum local oscillator error, in parts per million (default 100 ppm).
    pub drift_ppm: u32,
    /// Exclusive maximum age of retained good time evidence, including armed
    /// holdover. Never exceeds the six-hour SNTP policy.
    pub max_sample_age_ms: u64,
    /// Portion of the planner's uncertainty reserve left for alignment/orbit
    /// error, rather than spending the whole reserve on time (default 0.5°).
    pub non_timing_uncertainty_deg: f32,
}

impl Default for TimingPolicy {
    fn default() -> Self {
        Self {
            drift_ppm: 100,
            max_sample_age_ms: MAX_SYNC_AGE_MS,
            non_timing_uncertainty_deg: 0.5,
        }
    }
}

/// Why orbital time cannot authorize a tracking run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimingError {
    /// No synchronized sample, or the live clock was disarmed.
    Unavailable,
    /// Retained good time evidence reaches the exclusive age limit.
    Expired,
    /// A pending leap is not modeled by the linear monotonic mapping.
    LeapPending,
    /// Invalid drift, age, angular reserve, sky rate or time span.
    Configuration,
    /// AOS precedes uptime zero: the device started mid-pass, so the window
    /// has no uptime to start at and the pass cannot be joined.
    BeforeBoot,
    /// Sample uncertainty plus oscillator drift exceeds its pointing allocation.
    Uncertainty,
    /// A new sample disagrees with the frozen timeline beyond the same allocation.
    Correction,
    /// Reversed uptime, stale sample revision or unrepresentable UTC conversion.
    ClockOrder,
}

/// Immutable execution timeline. New samples only validate it; they cannot
/// alter its targets, AOS, LOS or command deadlines.
///
/// Unarmed previews require the current error bound to fit the timing share.
/// Armed passes retain good evidence across missed or imprecise samples, even
/// when estimated drift exceeds that share. This holdover is reported and ends
/// at the hard sample-age limit. A later sample stops the pass only when its
/// whole uncertainty interval contradicts the timeline; it never moves it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExecutionClock {
    anchor: UtcAnchor,
    policy: TimingPolicy,
    through_ms: u64,
    allowance_ms: f64,
}

impl ExecutionClock {
    /// Capture a timeline from a synchronized sample younger than the hard age
    /// limit. Preview admission separately checks its angular timing budget;
    /// armed capture permits drift beyond that budget until the age limit.
    pub fn new(
        anchor: UtcAnchor,
        policy: TimingPolicy,
        config: &PlanningConfig,
        through_ms: u64,
        now_ms: u64,
    ) -> Result<Self, TimingError> {
        let reserve =
            f64::from(config.uncertainty_deg) - f64::from(policy.non_timing_uncertainty_deg);
        let rate = f64::from(config.sky_rate_bound_deg_per_s);
        if policy.drift_ppm == 0
            || policy.drift_ppm > 10_000
            || policy.max_sample_age_ms == 0
            || policy.max_sample_age_ms > MAX_SYNC_AGE_MS
            || !policy.non_timing_uncertainty_deg.is_finite()
            || policy.non_timing_uncertainty_deg <= 0.0
            || !reserve.is_finite()
            || reserve <= 0.0
            || !rate.is_finite()
            || rate <= 0.0
            || through_ms < now_ms
        {
            return Err(TimingError::Configuration);
        }
        let clock = Self {
            anchor,
            policy,
            through_ms,
            allowance_ms: 1000.0 * reserve / rate,
        };
        clock.check_age(anchor, now_ms)?;
        if anchor.sample().leap_pending() {
            return Err(TimingError::LeapPending);
        }
        anchor.unix_ms(through_ms).ok_or(TimingError::ClockOrder)?;
        Ok(clock)
    }

    /// Exact frozen synchronization identity.
    pub const fn anchor(self) -> UtcAnchor {
        self.anchor
    }

    /// End of the captured timeline; no target may be propagated beyond it.
    pub const fn through_ms(self) -> u64 {
        self.through_ms
    }

    /// Fractional UTC for propagation, from the frozen timeline only. Its
    /// error is supervised separately by preview or armed validation.
    pub fn unix_s(self, now_ms: u64) -> Result<f64, TimingError> {
        if now_ms > self.through_ms {
            return Err(TimingError::Expired);
        }
        self.anchor
            .unix_ms(now_ms)
            .map(|ms| ms as f64 / 1000.0)
            .ok_or(TimingError::ClockOrder)
    }

    /// Admit an unarmed preview only while current timing error fits its share
    /// of the pointing budget. This never shifts the captured timeline.
    pub fn validate_now(self, current: UtcClock, now_ms: u64) -> Result<(), TimingError> {
        let latest = self.ordered_anchor(current, now_ms)?;
        let error_ms = self.error_ms(latest, now_ms)?;
        if error_ms > self.allowance_ms {
            return Err(TimingError::Uncertainty);
        }
        if self.disagreement_ms(latest, now_ms)? + error_ms > self.allowance_ms {
            return Err(TimingError::Correction);
        }
        Ok(())
    }

    /// Observe an armed pass's clock without rebasing its timeline. An uncertain
    /// answer cannot disprove the timeline or renew the retained good sample.
    /// Returns whether retained timing error exceeds the pointing budget. The
    /// retained sample must still be younger than the hard age limit.
    pub fn validate_armed(
        self,
        current: UtcClock,
        trusted: &mut UtcClock,
        now_ms: u64,
    ) -> Result<bool, TimingError> {
        let latest = self.ordered_anchor(current, now_ms)?;
        let previous = trusted.anchor().ok_or(TimingError::Unavailable)?;
        if latest.revision() < previous.revision()
            || latest.monotonic_ms() < previous.monotonic_ms()
        {
            return Err(TimingError::ClockOrder);
        }
        let disagreement = self.disagreement_ms(latest, now_ms)?;
        let uncertainty = latest.sample().uncertainty_ms() as f64;
        // Fail only when the sample's entire uncertainty interval contradicts
        // the admitted timeline. A wide interval is weak evidence, not a fault.
        if disagreement > self.allowance_ms + uncertainty {
            return Err(TimingError::Correction);
        }
        if self
            .error_ms(latest, now_ms)
            .is_ok_and(|error| disagreement + error <= self.allowance_ms)
        {
            *trusted = current;
        }
        let retained = trusted.anchor().ok_or(TimingError::Unavailable)?;
        let error_ms = self.error_ms(retained, now_ms)?;
        Ok(self.disagreement_ms(retained, now_ms)? + error_ms > self.allowance_ms)
    }

    fn ordered_anchor(self, current: UtcClock, now_ms: u64) -> Result<UtcAnchor, TimingError> {
        self.unix_s(now_ms)?;
        let latest = current.anchor().ok_or(TimingError::Unavailable)?;
        if latest.revision() < self.anchor.revision()
            || latest.monotonic_ms() < self.anchor.monotonic_ms()
            || now_ms < latest.monotonic_ms()
        {
            return Err(TimingError::ClockOrder);
        }
        if latest.sample().leap_pending() {
            return Err(TimingError::LeapPending);
        }
        Ok(latest)
    }

    fn disagreement_ms(self, latest: UtcAnchor, now_ms: u64) -> Result<f64, TimingError> {
        let frozen = self.anchor.unix_ms(now_ms).ok_or(TimingError::ClockOrder)?;
        let live = latest.unix_ms(now_ms).ok_or(TimingError::ClockOrder)?;
        Ok(frozen.abs_diff(live) as f64)
    }

    pub(super) fn allowance_ms(self) -> f64 {
        self.allowance_ms
    }

    fn check_age(self, anchor: UtcAnchor, at_ms: u64) -> Result<u64, TimingError> {
        let age = at_ms
            .checked_sub(anchor.monotonic_ms())
            .ok_or(TimingError::ClockOrder)?;
        if age >= self.policy.max_sample_age_ms {
            return Err(TimingError::Expired);
        }
        Ok(age)
    }

    pub(super) fn error_ms(self, anchor: UtcAnchor, at_ms: u64) -> Result<f64, TimingError> {
        let age = self.check_age(anchor, at_ms)?;
        let sample = anchor.sample();
        if sample.leap_pending() {
            return Err(TimingError::LeapPending);
        }
        Ok(sample.uncertainty_ms() as f64
            + age as f64 * f64::from(self.policy.drift_ppm) / 1_000_000.0)
    }
}

/// Construct fractional AOS/LOS and monotonic deadlines from one anchor. UTC
/// inputs are milliseconds so the same conversion can be checked exactly later.
pub fn execution_window(
    anchor: UtcAnchor,
    aos_unix_ms: u64,
    los_unix_ms: u64,
) -> Result<PlanningWindow, TimingError> {
    let duration = los_unix_ms
        .checked_sub(aos_unix_ms)
        .ok_or(TimingError::Configuration)?;
    if !(1..=u64::from(PlanningWindow::MAX_DURATION_MS)).contains(&duration) {
        return Err(TimingError::Configuration);
    }
    if aos_unix_ms < anchor.unix_ms(0).ok_or(TimingError::ClockOrder)? {
        return Err(TimingError::BeforeBoot);
    }
    let aos_at_ms = anchor
        .monotonic_at(aos_unix_ms)
        .ok_or(TimingError::ClockOrder)?;
    let los_at_ms = anchor
        .monotonic_at(los_unix_ms)
        .ok_or(TimingError::ClockOrder)?;
    if aos_at_ms.checked_add(duration) != Some(los_at_ms) {
        return Err(TimingError::ClockOrder);
    }
    Ok(PlanningWindow {
        aos_unix_s: aos_unix_ms as f64 / 1000.0,
        aos_at_ms,
        duration_ms: duration as u32,
    })
}
