//! The `resources` module (CORE-014, APP-078–080, APP-082, CORE-110): typed Libraries that
//! every app and agent share (Appendix F "Resources").
//!
//! A resource is one JSON file, `{schema_version: 1, kind, id, name, created, updated, data}`
//! in that key order, at `library/resources/<kind>/<id>.json` in the workspace
//! ([`ResourceScope::Workspace`]) or `%APPDATA%\LILITH\resources\<kind>\<id>.json`
//! ([`ResourceScope::Personal`]). The kinds are [`RESOURCE_KINDS`] (`resource.kind_unknown`
//! for any other), and an id matches `^[a-z][a-z0-9_-]{0,62}$` (`params.invalid`).
//!
//! **Scope.** A save without a scope goes to the workspace when the session has one, except a
//! `model_profile`, which is machine-level and goes to the personal scope, as does everything
//! in a session without a workspace ([`default_scope`]). A `get`, `delete` or `list` without a
//! scope looks in every scope the session can see; one that exists in both is refused with
//! `resource.ambiguous_scope` (P3-R23), so a call never acts on a resource other than the one
//! meant. Every `get`, `delete` and `list` result names the scope.
//!
//! **Nothing is lost** (CORE-110). A save over an existing resource first moves the old file to
//! the scope's trash with the reason `"resource.save"` (the workspace's `.trash\`, or the
//! personal trash, [`Personal`]); the new file keeps the old `created` and gets a new
//! `updated`. A delete moves the file to the trash with the reason `"resource.delete"`. Either
//! entry comes back with `trash.restore`, which never replaces a resource that is there now.
//! Saves and deletes run one at a time. The changes are published on the `resources` topic as
//! `{kind: "saved", resource_kind, id, scope, updated, replaced_entry_id?}` and
//! `{kind: "deleted", resource_kind, id, scope, entry_id}`.

pub mod tools;

use std::path::PathBuf;
use std::sync::Arc;

use lil_platform::Clock;
use lil_protocol::{ErrorBody, codes};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::store::{Expect, Personal, Store, StoreError, WsPath, list_files};
use crate::tools::CallCtx;

pub use tools::register_resource_tools;

/// The tools' module (CORE-014).
pub const RESOURCES_MODULE: &str = "resources";
/// The topic the changes are published on (APP-078).
pub const RESOURCES_TOPIC: &str = "resources";
/// The resource kinds (APP-079, APP-080).
pub const RESOURCE_KINDS: &[&str] = &[
    "swatch",
    "palette",
    "prompt_preset",
    "style_preset",
    "model_profile",
    "prompt_brief",
    "workflow",
];
/// The `schema_version` of the resource file.
pub const RESOURCE_SCHEMA_VERSION: u32 = 1;
/// The longest `name` a resource has, in characters.
pub const MAX_NAME_CHARS: usize = 200;
/// The largest resource file, in bytes.
pub const MAX_FILE_BYTES: usize = 1024 * 1024;
/// Why a save moves the old file to the trash.
pub const SAVE_REASON: &str = "resource.save";
/// Why a delete moves the file to the trash.
pub const DELETE_REASON: &str = "resource.delete";

/// Where a resource lives.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ResourceScope {
    Workspace,
    Personal,
}

impl ResourceScope {
    pub fn as_str(self) -> &'static str {
        match self {
            ResourceScope::Workspace => "workspace",
            ResourceScope::Personal => "personal",
        }
    }
}

/// The scope a save without one goes to (module docs).
pub fn default_scope(kind: &str, has_workspace: bool) -> ResourceScope {
    if has_workspace && kind != "model_profile" {
        ResourceScope::Workspace
    } else {
        ResourceScope::Personal
    }
}

/// A resource file; the field order is the file's key order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Resource {
    pub schema_version: u32,
    pub kind: String,
    pub id: String,
    pub name: String,
    /// RFC 3339 UTC.
    pub created: String,
    /// RFC 3339 UTC.
    pub updated: String,
    /// What the kind defines; the core keeps it as given.
    pub data: Value,
}

/// One row of `resource.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResourceSummary {
    pub kind: String,
    pub id: String,
    pub name: String,
    pub scope: ResourceScope,
    pub updated: String,
}

/// The core's resources (module docs).
#[derive(Debug)]
pub struct Resources {
    personal: Arc<Personal>,
    clock: Arc<dyn Clock>,
    /// Held by a save or a delete from its read to its last write.
    gate: tokio::sync::Mutex<()>,
}

/// One scope's files, as a resource call sees them.
enum Place {
    Workspace(Arc<Store>),
    Personal(Arc<Personal>),
}

impl Place {
    /// The path of `<kind>/<id>.json` in this scope, relative to the store's or the personal
    /// folder's root.
    fn rel(&self, kind: &str, id: &str) -> WsPath {
        let tail = format!("{kind}/{id}.json");
        match self {
            Place::Workspace(_) => WsPath::from_disk(format!("library/resources/{tail}")),
            Place::Personal(_) => WsPath::from_disk(tail),
        }
    }

    fn abs(&self, rel: &WsPath) -> PathBuf {
        match self {
            Place::Workspace(store) => rel.abs(store.root()),
            Place::Personal(personal) => rel.abs(personal.root()),
        }
    }

    async fn read(&self, rel: &WsPath) -> Result<Option<Vec<u8>>, StoreError> {
        match self {
            Place::Workspace(store) => store.read(rel).await,
            Place::Personal(personal) => personal.read(rel).await,
        }
    }

    async fn write_new(&self, rel: &WsPath, bytes: Vec<u8>) -> Result<(), StoreError> {
        match self {
            Place::Workspace(store) => store.write(rel, bytes, Expect::New).await.map(drop),
            Place::Personal(personal) => personal.write_new(rel, bytes).await,
        }
    }

    /// The entry id of the file moved to the trash.
    async fn trash(&self, rel: &WsPath, reason: &str) -> Result<String, StoreError> {
        match self {
            Place::Workspace(store) => store.trash(rel, reason).await.map(|r| r.entry_id),
            Place::Personal(personal) => personal.trash(rel, reason).await.map(|r| r.entry_id),
        }
    }

    async fn untrash(&self, entry_id: &str) -> Result<WsPath, StoreError> {
        match self {
            Place::Workspace(store) => store.trash_restore(entry_id).await,
            Place::Personal(personal) => personal.trash_restore(entry_id).await,
        }
    }

    /// The ids of the resources of `kind` here. A link on the way to the folder is refused
    /// (`store.path_invalid`).
    async fn ids(&self, kind: &str) -> Result<Vec<String>, StoreError> {
        let rel = self.rel(kind, "x");
        if let Place::Workspace(store) = self {
            store.refuse_links(&rel).await?;
        }
        let file = self.abs(&rel);
        let dir = file
            .parent()
            .map_or_else(|| file.clone(), std::path::Path::to_path_buf);
        let (probe, kind_dir) = match self {
            Place::Workspace(store) => (store.probe().clone(), None),
            Place::Personal(personal) => (personal.probe().clone(), Some(personal.clone())),
        };
        if let Some(personal) = kind_dir {
            return Ok(ids_of(personal.list(kind).await?));
        }
        let names = tokio::task::spawn_blocking(move || list_files(probe.as_ref(), &dir))
            .await
            .unwrap_or_else(|e| Err(std::io::Error::other(e.to_string())))
            .map_err(|e| StoreError::Io {
                path: rel.as_str().to_string(),
                why: e.to_string(),
            })?;
        Ok(ids_of(names))
    }
}

/// The resource ids among the file names of a kind's folder.
fn ids_of(names: Vec<String>) -> Vec<String> {
    names
        .into_iter()
        .filter_map(|n| n.strip_suffix(".json").map(str::to_string))
        .filter(|id| valid_id(id))
        .collect()
}

/// Whether `id` matches `^[a-z][a-z0-9_-]{0,62}$`.
pub fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && id.len() <= 63
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn invalid(ctx: &CallCtx, message: String) -> ErrorBody {
    ErrorBody::new(&codes::PARAMS_INVALID, message, ctx.trace_id.clone())
}

/// `resource.kind_unknown` or `params.invalid` for a kind and id that aren't acceptable. An id
/// that makes a path the store refuses (a Windows device name such as `con` or `lpt1`,
/// CORE-040) is refused too (P3-R22).
fn check(ctx: &CallCtx, kind: &str, id: &str) -> Result<(), ErrorBody> {
    check_kind(ctx, kind)?;
    if let Err(problem) = lil_schema::check_rel_path(&format!("{kind}/{id}.json")) {
        return Err(invalid(
            ctx,
            format!("`{id}` is not a resource id: {problem}"),
        ));
    }
    if !valid_id(id) {
        return Err(invalid(
            ctx,
            format!(
                "`{id}` is not a resource id: it starts with a lower-case letter and has at most \
                 63 characters from a-z, 0-9, `_` and `-`"
            ),
        ));
    }
    Ok(())
}

fn check_kind(ctx: &CallCtx, kind: &str) -> Result<(), ErrorBody> {
    if RESOURCE_KINDS.contains(&kind) {
        return Ok(());
    }
    let mut body = ErrorBody::new(
        &codes::RESOURCE_KIND_UNKNOWN,
        format!(
            "`{kind}` is not a resource kind; the kinds are {}",
            RESOURCE_KINDS.join(", ")
        ),
        ctx.trace_id.clone(),
    );
    body.data = Some(json!({ "kind": kind, "kinds": RESOURCE_KINDS }));
    Err(body)
}

fn store_error(ctx: &CallCtx, e: &StoreError) -> ErrorBody {
    e.to_error(&ctx.trace_id)
}

/// A resource file's bytes as a [`Resource`].
fn parse(rel: &WsPath, bytes: &[u8]) -> Result<Resource, StoreError> {
    serde_json::from_slice(bytes).map_err(|e| StoreError::Io {
        path: rel.as_str().to_string(),
        why: format!("this is not a resource file: {e}"),
    })
}

impl Resources {
    /// Resources whose personal scope is `personal`; `clock` stamps `created` and `updated`.
    pub fn new(personal: Arc<Personal>, clock: Arc<dyn Clock>) -> Resources {
        Resources {
            personal,
            clock,
            gate: tokio::sync::Mutex::new(()),
        }
    }

    fn now(&self) -> String {
        let secs = self
            .clock
            .utc_now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        lil_schema::time::rfc3339(secs)
    }

    /// The scope `scope` of the call, which for `Workspace` needs the session's workspace.
    fn place(&self, ctx: &CallCtx, scope: ResourceScope) -> Result<Place, ErrorBody> {
        match scope {
            ResourceScope::Workspace => Ok(Place::Workspace(ctx.workspace()?.store.clone())),
            ResourceScope::Personal => Ok(Place::Personal(self.personal.clone())),
        }
    }

    /// The scopes a call without one looks in, in order.
    fn scopes(ctx: &CallCtx, scope: Option<ResourceScope>) -> Vec<ResourceScope> {
        match scope {
            Some(scope) => vec![scope],
            None if ctx.workspace.is_some() => {
                vec![ResourceScope::Workspace, ResourceScope::Personal]
            }
            None => vec![ResourceScope::Personal],
        }
    }

    /// Saves `r` in `scope` (module docs). Its `schema_version`, `created` and `updated` are
    /// set here: `created` is the old file's, when there is one that can be read. Returns the
    /// resource as written, its absolute path and the trash entry of the file it replaced.
    ///
    /// # Errors
    /// `resource.kind_unknown`; `params.invalid` for a bad id; `workspace.required` for the
    /// workspace scope without a workspace; the store's errors (`workspace.read_only`,
    /// `store.io`, …). When the new file can't be written after the old one went to the trash,
    /// the old one is put back.
    pub async fn save(
        &self,
        ctx: &CallCtx,
        scope: ResourceScope,
        r: Resource,
    ) -> Result<(Resource, PathBuf, Option<String>), ErrorBody> {
        check(ctx, &r.kind, &r.id)?;
        if r.name.chars().count() > MAX_NAME_CHARS || r.name.chars().any(char::is_control) {
            return Err(invalid(
                ctx,
                format!(
                    "a resource name has at most {MAX_NAME_CHARS} characters and no control characters"
                ),
            ));
        }
        let place = self.place(ctx, scope)?;
        let rel = place.rel(&r.kind, &r.id);
        let _one_at_a_time = self.gate.lock().await;
        let now = self.now();
        let old = place.read(&rel).await.map_err(|e| store_error(ctx, &e))?;
        let created = old
            .as_deref()
            .and_then(|bytes| parse(&rel, bytes).ok())
            .map_or_else(|| now.clone(), |old| old.created);
        let saved = Resource {
            schema_version: RESOURCE_SCHEMA_VERSION,
            created,
            updated: now,
            ..r
        };
        let mut text = serde_json::to_string_pretty(&saved).map_err(|e| {
            ErrorBody::new(
                &codes::CORE_BUG,
                format!("a resource is not JSON: {e}"),
                ctx.trace_id.clone(),
            )
        })?;
        text.push('\n');
        if text.len() > MAX_FILE_BYTES {
            return Err(invalid(
                ctx,
                format!("a resource file is at most {MAX_FILE_BYTES} bytes"),
            ));
        }
        let replaced = match old {
            Some(_) => Some(
                place
                    .trash(&rel, SAVE_REASON)
                    .await
                    .map_err(|e| store_error(ctx, &e))?,
            ),
            None => None,
        };
        if let Err(e) = place.write_new(&rel, text.into_bytes()).await {
            if let Some(entry) = &replaced
                && let Err(back) = place.untrash(entry).await
            {
                tracing::warn!(entry = %entry, error = %back, "a replaced resource stays in the trash; trash.restore brings it back");
            }
            return Err(store_error(ctx, &e));
        }
        let mut event = json!({
            "kind": "saved",
            "resource_kind": saved.kind,
            "id": saved.id,
            "scope": scope.as_str(),
            "updated": saved.updated,
        });
        if let Some(entry) = &replaced {
            event["replaced_entry_id"] = json!(entry);
        }
        ctx.events.publish(RESOURCES_TOPIC, event);
        let path = place.abs(&rel);
        Ok((saved, path, replaced))
    }

    /// The resources, by kind (in the order of [`RESOURCE_KINDS`]), then id, then scope: of
    /// `kind` or of every kind, in `scope` or in every scope the call can see. A file that
    /// isn't a resource is left out.
    ///
    /// # Errors
    /// `resource.kind_unknown`; `workspace.required` for the workspace scope without a
    /// workspace; the store's errors.
    pub async fn list(
        &self,
        ctx: &CallCtx,
        kind: Option<&str>,
        scope: Option<ResourceScope>,
    ) -> Result<Vec<ResourceSummary>, ErrorBody> {
        if let Some(kind) = kind {
            check_kind(ctx, kind)?;
        }
        let mut found = Vec::new();
        for scope in Self::scopes(ctx, scope) {
            let place = self.place(ctx, scope)?;
            for kind in RESOURCE_KINDS
                .iter()
                .filter(|k| kind.is_none_or(|want| want == **k))
            {
                let ids = match place.ids(kind).await {
                    Ok(ids) => ids,
                    Err(e @ StoreError::PathInvalid { .. }) => {
                        tracing::warn!(error = %e, "a linked resource folder is left out of the list");
                        continue;
                    }
                    Err(e) => return Err(store_error(ctx, &e)),
                };
                for id in ids {
                    let rel = place.rel(kind, &id);
                    let Some(bytes) = place.read(&rel).await.map_err(|e| store_error(ctx, &e))?
                    else {
                        continue;
                    };
                    match parse(&rel, &bytes) {
                        Ok(r) => found.push(ResourceSummary {
                            kind: (*kind).to_string(),
                            id,
                            name: r.name,
                            scope,
                            updated: r.updated,
                        }),
                        Err(e) => {
                            tracing::warn!(error = %e, "a file in the resource folder is left out of the list");
                        }
                    }
                }
            }
        }
        let rank = |kind: &str| RESOURCE_KINDS.iter().position(|k| *k == kind);
        found.sort_by(|a, b| (rank(&a.kind), &a.id, a.scope).cmp(&(rank(&b.kind), &b.id, b.scope)));
        Ok(found)
    }

    /// The resource `kind`/`id` and its absolute path, from `scope`, or from the one scope that
    /// has it when there is none (module docs).
    ///
    /// # Errors
    /// `resource.kind_unknown`; `params.invalid`; `resource.unknown` when there is none;
    /// `resource.ambiguous_scope` without a scope when both scopes have it; `workspace.required`;
    /// the store's errors (`store.io` for a file that isn't a resource).
    pub async fn get(
        &self,
        ctx: &CallCtx,
        kind: &str,
        id: &str,
        scope: Option<ResourceScope>,
    ) -> Result<(Resource, PathBuf), ErrorBody> {
        let (resource, _, path) = self.find(ctx, kind, id, scope).await?;
        Ok((resource, path))
    }

    /// [`Resources::get`] with the scope it was found in.
    ///
    /// # Errors
    /// As [`Resources::get`].
    pub async fn find(
        &self,
        ctx: &CallCtx,
        kind: &str,
        id: &str,
        scope: Option<ResourceScope>,
    ) -> Result<(Resource, ResourceScope, PathBuf), ErrorBody> {
        check(ctx, kind, id)?;
        let mut found = Vec::new();
        for scope in Self::scopes(ctx, scope) {
            let place = self.place(ctx, scope)?;
            let rel = place.rel(kind, id);
            if let Some(bytes) = place.read(&rel).await.map_err(|e| store_error(ctx, &e))? {
                let resource = parse(&rel, &bytes).map_err(|e| store_error(ctx, &e))?;
                found.push((resource, scope, place.abs(&rel)));
            }
        }
        match found.len() {
            1 => Ok(found.remove(0)),
            0 => {
                let mut body = ErrorBody::new(
                    &codes::RESOURCE_UNKNOWN,
                    format!(
                        "there is no {kind} resource `{id}`; resource.list lists the ones there are"
                    ),
                    ctx.trace_id.clone(),
                );
                body.data = Some(json!({ "kind": kind, "id": id }));
                Err(body)
            }
            _ => {
                let mut body = ErrorBody::new(
                    &codes::RESOURCE_AMBIGUOUS_SCOPE,
                    format!(
                        "the {kind} resource `{id}` exists in the workspace and in the personal \
                         scope; pass scope"
                    ),
                    ctx.trace_id.clone(),
                );
                body.data =
                    Some(json!({ "kind": kind, "id": id, "scopes": ["workspace", "personal"] }));
                Err(body)
            }
        }
    }

    /// Moves the resource `kind`/`id` to the trash of the scope it is in (`scope`, or the one
    /// scope that has it) and returns the trash entry's id (APP-082: a soft delete;
    /// `trash.restore` brings it back).
    ///
    /// # Errors
    /// As [`Resources::get`], and the store's errors.
    pub async fn delete(
        &self,
        ctx: &CallCtx,
        kind: &str,
        id: &str,
        scope: Option<ResourceScope>,
    ) -> Result<String, ErrorBody> {
        self.remove(ctx, kind, id, scope)
            .await
            .map(|(entry, _)| entry)
    }

    /// [`Resources::delete`] with the scope the resource was in.
    ///
    /// # Errors
    /// As [`Resources::delete`].
    pub async fn remove(
        &self,
        ctx: &CallCtx,
        kind: &str,
        id: &str,
        scope: Option<ResourceScope>,
    ) -> Result<(String, ResourceScope), ErrorBody> {
        check(ctx, kind, id)?;
        let _one_at_a_time = self.gate.lock().await;
        let (_, found_in, _) = self.find(ctx, kind, id, scope).await?;
        let place = self.place(ctx, found_in)?;
        let entry = place
            .trash(&place.rel(kind, id), DELETE_REASON)
            .await
            .map_err(|e| store_error(ctx, &e))?;
        ctx.events.publish(
            RESOURCES_TOPIC,
            json!({
                "kind": "deleted",
                "resource_kind": kind,
                "id": id,
                "scope": found_in.as_str(),
                "entry_id": entry,
            }),
        );
        Ok((entry, found_in))
    }
}

/// Runs the personal trash's purge ([`Personal::maintain`]) in the background: a pass at once
/// and then every 24 hours (on tokio's clock), like a workspace's (CORE-037).
pub fn spawn_personal_maintenance(personal: Arc<Personal>) {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(crate::workspace::MAINTENANCE_PERIOD);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            match personal.maintain().await {
                Ok(purged) if !purged.is_empty() => {
                    tracing::info!(?purged, "personal trash maintenance");
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "the personal trash maintenance stopped; it runs again later");
                }
            }
        }
    });
}
