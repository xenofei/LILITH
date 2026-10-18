//! The write order (ARCH-075; Appendix E "Write order"), the orphan list (CORE-038) and run
//! ids (CORE-047 name rules).
//!
//! [`Store::commit`] writes in this order: output files → manifest → jobs log line →
//! `progress.md` entry → index (the [`ChangeSink`](super::ChangeSink)); events go out after
//! it. The fail points `store.after_output`, `store.after_manifest`, `store.after_jobs_log`,
//! `store.after_progress` and `store.after_index` sit between the steps. An error after the
//! outputs and before the manifest edit leaves them where they are: they become orphans,
//! listed by [`Store::orphans`], and nothing is ever deleted.
//!
//! Once the manifest edit has succeeded the operation is recorded (ruling Q31): the jobs log
//! line, the `progress.md` entry and the fail points after the manifest are best-effort. A
//! failure there is logged and returned in [`CommitReport::warnings`], never as the commit's
//! error, and the index step always runs.

use std::collections::{BTreeSet, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lil_platform::fs::{Probe, RetryClass, SystemProbe, classify};
use lil_platform::names::{CONFLICT_MARKER, LEGACY_CONFLICT_MARKER, is_workspace_dir};
use lil_schema::{Manifest, ManifestView, Version};
use serde_json::{Map, Value};

use super::fsops;
use super::git_gate::GIT_WAIT;
use super::logs::LogKind;
use super::paths::{WsPath, path_key, writable};
use super::write::{ChangeKind, Seen, Store, StoreError, StoreEvent, Written, blocking, event};

/// An output to ingest: a file in a staging handle, moved (renamed, same volume) to `to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputMove {
    /// Inside a staging handle.
    pub from: PathBuf,
    pub to: WsPath,
}

/// A manifest edit run by [`Store::commit`] on the manifest's ordered map.
pub type ManifestEdit = Arc<dyn Fn(&mut Manifest) -> Result<(), StoreError> + Send + Sync>;
/// A text edit (`progress.md`) run by [`Store::commit`].
pub type TextEdit = Arc<dyn Fn(&str) -> Result<String, StoreError> + Send + Sync>;

/// One operation's writes, in the ARCH-075 order.
#[derive(Clone, Default)]
pub struct CommitOp {
    pub outputs: Vec<OutputMove>,
    /// Files a caller already moved into place (a sealed version): they reach the index in
    /// the index step, with the hash and identity given, never before the manifest edit.
    pub moved: Vec<Written>,
    /// Paths a caller already moved away (the run files a seal took out of
    /// `candidates/<run>/`): they reach the index as removals in the index step, after the
    /// manifest edit.
    pub removed: Vec<WsPath>,
    pub manifest: Option<(WsPath, ManifestEdit)>,
    pub job_line: Option<Map<String, Value>>,
    pub progress: Option<(WsPath, TextEdit)>,
}

impl std::fmt::Debug for CommitOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommitOp")
            .field("outputs", &self.outputs)
            .field("moved", &self.moved)
            .field("removed", &self.removed)
            .field("manifest", &self.manifest.as_ref().map(|(p, _)| p))
            .field("job_line", &self.job_line)
            .field("progress", &self.progress.as_ref().map(|(p, _)| p))
            .finish()
    }
}

/// What [`Store::commit`] wrote.
#[derive(Debug, Clone)]
pub struct CommitReport {
    /// Each output, with its identity after the move (on ReFS a move across folders changes
    /// the file id, so the index gets the post-move identity; CORE-041).
    pub outputs: Vec<Written>,
    /// The manifest as written.
    pub manifest: Option<Manifest>,
    /// The `seq` of the jobs log line.
    pub job_seq: Option<u64>,
    /// What failed after the manifest edit (the jobs log line, the `progress.md` entry, a fail
    /// point): the operation is recorded all the same (ruling Q31).
    pub warnings: Vec<String>,
}

/// A folder entry: its name, whether it is a folder (not a link), and the path.
pub(super) struct Listed {
    pub(super) name: String,
    pub(super) folder: bool,
    pub(super) path: PathBuf,
}

/// The entries of `dir` (none when it doesn't exist). Links are listed as non-folders, so
/// nothing is ever followed through one.
pub(super) fn list(dir: &Path) -> io::Result<Vec<Listed>> {
    list_with(&SystemProbe, dir)
}

/// [`list`], with the folder listed through `probe`.
pub(super) fn list_with(probe: &dyn Probe, dir: &Path) -> io::Result<Vec<Listed>> {
    let entries = match probe.read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry?;
        let kind = entry.file_type()?;
        let name = entry.file_name().to_string_lossy().into_owned();
        found.push(Listed {
            folder: kind.is_dir() && !kind.is_symlink(),
            path: dir.join(&name),
            name,
        });
    }
    Ok(found)
}

/// `v<major.minor.patch>` (the `v` in either case) → the version.
pub(super) fn version_folder(name: &str) -> Option<Version> {
    name.strip_prefix(['v', 'V'])?.parse().ok()
}

/// Every conflict copy (`*.lil-conflict-*`, or one with the legacy [`LEGACY_CONFLICT_MARKER`])
/// under `start` (the workspace `root`, or a folder in it), skipping `.git`, and the root's
/// `.trash` and both workspace data folders ([`is_workspace_dir`]), and never following a
/// link. A `start` that doesn't exist holds none. Folders are listed through `probe`.
fn conflict_files(root: &Path, start: &Path, probe: &dyn Probe) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut folders = vec![start.to_path_buf()];
    while let Some(dir) = folders.pop() {
        let entries = match list_with(probe, &dir) {
            Ok(entries) => entries,
            Err(e) if dir == start => return Err(e),
            Err(e) => {
                tracing::warn!(dir = %dir.display(), error = %e, "the orphan scan can't list a folder");
                continue;
            }
        };
        for entry in entries {
            if entry.folder {
                let at_root = dir == root;
                let skip = entry.name.eq_ignore_ascii_case(".git")
                    || (at_root
                        && (entry.name.eq_ignore_ascii_case(".trash")
                            || is_workspace_dir(&entry.name)));
                if !skip {
                    folders.push(entry.path);
                }
            } else if is_conflict_copy(&entry.name) {
                found.push(entry.path);
            }
        }
    }
    Ok(found)
}

/// Whether `name` is a conflict copy's (`*.lil-conflict-*`, or the legacy marker).
pub(super) fn is_conflict_copy(name: &str) -> bool {
    name.contains(CONFLICT_MARKER) || name.contains(LEGACY_CONFLICT_MARKER)
}

impl Store {
    /// Runs the fail point `name` between two commit steps; its error stops the commit.
    async fn point(&self, name: &'static str, subject: &WsPath) -> Result<(), StoreError> {
        self.fs(Some(name), || Ok(()))
            .await
            .map_err(|e| StoreError::io(subject, &e))
    }

    /// Writes `op` in the ARCH-075 order:
    /// 1. after waiting at the git gate (ARCH-083), each output is moved from its staging
    ///    handle with `move_new` (never over a file), then hashed, and its identity taken
    ///    after the move;
    /// 2. the manifest edit (hash-checked, `Store::edit_manifest`);
    /// 3. the jobs log line;
    /// 4. the `progress.md` edit;
    /// 5. the index: one `applied` call with the changes of 1–4, then one `Removed` event per
    ///    entry of `op.removed` and one `Written` event per entry of `op.moved`, with its hash
    ///    and identity as given.
    ///
    /// Steps 3–5 and the fail points after step 2 are best-effort (ruling Q31): a failure is
    /// logged and listed in [`CommitReport::warnings`], the later steps still run, and step 5
    /// always runs.
    ///
    /// # Errors
    /// `workspace.read_only` on a read-only store; `store.git_busy` when git is still working
    /// after 60 s; `store.path_invalid` for an output that isn't in a staging handle, a target
    /// the store never writes, or one under a linked folder; `store.hash_conflict` when a file
    /// is already at an output's target; any error of steps 1 and 2. An error after step 1
    /// leaves the outputs in place (orphans until the manifest records them).
    pub async fn commit(&self, op: CommitOp) -> Result<CommitReport, StoreError> {
        self.writable_store()?;
        let subject = op
            .manifest
            .as_ref()
            .map(|(p, _)| p.clone())
            .or_else(|| op.outputs.first().map(|o| o.to.clone()))
            .unwrap_or_else(|| WsPath::from_disk(String::new()));
        let mut events: Vec<StoreEvent> = Vec::new();

        // 1. Outputs. They wait at the git gate first, as every write does: a run folder
        // never appears in the middle of a checkout.
        if !op.outputs.is_empty() {
            self.git_gate().wait_clear(GIT_WAIT).await?;
        }
        let staging = self.staging_root();
        let mut outputs = Vec::with_capacity(op.outputs.len());
        for out in &op.outputs {
            if !out.from.starts_with(&staging) || out.from == staging {
                return Err(StoreError::PathInvalid {
                    path: out.from.display().to_string(),
                    why: "an output is ingested only from a staging handle".to_string(),
                });
            }
            writable(&out.to)?;
            self.writable_on_disk(&out.to).await?;
            // The move checks only its target: a linked folder above it is refused here.
            self.refuse_links(&out.to).await?;
            let _held = self.lock_path(&out.to).await;
            let target = out.to.abs(self.root());
            let (from, to) = (out.from.clone(), target.clone());
            self.fs_retry(None, move || {
                if let Some(dir) = to.parent() {
                    fsops::ensure_dir(dir)?;
                }
                fsops::move_new(&from, &to)
            })
            .await
            .map_err(|e| match classify(&e) {
                RetryClass::TargetExists => StoreError::HashConflict {
                    path: out.to.clone(),
                    backup: None,
                    cloud_only: false,
                    kept: Vec::new(),
                },
                _ => StoreError::io(&out.to, &e),
            })?;
            let sha256 = match self.see_hash(&target).await {
                Ok(Seen::Found(h)) => h.sha256,
                Ok(_) => {
                    return Err(StoreError::Io {
                        path: out.to.as_str().to_string(),
                        why: "the output is no longer there after its move".to_string(),
                    });
                }
                Err(e) => return Err(StoreError::io(&out.to, &e)),
            };
            let written = Written {
                path: out.to.clone(),
                sha256,
                identity: self.identity(&out.to).await?,
            };
            events.push(event(&written, ChangeKind::Created));
            outputs.push(written);
        }
        self.point("store.after_output", &subject).await?;

        // 2. Manifest.
        let mut manifest = None;
        if let Some((rel, edit)) = &op.manifest {
            let (edited, written, kind) = self.edit_manifest_quiet(rel, &**edit).await?;
            if let Some(kind) = kind {
                events.push(event(&written, kind));
            }
            manifest = Some(edited);
        }
        // From here on the operation is recorded: what fails is a warning (ruling Q31).
        let mut warnings = Vec::new();
        let mut warn = |step: &str, e: &StoreError| {
            tracing::warn!(manifest = %subject, step, error = %e, "a step after the manifest edit failed; the operation is recorded");
            warnings.push(format!(
                "{step} failed after the manifest recorded the operation: {e}"
            ));
        };
        if let Err(e) = self.point("store.after_manifest", &subject).await {
            warn("store.after_manifest", &e);
        }

        // 3. The jobs log line.
        let mut job_seq = None;
        if let Some(record) = op.job_line.clone() {
            match self.append_log_at(LogKind::Jobs, record).await {
                Ok((seq, rel)) => {
                    job_seq = Some(seq);
                    // Without an identity the index reconciles the log itself.
                    let identity = self.identity(&rel).await.ok();
                    events.push(StoreEvent {
                        identity,
                        path: rel,
                        kind: ChangeKind::Written,
                        sha256: None,
                    });
                }
                Err(e) => warn("the jobs log line", &e),
            }
        }
        if let Err(e) = self.point("store.after_jobs_log", &subject).await {
            warn("store.after_jobs_log", &e);
        }

        // 4. progress.md.
        if let Some((rel, edit)) = &op.progress {
            match self.edit_text_quiet(rel, &**edit).await {
                Ok((written, Some(kind))) => events.push(event(&written, kind)),
                Ok((_, None)) => {}
                Err(e) => warn("the progress.md entry", &e),
            }
        }
        if let Err(e) = self.point("store.after_progress", &subject).await {
            warn("store.after_progress", &e);
        }

        // 5. The index, with the files already moved away and into place.
        events.extend(op.removed.iter().map(|path| StoreEvent {
            path: path.clone(),
            kind: ChangeKind::Removed,
            sha256: None,
            identity: None,
        }));
        events.extend(op.moved.iter().map(|w| event(w, ChangeKind::Written)));
        self.emit(events).await;
        if let Err(e) = self.point("store.after_index", &subject).await {
            warn("store.after_index", &e);
        }

        Ok(CommitReport {
            outputs,
            manifest,
            job_seq,
            warnings,
        })
    }

    /// What a crash or a failed call left without a record (CORE-038), sorted. Orphans are
    /// listed, never deleted:
    /// - `candidates/<run>/` folders that their asset's manifest has no run for (an asset with
    ///   no manifest records none; one whose manifest doesn't parse is skipped);
    /// - `v<semver>/` folders that hold a `manifest.snapshot.json` but whose version is
    ///   neither the manifest's `approved_version` nor a run's `signed_off.version`;
    /// - non-empty staging handles and `seal-*` folders that aren't this store's own (a dead
    ///   core's, at a read-write open);
    /// - conflict copies, `*.lil-conflict-*` and legacy ones.
    ///
    /// # Errors
    /// `store.io` when the workspace can't be listed.
    pub async fn orphans(&self) -> Result<Vec<WsPath>, StoreError> {
        let conflicts = {
            let (root, probe) = (self.root().to_path_buf(), self.probe().clone());
            blocking(move || conflict_files(&root, &root, probe.as_ref()))
                .await
                .map_err(|e| StoreError::io(&WsPath::from_disk(String::new()), &e))?
        };
        let conflicts = conflicts.iter().map(|path| self.rel_of(path)).collect();
        self.orphans_with_conflicts(conflicts).await
    }

    /// [`Store::orphans`], with the conflict copies already found (by the open's recovery walk,
    /// [`RecoveryReport::conflict_copies`](super::RecoveryReport::conflict_copies)), so the
    /// whole tree isn't walked again.
    ///
    /// # Errors
    /// `store.io` when the workspace can't be listed.
    pub async fn orphans_with_conflicts(
        &self,
        conflicts: Vec<WsPath>,
    ) -> Result<Vec<WsPath>, StoreError> {
        let mut found: BTreeSet<WsPath> = BTreeSet::new();
        let root = self.root().to_path_buf();
        let root_rel = WsPath::from_disk(String::new());
        let io = |e: io::Error| StoreError::io(&root_rel, &e);

        let projects = {
            let dir = root.join("projects");
            blocking(move || list(&dir)).await.map_err(io)?
        };
        for project in projects.into_iter().filter(|p| p.folder) {
            self.project_asset_orphans(&project.name, &project.path, &mut found)
                .await?;
        }
        found.extend(self.staging_orphans().await?);
        found.extend(conflicts);
        Ok(found.into_iter().collect())
    }

    /// [`Store::orphans`] of one project, sorted: the orphan run folders and sealed versions of
    /// `projects/<project>/assets/*`, and the `*.lil-conflict-*` files under
    /// `projects/<project>/`. Only that folder is walked (`project.status`); staging handles
    /// and conflict files elsewhere are not the project's. A project folder that doesn't exist
    /// has none.
    ///
    /// # Errors
    /// `store.path_invalid` for a project name that isn't a valid path segment; `store.io`
    /// when the project's folder can't be listed.
    pub async fn orphans_in(&self, project: &str) -> Result<Vec<WsPath>, StoreError> {
        let rel = WsPath::parse(&format!("projects/{project}"))?;
        if rel.as_str().matches('/').count() != 1 {
            return Err(StoreError::PathInvalid {
                path: rel.as_str().to_string(),
                why: "a project name is one path segment".to_string(),
            });
        }
        let mut found: BTreeSet<WsPath> = BTreeSet::new();
        let dir = rel.abs(self.root());
        self.project_asset_orphans(project, &dir, &mut found)
            .await?;
        let conflicts = {
            let (root, start, probe) = (self.root().to_path_buf(), dir, self.probe().clone());
            blocking(move || conflict_files(&root, &start, probe.as_ref()))
                .await
                .map_err(|e| StoreError::io(&rel, &e))?
        };
        found.extend(conflicts.iter().map(|path| self.rel_of(path)));
        Ok(found.into_iter().collect())
    }

    /// The orphans of every asset folder of the project folder `dir` (named `project`).
    async fn project_asset_orphans(
        &self,
        project: &str,
        dir: &Path,
        found: &mut BTreeSet<WsPath>,
    ) -> Result<(), StoreError> {
        let root_rel = WsPath::from_disk(String::new());
        let assets = {
            let dir = dir.join("assets");
            blocking(move || list(&dir))
                .await
                .map_err(|e| StoreError::io(&root_rel, &e))?
        };
        for asset in assets.into_iter().filter(|a| a.folder) {
            let asset_rel = format!("projects/{project}/assets/{}", asset.name);
            self.asset_orphans(&asset_rel, &asset.name, &asset.path, found)
                .await?;
        }
        Ok(())
    }

    /// The orphan run folders and sealed versions of one asset folder.
    async fn asset_orphans(
        &self,
        asset_rel: &str,
        asset_id: &str,
        asset_dir: &Path,
        found: &mut BTreeSet<WsPath>,
    ) -> Result<(), StoreError> {
        let manifest_rel = WsPath::from_disk(format!("{asset_rel}/{asset_id}.manifest.json"));
        let view = match self.read(&manifest_rel).await {
            Ok(None) => ManifestView::default(),
            Ok(Some(bytes)) => {
                match std::str::from_utf8(&bytes)
                    .ok()
                    .and_then(|t| Manifest::parse(t).ok())
                {
                    Some((manifest, _)) => manifest.view(),
                    None => {
                        tracing::warn!(manifest = %manifest_rel, "a manifest that doesn't parse; its folders aren't checked for orphans");
                        return Ok(());
                    }
                }
            }
            Err(e) => {
                tracing::warn!(manifest = %manifest_rel, error = %e, "a manifest can't be read; its folders aren't checked for orphans");
                return Ok(());
            }
        };
        let io = |e: io::Error| StoreError::io(&manifest_rel, &e);
        let runs: HashSet<String> = view
            .candidate_runs
            .iter()
            .map(|r| path_key(&r.run_id))
            .collect();
        let candidates = {
            let dir = asset_dir.join("candidates");
            blocking(move || list(&dir)).await.map_err(io)?
        };
        for run in candidates.into_iter().filter(|c| c.folder) {
            if !runs.contains(&path_key(&run.name)) {
                found.insert(WsPath::from_disk(format!(
                    "{asset_rel}/candidates/{}",
                    run.name
                )));
            }
        }

        let mut recorded: Vec<Version> = view.approved_version.into_iter().collect();
        recorded.extend(
            view.candidate_runs
                .iter()
                .filter_map(|r| r.signed_off.as_ref().map(|s| s.version)),
        );
        let folders = {
            let dir = asset_dir.to_path_buf();
            blocking(move || list(&dir)).await.map_err(io)?
        };
        for folder in folders.into_iter().filter(|f| f.folder) {
            let Some(version) = version_folder(&folder.name) else {
                continue;
            };
            let snapshot = folder.path.join("manifest.snapshot.json");
            let sealed = blocking(move || Ok(lil_platform::fs::attributes(&snapshot).is_ok()))
                .await
                .unwrap_or(false);
            if sealed && !recorded.contains(&version) {
                found.insert(WsPath::from_disk(format!("{asset_rel}/{}", folder.name)));
            }
        }
        Ok(())
    }

    /// A new run id for the asset folder `asset_dir` at `now_utc` (Unix seconds):
    /// `r_<yyyymmdd>_<hhmm>` (UTC), or with the smallest `_<n>` (from 2) that is free among
    /// both `m`'s runs and the existing `candidates/*` folders, so a retry never collides
    /// with an orphan's folder. Names compare without regard to case.
    ///
    /// # Errors
    /// `store.io` when the `candidates` folder can't be listed.
    pub async fn next_run_id(
        &self,
        asset_dir: &WsPath,
        m: Option<&Manifest>,
        now_utc: i64,
    ) -> Result<String, StoreError> {
        let base = lil_schema::time::run_id(now_utc);
        let mut taken: HashSet<String> = m
            .map(|m| {
                m.view()
                    .candidate_runs
                    .iter()
                    .map(|r| path_key(&r.run_id))
                    .collect()
            })
            .unwrap_or_default();
        let dir = asset_dir.abs(self.root()).join("candidates");
        let folders = blocking(move || list(&dir))
            .await
            .map_err(|e| StoreError::io(asset_dir, &e))?;
        taken.extend(folders.iter().map(|f| path_key(&f.name)));
        if !taken.contains(&path_key(&base)) {
            return Ok(base);
        }
        let free = (2_u64..)
            .map(|n| format!("{base}_{n}"))
            .find(|id| !taken.contains(&path_key(id)))
            .expect("some suffix is free");
        Ok(free)
    }
}
