//! The `rmcp` stdio server `lil` (ARCH-012): protocol adaptation only. `tools/list` is the
//! core's list for this `mcp` session, lowered, plus `lil_status`, which `lil-mcp` answers
//! itself; `tools/call` takes `retry_token` out of the arguments and uses it as the call's
//! idempotency key (a fresh key otherwise); results are lowered as Appendix F says, and an
//! `open_in` or `ui.show` that started a stopped app says so in one more text block
//! ([`crate::launch`]). On an incompatible or unreachable core only `lil_status` is listed
//! (ARCH-014).

use std::collections::BTreeMap;
use std::sync::Arc;

use lil_protocol::codes::{PARAMS_INVALID, TOOL_UNKNOWN};
use lil_protocol::lowering::{
    INSTRUCTIONS, LowerCtx, McpResult, RETRY_TOKEN, lower_outcome, mcp_input_schema, mcp_meta,
    mcp_name,
};
use lil_protocol::{EffectClass, ErrorBody, Outcome, ToolDescriptor, ToolOutput};
use rmcp::ErrorData as McpError;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    InitializeResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler};
use serde_json::{Value, json};

use crate::core_link::{CoreLink, LIL_STATUS, Link, Linked};
use crate::{launch, resources};

const LIL_STATUS_DESCRIPTION: &str = "Whether LILITH's core is connected (status connected, \
incompatible or unreachable), the caller kind the core gave this session (mcp, or cli when \
this server was not started by Claude Code itself), the protocol and build, the session's \
workspace and the core's process, and, when it is not connected, the error saying what to do: \
protocol.incompatible (update the older of the two) or core.unreachable (no core could be \
reached or started: wait, then call again). Call it first. Takes no arguments.";

pub struct LilServer {
    link: CoreLink,
}

impl LilServer {
    pub fn new(link: CoreLink) -> LilServer {
        LilServer { link }
    }

    /// `lil_status` (Appendix F): always an `ok` result.
    async fn status(&self) -> (Outcome<ToolOutput>, Option<Arc<Linked>>) {
        let (structured, summary, linked) = match self.link.link().await {
            Link::Connected(linked) => {
                let hello = linked.hello();
                let structured = json!({
                    "status": "connected",
                    "caller": hello.caller_kind,
                    "protocol": hello.protocol,
                    "build_id": hello.build_id,
                    "workspace": hello.workspace.as_ref().map(|w| json!({"id": w.id, "path": w.path})),
                    "core": {"pid": hello.core.pid, "channel": hello.core.channel},
                    "error": null,
                });
                let mut summary = format!("Connected to the core as {}.", hello.caller_kind);
                if let Some(note) = &linked.workspace_note {
                    summary.push(' ');
                    summary.push_str(note);
                }
                (structured, summary, Some(linked))
            }
            Link::Incompatible {
                core_protocol,
                error,
            } => {
                let summary = error.message.clone();
                let structured = json!({
                    "status": "incompatible", "caller": null, "protocol": core_protocol,
                    "build_id": null, "workspace": null, "core": null, "error": error,
                });
                (structured, summary, None)
            }
            Link::Unreachable(error) => {
                let summary = error.message.clone();
                let structured = json!({
                    "status": "unreachable", "caller": null, "protocol": null,
                    "build_id": null, "workspace": null, "core": null, "error": error,
                });
                (structured, summary, None)
            }
        };
        let out = ToolOutput {
            structured,
            artifacts: Vec::new(),
            summary_text: summary,
        };
        (Outcome::Ok(out), linked)
    }
}

/// `lil_status`'s descriptor, lowered like the core's.
fn lil_status_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: LIL_STATUS.to_string(),
        description: LIL_STATUS_DESCRIPTION.to_string(),
        effect: EffectClass::Read,
        module: "mcp".to_string(),
        input_schema: json!({"type": "object", "properties": {}, "additionalProperties": false}),
        output_schema: json!({}),
        annotations: BTreeMap::new(),
    }
}

/// A descriptor as an MCP tool: lowered name, the description, the input schema with
/// `retry_token`, and `_meta`.
fn mcp_tool(d: &ToolDescriptor) -> Result<Tool, McpError> {
    let meta = mcp_meta(d);
    let mut tool = json!({
        "name": mcp_name(&d.name),
        "description": d.description,
        "inputSchema": mcp_input_schema(&d.input_schema),
    });
    if !meta.is_empty() {
        tool["_meta"] = Value::Object(meta);
    }
    serde_json::from_value(tool)
        .map_err(|e| McpError::internal_error(format!("tool {}: {e}", d.name), None))
}

/// A lowered result as an MCP `CallToolResult`: the text blocks, and `structuredContent`.
fn call_result(lowered: McpResult) -> CallToolResult {
    let content = lowered.text.into_iter().map(ContentBlock::text).collect();
    let mut result = if lowered.is_error {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    };
    result.structured_content = lowered.structured;
    result
}

/// An error result `lil-mcp` makes itself.
fn local_error(spec: &lil_protocol::CodeSpec, message: String) -> Outcome<ToolOutput> {
    Outcome::Error {
        error: ErrorBody::new(spec, message, lil_client::mint_id("t")),
    }
}

impl ServerHandler for LilServer {
    fn get_info(&self) -> ServerConfig {
        let capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build();
        InitializeResult::new(capabilities)
            .with_server_info(Implementation::new("lil", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let mut tools = vec![mcp_tool(&lil_status_descriptor())?];
        if let Link::Connected(linked) = self.link.link().await {
            for d in &linked.tools {
                tools.push(mcp_tool(d)?);
            }
        }
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let mut args = request.arguments.unwrap_or_default();
        let key = match args.remove(RETRY_TOKEN) {
            None | Some(Value::Null) => lil_client::mint_id("ik"),
            Some(Value::String(token)) if !token.is_empty() => token,
            Some(_) => {
                let refused = local_error(
                    &PARAMS_INVALID,
                    "retry_token must be a non-empty string.".to_string(),
                );
                return Ok(lower(&refused, None).into());
            }
        };
        let mut started = None;
        let (outcome, linked) = if request.name == mcp_name(LIL_STATUS) {
            self.status().await
        } else {
            match self.link.link().await {
                Link::Connected(linked) => {
                    match linked
                        .tools
                        .iter()
                        .find(|t| mcp_name(&t.name) == request.name)
                    {
                        Some(d) => {
                            let (outcome, on) = self.link.call(&d.name, args, key).await;
                            if let Some(on) = &on {
                                started = launch::after_call(on, &d.name, &outcome).await;
                            }
                            (outcome, on)
                        }
                        None => {
                            let unknown = local_error(
                                &TOOL_UNKNOWN,
                                format!("There is no tool named {}.", request.name),
                            );
                            (unknown, Some(linked))
                        }
                    }
                }
                Link::Incompatible { error, .. } | Link::Unreachable(error) => {
                    (Outcome::Error { error }, None)
                }
            }
        };
        let mut result = lower(&outcome, linked.as_deref());
        if let Some(line) = started {
            result.content.push(ContentBlock::text(line));
        }
        Ok(result.into())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        // On an incompatible or unreachable core only `lil_status` is exposed (ARCH-014,
        // AI-108; review M3).
        if !matches!(self.link.link().await, Link::Connected(_)) {
            return Ok(ListResourcesResult::with_all_items(Vec::new()));
        }
        let listed = resources::RESOURCES
            .iter()
            .map(|(uri, name, description)| {
                serde_json::from_value::<Resource>(json!({
                    "uri": uri, "name": name, "description": description,
                    "mimeType": "application/json",
                }))
                .map_err(|e| McpError::internal_error(format!("resource {uri}: {e}"), None))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ListResourcesResult::with_all_items(listed))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let Some(text) = resources::read(&self.link, &request.uri).await else {
            return Err(McpError::resource_not_found(
                format!("no resource {}", request.uri),
                Some(json!({ "uri": request.uri })),
            ));
        };
        let contents = ResourceContents::TextResourceContents {
            uri: request.uri,
            mime_type: Some("application/json".to_string()),
            text,
            meta: None,
        };
        Ok(ReadResourceResult::new(vec![contents]).into())
    }
}

/// Lowers `outcome` for the session it ran on (its tool names and workspace).
fn lower(outcome: &Outcome<ToolOutput>, linked: Option<&Linked>) -> CallToolResult {
    let fallback = [LIL_STATUS.to_string()];
    let names: &[String] = linked.map_or(&fallback, |l| &l.names);
    let workspace = linked.and_then(Linked::workspace);
    let ctx = LowerCtx {
        tool_names: names,
        workspace: workspace.as_deref(),
        roots: linked.map_or(&[], |l| &l.roots),
    };
    call_result(lower_outcome(outcome, &ctx))
}
