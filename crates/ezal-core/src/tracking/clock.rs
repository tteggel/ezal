//! A frozen orbital clock with an explicit share of the pointing allowance.

use super::{PlanningConfig, PlanningWindow};
use crate::ntp::{UtcAnchor, UtcClock, MAX_SYNC_AGE_MS};

/// Provisional clock assumptions, subject to installation measurements.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimingPolicy {
    /// Maximum local oscillator error, in parts per million (default 100 ppm).
    pub drift_ppm: u32,
    /// Exclusive maximum age of a good sample; never exceeds the SNTP policy.
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
    /// Sample age reaches the exclusive limit before the required end.
    Expired,
    /// A pending leap is not modeled by the linear monotonic mapping.
    LeapPending,
    /// Invalid drift, age, angular reserve, sky rate or time span.
    Configuration,
    /// Sample uncertainty plus oscillator drift exceeds its pointing allocation.
    Uncertainty,
    /// A new sample disagrees with the frozen timeline beyond the same allocation.
    Correction,
    /// Reversed uptime, stale sample revision or unrepresentable UTC conversion.
    ClockOrder,
}

/// Immutable execution timeline. New samples only validate it; they cannot
/// alter its targets, AOS, LOS or command deadlines.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExecutionClock {
    anchor: UtcAnchor,
    policy: TimingPolicy,
    through_ms: u64,
    allowance_ms: f64,
    sky_rate_deg_per_s: f64,
}

impl ExecutionClock {
    /// A preview can exist hours before preparation while the device keeps
    /// synchronizing. Validate today's sample now; the full LOS horizon is
    /// checked again before freezing. Never use this alone to authorize output.
    pub(super) fn preview(
        anchor: UtcAnchor,
        policy: TimingPolicy,
        config: &PlanningConfig,
        through_ms: u64,
        now_ms: u64,
    ) -> Result<Self, TimingError> {
        if through_ms < now_ms {
            return Err(TimingError::Configuration);
        }
        let mut clock = Self::new(anchor, policy, config, now_ms, now_ms)?;
        clock.through_ms = through_ms;
        Ok(clock)
    }

    /// Capture a quantified anchor and require its timing error to fit through
    /// LOS. The sky-rate bound is the same assumption used by the planner.
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
            sky_rate_deg_per_s: rate,
        };
        clock.check_anchor(anchor, now_ms)?;
        if clock.error_ms(anchor, through_ms)? > clock.allowance_ms {
            return Err(TimingError::Uncertainty);
        }
        anchor.unix_ms(through_ms).ok_or(TimingError::ClockOrder)?;
        Ok(clock)
    }

    /// Exact frozen synchronization identity.
    pub const fn anchor(self) -> UtcAnchor {
        self.anchor
    }

    /// End of the span for which error/freshness were admitted.
    pub const fn through_ms(self) -> u64 {
        self.through_ms
    }

    /// Fractional UTC for propagation, from the frozen timeline only.
    pub fn unix_s(self, now_ms: u64) -> Result<f64, TimingError> {
        self.check_anchor(self.anchor, now_ms)?;
        if now_ms > self.through_ms {
            return Err(TimingError::Expired);
        }
        self.anchor
            .unix_ms(now_ms)
            .map(|ms| ms as f64 / 1000.0)
            .ok_or(TimingError::ClockOrder)
    }

    /// Timing contribution alone, not a claim about total physical pointing.
    pub fn uncertainty_deg(self, now_ms: u64) -> Result<f64, TimingError> {
        Ok(self.error_ms(self.anchor, now_ms)? * self.sky_rate_deg_per_s / 1000.0)
    }

    /// Check the latest live sample without rebasing. Its error interval must
    /// stay within the frozen timeline's timing allocation through LOS. Failed
    /// refreshes leave `current` unchanged and cannot renew either source age.
    pub fn validate(self, current: UtcClock, now_ms: u64) -> Result<(), TimingError> {
        self.unix_s(now_ms)?;
        if self.error_ms(self.anchor, self.through_ms)? > self.allowance_ms {
            return Err(TimingError::Uncertainty);
        }
        let latest = current.anchor().ok_or(TimingError::Unavailable)?;
        if latest.revision() < self.anchor.revision()
            || latest.monotonic_ms() < self.anchor.monotonic_ms()
        {
            return Err(TimingError::ClockOrder);
        }
        self.check_anchor(latest, now_ms)?;
        let frozen = self.anchor.unix_ms(now_ms).ok_or(TimingError::ClockOrder)?;
        let live = latest.unix_ms(now_ms).ok_or(TimingError::ClockOrder)?;
        let worst_ms = frozen.abs_diff(live) as f64 + self.error_ms(latest, self.through_ms)?;
        if worst_ms > self.allowance_ms {
            return Err(TimingError::Correction);
        }
        Ok(())
    }

    /// Exclusive deadline for admitting this preview's sample. Future
    /// execution still requires a complete-horizon check at preparation.
    pub fn admission_deadline_ms(self) -> Result<u64, TimingError> {
        let remaining = self.allowance_ms - self.anchor.sample().uncertainty_ms() as f64;
        if !remaining.is_finite() || remaining < 0.0 {
            return Err(TimingError::Uncertainty);
        }
        let age_limit = (remaining * 1_000_000.0 / f64::from(self.policy.drift_ppm))
            .min(self.policy.max_sample_age_ms as f64) as u64;
        self.anchor
            .monotonic_ms()
            .checked_add(
                age_limit
                    .saturating_add(1)
                    .min(self.policy.max_sample_age_ms),
            )
            .ok_or(TimingError::ClockOrder)
    }

    pub(super) fn allowance_ms(self) -> f64 {
        self.allowance_ms
    }

    fn check_anchor(self, anchor: UtcAnchor, at_ms: u64) -> Result<(), TimingError> {
        self.error_ms(anchor, at_ms).map(|_| ())
    }

    fn error_ms(self, anchor: UtcAnchor, at_ms: u64) -> Result<f64, TimingError> {
        let age = at_ms
            .checked_sub(anchor.monotonic_ms())
            .ok_or(TimingError::ClockOrder)?;
        if age >= self.policy.max_sample_age_ms {
            return Err(TimingError::Expired);
        }
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
    if !(1..=1_800_000).contains(&duration) {
        return Err(TimingError::Configuration);
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
