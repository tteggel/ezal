//! Explicit, leased assertion that the motors are already rested at startup.
use crate::coordination::CoordinationState;
use crate::session::CommandFreshness;
use core::fmt;

/// Buffer for any reply, including a maximum-width proof and longest rejection.
pub const STARTUP_REST_REPLY_CAPACITY: usize = 160;

/// `startup-rest:<proof>:<lease>`; both identifiers are decimal strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StartupRestIntent {
    /// Recent telemetry proof issued to this WebSocket connection.
    pub proof: u64,
    /// Generation of the current operator grant.
    pub lease: u64,
}

impl StartupRestIntent {
    pub(super) fn parse(message: &str) -> Option<Self> {
        let (proof, lease) = message.split_once(':')?;
        let decimal = |value: &str| {
            (!value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
                .then(|| value.parse::<u64>().ok())
                .flatten()
                .filter(|value| *value != 0)
        };
        Some(Self {
            proof: decimal(proof)?,
            lease: decimal(lease)?,
        })
    }
}

/// Closed set of refusals; no caller text is interpolated into replies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupRestRejection {
    /// This connection does not hold the referenced operator grant.
    Lease,
    /// Missing, expired or invalidated telemetry proof.
    StaleObservation,
    /// Requires unknown startup duty, idle outputs and Manual mode without a fault.
    Unavailable,
}

/// Acknowledges queuing only. Applied duty is reported by subsequent telemetry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StartupRestReply {
    /// Correlates with the pending request without refreshing its validity.
    pub proof: u64,
    /// Whether the output owner received a request to revalidate and apply.
    pub result: Result<(), StartupRestRejection>,
}

impl StartupRestReply {
    /// Encode acknowledgement without claiming the override has been applied.
    pub fn write_json(&self, out: &mut impl fmt::Write) -> fmt::Result {
        write!(
            out,
            "{{\"type\":\"startup_rest_result\",\"proof\":\"{}\",\"ok\":{},\"error\":",
            self.proof,
            self.result.is_ok()
        )?;
        match self.result {
            Ok(()) => out.write_str("null")?,
            Err(reason) => write!(out, "\"{reason:?}\"")?,
        }
        out.write_str("}")
    }
}

impl CoordinationState {
    /// The connection identity comes from the socket; fresh proof and lease
    /// are checked before queuing, then checked again by the output owner.
    pub fn request_startup_rest(
        &mut self,
        client_id: u32,
        intent: StartupRestIntent,
        freshness: &CommandFreshness,
        now_ms: u64,
    ) -> StartupRestReply {
        self.advance(now_ms);
        let result = (|| {
            let operator = self
                .operator(client_id, now_ms)
                .filter(|grant| grant.token.generation() == intent.lease)
                .ok_or(StartupRestRejection::Lease)?;
            let deadline = freshness
                .proof_deadline(intent.proof, now_ms)
                .ok_or(StartupRestRejection::StaleObservation)?;
            self.request_startup_rest_override(operator.token, deadline, now_ms)
                .then_some(())
                .ok_or(StartupRestRejection::Unavailable)
        })();
        StartupRestReply {
            proof: intent.proof,
            result,
        }
    }
}
