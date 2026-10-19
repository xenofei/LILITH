//! What a caller hands the meter for one external call (CORE-069; Appendix F "Usage records"):
//! a [`UsageDraft`] and its [`UsageCtx`], given to a [`UsageSink`]. The core's sink is
//! [`crate::meter::Meter`], which turns each draft into a usage line.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use lil_protocol::ErrorBody;
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

/// A write-ahead usage record that was begun ([`UsageSink::begin`]) and is not settled yet:
/// its row's id (`us_<16 hex>`, `None` when the sink keeps no rows or the row could not be
/// written), and the draft and context it was begun with.
#[derive(Debug, Clone)]
pub struct PendingUsage {
    pub id: Option<String>,
    pub draft: UsageDraft,
    pub ctx: UsageCtx,
}

/// Takes usage drafts (the core's is [`crate::meter::Meter`]).
///
/// An external call records its usage in one of two ways: [`UsageSink::record`] once it is
/// over (a draft with its outcome), or, write-ahead (CORE-071), [`UsageSink::begin`] before
/// the request is sent and [`UsageSink::settle`] with the outcome after it, so a core that
/// dies in between leaves a `pending` record. [`UsageSink::abandon`] drops a begun record
/// whose request was never sent. The defaults keep no rows: `settle` records the draft with
/// its outcome, so a sink that only implements `record` sees one draft per call.
///
/// A `begin` that fails is the caller's error: the request must not be sent (CORE-071), so
/// the call or job fails with it, and nothing is recorded after the fact.
pub trait UsageSink: Send + Sync {
    fn record<'a>(&'a self, u: UsageDraft, ctx: UsageCtx) -> BoxFuture<'a, ()>;

    /// # Errors
    /// Why the write-ahead record could not be written (`state.unavailable`).
    fn begin<'a>(
        &'a self,
        u: UsageDraft,
        ctx: UsageCtx,
    ) -> BoxFuture<'a, Result<PendingUsage, ErrorBody>> {
        Box::pin(std::future::ready(Ok(PendingUsage {
            id: None,
            draft: u,
            ctx,
        })))
    }

    fn settle<'a>(
        &'a self,
        p: PendingUsage,
        status: UsageStatus,
        latency_ms: Option<u64>,
    ) -> BoxFuture<'a, ()> {
        let mut draft = p.draft;
        draft.status = status;
        draft.latency_ms = latency_ms;
        self.record(draft, p.ctx)
    }

    fn abandon<'a>(&'a self, p: PendingUsage) -> BoxFuture<'a, ()> {
        let _ = p;
        Box::pin(std::future::ready(()))
    }
}
