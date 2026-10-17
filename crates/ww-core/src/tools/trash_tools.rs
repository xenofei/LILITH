//! The trash tools (ARCH-058, CORE-037), module `workspace`: they work in the session's
//! workspace (`workspace.required` without one).
//!
//! | Tool | Effect | Arguments | `structured` result |
//! |---|---|---|---|
//! | `trash.list` | read | `{}` | `{entries: [restore record]}` |
//! | `trash.restore` | write | `{entry_id}` | `{path}` |
//!
//! A restore record is the entry's `restore.json` (`entry_id`, `original_path`, `moved_utc`,
//! `purge_after`, `reason`, `sha256`?); `original_path` and the restored `path` are
//! workspace-relative with `/` separators. A restore never replaces what is at the original
//! path now: that is `trash.restore_conflict`, and an unknown `entry_id` is `trash.unknown`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ww_protocol::{EffectClass, ErrorBody};

use super::registry::{CallCtx, Registry, typed_tool};
use super::workspace_tools::WORKSPACE_MODULE;
use crate::store::{StoreError, TrashRecord};

const LIST_DESCRIPTION: &str = "List the entries in the workspace trash, oldest first: each \
    with its entry_id, the workspace-relative path it came from, when it was moved, when it \
    is purged (30 days later) and why it was moved.";

const RESTORE_DESCRIPTION: &str = "Move a trash entry back to the workspace-relative path it \
    came from. Nothing is ever replaced: when that path is taken now, the call fails with \
    trash.restore_conflict and the entry stays in the trash. Restored files of a sealed \
    version are made read-only again.";

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListArgs {}

#[derive(Debug, Serialize, JsonSchema)]
struct ListResult {
    entries: Vec<TrashRecord>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RestoreArgs {
    /// The entry's id, `te_<16 hex>`, as `trash.list` gives it.
    entry_id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct RestoreResult {
    /// The restored path, workspace-relative with `/` separators.
    path: String,
}

async fn list(ctx: &CallCtx) -> Result<(ListResult, String), ErrorBody> {
    let store = &ctx.workspace()?.store;
    let entries = store
        .trash_list()
        .await
        .map_err(|e: StoreError| e.to_error(&ctx.trace_id))?;
    let summary = format!(
        "{} trash entr{}",
        entries.len(),
        if entries.len() == 1 { "y" } else { "ies" }
    );
    Ok((ListResult { entries }, summary))
}

async fn restore(ctx: &CallCtx, args: RestoreArgs) -> Result<(RestoreResult, String), ErrorBody> {
    let store = &ctx.workspace()?.store;
    let path = store
        .trash_restore(&args.entry_id)
        .await
        .map_err(|e| e.to_error(&ctx.trace_id))?;
    let summary = format!("restored {path}");
    Ok((
        RestoreResult {
            path: path.as_str().to_string(),
        },
        summary,
    ))
}

/// Registers `trash.list` and `trash.restore` (module `workspace`).
pub fn register_trash_tools(reg: &mut Registry) {
    reg.register(typed_tool(
        "trash.list",
        LIST_DESCRIPTION,
        EffectClass::Read,
        WORKSPACE_MODULE,
        |ctx, _args: ListArgs| Box::pin(list(ctx)),
    ));
    reg.register(typed_tool(
        "trash.restore",
        RESTORE_DESCRIPTION,
        EffectClass::Write,
        WORKSPACE_MODULE,
        |ctx, args: RestoreArgs| Box::pin(restore(ctx, args)),
    ));
}
