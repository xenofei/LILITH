//! The dispatcher (CORE-015, CORE-016, CORE-017, QA-012, QA-016): every `tools/call` runs the
//! same stages in order.
//!
//! 1. **Resolve.** The trace id is the caller's (`[A-Za-z0-9_-]{1,64}`) or a fresh one. The
//!    tool must exist and be visible to the caller, else `tool.unknown`. A tool whose effect
//!    is not `read` needs an idempotency key, else `idempotency.key_required` (ARCH-035).
//!    With a session workspace: one that is no longer the default (after a switch; owner
//!    decision 2) is `workspace.unsupported`; otherwise the [`WorkspaceHub`] opens it and
//!    `CallCtx::workspace` is set (its errors, `state.unavailable` for one, are the call's).
//!    An effect other than `read` on a workspace this core serves read-only (another core
//!    holds its `core.lock`, ARCH-028) is `workspace.read_only`, before anything is stored.
//! 2. **Validate.** The tool checks the arguments (`params.invalid`). For effect ≠ `read`,
//!    `idem_begin` in the session's workspace `state.db`, or the suite one without a
//!    workspace, under the scope `"<caller kind>|<workspace id or none>"`: a finished call
//!    with the same arguments is replayed byte for byte, another hash is
//!    `idempotency.conflict`, a running one `idempotency.in_progress`.
//! 3. **Settings**, 4. **estimate**, 5. **confirm and caps** (`confirm.unavailable`, or a
//!    `pending_confirmation` result), 6. **reserve**: the [`PipelineHooks`]. Before the
//!    settings, the Resolve stage's project part runs on the valid arguments (ARCH-088,
//!    ARCH-098): with a session workspace, `CallCtx::project` is the `project` argument of a
//!    tool that declares one, else the session's active project, else the workspace's
//!    `state.json` hint, else `sketchbook`. `assets.query`'s `project` is a filter, so it is
//!    never the call's project. The settings stage gets that project and, as the request
//!    layer, the `settings` argument of a tool that declares one (only `test.settings` in
//!    plan 2b, CORE-057); its error (`params.invalid`) is the call's.
//! 7. **Execute** in its own task: a panic is `core.bug` with the panic text as untrusted
//!    detail, and the core goes on (QA-012, ARCH-060). A call that named a project and ended
//!    `ok` makes it the session's active project and, on a read-write workspace, the hint
//!    (a hint that can't be written is logged, never the call's error).
//! 8. **Meter** and 9. **record**: the hooks again. The record hook gets the plan the tool
//!    left in `CallCtx::record` (CORE-020); the plan-2b stage writes it only for an `ok`
//!    outcome, and its error (a failed commit) becomes the call's.
//!
//! Then the line cap (ARCH-025): an outcome too large to send in one line (with room for its
//! response envelope) becomes `core.bug`. Then the idempotency row is finished: `ok` and
//! `pending_confirmation` outcomes are stored, an error releases the key, except the
//! too-large `core.bug`, which is stored (the tool ran; a retry must not run it again). The
//! size is decided before the row is written, so the stored outcome and every replay agree.
//! A `state.db` failure is `state.unavailable`. When the row can't be finished after the tool
//! ran (an outcome to store), it stays `running` until the core restarts, because the tool
//! must not run again for that key; the error then says so and carries `data.ran: true`.
//!
//! A call in a workspace holds its open workspace until its row is finished, so a workspace
//! the hub lets go of mid-call (`workspace.set_default`) stays open until the call ends.
//!
//! **Draining** (ARCH-021). Before stage 1 the call is admitted by the [`Lifecycle`]: a
//! draining core answers `core.draining` (retry with the same idempotency key once a core
//! runs again). An admitted call counts as in flight until its pipeline ends, even if its
//! caller has gone, so a drain waits for it.
//!
//! **Tracing.** The pipeline and the tool's own task run in a `tool_call` span with `tool` and
//! `trace_id`, so every line they log, the panic hook's among them (ARCH-119), carries the
//! call's trace id.
//!
//! **Cancellation.** [`Dispatcher::call`] runs the pipeline in a task of its own, so a caller
//! that goes away (a client killed mid-call) cannot stop it between `idem_begin` and
//! `idem_finish`: the outcome is stored for the retry. A [`Claim`] that is dropped unfinished
//! anyway (a bug in a stage) releases its row from its `Drop`, in a task that counts as in
//! flight, so a drain waits for the release too.

use std::sync::Arc;
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};
use tokio::task::JoinError;
use tracing::Instrument as _;
use ww_platform::{Clock, IdSource};
use ww_protocol::{
    CodeSpec, Detail, EffectClass, ErrorBody, Outcome, ToolCallParams, ToolDescriptor, ToolOutput,
    args_hash, codes, fits_response_result,
};

use super::registry::{CallCtx, Registry, Tool};
use super::stages::{ConfirmDecision, PipelineHooks, SettingsInput};
use crate::events::EventBus;
use crate::lifecycle::{CallGuard, FollowUp, Lifecycle};
use crate::pipeline::RecordSlot;
use crate::projects::{ActiveProjects, read_hint, remember_project, resolve_project};
use crate::session::Session;
use crate::state::{IdemBegin, StateDb, StateError, StateRegistry};
use crate::store::StoreError;
use crate::workspace::{Access, OpenWorkspace, WorkspaceHub};

/// Runs `tools/call` (module docs).
#[derive(Clone)]
pub struct Dispatcher {
    inner: Arc<Inner>,
}

struct Inner {
    reg: Registry,
    hooks: PipelineHooks,
    states: Arc<StateRegistry>,
    events: Arc<EventBus>,
    hub: Arc<WorkspaceHub>,
    ids: Arc<dyn IdSource>,
    clock: Arc<dyn Clock>,
    lifecycle: Arc<Lifecycle>,
    /// Each session's active project (ARCH-088).
    active: Arc<ActiveProjects>,
}

impl std::fmt::Debug for Dispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dispatcher")
            .field("tools", &self.inner.reg)
            .finish_non_exhaustive()
    }
}

impl Dispatcher {
    #[allow(
        clippy::too_many_arguments,
        reason = "the plan's signature: one handle per core service"
    )]
    pub fn new(
        reg: Registry,
        hooks: PipelineHooks,
        states: Arc<StateRegistry>,
        events: Arc<EventBus>,
        hub: Arc<WorkspaceHub>,
        ids: Arc<dyn IdSource>,
        clock: Arc<dyn Clock>,
        lifecycle: Arc<Lifecycle>,
    ) -> Dispatcher {
        Dispatcher {
            inner: Arc::new(Inner {
                reg,
                hooks,
                states,
                events,
                hub,
                ids,
                clock,
                lifecycle,
                active: Arc::new(ActiveProjects::new()),
            }),
        }
    }

    /// Each session's active project: the router forgets a session's when its connection
    /// closes.
    pub fn active_projects(&self) -> &Arc<ActiveProjects> {
        &self.inner.active
    }

    /// The tools this dispatcher serves.
    pub fn registry(&self) -> &Registry {
        &self.inner.reg
    }

    /// The event bus its tools publish on (`CallCtx::events`).
    pub fn events(&self) -> &Arc<EventBus> {
        &self.inner.events
    }

    /// The lifecycle that admits its calls.
    pub fn lifecycle(&self) -> &Arc<Lifecycle> {
        &self.inner.lifecycle
    }

    /// The workspace hub its calls open workspaces through.
    pub fn hub(&self) -> &Arc<WorkspaceHub> {
        &self.inner.hub
    }

    /// Runs one `tools/call` through every stage (module docs). Never fails: every failure is
    /// an `Outcome::Error`.
    pub async fn call(&self, session: Arc<Session>, p: ToolCallParams) -> Outcome<ToolOutput> {
        let trace_id = match p.trace_id.as_deref() {
            Some(t) if is_trace_id(t) => t.to_string(),
            other => {
                if other.is_some() {
                    tracing::debug!(
                        tool = %p.name,
                        "the caller's trace id is not [A-Za-z0-9_-]{{1,64}}; a fresh one is used"
                    );
                }
                self.inner.ids.id("t")
            }
        };
        let Some(in_flight) = self.inner.lifecycle.try_call_started() else {
            tracing::info!(tool = %p.name, caller = %session.caller, trace_id, "tool call refused: the core is draining");
            return Outcome::Error {
                error: ErrorBody::new(
                    &codes::CORE_DRAINING,
                    "The core is shutting down. Retry with the same idempotency key once it runs again.",
                    trace_id,
                ),
            };
        };
        let (name, inner, trace) = (p.name.clone(), self.inner.clone(), trace_id.clone());
        // ERROR level, so the span and its `trace_id` stay on every line even under a raised
        // level filter.
        let span = tracing::error_span!("tool_call", tool = %name, trace_id = %trace_id);
        // Its own task: a caller that goes away cannot cut the pipeline short. The call stays
        // in flight until the pipeline ends.
        let pipeline = tokio::spawn(
            async move { inner.pipeline(&in_flight, session, p, trace).await }.instrument(span),
        );
        match pipeline.await {
            Ok(outcome) => outcome,
            Err(e) => {
                let text = join_error_text(e);
                tracing::error!(tool = %name, trace_id, panic = %text, "the dispatcher pipeline panicked");
                Outcome::Error {
                    error: core_bug(&name, text, &trace_id),
                }
            }
        }
    }
}

impl Inner {
    async fn pipeline(
        &self,
        in_flight: &CallGuard,
        session: Arc<Session>,
        p: ToolCallParams,
        trace_id: String,
    ) -> Outcome<ToolOutput> {
        let name = p.name.clone();
        let outcome = self.stages(in_flight, &session, p, &trace_id).await;
        match &outcome {
            Outcome::Ok(_) => {
                tracing::info!(tool = %name, caller = %session.caller, trace_id, "tool call ok")
            }
            Outcome::PendingConfirmation(_) => tracing::info!(
                tool = %name, caller = %session.caller, trace_id, "tool call waits for a confirmation"
            ),
            Outcome::Error { error } => tracing::info!(
                tool = %name, caller = %session.caller, trace_id, code = %error.code, "tool call failed"
            ),
        }
        outcome
    }

    async fn stages(
        &self,
        in_flight: &CallGuard,
        session: &Arc<Session>,
        p: ToolCallParams,
        trace_id: &str,
    ) -> Outcome<ToolOutput> {
        let fail = |spec: &CodeSpec, message: String| Outcome::Error {
            error: ErrorBody::new(spec, message, trace_id),
        };
        // 1. Resolve.
        let tool = match self.reg.get(&p.name) {
            Some(tool) if tool.visible_to(&session.caller) => tool,
            _ => {
                return fail(
                    &codes::TOOL_UNKNOWN,
                    format!("There is no tool called {:?}.", p.name),
                );
            }
        };
        let effect = tool.descriptor().effect;
        let key = match (effect, p.idempotency_key) {
            (EffectClass::Read, _) => None,
            (_, Some(key)) if !key.is_empty() => Some(key),
            (_, _) => {
                return fail(
                    &codes::IDEMPOTENCY_KEY_REQUIRED,
                    format!(
                        "{} changes things, so the call needs an idempotency key.",
                        p.name
                    ),
                );
            }
        };
        let workspace = match self.open_workspace(session, trace_id).await {
            Ok(workspace) => workspace,
            Err(error) => return Outcome::Error { error },
        };
        if effect != EffectClass::Read
            && let Some(open) = &workspace
            && open.access == Access::ReadOnly
        {
            return Outcome::Error {
                error: StoreError::ReadOnly.to_error(trace_id),
            };
        }
        let scope = format!("{}|{}", session.caller, session.workspace_id());

        // 2. Validate.
        let args = Value::Object(p.arguments);
        if let Err(why) = tool.validate(&args) {
            return fail(
                &codes::PARAMS_INVALID,
                format!("Invalid arguments for {}: {why}", p.name),
            );
        }
        let claim = match key {
            None => None,
            Some(key) => {
                let hash = args_hash(&json!({"name": p.name, "arguments": args}));
                match self
                    .begin(in_flight, workspace.as_ref(), scope, key, &hash, trace_id)
                    .await
                {
                    Ok(Begun::Fresh(claim)) => Some(claim),
                    Ok(Begun::Done(outcome)) => return *outcome,
                    Err(e) => return state_unavailable(&e, trace_id),
                }
            }
        };

        // 3–9.
        let outcome = self.run(session, workspace, &tool, args, trace_id).await;

        // The line cap (ARCH-025), decided once, before anything is stored: an outcome too
        // large to send becomes `core.bug`, and that error is what the row keeps, so every
        // replay gets it too. The tool has run, so the key is not released.
        let encoded = serde_json::to_string(&outcome).expect("an Outcome serialises");
        let (outcome, encoded, keep) = if fits_response_result(&encoded) {
            let keep = !matches!(outcome, Outcome::Error { .. });
            (outcome, encoded, keep)
        } else {
            tracing::error!(
                tool = %p.name,
                trace_id,
                bytes = encoded.len(),
                "a tool's result was too large to send; the caller gets core.bug"
            );
            let outcome = too_large(&p.name, trace_id);
            let encoded = serde_json::to_string(&outcome).expect("an Outcome serialises");
            (outcome, encoded, true)
        };

        // Finish the idempotency row: `ok` and `pending_confirmation` are stored, an error
        // releases the key (except the one above). A row that can't be finished after the
        // tool ran stays `running`: the tool must not run again for this key.
        match claim {
            None => outcome,
            Some(claim) => match claim.finish(keep.then_some(encoded)).await {
                Ok(()) => outcome,
                Err(e) if keep => ran_but_not_saved(&e, &p.name, trace_id),
                Err(e) => state_unavailable(&e, trace_id),
            },
        }
    }

    /// Stage 1's workspace part: `None` without a session workspace; `workspace.unsupported`
    /// when it is no longer the default (the hub re-reads `suite.toml` before it says so, so
    /// a default set outside this core is seen); else the hub's open workspace.
    async fn open_workspace(
        &self,
        session: &Session,
        trace_id: &str,
    ) -> Result<Option<Arc<OpenWorkspace>>, ErrorBody> {
        let Some(ws) = &session.workspace else {
            return Ok(None);
        };
        let is_default = |hub: &WorkspaceHub| hub.default_id().as_deref() == Some(ws.id.as_str());
        if !is_default(&self.hub) {
            self.hub.refresh_default();
            if !is_default(&self.hub) {
                return Err(self.hub.unsupported(ws, trace_id));
            }
        }
        self.hub.open(ws).await.map(Some).map_err(|mut error| {
            error.trace_id = trace_id.to_string();
            error
        })
    }

    /// `idem_begin` for `key` in the workspace's database, or the suite one without a
    /// workspace.
    async fn begin(
        &self,
        in_flight: &CallGuard,
        workspace: Option<&Arc<OpenWorkspace>>,
        scope: String,
        key: String,
        hash: &str,
        trace_id: &str,
    ) -> Result<Begun, StateError> {
        let db = match workspace {
            Some(open) => DbHandle::Workspace(open.clone()),
            None => DbHandle::Suite(self.states.clone()),
        };
        let now = self
            .clock
            .utc_now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        Ok(match db.get().idem_begin(&scope, &key, hash, now).await? {
            IdemBegin::Fresh => Begun::Fresh(Claim {
                db,
                scope,
                key,
                follow_up: Some(in_flight.follow_up()),
                finished: false,
            }),
            IdemBegin::Replay(stored) => Begun::Done(Box::new(replay(&stored, &key, trace_id))),
            IdemBegin::Conflict => Begun::Done(Box::new(Outcome::Error {
                error: ErrorBody::new(
                    &codes::IDEMPOTENCY_CONFLICT,
                    format!("The idempotency key {key:?} was used before with other arguments."),
                    trace_id,
                ),
            })),
            IdemBegin::InProgress => Begun::Done(Box::new(Outcome::Error {
                error: ErrorBody::new(
                    &codes::IDEMPOTENCY_IN_PROGRESS,
                    format!(
                        "A call with the idempotency key {key:?} is still running; retry with the same key."
                    ),
                    trace_id,
                ),
            })),
        })
    }

    /// Stages 3–9.
    async fn run(
        &self,
        session: &Arc<Session>,
        workspace: Option<Arc<OpenWorkspace>>,
        tool: &Arc<dyn Tool>,
        args: Value,
        trace_id: &str,
    ) -> Outcome<ToolOutput> {
        let hooks = &self.hooks;
        let d = tool.descriptor();
        // Resolve, the project part (ARCH-088, ARCH-098), now that the arguments are valid.
        let named = named_project(d, &args);
        let project = match &workspace {
            None => None,
            Some(open) => Some(self.project_of(session, open, named.as_deref()).await),
        };
        // 3. Settings; 4. estimate.
        let request = declares(d, "settings")
            .then(|| args.get("settings").and_then(Value::as_object))
            .flatten();
        let settings = match hooks
            .settings
            .resolve(SettingsInput {
                session,
                descriptor: d,
                workspace: workspace.as_deref(),
                project: project.as_deref(),
                request,
            })
            .await
        {
            Ok(settings) => settings,
            Err(mut error) => {
                error.trace_id = trace_id.to_string();
                return Outcome::Error { error };
            }
        };
        let estimate = hooks.estimate.estimate(d);
        // 5. Confirm and caps.
        match hooks.confirm.check(session, d, &estimate) {
            ConfirmDecision::Proceed => {}
            ConfirmDecision::Pending(pending) => return Outcome::PendingConfirmation(pending),
            ConfirmDecision::Unavailable => {
                return Outcome::Error {
                    error: ErrorBody::new(
                        &codes::CONFIRM_UNAVAILABLE,
                        format!(
                            "{} needs your confirmation, and this session has no way to ask you. Run it from the Worldwright app.",
                            d.name
                        ),
                        trace_id,
                    ),
                };
            }
        }
        // 6. Reserve.
        if let Err(mut error) = hooks.reserve.reserve(session, &estimate) {
            error.trace_id = trace_id.to_string();
            return Outcome::Error { error };
        }
        // 7. Execute, in its own task: a panic stays in it.
        let open = workspace.clone();
        let ctx = CallCtx {
            session: session.clone(),
            trace_id: trace_id.to_string(),
            settings,
            events: self.events.clone(),
            workspace,
            project,
            record: Arc::new(RecordSlot::new()),
        };
        let running = tool.clone();
        let run_ctx = ctx.clone();
        // In the pipeline's span: a panic in the tool is logged with the call's trace id.
        let joined =
            tokio::spawn(async move { running.run(&run_ctx, args).await }.in_current_span()).await;
        let mut outcome = match joined {
            Ok(Ok(output)) => Outcome::Ok(output),
            Ok(Err(mut error)) => {
                error.trace_id = trace_id.to_string();
                Outcome::Error { error }
            }
            Err(e) => {
                let text = join_error_text(e);
                tracing::error!(tool = %d.name, trace_id, panic = %text, "a tool panicked");
                Outcome::Error {
                    error: core_bug(&d.name, text, trace_id),
                }
            }
        };
        // A call that named a project and ended `ok` makes it the session's active project
        // and, on a read-write workspace, the workspace's hint (ARCH-088).
        if let (Outcome::Ok(_), Some(open), Some(slug)) = (&outcome, &open, &named)
            && ww_schema::is_slug(slug)
            && let Err(e) = remember_project(&self.active, &session.id, open, slug).await
        {
            tracing::warn!(project = %slug, error = %e, "the active-project hint in state.json could not be written");
        }
        // 8. Meter; 9. record (CORE-020): what the tool left in its record slot.
        hooks.meter.settle(session, &outcome);
        let plan = ctx.record.take();
        hooks.record.record(&ctx, d, plan, &mut outcome).await;
        outcome
    }

    /// The call's project in `open`: `named`, else the session's active project, else the
    /// workspace's `state.json` hint, else `sketchbook` (ARCH-088, ARCH-098).
    async fn project_of(
        &self,
        session: &Session,
        open: &OpenWorkspace,
        named: Option<&str>,
    ) -> String {
        if let Some(named) = named {
            return named.to_string();
        }
        let active = self.active.get(&session.id);
        let hint = match active {
            Some(_) => None,
            None => read_hint(&open.store).await,
        };
        resolve_project(None, active, hint)
    }
}

/// Tools whose `project` argument is a filter (omitted means every project), so it is never
/// filled in from the session and never sets the active project.
const PROJECT_FILTER_TOOLS: &[&str] = &["assets.query"];

/// Whether `d`'s input schema declares the argument `arg`.
fn declares(d: &ToolDescriptor, arg: &str) -> bool {
    d.input_schema
        .get("properties")
        .and_then(|p| p.get(arg))
        .is_some()
}

/// The project a call names in its `project` argument, for a tool that declares one and whose
/// `project` isn't a filter. It is the call's project as given, slug or not: the tool checks
/// it, so a bad name is never swapped for the session's project.
fn named_project(d: &ToolDescriptor, args: &Value) -> Option<String> {
    if PROJECT_FILTER_TOOLS.contains(&d.name.as_str()) || !declares(d, "project") {
        return None;
    }
    args.get("project")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// What `idem_begin` let the call do.
enum Begun {
    /// Run it; the claim finishes the row.
    Fresh(Claim),
    /// It is answered already: a replay or an idempotency error. Boxed: an `Outcome` holds
    /// `serde_json::Value`s, which `preserve_order` (ARCH-076) makes ordered maps.
    Done(Box<Outcome<ToolOutput>>),
}

/// A database the dispatcher holds across the call. A workspace one holds the whole open
/// workspace, so a workspace the hub lets go of mid-call stays open until the row is finished
/// (its last holder closes it).
#[derive(Clone)]
enum DbHandle {
    Suite(Arc<StateRegistry>),
    Workspace(Arc<OpenWorkspace>),
}

impl DbHandle {
    fn get(&self) -> &StateDb {
        match self {
            DbHandle::Suite(states) => states.suite(),
            DbHandle::Workspace(open) => &open.state,
        }
    }
}

/// A `running` idempotency row this call owns. [`Claim::finish`] ends it; a claim dropped
/// without finishing releases the row in a task of its own, which stays in flight until the
/// release is done.
struct Claim {
    db: DbHandle,
    scope: String,
    key: String,
    /// Counts the release as in flight, so the pipeline ending does not end the wait for it.
    /// Started in `Drop`, while the pipeline still holds the call's own guard (the claim
    /// lives inside the pipeline).
    follow_up: Option<FollowUp>,
    finished: bool,
}

impl Claim {
    /// Stores `outcome` (`Some`) or releases the key (`None`).
    async fn finish(mut self, outcome: Option<String>) -> Result<(), StateError> {
        self.finished = true;
        self.db
            .get()
            .idem_finish(&self.scope, &self.key, outcome)
            .await
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let (db, scope, key) = (self.db.clone(), self.scope.clone(), self.key.clone());
        tracing::warn!(scope, key, "an unfinished idempotency claim is released");
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            let in_flight = self.follow_up.take().map(FollowUp::start);
            rt.spawn(async move {
                let _ = db.get().idem_finish(&scope, &key, None).await;
                // The database before the call: whoever waits for the call (a drain) may
                // close the database next.
                drop(db);
                drop(in_flight);
            });
        }
    }
}

/// `core.bug` for an outcome of `tool` too large to send in one line (ARCH-025).
fn too_large(tool: &str, trace_id: &str) -> Outcome<ToolOutput> {
    Outcome::Error {
        error: ErrorBody::new(
            &codes::CORE_BUG,
            format!("The result of {tool} was too large to send. This is a bug in Worldwright."),
            trace_id,
        ),
    }
}

/// The outcome stored for `key`, as it was stored. One too large to send (stored by an
/// earlier build) is `core.bug` instead.
fn replay(stored: &str, key: &str, trace_id: &str) -> Outcome<ToolOutput> {
    if !fits_response_result(stored) {
        tracing::error!(
            key,
            bytes = stored.len(),
            "a stored outcome is too large to send; the caller gets core.bug"
        );
        return Outcome::Error {
            error: ErrorBody::new(
                &codes::CORE_BUG,
                "The stored result of this call was too large to send. This is a bug in Worldwright.",
                trace_id,
            ),
        };
    }
    serde_json::from_str(stored).unwrap_or_else(|e| {
        tracing::error!(key, error = %e, "a stored outcome does not parse");
        Outcome::Error {
            error: ErrorBody::new(
                &codes::CORE_BUG,
                "The stored result of this call cannot be read.",
                trace_id,
            ),
        }
    })
}

/// `state.unavailable` for a call whose tool ran but whose outcome couldn't be stored: its row
/// stays `running` until the core restarts, and `data.ran` is `true`.
fn ran_but_not_saved(e: &StateError, tool: &str, trace_id: &str) -> Outcome<ToolOutput> {
    tracing::error!(tool, trace_id, error = %e, "a tool ran, but its outcome could not be stored; its idempotency row stays running");
    let mut error = ErrorBody::new(
        &codes::STATE_UNAVAILABLE,
        "The call ran, but the core could not save its result. Until the core restarts, a retry \
         with the same idempotency key reports it as still running; check what it did before \
         you retry with a new key.",
        trace_id,
    );
    error.data = Some(json!({ "ran": true }));
    Outcome::Error { error }
}

fn state_unavailable(e: &StateError, trace_id: &str) -> Outcome<ToolOutput> {
    tracing::error!(trace_id, error = %e, "state.db failed during a tool call");
    Outcome::Error {
        error: ErrorBody::new(
            &codes::STATE_UNAVAILABLE,
            "The core's state database is not available; retry with the same idempotency key.",
            trace_id,
        ),
    }
}

/// `core.bug` for a panic in `tool`, with the panic text as untrusted detail (ARCH-060).
fn core_bug(tool: &str, panic_text: String, trace_id: &str) -> ErrorBody {
    let mut error = ErrorBody::new(
        &codes::CORE_BUG,
        format!("{tool} failed with an internal error. This is a bug in Worldwright."),
        trace_id,
    );
    error.detail = Some(Detail {
        text: panic_text,
        untrusted: true,
    });
    error
}

/// A panic's message, or why the task ended.
fn join_error_text(e: JoinError) -> String {
    if !e.is_panic() {
        return e.to_string();
    }
    let payload = e.into_panic();
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic without a message".to_string())
}

/// `[A-Za-z0-9_-]{1,64}`.
fn is_trace_id(t: &str) -> bool {
    (1..=64).contains(&t.len())
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
