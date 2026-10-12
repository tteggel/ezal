use crate::passes::{Pass, Report, Status, MAX_PASSES};

/// Maximum shift from the original selected event when reconciling a complete
/// refreshed forecast. Anchoring prevents small updates walking to another orbit.
pub const MAX_EVENT_SHIFT_S: i64 = 300;

/// Server-side identity for one selection, retained across forecast revisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventId(pub(super) u64);

impl EventId {
    /// Opaque boot-local serial; serialize as a string for browser clients.
    pub const fn serial(self) -> u64 {
        self.0
    }
}

/// Forecast geometry used to associate an event, not its identity or permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PassEvent {
    /// Catalog number, independent of display name or table row.
    pub norad_id: u64,
    /// Rounded acquisition from the complete forecast.
    pub aos_unix_s: i64,
    /// Rounded loss from the complete forecast.
    pub los_unix_s: i64,
}

impl PassEvent {
    /// Convert a forecast row without copying its display metadata.
    pub fn from_pass(pass: Pass) -> Self {
        Self {
            norad_id: pass.satellite.norad_id,
            aos_unix_s: pass.aos_unix_s,
            los_unix_s: pass.los_unix_s,
        }
    }

    /// Only a row of a ready report can become a new tracking selection.
    pub fn offered_by(self, report: &Report) -> bool {
        report.status() == Status::Ready
            && report
                .passes()
                .iter()
                .any(|&pass| Self::from_pass(pass) == self)
    }

    /// Supported reception span and representable UTC, consistent with the planner.
    pub fn is_valid(self) -> bool {
        self.norad_id != 0
            && self.aos_unix_s >= 0
            && self.los_unix_s <= 4_102_444_800
            && self
                .los_unix_s
                .checked_sub(self.aos_unix_s)
                .is_some_and(|duration| (1..=1800).contains(&duration))
    }
}

/// Why a selected event cannot be uniquely reconciled with a new complete forecast.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventError {
    /// Invalid row or forecast larger than its bounded capacity.
    Invalid,
    /// No candidate matches the original event; never substitute another orbit.
    Missing,
    /// Multiple matching candidates, including duplicated rows.
    Ambiguous,
}

/// Associate by satellite, overlap and bounded drift from the original window.
/// A caller must supply a complete forecast, never a partially built row list.
pub fn reconcile_event(
    original: PassEvent,
    forecast: &[PassEvent],
) -> Result<PassEvent, EventError> {
    if !original.is_valid() || forecast.len() > MAX_PASSES {
        return Err(EventError::Invalid);
    }
    let mut matched = None;
    for &candidate in forecast {
        if !candidate.is_valid() {
            return Err(EventError::Invalid);
        }
        if candidate.norad_id == original.norad_id
            && candidate.aos_unix_s.abs_diff(original.aos_unix_s) <= MAX_EVENT_SHIFT_S as u64
            && candidate.los_unix_s.abs_diff(original.los_unix_s) <= MAX_EVENT_SHIFT_S as u64
            && candidate.aos_unix_s < original.los_unix_s
            && candidate.los_unix_s > original.aos_unix_s
        {
            if matched.is_some() {
                return Err(EventError::Ambiguous);
            }
            matched = Some(candidate);
        }
    }
    matched.ok_or(EventError::Missing)
}
