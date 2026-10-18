//! `lil workspace init <path> [--name <n>] [--default]` and `lil workspace use <path>` (owner
//! decision 1): thin wrappers over the core tools `workspace.create` and
//! `workspace.set_default`, so the core stays the only writer of `suite.toml`.
//!
//! Both connect with workspace `"none"` and never resolve the `cli` chain, so a stale
//! `default_workspace` or a broken `suite.toml` never blocks the command that repairs it. The
//! path is made absolute here, against `lil`'s own current folder, before the core sees it.
//!
//! Output (Appendix E, "The `lil` commands this plan adds"): `workspace created at <path>` or
//! `workspace registered at <path>`, then `default workspace: <path>` when it is the default;
//! `lil workspace use` prints `default workspace: <path>`. Warnings go to stderr. An error
//! result prints its message on stderr and exits 1.

use std::path::Path;

use lil_client::blocking::BlockingClient;
use lil_client::{ClientError, StartPolicy};
use lil_protocol::Outcome;
use serde_json::{Map, Value, json};

use super::{
    EXIT_ERROR_RESULT, EXIT_OK, EXIT_PENDING, Exit, GlobalArgs, cannot_run, config, print_line,
};

/// `lil workspace init`.
pub(super) fn init(
    global: &GlobalArgs,
    path: &Path,
    name: Option<&str>,
    default: bool,
) -> Result<i32, Exit> {
    let mut args = Map::new();
    args.insert("path".into(), Value::from(absolute(path)?));
    if let Some(name) = name {
        args.insert("name".into(), Value::from(name));
    }
    if default {
        args.insert("default".into(), Value::Bool(true));
    }
    let structured = match call(global, "workspace.create", args)? {
        Called::Ok(structured) => structured,
        Called::Exit(code) => return Ok(code),
    };
    let path = text(&structured, "path");
    for warning in structured["warnings"].as_array().into_iter().flatten() {
        if let Some(warning) = warning.as_str() {
            eprintln!("warning: {warning}");
        }
    }
    let said = if structured["created"] == json!(true) {
        "created"
    } else {
        "registered"
    };
    let _ = print_line(&format!("workspace {said} at {path}"));
    if structured["default"] == json!(true) {
        let _ = print_line(&format!("default workspace: {path}"));
    }
    Ok(EXIT_OK)
}

/// `lil workspace use`.
pub(super) fn use_default(global: &GlobalArgs, path: &Path) -> Result<i32, Exit> {
    let mut args = Map::new();
    args.insert("path".into(), Value::from(absolute(path)?));
    let structured = match call(global, "workspace.set_default", args)? {
        Called::Ok(structured) => structured,
        Called::Exit(code) => return Ok(code),
    };
    let _ = print_line(&format!("default workspace: {}", text(&structured, "path")));
    Ok(EXIT_OK)
}

/// What a workspace tool call came to.
enum Called {
    /// The `structured` result.
    Ok(Value),
    /// `lil` exits with this code; anything to say is said.
    Exit(i32),
}

/// Calls `tool` with workspace `"none"`. No core with `--no-start` is a silent exit 0.
fn call(global: &GlobalArgs, tool: &str, args: Map<String, Value>) -> Result<Called, Exit> {
    let cfg = config(global, StartPolicy::StartIfNeeded)?;
    let client = match BlockingClient::connect(&cfg) {
        Ok(client) => client,
        Err(ClientError::NotRunning) => return Ok(Called::Exit(EXIT_OK)),
        Err(e) => return Err(cannot_run(e)),
    };
    Ok(
        match client.call_tool(tool, args, None).map_err(cannot_run)? {
            Outcome::Ok(output) => Called::Ok(output.structured),
            Outcome::Error { error } => {
                eprintln!("{}", error.message);
                Called::Exit(EXIT_ERROR_RESULT)
            }
            Outcome::PendingConfirmation(_) => {
                eprintln!("{tool} is waiting for a confirmation in the LILITH app.");
                Called::Exit(EXIT_PENDING)
            }
        },
    )
}

/// `path` made absolute against the current folder, as text.
fn absolute(path: &Path) -> Result<String, Exit> {
    let abs = std::path::absolute(path)
        .map_err(|e| cannot_run(format!("Cannot use the path {}: {e}", path.display())))?;
    abs.to_str().map(str::to_string).ok_or_else(|| {
        cannot_run(format!(
            "The path {} is not valid Unicode, so it can't be a workspace.",
            abs.display()
        ))
    })
}

/// The string `key` of `structured`.
fn text<'a>(structured: &'a Value, key: &str) -> &'a str {
    structured[key].as_str().unwrap_or_default()
}
