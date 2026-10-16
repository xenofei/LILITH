//! The dispatcher (CORE-015, CORE-016, CORE-017, QA-012, QA-016): every `tools/call` runs the
//! same stages in order.
//!
//! 1. **Resolve.** The trace id is the caller's (`[A-Za-z0-9_-]{1,64}`) or a fresh one. The
//!    tool must exist and be visible to the caller, else `tool.unknown`. A tool whose effect
//!    is not `read` needs an idempotency key, else `idempotency.key_required` (ARCH-035).
//! 2. **Validate.** The tool checks the arguments (`params.invalid`). For effect ≠ `read`,
//!    `idem_begin` in the session's workspace `state.db`, or the suite one without a
//!    workspace, under the scope `"<caller kind>|<workspace id or none>"`: a finished call
//!    with the same arguments is replayed byte for byte, another hash is
//!    `idempotency.conflict`, a running one `idempotency.in_progress`.
//! 3. **Settings**, 4. **estimate**, 5. **confirm and caps** (`confirm.unavailable`, or a
//!    `pending_confirmation` result), 6. **reserve**: the [`PipelineHooks`].
//! 7. **Execute** in its own task: a panic is `core.bug` with the panic text as untrusted
//!    detail, and the core goes on (QA-012, ARCH-060).
//! 8. **Meter** and 9. **record**: the hooks again.
//!
//! Then the line cap (ARCH-025): an outcome too large to send in one line (with room for its
//! response envelope) becomes `core.bug`. Then the idempotency row is finished: `ok` and
//! `pending_confirmation` outcomes are stored, an error releases the key, except the
//! too-large `core.bug`, which is stored (the tool ran; a retry must not run it again). The
//! size is decided before the row is written, so the stored outcome and every replay agree.
//! A `state.db` failure is `state.unavailable`.
//!
//! **Draining** (ARCH-021). Before stage 1 the call is admitted by the [`Lifecycle`]: a
//! draining core answers `core.draining` (retry with the same idempotency key once a core
//! runs again). An admitted call counts as in flight until its pipeline ends, even if its
//! caller has gone, so a drain waits for it.
//!
//! **Cancellation.** [`Dispatcher::call`] runs the pipeline in a task of its own, so a caller
//! that goes away (a client killed mid-call) cannot stop it between `idem_begin` and
//! `idem_finish`: the outcome is stored for the retry. A [`Claim`] that is dropped unfinished
//! anyway (a bug in a stage) releases its row from its `Drop`.

use std::sync::Arc;
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};
use tokio::task::JoinError;
use ww_platform::{Clock, IdSource};
use ww_protocol::{
    CodeSpec, Detail, EffectClass, ErrorBody, Outcome, ToolCallParams, ToolOutput, args_hash,
    codes, fits_response_result,
};

use super::registry::{CallCtx, Registry, Tool};
use super::stages::{ConfirmDecision, PipelineHooks};
use crate::events::EventBus;
use crate::lifecycle::Lifecycle;
use crate::session::Session;
use crate::state::{IdemBegin, StateDb, StateError, StateRegistry};

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
    ids: Arc<dyn IdSource>,
    clock: Arc<dyn Clock>,
    lifecycle: Arc<Lifecycle>,
}

impl std::fmt::Debug for Dispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dispatcher")
            .field("tools", &self.inner.reg)
            .finish_non_exhaustive()
    }
}

impl Dispatcher {
    pub fn new(
        reg: Registry,
        hooks: PipelineHooks,
        states: Arc<StateRegistry>,
        events: Arc<EventBus>,
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
                ids,
                clock,
                lifecycle,
            }),
        }
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
        // Its own task: a caller that goes away cannot cut the pipeline short. The call stays
        // in flight until the pipeline ends.
        let pipeline = tokio::spawn(async move {
            let _in_flight = in_flight;
            inner.pipeline(session, p, trace).await
        });
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
        session: Arc<Session>,
        p: ToolCallParams,
        trace_id: String,
    ) -> Outcome<ToolOutput> {
        let name = p.name.clone();
        let outcome = self.stages(&session, p, &trace_id).await;
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
                match self.begin(session, scope, key, &hash, trace_id).await {
                    Ok(Begun::Fresh(claim)) => Some(claim),
                    Ok(Begun::Done(outcome)) => return outcome,
                    Err(e) => return state_unavailable(&e, trace_id),
                }
            }
        };

        // 3–9.
        let outcome = self.run(session, &tool, args, trace_id).await;

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
        // releases the key (except the one above).
        match claim {
            None => outcome,
            Some(claim) => match claim.finish(keep.then_some(encoded)).await {
                Ok(()) => outcome,
                Err(e) => state_unavailable(&e, trace_id),
            },
        }
    }

    /// `idem_begin` for `key` in the session's database.
    async fn begin(
        &self,
        session: &Session,
        scope: String,
        key: String,
        hash: &str,
        trace_id: &str,
    ) -> Result<Begun, StateError> {
        let db = match &session.workspace {
            Some(ws) => DbHandle::Workspace(self.states.for_workspace(ws).await?),
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
                finished: false,
            }),
            IdemBegin::Replay(stored) => Begun::Done(replay(&stored, &key, trace_id)),
            IdemBegin::Conflict => Begun::Done(Outcome::Error {
                error: ErrorBody::new(
                    &codes::IDEMPOTENCY_CONFLICT,
                    format!("The idempotency key {key:?} was used before with other arguments."),
                    trace_id,
                ),
            }),
            IdemBegin::InProgress => Begun::Done(Outcome::Error {
                error: ErrorBody::new(
                    &codes::IDEMPOTENCY_IN_PROGRESS,
                    format!(
                        "A call with the idempotency key {key:?} is still running; retry with the same key."
                    ),
                    trace_id,
                ),
            }),
        })
    }

    /// Stages 3–9.
    async fn run(
        &self,
        session: &Arc<Session>,
        tool: &Arc<dyn Tool>,
        args: Value,
        trace_id: &str,
    ) -> Outcome<ToolOutput> {
        let hooks = &self.hooks;
        let d = tool.descriptor();
        // 3. Settings; 4. estimate.
        let settings = hooks.settings.resolve(session);
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
        let ctx = CallCtx {
            session: session.clone(),
            trace_id: trace_id.to_string(),
            settings,
            events: self.events.clone(),
        };
        let running = tool.clone();
        let joined = tokio::spawn(async move { running.run(&ctx, args).await }).await;
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
        // 8. Meter; 9. record.
        hooks.meter.settle(session, &outcome);
        hooks.record.record(session, d, &mut outcome);
        outcome
    }
}

/// What `idem_begin` let the call do.
enum Begun {
    /// Run it; the claim finishes the row.
    Fresh(Claim),
    /// It is answered already: a replay or an idempotency error.
    Done(Outcome<ToolOutput>),
}

/// A database the dispatcher holds across the call.
#[derive(Clone)]
enum DbHandle {
    Suite(Arc<StateRegistry>),
    Workspace(Arc<StateDb>),
}

impl DbHandle {
    fn get(&self) -> &StateDb {
        match self {
            DbHandle::Suite(states) => states.suite(),
            DbHandle::Workspace(db) => db,
        }
    }
}

/// A `running` idempotency row this call owns. [`Claim::finish`] ends it; a claim dropped
/// without finishing releases the row in a task of its own.
struct Claim {
    db: DbHandle,
    scope: String,
    key: String,
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
            rt.spawn(async move {
                let _ = db.get().idem_finish(&scope, &key, None).await;
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
