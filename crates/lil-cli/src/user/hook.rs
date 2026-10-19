//! `lil hook <session-start | pre-tool-use | post-tool-use>` (spec ARCH-018, ARCH-103–107,
//! AI-116; Appendix F "Plugin 1.3" and "The `lil` commands this plan adds"): the plugin's
//! hooks. It reads the hook JSON on stdin, prints the hook output JSON or nothing, and always
//! exits 0. It never answers `allow` (ruling P3-R28): only `ask`, `deny` or no decision, so a
//! command it doesn't flag is left to Claude Code's own permission mode. A malformed input
//! (not a JSON object, or missing the fields its tool needs) asks on PreToolUse and prints
//! nothing for the other events.
//!
//! - **PreToolUse:** [`classify_tool`] decides. Only for the comfy MCP's tools that are denied
//!   while a core runs does it look whether one runs (the pipe, as `lil status --no-start`
//!   does); nothing is started.
//! - **PostToolUse:** for a Write, Edit or MultiEdit of a file inside the default workspace
//!   (in `suite.toml`), `lil notify --no-start <file_path>`: `watch.notify` on a core that
//!   already runs, nothing otherwise.
//! - **SessionStart:** while a core runs, `additionalContext` with the active project, the open
//!   jobs (each `job_id`) and the pending confirmations (each `id`) of the workspace the `cli`
//!   chain finds from the session's folder; nothing otherwise.
//! - With `LIL_ENGINE=1` (Engine A) every hook prints nothing (ARCH-107).
//!
//! The hooks never start a core and never wait for a person (ARCH-018). PreToolUse fails
//! closed (ruling P3-R28): a hook that panics, can't read its input, or can't tell whether a
//! core runs for a core-dependent tool answers `ask`. The core check probes the pipe of this
//! `lil.exe`'s channel, so `lil_path` should name the channel that drives ComfyUI.

use std::io::Read as _;
use std::path::PathBuf;

use lil_client::blocking::BlockingClient;
use lil_client::doctor::{SuiteTomlState, core_running, inspect_suite_toml};
use lil_client::{
    ClientConfig, ClientError, StartPolicy, WorkspaceOrigin, WorkspaceSource, resolve_workspace,
};
use lil_platform::Env;
use lil_platform::paths::relative_inside;
use lil_protocol::app::ListWhich;
use lil_protocol::{CallerKind, NO_WORKSPACE, Outcome};
use serde_json::{Map, Value, json};

use super::GlobalArgs;
use super::hook_patterns::{Decision, classify_tool, depends_on_core, unknown_core};

/// The hook event, as `hooks.json` passes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum HookEvent {
    SessionStart,
    PreToolUse,
    PostToolUse,
}

/// The Engine A marker (ARCH-107).
const ENGINE_MARKER: &str = "LIL_ENGINE";

/// At most this many open jobs and pending confirmations are named in the session context
/// (Claude Code caps hook strings at 10,000 characters).
const LISTED: usize = 20;

/// `lil hook <event>`: reads stdin and prints the output.
///
/// PreToolUse fails closed (ruling P3-R28): when stdin can't be read or the hook panics, it
/// answers `ask`.
pub(super) fn run(event: HookEvent, global: &GlobalArgs) -> i32 {
    let mut stdin = String::new();
    let stdout = if std::io::stdin().read_to_string(&mut stdin).is_err() {
        failed(event)
    } else {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_hook(event, &stdin, global).0
        }))
        .unwrap_or_else(|_| failed(event))
    };
    if !stdout.is_empty() {
        let _ = super::print_line(&stdout);
    }
    0
}

/// What a hook that failed prints: `ask` for PreToolUse, nothing for the others.
fn failed(event: HookEvent) -> String {
    if event != HookEvent::PreToolUse {
        return String::new();
    }
    ask_output(
        "LILITH: the suite's hook failed, so it can't check this tool call. Allow it?".to_string(),
    )
    .to_string()
}

/// What a hook given malformed input prints: `ask` for PreToolUse, nothing for the others.
fn malformed(event: HookEvent) -> String {
    if event != HookEvent::PreToolUse {
        return String::new();
    }
    ask_output(MALFORMED.to_string()).to_string()
}

const MALFORMED: &str = "LILITH: the hook input isn't the JSON Claude Code sends, so the suite's \
    hook can't check this tool call. Allow it?";

/// A PreToolUse `ask` with `reason`.
fn ask_output(reason: String) -> Value {
    json!({"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "ask",
        "permissionDecisionReason": reason,
    }})
}

/// What `lil hook <event>` prints for the hook JSON `stdin`, and its exit code (always 0).
pub fn run_hook(event: HookEvent, stdin: &str, global: &GlobalArgs) -> (String, i32) {
    if std::env::var(ENGINE_MARKER).is_ok_and(|v| v == "1") {
        return (String::new(), 0);
    }
    let input = match serde_json::from_str::<Value>(stdin) {
        Ok(input) if input.is_object() => input,
        _ => return (malformed(event), 0),
    };
    let output = match event {
        HookEvent::PreToolUse => pre_tool_use(&input),
        HookEvent::PostToolUse => {
            post_tool_use(&input);
            None
        }
        HookEvent::SessionStart => session_start(&input, global),
    };
    (output.map(|v| v.to_string()).unwrap_or_default(), 0)
}

fn pre_tool_use(input: &Value) -> Option<Value> {
    let Some(tool_name) = input["tool_name"].as_str() else {
        return Some(ask_output(MALFORMED.to_string()));
    };
    let empty = json!({});
    let tool_input = input.get("tool_input").unwrap_or(&empty);
    let running = if depends_on_core(tool_name) {
        match Env::from_process() {
            Ok(env) => core_running(&env),
            // It can't tell whether a core runs: ask (fail closed, ruling P3-R28).
            Err(_) => return pre_output(unknown_core(tool_name)),
        }
    } else {
        false
    };
    pre_output(classify_tool(tool_name, tool_input, running))
}

/// The PreToolUse output for `decision`.
fn pre_output(decision: Decision) -> Option<Value> {
    let (decision, reason) = match decision {
        Decision::Ask(reason) => ("ask", reason),
        Decision::Deny(reason) => ("deny", reason),
        Decision::Warn(message) => return Some(json!({ "systemMessage": message })),
        Decision::None => return None,
    };
    Some(json!({"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": decision,
        "permissionDecisionReason": reason,
    }}))
}

/// The session's folder: the input's `cwd`, else this process's.
fn session_dir(input: &Value) -> Option<PathBuf> {
    input["cwd"]
        .as_str()
        .filter(|c| !c.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
}

/// A `cli` configuration that never starts a core and never waits for a person.
fn hook_config(env: Env) -> Option<ClientConfig> {
    let mut cfg = ClientConfig::for_env(env, CallerKind::Cli).ok()?;
    cfg.start = StartPolicy::NoStart;
    cfg.non_interactive = true;
    Some(cfg)
}

fn post_tool_use(input: &Value) {
    let Some(file) = input["tool_input"]["file_path"].as_str() else {
        return;
    };
    let Ok(env) = Env::from_process() else {
        return;
    };
    let SuiteTomlState::Ok { default, id } = inspect_suite_toml(&env) else {
        return;
    };
    let mut path = PathBuf::from(file);
    if path.is_relative() {
        let Some(dir) = session_dir(input) else {
            return;
        };
        path = dir.join(path);
    }
    let Some(rel) = relative_inside(&default, &path) else {
        return;
    };
    let Some(mut cfg) = hook_config(env) else {
        return;
    };
    cfg.workspace = id;
    cfg.workspace_origin = Some(WorkspaceOrigin {
        path: default,
        source: WorkspaceSource::Default,
    });
    let Ok(client) = BlockingClient::connect(&cfg) else {
        return;
    };
    let mut args = Map::new();
    args.insert("paths".into(), json!([rel]));
    let _ = client.call_tool("watch.notify", args, None);
}

fn session_start(input: &Value, global: &GlobalArgs) -> Option<Value> {
    let env = Env::from_process().ok()?;
    let dir = session_dir(input)?;
    let mut cfg = hook_config(env)?;
    if let Ok(ws) = resolve_workspace(&cfg.env, global.workspace.as_deref(), &dir) {
        cfg.set_workspace(ws);
    }
    let client = match BlockingClient::connect(&cfg) {
        Ok(client) => client,
        Err(ClientError::NotRunning) => return None,
        Err(_) => {
            // A workspace the core can't serve: report the rest without it.
            cfg.workspace = NO_WORKSPACE.to_string();
            cfg.workspace_origin = None;
            BlockingClient::connect(&cfg).ok()?
        }
    };
    let context = session_context(&client);
    Some(json!({"hookSpecificOutput": {
        "hookEventName": "SessionStart",
        "additionalContext": context,
    }}))
}

/// The structured result of a read tool, if it succeeded.
fn read(client: &BlockingClient, tool: &str, args: Value) -> Option<Value> {
    let Value::Object(args) = args else {
        return None;
    };
    match client.call_tool(tool, args, None) {
        Ok(Outcome::Ok(output)) => Some(output.structured),
        _ => None,
    }
}

/// `items` joined, at most [`LISTED`] of them, then how many more.
fn listed(items: &[String]) -> String {
    let mut text = items
        .iter()
        .take(LISTED)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    if items.len() > LISTED {
        text.push_str(&format!("; and {} more", items.len() - LISTED));
    }
    text
}

fn session_context(client: &BlockingClient) -> String {
    let hello = client.hello();
    let mut lines = vec![match &hello.workspace {
        Some(ws) => format!(
            "LILITH: a LILITH core is running for the workspace {}. While it runs, every \
             state-changing step goes through the suite's lil MCP tools; read files directly, \
             but don't write workspace files yourself (check with lil status --no-start).",
            ws.path
        ),
        None => "LILITH: a LILITH core is running (no workspace for this folder). While it \
                 runs, every state-changing step goes through the suite's lil MCP tools."
            .to_string(),
    }];
    if hello.workspace.is_some() {
        let project = read(client, "project.context", json!({}))
            .and_then(|s| s["project"].as_str().map(str::to_string));
        lines.push(match project {
            Some(p) => format!("Active project: {p}."),
            None => "Active project: none.".to_string(),
        });
    }
    let mut jobs = Vec::new();
    for status in ["queued", "running"] {
        if let Some(list) = read(client, "jobs.list", json!({ "status": status })) {
            for job in list["jobs"].as_array().into_iter().flatten() {
                jobs.push(format!(
                    "{} ({}, {}/{})",
                    job["job_id"].as_str().unwrap_or("?"),
                    job["kind"].as_str().unwrap_or("?"),
                    job["status"].as_str().unwrap_or("?"),
                    job["substatus"].as_str().unwrap_or("?"),
                ));
            }
        }
    }
    lines.push(if jobs.is_empty() {
        "Open jobs: none.".to_string()
    } else {
        format!(
            "Open jobs ({}): {}. Wait on a job with job_wait until done is true.",
            jobs.len(),
            listed(&jobs)
        )
    });
    let pending: Vec<String> = match client.confirmations(ListWhich::Pending) {
        Ok(Outcome::Ok(list)) => list
            .confirmations
            .iter()
            .map(|c| format!("{} ({})", c.id, c.tool))
            .collect(),
        _ => Vec::new(),
    };
    lines.push(if pending.is_empty() {
        "Pending confirmations: none.".to_string()
    } else {
        format!(
            "Pending confirmations ({}): {}. Each waits for the person to answer in LILITH; \
             continue with other steps, then call the tool again with the same arguments and \
             retry_token.",
            pending.len(),
            listed(&pending)
        )
    });
    lines.join("\n")
}
