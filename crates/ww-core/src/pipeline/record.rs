//! The Record stage's plan and its steps (CORE-015 stage 9, CORE-020, CORE-025; ARCH-075),
//! and the start of a candidate run (CORE-023, CORE-027; I5 "Version fields").
//!
//! A tool that records an operation puts a [`RecordPlan`] in its call's [`RecordSlot`] during
//! Execute; the Record stage (`tools::record_stage::WorkspaceRecord`) takes it once the
//! outcome is `ok` and calls [`apply_record`]:
//! 1. `Store::commit` of the plan's operation, with `progress` = `projects/<project>/progress.md`
//!    and a text edit that adds the plan's `progress_entry` under `## Done` (the only source of
//!    the line: tools leave `commit.progress` as `None`). That is output → manifest → jobs log
//!    → progress → index, including `commit.moved`. The plan's version-lock guard is dropped
//!    when the commit returns, whether it succeeded or not, and the staging handles the
//!    outputs came from are given back (one that still holds files stays as an orphan).
//! 2. For a sealed version: the dependents of the asset (`projects::dependents`, which skips
//!    archived projects) that are `approved`, `exported` or `released`, take the new version
//!    through an input that isn't pinned, and whose input names an older version, go
//!    `MarkStale` (a hash-checked manifest edit), into `made_stale` as refs that carry the
//!    content hash. Dependents in `planned` or `blocked` whose every asset input now names a
//!    sealed version go into `unlocks`. A dependent that can't be read or edited is logged
//!    and skipped: the commit has happened.
//! 3. Events: `asset:<ref>` `{kind: "candidate_run", run_id}` or
//!    `{kind: "version_sealed", version}`, `project:<slug>` `{kind: "progress"}`, and
//!    `{kind: "status", status: "stale"}` on the topic of each asset marked stale.
//!
//! Only an error before the commit's manifest edit is the call's error. Once the manifest
//! records the operation, everything after it (the jobs line, `progress.md`, the index, the
//! events and the dependents) is best-effort (ruling Q31): a failure is logged and returned in
//! [`RecordOutcome::warnings`], which the Record stage puts in the result as `warnings`, and steps 2
//! and 3 always run.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Map, Value, json};
use ww_schema::{Bump, LinkRef, Manifest, ManifestView, Status, StatusEvent, Version, transition};

use crate::events::EventBus;
use crate::graph::is_stale;
use crate::index::Index;
use crate::projects::{add_done_entry, dependents, stale_ref};
use crate::store::{CommitOp, StagingHandle, Store, StoreError, WsPath};

/// Which Record branch a plan takes (CORE-020).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordBranch {
    /// A candidate run of the asset.
    CandidateRun { run_id: String },
    /// A sign-off: the version is sealed already, and the commit records it.
    SealedVersion { version: Version },
}

/// The hand-off block every recorded result carries (CORE-020): what the operation unlocks,
/// what it made stale, and the refs it made.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct HandOff {
    pub unlocks: Vec<String>,
    pub made_stale: Vec<String>,
    pub refs: Vec<String>,
}

/// What [`apply_record`] did: the hand-off block, and what failed after the manifest edit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordOutcome {
    pub handoff: HandOff,
    /// One line per step that failed after the manifest recorded the operation.
    pub warnings: Vec<String>,
}

/// What the Record stage writes for one call (module docs).
pub struct RecordPlan {
    pub branch: RecordBranch,
    pub project: String,
    pub asset_id: String,
    /// The operation; `progress` is `None` (the stage fills it from `progress_entry`).
    pub commit: CommitOp,
    /// The `progress.md` line's text, after `- YYYY-MM-DD: `.
    pub progress_entry: String,
    /// The refs the operation made, for the hand-off block.
    pub refs: Vec<String>,
    /// The guard of the asset's version lock, taken in Execute (before the seal of a sealed
    /// version, before the run id of a candidate run) and dropped once the commit returns, so
    /// no second sign-off runs between the seal and the manifest edit, and no second run takes
    /// the same run id.
    pub version_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl std::fmt::Debug for RecordPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordPlan")
            .field("branch", &self.branch)
            .field("project", &self.project)
            .field("asset_id", &self.asset_id)
            .field("commit", &self.commit)
            .field("progress_entry", &self.progress_entry)
            .field("refs", &self.refs)
            .field("version_guard", &self.version_guard.is_some())
            .finish()
    }
}

/// Where a call's tool leaves its [`RecordPlan`] for the Record stage. At most one plan: a
/// second `set` replaces the first.
#[derive(Default)]
pub struct RecordSlot(Mutex<Option<RecordPlan>>);

impl std::fmt::Debug for RecordSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let held = self
            .0
            .try_lock()
            .map(|p| p.as_ref().map(|p| p.branch.clone()));
        f.debug_tuple("RecordSlot").field(&held.ok()).finish()
    }
}

impl RecordSlot {
    pub fn new() -> RecordSlot {
        RecordSlot::default()
    }

    pub fn set(&self, plan: RecordPlan) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(plan);
    }

    pub fn take(&self) -> Option<RecordPlan> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

/// The live topic of an asset (ARCH-044).
pub fn asset_topic(ws_id: &str, project: &str, asset_id: &str) -> String {
    format!("asset:ww://{ws_id}/{project}/{asset_id}")
}

/// The Record stage's steps 1–3 for `plan` in workspace `ws_id` on the UTC day `today`
/// (module docs).
///
/// # Errors
/// The commit's error before its manifest edit (`store.path_invalid` for a project that isn't
/// a valid path segment); nothing after the manifest edit fails the call (module docs).
pub async fn apply_record(
    store: &Store,
    index: &Index,
    events: &EventBus,
    ws_id: &str,
    today: &str,
    plan: RecordPlan,
) -> Result<RecordOutcome, StoreError> {
    let RecordPlan {
        branch,
        project,
        asset_id,
        mut commit,
        progress_entry,
        refs,
        version_guard,
    } = plan;
    debug_assert!(
        commit.progress.is_none(),
        "a tool leaves commit.progress to the Record stage"
    );
    let handles: BTreeSet<PathBuf> = commit
        .outputs
        .iter()
        .filter_map(|o| o.from.parent().map(PathBuf::from))
        .collect();
    let committed = match WsPath::parse(&format!("projects/{project}/progress.md")) {
        Ok(progress) => {
            let (date, entry, shown) = (
                today.to_string(),
                progress_entry.clone(),
                progress.as_str().to_string(),
            );
            commit.progress = Some((
                progress,
                Arc::new(move |text: &str| {
                    add_done_entry(text, &date, &entry).map_err(|why| StoreError::Io {
                        path: shown.clone(),
                        why,
                    })
                }),
            ));
            store.commit(commit).await
        }
        Err(e) => Err(e),
    };
    drop(version_guard);
    release_handles(store, handles).await;
    let report = committed?;

    let mut handoff = HandOff {
        refs,
        ..HandOff::default()
    };
    let mut warnings = report.warnings;
    let topic = asset_topic(ws_id, &project, &asset_id);
    match &branch {
        RecordBranch::CandidateRun { run_id } => {
            events.publish(&topic, json!({"kind": "candidate_run", "run_id": run_id}));
        }
        RecordBranch::SealedVersion { version } => {
            hand_off_dependents(
                store,
                index,
                events,
                ws_id,
                (&project, &asset_id),
                *version,
                &mut handoff,
                &mut warnings,
            )
            .await;
            events.publish(
                &topic,
                json!({"kind": "version_sealed", "version": version.to_string()}),
            );
        }
    }
    events.publish(&format!("project:{project}"), json!({"kind": "progress"}));
    Ok(RecordOutcome { handoff, warnings })
}

/// Gives back the staging handles in `dirs` (the folders the outputs came from); a handle that
/// still holds files stays as an orphan (logged by the store).
async fn release_handles(store: &Store, dirs: BTreeSet<PathBuf>) {
    for dir in dirs {
        let Some(id) = dir.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        if let Err(e) = store.release_handle(StagingHandle { id, dir }).await {
            tracing::warn!(error = %e, "a staging handle of a recorded call stays");
        }
    }
}

/// The manifest of `projects/<project>/assets/<asset_id>/`, parsed; `None` when it is
/// missing, can't be read or doesn't parse.
async fn read_manifest(store: &Store, project: &str, asset_id: &str) -> Option<Manifest> {
    let rel = manifest_rel(project, asset_id).ok()?;
    let bytes = store.read(&rel).await.ok()??;
    Manifest::parse(std::str::from_utf8(&bytes).ok()?)
        .ok()
        .map(|(m, _)| m)
}

fn manifest_rel(project: &str, asset_id: &str) -> Result<WsPath, StoreError> {
    WsPath::parse(&format!(
        "projects/{project}/assets/{asset_id}/{asset_id}.manifest.json"
    ))
}

/// The asset an input names from `project`, and the version it names; `None` for a library
/// or style input.
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

/// Whether `view` (of an asset in `project`) pins `target`.
fn pins(project: &str, view: &ManifestView, target: &(String, String)) -> bool {
    view.pinned_inputs
        .iter()
        .filter_map(|p| p.r#ref.parse::<LinkRef>().ok())
        .filter_map(|l| target_of(project, &l).map(|(t, _)| t))
        .any(|t| &t == target)
}

/// Step 2 (module docs): marks dependents stale and lists what the new version unlocks. What
/// fails is logged and added to `warnings`.
#[allow(clippy::too_many_arguments)]
async fn hand_off_dependents(
    store: &Store,
    index: &Index,
    events: &EventBus,
    ws_id: &str,
    sealed: (&str, &str),
    version: Version,
    handoff: &mut HandOff,
    warnings: &mut Vec<String>,
) {
    let target = (sealed.0.to_string(), sealed.1.to_string());
    let found = match dependents(store, index, sealed.0, sealed.1).await {
        Ok(found) => found,
        Err(e) => {
            tracing::warn!(project = sealed.0, asset = sealed.1, error = %e, "the dependents of a sealed version can't be listed");
            warnings.push(format!(
                "the dependents of the new version couldn't be listed, so none was marked stale: {e}"
            ));
            return;
        }
    };
    // Approved versions of input targets, read once each.
    let mut approved: HashMap<(String, String), Option<Version>> = HashMap::new();
    approved.insert(target.clone(), Some(version));
    for (project, asset_id) in found {
        let Some(manifest) = read_manifest(store, &project, &asset_id).await else {
            tracing::warn!(
                project,
                asset_id,
                "a dependent's manifest can't be read; it is skipped"
            );
            warnings.push(format!(
                "the dependent {project}/{asset_id} couldn't be read, so it wasn't checked for staleness"
            ));
            continue;
        };
        let view = manifest.view();
        match view.status {
            Some(Status::Approved | Status::Exported | Status::Released) => {
                let stale_through = view.inputs.iter().any(|input| {
                    input
                        .r#ref
                        .parse::<LinkRef>()
                        .ok()
                        .and_then(|l| target_of(&project, &l))
                        .is_some_and(|(t, named)| {
                            t == target
                                && is_stale(&named, Some(&version), pins(&project, &view, &t))
                        })
                });
                if !stale_through {
                    continue;
                }
                match mark_stale(store, &project, &asset_id, sealed.1, version).await {
                    Ok(Some(after)) => {
                        let r = stale_ref(
                            store,
                            ws_id,
                            &project,
                            &asset_id,
                            after.view().approved_version,
                        )
                        .await;
                        handoff.made_stale.push(r);
                        events.publish(
                            &asset_topic(ws_id, &project, &asset_id),
                            json!({"kind": "status", "status": Status::Stale.as_str()}),
                        );
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(project, asset_id, error = %e, "a dependent couldn't be marked stale");
                        warnings.push(format!(
                            "the dependent {project}/{asset_id} couldn't be marked stale: {e}"
                        ));
                    }
                }
            }
            Some(Status::Planned | Status::Blocked) => {
                let mut all_sealed = true;
                for input in &view.inputs {
                    let Some((t, named)) = input
                        .r#ref
                        .parse::<LinkRef>()
                        .ok()
                        .and_then(|l| target_of(&project, &l))
                    else {
                        continue;
                    };
                    let now = match approved.get(&t) {
                        Some(v) => *v,
                        None => {
                            let v = read_manifest(store, &t.0, &t.1)
                                .await
                                .and_then(|m| m.view().approved_version);
                            approved.insert(t.clone(), v);
                            v
                        }
                    };
                    if now.is_none_or(|now| now < named) {
                        all_sealed = false;
                        break;
                    }
                }
                if all_sealed {
                    handoff
                        .unlocks
                        .push(format!("ww://{ws_id}/{project}/{asset_id}"));
                }
            }
            _ => {}
        }
    }
}

/// `MarkStale` on the dependent `project`/`asset_id`, re-checked on the manifest as it is
/// written; `Some(manifest)` when this call marked it.
async fn mark_stale(
    store: &Store,
    project: &str,
    asset_id: &str,
    input: &str,
    version: Version,
) -> Result<Option<Manifest>, StoreError> {
    let rel = manifest_rel(project, asset_id)?;
    let reason = format!("input {input} {version} signed off");
    let marked = Arc::new(AtomicBool::new(false));
    let did = marked.clone();
    let (after, _) = store
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
    Ok(marked.load(Ordering::SeqCst).then_some(after))
}

/// The manifest path a view names (for errors).
fn manifest_rel_of(view: &ManifestView) -> WsPath {
    manifest_rel(&view.project, &view.asset_id)
        .unwrap_or_else(|_| WsPath::parse("manifest.json").expect("a plain name parses"))
}

/// Starts a candidate run on `m` (CORE-023, CORE-027; I5 "Version fields"): appends `run`
/// (its `created` filled from `now_utc` when it has none) and moves the status by
/// `transition(…, CandidateRun)`.
///
/// The run's `target_version` (set in place when `run` has the key, else appended): when the
/// manifest has an `approved_version` and no open run targets a newer one, new work starts,
/// so `version` becomes `approved_version` bumped by `minor` ("set it by the bump rules") and
/// the run targets it; otherwise the run targets the manifest's `version` (`1.0.0` when it
/// has none).
///
/// # Errors
/// `status.invalid_transition` for a `blocked` or `deprecated` asset; `store.hash_conflict`
/// when the manifest has a run with the same `run_id` already (never two runs of one id);
/// `manifest.needs_repair` (logged) when `candidate_runs` isn't a list.
pub fn start_candidate_run(
    m: &mut Manifest,
    mut run: Map<String, Value>,
    now_utc: i64,
) -> Result<(), StoreError> {
    let view = m.view();
    let parse = |key: &str| {
        m.get(key)
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<Status>().ok())
    };
    let moved = transition(
        view.status.unwrap_or(Status::Planned),
        parse("status_before_stale"),
        parse("status_before_blocked"),
        &StatusEvent::CandidateRun,
    )
    .map_err(StoreError::InvalidTransition)?;
    let target = match view.approved_version {
        Some(approved)
            if !view.candidate_runs.iter().any(|r| {
                r.status.as_deref() == Some("open")
                    && r.trashed_utc.is_none()
                    && r.target_version.is_some_and(|t| t > approved)
            }) =>
        {
            let next = approved.bump(Bump::Minor);
            m.set("version", Value::from(next.to_string()));
            next
        }
        _ => view.version.unwrap_or(Version::new(1, 0, 0)),
    };
    let id = run
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if view
        .candidate_runs
        .iter()
        .any(|r| r.run_id.eq_ignore_ascii_case(id))
    {
        return Err(StoreError::HashConflict {
            path: manifest_rel_of(&view),
            backup: None,
            cloud_only: false,
            kept: Vec::new(),
        });
    }
    run.insert("target_version".into(), Value::from(target.to_string()));
    if !run.contains_key("created") {
        run.insert(
            "created".into(),
            Value::from(ww_schema::time::rfc3339(now_utc)),
        );
    }
    m.push("candidate_runs", Value::Object(run))
        .map_err(|e| StoreError::manifest_edit(&manifest_rel_of(&view), &e))?;
    m.set("status", Value::from(moved.to.as_str()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: &str) -> Map<String, Value> {
        let Value::Object(map) = json!({
            "run_id": id, "path": format!("candidates/{id}/"), "status": "open",
            "target_version": null, "files": [], "pinned": false,
        }) else {
            unreachable!()
        };
        map
    }

    #[test] // spec: CORE-023, CORE-027
    fn a_new_asset_targets_its_version_and_becomes_candidate() {
        let mut m = Manifest::new("sketchbook", "prop_kite", "image", "u");
        start_candidate_run(&mut m, run("r_20261016_1200"), 0).unwrap();
        let v = m.view();
        assert_eq!(v.status, Some(Status::Candidate));
        assert_eq!(v.version, Some(Version::new(1, 0, 0)));
        assert_eq!(
            v.candidate_runs[0].target_version,
            Some(Version::new(1, 0, 0))
        );
        // `target_version` kept its place in the run; `created` was added.
        let keys: Vec<&String> = m.as_map()["candidate_runs"][0]
            .as_object()
            .unwrap()
            .keys()
            .collect();
        assert_eq!(keys[3], "target_version");
        assert_eq!(keys.last().unwrap().as_str(), "created");
    }

    #[test] // spec: CORE-027
    fn new_work_after_approval_bumps_minor_once() {
        let mut m = Manifest::new("sketchbook", "prop_kite", "image", "u");
        m.set("approved_version", json!("1.0.0"));
        m.set("status", json!("approved"));
        start_candidate_run(&mut m, run("r_1"), 0).unwrap();
        assert_eq!(m.view().version, Some(Version::new(1, 1, 0)));
        // A second run while the first still targets 1.1.0 doesn't bump again.
        start_candidate_run(&mut m, run("r_2"), 0).unwrap();
        let v = m.view();
        assert_eq!(v.version, Some(Version::new(1, 1, 0)));
        assert_eq!(
            v.candidate_runs[1].target_version,
            Some(Version::new(1, 1, 0))
        );
    }

    #[test] // spec: CORE-047
    fn a_duplicate_run_id_is_refused() {
        let mut m = Manifest::new("sketchbook", "prop_kite", "image", "u");
        start_candidate_run(&mut m, run("r_20261016_1200"), 0).unwrap();
        assert!(matches!(
            start_candidate_run(&mut m, run("R_20261016_1200"), 0),
            Err(StoreError::HashConflict { .. })
        ));
        assert_eq!(m.view().candidate_runs.len(), 1);
    }

    #[test] // spec: CORE-048
    fn a_blocked_asset_takes_no_run() {
        let mut m = Manifest::new("sketchbook", "prop_kite", "image", "u");
        m.set("status", json!("blocked"));
        assert!(matches!(
            start_candidate_run(&mut m, run("r_1"), 0),
            Err(StoreError::InvalidTransition(_))
        ));
        assert!(m.view().candidate_runs.is_empty());
    }
}
