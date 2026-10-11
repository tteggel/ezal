//! Second finite search: replay a supported prefix, inhibit/coast, then reuse
//! T6's all-scenario intercept validation through the original LOS. Complete
//! routes are exhausted first. One consolidated recovery interval is admitted;
//! the first qualifying partial route wins, not a claim of global optimality.

use super::*;
use crate::feedback::FEEDBACK_MAX_AGE_MS;
use crate::tracking::recovery::{InterceptSearch, SearchRoute};
use crate::tracking::{
    ReceptionGap, ReceptionWindow, PARTIAL_ADMISSION_LEAD_MS, REACQUISITION_DWELL_MS,
    REACQUISITION_LIMIT_MS,
};

pub(super) struct PartialEvaluation {
    pub recovery_at_ms: u64,
    late: bool,
    deadline_ms: u64,
    earliest_join: u64,
    latest_join: u64,
    search: Option<InterceptSearch>,
}

impl<E: Ephemeris> Planner<E> {
    pub(super) fn begin_partial(&mut self, cut: Option<u32>) {
        let cut = cut.filter(|cut| *cut > 0);
        let late = cut.is_none();
        let recovery_at_ms = cut.map_or_else(
            || {
                self.window
                    .aos_at_ms
                    .max(self.start.at_ms.saturating_add(PARTIAL_ADMISSION_LEAD_MS))
            },
            |cut| self.window.aos_at_ms + u64::from(cut),
        );
        let los = self.window.aos_at_ms + u64::from(self.window.duration_ms);
        let deadline_ms = recovery_at_ms
            .saturating_add(REACQUISITION_LIMIT_MS)
            .min(los);
        let previous = &self.assessment;
        let mut assessment = Assessment::new(self.route.choice);
        if late {
            assessment.prepare_at_ms = recovery_at_ms;
            assessment.preparation_lead_ms = 0;
        } else {
            assessment.prepare_at_ms = previous.prepare_at_ms;
            assessment.preparation_ms = previous.preparation_ms;
            assessment.preparation_lead_ms = previous.preparation_lead_ms;
        }
        self.assessment = assessment;
        self.partial = Some(PartialEvaluation {
            recovery_at_ms,
            late,
            deadline_ms,
            earliest_join: u64::MAX,
            latest_join: 0,
            search: None,
        });
        self.sim = None;
        self.scenario = 0;
        self.metrics = RunMetrics::default();
        self.partial_cut = cut;
        self.phase = if late {
            Phase::PartialSettle
        } else {
            Phase::PartialPrefix
        };
        if recovery_at_ms >= deadline_ms {
            self.reject_partial();
        }
    }

    fn reject_partial(&mut self) {
        self.assessment.failure = Some(CandidateFailure::NoRecovery);
        self.assessment.failure_scenario = Some(self.scenario);
        self.assessment.failure_phase = Some(EvaluationPhase::Following);
        self.finish_candidate();
    }

    /// Hold until both physical coast and a new settled feedback pair complete.
    /// Search starts from the copied actuator, never from an unused duty budget.
    pub(super) fn settle_partial(&mut self) -> Result<(), PlanningError> {
        if self.sim.is_none() {
            self.start_simulation(false)?;
            return Ok(());
        }
        let now = self.sim.as_ref().unwrap().now_ms();
        if now >= self.partial.as_ref().unwrap().deadline_ms {
            self.reject_partial();
            return Ok(());
        }
        let sky = self.sky_at((now - self.window.aos_at_ms) as u32)?;
        let sample = match self.sim.as_mut().unwrap().step_target(None) {
            Ok(sample) => sample,
            Err(_) => {
                self.reject_partial();
                return Ok(());
            }
        };
        self.observe_error(
            now,
            pointing_deviation_deg(sample.position, sky).ok_or(PlanningError::Source)?,
            sky,
        )?;
        if let Some(start) = self.sim.as_ref().unwrap().resting_start() {
            let plan = SearchRoute {
                route: &self.route,
                window: self.window,
                config: &self.config,
                calibration: self.calibration,
                actuator: self.start.actuator.configuration(),
            };
            let partial = self.partial.as_mut().unwrap();
            partial.search = InterceptSearch::new(plan, start, partial.deadline_ms);
            if partial.search.is_some() {
                self.phase = Phase::PartialSearch;
            } else {
                self.reject_partial();
            }
        }
        Ok(())
    }

    /// Each prefix case gets a separate search over every recovery case. This
    /// keeps the duty and relay state produced by that prefix in every suffix.
    pub(super) fn search_partial(&mut self) -> Result<(), PlanningError> {
        let plan = SearchRoute {
            route: &self.route,
            window: self.window,
            config: &self.config,
            calibration: self.calibration,
            actuator: self.start.actuator.configuration(),
        };
        let partial = self.partial.as_mut().unwrap();
        let search = partial.search.as_mut().unwrap();
        search
            .step(plan, &self.source)
            .map_err(|_| PlanningError::Source)?;
        if !search.finished {
            return Ok(());
        }
        let Some(intercept) = search.result else {
            self.reject_partial();
            return Ok(());
        };
        partial.earliest_join = partial.earliest_join.min(intercept.join_at_ms);
        partial.latest_join = partial.latest_join.max(intercept.join_at_ms);
        let prefix = self.sim.as_ref().unwrap().summary();
        let recovery = search.metrics;
        self.assessment.sampled_peak_deg = self
            .assessment
            .sampled_peak_deg
            .max(self.metrics.peak)
            .max(recovery.peak);
        self.assessment.bounded_peak_deg = self
            .assessment
            .bounded_peak_deg
            .max(self.metrics.bound)
            .max(recovery.bound);
        for axis in 0..2 {
            self.assessment.energized_ms[axis] = self.assessment.energized_ms[axis]
                .max(prefix.energized_ms[axis] + recovery.energized_ms[axis]);
        }
        // Each restarted simulation omits its predecessor's last direction.
        // Bound that possible cross-boundary reversal on each moving axis.
        let boundary_reversals = (0..2)
            .filter(|&axis| prefix.energized_ms[axis] > 0 && recovery.energized_ms[axis] > 0)
            .count() as u64;
        self.assessment.reversals = self
            .assessment
            .reversals
            .max(prefix.reversals.iter().sum::<u64>() + recovery.reversals + boundary_reversals);
        self.assessment.transitions = self
            .assessment
            .transitions
            .max(prefix.transitions.iter().sum::<u64>() + recovery.transitions);
        self.assessment.completed_scenarios += 1;
        self.scenario += 1;
        self.sim = None;
        partial.search = None;
        if self.scenario < self.config.scenarios.len() {
            self.phase = if partial.late {
                Phase::PartialSettle
            } else {
                Phase::PartialPrefix
            };
            return Ok(());
        }
        let gap = ReceptionGap {
            missed: ReceptionWindow {
                start_ms: if partial.late {
                    0
                } else {
                    (partial.recovery_at_ms - self.window.aos_at_ms) as u32
                },
                end_ms: (partial.latest_join + REACQUISITION_DWELL_MS + FEEDBACK_MAX_AGE_MS
                    - self.window.aos_at_ms) as u32,
            },
            late_acquisition: partial.late,
            recovery_at_ms: partial.recovery_at_ms,
            deadline_ms: partial.deadline_ms,
            earliest_join_at_ms: partial.earliest_join,
            latest_join_at_ms: partial.latest_join,
        };
        let reception = ReceptionCoverage::Partial(gap);
        if !reception.valid(self.window) {
            self.reject_partial();
            return Ok(());
        }
        self.assessment.gap_time_ms = u64::from(gap.missed.end_ms - gap.missed.start_ms);
        self.assessment
            .gaps
            .push(PlannedGap {
                start_ms: gap.missed.start_ms,
                end_ms: gap.missed.end_ms,
            })
            .expect("one gap");
        self.report.closest_attempt = Some(self.assessment.clone());
        self.report.plan = Some(TrackingPlan {
            reception,
            route: self.route.clone(),
            window: self.window,
            config: self.config.clone(),
            calibration: self.calibration,
            actuator: self.start.actuator.configuration(),
            assessment: self.assessment.clone(),
        });
        self.phase = Phase::Complete;
        Ok(())
    }
}
