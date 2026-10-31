//! The production [`Router`] (ARCH-029, ARCH-031, QA-010): per connection a small state
//! machine around `core/hello`. Infrastructure, not a CORE-014 module.
//!
//! - [`ConnState::AwaitingHello`]: only `core/hello` and `core/prove` are served; anything else
//!   is -32001. A hello that fails with an error result (an unknown workspace, a token with the
//!   wrong caller kind) leaves the connection here, so the client may try again until the hello
//!   timeout. `core/prove` (Appendix G "Connections") sets the event a client names, whatever
//!   its version, and leaves the connection here too, with its hello timeout running. After a
//!   good hello or a `protocol.incompatible` refusal it is -32600.
//! - An invalid engine token is terminal (ARCH-050: the connection "is refused", never
//!   downgraded): the `caller.engine_token_invalid` reply is queued, then the connection is
//!   closed. The writer still sends that reply (its flush grace); nothing read after that
//!   hello is served.
//! - [`ConnState::Refused`]: the hello named another major version and got the frozen
//!   `protocol.incompatible` refusal. `core/drain` drains the core only when the client's
//!   major is greater than the core's (a newer client may replace an older core, ARCH-023);
//!   otherwise it gets the same frozen refusal and the core keeps running (an older client
//!   never stops a newer core, ARCH-014). Everything else is -32001.
//! - [`ConnState::Ready`]: the session exists. A second hello is -32600. `core/drain` replies
//!   `{"status":"ok","draining":true}` and begins a requested drain. `tools/list` and
//!   `tools/call` go to the [`Dispatcher`] (the `tools` module; a draining core answers new
//!   calls with `core.draining`); `lil/subscribe` and `lil/unsubscribe` go to the [`EventBus`];
//!   `app/*` is refused with `caller.not_allowed` for anyone but a verified `ui:*` caller
//!   (ARCH-031, APP-040), except `app/confirmations.list`, which `cli` may call too; protocol
//!   1.1's `app/confirmations.list` and `app/confirmation.answer` go to the [`Confirm`]
//!   (they need `confirmations.v1` or `caps.v1` in the shared capabilities, else
//!   `caller.not_allowed`), `app/caps.set` goes to the [`Caps`] (it needs `caps.v1`, else
//!   `caller.not_allowed`; CORE-008), protocol 1.2's `app/queue.pause` and `app/queue.resume`
//!   go to the jobs (they need `queue.v1`, else `caller.not_allowed`; CORE-160),
//!   `app/desktop.set` goes to the [`DesktopSettingsStore`] (it needs `desktop.v1`, else
//!   `caller.not_allowed`; a category `desktop.get` doesn't list is -32602; APP-146), as does
//!   `app/confirmation.answer`'s `toast_token` (a deny from a toast, P4a-R9),
//!   `app/set_context` goes to the dispatcher, which keeps the sessions' projects (it needs
//!   `context.v1`, else `caller.not_allowed`; ARCH-088), and any
//!   other `app/*` method is -32601 for an app. `tools/list`
//!   carries the per-connection `requiresUserInteraction` marker ([`Confirm::annotate`]).
//!   `app/*` methods are admitted as `tools/call` is (APP-041): a draining core answers them
//!   `core.draining`, each counts as a call in flight until it is answered, and each error
//!   outcome goes to the dispatcher's error sink (the recent errors) with the method as its
//!   tool. A verified `ui:*` session that can answer confirmations counts in the
//!   [`crate::confirm::UiPresence`] while its connection is open.
//!   `core/drain` and `lil/*` run on the connection's reader ([`Router::runs_inline`]): they
//!   are cheap and take no in-flight permit, so they are not refused or delayed for want of
//!   one. Reading stays in order, though: one read after a request that waits for a permit
//!   (the connection has [`crate::MAX_IN_FLIGHT_PER_CONN`] requests running) waits with it. A
//!   request handler that panics in its own task is answered with `core.bug`
//!   ([`Router::panicked`]).
//!
//! **An app's cap prompts** (APP-029; Appendix G "Connections"). A `ui:*` session's pending
//! cap confirmations end with its app's process, not with one of its connections, so a
//! reconnect keeps them: the process is opened at the hello, and when the connection closes
//! the confirmations are cancelled once the process exits. They are cancelled at once instead
//! when there is no process to wait for (it could not be opened), and on a switch: the same
//! process (client pid and caller kind) has a session in another workspace and none in this
//! one when the connection closes, or says hello in another workspace afterwards. Either way,
//! once no app that can answer is connected, the toasts held back while it was in front go out.
//! A process is assumed to keep one session at a time: a second live one of the same pid and
//! caller in another workspace counts as a switch. The waits live in one core instance: after
//! a core restart, prompts reloaded from `state.db` end at their expiry (Appendix G).
//!
//! **Lifecycle** (ARCH-020, ruling P23). [`Router::opened`] and [`Router::closed`] feed the
//! [`Lifecycle`]'s connection count. A refused connection stops counting when it is refused
//! (an incompatible client cannot keep an otherwise idle core alive) and is closed once the
//! core drains; `AwaitingHello` and `Ready` connections count until they close. A `Ready`
//! connection's session forgets its active project when it closes (ARCH-088).
//!
//! **Events** (ARCH-032). A connection is attached to the event bus (its forwarder started on
//! the connection's outbound channel) when its session is established, and dropped from it in
//! [`Router::closed`]. `closed` drops the client under the connection-map lock; a
//! `lil/subscribe` awaits the bus (its snapshots read the workspace) with no lock held, then
//! checks the map under the lock and drops the client again if its connection closed
//! meanwhile, so it cannot leave a forgotten client in the bus. A topic that is not a valid name, or a
//! `since_seq` cursor for a topic not in `topics`, is -32602; the results are
//! `Outcome<SubscribeResult>` and `Outcome<UnsubscribeResult>`.
//!
//! Hello handling follows a fixed order. `protocol` is read first and alone
//! ([`peek_protocol`]), so a client of any version gets the frozen refusal before any other
//! field is checked; only then are the params parsed strictly (-32602 on an unknown field).
//! Next, a draining core answers the hello `core.draining` and starts no session (the
//! connection stays `AwaitingHello`). Then the caller is
//! verified and the workspace resolved, on a blocking thread (they inspect processes and read
//! `suite.toml`); the reader awaits that, so a request pipelined right behind the hello still
//! sees its session (I4). Then the session is minted.
//!
//! [`Router::opened`] starts the hello timeout: a connection that has neither a session nor a
//! refusal when it runs out is closed.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use lil_platform::pipe::set_proof_event;
use lil_platform::process::{ProcessHandle, ProcessInfo, ProcessInspector};
use lil_platform::{Env, IdSource};
use lil_protocol::app;
use lil_protocol::{
    BUILD_ID, CAPABILITIES, CallerKind, CoreInfo, DrainParams, DrainResult, ErrorBody, HelloParams,
    HelloResult, Id, Negotiation, Outcome, PROTOCOL, ProtocolVersion, ProveParams, ProveResult,
    Request, Response, RpcError, SubscribeParams, SubscribeResult, ToolCallParams, ToolOutput,
    ToolsListParams, ToolsListResult, UnsubscribeParams, UnsubscribeResult, WorkspaceInfo, codes,
    incompatible_refusal, is_valid_topic, method, negotiate, peek_protocol, rpc_code, schema_hash,
    shared_capabilities,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize as _, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::config::CoreConfig;
use crate::confirm::{
    Confirm, ConfirmationRow, ListState, can_answer, has_confirmation_capability,
};
use crate::desktop::{DesktopSettingsStore, unknown_category};
use crate::events::EventBus;
use crate::identity::{EngineSession, EngineTokens, TrustedDirs, resolve_caller};
use crate::jobs::{Lane, PauseReason};
use crate::lifecycle::{DrainReason, Lifecycle};
use crate::meter::Caps;
use crate::session::Session;
use crate::tools::Dispatcher;
use crate::transport::{ConnCtx, ConnObserver, Router};
use crate::workspace::{WorkspaceRef, resolve_workspace};

/// Where a connection stands in the handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnState {
    AwaitingHello,
    /// The hello's major version differed from the core's.
    Refused {
        client_major: u16,
    },
    Ready(Arc<Session>),
}

/// One open connection.
struct Conn {
    state: ConnState,
    /// Counted in the lifecycle's connections (until it is refused or closes).
    counted: bool,
    /// Cancelled once the hello succeeded or was refused, or the connection closed: ends the
    /// hello timeout.
    hello_settled: CancellationToken,
    /// Cancelled in [`Router::closed`]: ends the tasks watching this connection.
    ended: CancellationToken,
    /// A `ui:*` session's app process, opened at the hello: its cap prompts end when it
    /// exits (module docs, "An app's cap prompts"). `None` for any other caller, or a process
    /// that could not be opened.
    app: Option<ProcessHandle>,
}

/// An app process some of whose `ui:*` sessions lost their connection while it still ran:
/// their pending cap confirmations wait for it to exit, or for a switch (module docs). One task
/// waits per process.
struct LeftApp {
    /// Each such session's id and workspace id.
    sessions: Vec<(String, String)>,
}

/// The core's router.
pub struct RpcRouter {
    env: Env,
    ids: Arc<dyn IdSource>,
    inspector: Arc<dyn ProcessInspector>,
    trusted: TrustedDirs,
    engine_tokens: Arc<dyn EngineTokens>,
    hello_timeout: Duration,
    /// `c_<16 hex>`, minted once per core.
    instance_id: String,
    core: CoreInfo,
    conns: Mutex<HashMap<u64, Conn>>,
    /// By client pid: the app processes whose sessions' cap prompts wait for them
    /// ([`LeftApp`]). Locked after `conns` when both are held.
    left: Arc<Mutex<HashMap<u32, LeftApp>>>,
    dispatcher: Dispatcher,
    /// The dispatcher's bus.
    events: Arc<EventBus>,
    /// The dispatcher's lifecycle.
    lifecycle: Arc<Lifecycle>,
    /// The confirmations: the `app/*` methods, the apps that can answer, and the
    /// `tools/list` marker (`mcp_confirmation`).
    confirm: Arc<Confirm>,
    /// The caps `app/caps.set` changes; `None`: the method is -32601.
    caps: Option<Arc<Caps>>,
    /// The desktop settings `app/desktop.set` changes; `None`: the method is -32601.
    desktop: Option<Arc<DesktopSettingsStore>>,
    /// Told when a session is established and when its connection ends.
    observers: Vec<Arc<dyn ConnObserver>>,
    /// Unit tests only: requests with this method panic (`CoreConfig::panic_on_method`).
    #[cfg(test)]
    panic_on_method: Option<&'static str>,
}

impl RpcRouter {
    /// A router for the core configured by `cfg`, serving the pipe `pipe`, whose tool calls go
    /// to `dispatcher`, whose subscriptions to the dispatcher's event bus, and whose
    /// confirmation methods to `confirm`.
    pub fn new(
        cfg: &CoreConfig,
        pipe: &str,
        dispatcher: Dispatcher,
        confirm: Arc<Confirm>,
    ) -> RpcRouter {
        let events = dispatcher.events().clone();
        let lifecycle = dispatcher.lifecycle().clone();
        RpcRouter {
            env: cfg.env.clone(),
            ids: cfg.ids.clone(),
            inspector: cfg.inspector.clone(),
            trusted: cfg.trusted.clone(),
            engine_tokens: cfg.engine_tokens.clone(),
            hello_timeout: cfg.hello_timeout,
            instance_id: cfg.ids.id("c"),
            core: CoreInfo {
                pid: std::process::id(),
                channel: cfg.env.channel().to_string(),
                pipe: pipe.to_string(),
            },
            conns: Mutex::new(HashMap::new()),
            left: Arc::new(Mutex::new(HashMap::new())),
            dispatcher,
            events,
            lifecycle,
            confirm,
            caps: None,
            desktop: None,
            observers: Vec::new(),
            #[cfg(test)]
            panic_on_method: cfg.panic_on_method,
        }
    }

    /// Serves `app/caps.set` with `caps`.
    #[must_use]
    pub fn with_caps(mut self, caps: Arc<Caps>) -> RpcRouter {
        self.caps = Some(caps);
        self
    }

    /// Serves `app/desktop.set` with `store`.
    #[must_use]
    pub fn with_desktop(mut self, store: Arc<DesktopSettingsStore>) -> RpcRouter {
        self.desktop = Some(store);
        self
    }

    /// Also tells `observer` about each session and each connection's end.
    #[must_use]
    pub fn with_observer(mut self, observer: Arc<dyn ConnObserver>) -> RpcRouter {
        self.observers.push(observer);
        self
    }

    /// The state of connection `conn_id`; `None` once it has closed.
    pub fn state(&self, conn_id: u64) -> Option<ConnState> {
        self.conns().get(&conn_id).map(|c| c.state.clone())
    }

    fn conns(&self) -> MutexGuard<'_, HashMap<u64, Conn>> {
        // The map stays consistent even if a holder panicked: every update is one statement.
        self.conns.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Moves `conn_id` out of `AwaitingHello` into `Ready` with its session (and `app`, its app
    /// process) and stops its hello timeout. An app that can answer confirmations makes them
    /// answerable (APP-051) from now until [`Router::closed`], and the observers are told of the
    /// session: all under the map lock, so a connection that closes meanwhile is never counted.
    /// An app's session in another workspace than one it left ends that one's cap prompts (a
    /// switch; module docs).
    fn settle_ready(
        &self,
        conn_id: u64,
        session: Arc<Session>,
        build_id: &str,
        app: Option<ProcessHandle>,
    ) {
        let mut conns = self.conns();
        if let Some(conn) = conns.get_mut(&conn_id) {
            if can_answer(&session) {
                self.confirm.presence().opened();
            }
            // Under the map lock too: a connection that closed meanwhile is never registered
            // as running or holding.
            for observer in &self.observers {
                observer.ready(conn_id, &session);
                observer.said_hello(conn_id, &session, build_id);
            }
            if matches!(session.caller, CallerKind::Ui(_)) {
                let switched: Vec<(String, String)> =
                    match lock(&self.left).get_mut(&session.client_pid) {
                        Some(left) => {
                            let (gone, kept) = std::mem::take(&mut left.sessions)
                                .into_iter()
                                .partition(|(_, ws)| ws != session.workspace_id());
                            left.sessions = kept;
                            gone
                        }
                        None => Vec::new(),
                    };
                for (id, _) in switched {
                    self.cancel_app_prompts(id);
                }
            }
            conn.state = ConnState::Ready(session);
            conn.app = app;
            conn.hello_settled.cancel();
        }
    }

    /// A `ui:*` session's connection closed; `conns` are the connections still open and `app`
    /// its app process. Its cap prompts are cancelled at once when that process has a session
    /// in another workspace and none in this one (a switch), or when there is no process to
    /// wait for; otherwise when the process exits (module docs). Either way, the toasts held
    /// back while an app was in front go out if no app that can answer is left.
    fn app_connection_closed(
        &self,
        conns: &HashMap<u64, Conn>,
        session: &Session,
        app: Option<ProcessHandle>,
    ) {
        let (mut here, mut elsewhere) = (false, false);
        for conn in conns.values() {
            if let ConnState::Ready(other) = &conn.state
                && other.client_pid == session.client_pid
                && other.caller == session.caller
            {
                if other.workspace_id() == session.workspace_id() {
                    here = true;
                } else {
                    elsewhere = true;
                }
            }
        }
        let app = match app {
            Some(app) if here || !elsewhere => app,
            _ => return self.cancel_app_prompts(session.id.clone()),
        };
        self.confirm.after_app_closed();
        let (pid, entry) = (
            session.client_pid,
            (session.id.clone(), session.workspace_id().to_string()),
        );
        let mut left = lock(&self.left);
        if let Some(waiting) = left.get_mut(&pid) {
            // A task waits for this process already.
            waiting.sessions.push(entry);
            return;
        }
        left.insert(
            pid,
            LeftApp {
                sessions: vec![entry],
            },
        );
        let (confirm, left) = (self.confirm.clone(), self.left.clone());
        tokio::spawn(async move {
            if let Err(e) = app.wait().await {
                tracing::warn!(pid, error = %e, "cannot wait for an app to exit; its cap prompts are cancelled now");
            }
            let sessions = lock(&left).remove(&pid).map(|l| l.sessions);
            for (id, _) in sessions.unwrap_or_default() {
                confirm.cancel_for_session(&id).await;
            }
            confirm.after_app_closed();
        });
    }

    /// Cancels the pending cap confirmations of the `ui:*` session `session_id` now
    /// (Appendix G "Connections"); then, with no app left that can answer, the toasts held back
    /// while it was in front go out (the notifier's reconcile).
    fn cancel_app_prompts(&self, session_id: String) {
        let confirm = self.confirm.clone();
        tokio::spawn(async move {
            confirm.cancel_for_session(&session_id).await;
            confirm.after_app_closed();
        });
    }

    /// Settles `conn` as refused: it stops counting towards idle and is closed once the core
    /// drains (ruling P23).
    fn refuse(&self, conn: &ConnCtx, client_major: u16) {
        let (uncount, ended) = match self.conns().get_mut(&conn.conn_id) {
            Some(c) => {
                c.state = ConnState::Refused { client_major };
                c.hello_settled.cancel();
                (std::mem::replace(&mut c.counted, false), c.ended.clone())
            }
            // Closed meanwhile: nothing to count or watch.
            None => return,
        };
        if uncount {
            self.lifecycle.connection_closed();
        }
        if self.lifecycle.is_draining() {
            // Closing now could beat the refusal to the client; the pipe's close ends it.
            return;
        }
        // Ends with the connection however it ends (an older client polling the core must
        // not leave a task behind per connection).
        let (draining, close) = (self.lifecycle.drain_token(), conn.close.clone());
        tokio::spawn(async move {
            tokio::select! {
                () = draining.cancelled() => close.cancel(),
                () = close.cancelled() => {}
                () = ended.cancelled() => {}
            }
        });
    }

    async fn route(&self, conn: &ConnCtx, req: Request) -> Option<Response> {
        #[cfg(test)]
        if self.panic_on_method == Some(req.method.as_str()) {
            panic!("the test hook panics on {}", req.method);
        }
        let id = req.id?;
        let state = self.state(conn.conn_id).unwrap_or(ConnState::AwaitingHello);
        let response = match (state, req.method.as_str()) {
            (ConnState::AwaitingHello, method::HELLO) => {
                return self.hello(conn, id, req.params).await;
            }
            (ConnState::AwaitingHello, method::PROVE) => prove(id, req.params),
            (ConnState::AwaitingHello, _) => fault(
                id,
                rpc_code::HANDSHAKE_REQUIRED,
                "call core/hello first on this connection",
            ),
            (_, method::PROVE) => fault(
                id,
                rpc_code::INVALID_REQUEST,
                "core/prove comes before core/hello on a connection",
            ),
            (_, method::HELLO) => fault(
                id,
                rpc_code::INVALID_REQUEST,
                "core/hello was already called on this connection",
            ),
            // The drain direction (ARCH-023, ARCH-014).
            (ConnState::Refused { client_major }, method::DRAIN) => {
                if client_major > PROTOCOL.major {
                    return self.drain(conn, id, req.params).await;
                }
                let trace_id = self.ids.id("t");
                tracing::info!(
                    conn_id = conn.conn_id,
                    client_major,
                    trace_id,
                    "core/drain refused: an older client does not stop a newer core"
                );
                let refusal = incompatible_refusal(PROTOCOL, &trace_id);
                outcome::<DrainResult>(id, Outcome::Error { error: refusal })
            }
            (ConnState::Refused { .. }, _) => fault(
                id,
                rpc_code::HANDSHAKE_REQUIRED,
                "this connection's core/hello was refused (protocol.incompatible)",
            ),
            (ConnState::Ready(session), method::TOOLS_LIST) => {
                match parse_params::<ToolsListParams>(req.params) {
                    Ok(ToolsListParams {}) => {
                        let listed = self.dispatcher.registry().list_for(&session.caller);
                        let tools = self.confirm.annotate(&session.caller, listed);
                        outcome(id, Outcome::Ok(ToolsListResult { tools }))
                    }
                    Err(error) => Response::Error {
                        id: Some(id),
                        error,
                    },
                }
            }
            (ConnState::Ready(session), method::TOOLS_CALL) => {
                match parse_params::<ToolCallParams>(req.params) {
                    Ok(params) => outcome(id, self.dispatcher.call(session, params).await),
                    Err(error) => Response::Error {
                        id: Some(id),
                        error,
                    },
                }
            }
            (ConnState::Ready(_), method::DRAIN) => return self.drain(conn, id, req.params).await,
            (ConnState::Ready(_), method::SUBSCRIBE) => self.subscribe(conn, id, req.params).await,
            (ConnState::Ready(_), method::UNSUBSCRIBE) => self.unsubscribe(conn, id, req.params),
            (ConnState::Ready(session), m) if m.starts_with(method::APP_PREFIX) => {
                self.admitted_app(conn, id, &session, m, req.params).await
            }
            (ConnState::Ready(_), _) => not_found(id),
        };
        Some(response)
    }

    /// An `app/*` method, admitted as `tools/call` is (APP-041; Appendix G "Connections"): a
    /// draining core answers it `core.draining`, it counts as a call in flight until it is
    /// answered, and its error outcome goes to the dispatcher's error sink with the method as
    /// its tool.
    async fn admitted_app(
        &self,
        conn: &ConnCtx,
        id: Id,
        session: &Session,
        m: &str,
        params: Option<Value>,
    ) -> Response {
        let Some(_in_flight) = self.lifecycle.try_call_started() else {
            let trace_id = self.ids.id("t");
            tracing::info!(conn_id = conn.conn_id, method = m, caller = %session.caller, trace_id, "app/* refused: the core is draining");
            let error = ErrorBody::new(
                &codes::CORE_DRAINING,
                "The core is shutting down, so this was not done. Call it again once a core runs.",
                trace_id,
            );
            self.dispatcher.record_error(m, &error);
            return outcome::<ToolOutput>(id, Outcome::Error { error });
        };
        let response = self.app(conn, id, session, m, params).await;
        if let Response::Result { result, .. } = &response
            && result.get("status").and_then(Value::as_str) == Some("error")
            && let Some(error) = result
                .get("error")
                .and_then(|e| ErrorBody::deserialize(e).ok())
        {
            self.dispatcher.record_error(m, &error);
        }
        response
    }

    /// An `app/*` method (module docs). A verified `ui:*` caller may call each; `cli` only
    /// `app/confirmations.list`; anyone else gets `caller.not_allowed`. Each needs
    /// `confirmations.v1` or `caps.v1` in the session's shared capabilities (`app/caps.set`:
    /// `caps.v1`), else `caller.not_allowed`. One this core does not define is -32601 for an
    /// app.
    async fn app(
        &self,
        conn: &ConnCtx,
        id: Id,
        session: &Session,
        m: &str,
        params: Option<Value>,
    ) -> Response {
        let refuse = |message: String| {
            let trace_id = self.ids.id("t");
            tracing::info!(
                conn_id = conn.conn_id,
                method = m,
                caller = %session.caller,
                trace_id,
                "app/* refused: {message}"
            );
            let error = ErrorBody::new(&codes::CALLER_NOT_ALLOWED, message, trace_id);
            outcome::<ToolOutput>(id.clone(), Outcome::Error { error })
        };
        let is_app = matches!(session.caller, CallerKind::Ui(_));
        let may_call =
            is_app || (m == app::CONFIRMATIONS_LIST && session.caller == CallerKind::Cli);
        if !may_call {
            return refuse(format!("{m} is for LILITH apps only."));
        }
        if m == app::CAPS_SET {
            let Some(caps) = &self.caps else {
                return not_found(id);
            };
            if !session
                .capabilities
                .iter()
                .any(|c| c == app::CAPS_CAPABILITY)
            {
                return refuse(format!(
                    "{m} needs the caps.v1 capability, and this session's core/hello did not offer it."
                ));
            }
            let p: app::CapsSetParams = match parse_params(params) {
                Ok(p) => p,
                Err(error) => {
                    return Response::Error {
                        id: Some(id),
                        error,
                    };
                }
            };
            return outcome(id, into_outcome(caps.set(session, p).await));
        }
        if matches!(m, app::QUEUE_PAUSE | app::QUEUE_RESUME) {
            return self.queue(id.clone(), session, m, params, refuse).await;
        }
        if m == app::SET_CONTEXT {
            if !session
                .capabilities
                .iter()
                .any(|c| c == app::CONTEXT_CAPABILITY)
            {
                return refuse(format!(
                    "{m} needs the context.v1 capability, and this session's core/hello did not offer it."
                ));
            }
            let p: app::SetContextParams = match parse_params(params) {
                Ok(p) => p,
                Err(error) => {
                    return Response::Error {
                        id: Some(id),
                        error,
                    };
                }
            };
            let trace_id = self.ids.id("t");
            let set = self
                .dispatcher
                .set_context(session, p.project.as_ref().map(Option::as_deref), &trace_id)
                .await;
            return outcome(id, into_outcome(set));
        }
        if m == app::DESKTOP_SET {
            let Some(store) = &self.desktop else {
                return not_found(id);
            };
            if !session
                .capabilities
                .iter()
                .any(|c| c == app::DESKTOP_CAPABILITY)
            {
                return refuse(format!(
                    "{m} needs the desktop.v1 capability, and this session's core/hello did not offer it."
                ));
            }
            let p: app::DesktopSetParams = match parse_params(params) {
                Ok(p) => p,
                Err(error) => {
                    return Response::Error {
                        id: Some(id),
                        error,
                    };
                }
            };
            if let Some(category) = unknown_category(&p) {
                return fault(
                    id,
                    rpc_code::INVALID_PARAMS,
                    &format!("invalid params: {category:?} is not a notification category"),
                );
            }
            return outcome(id, into_outcome(store.set(p).await));
        }
        if !matches!(m, app::CONFIRMATIONS_LIST | app::CONFIRMATION_ANSWER) {
            return not_found(id);
        }
        if !has_confirmation_capability(session) {
            return refuse(format!(
                "{m} needs the confirmations.v1 or caps.v1 capability, and this session's \
                 core/hello did not offer either."
            ));
        }
        let invalid = |error: RpcError| Response::Error {
            id: Some(id.clone()),
            error,
        };
        if m == app::CONFIRMATIONS_LIST {
            let p: app::ConfirmationsListParams = match parse_params(params) {
                Ok(p) => p,
                Err(error) => return invalid(error),
            };
            let limit = match p.limit() {
                Ok(limit) => limit,
                Err(why) => {
                    return invalid(RpcError::new(
                        rpc_code::INVALID_PARAMS,
                        format!("invalid params: {why}"),
                    ));
                }
            };
            let which = match p.state {
                app::ListWhich::Pending => ListState::Pending,
                app::ListWhich::History => ListState::History,
            };
            let listed =
                self.confirm
                    .list(which, limit)
                    .await
                    .map(|rows| app::ConfirmationsListResult {
                        confirmations: rows.iter().map(ConfirmationRow::view).collect(),
                    });
            return outcome(id, into_outcome(listed));
        }
        let p: app::ConfirmationAnswerParams = match parse_params(params) {
            Ok(p) => p,
            Err(error) => return invalid(error),
        };
        if p.toast_token.is_some()
            && !session
                .capabilities
                .iter()
                .any(|c| c == app::DESKTOP_CAPABILITY)
        {
            return refuse(format!(
                "{m}'s toast_token needs the desktop.v1 capability, and this session's core/hello did not offer it."
            ));
        }
        let answered =
            self.confirm
                .answer(session, p)
                .await
                .map(|row| app::ConfirmationAnswerResult {
                    confirmation: row.view(),
                });
        outcome(id, into_outcome(answered))
    }

    /// `app/queue.pause {lane?}` and `app/queue.resume {lane?, held_only?}` (CORE-160;
    /// Appendix G "Pause GPU work"), for a verified app whose session shares `queue.v1`
    /// (`refuse` answers anyone else). Your pause is the reason `manual`; a resume removes only
    /// it and releases the lane's jobs a restart holds, `held_only: true` releases those jobs
    /// only (P4a-R41). `held_only` on a pause is -32602.
    async fn queue(
        &self,
        id: Id,
        session: &Session,
        m: &str,
        params: Option<Value>,
        refuse: impl Fn(String) -> Response,
    ) -> Response {
        if !session
            .capabilities
            .iter()
            .any(|c| c == app::QUEUE_CAPABILITY)
        {
            return refuse(format!(
                "{m} needs the queue.v1 capability, and this session's core/hello did not offer it."
            ));
        }
        let p: app::QueueParams = match parse_params(params) {
            Ok(p) => p,
            Err(error) => {
                return Response::Error {
                    id: Some(id),
                    error,
                };
            }
        };
        let lane = match p.lane.unwrap_or_default() {
            app::QueueLane::GpuHeavy => Lane::GpuHeavy,
            app::QueueLane::Cpu => Lane::Cpu,
        };
        let jobs = self.dispatcher.jobs();
        let state = match (m, p.held_only) {
            (app::QUEUE_PAUSE, Some(_)) => {
                return fault(
                    id,
                    rpc_code::INVALID_PARAMS,
                    "invalid params: held_only is for app/queue.resume",
                );
            }
            (app::QUEUE_PAUSE, None) => jobs.pause(lane, PauseReason::Manual).await,
            (_, Some(true)) => jobs.release_restart_held(lane).await,
            _ => jobs.resume(lane, PauseReason::Manual).await,
        };
        outcome(id, Outcome::Ok(state))
    }

    /// `core/hello`, in the order of the module docs. `None`: the reply is already queued
    /// and the connection is closing (an invalid engine token).
    async fn hello(&self, conn: &ConnCtx, id: Id, params: Option<Value>) -> Option<Response> {
        let raw = params.unwrap_or(Value::Null);
        // 1. The protocol, alone.
        let client_version = match peek_protocol(&raw) {
            Ok(v) => v,
            Err(error) => {
                return Some(Response::Error {
                    id: Some(id),
                    error,
                });
            }
        };
        // 2. A major mismatch gets the frozen refusal; no other field is looked at.
        let minor = match negotiate(client_version, PROTOCOL) {
            Negotiation::Compatible { minor } => minor,
            Negotiation::Incompatible => {
                let trace_id = self.ids.id("t");
                tracing::info!(
                    conn_id = conn.conn_id,
                    client_protocol = %client_version,
                    trace_id,
                    "core/hello refused: another major protocol version"
                );
                self.refuse(conn, client_version.major);
                let refusal = incompatible_refusal(PROTOCOL, &trace_id);
                return Some(outcome::<HelloResult>(
                    id,
                    Outcome::Error { error: refusal },
                ));
            }
        };
        // 3. Strict parse; 4. the declared caller kind.
        let p: HelloParams = match parse_params(Some(raw)) {
            Ok(p) => p,
            Err(error) => {
                return Some(Response::Error {
                    id: Some(id),
                    error,
                });
            }
        };
        let declared: CallerKind = match p.caller_kind.parse() {
            Ok(kind) => kind,
            Err(e) => {
                let error = RpcError::new(rpc_code::INVALID_PARAMS, format!("caller_kind: {e}"));
                return Some(Response::Error {
                    id: Some(id),
                    error,
                });
            }
        };
        let trace_id = self.ids.id("t");
        if p.schema_hash != schema_hash() {
            tracing::warn!(
                conn_id = conn.conn_id,
                client_schema_hash = %p.schema_hash,
                core_schema_hash = %schema_hash(),
                client_build_id = %p.build_id,
                trace_id,
                "the client's schema hash differs from the core's"
            );
        }
        // A core that no longer admits calls starts no session (Appendix G "Connections").
        if self.lifecycle.is_draining() {
            let error = ErrorBody::new(
                &codes::CORE_DRAINING,
                "The core is shutting down. Connect again once a core runs.",
                trace_id,
            );
            return Some(self.hello_error(conn, id, error));
        }
        // 5. Who is calling; 6. the workspace. On a blocking thread (module docs).
        let identify = Identify {
            inspector: self.inspector.clone(),
            trusted: self.trusted.clone(),
            engine_tokens: self.engine_tokens.clone(),
            env: self.env.clone(),
            conn_id: conn.conn_id,
            client_pid: conn.client_pid,
            declared: declared.clone(),
            engine_token: p.engine_token.clone(),
            workspace: p.workspace.clone(),
            trace_id: trace_id.clone(),
        };
        let identified = match tokio::task::spawn_blocking(move || identify.run()).await {
            Ok(identified) => identified,
            // A panic there is this handler's panic (see `transport::conn`).
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            Err(e) => {
                tracing::error!(conn_id = conn.conn_id, error = %e, trace_id, "core/hello stopped");
                let error = ErrorBody::new(
                    &codes::CORE_BUG,
                    "core/hello could not finish. This is a bug in LILITH.",
                    trace_id,
                );
                return Some(self.hello_error(conn, id, error));
            }
        };
        let (caller, engine, workspace) = match identified {
            Ok(identified) => identified,
            Err(error) if error.code == codes::CALLER_ENGINE_TOKEN_INVALID.code => {
                self.refuse_engine_token(conn, id, error).await;
                return None;
            }
            Err(error) => return Some(self.hello_error(conn, id, error)),
        };
        // 7. Capabilities; 8. the session.
        let capabilities = shared_capabilities(&p.capabilities, CAPABILITIES);
        let session = Arc::new(Session {
            id: self.ids.id("s"),
            caller,
            non_interactive: p.non_interactive,
            workspace,
            capabilities,
            client_pid: conn.client_pid,
            profile_id: match engine {
                Some(e) => e.profile_id,
                None => p.profile_id,
            },
        });
        tracing::info!(
            conn_id = conn.conn_id,
            session_id = %session.id,
            declared = %declared,
            caller = %session.caller,
            workspace = session.workspace_id(),
            client_pid = conn.client_pid,
            trace_id,
            "session started"
        );
        let result = HelloResult {
            session_id: session.id.clone(),
            instance_id: self.instance_id.clone(),
            protocol: ProtocolVersion {
                major: PROTOCOL.major,
                minor,
            }
            .to_string(),
            build_id: BUILD_ID.to_string(),
            schema_hash: schema_hash(),
            capabilities: session.capabilities.clone(),
            caller_kind: session.caller.to_string(),
            workspace: session.workspace.as_ref().map(|w| WorkspaceInfo {
                id: w.id.clone(),
                path: w.path.to_string_lossy().into_owned(),
            }),
            core: self.core.clone(),
        };
        // An app's process, so its cap prompts can wait for it (module docs).
        let app = match session.caller {
            CallerKind::Ui(_) => lil_platform::process::open(session.client_pid).ok(),
            _ => None,
        };
        self.settle_ready(conn.conn_id, session, &p.build_id, app);
        // Events can reach the connection from now on.
        self.events.attach(conn.conn_id, conn.outbound.clone());
        // 9. The reply.
        Some(outcome(id, Outcome::Ok(result)))
    }

    /// `core/drain` (ARCH-023): replies `{"status":"ok","draining":true}`, then begins a
    /// requested drain. The reply is queued before the drain begins, so it is written before
    /// the connection closes.
    async fn drain(&self, conn: &ConnCtx, id: Id, params: Option<Value>) -> Option<Response> {
        if let Err(error) = parse_params::<DrainParams>(params) {
            return Some(Response::Error {
                id: Some(id),
                error,
            });
        }
        tracing::info!(
            conn_id = conn.conn_id,
            client_pid = conn.client_pid,
            "core/drain"
        );
        let reply = outcome(id, Outcome::Ok(DrainResult { draining: true }));
        conn.send_response(&reply).await;
        self.lifecycle.begin_drain(DrainReason::Requested);
        None
    }

    /// `lil/subscribe` (ARCH-032). The bus is awaited with no connection lock held (its
    /// snapshots read the workspace); a connection that closed meanwhile is dropped from the
    /// bus again under the map lock, so no forgotten client stays behind.
    async fn subscribe(&self, conn: &ConnCtx, id: Id, params: Option<Value>) -> Response {
        let p: SubscribeParams = match parse_params(params).and_then(|p: SubscribeParams| {
            check_topics(&p.topics)?;
            match p.since_seq.keys().find(|t| !p.topics.contains(t)) {
                Some(t) => Err(RpcError::new(
                    rpc_code::INVALID_PARAMS,
                    format!("since_seq names {t:?}, which is not in topics"),
                )),
                None => Ok(p),
            }
        }) {
            Ok(p) => p,
            Err(error) => {
                return Response::Error {
                    id: Some(id),
                    error,
                };
            }
        };
        let open = self.conns().contains_key(&conn.conn_id);
        let topics = if open {
            let topics = self
                .events
                .subscribe(conn.conn_id, &p.topics, &p.since_seq)
                .await;
            let conns = self.conns();
            if !conns.contains_key(&conn.conn_id) {
                // Closed while the bus worked: `closed` may have dropped the client before the
                // subscription re-created it.
                self.events.drop_client(conn.conn_id);
            }
            topics
        } else {
            // Closed meanwhile: nothing to subscribe, and nobody reads the reply.
            Vec::new()
        };
        outcome(id, Outcome::Ok(SubscribeResult { topics }))
    }

    /// `lil/unsubscribe` (ARCH-032).
    fn unsubscribe(&self, conn: &ConnCtx, id: Id, params: Option<Value>) -> Response {
        let p: UnsubscribeParams = match parse_params(params).and_then(|p: UnsubscribeParams| {
            check_topics(&p.topics)?;
            Ok(p)
        }) {
            Ok(p) => p,
            Err(error) => {
                return Response::Error {
                    id: Some(id),
                    error,
                };
            }
        };
        let topics = self.events.unsubscribe(conn.conn_id, &p.topics);
        outcome(id, Outcome::Ok(UnsubscribeResult { topics }))
    }

    /// A hello that failed with an error result; the connection stays `AwaitingHello`.
    fn hello_error(&self, conn: &ConnCtx, id: Id, error: lil_protocol::ErrorBody) -> Response {
        tracing::info!(
            conn_id = conn.conn_id,
            code = %error.code,
            trace_id = %error.trace_id,
            "core/hello failed"
        );
        outcome::<HelloResult>(id, Outcome::Error { error })
    }

    /// An invalid engine token (ARCH-050): the refusal is queued, then the connection is
    /// closed. The writer still sends what was queued before the close.
    async fn refuse_engine_token(&self, conn: &ConnCtx, id: Id, error: ErrorBody) {
        tracing::info!(
            conn_id = conn.conn_id,
            client_pid = conn.client_pid,
            trace_id = %error.trace_id,
            "core/hello refused: the engine token is invalid; the connection is closed"
        );
        let reply = outcome::<HelloResult>(id, Outcome::Error { error });
        conn.send_response(&reply).await;
        conn.close.cancel();
    }
}

/// Steps 5 and 6 of a hello, which may block: they inspect the client process and its
/// parents, check an engine token and read `suite.toml`.
struct Identify {
    inspector: Arc<dyn ProcessInspector>,
    trusted: TrustedDirs,
    engine_tokens: Arc<dyn EngineTokens>,
    env: Env,
    conn_id: u64,
    client_pid: u32,
    declared: CallerKind,
    engine_token: Option<String>,
    workspace: String,
    trace_id: String,
}

/// The caller kind, the engine session behind an `assistant`, and the workspace.
type Identified = (CallerKind, Option<EngineSession>, Option<WorkspaceRef>);

impl Identify {
    fn run(self) -> Result<Identified, ErrorBody> {
        let Identify {
            inspector,
            trusted,
            engine_tokens,
            env,
            conn_id,
            client_pid,
            declared,
            engine_token,
            workspace,
            trace_id,
        } = self;
        // 5. Who is calling. A declared `mcp` whose parent is not `claude.exe` is `cli`.
        let client = client_process(inspector.as_ref(), conn_id, client_pid);
        let (caller, engine) = resolve_caller(
            &declared,
            engine_token.as_deref(),
            &client,
            inspector.as_ref(),
            &trusted,
            engine_tokens.as_ref(),
            &trace_id,
        )?;
        // 6. The workspace: an engine session's own, else the requested one.
        let requested = engine
            .as_ref()
            .map_or(workspace.as_str(), |e| e.workspace.as_str());
        let workspace = resolve_workspace(&env, requested, &trace_id)?;
        Ok((caller, engine, workspace))
    }
}

/// The client process. One that cannot be inspected is verified as nothing (an empty image),
/// so every claim it makes resolves to `cli`.
fn client_process(inspector: &dyn ProcessInspector, conn_id: u64, client_pid: u32) -> ProcessInfo {
    inspector.inspect(client_pid).unwrap_or_else(|e| {
        tracing::warn!(
            conn_id,
            client_pid,
            error = %e,
            "cannot inspect the client process; it is treated as unverified"
        );
        ProcessInfo {
            pid: client_pid,
            image: PathBuf::new(),
            user_sid: String::new(),
            parent_pid: None,
        }
    })
}

impl Router for RpcRouter {
    fn opened(&self, conn: &ConnCtx) {
        let hello_settled = CancellationToken::new();
        self.lifecycle.connection_opened();
        self.conns().insert(
            conn.conn_id,
            Conn {
                state: ConnState::AwaitingHello,
                counted: true,
                hello_settled: hello_settled.clone(),
                ended: CancellationToken::new(),
                app: None,
            },
        );
        let close = conn.close.clone();
        let timeout = self.hello_timeout;
        let conn_id = conn.conn_id;
        tokio::spawn(async move {
            tokio::select! {
                () = hello_settled.cancelled() => {}
                () = close.cancelled() => {}
                () = tokio::time::sleep(timeout) => {
                    tracing::info!(conn_id, ?timeout, "no core/hello in time; closing the connection");
                    close.cancel();
                }
            }
        });
    }

    fn handshake_done(&self, conn: &ConnCtx) -> bool {
        matches!(self.state(conn.conn_id), Some(ConnState::Ready(_)))
    }

    fn runs_inline(&self, req: &Request) -> bool {
        req.method == method::DRAIN || req.method.starts_with(method::PREFIX)
    }

    fn handle<'a>(
        &'a self,
        conn: &'a ConnCtx,
        req: Request,
    ) -> Pin<Box<dyn Future<Output = Option<Response>> + Send + 'a>> {
        Box::pin(self.route(conn, req))
    }

    fn panicked(&self, conn: &ConnCtx, id: Id) -> Response {
        let trace_id = self.ids.id("t");
        tracing::error!(
            conn_id = conn.conn_id,
            trace_id,
            "a request handler panicked; answered with core.bug"
        );
        let error = ErrorBody::new(
            &codes::CORE_BUG,
            "The core failed with an internal error. This is a bug in LILITH.",
            trace_id,
        );
        outcome::<ToolOutput>(id, Outcome::Error { error })
    }

    fn closed(&self, conn: &ConnCtx) {
        let counted = {
            let mut conns = self.conns();
            let counted = conns.remove(&conn.conn_id).is_some_and(|gone| {
                gone.hello_settled.cancel();
                gone.ended.cancel();
                // The session's active project lives as long as its connection (ARCH-088).
                if let ConnState::Ready(session) = &gone.state {
                    self.dispatcher.active_projects().forget(&session.id);
                    if can_answer(session) {
                        self.confirm.presence().closed();
                    }
                    // An app's pending cap confirmations end with its process, or at once
                    // when it has switched workspace (module docs).
                    if matches!(session.caller, CallerKind::Ui(_)) {
                        self.app_connection_closed(&conns, session, gone.app);
                    }
                }
                gone.counted
            });
            // Under the map lock: see the module docs on events.
            self.events.drop_client(conn.conn_id);
            counted
        };
        if counted {
            self.lifecycle.connection_closed();
        }
        for observer in &self.observers {
            observer.closed(conn.conn_id);
        }
    }
}

/// Every update is a single step, so the data stays consistent after a panic.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Parses a method's params strictly; omitted params are `{}`. Any failure is -32602
/// (QA-010), with serde's reason in the message.
pub fn parse_params<T: DeserializeOwned>(params: Option<Value>) -> Result<T, RpcError> {
    let params = params.unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    serde_json::from_value(params)
        .map_err(|e| RpcError::new(rpc_code::INVALID_PARAMS, format!("invalid params: {e}")))
}

/// -32602 unless every topic is a valid name (`^[a-z][a-z0-9_]*(:.+)?$`, ARCH-032).
fn check_topics(topics: &[String]) -> Result<(), RpcError> {
    match topics.iter().find(|t| !is_valid_topic(t)) {
        Some(t) => Err(RpcError::new(
            rpc_code::INVALID_PARAMS,
            format!("{t:?} is not a valid topic name"),
        )),
        None => Ok(()),
    }
}

/// `core/prove` (Appendix G "Connections"): sets the proof event the params name, and does nothing
/// else. A name that isn't of a proof's shape, or that can't be opened and set, is -32602.
fn prove(id: Id, params: Option<Value>) -> Response {
    let set = parse_params::<ProveParams>(params).and_then(|p| {
        set_proof_event(&p.name).map_err(|e| {
            RpcError::new(
                rpc_code::INVALID_PARAMS,
                format!("invalid params: the proof: {e}"),
            )
        })
    });
    match set {
        Ok(()) => outcome(id, Outcome::Ok(ProveResult { proved: true })),
        Err(error) => Response::Error {
            id: Some(id),
            error,
        },
    }
}

fn fault(id: Id, code: i64, message: &str) -> Response {
    Response::Error {
        id: Some(id),
        error: RpcError::new(code, message),
    }
}

fn not_found(id: Id) -> Response {
    fault(id, rpc_code::METHOD_NOT_FOUND, "method not found")
}

fn into_outcome<T>(result: Result<T, ErrorBody>) -> Outcome<T> {
    match result {
        Ok(value) => Outcome::Ok(value),
        Err(error) => Outcome::Error { error },
    }
}

fn outcome<T: Serialize>(id: Id, outcome: Outcome<T>) -> Response {
    let result = serde_json::to_value(&outcome).expect("an Outcome serialises");
    Response::Result { id, result }
}

#[cfg(test)]
mod tests {
    use lil_protocol::{DrainParams, ToolsListParams};
    use serde_json::json;

    use super::*;

    #[test] // spec: QA-010, ARCH-029
    fn params_failures_are_invalid_params() {
        // `arguments` is required on the wire.
        let err = parse_params::<ToolCallParams>(Some(json!({"name": "test.echo"}))).unwrap_err();
        assert_eq!(err.code, rpc_code::INVALID_PARAMS);
        assert!(err.message.contains("arguments"), "{}", err.message);
        let err = parse_params::<ToolCallParams>(Some(json!({"name": "x", "arguments": []})))
            .unwrap_err();
        assert_eq!(err.code, rpc_code::INVALID_PARAMS);
        let ok = parse_params::<ToolCallParams>(Some(json!({"name": "x", "arguments": {}})));
        assert!(ok.unwrap().arguments.is_empty());

        let err = parse_params::<DrainParams>(Some(json!({"now": true}))).unwrap_err();
        assert_eq!(err.code, rpc_code::INVALID_PARAMS);
        // Omitted params are `{}`.
        parse_params::<DrainParams>(None).unwrap();
        parse_params::<ToolsListParams>(None).unwrap();
        let err = parse_params::<HelloParams>(None).unwrap_err();
        assert_eq!(err.code, rpc_code::INVALID_PARAMS);
    }
}

/// A panicking handler, through the `CoreConfig::panic_on_method` test hook (compiled into
/// this crate's unit tests only).
#[cfg(test)]
mod panic_tests {
    use std::time::{Duration, Instant};

    use lil_testkit::{FakeClock, RawPipe, SeqIds, TestEnv};
    use serde_json::json;

    use super::*;
    use crate::{CoreThread, start_in_thread};

    /// Failure deadline for anything the test waits on.
    const DEADLINE: Duration = Duration::from_secs(30);

    fn core_panicking_on(env: &TestEnv, method: &'static str) -> CoreThread {
        start_in_thread(CoreConfig {
            clock: FakeClock::new(),
            ids: Arc::new(SeqIds::new()),
            test_parent_pid: None,
            panic_on_method: Some(method),
            ..CoreConfig::for_env(env.env.clone()).expect("core config")
        })
    }

    fn wait_until(what: &str, cond: impl Fn() -> bool) {
        let deadline = Instant::now() + DEADLINE;
        while !cond() {
            assert!(Instant::now() < deadline, "{what}: not within {DEADLINE:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test] // spec: ARCH-025, ARCH-020
    fn inline_handler_panic_still_runs_closed() {
        let env = TestEnv::new();
        let core = core_panicking_on(&env, method::SUBSCRIBE);
        let lifecycle = core.core().lifecycle().clone();
        let events = core.core().events().clone();
        // The bus's in-process clients (the notifier) aren't connections.
        let in_process = events.clients();
        let mut pipe = RawPipe::connect(&env);
        let hello = pipe.hello(json!({}));
        assert_eq!(hello["result"]["status"], "ok", "{hello}");
        assert_eq!(lifecycle.connections(), 1);
        assert_eq!(events.clients(), in_process + 1);

        // `lil/subscribe` runs on the reader; its panic ends the connection, and `closed` still
        // runs: the connection stops counting and leaves the event bus.
        pipe.send_line(
            &json!({
                "jsonrpc": "2.0", "id": 2, "method": method::SUBSCRIBE,
                "params": {"topics": ["test:a"]},
            })
            .to_string(),
        );
        assert!(pipe.wait_closed(DEADLINE), "the connection stays open");
        wait_until("closed to run", || {
            lifecycle.connections() == 0 && events.clients() == in_process
        });

        // The core still serves.
        let mut fresh = RawPipe::connect(&env);
        let again = fresh.hello(json!({}));
        assert_eq!(again["result"]["status"], "ok", "{again}");
    }

    #[test] // spec: QA-010, QA-012
    fn spawned_handler_panic_answers_core_bug() {
        let env = TestEnv::new();
        let _core = core_panicking_on(&env, method::TOOLS_LIST);
        let mut pipe = RawPipe::connect(&env);
        let hello = pipe.hello(json!({}));
        assert_eq!(hello["result"]["status"], "ok", "{hello}");

        let listed = pipe.call(method::TOOLS_LIST, json!({}));
        let result = &listed["result"];
        assert_eq!(result["status"], "error", "{listed}");
        assert_eq!(result["error"]["code"], "core.bug", "{listed}");
        assert_eq!(result["error"]["category"], "bug", "{listed}");
        assert_eq!(result["error"]["retry"], "never", "{listed}");

        // The connection still serves.
        let echo = pipe.tool("test.echo", json!({"text": "after"}), None);
        assert_eq!(echo["status"], "ok", "{echo}");
    }
}
