//! The trash tools (ARCH-058, CORE-037), module `workspace`: they work in the session's
//! workspace trash or in the personal resource trash (APP-078).
//!
//! | Tool | Effect | Arguments | `structured` result |
//! |---|---|---|---|
//! | `trash.list` | read | `{scope?}` | `{entries: [restore record + scope]}` |
//! | `trash.restore` | write | `{entry_id, scope?}` | `{path}` |
//!
//! A restore record is the entry's `restore.json` (`entry_id`, `original_path`, `moved_utc`,
//! `purge_after`, `reason`, `sha256`?); `original_path` and the restored `path` are relative
//! to the scope's root with `/` separators: the workspace, or `%APPDATA%\LILITH\resources\`
//! for `personal`. Without a `scope`, `trash.list` lists every trash the session can see (the
//! workspace's, then the personal one), each row naming its `scope`, and `trash.restore` tries the
//! workspace trash (when the session has one) and then the personal trash, where
//! `resource.delete` puts a personal resource (`workspace.required` for an explicit `workspace`
//! without one). Restoring a resource file publishes it on the `resources` topic as `saved`
//! with `restored_entry_id`. A restore never replaces what is at the
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
    why it was moved, and which trash it is in (scope: workspace, or personal for personal \
    resources). Pass scope to list one trash; without it every trash is listed.";

const RESTORE_DESCRIPTION: &str = "Move a trash entry back to the path it came from. Nothing \
    is ever replaced: when that path is taken now, the call fails with trash.restore_conflict \
    and the entry stays in the trash. Restored files of a sealed version are made read-only \
    again. scope is workspace or personal; without it the workspace's trash is searched first \
    (when the session has one), then the personal trash.";

/// Whose trash a call works in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum TrashScope {
    Workspace,
    Personal,
}

impl TrashScope {
    fn as_str(self) -> &'static str {
        match self {
            TrashScope::Workspace => "workspace",
            TrashScope::Personal => "personal",
        }
    }
}

/// The topic `resources` publishes its changes on (APP-078); named here, not imported, so that
/// `tools` names no `resources` item.
const RESOURCES_TOPIC: &str = "resources";

/// A trash entry and the trash it is in.
#[derive(Debug, Serialize, JsonSchema)]
struct TrashRow {
    #[serde(flatten)]
    record: TrashRecord,
    scope: TrashScope,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    scope: Option<TrashScope>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ListResult {
    entries: Vec<TrashRow>,
    /// A trash that couldn't be listed (a link in its path, say) when no scope was given and
    /// another could be.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
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
    // Without a scope, every trash the session can see: a `model_profile` delete lands in the
    // personal one even in a workspace session.
    let scopes = match args.scope {
        Some(scope) => vec![scope],
        None if ctx.workspace.is_some() => vec![TrashScope::Workspace, TrashScope::Personal],
        None => vec![TrashScope::Personal],
    };
    let several = scopes.len() > 1;
    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    let mut failed = None;
    for scope in scopes {
        let records = match scope {
            TrashScope::Workspace => ctx.workspace()?.store.trash_list().await,
            TrashScope::Personal => personal.trash_list().await,
        };
        match records {
            Ok(records) => {
                entries.extend(records.into_iter().map(|record| TrashRow { record, scope }));
            }
            // With no scope given, the trash that works is listed and the other is named.
            Err(e @ StoreError::Io { .. }) if several => {
                warnings.push(format!(
                    "the {} trash couldn't be listed: {e}",
                    scope.as_str()
                ));
                failed.get_or_insert(e);
            }
            Err(e) => return Err(e.to_error(&ctx.trace_id)),
        }
    }
    if let Some(e) = failed
        && entries.is_empty()
        && warnings.len() > 1
    {
        return Err(e.to_error(&ctx.trace_id));
    }
    entries.sort_by(|a, b| {
        (&a.record.moved_utc, &a.record.entry_id).cmp(&(&b.record.moved_utc, &b.record.entry_id))
    });
    let summary = format!(
        "{} trash entr{}",
        entries.len(),
        if entries.len() == 1 { "y" } else { "ies" }
    );
    Ok((ListResult { entries, warnings }, summary))
}

async fn restore(
    ctx: &CallCtx,
    personal: &Personal,
    args: RestoreArgs,
) -> Result<(RestoreResult, String), ErrorBody> {
    let (restored, scope) = match scope_of(ctx, args.scope) {
        TrashScope::Workspace => {
            let found = ctx.workspace()?.store.trash_restore(&args.entry_id).await;
            match found {
                // Without a scope, a personal resource's entry is found too.
                Err(StoreError::TrashUnknown(_)) if args.scope.is_none() => (
                    personal.trash_restore(&args.entry_id).await,
                    TrashScope::Personal,
                ),
                found => (found, TrashScope::Workspace),
            }
        }
        TrashScope::Personal => (
            personal.trash_restore(&args.entry_id).await,
            TrashScope::Personal,
        ),
    };
    let path = restored.map_err(|e| e.to_error(&ctx.trace_id))?;
    if let Some((kind, id)) = resource_of(scope, path.as_str()) {
        // The Libraries panel hears a restored resource as a save (APP-078).
        ctx.events.publish(
            RESOURCES_TOPIC,
            serde_json::json!({
                "kind": "saved",
                "resource_kind": kind,
                "id": id,
                "scope": scope.as_str(),
                "restored_entry_id": args.entry_id,
            }),
        );
    }
    let summary = format!("restored {path}");
    Ok((
        RestoreResult {
            path: path.as_str().to_string(),
        },
        summary,
    ))
}

/// The kind and id of the resource file at `path` in `scope`'s root (`<kind>/<id>.json`, under
/// `library/resources/` in a workspace), if it is one.
fn resource_of(scope: TrashScope, path: &str) -> Option<(&str, &str)> {
    let tail = match scope {
        TrashScope::Workspace => path.strip_prefix("library/resources/")?,
        TrashScope::Personal => path,
    };
    let (kind, file) = tail.split_once('/')?;
    let id = file.strip_suffix(".json")?;
    (!kind.is_empty() && !id.is_empty() && !id.contains('/')).then_some((kind, id))
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
