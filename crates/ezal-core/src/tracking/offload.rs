//! Portable planner inputs and compact uploaded plans. An uploaded plan names a
//! deterministic route and its full-result fingerprint. The device rebuilds and
//! evaluates that route through the production planner before it can publish.

use super::{
    Assessment, Planner, PlanningConfig, PlanningError, PlanningWindow, PreparedTrack,
    ReceptionCoverage, RestingStart, Route, TrackingPlan,
};
use crate::actuator::ActuatorConfig;
use crate::position::PositionCalibration;
use crate::wire::{Reader, Wire, Writer};
use core::fmt::{self, Write};

/// Wire schema. Change whenever snapshot encoding or planner semantics change.
pub const VERSION: u32 = 1;
/// Bounded portable input; only one is held by the device.
pub const INPUT_CAPACITY: usize = 4096;

/// Immutable device-issued inputs, with exact floating-point bit patterns and
/// complete relay, duty and motion history. Browsers cannot replace these values
/// on the device; uploads carry only a route selection and comparison digest.
#[derive(Clone, Debug)]
pub struct PlanningInput {
    /// Boot-local ticket identity.
    pub job: u64,
    /// Same minute knots and observer geometry as the lazy device track.
    pub track: PreparedTrack,
    /// Captured execution window, including rehearsal mapping.
    pub window: PlanningWindow,
    /// Installed measured travel and feedback mapping.
    pub calibration: PositionCalibration,
    /// Output owner's complete serviced resting snapshot.
    pub start: RestingStart,
    /// Exact motor, timing and measurement scenarios.
    pub config: PlanningConfig,
}
wire_struct!(PlanningInput {
    job,
    track,
    window,
    calibration,
    start,
    config
});
impl PlanningInput {
    /// Serialize without allocation, rejecting an undersized destination.
    pub fn encode(&self, out: &mut [u8]) -> Option<usize> {
        let mut writer = Writer { data: out, at: 0 };
        VERSION.encode(&mut writer)?;
        Wire::encode(self, &mut writer)?;
        Some(writer.at)
    }
    /// Decode only this schema, bounded collections and the complete payload.
    pub fn decode(data: &[u8]) -> Option<Self> {
        let mut reader = Reader(data);
        if u32::decode(&mut reader)? != VERSION {
            return None;
        }
        let value = <Self as Wire>::decode(&mut reader)?;
        reader.0.is_empty().then_some(value)
    }
    /// Run the identical route search used by firmware.
    pub fn planner(self) -> Result<Planner<PreparedTrack>, PlanningError> {
        Planner::new(
            self.track,
            self.window,
            self.calibration,
            self.start,
            self.config,
        )
    }
}

/// Compact plan upload: a reproducible candidate instead of thousands of
/// redundant route knots. This digest checks equivalence, never authorizes motion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UploadedPlan {
    /// Ticket issued by the device, never a browser revision or timestamp.
    pub job: u64,
    /// Index in the shared finite route set.
    pub candidate: u8,
    /// Evaluate the partial-reception search for this candidate.
    pub partial: bool,
    /// Fingerprint of the entire chosen plan, including route, coverage and metrics.
    pub digest: u64,
}
impl UploadedPlan {
    /// Produce an upload only from an actual supported planner result.
    pub fn new(job: u64, plan: &TrackingPlan) -> Self {
        let candidate = (0..super::route::CANDIDATES)
            .find(|&i| super::route::choice(i) == plan.route.choice())
            .expect("planner candidate");
        Self {
            job,
            candidate: candidate as u8,
            partial: !plan.reception().is_complete(),
            digest: fingerprint(plan),
        }
    }
    /// Check the complete device-rebuilt result. A mismatch rejects the upload.
    pub fn matches(self, plan: &TrackingPlan) -> bool {
        fingerprint(plan) == self.digest
    }
}

struct Digest(u64);
impl Write for Digest {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        for byte in value.bytes() {
            self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x100000001b3);
        }
        Ok(())
    }
}
fn fingerprint(plan: &TrackingPlan) -> u64 {
    PlanIdentity {
        route: &plan.route,
        window: &plan.window,
        config: &plan.config,
        calibration: &plan.calibration,
        actuator: plan.actuator,
        assessment: &plan.assessment,
        reception: plan.reception(),
    }
    .fingerprint()
}

/// Borrow the active workspace to compare a receipt without copying its route
/// or replacing the best result retained by the local search.
pub(super) struct PlanIdentity<'a> {
    pub route: &'a Route,
    pub window: &'a PlanningWindow,
    pub config: &'a PlanningConfig,
    pub calibration: &'a PositionCalibration,
    pub actuator: ActuatorConfig,
    pub assessment: &'a Assessment,
    pub reception: ReceptionCoverage,
}
impl PlanIdentity<'_> {
    pub(super) fn fingerprint(&self) -> u64 {
        let mut digest = Digest(0xcbf29ce484222325);
        // Route coordinates dominate the data: hash their exact bits directly,
        // avoiding thousands of float-to-decimal conversions on the Pico.
        for point in &self.route.points {
            for byte in point
                .at_ms
                .to_le_bytes()
                .into_iter()
                .chain(point.az.to_le_bytes())
                .chain(point.el.to_le_bytes())
            {
                digest.0 = (digest.0 ^ u64::from(byte)).wrapping_mul(0x100000001b3);
            }
        }
        let _ = write!(
            &mut digest,
            "{:?}{:?}{:?}{:?}{:?}{:?}{:?}",
            self.route.choice(),
            self.window,
            self.config,
            self.calibration,
            self.actuator,
            self.assessment,
            self.reception
        );
        digest.0
    }
}
