//! The device's orbital catalog and its single publish path.
//!
//! [`orbit`](crate::orbit) parses; this module publishes. Only a completed
//! download publishes, and per satellite a newer-or-equal epoch replaces the
//! current entry while an older one is ignored. HTTP is an explicit
//! installation decision, not an authentication claim; only the device
//! download task publishes data.

use crate::http;
use crate::orbit::{Orbit, Update, SATELLITE_COUNT};

/// Bounded HTTP framing and TLE staging in one publication boundary. A partial
/// or failed response cannot produce the value [`Catalog::commit`] requires.
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
    /// The validated elements, for propagation.
    pub(crate) const fn orbit(&self) -> &Orbit {
        &self.orbit
    }
}

/// A download completed before the catalog's latest publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceOrder;

/// Bounded in-memory catalog. Each slot holds `SATELLITES[index]`'s elements
/// with the publication that supplied them, so entries can be captured for
/// execution. Failed or older downloads never replace good data.
#[derive(Clone, Debug)]
pub struct Catalog {
    entries: [Option<OrbitalSnapshot>; SATELLITE_COUNT],
    revision: u64,
    received_at_ms: Option<u64>,
}

impl Default for Catalog {
    fn default() -> Self {
        Self::new()
    }
}

impl Catalog {
    /// Empty at boot: no baked-in orbital elements silently masquerade as fresh.
    pub const fn new() -> Self {
        Self {
            entries: [const { None }; SATELLITE_COUNT],
            revision: 0,
            received_at_ms: None,
        }
    }

    /// Publish a download completed at monotonic `received_at_ms`. Each staged
    /// element set replaces its slot unless the slot holds a newer epoch, and
    /// carries the next revision. Unusable entries keep both their old orbit
    /// and its old publication. Returns the number of entries published; a
    /// download that publishes none advances neither revision nor time.
    pub fn commit(
        &mut self,
        download: CompletedOrbitalDownload,
        received_at_ms: u64,
    ) -> Result<usize, SourceOrder> {
        if self.received_at_ms.is_some_and(|old| received_at_ms < old) {
            return Err(SourceOrder);
        }
        let publication = OrbitalPublication {
            revision: self.revision + 1,
            received_at_ms,
        };
        let mut published = 0;
        for (entry, staged) in self.entries.iter_mut().zip(download.0.finish()) {
            let Some(orbit) = staged else {
                continue;
            };
            if entry
                .as_ref()
                .is_none_or(|old| orbit.epoch_unix_s >= old.orbit.epoch_unix_s)
            {
                *entry = Some(OrbitalSnapshot { orbit, publication });
                published += 1;
            }
        }
        if published > 0 {
            self.revision = publication.revision;
            self.received_at_ms = Some(received_at_ms);
        }
        Ok(published)
    }

    /// The validated entry for `SATELLITES[index]`, if any.
    pub fn orbit(&self, index: usize) -> Option<&Orbit> {
        self.entries
            .get(index)?
            .as_ref()
            .map(OrbitalSnapshot::orbit)
    }

    /// Validated entries; callers select services and enforce current epoch age.
    pub fn orbits(&self) -> impl Iterator<Item = &Orbit> {
        self.entries.iter().flatten().map(OrbitalSnapshot::orbit)
    }

    /// Epoch of each catalog entry, in `SATELLITES` order, so availability can
    /// be judged later without holding the catalog.
    pub fn epochs(&self) -> [Option<f64>; SATELLITE_COUNT] {
        core::array::from_fn(|index| self.orbit(index).map(|orbit| orbit.epoch_unix_s))
    }

    /// Capture one satellite's immutable elements and original publication.
    pub fn snapshot(&self, norad_id: u64) -> Option<OrbitalSnapshot> {
        self.entries
            .iter()
            .flatten()
            .find(|entry| entry.norad_id() == norad_id)
            .cloned()
    }
}
