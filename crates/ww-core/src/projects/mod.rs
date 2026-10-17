//! The `projects` module (CORE-014; CORE-039, CORE-048, CORE-055, ARCH-088, ARCH-098): plain
//! functions over [`Store`] and the index that take no `tools` types, so the project tools and
//! the `project:` snapshot provider (Task 13) share them.
//!
//! - [`progress`]: `progress.md` entries under `## Done`;
//! - [`active`]: the per-session active project and the `state.json` hint;
//! - [`staleness`]: the staleness check when a project is restored, and dependents that skip
//!   archived projects (I6);
//! - this file: reading `project.yaml`, and [`project_status`], the `project.status` result.
//!
//! A project exists when its `projects/<slug>/project.yaml` does.

pub mod active;
pub mod progress;
pub mod staleness;

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Serialize;
use ww_protocol::ErrorBody;
use ww_protocol::codes::{PARAMS_INVALID, PROJECT_UNKNOWN};
use ww_schema::is_slug;
use ww_schema::workspace_yaml::{ProjectYaml, read_project_yaml};

use crate::index::FileState;
use crate::store::{Store, StoreError, WsPath};
use crate::workspace::OpenWorkspace;

pub use active::{
    ActiveProjects, DEFAULT_PROJECT, HintWrite, STATE_JSON, read_hint, remember_project,
    resolve_project, write_hint,
};
pub use progress::add_done_entry;
pub use staleness::{dependents, restore_staleness, stale_ref};

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
    /// Manifests that don't parse or disagree with their folder, files in state
    /// `needs_repair`, and a `project.yaml` that doesn't parse.
    pub needs_repair: Vec<String>,
    /// Files the suite still expects that disappeared outside it (state `missing`).
    pub missing: Vec<String>,
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
    needs_repair.sort();
    needs_repair.dedup();
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
    })
}
