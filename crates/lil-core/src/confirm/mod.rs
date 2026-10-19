//! The `confirm` module (CORE-014; ARCH-052–055, CORE-109–115, APP-044–046, APP-051, QA-016;
//! Appendix F "Confirmations"): agent safety confirmations, their answers, and the retries
//! that run an approved call.
//!
//! - **Who is asked.** An agent's (`assistant`, `mcp`, `cli`) call of a `destructive` or `exec`
//!   tool ([`AgentConfirm`], stage 5). Your own actions in an app never ask (CORE-109).
//! - **When it can be answered** ([`Confirm::answerable`]): while a verified `ui:*` connection
//!   whose shared capabilities hold `confirmations.v1` or `caps.v1` is open ([`UiPresence`]),
//!   or the notification path can reach you (plan 4; [`NoNotificationPath`] until then).
//!   Otherwise, and always for a non-interactive caller, the call gets `confirm.unavailable`.
//! - **Where it lives.** In the `state.db` of the call's scope (the suite one, or the
//!   workspace's), keyed by the call's idempotency row `(scope, key)` and bound to the tool and
//!   the arguments' hash. It expires [`Confirm::new`]'s `lifetime` after it was made (30
//!   minutes; `LIL_TEST_CONFIRM_LIFETIME_SECS` on a test channel).
//! - **The retry token is the idempotency key.** The dispatcher stores the
//!   `pending_confirmation` as the call's outcome; a retry with the key finds it and asks
//!   [`Confirm::on_replay`]: approved runs the call once, for the tool, arguments and project
//!   you saw, if the retry comes within the lifetime from your answer (the row is taken back
//!   with `StateDb::idem_reclaim`, so of racing retries exactly one runs it; later the approval
//!   has expired); denied and expired release the key; still pending replays the same
//!   `pending_confirmation` (a non-interactive caller gets `confirm.unavailable` instead, and
//!   the key stays). Answered, expired and cancelled rows older than 30 days are pruned with
//!   the idempotency rows.
//! - **Who answers.** Only a verified `ui:*` session ([`Confirm::answer`]; the router lets
//!   nobody else reach `app/confirmation.answer`), after a real input event in the shared
//!   dialog, which the core cannot see: it verifies the connection, not the click (APP-044).
//!   A caller-supplied `confirm: true` is an unknown field everywhere (CORE-111). Every answer
//!   records how it was given (`confirmed_via`), and the answered ones are the history.
//! - **`mcp_confirmation`** ([`McpConfirmation`], Appendix F: [`MCP_CONFIRMATION`] =
//!   `Core`). With `Core` an `mcp` call is confirmed like any agent call and no tool carries
//!   `anthropic/requiresUserInteraction` (ruling P3-R10: one prompt); with `ClaudeCode` the
//!   `destructive` and `exec` tools of an `mcp` session carry it, and such a call proceeds,
//!   recorded `approved` with `confirmed_via: claude_code` ([`Confirm::annotate`]).
//! - **Events** on the `confirmations` topic: `{kind: "pending" | "answered" | "expired" |
//!   "cancelled", confirmation}`, each once per confirmation (every change is one
//!   `UPDATE … WHERE state = 'pending'`).
//! - **Idle** (ARCH-020): [`Confirm::busy`] while a confirmation this core knows of is pending
//!   and not yet expired. [`Confirm::sweep`] (every second) marks the expired ones.
//! - **Known limit.** After a restart the core knows the suite database's pending
//!   confirmations ([`Confirm::load`] at start) and the default workspace's once it opens;
//!   another workspace's appear once a call opens that workspace.

mod stage;
mod store;

pub use stage::AgentConfirm;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, UNIX_EPOCH};

use lil_platform::{Clock, IdSource};
use lil_protocol::app::{
    Answer, CONFIRMATION_CAPABILITIES, ConfirmationAnswerParams, ConfirmationView,
};
use lil_protocol::{
    CallerKind, CodeSpec, Cover, EffectClass, ErrorBody, NO_WORKSPACE, PendingConfirmation,
    ToolDescriptor, codes,
};
use serde_json::{Value, json};

use crate::events::EventBus;
use crate::lifecycle::IdleBlocker;
use crate::session::Session;
use crate::state::{StateDb, StateError, StateRegistry};
use crate::store::FailPoints;
pub use crate::tools::ConfirmedVia;
use crate::tools::{CapHit, IdemRef};

/// The event topic of the confirmations.
pub const CONFIRMATIONS_TOPIC: &str = "confirmations";

/// The fail point after an answer is committed and before its `answered` event.
pub const AFTER_ANSWER: &str = "confirm.after_answer";

/// The `_meta` key that makes Claude Code prompt for a tool every time.
pub const REQUIRES_USER_INTERACTION: &str = "anthropic/requiresUserInteraction";

/// A confirmation's state (Appendix F): wire `pending`, `approved`, `denied`, `expired`,
/// `cancelled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConfState {
    Pending,
    Approved,
    Denied,
    Expired,
    Cancelled,
}

impl ConfState {
    pub fn wire(self) -> &'static str {
        match self {
            ConfState::Pending => "pending",
            ConfState::Approved => "approved",
            ConfState::Denied => "denied",
            ConfState::Expired => "expired",
            ConfState::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<ConfState> {
        Some(match s {
            "pending" => ConfState::Pending,
            "approved" => ConfState::Approved,
            "denied" => ConfState::Denied,
            "expired" => ConfState::Expired,
            "cancelled" => ConfState::Cancelled,
            _ => return None,
        })
    }
}

/// One confirmation, as its `state.db` keeps it. Times are Unix seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfirmationRow {
    /// `cf_<16 hex>`.
    pub id: String,
    /// The call's idempotency scope, `<caller kind>|<workspace id or none>`.
    pub scope: String,
    /// The call's idempotency key: the retry token.
    pub idem_key: String,
    /// The tool the confirmation is for (for a `job.retry`, the retried job's tool).
    pub tool: String,
    /// The hash of the call's idempotency row.
    pub args_hash: String,
    /// The tool's arguments, as the agent sent them (untrusted).
    pub arguments: Value,
    pub caller_kind: String,
    pub covers: Vec<Cover>,
    /// The caps the call would pass (Task 16).
    pub cap: Option<Value>,
    pub state: ConfState,
    pub confirmed_via: Option<ConfirmedVia>,
    pub created_utc: i64,
    pub expires_utc: i64,
    pub answered_utc: Option<i64>,
    /// The cap action of the answer (`allow_once`, `raise`).
    pub cap_action: Option<String>,
    /// The session that made the call.
    pub session_id: String,
    /// The call's resolved project (none without a workspace): the approval is for it.
    pub project: Option<String>,
}

impl ConfirmationRow {
    /// The workspace id of its scope, or `"none"`.
    fn workspace(&self) -> &str {
        workspace_of(&self.scope)
    }

    /// As `app/confirmations.list` and the events show it.
    pub fn view(&self) -> ConfirmationView {
        let time = lil_schema::time::rfc3339;
        ConfirmationView {
            id: self.id.clone(),
            tool: self.tool.clone(),
            arguments: self.arguments.clone(),
            caller_kind: self.caller_kind.clone(),
            project: self.project.clone(),
            covers: self.covers.clone(),
            cap: self.cap.clone(),
            state: self.state.wire().to_string(),
            confirmed_via: self.confirmed_via.map(|v| v.wire().to_string()),
            created: time(self.created_utc),
            expires_at: time(self.expires_utc),
            answered: self.answered_utc.map(time),
        }
    }

    /// The `pending_confirmation` result of its call.
    pub fn pending(&self) -> PendingConfirmation {
        PendingConfirmation {
            confirmation_id: self.id.clone(),
            retry_token: self.idem_key.clone(),
            expires_at: lil_schema::time::rfc3339(self.expires_utc),
            covers: self.covers.clone(),
        }
    }
}

/// The workspace id of an idempotency scope `<caller kind>|<workspace id or none>`.
fn workspace_of(scope: &str) -> &str {
    scope.rsplit_once('|').map_or(NO_WORKSPACE, |(_, ws)| ws)
}

/// How the core reaches you with no app open (plan 4: toasts and `lil:confirm/<id>`).
pub trait NotificationPath: Send + Sync {
    fn reachable(&self) -> bool;
}

/// Plan 3: there is no notification path, so it never reaches you (APP-051).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoNotificationPath;

impl NotificationPath for NoNotificationPath {
    fn reachable(&self) -> bool {
        false
    }
}

/// The verified `ui:*` connections open now that can answer a confirmation (their shared
/// capabilities hold `confirmations.v1` or `caps.v1`). The router counts them.
#[derive(Debug, Default)]
pub struct UiPresence {
    open: AtomicUsize,
}

impl UiPresence {
    pub fn new() -> Arc<UiPresence> {
        Arc::new(UiPresence::default())
    }

    /// One more such connection.
    pub fn opened(&self) {
        self.open.fetch_add(1, Ordering::SeqCst);
    }

    /// One fewer.
    pub fn closed(&self) {
        let _ = self
            .open
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1));
    }

    /// Whether one is open.
    pub fn any(&self) -> bool {
        self.open.load(Ordering::SeqCst) > 0
    }
}

/// Whether `session` can answer confirmations: a verified `ui:*` caller whose shared
/// capabilities hold `confirmations.v1` or `caps.v1` (APP-043, APP-044).
pub fn can_answer(session: &Session) -> bool {
    matches!(session.caller, CallerKind::Ui(_)) && has_confirmation_capability(session)
}

/// Whether `session`'s shared capabilities hold `confirmations.v1` or `caps.v1`.
pub fn has_confirmation_capability(session: &Session) -> bool {
    session
        .capabilities
        .iter()
        .any(|c| CONFIRMATION_CAPABILITIES.contains(&c.as_str()))
}

/// What a retry with the token of a stored `pending_confirmation` finds.
#[derive(Debug, Clone, PartialEq)]
pub enum Replay {
    /// Approved: run the call once. Boxed: a row is much larger than the other variants.
    Run(Box<ConfirmationRow>),
    StillPending(PendingConfirmation),
    Denied,
    /// Expired, cancelled, or gone.
    Expired,
}

/// Appendix F's `mcp_confirmation`: whether Claude Code's own prompt is an `mcp` call's
/// confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum McpConfirmation {
    /// The core's own confirmation; no tool carries the marker (ruling P3-R10).
    #[default]
    Core,
    /// Claude Code's prompt, through the marker on `destructive` and `exec` tools.
    ClaudeCode,
}

/// The value Appendix F records (verified 2026-10-18, Appendix B.3).
pub const MCP_CONFIRMATION: McpConfirmation = McpConfirmation::Core;

/// A call that needs a confirmation.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfirmRequest {
    pub scope: String,
    pub idem_key: String,
    pub tool: String,
    pub args_hash: String,
    pub arguments: Value,
    pub caller_kind: String,
    pub session_id: String,
    /// The call's resolved project (none without a workspace).
    pub project: Option<String>,
    pub covers: Vec<Cover>,
    /// The caps the call would pass (Task 16).
    pub cap: Option<Vec<CapHit>>,
}

/// Which confirmations [`Confirm::list`] gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListState {
    Pending,
    History,
}

/// A pending confirmation this core knows of.
#[derive(Debug, Clone)]
struct Tracked {
    /// The workspace id of its scope (`"none"`: the suite database).
    workspace: String,
    expires_utc: i64,
    session_id: String,
    ui_cap: bool,
}

/// The confirmations of every scope (module docs).
pub struct Confirm {
    states: Arc<StateRegistry>,
    events: Arc<EventBus>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdSource>,
    presence: Arc<UiPresence>,
    notify: Arc<dyn NotificationPath>,
    lifetime: Duration,
    mcp: McpConfirmation,
    failpoints: Arc<FailPoints>,
    pending: Mutex<HashMap<String, Tracked>>,
}

impl std::fmt::Debug for Confirm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Confirm")
            .field("lifetime", &self.lifetime)
            .field("mcp", &self.mcp)
            .finish_non_exhaustive()
    }
}

impl Confirm {
    #[allow(
        clippy::too_many_arguments,
        reason = "the plan's signature, plus the core's fail points"
    )]
    pub fn new(
        states: Arc<StateRegistry>,
        events: Arc<EventBus>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdSource>,
        presence: Arc<UiPresence>,
        notify: Arc<dyn NotificationPath>,
        lifetime: Duration,
        mcp: McpConfirmation,
        failpoints: Arc<FailPoints>,
    ) -> Arc<Confirm> {
        Arc::new(Confirm {
            states,
            events,
            clock,
            ids,
            presence,
            notify,
            lifetime,
            mcp,
            failpoints,
            pending: Mutex::new(HashMap::new()),
        })
    }

    /// `mcp_confirmation`.
    pub fn mcp(&self) -> McpConfirmation {
        self.mcp
    }

    /// The `ui:*` connections that can answer.
    pub fn presence(&self) -> &Arc<UiPresence> {
        &self.presence
    }

    /// The suite database.
    pub(crate) fn suite_db(&self) -> &StateDb {
        self.states.suite()
    }

    /// Whether a confirmation can be answered now (APP-051): an app that can answer is
    /// connected, or the notification path reaches you.
    pub fn answerable(&self) -> bool {
        self.presence.any() || self.notify.reachable()
    }

    fn now(&self) -> i64 {
        self.clock
            .utc_now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
    }

    fn lifetime_secs(&self) -> i64 {
        i64::try_from(self.lifetime.as_secs()).unwrap_or(i64::MAX)
    }

    fn tracked(&self) -> MutexGuard<'_, HashMap<String, Tracked>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn track(&self, row: &ConfirmationRow) {
        self.tracked().insert(
            row.id.clone(),
            Tracked {
                workspace: row.workspace().to_string(),
                expires_utc: row.expires_utc,
                session_id: row.session_id.clone(),
                ui_cap: row.caller_kind.starts_with("ui:") && row.covers.contains(&Cover::Cap),
            },
        );
    }

    fn untrack(&self, id: &str) {
        self.tracked().remove(id);
    }

    fn publish(&self, kind: &str, row: &ConfirmationRow) {
        self.events.publish(
            CONFIRMATIONS_TOPIC,
            json!({"kind": kind, "confirmation": row.view()}),
        );
    }

    /// The database of the workspace `id` (`"none"`: the suite one), if it is open.
    fn db_of(&self, workspace: &str) -> Option<Arc<StateDb>> {
        if workspace == NO_WORKSPACE {
            Some(self.states.suite_db())
        } else {
            self.states.workspace_db(workspace)
        }
    }

    /// The new row for `r`, pending or (with `state`) already answered.
    fn row_for(&self, r: ConfirmRequest, state: ConfState) -> ConfirmationRow {
        let now = self.now();
        let answered = state != ConfState::Pending;
        ConfirmationRow {
            id: self.ids.id("cf"),
            scope: r.scope,
            idem_key: r.idem_key,
            tool: r.tool,
            args_hash: r.args_hash,
            arguments: r.arguments,
            caller_kind: r.caller_kind,
            covers: ordered(r.covers),
            cap: r.cap.as_deref().map(caps_json),
            state,
            confirmed_via: answered.then_some(ConfirmedVia::ClaudeCode),
            created_utc: now,
            expires_utc: now.saturating_add(self.lifetime_secs()),
            answered_utc: answered.then_some(now),
            cap_action: None,
            session_id: r.session_id,
            project: r.project,
        }
    }

    /// The pending confirmation of `r`'s call: the one its key has already (same arguments,
    /// not expired), else a new one, published as `pending`.
    ///
    /// # Errors
    /// The scope's database failed, or (for a workspace scope) is not open.
    pub async fn request(&self, r: ConfirmRequest) -> Result<PendingConfirmation, StateError> {
        let workspace = workspace_of(&r.scope).to_string();
        let db = self.db_of(&workspace).ok_or_else(|| {
            StateError::Io(format!("the state.db of workspace {workspace} is not open"))
        })?;
        let new = self.row_for(r, ConfState::Pending);
        let requested = store::request(&db, new, self.now()).await?;
        if let Some(ended) = &requested.ended {
            self.untrack(&ended.id);
            self.publish(ended.state.wire(), ended);
        }
        self.track(&requested.row);
        if requested.inserted {
            tracing::info!(
                confirmation = %requested.row.id,
                tool = %requested.row.tool,
                caller = %requested.row.caller_kind,
                "a call waits for your confirmation"
            );
            self.publish("pending", &requested.row);
        }
        Ok(requested.row.pending())
    }

    /// What a retry of `idem` (in `db`), whose stored outcome is a `pending_confirmation`,
    /// gets: its key's newest confirmation decides. One that has expired is marked so now.
    ///
    /// # Errors
    /// The database failed.
    pub async fn on_replay(&self, db: &StateDb, idem: IdemRef<'_>) -> Result<Replay, StateError> {
        let Some(row) = store::latest(db, idem.scope, idem.key).await? else {
            return Ok(Replay::Expired);
        };
        if row.args_hash != idem.args_hash {
            return Ok(Replay::Expired);
        }
        Ok(match row.state {
            ConfState::Pending if row.expires_utc <= self.now() => {
                self.expire(db, &row.id).await?;
                Replay::Expired
            }
            ConfState::Pending => {
                self.track(&row);
                Replay::StillPending(row.pending())
            }
            // An approval holds for the lifetime from its answer (Appendix F: a confirmation
            // expires after 30 minutes); after that the call needs a fresh one. An answer dated
            // after now (the wall clock moved back) counts as expired too, so moving the clock
            // can never stretch an approval.
            ConfState::Approved
                if {
                    let (answered, now) = (row.answered_utc.unwrap_or(row.created_utc), self.now());
                    answered > now || answered.saturating_add(self.lifetime_secs()) <= now
                } =>
            {
                tracing::info!(confirmation = %row.id, "an approval expired before its call ran");
                Replay::Expired
            }
            ConfState::Approved => Replay::Run(Box::new(row)),
            ConfState::Denied => Replay::Denied,
            ConfState::Expired | ConfState::Cancelled => Replay::Expired,
        })
    }

    /// Ends the pending confirmation `id` in `db` as expired; publishes `expired` if this
    /// ended it.
    async fn expire(&self, db: &StateDb, id: &str) -> Result<(), StateError> {
        self.untrack(id);
        if let Some(row) = store::end(db, id, ConfState::Expired, None, None, None, None).await? {
            tracing::info!(confirmation = %row.id, "a confirmation expired unanswered");
            self.publish("expired", &row);
        }
        Ok(())
    }

    /// Records that Claude Code's own prompt confirmed `r`'s call (`mcp_confirmation =
    /// "claude_code"`): an `approved` row with `confirmed_via: claude_code`, in `db`.
    ///
    /// # Errors
    /// The database failed.
    pub async fn record_claude_code(
        &self,
        db: &StateDb,
        r: ConfirmRequest,
    ) -> Result<(), StateError> {
        let row = self.row_for(r, ConfState::Approved);
        store::insert_row(db, row.clone()).await?;
        self.publish("answered", &row);
        Ok(())
    }

    /// Finds the confirmation `id` and its database: the one this core knows it in, else
    /// each writable database open now.
    async fn find(&self, id: &str) -> Result<Option<(Arc<StateDb>, ConfirmationRow)>, StateError> {
        let known = self.tracked().get(id).map(|t| t.workspace.clone());
        if let Some(db) = known.and_then(|ws| self.db_of(&ws))
            && let Some(row) = store::get(&db, id).await?
        {
            return Ok(Some((db, row)));
        }
        for (_, db) in self.states.writable_dbs() {
            if let Some(row) = store::get(&db, id).await? {
                return Ok(Some((db, row)));
            }
        }
        Ok(None)
    }

    /// Answers confirmation `p.confirmation_id` for `by`, a verified `ui:*` session that can
    /// answer ([`can_answer`]): the row after the answer, published as `answered` (after the
    /// `confirm.after_answer` fail point).
    ///
    /// # Errors
    /// `caller.not_allowed` for any other session; `confirm.unknown`; `confirm.expired` for one
    /// that expired or was cancelled; `confirm.already_answered`; `params.invalid` for a cap
    /// action on a confirmation that covers no cap; `state.unavailable`.
    pub async fn answer(
        &self,
        by: &Session,
        p: ConfirmationAnswerParams,
    ) -> Result<ConfirmationRow, ErrorBody> {
        let trace_id = self.ids.id("t");
        let fail = |spec: &CodeSpec, message: String| ErrorBody::new(spec, message, &trace_id);
        let state_error = |e: &StateError| {
            tracing::error!(error = %e, "state.db failed while answering a confirmation");
            fail(
                &codes::STATE_UNAVAILABLE,
                "The core's state database is not available; answer again.".into(),
            )
        };
        if !can_answer(by) {
            tracing::warn!(caller = %by.caller, session = %by.id, "a confirmation answer from a session that can't answer; refused");
            return Err(fail(
                &codes::CALLER_NOT_ALLOWED,
                "Only the LILITH app can answer a confirmation.".into(),
            ));
        }
        let id = p.confirmation_id;
        let Some((db, row)) = self.find(&id).await.map_err(|e| state_error(&e))? else {
            return Err(fail(
                &codes::CONFIRM_UNKNOWN,
                format!("There is no confirmation {id}."),
            ));
        };
        if p.cap.is_some() && !row.covers.contains(&Cover::Cap) {
            return Err(fail(
                &codes::PARAMS_INVALID,
                format!("Confirmation {id} covers no cap, so it takes no cap action."),
            ));
        }
        let expired = || {
            fail(
                &codes::CONFIRM_EXPIRED,
                format!("Confirmation {id} expired before it was answered."),
            )
        };
        let ended = |row: &ConfirmationRow| match row.state {
            ConfState::Expired | ConfState::Cancelled => expired(),
            _ => fail(
                &codes::CONFIRM_ALREADY_ANSWERED,
                format!(
                    "Confirmation {id} was answered already ({}).",
                    row.state.wire()
                ),
            ),
        };
        if row.state != ConfState::Pending {
            return Err(ended(&row));
        }
        let state = match p.answer {
            Answer::Approve => ConfState::Approved,
            Answer::Deny => ConfState::Denied,
        };
        let cap_action = p.cap.map(|c| c.action.wire().to_string());
        // Only while it has not expired: checked in the same statement as the answer.
        let now = self.now();
        let answered = store::end(
            &db,
            &id,
            state,
            Some(ConfirmedVia::App),
            Some(now),
            cap_action,
            Some(now),
        )
        .await
        .map_err(|e| state_error(&e))?;
        let Some(answered) = answered else {
            // Another answer, the expiry, or its time came first.
            let row = store::get(&db, &id)
                .await
                .map_err(|e| state_error(&e))?
                .unwrap_or(row);
            if row.state == ConfState::Pending {
                self.expire(&db, &id).await.map_err(|e| state_error(&e))?;
                return Err(expired());
            }
            return Err(ended(&row));
        };
        self.untrack(&id);
        tracing::info!(confirmation = %id, answer = answered.state.wire(), by = %by.caller, "a confirmation was answered");
        if let Err(e) = self.failpoints.hit(AFTER_ANSWER) {
            tracing::warn!(error = %e, "fail point confirm.after_answer: the answered event is not published");
            return Ok(answered);
        }
        self.publish("answered", &answered);
        Ok(answered)
    }

    /// At most `limit` confirmations of every writable database open now, newest first: the
    /// pending ones (expired ones are marked so first), or the history.
    ///
    /// # Errors
    /// `state.unavailable`.
    pub async fn list(
        &self,
        state: ListState,
        limit: u32,
    ) -> Result<Vec<ConfirmationRow>, ErrorBody> {
        let state_error = |e: &StateError| {
            tracing::error!(error = %e, "state.db failed while listing confirmations");
            ErrorBody::new(
                &codes::STATE_UNAVAILABLE,
                "The core's state database is not available; list again.",
                self.ids.id("t"),
            )
        };
        let now = self.now();
        let mut rows = Vec::new();
        for (_, db) in self.states.writable_dbs() {
            let found = store::list(&db, state == ListState::Pending, limit)
                .await
                .map_err(|e| state_error(&e))?;
            for row in found {
                if row.state == ConfState::Pending && row.expires_utc <= now {
                    self.expire(&db, &row.id)
                        .await
                        .map_err(|e| state_error(&e))?;
                    continue;
                }
                if row.state == ConfState::Pending {
                    self.track(&row);
                }
                rows.push(row);
            }
        }
        rows.sort_by_key(|r| std::cmp::Reverse(r.created_utc));
        rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        Ok(rows)
    }

    /// Cancels the pending cap confirmations of the `ui:*` session `session_id`, whose
    /// connection closed (Appendix F); each is published as `cancelled`. An agent's safety
    /// confirmations stay: the agent retries with its token.
    pub async fn cancel_for_session(&self, session_id: &str) {
        let ids: Vec<(String, String)> = self
            .tracked()
            .iter()
            .filter(|(_, t)| t.session_id == session_id && t.ui_cap)
            .map(|(id, t)| (id.clone(), t.workspace.clone()))
            .collect();
        for (id, workspace) in ids {
            self.untrack(&id);
            let Some(db) = self.db_of(&workspace) else {
                continue;
            };
            match store::end(&db, &id, ConfState::Cancelled, None, None, None, None).await {
                Ok(Some(row)) => {
                    tracing::info!(confirmation = %id, "a cap confirmation is cancelled: its app closed");
                    self.publish("cancelled", &row);
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(confirmation = %id, error = %e, "a cap confirmation could not be cancelled")
                }
            }
        }
    }

    /// Marks the pending confirmations this core knows of whose time has passed as expired.
    pub async fn sweep(&self) {
        let now = self.now();
        let due: Vec<(String, String)> = self
            .tracked()
            .iter()
            .filter(|(_, t)| t.expires_utc <= now)
            .map(|(id, t)| (id.clone(), t.workspace.clone()))
            .collect();
        for (id, workspace) in due {
            match self.db_of(&workspace) {
                Some(db) => {
                    if let Err(e) = self.expire(&db, &id).await {
                        tracing::warn!(confirmation = %id, error = %e, "an expired confirmation could not be marked");
                    }
                }
                // Its workspace closed: the row is marked when a retry or a list finds it.
                None => self.untrack(&id),
            }
        }
    }

    /// Learns the pending confirmations of `db` (the suite database, or an open workspace's),
    /// as a core does at start: they survive a restart.
    ///
    /// # Errors
    /// The database failed.
    pub async fn load(&self, db: &StateDb) -> Result<usize, StateError> {
        let rows = store::pending(db).await?;
        for row in &rows {
            self.track(row);
        }
        Ok(rows.len())
    }

    /// `tools` as `caller` sees them in `tools/list`: with [`McpConfirmation::ClaudeCode`] an
    /// `mcp` session's `destructive` and `exec` tools carry
    /// `annotations["_meta"]["anthropic/requiresUserInteraction"] = true`; otherwise no tool
    /// carries it (ruling P3-R10: the core's confirmation is the only prompt).
    pub fn annotate(&self, caller: &CallerKind, tools: Vec<ToolDescriptor>) -> Vec<ToolDescriptor> {
        let mark = *caller == CallerKind::Mcp && self.mcp == McpConfirmation::ClaudeCode;
        tools
            .into_iter()
            .map(|mut d| {
                let meta = d
                    .annotations
                    .get_mut("_meta")
                    .and_then(Value::as_object_mut);
                if mark && matches!(d.effect, EffectClass::Destructive | EffectClass::Exec) {
                    match meta {
                        Some(meta) => {
                            meta.insert(REQUIRES_USER_INTERACTION.into(), Value::Bool(true));
                        }
                        None => {
                            d.annotations
                                .insert("_meta".into(), json!({ REQUIRES_USER_INTERACTION: true }));
                        }
                    }
                } else if let Some(meta) = meta {
                    meta.shift_remove(REQUIRES_USER_INTERACTION);
                }
                d
            })
            .collect()
    }
}

impl IdleBlocker for Confirm {
    fn name(&self) -> &'static str {
        "confirmations"
    }

    /// A confirmation this core knows of is pending and not yet expired (ARCH-020).
    fn busy(&self) -> bool {
        let now = self.now();
        self.tracked().values().any(|t| t.expires_utc > now)
    }

    /// A pending confirmation survives the restart, so it never holds a drain.
    fn holds_drain(&self) -> bool {
        false
    }
}

impl Confirm {
    /// [`IdleBlocker::busy`].
    pub fn busy(&self) -> bool {
        IdleBlocker::busy(self)
    }
}

/// `confirm` before `cap`, each once.
fn ordered(covers: Vec<Cover>) -> Vec<Cover> {
    [Cover::Confirm, Cover::Cap]
        .into_iter()
        .filter(|c| covers.contains(c))
        .collect()
}

/// The JSON of the caps a call would pass: `[{cap, limit, settled, held, estimate}]`, amounts
/// as decimal strings in the display currency.
fn caps_json(hits: &[CapHit]) -> Value {
    Value::Array(
        hits.iter()
            .map(|h| {
                json!({
                    "cap": h.cap,
                    "limit": h.limit.to_decimal_string(),
                    "settled": h.settled.to_decimal_string(),
                    "held": h.held.to_decimal_string(),
                    "estimate": h.estimate.to_decimal_string(),
                })
            })
            .collect(),
    )
}
