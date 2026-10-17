//! Staleness when a project is restored, and dependents that skip archived projects (I6,
//! CORE-048, CORE-055).
//!
//! While a project is archived, dependents scans skip it ([`dependents`]), so its assets are
//! not marked stale when their inputs move on. When it goes back to `active`,
//! [`restore_staleness`] catches up: each of its assets with an input whose
//! `approved_version` is now higher than the version in the input's ref, and which isn't
//! pinned, takes `MarkStale` (status `stale`, `status_before_stale` = the status it had, and a
//! `status_reason` naming the input). Only `approved`, `exported` and `released` assets can go
//! stale (CORE-048); others are left as they are.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;
use ww_schema::{
    AssetRef, LinkRef, Manifest, ManifestView, Status, StatusEvent, Version, transition,
};

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

/// Marks the assets of `project` stale whose inputs moved on while it was archived or paused
/// (module docs), and returns their refs (`ww://<ws>/<project>/<asset_id>@<approved version>`,
/// or without a version when none is approved). Each edit is the store's hash-checked
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
            let view = after.view();
            stale.push(match view.approved_version {
                Some(version) => AssetRef::Sealed {
                    ws: ws.ws.id.clone(),
                    project: project.to_string(),
                    asset: asset.asset_id.clone(),
                    version,
                    sha256: None,
                }
                .to_string(),
                None => format!("ww://{}/{project}/{}", ws.ws.id, asset.asset_id),
            });
        }
    }
    Ok(stale)
}
