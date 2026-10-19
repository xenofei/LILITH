//! What a caller hands the meter for one external call (CORE-069; Appendix F "Usage records"):
//! a [`UsageDraft`] and its [`UsageCtx`]. Tasks 11 and 13 record through a [`UsageSink`];
//! Task 15's `Meter` implements it and turns each draft into a usage line. Until then
//! [`LogSink`] only logs the draft.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use lil_schema::money::{Currency, Money};

use crate::tools::stages::BillingBasis;
use crate::workspace::OpenWorkspace;

/// Whose call it was (Appendix F `attribution`): the suite's own, or one the suite only saw
/// (an outside prompt with partner nodes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribution {
    Suite,
    Unattributed,
}

impl Attribution {
    /// `suite` or `unattributed`.
    pub fn wire(self) -> &'static str {
        match self {
            Attribution::Suite => "suite",
            Attribution::Unattributed => "unattributed",
        }
    }
}

/// How the call ended (Appendix F usage `status`). `Pending` is the write-ahead state of a
/// call not yet answered (CORE-071; Task 15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageStatus {
    Pending,
    Succeeded,
    Failed,
    Cancelled,
}

impl UsageStatus {
    /// `pending`, `succeeded`, `failed` or `cancelled`.
    pub fn wire(self) -> &'static str {
        match self {
            UsageStatus::Pending => "pending",
            UsageStatus::Succeeded => "succeeded",
            UsageStatus::Failed => "failed",
            UsageStatus::Cancelled => "cancelled",
        }
    }
}

/// What one external call used.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageDraft {
    /// The tool the call ran for.
    pub tool: String,
    pub provider: String,
    pub model: Option<String>,
    pub route: String,
    pub basis: BillingBasis,
    /// The units that apply (`requests`, `images`, `input_tokens`, …).
    pub units: serde_json::Map<String, serde_json::Value>,
    pub est_cost: Option<(Money, Currency)>,
    /// An opaque account id.
    pub account: Option<String>,
    /// The provider's own usage report, as it came (untrusted).
    pub raw_usage: Option<serde_json::Value>,
    pub attribution: Attribution,
    /// How the call ended (CORE-069).
    pub status: UsageStatus,
    /// From the request to its answer, when it was timed (CORE-069).
    pub latency_ms: Option<u64>,
}

/// The job or call a draft belongs to.
#[derive(Debug, Clone)]
pub struct UsageCtx {
    /// The call's workspace (`None`: the usage goes to `usage-loose.jsonl`).
    pub workspace: Option<Arc<OpenWorkspace>>,
    pub project: Option<String>,
    pub asset: Option<String>,
    pub job_id: Option<String>,
    /// The job id, or the call's `c_<16 hex>`.
    pub call_id: String,
    pub caller_kind: String,
    /// The cap reservation the call or job holds.
    pub reservation: Option<String>,
}

/// Takes usage drafts (Task 15's `Meter`).
pub trait UsageSink: Send + Sync {
    fn record<'a>(&'a self, u: UsageDraft, ctx: UsageCtx) -> BoxFuture<'a, ()>;
}

/// Logs each draft and keeps nothing: the stand-in until Task 15.
#[derive(Debug, Clone, Copy, Default)]
pub struct LogSink;

impl UsageSink for LogSink {
    fn record<'a>(&'a self, u: UsageDraft, ctx: UsageCtx) -> BoxFuture<'a, ()> {
        tracing::info!(
            tool = %u.tool,
            provider = %u.provider,
            route = %u.route,
            basis = u.basis.wire(),
            units = %serde_json::Value::Object(u.units),
            attribution = u.attribution.wire(),
            status = u.status.wire(),
            latency_ms = u.latency_ms,
            job_id = ctx.job_id.as_deref().unwrap_or(""),
            call_id = %ctx.call_id,
            caller_kind = %ctx.caller_kind,
            "usage (not yet recorded: the meter comes with plan 3 Task 15)"
        );
        Box::pin(std::future::ready(()))
    }
}
