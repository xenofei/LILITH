//! Staleness when a project is restored, and dependents that skip archived projects (I6,
//! CORE-048, CORE-055).
//!
//! While a project is archived, dependents scans skip it ([`dependents`]), so its assets are
//! not marked stale when their inputs move on. Whenever it is set to anything but `archived`,
//! [`restore_staleness`] catches up: each of its assets with an input whose
//! `approved_version` is now higher than the version in the input's ref, and which isn't
//! pinned, takes `MarkStale` (status `stale`, `status_before_stale` = the status it had, and a
//! `status_reason` naming the input). Only `approved`, `exported` and `released` assets can go
//! stale (CORE-048); others, and assets already `stale`, are left as they are, so the check
//! can run again at any time (a retry after it failed midway finishes it).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lil_schema::{
    AssetRef, LinkRef, Manifest, ManifestView, SCHEME, Status, StatusEvent, Version, transition,
};
use serde_json::Value;

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

/// Whether `project.yaml` of `slug` says `archived`.
async fn is_archived(store: &Store, slug: &str) -> bool {
    matches!(
        read_project(store, slug).await,
        Ok(ProjectFile::Parsed(p)) if p.status.as_deref() == Some("archived")
    )
}

/// The assets whose manifest `inputs` or `pinned_inputs` name `project`/`asset_id`, as
/// `(project, asset_id)`, sorted, without those in archived projects (I6: "Archived projects
/// are skipped until restored").
///
/// # Errors
/// `store.io` when the index fails.
pub async fn dependents(
    store: &Store,
    index: &Index,
    project: &str,
    asset_id: &str,
) -> Result<Vec<(String, String)>, StoreError> {
    let all = index
        .dependents(project, asset_id)
        .await
        .map_err(|e| index_error(&e))?;
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

/// The first input of `view` that makes it stale, as `(asset_id, approved version)`, looking
/// targets' approved versions up in `approved` (read once per target).
async fn stale_input(
    store: &Store,
    project: &str,
    view: &ManifestView,
    approved: &mut HashMap<(String, String), Option<Version>>,
) -> Option<(String, Version)> {
    let pinned: Vec<(String, String)> = view
        .pinned_inputs
        .iter()
        .filter_map(|p| p.r#ref.parse::<LinkRef>().ok())
        .filter_map(|link| target_of(project, &link).map(|(target, _)| target))
        .collect();
    for input in &view.inputs {
        let Ok(link) = input.r#ref.parse::<LinkRef>() else {
            continue;
        };
        let Some((target, version)) = target_of(project, &link) else {
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
            return now.map(|v| (target.1.clone(), v));
        }
    }
    None
}

/// Marks the assets of `project` stale whose inputs moved on while it was archived (module
/// docs), and returns the refs of those this call marked
/// (`lil://<ws>/<project>/<asset_id>@<approved version>#sha256=<content hash>`, see
/// `stale_ref`). Each edit is the store's hash-checked
/// manifest edit; the status is checked again on the manifest as it is written.
///
/// # Errors
/// `store.io` when the index fails; a store error of a manifest edit.
pub async fn restore_staleness(
    ws: &OpenWorkspace,
    project: &str,
) -> Result<Vec<String>, StoreError> {
    ws.index.ready_result().await.map_err(|e| index_error(&e))?;
    let assets = ws
        .index
        .assets_of(project)
        .await
        .map_err(|e| index_error(&e))?;
    let mut approved: HashMap<(String, String), Option<Version>> = HashMap::new();
    let mut stale = Vec::new();
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
        let Some((input, version)) = stale_input(&ws.store, project, &view, &mut approved).await
        else {
            continue;
        };
        let Some(rel) = manifest_path(project, &asset.asset_id) else {
            continue;
        };
        let reason = format!("input {input} {version} signed off");
        let marked = Arc::new(AtomicBool::new(false));
        let did = marked.clone();
        let (after, _written) = ws
            .store
            .edit_manifest(&rel, &move |m: &mut Manifest| {
                did.store(false, Ordering::SeqCst);
                let parse = |key: &str| {
                    m.get(key)
                        .and_then(Value::as_str)
                        .and_then(|s| s.parse::<Status>().ok())
                };
                let Some(from) = m.view().status else {
                    return Ok(());
                };
                let Ok(moved) = transition(
                    from,
                    parse("status_before_stale"),
                    parse("status_before_blocked"),
                    &StatusEvent::MarkStale,
                ) else {
                    return Ok(());
                };
                m.set("status", Value::from(moved.to.as_str()));
                if let Some(before) = moved.set_before_stale {
                    m.set("status_before_stale", Value::from(before.as_str()));
                }
                m.set("status_reason", Value::from(reason.as_str()));
                did.store(true, Ordering::SeqCst);
                Ok(())
            })
            .await?;
        if marked.load(Ordering::SeqCst) {
            let approved = after.view().approved_version;
            stale.push(stale_ref(&ws.store, &ws.ws.id, project, &asset.asset_id, approved).await);
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
