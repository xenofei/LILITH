//! Staleness when a project is restored, and dependents that skip archived projects (I6,
//! CORE-048, CORE-055).
//!
//! While a project is archived, dependents scans skip it ([`dependents`]), so its assets are
//! not marked stale when their inputs move on. Whenever it is set to anything but `archived`,
//! [`restore_staleness`] catches up: each of its assets with an input whose
//! `approved_version` is now higher than the version in the input's ref, and which isn't
//! pinned, takes `MarkStale` through `lil_schema::change_status` by `core` (status `stale`,
//! `status_before_stale` = the status it had, a `status_reason` naming the input, and a
//! status-log entry with the reason `{code: "input_changed", ref: <the input's ref>}`). Only
//! `approved`, `exported` and `released` assets can go
//! stale (CORE-048); others, and assets already `stale`, are left as they are, so the check
//! can run again at any time (a retry after it failed midway finishes it).
//!
//! Every input and pin is resolved through [`Resolver`] (ARCH-125): an input naming an
//! asset's old place reads the approved version where the asset is now, and a pin of either
//! name pins it. An input that doesn't resolve (a loop, a dead end, the trash) can't make its
//! asset stale. A redirects file that can't be read or parsed fails the check only when an
//! input or a pin of a checked asset needs it (its name has no live folder), naming that asset
//! and that input or pin: never a silent "no redirects".
//!
//! The check is two steps, [`plan_staleness`] (reads only; every failure happens here) and
//! [`apply_staleness`] (the manifest edits), so `project.set_status` plans before it writes
//! `project.yaml` and a failed check writes nothing; [`restore_staleness`] runs both.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lil_schema::{
    AssetRef, LinkRef, Manifest, ManifestView, Reason, SCHEME, Status, StatusBy, StatusEvent,
    Version, change_status,
};
use serde_json::Value;

use super::redirects::REDIRECTS_YAML;
use super::resolve::Resolver;
use super::{ProjectFile, read_project};
use crate::graph::is_stale;
use crate::index::{Index, IndexError};
use crate::store::{Store, StoreError, WsPath};
use crate::workspace::OpenWorkspace;

fn index_error(e: &IndexError) -> StoreError {
    StoreError::Io {
        path: String::new(),
        why: e.to_string(),
    }
}

/// A redirects file that can't be read or parsed, needed for `needed` (`"<asset>'s input
/// <ref>"`), as a store error: one that doesn't parse is `yaml.unsupported_edit` (as a
/// `project.yaml` that doesn't parse is for `project.set_status`), any other failure
/// `store.io`. The message says why, names the file once and says what needed it.
pub(crate) fn redirects_error(e: &lil_protocol::ErrorBody, needed: &str) -> StoreError {
    if e.code == lil_protocol::codes::MANIFEST_NEEDS_REPAIR.code {
        StoreError::YamlUnsupported(format!("{} (needed for {needed})", e.message))
    } else {
        // The store's message already starts with the path; `StoreError::Io` adds it again.
        let prefix = format!("`{REDIRECTS_YAML}`: ");
        let why = e.message.strip_prefix(&prefix).unwrap_or(&e.message);
        StoreError::Io {
            path: REDIRECTS_YAML.to_string(),
            why: format!("{why} (needed for {needed})"),
        }
    }
}

/// Whether `project.yaml` of `slug` says `archived`.
async fn is_archived(store: &Store, slug: &str) -> bool {
    matches!(
        read_project(store, slug).await,
        Ok(ProjectFile::Parsed(p)) if p.status.as_deref() == Some("archived")
    )
}

/// The assets whose manifest `inputs` or `pinned_inputs` name `project`/`asset_id`, as
/// `(project, asset_id)`, sorted, without those in archived projects (I6: "Archived projects
/// are skipped until restored"). By this name only; use [`dependents_of`] with
/// `Resolver::names_resolving_to`.
///
/// # Errors
/// `store.io` when the index fails.
pub async fn dependents(
    store: &Store,
    index: &Index,
    project: &str,
    asset_id: &str,
) -> Result<Vec<(String, String)>, StoreError> {
    dependents_of(store, index, &[(project.to_string(), asset_id.to_string())]).await
}

/// [`dependents`] of any of `names` (`(project, asset_id)`: an asset and every name that
/// resolves to it, `Resolver::names_resolving_to`), each dependent once.
///
/// # Errors
/// `store.io` when the index fails.
pub async fn dependents_of(
    store: &Store,
    index: &Index,
    names: &[(String, String)],
) -> Result<Vec<(String, String)>, StoreError> {
    let all: std::collections::BTreeSet<(String, String)> = index
        .edges_to_any(names)
        .await
        .map_err(|e| index_error(&e))?
        .into_iter()
        .map(|e| (e.from_project, e.from_asset))
        .collect();
    let mut archived: HashMap<String, bool> = HashMap::new();
    let mut kept = Vec::with_capacity(all.len());
    for (p, a) in all {
        let skip = match archived.get(&p) {
            Some(skip) => *skip,
            None => {
                let skip = is_archived(store, &p).await;
                archived.insert(p.clone(), skip);
                skip
            }
        };
        if !skip {
            kept.push((p, a));
        }
    }
    Ok(kept)
}

/// The manifest of `projects/<project>/assets/<asset_id>/`, parsed; `None` when it is missing
/// or doesn't parse.
async fn manifest_of(store: &Store, project: &str, asset_id: &str) -> Option<Manifest> {
    let rel = manifest_path(project, asset_id)?;
    let bytes = store.read(&rel).await.ok()??;
    let text = std::str::from_utf8(&bytes).ok()?;
    Manifest::parse(text).ok().map(|(m, _)| m)
}

fn manifest_path(project: &str, asset_id: &str) -> Option<WsPath> {
    WsPath::parse(&format!(
        "projects/{project}/assets/{asset_id}/{asset_id}.manifest.json"
    ))
    .ok()
}

/// The asset an input ref points to in `project`'s scope, and the version it names.
fn target_of(project: &str, link: &LinkRef) -> Option<((String, String), Version)> {
    match link {
        LinkRef::Local { asset, version } => Some(((project.to_string(), asset.clone()), *version)),
        LinkRef::Project {
            slug,
            asset,
            version,
        } => Some(((slug.clone(), asset.clone()), *version)),
        LinkRef::Library { .. } | LinkRef::Style(_) => None,
    }
}

/// Where `target` resolves to; `None` when it doesn't. `needed` says what asks (for the error).
///
/// # Errors
/// The redirects file's own error when the resolution needed the file and it can't be read or
/// parsed.
async fn resolved(
    resolver: &Resolver<'_>,
    target: &(String, String),
    needed: &str,
) -> Result<Option<(String, String)>, StoreError> {
    match resolver.resolve_name(&target.0, &target.1).await {
        Ok(r) => Ok(Some((r.project, r.asset_id))),
        Err(e) if resolver.is_file_error(&e) => Err(redirects_error(&e, needed)),
        Err(_) => Ok(None),
    }
}

/// The first input of `view` that makes it stale, as `(asset_id, approved version, the
/// input's ref)`, looking targets' approved versions up in `approved` (read once per target),
/// every target resolved through `resolver`.
async fn stale_input(
    store: &Store,
    resolver: &Resolver<'_>,
    project: &str,
    view: &ManifestView,
    approved: &mut HashMap<(String, String), Option<Version>>,
) -> Result<Option<(String, Version, String)>, StoreError> {
    let asset = &view.asset_id;
    let mut pinned: Vec<(String, String)> = Vec::new();
    for pin in &view.pinned_inputs {
        let Ok(link) = pin.r#ref.parse::<LinkRef>() else {
            continue;
        };
        if let Some((target, _)) = target_of(project, &link) {
            let needed = format!("{asset}'s pin {}", pin.r#ref);
            pinned.push(
                resolved(resolver, &target, &needed)
                    .await?
                    .unwrap_or(target),
            );
        }
    }
    for input in &view.inputs {
        let Ok(link) = input.r#ref.parse::<LinkRef>() else {
            continue;
        };
        let Some((named, version)) = target_of(project, &link) else {
            continue;
        };
        let needed = format!("{asset}'s input {}", input.r#ref);
        let Some(target) = resolved(resolver, &named, &needed).await? else {
            continue;
        };
        let now = match approved.get(&target) {
            Some(v) => *v,
            None => {
                let v = manifest_of(store, &target.0, &target.1)
                    .await
                    .and_then(|m| m.view().approved_version);
                approved.insert(target.clone(), v);
                v
            }
        };
        if is_stale(&version, now.as_ref(), pinned.contains(&target)) {
            return Ok(now.map(|v| (target.1.clone(), v, input.r#ref.clone())));
        }
    }
    Ok(None)
}

/// One asset [`apply_staleness`] marks.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Mark {
    asset_id: String,
    /// The SHA-256 of the manifest the plan read (as `Manifest::to_text` writes it).
    manifest_sha256: String,
    /// The input's asset id, its approved version and the input's ref.
    input: String,
    version: Version,
    input_ref: String,
}

/// What [`apply_staleness`] marks: per asset of the project, the input that makes it stale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalenessPlan {
    project: String,
    marks: Vec<Mark>,
}

fn manifest_sha256(m: &Manifest) -> String {
    lil_schema::sha256_hex(m.to_text().as_bytes())
}

/// The first step of [`restore_staleness`]: which assets of `project` an input moved on for
/// while it was archived (module docs). Reads only.
///
/// # Errors
/// `store.io` when the index fails; the redirects file's error (`yaml.unsupported_edit`, or
/// `store.io` when it can't be read) when a checked input or pin needs it.
pub async fn plan_staleness(
    ws: &OpenWorkspace,
    project: &str,
) -> Result<StalenessPlan, StoreError> {
    ws.index.ready_result().await.map_err(|e| index_error(&e))?;
    let resolver = Resolver::load(&ws.index, &ws.store)
        .await
        .map_err(|e| redirects_error(&e, "the staleness check"))?;
    let assets = ws
        .index
        .assets_of(project)
        .await
        .map_err(|e| index_error(&e))?;
    let mut approved: HashMap<(String, String), Option<Version>> = HashMap::new();
    let mut marks = Vec::new();
    for asset in assets {
        if asset.needs_repair {
            continue;
        }
        let Some(manifest) = manifest_of(&ws.store, project, &asset.asset_id).await else {
            continue;
        };
        let view = manifest.view();
        if !matches!(
            view.status,
            Some(Status::Approved | Status::Exported | Status::Released)
        ) {
            continue;
        }
        if let Some((input, version, input_ref)) =
            stale_input(&ws.store, &resolver, project, &view, &mut approved).await?
        {
            marks.push(Mark {
                asset_id: asset.asset_id.clone(),
                manifest_sha256: manifest_sha256(&manifest),
                input,
                version,
                input_ref,
            });
        }
    }
    Ok(StalenessPlan {
        project: project.to_string(),
        marks,
    })
}

/// Marks the assets of `project` stale whose inputs moved on while it was archived (module
/// docs), and returns the refs of those this call marked
/// (`lil://<ws>/<project>/<asset_id>@<approved version>#sha256=<content hash>`, see
/// `stale_ref`): [`plan_staleness`], then [`apply_staleness`].
///
/// # Errors
/// As [`plan_staleness`]; a store error of a manifest edit.
pub async fn restore_staleness(
    ws: &OpenWorkspace,
    project: &str,
) -> Result<Vec<String>, StoreError> {
    let plan = plan_staleness(ws, project).await?;
    apply_staleness(ws, plan).await
}

/// The second step of [`restore_staleness`]: marks each planned asset stale. Each edit is the
/// store's hash-checked manifest edit, and inside it the manifest must still hash as it did
/// when the plan read it: an asset that changed since the plan (re-signed, re-pinned, edited)
/// is left as it is, and the next check sees it as it is now.
///
/// # Errors
/// A store error of a manifest edit.
pub async fn apply_staleness(
    ws: &OpenWorkspace,
    plan: StalenessPlan,
) -> Result<Vec<String>, StoreError> {
    let project = plan.project.as_str();
    let mut stale = Vec::new();
    for Mark {
        asset_id,
        manifest_sha256: planned,
        input,
        version,
        input_ref,
    } in plan.marks
    {
        let Some(rel) = manifest_path(project, &asset_id) else {
            continue;
        };
        let note = format!("input {input} {version} signed off");
        let reason = Reason {
            code: "input_changed".to_string(),
            r#ref: Some(input_ref),
            note: None,
        };
        let utc = lil_schema::time::rfc3339(ws.store.now_utc());
        let marked = Arc::new(AtomicBool::new(false));
        let did = marked.clone();
        let (after, _written) = ws
            .store
            .edit_manifest(&rel, &move |m: &mut Manifest| {
                did.store(false, Ordering::SeqCst);
                if manifest_sha256(m) != planned {
                    return Ok(());
                }
                let ev = StatusEvent::MarkStale;
                let by = StatusBy::Core;
                if change_status(m, &ev, by, None, Some(reason.clone()), &utc).is_err() {
                    return Ok(());
                }
                m.set("status_reason", Value::from(note.as_str()));
                did.store(true, Ordering::SeqCst);
                Ok(())
            })
            .await?;
        if marked.load(Ordering::SeqCst) {
            let approved = after.view().approved_version;
            stale.push(stale_ref(&ws.store, &ws.ws.id, project, &asset_id, approved).await);
        }
    }
    Ok(stale)
}

/// The ref of an asset marked stale: its approved version with the version's content hash
/// (`lil://<ws>/<project>/<asset_id>@<version>#sha256=<hex>`; CORE-047: every new reference to
/// a sealed version carries the hash), from `Store::versions` as `asset.get` gives it. When
/// that version's folder isn't on this machine (or can't be read without a download), the
/// hash can't be known and the ref names the version without it (logged); an asset with no
/// approved version gets its ref without a version.
pub async fn stale_ref(
    store: &Store,
    ws_id: &str,
    project: &str,
    asset_id: &str,
    approved: Option<Version>,
) -> String {
    let Some(version) = approved else {
        return format!("{SCHEME}{ws_id}/{project}/{asset_id}");
    };
    let sha256 = match store.versions(project, asset_id).await {
        Ok(versions) => versions
            .into_iter()
            .find(|v| v.version == version)
            .and_then(|v| v.content_hash),
        Err(e) => {
            tracing::warn!(project, asset_id, error = %e, "a stale asset's versions can't be read");
            None
        }
    };
    if sha256.is_none() {
        tracing::warn!(project, asset_id, %version, "no folder for the approved version; its ref carries no content hash");
    }
    AssetRef::Sealed {
        ws: ws_id.to_string(),
        project: project.to_string(),
        asset: asset_id.to_string(),
        version,
        sha256,
    }
    .to_string()
}
