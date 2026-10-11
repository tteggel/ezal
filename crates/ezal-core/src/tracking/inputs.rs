//! Owned, validated orbital inputs. HTTP is an explicit installation decision,
//! not an authentication claim; only the device download task publishes data.

use super::{
    ExecutionClock, PlanTicket, PlanningConfig, PlanningWindow, TimingError, TimingPolicy,
    TrackingPlan,
};
use crate::http;
use crate::ntp::UtcClock;
use crate::orbit::{
    Catalog, GroundStation, LookAngle, Observer, Orbit, Update, MAX_FUTURE_EPOCH_S, MAX_TLE_AGE_S,
    SATELLITE_COUNT,
};
use crate::passes::Ephemeris;

/// Provenance of accepted orbital data. The current installation accepts the
/// network-tampering risk of CelesTrak's unauthenticated HTTP feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrbitalTransport {
    /// Complete, framed HTTP response from the configured CelesTrak endpoint.
    CelestrakHttp,
}

/// Bounded HTTP framing and TLE staging in one publication boundary. A partial
/// or failed response cannot produce the value required by `OrbitalCatalog`.
pub struct OrbitalDownload {
    response: http::Response,
    update: Update,
    failure: Option<http::Error>,
}

/// Complete, successfully framed response; element validation is still applied
/// by the catalog. This proves framing, not authentication or orbital accuracy.
pub struct CompletedOrbitalDownload(Update);

impl OrbitalDownload {
    /// Start one download using the device's validated current UTC.
    pub fn new(now_unix_s: i64) -> Self {
        Self {
            response: http::Response::new(),
            update: Update::new(now_unix_s),
            failure: None,
        }
    }
    /// Stream socket bytes with arbitrary read boundaries. The first error is
    /// latched, so calling finish after a decoding failure cannot publish data.
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), http::Error> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if let Err(error) = self.response.feed(bytes, |body| self.update.push(body)) {
            self.failure = Some(error);
            return Err(error);
        }
        Ok(())
    }
    /// Content-Length/chunked message completed; close-delimited requires EOF.
    pub fn is_complete(&self) -> bool {
        self.failure.is_none() && self.response.is_done()
    }
    /// Finish at validated framing completion or socket EOF, never on timeout.
    pub fn finish(self) -> Result<CompletedOrbitalDownload, http::Error> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        self.response.finish()?;
        Ok(CompletedOrbitalDownload(self.update))
    }
}

/// Download metadata retained with the exact accepted element set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrbitalPublication {
    /// Device-owned publication identity; failed/older updates do not advance it.
    pub revision: u64,
    /// Original monotonic completion instant, never renewed by reading a cache.
    pub received_at_ms: u64,
    /// Source channel, not an assertion of cryptographic authenticity.
    pub transport: OrbitalTransport,
}

/// An owned element set with publication metadata. No mutable orbit accessor
/// allows a caller to change the epoch while retaining its SGP4 constants.
#[derive(Clone, Debug)]
pub struct OrbitalSnapshot {
    orbit: Orbit,
    publication: OrbitalPublication,
}

impl OrbitalSnapshot {
    /// Stable catalog number, independent of row order or feed names.
    pub fn norad_id(&self) -> u64 {
        self.orbit.satellite.norad_id
    }
    /// Fractional epoch, used for age rather than the time of download.
    pub const fn epoch_unix_s(&self) -> f64 {
        self.orbit.epoch_unix_s
    }
    /// Metadata of the exact download that supplied these elements.
    pub const fn publication(&self) -> OrbitalPublication {
        self.publication
    }
}

/// A catalog whose entries can be captured for execution. Ordinary advisory
/// `Catalog` values cannot be relabeled as execution snapshots. The firmware
/// calls `commit` only after the bounded HTTP decoder completes successfully.
pub struct OrbitalCatalog {
    catalog: Catalog,
    publications: [Option<OrbitalPublication>; SATELLITE_COUNT],
    revision: u64,
    received_at_ms: Option<u64>,
}

impl Default for OrbitalCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl OrbitalCatalog {
    /// Boot without persisted or implicitly trusted elements.
    pub const fn new() -> Self {
        Self {
            catalog: Catalog::new(),
            publications: [None; SATELLITE_COUNT],
            revision: 0,
            received_at_ms: None,
        }
    }
    /// Publish a completed HTTP download. Validation and non-regressive epoch
    /// replacement share the advisory catalog's parser. Unusable entries leave
    /// both the old orbit and its old metadata untouched.
    pub fn commit(
        &mut self,
        mut download: CompletedOrbitalDownload,
        received_at_ms: u64,
    ) -> Result<usize, InputError> {
        if self.received_at_ms.is_some_and(|old| received_at_ms < old) {
            return Err(InputError::SourceOrder);
        }
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(InputError::SourceOrder)?;
        let mask = self.catalog.commit_mask(&mut download.0);
        if mask == 0 {
            return Ok(0);
        }
        self.revision = revision;
        self.received_at_ms = Some(received_at_ms);
        for (index, publication) in self.publications.iter_mut().enumerate() {
            if mask & (1 << index) != 0 {
                *publication = Some(OrbitalPublication {
                    revision,
                    received_at_ms,
                    transport: OrbitalTransport::CelestrakHttp,
                });
            }
        }
        Ok(mask.count_ones() as usize)
    }
    /// Reuse the same accepted elements for the forecast without copying them.
    pub const fn catalog(&self) -> &Catalog {
        &self.catalog
    }
    /// Capture one satellite's immutable elements and original publication.
    pub fn snapshot(&self, norad_id: u64) -> Option<OrbitalSnapshot> {
        (0..SATELLITE_COUNT).find_map(|index| {
            let orbit = self.catalog.orbit(index)?;
            (orbit.satellite.norad_id == norad_id).then(|| OrbitalSnapshot {
                orbit: orbit.clone(),
                publication: self.publications[index].expect("accepted slot has publication"),
            })
        })
    }
}

/// Input rejection, suitable for both a blocked preview and a terminal run reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputError {
    /// No matching elements in the device's validated catalog.
    MissingOrbit,
    /// Orbital epoch cannot support the whole window plus the timing allowance.
    StaleOrbit,
    /// Download completion predates an already accepted publication or this boot.
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
    settings: InputSettings,
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
            || ticket.revisions().orbit != orbit.publication.revision
            || ticket.revisions().clock != anchor.revision()
            || config.preparation_timing != ticket.preparation_timing()
            || !window.aos_unix_s.is_finite()
            || aos_utc != window.aos_unix_s
            || !(1..=1_800_000).contains(&window.duration_ms)
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
            settings,
            station,
            clock,
            ticket,
            window,
            config: config.clone(),
        };
        inputs.validate_preview(current, now_ms)?;
        Ok(inputs)
    }
    /// Immutable publication metadata, including its unauthenticated transport.
    pub const fn orbit(&self) -> &OrbitalSnapshot {
        &self.orbit
    }
    /// Captured station and timing assumptions.
    pub const fn settings(&self) -> InputSettings {
        self.settings
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
        if now_ms < self.orbit.publication.received_at_ms {
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
        self.orbit.orbit.look_at(&self.station, unix_s)
    }
}
