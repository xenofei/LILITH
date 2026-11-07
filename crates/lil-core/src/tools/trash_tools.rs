//! The trash tools (ARCH-058, CORE-037), module `workspace`: they work in the session's
//! workspace trash or in the personal resource trash (APP-078).
//!
//! | Tool | Effect | Arguments | `structured` result |
//! |---|---|---|---|
//! | `trash.list` | read | `{scope?}` | `{entries: [restore record + scope]}` |
//! | `trash.restore` | write | `{entry_id, scope?, to?}` | `{path}` |
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
//!
//! **Restore as…** (`to`; CORE-037, P4b-R39; Appendix H "Trash an asset"): the entry is
//! restored under the name `to` in the folder it came from, never over anything
//! (`trash.restore_conflict` for a taken name). What `to` is depends on the entry:
//! - a whole asset folder (`projects/<p>/assets/<a>`, as `asset.trash` leaves it): `to` is a
//!   free asset id (`params.invalid` otherwise). Holding the version lock of `<p>/<to>`, the
//!   folder moves to `projects/<p>/assets/<to>/`, read-only re-applied where hashes match, and
//!   `projects::repair::restored_as` appends the redirect `<p>/<a>` → `<p>/<to>` (`by` the
//!   caller, as `asset.accept_move` writes one; no `actor` until attribution), renames the
//!   manifest to `<to>.manifest.json` and sets its `asset_id` to `to`. A crash after the move
//!   leaves an asset the index lists as `moved`, which `asset.accept_move` finishes without a
//!   second redirect. `{kind: "moved", from, to}` is published on the asset's topic at its new
//!   name and on `project:<p>`;
//! - a resource file (`<kind>/<id>.json`, under `library/resources/` in a workspace): `to` is
//!   the new resource id; the file comes back as `<kind>/<to>.json` with `to` as its `id`
//!   inside, a targeted edit by the `ResourceIds` service `server` wires (`state.unavailable`
//!   while it isn't);
//! - a path inside an asset folder (a run, a version folder, a file): `to` is `params.invalid`,
//!   as the manifest names it by its own name;
//! - anything else: `to` is the new file or folder name.
//!
//! A `to` that is the entry's own name (its asset id, resource id or file name) is a plain
//! restore.

use std::sync::Arc;

use lil_protocol::codes::{PARAMS_INVALID, STATE_UNAVAILABLE};
use lil_protocol::{EffectClass, ErrorBody};
use lil_schema::is_asset_id;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::registry::{CallCtx, Registry, typed_tool};
use super::services::{AssetServices, ResourceIds};
use super::workspace_tools::WORKSPACE_MODULE;
use crate::pipeline::{asset_topic, status_by};
use crate::projects::repair::restored_as;
use crate::store::{Personal, RestoreRewrite, Store, StoreError, TrashRecord, WsPath};

const LIST_DESCRIPTION: &str = "List the entries in the trash, oldest first: each with its \
    entry_id, the path it came from, when it was moved, when it is purged (30 days later) and \
    why it was moved, and which trash it is in (scope: workspace, or personal for personal \
    resources). Pass scope to list one trash; without it every trash is listed.";

const RESTORE_DESCRIPTION: &str = "Move a trash entry back to the path it came from. Nothing \
    is ever replaced: when that path is taken now, the call fails with trash.restore_conflict \
    and the entry stays in the trash. to restores it under another name in the same folder \
    instead: for a whole asset a free asset id (its manifest takes that id and a redirect from \
    the old id keeps old refs working), for a resource a free resource id (written inside the \
    file too), otherwise a free file name. Restored files of a sealed version are made \
    read-only again. scope is workspace or personal; without it the workspace's trash is \
    searched first (when the session has one), then the personal trash.";

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
    /// Restore under this name in the folder the entry came from (module docs): a free asset
    /// id for a whole asset, a free resource id for a resource, else a free file name.
    to: Option<String>,
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
    services: &AssetServices,
    args: RestoreArgs,
) -> Result<(RestoreResult, String), ErrorBody> {
    let (restored, scope) = match scope_of(ctx, args.scope) {
        TrashScope::Workspace => {
            let found = restore_workspace(ctx, services, &args).await;
            match found {
                // Without a scope, a personal resource's entry is found too.
                Err(Refused::Store(StoreError::TrashUnknown(_))) if args.scope.is_none() => (
                    restore_personal(ctx, personal, services, &args).await,
                    TrashScope::Personal,
                ),
                found => (found, TrashScope::Workspace),
            }
        }
        TrashScope::Personal => (
            restore_personal(ctx, personal, services, &args).await,
            TrashScope::Personal,
        ),
    };
    let path = restored.map_err(|e| match e {
        Refused::Store(e) => e.to_error(&ctx.trace_id),
        Refused::Body(mut e) => {
            e.trace_id.clone_from(&ctx.trace_id);
            *e
        }
    })?;
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

/// Why a restore was refused: the store's error, or one the tool made.
enum Refused {
    Store(StoreError),
    Body(Box<ErrorBody>),
}

impl From<StoreError> for Refused {
    fn from(e: StoreError) -> Refused {
        Refused::Store(e)
    }
}

/// What a trash entry's original path is, for a restore with `to` (module docs).
enum EntryKind {
    /// `projects/<p>/assets/<a>`: `(project, asset_id)`.
    Asset(String, String),
    /// A path inside an asset folder (a run, a version, a file): its manifest names it, so it
    /// is never restored under another name.
    InsideAsset,
    /// A resource file.
    Resource,
    Other,
}

fn entry_kind(scope: TrashScope, original: &str) -> EntryKind {
    let parts: Vec<&str> = original.split('/').collect();
    let in_assets = scope == TrashScope::Workspace
        && parts.len() >= 4
        && parts[0].eq_ignore_ascii_case("projects")
        && parts[2].eq_ignore_ascii_case("assets");
    if in_assets && parts.len() == 4 {
        return EntryKind::Asset(parts[1].to_string(), parts[3].to_string());
    }
    if in_assets {
        return EntryKind::InsideAsset;
    }
    if resource_of(scope, original).is_some() {
        return EntryKind::Resource;
    }
    EntryKind::Other
}

/// The name `original` has now, the one a `to` replaces (a resource's: its id).
fn current_name(kind: &EntryKind, original: &str) -> String {
    let name = original.rsplit('/').next().unwrap_or(original);
    match kind {
        EntryKind::Resource => name.strip_suffix(".json").unwrap_or(name).to_string(),
        _ => name.to_string(),
    }
}

fn inside_asset(original: &str) -> Refused {
    params_invalid(format!(
        "{original} is inside an asset folder, whose manifest names it, so it is restored only          under its own name: leave out to"
    ))
}

fn params_invalid(why: String) -> Refused {
    Refused::Body(Box::new(ErrorBody::new(&PARAMS_INVALID, why, "")))
}

/// The rewrite of a restored resource file to the id `to`, through the wired service.
fn resource_rewrite(services: &AssetServices, to: &str) -> Result<RestoreRewrite, Refused> {
    let Some(ids) = services.resource_ids.get().cloned() else {
        return Err(Refused::Body(Box::new(ErrorBody::new(
            &STATE_UNAVAILABLE,
            "A resource can't be restored under a new id yet (the core is still starting); \
             retry in a moment.",
            "",
        ))));
    };
    let to = to.to_string();
    Ok(Arc::new(move |bytes: &[u8]| {
        let ids: &dyn ResourceIds = ids.as_ref();
        ids.with_id(bytes, &to)
    }))
}

async fn restore_workspace(
    ctx: &CallCtx,
    services: &AssetServices,
    args: &RestoreArgs,
) -> Result<WsPath, Refused> {
    let open = ctx
        .workspace()
        .map_err(|e| Refused::Body(Box::new(e)))?
        .clone();
    let store: &Store = &open.store;
    let Some(to) = args.to.as_deref() else {
        return Ok(store.trash_restore(&args.entry_id).await?);
    };
    let record = store.trash_entry(&args.entry_id).await?;
    let kind = entry_kind(TrashScope::Workspace, &record.original_path);
    // `to` naming the entry's own name is a plain restore (no redirect to itself).
    if to == current_name(&kind, &record.original_path) {
        return Ok(store.trash_restore(&args.entry_id).await?);
    }
    match kind {
        EntryKind::InsideAsset => Err(inside_asset(&record.original_path)),
        EntryKind::Asset(project, old_id) => {
            if !is_asset_id(to) {
                return Err(params_invalid(format!(
                    "to is {to:?}: the entry is a whole asset, so to is a free asset id (a \
                     lowercase letter, then lowercase letters, digits and '_', at most 63 \
                     characters)"
                )));
            }
            // The new name's version lock, as a sign-off or a repair of it would hold it.
            let guard = store.version_lock(&project, to).lock_owned().await;
            let path = store
                .trash_restore_to(&args.entry_id, Some(to), None)
                .await?;
            let by = status_by(&ctx.session.caller);
            restored_as(
                &open.index,
                store,
                &project,
                &old_id,
                to,
                by.as_str(),
                None,
            )
            .await
            .map_err(|mut e| {
                // The folder is back already: say so, and how the rest gets done.
                e.message = format!(
                    "The asset was restored as {path}, but its redirect and manifest weren't                      updated: {} Once that is fixed, asset.accept_move {{asset_id: {to}}}                      finishes it.",
                    e.message
                );
                Refused::Body(Box::new(e))
            })?;
            drop(guard);
            let (from, to_name) = (format!("{project}/{old_id}"), format!("{project}/{to}"));
            let event = json!({"kind": "moved", "from": from, "to": to_name});
            ctx.events
                .publish(&asset_topic(&open.ws.id, &project, to), event.clone());
            ctx.events.publish(&format!("project:{project}"), event);
            Ok(path)
        }
        EntryKind::Resource => {
            let rewrite = resource_rewrite(services, to)?;
            Ok(store
                .trash_restore_to(&args.entry_id, Some(&format!("{to}.json")), Some(rewrite))
                .await?)
        }
        EntryKind::Other => Ok(store
            .trash_restore_to(&args.entry_id, Some(to), None)
            .await?),
    }
}

async fn restore_personal(
    _ctx: &CallCtx,
    personal: &Personal,
    services: &AssetServices,
    args: &RestoreArgs,
) -> Result<WsPath, Refused> {
    let Some(to) = args.to.as_deref() else {
        return Ok(personal.trash_restore(&args.entry_id).await?);
    };
    let record = personal.trash_entry(&args.entry_id).await?;
    let kind = entry_kind(TrashScope::Personal, &record.original_path);
    if to == current_name(&kind, &record.original_path) {
        return Ok(personal.trash_restore(&args.entry_id).await?);
    }
    match kind {
        EntryKind::Resource => {
            let rewrite = resource_rewrite(services, to)?;
            Ok(personal
                .trash_restore_to(&args.entry_id, Some(&format!("{to}.json")), Some(rewrite))
                .await?)
        }
        EntryKind::Asset(..) | EntryKind::InsideAsset | EntryKind::Other => Ok(personal
            .trash_restore_to(&args.entry_id, Some(to), None)
            .await?),
    }
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
/// resource trash, `services` the late-bound services (the resource id rewrite).
pub fn register_trash_tools(
    reg: &mut Registry,
    personal: &Arc<Personal>,
    services: &AssetServices,
) {
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
    let (p, services) = (personal.clone(), services.clone());
    reg.register(typed_tool(
        "trash.restore",
        RESTORE_DESCRIPTION,
        EffectClass::Write,
        WORKSPACE_MODULE,
        move |ctx, args: RestoreArgs| {
            let (p, services) = (p.clone(), services.clone());
            Box::pin(async move { restore(ctx, &p, &services, args).await })
        },
    ));
}
