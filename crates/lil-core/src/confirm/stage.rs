//! Stage 5 (CORE-015): [`AgentConfirm`], the agent safety confirmations and the caps' half, one
//! prompt at most.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use lil_protocol::{CallerKind, Cover, ErrorBody, codes};
use lil_schema::money::Money;
use serde_json::Value;

use super::{Confirm, ConfirmRequest, McpConfirmation, Replay};
use crate::meter::CapCheck;
use crate::state::{StateDb, StateError};
use crate::tools::{
    AllowOnce, Approval, CapHit, ConfirmDecision, ConfirmInput, ConfirmStage, ConfirmedVia,
    IdemRef, ReplayDecision, cap_exceeded,
};

/// Agent safety confirmations and cap confirmations (ARCH-052–056, CORE-091, CORE-109–113,
/// APP-045, APP-054, QA-016):
/// - an agent's (`assistant`, `mcp`, `cli`) call of a `destructive` or `exec` tool needs a
///   confirmation (or a retry that must, [`ConfirmInput::always_confirm`]); your own actions
///   don't;
/// - any caller's call whose estimate would pass a cap needs one too (the caps' check); a call
///   that needs both gets one confirmation covering `["confirm", "cap"]`;
/// - a non-interactive caller is never asked (QA-016), whatever the mode: `confirm.unavailable`
///   when a safety confirmation is needed, else `cap.exceeded`; so is any call while no
///   confirmation can be answered;
/// - an interactive `mcp` call with `mcp_confirmation = "claude_code"` that needs only the
///   safety confirmation was confirmed by Claude Code's own prompt (its tool carries the
///   marker): it proceeds and is recorded `approved`, `confirmed_via: claude_code`. Not for a
///   `job.retry`, whose tool carries no marker, nor for a cap, which only you answer;
/// - otherwise the call gets `pending_confirmation`, one per idempotency key;
/// - a cap the reserve stage finds passed after this stage ([`ConfirmStage::cap_needed`]) asks
///   for the cap only, by the same rules.
pub struct AgentConfirm {
    confirm: Arc<Confirm>,
}

impl AgentConfirm {
    pub fn new(confirm: Arc<Confirm>) -> Arc<AgentConfirm> {
        Arc::new(AgentConfirm { confirm })
    }

    /// The decision for a call that needs a safety confirmation (`confirm`) and/or the caps
    /// `hits` (module docs).
    async fn ask(
        &self,
        i: ConfirmInput<'_>,
        confirm: bool,
        hits: Option<Vec<CapHit>>,
    ) -> ConfirmDecision {
        let refuse = |hits: &Option<Vec<CapHit>>| match hits {
            Some(hits) if !confirm => {
                ConfirmDecision::CapExceeded(cap_exceeded(&i.descriptor.name, hits))
            }
            _ => ConfirmDecision::Unavailable,
        };
        // Only a tool that changes things needs a key, and only such a tool asks.
        let Some(idem) = i.idem else {
            return refuse(&hits);
        };
        // A non-interactive caller is never asked, nor counted as asked (QA-016): no person
        // may have seen a prompt (`claude -p`, an SDK host), so it fails closed.
        if i.session.non_interactive {
            return refuse(&hits);
        }
        let mut covers = Vec::new();
        if confirm {
            covers.push(Cover::Confirm);
        }
        if hits.is_some() {
            covers.push(Cover::Cap);
        }
        let request = ConfirmRequest {
            scope: idem.scope.to_string(),
            idem_key: idem.key.to_string(),
            tool: i.descriptor.name.clone(),
            args_hash: idem.args_hash.to_string(),
            arguments: i.args.clone(),
            caller_kind: i.session.caller.to_string(),
            session_id: i.session.id.clone(),
            project: i.project.map(str::to_string),
            covers,
            cap: hits.clone(),
        };
        let claude_code_asked = confirm
            && hits.is_none()
            && i.session.caller == CallerKind::Mcp
            && self.confirm.mcp() == McpConfirmation::ClaudeCode
            && i.descriptor.effect.needs_agent_confirmation()
            && !i.retry;
        if claude_code_asked {
            let db: &StateDb = match i.workspace {
                Some(open) => &open.state,
                None => self.confirm.suite_db(),
            };
            return match self.confirm.record_claude_code(db, request).await {
                Ok(()) => ConfirmDecision::Proceed {
                    confirmed_via: Some(ConfirmedVia::ClaudeCode),
                    cap_override: None,
                },
                Err(e) => ConfirmDecision::Failed(state_unavailable(&e)),
            };
        }
        if !self.confirm.answerable() {
            return refuse(&hits);
        }
        match self.confirm.request(request).await {
            Ok(pending) => ConfirmDecision::Pending(pending),
            Err(e) => ConfirmDecision::Failed(state_unavailable(&e)),
        }
    }
}

/// What an `allow_once` answer allowed: the caps the confirmation's `cap` names and the
/// largest estimate it showed. `None` for another action, or a row without caps.
fn allowed_once(id: &str, action: Option<&str>, cap: Option<&Value>) -> Option<AllowOnce> {
    if action != Some("allow_once") {
        return None;
    }
    let hits = cap?.as_array()?;
    let mut caps = Vec::new();
    let mut estimate = Money::ZERO;
    for h in hits {
        caps.push(h.get("cap")?.as_str()?.to_string());
        let e = Money::parse_decimal(h.get("estimate")?.as_str()?).ok()?;
        estimate = estimate.max(e);
    }
    (!caps.is_empty()).then(|| AllowOnce {
        confirmation_id: id.to_string(),
        caps,
        estimate,
    })
}

fn state_unavailable(e: &StateError) -> ErrorBody {
    tracing::error!(error = %e, "state.db failed in the confirm stage");
    ErrorBody::new(
        &codes::STATE_UNAVAILABLE,
        "The core's state database is not available; retry with the same idempotency key.",
        "",
    )
}

impl ConfirmStage for AgentConfirm {
    fn check<'a>(&'a self, i: ConfirmInput<'a>) -> BoxFuture<'a, ConfirmDecision> {
        Box::pin(async move {
            let confirm = i.needs_agent_confirmation();
            let hits = match self.confirm.caps() {
                None => None,
                Some(caps) => match caps.check(i.estimate).await {
                    Ok(CapCheck::Within) => None,
                    Ok(CapCheck::Exceeds(hits)) => Some(hits),
                    Err(e) => return ConfirmDecision::Failed(e),
                },
            };
            if !confirm && hits.is_none() {
                return ConfirmDecision::Proceed {
                    confirmed_via: None,
                    cap_override: None,
                };
            }
            self.ask(i, confirm, hits).await
        })
    }

    fn cap_needed<'a>(
        &'a self,
        i: ConfirmInput<'a>,
        hits: Vec<CapHit>,
    ) -> BoxFuture<'a, ConfirmDecision> {
        Box::pin(self.ask(i, false, Some(hits)))
    }

    fn on_replay<'a>(
        &'a self,
        db: &'a StateDb,
        idem: IdemRef<'a>,
    ) -> BoxFuture<'a, Result<Option<ReplayDecision>, StateError>> {
        Box::pin(async move {
            Ok(Some(match self.confirm.on_replay(db, idem).await? {
                Replay::Run(row) => ReplayDecision::Run(Approval {
                    allowed: allowed_once(&row.id, row.cap_action.as_deref(), row.cap.as_ref()),
                    confirmation_id: row.id,
                    tool: row.tool,
                    arguments: row.arguments,
                    project: row.project,
                    confirmed_via: row.confirmed_via.unwrap_or(ConfirmedVia::App),
                    cap_action: row.cap_action,
                }),
                Replay::StillPending(pending) => ReplayDecision::StillPending(pending),
                Replay::Denied => ReplayDecision::Denied,
                Replay::Expired => ReplayDecision::Expired,
            }))
        })
    }
}
