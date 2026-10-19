//! The trash tools (ARCH-058, CORE-037), module `workspace`: they work in the session's
//! workspace trash or in the personal resource trash (APP-078).
//!
//! | Tool | Effect | Arguments | `structured` result |
//! |---|---|---|---|
//! | `trash.list` | read | `{scope?}` | `{entries: [restore record]}` |
//! | `trash.restore` | write | `{entry_id, scope?}` | `{path}` |
//!
//! A restore record is the entry's `restore.json` (`entry_id`, `original_path`, `moved_utc`,
//! `purge_after`, `reason`, `sha256`?); `original_path` and the restored `path` are relative
//! to the scope's root with `/` separators: the workspace, or `%APPDATA%\LILITH\resources\`
//! for `personal`. `scope` defaults to `workspace` when the session has one, else `personal`
//! (`workspace.required` for an explicit `workspace` without one); a `trash.restore` without a
//! scope that finds no such entry in the workspace also looks in the personal trash, where
//! `resource.delete` puts a personal resource. A restore never replaces what is at the
//! original path now: that is `trash.restore_conflict`, and an unknown `entry_id` is
//! `trash.unknown`.

use std::sync::Arc;

use lil_protocol::{EffectClass, ErrorBody};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::registry::{CallCtx, Registry, typed_tool};
use super::workspace_tools::WORKSPACE_MODULE;
use crate::store::{Personal, StoreError, TrashRecord};

const LIST_DESCRIPTION: &str = "List the entries in the trash, oldest first: each with its \
    entry_id, the path it came from, when it was moved, when it is purged (30 days later) and \
    why it was moved. scope is workspace (the workspace's trash) or personal (the trash of \
    personal resources); without it the workspace is used when the session has one.";

const RESTORE_DESCRIPTION: &str = "Move a trash entry back to the path it came from. Nothing \
    is ever replaced: when that path is taken now, the call fails with trash.restore_conflict \
    and the entry stays in the trash. Restored files of a sealed version are made read-only \
    again. scope is workspace or personal; without it the workspace's trash is searched first \
    (when the session has one), then the personal trash.";

/// Whose trash a call works in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum TrashScope {
    Workspace,
    Personal,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    scope: Option<TrashScope>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ListResult {
    entries: Vec<TrashRecord>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RestoreArgs {
    /// The entry's id, `te_<16 hex>`, as `trash.list` gives it.
    entry_id: String,
    scope: Option<TrashScope>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct RestoreResult {
    /// The restored path, relative to the scope's root, with `/` separators.
    path: String,
}

/// The scope a call without one works in.
fn scope_of(ctx: &CallCtx, scope: Option<TrashScope>) -> TrashScope {
    scope.unwrap_or(if ctx.workspace.is_some() {
        TrashScope::Workspace
    } else {
        TrashScope::Personal
    })
}

async fn list(
    ctx: &CallCtx,
    personal: &Personal,
    args: ListArgs,
) -> Result<(ListResult, String), ErrorBody> {
    let entries = match scope_of(ctx, args.scope) {
        TrashScope::Workspace => ctx.workspace()?.store.trash_list().await,
        TrashScope::Personal => personal.trash_list().await,
    }
    .map_err(|e: StoreError| e.to_error(&ctx.trace_id))?;
    let summary = format!(
        "{} trash entr{}",
        entries.len(),
        if entries.len() == 1 { "y" } else { "ies" }
    );
    Ok((ListResult { entries }, summary))
}

async fn restore(
    ctx: &CallCtx,
    personal: &Personal,
    args: RestoreArgs,
) -> Result<(RestoreResult, String), ErrorBody> {
    let restored = match scope_of(ctx, args.scope) {
        TrashScope::Workspace => {
            let found = ctx.workspace()?.store.trash_restore(&args.entry_id).await;
            match found {
                // Without a scope, a personal resource's entry is found too.
                Err(StoreError::TrashUnknown(_)) if args.scope.is_none() => {
                    personal.trash_restore(&args.entry_id).await
                }
                found => found,
            }
        }
        TrashScope::Personal => personal.trash_restore(&args.entry_id).await,
    };
    let path = restored.map_err(|e| e.to_error(&ctx.trace_id))?;
    let summary = format!("restored {path}");
    Ok((
        RestoreResult {
            path: path.as_str().to_string(),
        },
        summary,
    ))
}

/// Registers `trash.list` and `trash.restore` (module `workspace`); `personal` is the personal
/// resource trash.
pub fn register_trash_tools(reg: &mut Registry, personal: &Arc<Personal>) {
    let p = personal.clone();
    reg.register(typed_tool(
        "trash.list",
        LIST_DESCRIPTION,
        EffectClass::Read,
        WORKSPACE_MODULE,
        move |ctx, args: ListArgs| {
            let p = p.clone();
            Box::pin(async move { list(ctx, &p, args).await })
        },
    ));
    let p = personal.clone();
    reg.register(typed_tool(
        "trash.restore",
        RESTORE_DESCRIPTION,
        EffectClass::Write,
        WORKSPACE_MODULE,
        move |ctx, args: RestoreArgs| {
            let p = p.clone();
            Box::pin(async move { restore(ctx, &p, args).await })
        },
    ));
}
