//! The production [`Router`] (ARCH-029, ARCH-031, QA-010): per connection a small state
//! machine around `core/hello`. Infrastructure, not a CORE-014 module.
//!
//! - [`ConnState::AwaitingHello`]: only `core/hello` is served; anything else is -32001. A
//!   hello that fails with an error result (an invalid engine token, an unknown workspace)
//!   leaves the connection here, so the client may try again until the hello timeout.
//! - [`ConnState::Refused`]: the hello named another major version and got the frozen
//!   `protocol.incompatible` refusal. `core/drain` drains the core only when the client's
//!   major is greater than the core's (a newer client may replace an older core, ARCH-023);
//!   otherwise it gets the same frozen refusal and the core keeps running (an older client
//!   never stops a newer core, ARCH-014). Everything else is -32001.
//! - [`ConnState::Ready`]: the session exists. A second hello is -32600. `core/drain` replies
//!   `{"status":"ok","draining":true}` and begins a requested drain. `tools/list` and
//!   `tools/call` go to the [`Dispatcher`] (the `tools` module; a draining core answers new
//!   calls with `core.draining`); `ww/subscribe` and `ww/unsubscribe` go to the [`EventBus`];
//!   `app/*` is refused with `caller.not_allowed` for anyone but a verified `ui:*` caller, and
//!   is -32601 for one, since plan 2 defines no `app/*` method (ARCH-031, APP-040).
//!
//! **Lifecycle** (ARCH-020, ruling P23). [`Router::opened`] and [`Router::closed`] feed the
//! [`Lifecycle`]'s connection count. A refused connection stops counting when it is refused
//! (an incompatible client cannot keep an otherwise idle core alive) and is closed once the
//! core drains; `AwaitingHello` and `Ready` connections count until they close.
//!
//! **Events** (ARCH-032). A connection is attached to the event bus (its forwarder started on
//! the connection's outbound channel) when its session is established, and dropped from it in
//! [`Router::closed`]. A subscription is made under the connection-map lock, and `closed`
//! drops the client under it too, so a `ww/subscribe` still running when its connection closes
//! cannot leave a forgotten client in the bus. A topic that is not a valid name, or a
//! `since_seq` cursor for a topic not in `topics`, is -32602; the results are
//! `Outcome<SubscribeResult>` and `Outcome<UnsubscribeResult>`.
//!
//! Hello handling follows a fixed order. `protocol` is read first and alone
//! ([`peek_protocol`]), so a client of any version gets the frozen refusal before any other
//! field is checked; only then are the params parsed strictly (-32602 on an unknown field).
//! Then the caller is verified, the workspace resolved and the session minted.
//!
//! [`Router::opened`] starts the hello timeout: a connection that has neither a session nor a
//! refusal when it runs out is closed.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use ww_platform::process::{ProcessInfo, ProcessInspector};
use ww_platform::{Env, IdSource};
use ww_protocol::{
    BUILD_ID, CAPABILITIES, CallerKind, CoreInfo, DrainParams, DrainResult, ErrorBody, HelloParams,
    HelloResult, Id, Negotiation, Outcome, PROTOCOL, ProtocolVersion, Request, Response, RpcError,
    SubscribeParams, SubscribeResult, ToolCallParams, ToolOutput, ToolsListParams, ToolsListResult,
    UnsubscribeParams, UnsubscribeResult, WorkspaceInfo, codes, incompatible_refusal,
    is_valid_topic, method, negotiate, peek_protocol, rpc_code, schema_hash, shared_capabilities,
};

use crate::config::CoreConfig;
use crate::events::EventBus;
use crate::identity::{EngineTokens, TrustedDirs, claude_ancestor, resolve_caller};
use crate::lifecycle::{DrainReason, Lifecycle};
use crate::server::{ConnCtx, Router};
use crate::session::Session;
use crate::tools::Dispatcher;
use crate::workspace::resolve_workspace;

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
    dispatcher: Dispatcher,
    /// The dispatcher's bus.
    events: Arc<EventBus>,
    /// The dispatcher's lifecycle.
    lifecycle: Arc<Lifecycle>,
}

impl RpcRouter {
    /// A router for the core configured by `cfg`, serving the pipe `pipe`, whose tool calls go
    /// to `dispatcher` and whose subscriptions to the dispatcher's event bus.
    pub fn new(cfg: &CoreConfig, pipe: &str, dispatcher: Dispatcher) -> RpcRouter {
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
            dispatcher,
            events,
            lifecycle,
        }
    }

    /// The state of connection `conn_id`; `None` once it has closed.
    pub fn state(&self, conn_id: u64) -> Option<ConnState> {
        self.conns().get(&conn_id).map(|c| c.state.clone())
    }

    fn conns(&self) -> MutexGuard<'_, HashMap<u64, Conn>> {
        // The map stays consistent even if a holder panicked: every update is one statement.
        self.conns.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Moves `conn_id` out of `AwaitingHello` and stops its hello timeout.
    fn settle(&self, conn_id: u64, state: ConnState) {
        if let Some(conn) = self.conns().get_mut(&conn_id) {
            conn.state = state;
            conn.hello_settled.cancel();
        }
    }

    /// Settles `conn` as refused: it stops counting towards idle and is closed once the core
    /// drains (ruling P23).
    fn refuse(&self, conn: &ConnCtx, client_major: u16) {
        let uncount = match self.conns().get_mut(&conn.conn_id) {
            Some(c) => {
                c.state = ConnState::Refused { client_major };
                c.hello_settled.cancel();
                std::mem::replace(&mut c.counted, false)
            }
            None => false,
        };
        if uncount {
            self.lifecycle.connection_closed();
        }
        if self.lifecycle.is_draining() {
            // Closing now could beat the refusal to the client; the pipe's close ends it.
            return;
        }
        let (draining, close) = (self.lifecycle.drain_token(), conn.close.clone());
        tokio::spawn(async move {
            tokio::select! {
                () = draining.cancelled() => close.cancel(),
                () = close.cancelled() => {}
            }
        });
    }

    async fn route(&self, conn: &ConnCtx, req: Request) -> Option<Response> {
        let id = req.id?;
        let state = self.state(conn.conn_id).unwrap_or(ConnState::AwaitingHello);
        let response = match (state, req.method.as_str()) {
            (ConnState::AwaitingHello, method::HELLO) => self.hello(conn, id, req.params),
            (ConnState::AwaitingHello, _) => fault(
                id,
                rpc_code::HANDSHAKE_REQUIRED,
                "call core/hello first on this connection",
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
                        let tools = self.dispatcher.registry().list_for(&session.caller);
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
            (ConnState::Ready(_), method::SUBSCRIBE) => self.subscribe(conn, id, req.params),
            (ConnState::Ready(_), method::UNSUBSCRIBE) => self.unsubscribe(conn, id, req.params),
            (ConnState::Ready(session), m) if m.starts_with(method::APP_PREFIX) => {
                match session.caller {
                    // Plan 2 defines no `app/*` method.
                    CallerKind::Ui(_) => not_found(id),
                    _ => {
                        let trace_id = self.ids.id("t");
                        tracing::info!(
                            conn_id = conn.conn_id,
                            method = m,
                            caller = %session.caller,
                            trace_id,
                            "app/* refused: the caller is not an app"
                        );
                        let error = ErrorBody::new(
                            &codes::CALLER_NOT_ALLOWED,
                            format!("{m} is for Worldwright apps only."),
                            trace_id,
                        );
                        outcome::<ToolOutput>(id, Outcome::Error { error })
                    }
                }
            }
            (ConnState::Ready(_), _) => not_found(id),
        };
        Some(response)
    }

    /// `core/hello`, in the order of the module docs.
    fn hello(&self, conn: &ConnCtx, id: Id, params: Option<Value>) -> Response {
        let raw = params.unwrap_or(Value::Null);
        // 1. The protocol, alone.
        let client_version = match peek_protocol(&raw) {
            Ok(v) => v,
            Err(error) => {
                return Response::Error {
                    id: Some(id),
                    error,
                };
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
                return outcome::<HelloResult>(id, Outcome::Error { error: refusal });
            }
        };
        // 3. Strict parse; 4. the declared caller kind.
        let p: HelloParams = match parse_params(Some(raw)) {
            Ok(p) => p,
            Err(error) => {
                return Response::Error {
                    id: Some(id),
                    error,
                };
            }
        };
        let declared: CallerKind = match p.caller_kind.parse() {
            Ok(kind) => kind,
            Err(e) => {
                let error = RpcError::new(rpc_code::INVALID_PARAMS, format!("caller_kind: {e}"));
                return Response::Error {
                    id: Some(id),
                    error,
                };
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
        // 5. Who is calling.
        let client = self.client_process(conn);
        let (caller, engine) = match resolve_caller(
            &declared,
            p.engine_token.as_deref(),
            &client,
            &self.trusted,
            self.engine_tokens.as_ref(),
            &trace_id,
        ) {
            Ok(resolved) => resolved,
            Err(error) => return self.hello_error(conn, id, error),
        };
        if caller == CallerKind::Mcp && !claude_ancestor(self.inspector.as_ref(), &client) {
            tracing::warn!(
                conn_id = conn.conn_id,
                client_pid = client.pid,
                trace_id,
                "the MCP server's parent or grandparent is not claude.exe; it stays mcp"
            );
        }
        // 6. The workspace: an engine session's own, else the requested one.
        let requested = engine
            .as_ref()
            .map_or(p.workspace.as_str(), |e| e.workspace.as_str());
        let workspace = match resolve_workspace(&self.env, requested, &trace_id) {
            Ok(ws) => ws,
            Err(error) => return self.hello_error(conn, id, error),
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
        self.settle(conn.conn_id, ConnState::Ready(session));
        // Events can reach the connection from now on.
        self.events.attach(conn.conn_id, conn.outbound.clone());
        // 9. The reply.
        outcome(id, Outcome::Ok(result))
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
        let _ = conn
            .outbound
            .send(ww_protocol::encode_response(&reply))
            .await;
        self.lifecycle.begin_drain(DrainReason::Requested);
        None
    }

    /// `ww/subscribe` (ARCH-032).
    fn subscribe(&self, conn: &ConnCtx, id: Id, params: Option<Value>) -> Response {
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
        let topics = {
            let conns = self.conns();
            if conns.contains_key(&conn.conn_id) {
                self.events.subscribe(conn.conn_id, &p.topics, &p.since_seq)
            } else {
                // Closed meanwhile: nothing to subscribe, and nobody reads the reply.
                Vec::new()
            }
        };
        outcome(id, Outcome::Ok(SubscribeResult { topics }))
    }

    /// `ww/unsubscribe` (ARCH-032).
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
    fn hello_error(&self, conn: &ConnCtx, id: Id, error: ww_protocol::ErrorBody) -> Response {
        tracing::info!(
            conn_id = conn.conn_id,
            code = %error.code,
            trace_id = %error.trace_id,
            "core/hello failed"
        );
        outcome::<HelloResult>(id, Outcome::Error { error })
    }

    /// The client process. One that cannot be inspected is verified as nothing (an empty
    /// image), so every claim it makes resolves to `cli`.
    fn client_process(&self, conn: &ConnCtx) -> ProcessInfo {
        self.inspector.inspect(conn.client_pid).unwrap_or_else(|e| {
            tracing::warn!(
                conn_id = conn.conn_id,
                client_pid = conn.client_pid,
                error = %e,
                "cannot inspect the client process; it is treated as unverified"
            );
            ProcessInfo {
                pid: conn.client_pid,
                image: PathBuf::new(),
                user_sid: String::new(),
                parent_pid: None,
            }
        })
    }
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

    fn handle<'a>(
        &'a self,
        conn: &'a ConnCtx,
        req: Request,
    ) -> Pin<Box<dyn Future<Output = Option<Response>> + Send + 'a>> {
        Box::pin(self.route(conn, req))
    }

    fn closed(&self, conn: &ConnCtx) {
        let counted = {
            let mut conns = self.conns();
            let counted = conns.remove(&conn.conn_id).is_some_and(|gone| {
                gone.hello_settled.cancel();
                gone.counted
            });
            // Under the map lock: see the module docs on events.
            self.events.drop_client(conn.conn_id);
            counted
        };
        if counted {
            self.lifecycle.connection_closed();
        }
    }
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

fn fault(id: Id, code: i64, message: &str) -> Response {
    Response::Error {
        id: Some(id),
        error: RpcError::new(code, message),
    }
}

fn not_found(id: Id) -> Response {
    fault(id, rpc_code::METHOD_NOT_FOUND, "method not found")
}

fn outcome<T: Serialize>(id: Id, outcome: Outcome<T>) -> Response {
    let result = serde_json::to_value(&outcome).expect("an Outcome serialises");
    Response::Result { id, result }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use ww_protocol::{DrainParams, ToolsListParams};

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
