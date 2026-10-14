//! Bounded tracking wire projections. No route/source clones, propagation, I/O,
//! lease renewal or lifecycle transitions occur while reading telemetry.
//! Reasons are written in their `Debug` form, so every reason type nests closed
//! enums down to fieldless leaves (`Rejected(Lifecycle(StateRevision))`), which the
//! dashboard explains by innermost variant.

use core::fmt;

use crate::coordination::CoordinationState;

use crate::feedback::{FeedbackHealth, FeedbackSnapshot, FEEDBACK_MAX_AGE_MS};
use crate::operator::OperatorGrant;
use crate::session::CommandFreshness;
use crate::tracking::*;

// Representation adapters use the owner's public serialized boundary; the
// coordination domain remains independent of the browser wire format.
impl CoordinationState {
    /// Copy a bounded read-only projection, without servicing or renewing a run.
    pub fn tracking_telemetry(&self, client_id: u32, now_ms: u64) -> TrackingTelemetry {
        TrackingTelemetry::capture(
            self.tracking(),
            self.feedback(),
            self.operator(client_id, now_ms),
            now_ms,
        )
    }

    /// Validate browser intent against its actual connection and current device
    /// forecast before passing the ordered request to the serialized runtime.
    pub fn request_tracking(
        &mut self,
        client_id: u32,
        intent: TrackingIntent,
        freshness: &CommandFreshness,
        now_ms: u64,
        offered: impl FnOnce(PassEvent) -> bool,
    ) -> TrackingReply {
        let result = self.tracking_operation(now_ms, |runtime, observation| {
            let grant = runtime
                .operator(now_ms)
                .filter(|g| g.token.client_id() == u64::from(client_id));
            let request = intent.authorize(grant, freshness, now_ms)?;
            if let TrackingAction::Select(pass) = intent.action {
                if !offered(pass) {
                    return Err(TrackingRejection::ForecastChanged);
                }
            }
            runtime
                .request(request, observation)
                .map_err(TrackingRejection::Rejected)
        });
        TrackingReply {
            sequence: intent.sequence,
            result,
        }
    }
}

/// `track:<proof>:<lease>:<sequence>:<revision>:<action>[:norad:aos:los]`.
/// Identifiers are decimal strings on the wire to preserve all 64 bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackingIntent {
    /// Recent server telemetry challenge; never a browser timestamp.
    pub proof: u64,
    /// Generation of the grant observed by this connection.
    pub lease: u64,
    /// Nonzero ordered intent number within this grant.
    pub sequence: u64,
    /// Device revision observed before issuing the action.
    pub revision: u64,
    /// Explicit operator intent, independent of any browser timer.
    pub action: TrackingAction,
}

impl TrackingIntent {
    pub(super) fn parse(message: &str) -> Option<Self> {
        fn decimal<T: core::str::FromStr>(s: &str) -> Option<T> {
            (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse().ok())
                .flatten()
        }
        let mut fields = message.split(':');
        let proof = decimal(fields.next()?)?;
        let lease = decimal(fields.next()?)?;
        let sequence = decimal(fields.next()?)?;
        let revision = decimal(fields.next()?)?;
        let action = match fields.next()? {
            "select" => TrackingAction::Select(PassEvent {
                norad_id: decimal(fields.next()?)?,
                aos_unix_s: decimal(fields.next()?)?,
                los_unix_s: decimal(fields.next()?)?,
            }),
            "arm" => TrackingAction::Arm,
            "prepare" => TrackingAction::PrepareNow,
            "cancel" => TrackingAction::Cancel,
            "manual" => TrackingAction::Manual,
            _ => return None,
        };
        if fields.next().is_some() || lease == 0 || sequence == 0 {
            return None;
        }
        Some(Self {
            proof,
            lease,
            sequence,
            revision,
            action,
        })
    }

    /// Connection identity is supplied by the transport, never by this message.
    pub fn authorize(
        self,
        grant: Option<OperatorGrant>,
        freshness: &CommandFreshness,
        now_ms: u64,
    ) -> Result<TrackingRequest, TrackingRejection> {
        let grant = grant
            .filter(|g| g.token.generation() == self.lease && now_ms < g.expires_at_ms)
            .ok_or(TrackingRejection::Lease)?;
        // Cancellation remains possible with an old observation/revision, but
        // still requires the current connection/grant and ordered intent.
        if !matches!(self.action, TrackingAction::Cancel | TrackingAction::Manual)
            && !freshness.proves_fresh(self.proof, now_ms)
        {
            return Err(TrackingRejection::StaleObservation);
        }
        Ok(TrackingRequest {
            operator: grant.token,
            sequence: self.sequence,
            expected_revision: self.revision,
            action: self.action,
        })
    }
}

/// Readable failures; no caller-supplied text is interpolated into JSON.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackingRejection {
    /// This connection no longer owns the referenced lease.
    Lease,
    /// Starting/changing tracking needs a current device observation.
    StaleObservation,
    /// The selected row is no longer offered by the device.
    ForecastChanged,
    /// The runtime rejected ordering, phase, revision or source validity.
    Rejected(RuntimeError),
}

/// Fixed acknowledgement buffer: maximum-width identifiers and the longest
/// rejection need 140 bytes.
pub const TRACKING_REPLY_CAPACITY: usize = 192;

/// Acknowledgement is correlated with intent, and never renews observation age.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackingReply {
    /// Correlation with the browser intent, represented as a string.
    pub sequence: u64,
    /// Accepted revision, or an explicit failure with no implied retry.
    pub result: Result<u64, TrackingRejection>,
}

impl TrackingReply {
    /// Write a bounded acknowledgement without issuing another freshness proof.
    pub fn write_json(&self, out: &mut impl fmt::Write) -> fmt::Result {
        write!(
            out,
            "{{\"type\":\"tracking_result\",\"sequence\":\"{}\",\"ok\":{},\"revision\":",
            self.sequence,
            self.result.is_ok()
        )?;
        match self.result {
            Ok(revision) => write!(out, "\"{revision}\",\"error\":null}}"),
            Err(error) => write!(out, "null,\"error\":\"{error:?}\"}}"),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Preview {
    choice: RouteChoice,
    window: PlanningWindow,
    reception: ReceptionCoverage,
    prepare_at_ms: u64,
    lead_ms: u64,
    bound_deg: f32,
    tolerance_deg: f32,
    reserve_deg: f32,
}

/// Small copy taken alongside manual telemetry under the same state borrow.
#[derive(Clone, Copy, Debug)]
pub struct TrackingTelemetry {
    snapshot: TrackingSnapshot,
    operator: Option<u64>,
    preview: Option<Preview>,
    deviation: DeviationStatus,
    recovery: Option<RecoveryStatus>,
    summary: Option<TrackingSummary>,
    feedback_age_ms: Option<u64>,
    feedback_stale: bool,
    now_ms: u64,
}

impl TrackingTelemetry {
    /// Copy display values while borrowing the immutable runtime and feedback.
    pub fn capture(
        runtime: &TrackingRuntime,
        feedback: FeedbackSnapshot,
        operator: Option<OperatorGrant>,
        now_ms: u64,
    ) -> Self {
        let feedback_age_ms = feedback
            .last_valid
            .and_then(|s| now_ms.checked_sub(s.sampled_at_ms()));
        Self {
            snapshot: runtime.snapshot(),
            operator: operator.map(|g| g.token.generation()),
            preview: runtime.bound_plan().map(|bound| {
                let plan = bound.plan();
                Preview {
                    choice: plan.route.choice(),
                    window: plan.window,
                    reception: plan.reception(),
                    prepare_at_ms: plan.assessment.prepare_at_ms,
                    lead_ms: plan.assessment.preparation_lead_ms,
                    bound_deg: plan.assessment.bounded_peak_deg,
                    tolerance_deg: plan.config.tolerance_deg,
                    reserve_deg: plan.config.uncertainty_deg,
                }
            }),
            deviation: runtime.deviation(now_ms),
            recovery: runtime.recovery(),
            summary: runtime.last_summary(),
            feedback_age_ms,
            feedback_stale: feedback.health != FeedbackHealth::Healthy
                || feedback_age_ms.is_none_or(|age| age >= FEEDBACK_MAX_AGE_MS),
            now_ms,
        }
    }

    /// Encode the copied projection; no runtime borrow survives formatting.
    pub fn write_json(&self, out: &mut impl fmt::Write) -> fmt::Result {
        let s = self.snapshot;
        write!(out, "{{\"revision\":\"{}\",\"operator\":", s.revision)?;
        identity(out, self.operator)?;
        write!(
            out,
            ",\"mode\":\"{}\",\"phase\":\"{}\",\"run\":",
            match s.mode {
                TrackingMode::Manual => "manual",
                TrackingMode::Tracking => "tracking",
            },
            phase(s.phase)
        )?;
        identity(out, s.run.map(RunToken::serial))?;
        out.write_str(",\"selection\":")?;
        if let Some(selection) = s.selection {
            let pass = selection.current.unwrap_or(selection.original);
            write!(out, "{{\"id\":\"{}\",\"norad_id\":\"{}\",\"aos_unix_s\":{},\"los_unix_s\":{},\"associated\":{}}}", selection.id.serial(), pass.norad_id, pass.aos_unix_s, pass.los_unix_s, selection.current.is_some())?;
        } else {
            out.write_str("null")?;
        }
        write!(
            out,
            ",\"plan\":{{\"status\":\"{}\",\"reason\":",
            plan_status(s.plan)
        )?;
        if let PlanStatus::Blocked(problem) = s.plan {
            write!(out, "\"{problem:?}\"")?;
        } else {
            out.write_str("null")?;
        }
        out.write_str(",\"preview\":")?;
        if let Some(p) = self.preview {
            let route = match p.choice.kind {
                RouteKind::Conventional => "conventional",
                RouteKind::Inverted => "inverted",
                RouteKind::OverTop { .. } => "over-the-top",
            };
            let prepare_unix_s =
                p.window.aos_unix_s + (p.prepare_at_ms as f64 - p.window.aos_at_ms as f64) / 1000.0;
            write!(out, "{{\"route\":\"{route}\",\"azimuth_turns\":{},\"prepare_unix_s\":{prepare_unix_s:.3},\"preparation_lead_ms\":{},\"bound_deg\":{:.3},\"tolerance_deg\":{:.3},\"reserve_deg\":{:.3},\"reception\":", p.choice.azimuth_turns, p.lead_ms, p.bound_deg, p.tolerance_deg, p.reserve_deg)?;
            reception(out, p.window, p.reception)?;
            out.write_str("}")?;
        } else {
            out.write_str("null")?;
        }
        write!(
            out,
            "}},\"feedback_stale\":{},\"feedback_age_ms\":",
            self.feedback_stale
        )?;
        number(out, self.feedback_age_ms)?;
        out.write_str(",\"deviation\":")?;
        deviation(out, self.deviation, self.now_ms)?;
        out.write_str(",\"recovery\":")?;
        if let Some(r) = self.recovery {
            write!(out, "{{\"episode\":\"{}\",\"cause\":\"{:?}\",\"attempts\":{},\"planning\":{},\"work_steps\":{},\"remaining_ms\":{},\"launch_in_ms\":", r.episode.serial(), r.episode.cause(), r.attempts, r.planning, r.work_steps, r.episode.deadline_ms().saturating_sub(self.now_ms))?;
            number(out, r.launch_at_ms.map(|at| at.saturating_sub(self.now_ms)))?;
            out.write_str(",\"intercept_in_ms\":")?;
            number(
                out,
                r.intercept_at_ms.map(|at| at.saturating_sub(self.now_ms)),
            )?;
            out.write_str(",\"hold\":")?;
            if let Some(hold) = r.hold {
                write!(out, "\"{hold:?}\"")?;
            } else {
                out.write_str("null")?;
            }
            out.write_str("}")?;
        } else {
            out.write_str("null")?;
        }
        out.write_str(",\"termination\":")?;
        termination(out, s.termination)?;
        out.write_str(",\"summary\":")?;
        if let Some(summary) = self.summary {
            out.write_str("{\"termination\":")?;
            termination(out, Some(summary.termination))?;
            out.write_str(",\"reception\":")?;
            reception(out, summary.window, summary.reception)?;
            out.write_str(",\"deviation\":")?;
            deviation(out, summary.deviation, self.now_ms)?;
            out.write_str("}")?;
        } else {
            out.write_str("null")?;
        }
        out.write_str("}")
    }
}

fn identity(out: &mut impl fmt::Write, value: Option<u64>) -> fmt::Result {
    match value {
        Some(v) => write!(out, "\"{v}\""),
        None => out.write_str("null"),
    }
}
fn number(out: &mut impl fmt::Write, value: Option<u64>) -> fmt::Result {
    match value {
        Some(v) => write!(out, "{v}"),
        None => out.write_str("null"),
    }
}
fn angle(out: &mut impl fmt::Write, value: Option<f32>) -> fmt::Result {
    match value.filter(|v| v.is_finite()) {
        Some(v) => write!(out, "{v:.3}"),
        None => out.write_str("null"),
    }
}
fn deviation(out: &mut impl fmt::Write, d: DeviationStatus, now: u64) -> fmt::Result {
    out.write_str("{\"run\":")?;
    identity(out, d.run.map(RunToken::serial))?;
    write!(out, ",\"warning\":{},\"maximum_deg\":", d.warning)?;
    angle(out, d.maximum_deg)?;
    write!(
        out,
        ",\"outside_ms\":{},\"current\":",
        d.outside_tolerance_ms
    )?;
    if let Some(r) = d.current {
        write!(out, "{{\"age_ms\":{},\"desired\":[{:.3},{:.3}],\"measured\":[{:.3},{:.3}],\"degrees\":{:.3},\"uncertainty_deg\":{:.3}}}", now.saturating_sub(r.sampled_at_ms), r.desired.azimuth_deg, r.desired.elevation_deg, r.measured.azimuth_deg, r.measured.elevation_deg, r.degrees, r.uncertainty_deg)?;
    } else {
        out.write_str("null")?;
    }
    out.write_str("}")
}
fn termination(out: &mut impl fmt::Write, t: Option<TrackingTermination>) -> fmt::Result {
    if let Some(t) = t {
        write!(out, "{{\"reason\":\"{:?}\",\"event\":", t.reason)?;
        identity(out, t.event.map(EventId::serial))?;
        out.write_str(",\"run\":")?;
        identity(out, t.run.map(RunToken::serial))?;
        out.write_str("}")
    } else {
        out.write_str("null")
    }
}
fn reception(
    out: &mut impl fmt::Write,
    window: PlanningWindow,
    coverage: ReceptionCoverage,
) -> fmt::Result {
    write!(
        out,
        "{{\"aos_unix_s\":{:.3},\"duration_ms\":{},\"windows\":[",
        window.aos_unix_s, window.duration_ms
    )?;
    for (index, interval) in coverage
        .windows(window.duration_ms)
        .into_iter()
        .flatten()
        .enumerate()
    {
        if index > 0 {
            out.write_str(",")?;
        }
        write!(out, "[{},{}]", interval.start_ms, interval.end_ms)?;
    }
    out.write_str("],\"missed\":")?;
    match coverage {
        ReceptionCoverage::Complete => out.write_str("null")?,
        ReceptionCoverage::Partial(gap) => {
            write!(out, "[{},{}]", gap.missed.start_ms, gap.missed.end_ms)?
        }
    }
    out.write_str("}")
}
fn phase(phase: TrackingPhase) -> &'static str {
    match phase {
        TrackingPhase::Idle => "idle",
        TrackingPhase::Armed => "armed",
        TrackingPhase::Preparing => "preparing",
        TrackingPhase::Ready => "ready",
        TrackingPhase::Following => "following",
        TrackingPhase::Reacquiring => "reacquiring",
        TrackingPhase::Complete => "complete",
        TrackingPhase::Cancelled => "cancelled",
        TrackingPhase::Stopped => "stopped",
    }
}
fn plan_status(status: PlanStatus) -> &'static str {
    match status {
        PlanStatus::Unplanned => "unplanned",
        PlanStatus::Planning => "planning",
        PlanStatus::Supported => "supported",
        PlanStatus::SupportedPartial => "partial",
        PlanStatus::Frozen => "frozen",
        PlanStatus::Blocked(_) => "blocked",
    }
}
