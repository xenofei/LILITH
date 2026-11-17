//! The watcher's tool (ARCH-016, ARCH-073), module `workspace`: `lil notify` calls it.
//!
//! | Tool | Effect | Arguments | `structured` result |
//! |---|---|---|---|
//! | `watch.notify` | write | `{paths: [string]}` | `{reconciled: [string], ignored: [string]}` |
//!
//! Each path is workspace-relative (`/` or `\` separated) or absolute inside the session's
//! workspace; the index reconciles them (a folder with its whole subtree) and the watcher
//! decides what changed, as it does for the changes it sees itself. A path outside the
//! workspace, one that isn't a valid workspace path, and one the watcher doesn't watch
//! (`.lilith/` and the legacy workspace data folder, `.trash/`, `.git/`) is `ignored`. While
//! git works the paths wait for the post-git rescan. It works where the workspace has no folder watch too: that is what it is
//! for.

use lil_protocol::{EffectClass, ErrorBody};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::registry::{CallCtx, Registry, typed_tool};
use super::workspace_tools::WORKSPACE_MODULE;
use crate::store::StoreError;
use crate::watch::notify_paths;

const NOTIFY_DESCRIPTION: &str = "Tell the workspace watcher that files changed: each path \
    (workspace-relative, or absolute inside the workspace; a folder covers its whole subtree) \
    is reconciled with the index, and an outside edit of a candidate or sealed file is \
    recorded as a human edit, never overwritten. Paths outside the workspace or in its private \
    folders are returned as ignored. Use it where the folder can't be watched, or after a \
    tool changed files the watcher may have missed.";

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NotifyArgs {
    /// Workspace-relative paths, or absolute paths inside the workspace.
    paths: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct NotifyResult {
    /// The workspace-relative paths reconciled (or waiting for git to finish).
    reconciled: Vec<String>,
    /// The paths given that were left alone.
    ignored: Vec<String>,
}

async fn notify(ctx: &CallCtx, args: NotifyArgs) -> Result<(NotifyResult, String), ErrorBody> {
    let open = ctx.workspace()?;
    let Some(watch) = &open.watch else {
        // Only the core that holds `core.lock` watches (the dispatcher refuses write tools
        // on a read-only workspace before this runs).
        return Err(StoreError::ReadOnly.to_error(&ctx.trace_id));
    };
    let (paths, mut ignored) = notify_paths(&open.ws.path, &args.paths);
    let report = watch.notify(paths).await;
    ignored.extend(report.ignored);
    let summary = format!(
        "{} path(s) reconciled, {} ignored",
        report.reconciled.len(),
        ignored.len()
    );
    Ok((
        NotifyResult {
            reconciled: report.reconciled,
            ignored,
        },
        summary,
    ))
}

/// Registers `watch.notify` on `reg`.
pub fn register_watch_tools(reg: &mut Registry) {
    reg.register(typed_tool(
        "watch.notify",
        NOTIFY_DESCRIPTION,
        EffectClass::Write,
        WORKSPACE_MODULE,
        |ctx, args: NotifyArgs| Box::pin(notify(ctx, args)),
    ));
}
