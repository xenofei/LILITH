//! Sealed versions (CORE-026, CORE-028, CORE-033, CORE-041; I5 "Sign-off"): the per-asset
//! version lock, [`Store::seal`] and [`Store::versions`].
//!
//! A sign-off (Task 13) takes the asset's [`Store::version_lock`] with `lock_owned()` and
//! keeps the guard from the seal to the end of its Record stage, so two sign-offs of one asset
//! never allocate the same version and the watcher can defer removals under a sealing asset.
//!
//! [`Store::seal`]:
//! 1. refuses an existing `v<version>/` (`version.exists`);
//! 2. creates `<ws>\.worldwright\staging\seal-<sh_…>\`, a live staging entry while it works;
//! 3. moves the chosen files and the files derived from them (`role: derived`, following
//!    `from` through the run) from `candidates/<run>/`, copying instead a file that another
//!    run file was derived from (I5), and moves `source/` if the run has one;
//! 4. writes `manifest.snapshot.json` (`snapshot.to_text()`);
//! 5. sets the read-only attribute on every file;
//! 6. moves the folder to `projects/<p>/assets/<a>/v<version>/` with `move_new`;
//! 7. hashes every sealed file, records each hash as the file's generated hash, takes the
//!    identities after the move (the read-only bit moves ChangeTime, and a ReFS move across
//!    folders changes the file id), and computes the content hash;
//! 8. hits the fail point `store.after_seal`.
//!
//! Every copy and move hits the fail point `store.replace` and retries sharing violations
//! under the store's budget (`store.busy` after it). On an error before step 6 finishes, each
//! moved file goes back to the run (writable again), each copy in the sealing folder is
//! removed only if it still holds the original's bytes (the store's `remove_if_hash`), the
//! snapshot likewise, and the emptied folder is removed; anything that can't go back stays in
//! the sealing folder, which is then an orphan. A half-filled `v<version>/` never exists.
//! Those removals are hard deletes of the core's own just-written duplicates (CORE-110; the
//! `crates/ww-core/tests/trash.rs` scan allows them here).
//!
//! The caller passes its guard of the asset's version lock, and a seal under any other lock
//! is refused.
//!
//! The sealed files reach the index through the Record stage's `commit.moved`, after the
//! manifest edit (ARCH-075), never from `seal`. A crash after step 6 and before that edit
//! leaves a `v<version>/` with a snapshot that no manifest records: `orphans()` lists it.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use tokio::sync::OwnedMutexGuard;

use ww_platform::fs::{FileIdentity, Probe, RetryClass, classify};
use ww_schema::{
    CandidateFile, Manifest, ManifestView, Version, bare_sha256, sha256_hex, version_content_hash,
};

use super::fsops;
use super::order::{list, version_folder};
use super::paths::{WsPath, path_key};
use super::staging::SEAL_PREFIX;
use super::write::{
    PathLock, Seen, Store, StoreError, blocking, exists, link_refused, lock, remove_if_hash, see,
};

/// The snapshot every sealed version holds (I5 sign-off step 4).
pub const SNAPSHOT: &str = "manifest.snapshot.json";

/// What [`Store::seal`] seals.
#[derive(Debug, Clone)]
pub struct SealRequest {
    pub project: String,
    pub asset_id: String,
    pub version: Version,
    pub run_id: String,
    /// The chosen file names in `candidates/<run_id>/`.
    pub files: Vec<String>,
    /// The manifest as it stands after sign-off: written as `manifest.snapshot.json`, and the
    /// run's file records (`role`, `from`) say which files are derived.
    pub snapshot: Manifest,
}

/// A sealed version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    /// `projects/<p>/assets/<a>/v<version>`.
    pub dir: WsPath,
    /// Every sealed file except the snapshot, sorted: (path relative to the asset folder,
    /// `v<version>/<file>`, bare SHA-256 hex). `source/**` files are listed too; the content
    /// hash leaves them out.
    pub files: Vec<(String, String)>,
    pub content_hash: String,
    /// Every file in the version (the snapshot included), with its identity taken after the
    /// final move.
    pub identities: Vec<(WsPath, FileIdentity)>,
}

/// A `v<version>/` folder of an asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionDir {
    pub version: Version,
    pub dir: WsPath,
    /// No `manifest.snapshot.json` and no run signed off as this version (CORE-033): read as
    /// it is, never moved.
    pub legacy: bool,
    pub content_hash: String,
}

/// One file or folder to put in the sealing folder.
#[derive(Debug, Clone)]
struct Item {
    /// The name in the run folder (and in the version).
    name: String,
    from: PathBuf,
    to: PathBuf,
    copy: bool,
}

/// What the seal did in the sealing folder, undone on an error.
#[derive(Debug, Clone)]
enum Done {
    /// Moved from `run` to `sealing`.
    Moved { sealing: PathBuf, run: PathBuf },
    /// A file the seal made in the sealing folder (a copy, the snapshot) with these bytes.
    Made { path: PathBuf, sha256: String },
}

/// Every file under `dir` (links are skipped, never followed), as (`/`-separated path
/// relative to `dir`, absolute path), sorted by the relative path in byte order. A missing
/// `dir` has none.
pub(super) fn files_under(dir: &Path) -> io::Result<Vec<(String, PathBuf)>> {
    let mut found = Vec::new();
    let mut folders = vec![(String::new(), dir.to_path_buf())];
    while let Some((prefix, folder)) = folders.pop() {
        let entries = match std::fs::read_dir(ww_platform::fs::long_path(&folder)) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for entry in entries {
            let entry = entry?;
            let kind = entry.file_type()?;
            let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
            let path = folder.join(entry.file_name());
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                folders.push((format!("{name}/"), path));
            } else {
                found.push((name, path));
            }
        }
    }
    found.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    Ok(found)
}

/// The first link or cloud-only file under `dir`, found through `probe` (attributes and
/// reparse tags only, never following a link or reading a placeholder): its `/`-separated path
/// relative to `dir`, and `true` for a link.
fn unsafe_entry(probe: &dyn Probe, dir: &Path) -> io::Result<Option<(String, bool)>> {
    let mut folders = vec![(String::new(), dir.to_path_buf())];
    while let Some((prefix, folder)) = folders.pop() {
        for entry in std::fs::read_dir(ww_platform::fs::long_path(&folder))? {
            let entry = entry?;
            let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
            let path = folder.join(entry.file_name());
            match see(probe, &path, |_| Ok(()))? {
                Seen::Link => return Ok(Some((name, true))),
                Seen::Placeholder => return Ok(Some((name, false))),
                Seen::Folder => folders.push((format!("{name}/"), path)),
                Seen::Found(()) | Seen::Missing => {}
            }
        }
    }
    Ok(None)
}

/// Sets (or clears) the read-only attribute of `path`, and of every file under it when it is
/// a folder.
pub(super) fn set_readonly_all(path: &Path, on: bool) -> io::Result<()> {
    let attrs = ww_platform::fs::attributes(path)?;
    if attrs & 0x10 == 0 {
        return fsops::set_readonly(path, on);
    }
    for (_, file) in files_under(path)? {
        fsops::set_readonly(&file, on)?;
    }
    Ok(())
}

/// The bare name a run file record or a `from` value names (`candidates/<run>/x.png` and
/// `x.png` both name `x.png`).
fn bare_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The files of a sealed run: the chosen ones, then (transitively) every `derived` file made
/// from one of them. Each is copied when any run file names it in `from`.
fn plan_names(chosen: &[String], records: &[CandidateFile]) -> Vec<(String, bool)> {
    let key = |name: &str| path_key(name);
    let mut included: BTreeMap<String, String> =
        chosen.iter().map(|n| (key(n), n.clone())).collect();
    loop {
        let more: Vec<String> = records
            .iter()
            .filter(|r| r.role == "derived")
            .filter(|r| {
                r.from
                    .as_deref()
                    .is_some_and(|from| included.contains_key(&key(bare_name(from))))
            })
            .map(|r| bare_name(&r.path).to_string())
            .filter(|name| !included.contains_key(&key(name)))
            .collect();
        if more.is_empty() {
            break;
        }
        for name in more {
            included.insert(key(&name), name);
        }
    }
    let sources: BTreeSet<String> = records
        .iter()
        .filter_map(|r| r.from.as_deref())
        .map(|from| key(bare_name(from)))
        .collect();
    let mut names: Vec<(String, bool)> = included
        .into_iter()
        .map(|(k, name)| (name, sources.contains(&k)))
        .collect();
    names.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    names
}

impl Store {
    /// The version lock of an asset: one `tokio::sync::Mutex` per `<project>/<asset_id>`
    /// (compared without regard to case), shared by every caller while any holds it. A
    /// sign-off takes `lock_owned()` before [`Store::seal`] and keeps the guard until its
    /// Record stage ends (Task 13); maintenance takes it before it moves an asset's rejected
    /// runs.
    pub fn version_lock(&self, project: &str, asset_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = lock(&self.version_locks);
        locks.retain(|_, w| w.strong_count() > 0);
        let key = path_key(&format!("{project}/{asset_id}"));
        if let Some(held) = locks.get(&key).and_then(Weak::upgrade) {
            return held;
        }
        let held = Arc::new(PathLock::new(()));
        locks.insert(key, Arc::downgrade(&held));
        held
    }

    /// Seals run `req.run_id`'s chosen files as `v<req.version>/` (module docs). `guard` is
    /// the caller's guard of the asset's [`Store::version_lock`]: a seal under any other lock
    /// (or another asset's) is refused, so no seal runs without the asset's version lock.
    ///
    /// # Errors
    /// `workspace.read_only`; `store.io` when `guard` isn't the asset's version lock;
    /// `store.path_invalid` for names that aren't single path components, or a link (also one
    /// inside `source/`); `version.exists`; `store.hash_conflict` for a cloud-only file (it
    /// would have to be read); `store.io` for a chosen file that isn't in the run, or a failed
    /// file call (`store.busy` when a file stays held, `store.disk_full`); and any error after
    /// the final move, which leaves the sealed folder for the orphan list.
    pub async fn seal(
        &self,
        guard: &OwnedMutexGuard<()>,
        req: SealRequest,
    ) -> Result<Sealed, StoreError> {
        self.writable_store()?;
        let asset = WsPath::parse(&format!("projects/{}/assets/{}", req.project, req.asset_id))?;
        if !Arc::ptr_eq(
            OwnedMutexGuard::mutex(guard),
            &self.version_lock(&req.project, &req.asset_id),
        ) {
            return Err(StoreError::Io {
                path: asset.as_str().to_string(),
                why: "a seal runs only under the asset's own version lock".to_string(),
            });
        }
        let single = |name: &str| {
            if name.is_empty() || name.contains('/') {
                return Err(StoreError::PathInvalid {
                    path: name.to_string(),
                    why: "a run or file name is one path component".to_string(),
                });
            }
            Ok(())
        };
        single(&req.run_id)?;
        let run = asset.join(&format!("candidates/{}", req.run_id))?;
        let mut chosen: Vec<String> = Vec::new();
        for name in &req.files {
            single(name)?;
            run.join(name)?;
            if !chosen.iter().any(|c| path_key(c) == path_key(name)) {
                chosen.push(name.clone());
            }
        }
        let folder_name = format!("v{}", req.version);
        let version_rel = asset.join(&folder_name)?;
        let version_abs = version_rel.abs(self.root());
        // 1.
        if self.exists_now(&version_abs).await {
            return Err(StoreError::VersionExists(req.version.to_string()));
        }

        // The plan: every name is checked before anything moves.
        let records: Vec<CandidateFile> = req
            .snapshot
            .view()
            .candidate_runs
            .into_iter()
            .find(|r| path_key(&r.run_id) == path_key(&req.run_id))
            .map(|r| r.files)
            .unwrap_or_default();
        let run_abs = run.abs(self.root());
        let mut planned: Vec<(String, bool)> = Vec::new();
        for (name, copy) in plan_names(&chosen, &records) {
            let rel = run.join(&name)?;
            let is_chosen = chosen.iter().any(|c| path_key(c) == path_key(&name));
            match self
                .see_hash(&rel.abs(self.root()))
                .await
                .map_err(|e| StoreError::io(&rel, &e))?
            {
                Seen::Found(_) => planned.push((name, copy)),
                Seen::Placeholder => return Err(StoreError::cloud_only(&rel)),
                Seen::Link => return Err(link_refused(&rel)),
                Seen::Missing | Seen::Folder if is_chosen => {
                    return Err(StoreError::Io {
                        path: rel.as_str().to_string(),
                        why: format!("run {} has no file {name} to sign off", req.run_id),
                    });
                }
                Seen::Missing | Seen::Folder => {
                    tracing::warn!(path = %rel, "a derived file of the run isn't there; it isn't sealed");
                }
            }
        }
        let source = run.join("source")?;
        let has_source = match self
            .see_hash(&source.abs(self.root()))
            .await
            .map_err(|e| StoreError::io(&source, &e))?
        {
            Seen::Folder => true,
            Seen::Link => return Err(link_refused(&source)),
            _ => false,
        };
        if has_source {
            // Nothing inside `source/` is a link (never moved through or sealed) or a
            // cloud-only file (step 7 would have to read it).
            let (probe, dir) = (self.probe().clone(), source.abs(self.root()));
            let found = blocking(move || unsafe_entry(probe.as_ref(), &dir))
                .await
                .map_err(|e| StoreError::io(&source, &e))?;
            if let Some((inside, link)) = found {
                let rel = WsPath::from_disk(format!("{source}/{inside}"));
                return Err(if link {
                    link_refused(&rel)
                } else {
                    StoreError::cloud_only(&rel)
                });
            }
        }

        // 2.
        let (handle, sealing) = self.sealing_folder().await?;
        let mut items: Vec<Item> = planned
            .into_iter()
            .map(|(name, copy)| Item {
                from: run_abs.join(&name),
                to: sealing.join(&name),
                name,
                copy,
            })
            .collect();
        if has_source {
            items.push(Item {
                name: "source".to_string(),
                from: run_abs.join("source"),
                to: sealing.join("source"),
                copy: false,
            });
        }

        let mut done: Vec<Done> = Vec::new();
        let staged = self
            .fill_sealing(&run, &items, &req.snapshot, &sealing, &mut done)
            .await;
        // 6.
        let placed = match staged {
            Ok(()) => {
                let (from, to) = (sealing.clone(), version_abs.clone());
                self.fs_retry_move(Some("store.replace"), &sealing, move || {
                    fsops::move_new(&from, &to)
                })
                .await
                .map_err(|e| match classify(&e) {
                    RetryClass::TargetExists => StoreError::VersionExists(req.version.to_string()),
                    _ => StoreError::io(&version_rel, &e),
                })
            }
            Err(e) => Err(e),
        };
        if let Err(e) = placed {
            self.undo_seal(&done, &sealing).await;
            self.forget_handle(&handle);
            return Err(e);
        }
        self.forget_handle(&handle);

        // 7.
        let sealed = self.record_sealed(&version_rel, &folder_name).await?;
        // 8.
        self.fs(Some("store.after_seal"), || Ok(()))
            .await
            .map_err(|e| StoreError::io(&version_rel, &e))?;
        tracing::info!(version = %version_rel, files = sealed.files.len(), "version sealed");
        Ok(sealed)
    }

    /// Whether `path` exists, by its attributes.
    async fn exists_now(&self, path: &Path) -> bool {
        let path = path.to_path_buf();
        blocking(move || Ok(exists(&path))).await.unwrap_or(false)
    }

    /// A new, empty `seal-<sh_…>` folder in the staging folder, live (never swept) until
    /// [`Store::forget_handle`]. Returns the handle id and the folder.
    async fn sealing_folder(&self) -> Result<(String, PathBuf), StoreError> {
        let root = self.staging_root();
        for _ in 0..16 {
            let id = self.ids().id("sh");
            let dir = root.join(format!("{SEAL_PREFIX}{id}"));
            lock(&self.live_handles).insert(id.clone());
            let (parent, folder) = (root.clone(), dir.clone());
            let made = blocking(move || {
                fsops::ensure_dir(&parent)?;
                fsops::create_dir_new(&folder)
            })
            .await;
            match made {
                Ok(()) => return Ok((id, dir)),
                Err(e) => {
                    lock(&self.live_handles).remove(&id);
                    if e.kind() != io::ErrorKind::AlreadyExists {
                        return Err(StoreError::io(&self.rel_of(&dir), &e));
                    }
                }
            }
        }
        Err(StoreError::Io {
            path: super::staging::STAGING_DIR.to_string(),
            why: "no free sealing folder name".to_string(),
        })
    }

    fn forget_handle(&self, id: &str) {
        lock(&self.live_handles).remove(id);
    }

    /// Steps 3–5 into `sealing`; what was done is pushed to `done` as it happens.
    async fn fill_sealing(
        &self,
        run: &WsPath,
        items: &[Item],
        snapshot: &Manifest,
        sealing: &Path,
        done: &mut Vec<Done>,
    ) -> Result<(), StoreError> {
        // 3.
        for item in items {
            let rel = run.join(&item.name)?;
            let (from, to) = (item.from.clone(), item.to.clone());
            if item.copy {
                let sha256 = match self
                    .see_hash(&from)
                    .await
                    .map_err(|e| StoreError::io(&rel, &e))?
                {
                    Seen::Found(h) => h.sha256,
                    _ => {
                        return Err(StoreError::Io {
                            path: rel.as_str().to_string(),
                            why: "the file left the run while it was being sealed".to_string(),
                        });
                    }
                };
                self.fs_retry(Some("store.replace"), move || {
                    fsops::copy_new(&from, &to).map(drop)
                })
                .await
                .map_err(|e| StoreError::io(&rel, &e))?;
                done.push(Done::Made {
                    path: item.to.clone(),
                    sha256,
                });
            } else {
                // `source/` is a folder: its move also retries error 5 (ruling Q19).
                self.fs_retry_move(Some("store.replace"), &item.from, move || {
                    fsops::move_new(&from, &to)
                })
                .await
                .map_err(|e| StoreError::io(&rel, &e))?;
                done.push(Done::Moved {
                    sealing: item.to.clone(),
                    run: item.from.clone(),
                });
            }
        }
        // 4.
        let bytes = snapshot.to_text().into_bytes();
        let sha256 = sha256_hex(&bytes);
        let (dir, fp) = (sealing.to_path_buf(), self.failpoints().clone());
        let snapshot_rel = self.rel_of(&sealing.join(SNAPSHOT));
        let path = blocking(move || fsops::write_temp(&dir, SNAPSHOT, &bytes, &fp))
            .await
            .map_err(|e| StoreError::io(&snapshot_rel, &e))?;
        done.push(Done::Made { path, sha256 });
        // 5.
        let dir = sealing.to_path_buf();
        blocking(move || set_readonly_all(&dir, true))
            .await
            .map_err(|e| StoreError::io(&self.rel_of(sealing), &e))
    }

    /// Undoes what a failed seal did in `sealing`, newest first: moved files go back to the
    /// run, writable again; copies and the snapshot are removed only while they still hold the
    /// bytes the seal put there. The folder is removed once it is empty; whatever stays in it
    /// is an orphan, and nothing is lost.
    ///
    /// CORE-110: removing a copy or the snapshot is a hard delete, of the core's own bytes
    /// that this call wrote moments before (a copy whose original is still in the run, or the
    /// snapshot of a manifest that is still on disk). It goes through the store's
    /// `remove_if_hash`, which hashes and deletes through one unshared handle, so a file that
    /// changed since is never deleted.
    async fn undo_seal(&self, done: &[Done], sealing: &Path) {
        for step in done.iter().rev() {
            match step {
                Done::Moved { sealing: at, run } => {
                    let (from, at, run) = (at.clone(), at.clone(), run.clone());
                    let back = self
                        .fs_retry_move(None, &from, move || {
                            set_readonly_all(&at, false)?;
                            fsops::move_new(&at, &run)
                        })
                        .await;
                    if let Err(e) = back {
                        tracing::warn!(path = %step_path(step).display(), error = %e, "a file of a failed seal stays in its sealing folder");
                    }
                }
                Done::Made { path, sha256 } => {
                    let (path, sha256) = (path.clone(), sha256.clone());
                    match self
                        .fs_retry(None, move || remove_if_hash(&path, &sha256))
                        .await
                    {
                        Ok(true) => {}
                        Ok(false) => {
                            tracing::warn!(path = %step_path(step).display(), "a copy of a failed seal changed; it stays in its sealing folder");
                        }
                        Err(e) => {
                            tracing::warn!(path = %step_path(step).display(), error = %e, "a copy of a failed seal stays in its sealing folder");
                        }
                    }
                }
            }
        }
        let dir = sealing.to_path_buf();
        if let Err(e) = blocking(move || fsops::remove_empty_dir(&dir)).await {
            tracing::warn!(dir = %sealing.display(), error = %e, "a failed seal's folder stays as an orphan");
        }
    }

    /// Step 7: every file of the sealed `version` folder hashed (its hash recorded as the
    /// generated one) and its identity taken; the content hash.
    async fn record_sealed(&self, version: &WsPath, folder: &str) -> Result<Sealed, StoreError> {
        let dir = version.abs(self.root());
        let listed = blocking(move || files_under(&dir))
            .await
            .map_err(|e| StoreError::io(version, &e))?;
        let now = self.now_utc();
        let mut files = Vec::new();
        let mut hashes = Vec::new();
        let mut identities = Vec::new();
        for (inside, path) in listed {
            let rel = WsPath::from_disk(format!("{version}/{inside}"));
            let sha256 = match self
                .see_hash(&path)
                .await
                .map_err(|e| StoreError::io(&rel, &e))?
            {
                Seen::Found(h) => h.sha256,
                _ => {
                    return Err(StoreError::Io {
                        path: rel.as_str().to_string(),
                        why: "a sealed file can't be read after its move".to_string(),
                    });
                }
            };
            self.state()
                .set_generated(rel.as_str(), &sha256, now)
                .await
                .map_err(|e| StoreError::state(&rel, &e))?;
            identities.push((rel.clone(), self.identity(&rel).await?));
            if inside != SNAPSHOT {
                files.push((format!("{folder}/{inside}"), sha256.clone()));
            }
            hashes.push((inside, sha256));
        }
        Ok(Sealed {
            dir: version.clone(),
            files,
            content_hash: version_content_hash(&hashes),
            identities,
        })
    }

    /// The `v<version>/` folders of an asset, by version. A folder with no
    /// `manifest.snapshot.json`, in an asset whose manifest has no run signed off as that
    /// version, is `legacy` (CORE-033). Each folder's content hash is computed from its files;
    /// a cloud-only file counts with the hash its manifest records (`hashes`), and without one
    /// the call fails with `store.hash_conflict`, since it would have to be downloaded. Nothing
    /// is moved or changed.
    ///
    /// # Errors
    /// `store.path_invalid` for a bad project or asset id; `store.io` when a folder can't be
    /// listed or a file read.
    pub async fn versions(
        &self,
        project: &str,
        asset_id: &str,
    ) -> Result<Vec<VersionDir>, StoreError> {
        let asset = WsPath::parse(&format!("projects/{project}/assets/{asset_id}"))?;
        let manifest = asset.join(&format!("{asset_id}.manifest.json"))?;
        let view = match self.read(&manifest).await {
            Ok(Some(bytes)) => std::str::from_utf8(&bytes)
                .ok()
                .and_then(|t| Manifest::parse(t).ok())
                .map(|(m, _)| m.view())
                .unwrap_or_default(),
            Ok(None) => ManifestView::default(),
            Err(e) => {
                tracing::warn!(manifest = %manifest, error = %e, "a manifest can't be read; its versions are read without it");
                ManifestView::default()
            }
        };
        let dir = asset.abs(self.root());
        let folders = blocking(move || list(&dir))
            .await
            .map_err(|e| StoreError::io(&asset, &e))?;
        let mut found = Vec::new();
        for folder in folders.into_iter().filter(|f| f.folder) {
            let Some(version) = version_folder(&folder.name) else {
                continue;
            };
            let rel = WsPath::from_disk(format!("{asset}/{}", folder.name));
            let path = folder.path.clone();
            let listed = blocking(move || files_under(&path))
                .await
                .map_err(|e| StoreError::io(&rel, &e))?;
            let has_snapshot = listed.iter().any(|(inside, _)| inside == SNAPSHOT);
            let mut hashes = Vec::new();
            for (inside, path) in listed {
                let file_rel = WsPath::from_disk(format!("{rel}/{inside}"));
                let sha256 = match self
                    .see_hash(&path)
                    .await
                    .map_err(|e| StoreError::io(&file_rel, &e))?
                {
                    Seen::Found(h) => h.sha256,
                    Seen::Placeholder => view
                        .hashes
                        .get(&format!("{}/{inside}", folder.name))
                        .and_then(|h| bare_sha256(h))
                        .map(str::to_string)
                        .ok_or_else(|| StoreError::cloud_only(&file_rel))?,
                    _ => continue,
                };
                hashes.push((inside, sha256));
            }
            let signed = view
                .candidate_runs
                .iter()
                .any(|r| r.signed_off.as_ref().is_some_and(|s| s.version == version));
            found.push(VersionDir {
                version,
                dir: rel,
                legacy: !has_snapshot && !signed,
                content_hash: version_content_hash(&hashes),
            });
        }
        found.sort_by_key(|v| v.version);
        Ok(found)
    }
}

fn step_path(step: &Done) -> &Path {
    match step {
        Done::Moved { sealing, .. } => sealing,
        Done::Made { path, .. } => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str, role: &str, from: Option<&str>) -> CandidateFile {
        CandidateFile {
            path: format!("candidates/r_1/{name}"),
            seed: None,
            role: role.to_string(),
            from: from.map(str::to_string),
            sha256: None,
        }
    }

    #[test] // spec: CORE-026
    fn plan_follows_derived_files_and_copies_their_sources() {
        let records = [
            record("a.png", "candidate", None),
            record("a_cut.png", "derived", Some("a.png")),
            record("a_big.png", "derived", Some("candidates/r_1/A_CUT.png")),
            record("b.png", "candidate", None),
            record("b_fix.png", "repair", Some("b.png")),
            record("b_fix_cut.png", "derived", Some("b_fix.png")),
        ];
        assert_eq!(
            plan_names(&["a.png".to_string()], &records),
            [
                ("a.png".to_string(), true),
                ("a_big.png".to_string(), false),
                ("a_cut.png".to_string(), true),
            ]
        );
        // A chosen repair file brings its own derived files; its source stays out.
        assert_eq!(
            plan_names(&["b_fix.png".to_string()], &records),
            [
                ("b_fix.png".to_string(), true),
                ("b_fix_cut.png".to_string(), false),
            ]
        );
        // Without records only the chosen files, moved.
        assert_eq!(
            plan_names(&["x.png".to_string()], &[]),
            [("x.png".to_string(), false)]
        );
    }
}
