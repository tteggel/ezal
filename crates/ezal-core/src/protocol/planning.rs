//! Leased offload intent. All input authority remains on the device.
use super::decimal;
use crate::coordination::CoordinationState;
use crate::session::CommandFreshness;
use crate::tracking::offload::UploadedPlan;

/// Maximum snapshot payload in one binary WebSocket message.
pub const PLANNING_CHUNK_SIZE: usize = 192;
/// Little-endian job (u64), offset (u16), total snapshot size (u16).
pub const PLANNING_CHUNK_HEADER_SIZE: usize = 12;

/// `plan:proof:lease:job:get` or `plan:proof:lease:job:put:candidate:partial:digest`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct PlanningIntent {
    pub proof: u64,
    pub lease: u64,
    pub job: u64,
    pub upload: Option<UploadedPlan>,
}
impl PlanningIntent {
    pub(super) fn parse(message: &str) -> Option<Self> {
        let mut fields = message.split(':');
        let proof = decimal(fields.next()?)?;
        let lease = decimal(fields.next()?)?;
        let job = decimal(fields.next()?)?;
        let upload = match fields.next()? {
            "get" => None,
            "put" => {
                let candidate = decimal(fields.next()?)?;
                if usize::from(candidate) >= crate::tracking::CANDIDATES {
                    return None;
                }
                let partial = match fields.next()? {
                    "0" => false,
                    "1" => true,
                    _ => return None,
                };
                Some(UploadedPlan {
                    job,
                    candidate,
                    partial,
                    digest: decimal(fields.next()?)?,
                })
            }
            _ => return None,
        };
        (fields.next().is_none() && lease > 0 && job > 0).then_some(Self {
            proof,
            lease,
            job,
            upload,
        })
    }
}
impl CoordinationState {
    /// Admit only a fresh observation from the current controller connection.
    /// This queues validation; it does not arm, move, renew a lease or publish a plan.
    pub fn request_planning(
        &mut self,
        client: u32,
        intent: PlanningIntent,
        freshness: &CommandFreshness,
        now: u64,
    ) -> bool {
        if !freshness.proves_fresh(intent.proof, now)
            || !self
                .operator(client, now)
                .is_some_and(|g| g.token.generation() == intent.lease)
            || !self.network().ready(now)
            || self.tracking().planning_ticket().map(|t| t.serial()) != Some(intent.job)
        {
            return false;
        }
        if let Some(upload) = intent.upload {
            self.upload_plan(upload)
        } else {
            self.planning_status().is_some_and(|s| s.offload_ready)
        }
    }
}
