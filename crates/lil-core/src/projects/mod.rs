//! The `projects` module (CORE-014; CORE-039, CORE-048, CORE-055, ARCH-088, ARCH-098): plain
//! functions over [`Store`] and the index that take no `tools` types, so the project tools and
//! the `project:` snapshot provider (Task 13) share them.
//!
//! - [`progress`]: `progress.md` entries under `## Done`;
//! - [`active`]: the per-session active project and the `state.json` hint;
//! - [`staleness`]: the staleness check when a project is restored, and dependents that skip
//!   archived projects (I6);
//! - [`redirects`]: `registry/redirects.yaml` (ARCH-125);
//! - [`resolve`]: the [`Resolver`], which every ref-to-folder lookup goes through (ARCH-125,
//!   APP-019);
//! - [`repair`]: accepting a move and making a copy independent (CORE-154);
//! - this file: reading `project.yaml`, and [`project_status`], the `project.status` result.
//!
//! A project exists when its `projects/<slug>/project.yaml` does.

pub mod active;
pub mod progress;
pub mod redirects;
pub mod repair;
pub mod resolve;
pub mod staleness;

use std::collections::{BTreeMap, BTreeSet};

use lil_protocol::ErrorBody;
use lil_protocol::codes::{PARAMS_INVALID, PROJECT_UNKNOWN};
use lil_schema::is_slug;
use lil_schema::workspace_yaml::{ProjectYaml, read_project_yaml};
use schemars::JsonSchema;
use serde::Serialize;

use crate::index::FileState;
use crate::index::repair::{RepairKind, uuid_key};
use crate::store::{Store, StoreError, WsPath};
use crate::workspace::OpenWorkspace;

pub use active::{
    ActiveProjects, DEFAULT_PROJECT, HintWrite, STATE_JSON, read_hint, remember_project,
    resolve_project, write_hint,
};
pub use progress::add_done_entry;
pub use redirects::{REDIRECTS_YAML, Redirect, append_redirect, read_redirects};
pub use resolve::{Resolution, Resolved, ResolvedKind, Resolver};
pub use staleness::{
    StalenessPlan, apply_staleness, dependents, dependents_of, plan_staleness, restore_staleness,
    stale_ref,
};

/// The project statuses `project.yaml` takes (I4).
pub const PROJECT_STATUSES: &[&str] = &["active", "paused", "archived"];

/// A project's `project.yaml` path.
///
/// # Errors
/// `params.invalid` (with an empty trace id) when `slug` isn't a project slug.
pub fn project_yaml_path(slug: &str) -> Result<WsPath, ErrorBody> {
    project_file(slug, "project.yaml")
}

/// A project's `progress.md` path.
///
/// # Errors
/// `params.invalid` (with an empty trace id) when `slug` isn't a project slug.
pub fn progress_path(slug: &str) -> Result<WsPath, ErrorBody> {
    project_file(slug, "progress.md")
}

fn project_file(slug: &str, name: &str) -> Result<WsPath, ErrorBody> {
    let invalid = || {
        ErrorBody::new(
            &PARAMS_INVALID,
            format!(
                "{slug:?} is not a project slug: lowercase letters, digits and '-', starting \
                 with a letter or digit, at most 63 characters"
            ),
            "",
        )
    };
    if !is_slug(slug) {
        return Err(invalid());
    }
    WsPath::parse(&format!("projects/{slug}/{name}")).map_err(|_| invalid())
}

/// `project.unknown` for `slug`, with an empty trace id.
pub fn unknown_project(slug: &str) -> ErrorBody {
    let mut error = ErrorBody::new(
        &PROJECT_UNKNOWN,
        format!(
            "There is no project `{slug}` in this workspace (no projects/{slug}/project.yaml); \
             project.create makes one."
        ),
        "",
    );
    error.data = Some(serde_json::json!({ "project": slug }));
    error
}

/// What `projects/<slug>/project.yaml` holds.
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectFile {
    Missing,
    Parsed(Box<ProjectYaml>),
    /// The file isn't a YAML mapping: why.
    Unreadable(String),
}

/// Reads `projects/<slug>/project.yaml` (a slug that isn't valid has none).
///
/// # Errors
/// The store's read error.
pub async fn read_project(store: &Store, slug: &str) -> Result<ProjectFile, StoreError> {
    let Ok(rel) = project_yaml_path(slug) else {
        return Ok(ProjectFile::Missing);
    };
    let Some(bytes) = store.read(&rel).await? else {
        return Ok(ProjectFile::Missing);
    };
    let parsed = std::str::from_utf8(&bytes)
        .map_err(|_| format!("{rel} is not UTF-8"))
        .and_then(read_project_yaml);
    Ok(match parsed {
        Ok(p) => ProjectFile::Parsed(Box::new(p)),
        Err(why) => ProjectFile::Unreadable(why),
    })
}

/// The `project.status` result (Global Constraints, Product tools; CORE-038, CORE-055).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ProjectStatus {
    pub project: String,
    /// `active`, `paused` or `archived` (`active` when `project.yaml` has no `status`), or
    /// `unknown` when `project.yaml` doesn't parse (it is then in `needs_repair`).
    pub status: String,
    /// The number of the project's assets in each manifest status; an asset whose manifest
    /// has no status counts as `none`.
    pub assets_by_status: BTreeMap<String, u64>,
    /// Orphans under the project's folder (CORE-038): never deleted, only listed.
    pub orphans: Vec<String>,
    /// The manifests of assets that need repair (one that doesn't parse or disagrees with its
    /// folder, CORE-047; a folder moved or sharing its creation UUID, CORE-154), files in state
    /// `needs_repair`, a `project.yaml` that doesn't parse, the project's redirects that loop
    /// (`registry/redirects.yaml#<project>/<asset_id>`, ARCH-125), and
    /// `registry/redirects.yaml` itself when it can't be read.
    pub needs_repair: Vec<String>,
    /// Files the suite still expects that disappeared outside it (state `missing`).
    pub missing: Vec<String>,
    /// Asset folders moved outside the suite (CORE-154) from or into the project, by `from`:
    /// names `<project>/<asset_id>`. Each stays until `asset.accept_move`.
    pub moved: Vec<MovedEntry>,
    /// Asset folders that share a creation UUID (copies, CORE-154), one group per UUID with a
    /// folder in the project: each group's folders (`projects/<p>/assets/<a>`), sorted.
    pub duplicates: Vec<Vec<String>>,
}

/// One of `project.status`'s `moved`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
pub struct MovedEntry {
    /// Where the manifest says the asset is, `<project>/<asset_id>`.
    pub from: String,
    /// The folder it is in, `<project>/<asset_id>`.
    pub to: String,
}

/// `project.status` of `project` in `ws`. It waits for the workspace's first reconcile or
/// cold rebuild, so a fresh open never answers with partial counts. The errors carry an empty
/// trace id; the caller sets its own.
///
/// # Errors
/// `params.invalid` for a slug that isn't valid; `project.unknown` without a `project.yaml`;
/// `store.io` when the index or a read fails.
pub async fn project_status(ws: &OpenWorkspace, project: &str) -> Result<ProjectStatus, ErrorBody> {
    let yaml_rel = project_yaml_path(project)?;
    let mut needs_repair = Vec::new();
    let status = match read_project(&ws.store, project)
        .await
        .map_err(|e| e.to_error(""))?
    {
        ProjectFile::Missing => return Err(unknown_project(project)),
        ProjectFile::Parsed(p) => p.status.unwrap_or_else(|| "active".to_string()),
        ProjectFile::Unreadable(why) => {
            tracing::warn!(project, why, "project.yaml doesn't parse");
            needs_repair.push(yaml_rel.as_str().to_string());
            "unknown".to_string()
        }
    };
    ws.index.ready_result().await.map_err(|e| e.to_error(""))?;
    let assets = ws
        .index
        .assets_of(project)
        .await
        .map_err(|e| e.to_error(""))?;
    let mut assets_by_status: BTreeMap<String, u64> = BTreeMap::new();
    for asset in &assets {
        let status = asset.status.clone().unwrap_or_else(|| "none".to_string());
        *assets_by_status.entry(status).or_default() += 1;
        if asset.needs_repair {
            needs_repair.push(asset.manifest.as_str().to_string());
        }
    }
    let files = ws
        .index
        .files_in_states(project, &[FileState::NeedsRepair, FileState::Missing])
        .await
        .map_err(|e| e.to_error(""))?;
    let mut missing = Vec::new();
    for file in files {
        let path = file.path.as_str().to_string();
        match file.state {
            FileState::Missing => missing.push(path),
            _ => needs_repair.push(path),
        }
    }
    // Redirects from the project's names that loop (Appendix H); a redirects file that can't
    // be read needs repair itself, and nothing else of the status depends on it.
    match Resolver::load(&ws.index, &ws.store).await {
        Ok(resolver) => {
            if resolver.file_error().is_some() {
                needs_repair.push(REDIRECTS_YAML.to_string());
            }
            needs_repair.extend(resolver.broken_from(project));
        }
        Err(e) => {
            tracing::warn!(project, error = %e.message, "registry/redirects.yaml can't be read");
            needs_repair.push(REDIRECTS_YAML.to_string());
        }
    }
    needs_repair.sort();
    needs_repair.dedup();
    let (moved, duplicates) = moves_and_copies(ws, project).await?;
    // Only the project's own folder is walked, never the whole workspace.
    let orphans = ws
        .store
        .orphans_in(project)
        .await
        .map_err(|e| e.to_error(""))?
        .into_iter()
        .map(|o| o.as_str().to_string())
        .collect();
    Ok(ProjectStatus {
        project: project.to_string(),
        status,
        assets_by_status,
        orphans,
        needs_repair,
        missing,
        moved,
        duplicates,
    })
}

/// `project.status`'s `moved` and `duplicates` (Appendix H "Moved and copied folders"), from
/// the index's `repair` column, workspace-wide (a folder moved from or into another project
/// counts in both).
async fn moves_and_copies(
    ws: &OpenWorkspace,
    project: &str,
) -> Result<(Vec<MovedEntry>, Vec<Vec<String>>), ErrorBody> {
    let rows = ws
        .index
        .repairs(&["moved", "copy", "duplicate_uuid"])
        .await
        .map_err(|e| e.to_error(""))?;
    let mut moved = BTreeSet::new();
    let mut groups: BTreeMap<String, BTreeSet<(String, String)>> = BTreeMap::new();
    for row in &rows {
        match row.repair.as_ref().and_then(RepairKind::from_json) {
            Some(RepairKind::Moved { from, to }) => {
                let in_project =
                    |name: &str| name.split_once('/').is_some_and(|(p, _)| p == project);
                if in_project(&from) || in_project(&to) {
                    moved.insert(MovedEntry { from, to });
                }
            }
            Some(RepairKind::Copy { .. } | RepairKind::DuplicateUuid { .. }) => {
                if let Some(uuid) = uuid_key(row) {
                    groups
                        .entry(uuid)
                        .or_default()
                        .insert((row.project.clone(), row.asset_id.clone()));
                }
            }
            _ => {}
        }
    }
    let duplicates = groups
        .into_values()
        .filter(|g| g.iter().any(|(p, _)| p == project))
        .map(|g| {
            g.into_iter()
                .map(|(p, a)| format!("projects/{p}/assets/{a}"))
                .collect()
        })
        .collect::<BTreeSet<Vec<String>>>()
        .into_iter()
        .collect();
    Ok((moved.into_iter().collect(), duplicates))
}
