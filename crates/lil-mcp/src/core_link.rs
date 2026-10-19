//! The one core connection (ARCH-012, ARCH-014, ARCH-018).
//!
//! `lil-mcp` connects as `mcp` from the folder Claude Code started it in
//! (`ClientConfig::for_mcp`), starting the core when none runs. The connection is made on the
//! first request that needs the core and kept; once it has closed (the core stopped, say), the
//! next request makes a new one, starting a new core. A call whose connection closes under it
//! is sent once more on the new connection with the same idempotency key, so it still runs at
//! most once. Nothing else is kept: the core holds all policy and state.

use std::path::PathBuf;
use std::sync::Arc;

use lil_client::{Client, ClientConfig, ClientError, ResolvedWorkspace};
use lil_protocol::codes::{CORE_BUG, CORE_DRAINING, PROTOCOL_INCOMPATIBLE};
use lil_protocol::lowering::assert_injective;
use lil_protocol::{ErrorBody, HelloResult, NO_WORKSPACE, Outcome, ToolDescriptor, ToolOutput};
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;

/// The name of the tool `lil-mcp` answers itself.
pub const LIL_STATUS: &str = "lil.status";

/// A live session on the core, and what it lists.
pub struct Linked {
    pub client: Client,
    /// The core's `tools/list` for this session.
    pub tools: Vec<ToolDescriptor>,
    /// The registered names: the core's tools and [`LIL_STATUS`].
    pub names: Vec<String>,
}

impl Linked {
    pub fn hello(&self) -> &HelloResult {
        self.client.hello()
    }

    /// The workspace folder the core reported for this session.
    pub fn workspace(&self) -> Option<PathBuf> {
        self.hello()
            .workspace
            .as_ref()
            .map(|w| PathBuf::from(&w.path))
    }
}

/// What a request finds when it needs the core.
pub enum Link {
    Connected(Arc<Linked>),
    /// The core speaks another major protocol (ARCH-014): only `lil_status` is listed.
    Incompatible {
        core_protocol: Option<String>,
        error: ErrorBody,
    },
    /// No core could be reached or started.
    Unreachable(ErrorBody),
}

pub struct CoreLink {
    /// The folder Claude Code started the server in (ARCH-030).
    cwd: PathBuf,
    current: Mutex<Option<Arc<Linked>>>,
}

impl CoreLink {
    pub fn new(cwd: PathBuf) -> CoreLink {
        CoreLink {
            cwd,
            current: Mutex::new(None),
        }
    }

    /// The live session, connecting (and starting the core) when there is none or it closed.
    pub async fn link(&self) -> Link {
        let mut current = self.current.lock().await;
        if let Some(linked) = current.as_ref().filter(|l| !l.client.is_closed()) {
            return Link::Connected(linked.clone());
        }
        *current = None;
        let link = connect(&self.cwd).await;
        if let Link::Connected(linked) = &link {
            *current = Some(linked.clone());
        }
        link
    }

    /// `tools/call` of the registered tool `name`, with `key` as its idempotency key. Also
    /// returns the session it ran on, for the lowering.
    pub async fn call(
        &self,
        name: &str,
        args: Map<String, Value>,
        key: String,
    ) -> (Outcome<ToolOutput>, Option<Arc<Linked>>) {
        let mut retried = false;
        loop {
            let linked = match self.link().await {
                Link::Connected(linked) => linked,
                Link::Incompatible { error, .. } | Link::Unreachable(error) => {
                    return (Outcome::Error { error }, None);
                }
            };
            match linked
                .client
                .call_tool(name, args.clone(), Some(key.clone()))
                .await
            {
                Ok(outcome) => return (outcome, Some(linked)),
                // The core went away: the next `link` connects again. The same key keeps the
                // call from running twice.
                Err(ClientError::Closed | ClientError::Io(_)) if !retried => retried = true,
                Err(e) => {
                    let error = error_body(&e);
                    return (Outcome::Error { error }, Some(linked));
                }
            }
        }
    }
}

/// Finds or starts the core and opens a session as `mcp`.
async fn connect(cwd: &std::path::Path) -> Link {
    let mut cfg = match ClientConfig::for_mcp(cwd) {
        Ok(cfg) => cfg,
        Err(e) => return Link::Unreachable(error_body(&e)),
    };
    let client = match Client::connect(&cfg).await {
        // A workspace found from the folder that the suite does not know: no workspace, as
        // `lil status` does.
        Err(ClientError::UnknownWorkspace { origin, .. }) => {
            tracing::warn!(
                workspace = %origin.path.display(),
                "the workspace is not registered in suite.toml; continuing without one"
            );
            cfg.set_workspace(ResolvedWorkspace {
                id: NO_WORKSPACE.to_string(),
                origin: None,
            });
            Client::connect(&cfg).await
        }
        other => other,
    };
    let client = match client {
        Ok(client) => client,
        Err(ClientError::Incompatible { core_protocol }) => {
            let error = incompatible(&core_protocol);
            let core_protocol = (!core_protocol.is_empty()).then_some(core_protocol);
            return Link::Incompatible {
                core_protocol,
                error,
            };
        }
        Err(e) => return Link::Unreachable(error_body(&e)),
    };
    let tools = match client.list_tools().await {
        Ok(tools) => tools,
        Err(e) => return Link::Unreachable(error_body(&e)),
    };
    let mut names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
    names.push(LIL_STATUS.to_string());
    if let Err(collision) = assert_injective(&names) {
        let error = ErrorBody::new(
            &CORE_BUG,
            format!(
                "The core's tools cannot be shown over MCP: {collision}. This is a bug in LILITH."
            ),
            lil_client::mint_id("t"),
        );
        return Link::Unreachable(error);
    }
    Link::Connected(Arc::new(Linked {
        client,
        tools,
        names,
    }))
}

/// `protocol.incompatible` for a core that speaks `core_protocol` (Appendix D:
/// `data.core_protocol`).
fn incompatible(core_protocol: &str) -> ErrorBody {
    let message = ClientError::Incompatible {
        core_protocol: core_protocol.to_string(),
    }
    .to_string();
    let mut error = ErrorBody::new(&PROTOCOL_INCOMPATIBLE, message, lil_client::mint_id("t"));
    if !core_protocol.is_empty() {
        error.data = Some(json!({ "core_protocol": core_protocol }));
    }
    error
}

/// The error result for a client failure. The core's own error results keep their code; a
/// core that cannot be reached or started is `core.draining` (transient: retry the call with
/// the same `retry_token`), with the client's plain-language message.
fn error_body(e: &ClientError) -> ErrorBody {
    match e {
        ClientError::Refused(error) | ClientError::UnknownWorkspace { error, .. } => error.clone(),
        ClientError::Incompatible { core_protocol } => incompatible(core_protocol),
        other => ErrorBody::new(&CORE_DRAINING, other.to_string(), lil_client::mint_id("t")),
    }
}
