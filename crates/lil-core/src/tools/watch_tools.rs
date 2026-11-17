//! The watcher's tool (ARCH-016, ARCH-073), module `workspace`: `lil notify` calls it.
//!
//! | Tool | Effect | Arguments | `structured` result |
//! |---|---|---|---|
//! | `watch.notify` | write | `{paths: [string], by?: Actor}` | `{reconciled: [string], ignored: [string]}` |
//!
//! Each path is workspace-relative (`/` or `\` separated) or absolute inside the session's
//! workspace; the index reconciles them (a folder with its whole subtree) and the watcher
//! decides what changed, as it does for the changes it sees itself. A path outside the
//! workspace, one that isn't a valid workspace path, and one the watcher doesn't watch
//! (`.lilith/` and the legacy workspace data folder, `.trash/`, `.git/`) is `ignored`. While
//! git works the paths wait for the post-git rescan. It works where the workspace has no folder watch too: that is what it is
//! for.
//!
//! **Who made the changes** (CORE-034, ARCH-103; P4b-R16, owner decision OD-15). The tool
//! computes the reporter's [`Actor`] and passes it to the watcher (`watch` never names
//! `pipeline`), which writes it into each `human_edits[]` entry the call records, and into the
//! latest entry of a file given when the watcher recorded it already, without an actor: `by`
//! when given, else the caller's ([`actor_of`]). A change reported with an agent's `by`
//! (`claude_code`, `tether`, `director`), or by an agent caller (`mcp`, `assistant`) without
//! one, is an agent edit; from an app (`ui:*`) or `cli` without `by` it stays a human edit.
//! `by` is asserted by the caller, so it is checked before anything is reconciled, and a
//! refused call records nothing (`params.invalid`): `by.kind` is never `owner` (a change of
//! yours needs no `by`), and an agent caller's `by` has its own kind (Claude Code can't report
//! as the assistant). Known limit: `lil` run from a shell an agent opened connects as `cli`
//! (ARCH-047), so its report without `by` is recorded as yours; the plugin's hook passes `by`
//! for Claude Code's own writes.

use lil_protocol::codes::PARAMS_INVALID;
use lil_protocol::{CallerKind, EffectClass, ErrorBody};
use lil_schema::{Actor, ActorKind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::registry::{CallCtx, Registry, typed_tool};
use super::workspace_tools::WORKSPACE_MODULE;
use crate::pipeline::actor_of;
use crate::store::StoreError;
use crate::watch::notify_paths;

const NOTIFY_DESCRIPTION: &str = "Tell the workspace watcher that files changed: each path \
    (workspace-relative, or absolute inside the workspace; a folder covers its whole subtree) \
    is reconciled with the index, and an outside edit of a candidate or sealed file is \
    recorded as a human edit, never overwritten. Paths outside the workspace or in its private \
    folders are returned as ignored. Use it where the folder can't be watched, or after a \
    tool changed files the watcher may have missed. `by` says who made the changes: an agent \
    gives its own kind (and its session_id); without it, the caller is recorded.";

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NotifyArgs {
    /// Workspace-relative paths, or absolute paths inside the workspace.
    paths: Vec<String>,
    /// Who made the changes (never `owner`; an agent caller gives its own kind). Without it,
    /// the caller.
    by: Option<ActorArg>,
}

/// `Actor.kind` (P4b-R16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ActorKindArg {
    Owner,
    Tether,
    Director,
    ClaudeCode,
    Cli,
    Core,
}

/// `Actor`: `{kind, task_id?, conversation_id?, session_id?}`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ActorArg {
    kind: ActorKindArg,
    task_id: Option<String>,
    conversation_id: Option<String>,
    session_id: Option<String>,
}

impl ActorArg {
    fn into_actor(self) -> Actor {
        let kind = match self.kind {
            ActorKindArg::Owner => ActorKind::Owner,
            ActorKindArg::Tether => ActorKind::Tether,
            ActorKindArg::Director => ActorKind::Director,
            ActorKindArg::ClaudeCode => ActorKind::ClaudeCode,
            ActorKindArg::Cli => ActorKind::Cli,
            ActorKindArg::Core => ActorKind::Core,
        };
        Actor {
            kind,
            task_id: self.task_id,
            conversation_id: self.conversation_id,
            session_id: self.session_id,
        }
    }
}

/// The actor of a `watch.notify` by `caller` with `by` (module docs). The error is the
/// `params.invalid` message.
fn reporter(caller: &CallerKind, by: Option<ActorArg>) -> Result<Actor, String> {
    let own = actor_of(caller);
    let Some(by) = by.map(ActorArg::into_actor) else {
        return Ok(own);
    };
    if by.kind == ActorKind::Owner {
        return Err(
            "by.kind is never owner: a change of yours, from an app or lil, needs no by"
                .to_string(),
        );
    }
    let agent = matches!(caller, CallerKind::Mcp | CallerKind::Assistant);
    if agent && by.kind != own.kind {
        let own = own.kind.as_str();
        return Err(format!(
            "this caller's changes are {own}'s, so by.kind must be {own} (or leave by out)"
        ));
    }
    Ok(by)
}

#[derive(Debug, Serialize, JsonSchema)]
struct NotifyResult {
    /// The workspace-relative paths reconciled (or waiting for git to finish).
    reconciled: Vec<String>,
    /// The paths given that were left alone.
    ignored: Vec<String>,
}

async fn notify(ctx: &CallCtx, args: NotifyArgs) -> Result<(NotifyResult, String), ErrorBody> {
    let actor = reporter(&ctx.session.caller, args.by)
        .map_err(|why| ErrorBody::new(&PARAMS_INVALID, why, ctx.trace_id.clone()))?;
    let open = ctx.workspace()?;
    let Some(watch) = &open.watch else {
        // Only the core that holds `core.lock` watches (the dispatcher refuses write tools
        // on a read-only workspace before this runs).
        return Err(StoreError::ReadOnly.to_error(&ctx.trace_id));
    };
    let (paths, mut ignored) = notify_paths(&open.ws.path, &args.paths);
    let report = watch.notify(paths, Some(actor)).await;
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
