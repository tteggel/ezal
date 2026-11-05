//! Timestamp-matched pointing telemetry. Repeated reads of one ADC sample do
//! not establish dwell; gaps break continuity rather than implying recovery.

use super::pointing_deviation_deg;
use crate::feedback::{FeedbackSample, FEEDBACK_MAX_AGE_MS};
use crate::position::Pointing;

/// Provisional operational recovery thresholds; these are not receiver-lock tests.
pub const REACQUISITION_ENTER_MS: u64 = 1_000;
/// Fresh observations must establish this much continuous recovery.
pub const REACQUISITION_DWELL_MS: u64 = 2_000;
/// Time after a route join by which measured recovery is established: the
/// dwell, plus the feedback age at which its final sample may still arrive.
pub(super) const MEASURED_RECOVERY_MS: u64 = REACQUISITION_DWELL_MS + FEEDBACK_MAX_AGE_MS;
/// Every recovery attempt shares this one episode budget, clipped to LOS.
pub const REACQUISITION_LIMIT_MS: u64 = 30_000;
/// Provisional recovery threshold, below the plan's warning tolerance,
/// [`PlanningConfig::tolerance_deg`](super::PlanningConfig::tolerance_deg).
pub const REACQUISITION_TOLERANCE_DEG: f32 = 2.0;

/// Measured and desired boresights at the oldest ADC channel's timestamp.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeviationReading {
    /// Original acquisition time, never the time telemetry was requested.
    pub sampled_at_ms: u64,
    /// Calibrated mechanical coordinates, including overlap/over-the-top travel.
    pub measured: Pointing,
    /// Satellite direction propagated at that same instant.
    pub desired: Pointing,
    /// Spherical separation, not an azimuth/elevation coordinate difference.
    pub degrees: f32,
    /// Reserved source/alignment/time error plus ADC resolution and worst-case
    /// paired-channel skew. This is an assumption bound, not measured RF error.
    pub uncertainty_deg: f32,
}

/// Retained pass metrics, with a current reading only while it is fresh.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DeviationStatus {
    /// Run that owns these metrics, including after a new pass is selected.
    pub run: Option<super::RunToken>,
    /// None on stale/failed feedback or after the run terminates.
    pub current: Option<DeviationReading>,
    /// Immediate warning; never waits for the recovery-entry dwell.
    pub warning: bool,
    /// Maximum timestamp-matched estimate observed during this run.
    pub maximum_deg: Option<f32>,
    /// Time outside the warning threshold, excluding unobserved/stale gaps.
    pub outside_tolerance_ms: u64,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct DeviationMonitor {
    tolerance: f32,
    current: Option<DeviationReading>,
    maximum: Option<f32>,
    outside_ms: u64,
    above_since: Option<u64>,
    below_since: Option<u64>,
}

impl DeviationMonitor {
    pub fn new(tolerance: f32) -> Self {
        Self {
            tolerance,
            current: None,
            maximum: None,
            outside_ms: 0,
            above_since: None,
            below_since: None,
        }
    }

    pub fn observe(
        &mut self,
        feedback: FeedbackSample,
        sky: Pointing,
        uncertainty_deg: f32,
    ) -> Option<()> {
        let at = feedback.sampled_at_ms();
        if let Some(previous) = self.current {
            if at < previous.sampled_at_ms
                || (at == previous.sampled_at_ms && feedback.position() != previous.measured)
            {
                return None;
            }
            if at == previous.sampled_at_ms {
                return Some(());
            }
            let dt = at - previous.sampled_at_ms;
            if dt > FEEDBACK_MAX_AGE_MS {
                self.above_since = None;
                self.below_since = None;
            }
            if previous.degrees > self.tolerance {
                self.outside_ms = self.outside_ms.saturating_add(dt.min(FEEDBACK_MAX_AGE_MS));
            }
        }
        let degrees = pointing_deviation_deg(feedback.position(), sky)?;
        self.maximum = Some(self.maximum.map_or(degrees, |old| old.max(degrees)));
        self.current = Some(DeviationReading {
            sampled_at_ms: at,
            measured: feedback.position(),
            desired: sky,
            degrees,
            uncertainty_deg,
        });
        if degrees > self.tolerance {
            self.above_since.get_or_insert(at);
        } else {
            self.above_since = None;
        }
        if degrees <= REACQUISITION_TOLERANCE_DEG.min(self.tolerance) {
            self.below_since.get_or_insert(at);
        } else {
            self.below_since = None;
        }
        Some(())
    }

    pub fn needs_recovery(&self) -> bool {
        self.dwell(self.above_since, REACQUISITION_ENTER_MS)
    }

    pub fn recovered(&self) -> bool {
        self.dwell(self.below_since, REACQUISITION_DWELL_MS)
    }

    fn dwell(&self, since: Option<u64>, duration: u64) -> bool {
        self.current
            .zip(since)
            .is_some_and(|(last, since)| last.sampled_at_ms - since >= duration)
    }

    pub fn status(&self, now_ms: u64) -> DeviationStatus {
        let current = self.current.filter(|reading| {
            now_ms
                .checked_sub(reading.sampled_at_ms)
                .is_some_and(|age| age < FEEDBACK_MAX_AGE_MS)
        });
        let tail = self.current.map_or(0, |reading| {
            if reading.degrees > self.tolerance {
                now_ms
                    .saturating_sub(reading.sampled_at_ms)
                    .min(FEEDBACK_MAX_AGE_MS)
            } else {
                0
            }
        });
        DeviationStatus {
            run: None,
            current,
            warning: current.is_some_and(|reading| reading.degrees > self.tolerance),
            maximum_deg: self.maximum,
            outside_tolerance_ms: self.outside_ms.saturating_add(tail),
        }
    }

    pub fn finish(&mut self, now_ms: u64) {
        self.outside_ms = self.status(now_ms).outside_tolerance_ms;
        self.current = None;
        self.above_since = None;
        self.below_since = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::sample;

    #[test]
    fn warning_is_immediate_but_entry_needs_distinct_continuous_samples() {
        let mut monitor = DeviationMonitor::new(3.0);
        let sky = Pointing::new(180.0, 40.0);
        let pose = Pointing::new(180.0, 45.0);
        monitor.observe(sample(pose, 0), sky, 1.5).unwrap();
        assert!(monitor.status(0).warning);
        for _ in 0..100 {
            monitor.observe(sample(pose, 0), sky, 1.5).unwrap();
        }
        assert!(!monitor.needs_recovery());
        assert!(monitor.status(500).current.is_none());
        // A gap must not be counted as an established entry dwell.
        monitor.observe(sample(pose, 2000), sky, 1.5).unwrap();
        assert!(!monitor.needs_recovery());
        for at in (2200..=3000).step_by(200) {
            monitor.observe(sample(pose, at), sky, 1.5).unwrap();
        }
        assert!(monitor.needs_recovery());
        assert_eq!(monitor.status(3000).outside_tolerance_ms, 1500);
        monitor.finish(3200);
        assert_eq!(monitor.status(9999).outside_tolerance_ms, 1700);
        assert!(monitor.status(9999).current.is_none());
    }

    #[test]
    fn recovery_requires_two_seconds_below_lower_threshold_and_gaps_reset_it() {
        let mut monitor = DeviationMonitor::new(3.0);
        let sky = Pointing::new(180.0, 40.0);
        let good = Pointing::new(180.0, 41.0);
        for at in (0..=1800).step_by(200) {
            monitor.observe(sample(good, at), sky, 1.0).unwrap();
        }
        assert!(!monitor.recovered());
        // Within warning tolerance but above the recovery threshold resets dwell.
        monitor
            .observe(sample(Pointing::new(180.0, 42.5), 2000), sky, 1.0)
            .unwrap();
        assert!(!monitor.status(2000).warning);
        for at in (2200..=4200).step_by(200) {
            monitor.observe(sample(good, at), sky, 1.0).unwrap();
        }
        assert!(monitor.recovered());
        monitor.observe(sample(good, 5000), sky, 1.0).unwrap();
        assert!(!monitor.recovered());
    }

    #[test]
    fn deviation_is_spherical_through_wrap_and_inverted_pointing() {
        let mut monitor = DeviationMonitor::new(3.0);
        monitor
            .observe(
                sample(Pointing::new(359.0, 89.0), 0),
                Pointing::new(1.0, 89.0),
                1.0,
            )
            .unwrap();
        assert!(monitor.status(0).current.unwrap().degrees < 0.1);
        monitor
            .observe(
                sample(Pointing::new(0.0, 40.0), 200),
                Pointing::new(180.0, 140.0),
                1.0,
            )
            .unwrap();
        assert!(monitor.status(200).current.unwrap().degrees < 0.01);
        // A repeated timestamp may not smuggle in a changed measurement.
        assert!(monitor
            .observe(
                sample(Pointing::new(1.0, 40.0), 200),
                Pointing::new(180.0, 140.0),
                1.0
            )
            .is_none());
        assert!(monitor
            .observe(
                sample(Pointing::new(0.0, 40.0), 100),
                Pointing::new(180.0, 140.0),
                1.0
            )
            .is_none());
    }
}
