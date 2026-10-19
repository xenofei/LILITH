//! The confirm half of stage 5 (CORE-015): [`AgentConfirm`]. The cap half is Task 16's.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use lil_protocol::{CallerKind, Cover, ErrorBody, codes};

use super::{Confirm, ConfirmRequest, McpConfirmation, Replay};
use crate::state::{StateDb, StateError};
use crate::tools::{
    Approval, ConfirmDecision, ConfirmInput, ConfirmStage, ConfirmedVia, IdemRef, ReplayDecision,
};

/// Agent safety confirmations (ARCH-052–055, CORE-109–113, QA-016):
/// - only an agent's (`assistant`, `mcp`, `cli`) call of a `destructive` or `exec` tool asks
///   (or a retry that must, [`ConfirmInput::always_confirm`]); your own actions never do;
/// - an `mcp` call with `mcp_confirmation = "claude_code"` was confirmed by Claude Code's own
///   prompt (its tool carries the marker): it proceeds and is recorded `approved`,
///   `confirmed_via: claude_code`. Not for a `job.retry`, whose tool carries no marker;
/// - a non-interactive caller, or any call while no confirmation can be answered, gets
///   `confirm.unavailable`;
/// - otherwise the call gets `pending_confirmation`, one per idempotency key.
pub struct AgentConfirm {
    confirm: Arc<Confirm>,
}

impl AgentConfirm {
    pub fn new(confirm: Arc<Confirm>) -> Arc<AgentConfirm> {
        Arc::new(AgentConfirm { confirm })
    }
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
            if !i.needs_agent_confirmation() {
                return ConfirmDecision::Proceed {
                    confirmed_via: None,
                    cap_override: None,
                };
            }
            // Only a tool that changes things needs a key, and only such a tool asks.
            let Some(idem) = i.idem else {
                return ConfirmDecision::Unavailable;
            };
            let request = ConfirmRequest {
                scope: idem.scope.to_string(),
                idem_key: idem.key.to_string(),
                tool: i.descriptor.name.clone(),
                args_hash: idem.args_hash.to_string(),
                arguments: i.args.clone(),
                caller_kind: i.session.caller.to_string(),
                session_id: i.session.id.clone(),
                covers: vec![Cover::Confirm],
                cap: None,
            };
            let claude_code_asked = i.session.caller == CallerKind::Mcp
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
            if i.session.non_interactive || !self.confirm.answerable() {
                return ConfirmDecision::Unavailable;
            }
            match self.confirm.request(request).await {
                Ok(pending) => ConfirmDecision::Pending(pending),
                Err(e) => ConfirmDecision::Failed(state_unavailable(&e)),
            }
        })
    }

    fn on_replay<'a>(
        &'a self,
        db: &'a StateDb,
        idem: IdemRef<'a>,
    ) -> BoxFuture<'a, Result<Option<ReplayDecision>, StateError>> {
        Box::pin(async move {
            Ok(Some(match self.confirm.on_replay(db, idem).await? {
                Replay::Run(row) => ReplayDecision::Run(Approval {
                    confirmation_id: row.id,
                    tool: row.tool,
                    arguments: row.arguments,
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
