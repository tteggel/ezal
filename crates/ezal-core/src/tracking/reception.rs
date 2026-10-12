//! Reception claims are separate from motor-route geometry. A failed nominal
//! trajectory is never made armable merely by relabeling its pointing errors.

use super::deviation::MEASURED_RECOVERY_MS;
use super::{PlanningWindow, REACQUISITION_LIMIT_MS};

/// Preview time reserved before a late acquisition's fixed execution start.
/// Like T6's computation lead, this needs Pico timing validation in T7.
pub const PARTIAL_ADMISSION_LEAD_MS: u64 = 5_000;

/// A predicted usable reception interval, relative to the original AOS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceptionWindow {
    /// Inclusive beginning in milliseconds after original AOS.
    pub start_ms: u32,
    /// Exclusive end in milliseconds after original AOS.
    pub end_ms: u32,
}

/// A validated recovery opportunity, not an unsupported nominal-route gap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceptionGap {
    /// Missed interval, including acquisition and measured-recovery allowance.
    pub missed: ReceptionWindow,
    /// No initial reception is claimed; the run starts with a future intercept.
    pub late_acquisition: bool,
    /// Absolute start of the episode; retries cannot move it.
    pub recovery_at_ms: u64,
    /// Earlier of episode start + 30 seconds and the original LOS.
    pub deadline_ms: u64,
    /// Earliest validated route join across the evaluated starting scenarios.
    pub earliest_join_at_ms: u64,
    /// Latest validated route join across those scenarios, before measured dwell.
    pub latest_join_at_ms: u64,
}

/// Complete-window support or an explicitly partial reception claim. The
/// planner alone writes a plan's coverage after all required validation passes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceptionCoverage {
    /// All configured scenarios cover AOS through LOS without predicted gaps.
    Complete,
    /// A supported prefix (possibly empty), one bounded recovery interval and
    /// a supported suffix. Multiple nominal gaps may share this one interval;
    /// separated gaps needing additional planned episodes remain unsupported.
    Partial(ReceptionGap),
}

impl ReceptionCoverage {
    /// True only for complete-window support, never for a partially usable pass.
    pub const fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }

    /// The bounded opportunity advertised before arming, if one was validated.
    pub const fn gap(self) -> Option<ReceptionGap> {
        match self {
            Self::Complete => None,
            Self::Partial(gap) => Some(gap),
        }
    }

    /// Predicted received intervals in original-pass coordinates. The prefix
    /// is absent for late acquisition; neither interval shifts the pass identity.
    pub fn windows(self, duration_ms: u32) -> [Option<ReceptionWindow>; 2] {
        match self {
            Self::Complete => [
                Some(ReceptionWindow {
                    start_ms: 0,
                    end_ms: duration_ms,
                }),
                None,
            ],
            Self::Partial(gap) => [
                (gap.missed.start_ms > 0).then_some(ReceptionWindow {
                    start_ms: 0,
                    end_ms: gap.missed.start_ms,
                }),
                Some(ReceptionWindow {
                    start_ms: gap.missed.end_ms,
                    end_ms: duration_ms,
                }),
            ],
        }
    }

    /// Total predicted usable duration, excluding the explicitly missed interval.
    pub fn received_ms(self, duration_ms: u32) -> u32 {
        match self {
            Self::Complete => duration_ms,
            Self::Partial(gap) => {
                duration_ms.saturating_sub(gap.missed.end_ms.saturating_sub(gap.missed.start_ms))
            }
        }
    }

    pub(super) fn valid(self, window: PlanningWindow) -> bool {
        let Self::Partial(gap) = self else {
            return true;
        };
        let Some(los) = window.aos_at_ms.checked_add(u64::from(window.duration_ms)) else {
            return false;
        };
        let Some(recovered) = gap.latest_join_at_ms.checked_add(MEASURED_RECOVERY_MS) else {
            return false;
        };
        gap.missed.start_ms < gap.missed.end_ms
            && gap.missed.end_ms < window.duration_ms
            && gap.recovery_at_ms >= window.aos_at_ms
            && gap.recovery_at_ms < los
            && gap.deadline_ms
                == gap
                    .recovery_at_ms
                    .saturating_add(REACQUISITION_LIMIT_MS)
                    .min(los)
            && gap.earliest_join_at_ms > gap.recovery_at_ms
            && gap.latest_join_at_ms >= gap.earliest_join_at_ms
            && recovered == window.aos_at_ms + u64::from(gap.missed.end_ms)
            && window.aos_at_ms + u64::from(gap.missed.end_ms) < gap.deadline_ms
            && if gap.late_acquisition {
                gap.missed.start_ms == 0
            } else {
                gap.recovery_at_ms == window.aos_at_ms + u64::from(gap.missed.start_ms)
            }
    }
}
