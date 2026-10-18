//! Sealed versions (CORE-026, CORE-028, CORE-033, CORE-041; I5 "Sign-off"): the per-asset
//! version lock, [`Store::seal`] and [`Store::versions`].
//!
//! A sign-off (Task 13) takes the asset's [`Store::version_lock`] with `lock_owned()` and
//! keeps the guard from the seal to the end of its Record stage, so two sign-offs of one asset
//! never allocate the same version and the watcher can defer removals under a sealing asset.
//!
//! [`Store::seal`]:
//! 1. refuses an existing `v<version>/` (`version.exists`), unless it adopts it (below);
//! 2. creates `<ws>\.lilith\staging\seal-<sh_…>\`, a live staging entry while it works;
//! 3. moves the chosen files and the files derived from them (`role: derived`, following
//!    `from` through the run) from `candidates/<run>/`, copying instead a file that another
//!    run file was derived from (I5), and moves `source/` if the run has one. Before anything
//!    moves, each of them that the run records a `sha256` for must still have it (a derived
//!    file is checked like a chosen one): one changed outside the suite is
//!    `store.hash_conflict`;
//! 4. writes `manifest.snapshot.json` (`snapshot.to_text()`);
//! 5. sets the read-only attribute on every file, then checks each of those files in the
//!    sealing folder against its expected hash (`SealRequest::expected` for the chosen files,
//!    the run's recorded `sha256` for derived ones): a file changed outside the suite
//!    meanwhile is `store.hash_conflict` and the seal is undone;
//! 6. waits at the git gate (ARCH-083; `store.git_busy` after 60 s undoes the seal), then
//!    moves the folder to `projects/<p>/assets/<a>/v<version>/` with `move_new`, so a version
//!    folder never appears in the tracked tree in the middle of a checkout;
//! 7. hashes every sealed file, records each hash as the file's generated hash, takes the
//!    identities after the move (the read-only bit moves ChangeTime, and a ReFS move across
//!    folders changes the file id), and computes the content hash;
//! 8. hits the fail point `store.after_seal`.
//!
//! **Adopting an unrecorded seal** (ruling Q31). A sign-off whose seal succeeded but whose
//! Record stage failed (git busy, a held manifest, a fail point) leaves a `v<version>/` that
//! no manifest records. A retry of the same sign-off adopts it instead of failing with
//! `version.exists`, and never undoes it (that would mean clearing read-only and moving files
//! out of a sealed folder). It is adopted only when all of these hold, and otherwise the
//! retry is `version.exists` as before, so a sealed folder is never overwritten:
//! - the manifest records the version nowhere (not `approved_version`, no run's
//!   `signed_off.version`);
//! - `v<version>` itself is a plain folder: a link (never followed), a file or a cloud-only
//!   placeholder there is `version.exists` (ruling Q34);
//! - nothing in it is a link or a cloud-only file;
//! - its `manifest.snapshot.json` names this run as signed off as this version, with the
//!   same chosen files;
//! - every chosen file is there, every other top-level file is one the seal would have
//!   sealed, and each has the run's recorded `sha256` (or, without one, the generated hash
//!   the seal recorded); each `source/**` file has the generated hash the seal recorded;
//! - no file the seal would have sealed is missing from it while the run still holds it
//!   (it is exactly what the seal makes, ruling Q34).
//!
//! An adopted file that lost its read-only bit gets it back (the bit is only ever set, never
//! cleared, ruling Q34). The adopted files are recorded as generated again,
//! [`Sealed::adopted`] is set, and [`Sealed::signed_off_date`] carries the snapshot's
//! `signed_off.date`, which the manifest then records (ruling Q34).
//!
//! Every copy and move hits the fail point `store.replace` and retries sharing violations
//! under the store's budget (`store.busy` after it). On an error before step 6 finishes, each
//! moved file goes back to the run (writable again), each copy in the sealing folder is
//! removed only if it still holds the original's bytes (the store's `remove_if_hash`), the
//! snapshot likewise, and the emptied folder is removed; anything that can't go back stays in
//! the sealing folder, which is then an orphan. A half-filled `v<version>/` never exists.
//! Those removals are hard deletes of the core's own just-written duplicates (CORE-110; the
//! `crates/lil-core/tests/trash.rs` scan allows them here).
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

use lil_platform::fs::{FileIdentity, Probe, RetryClass, classify};
use lil_schema::{
    CandidateFile, Manifest, ManifestView, Version, bare_sha256, is_os_metadata, sha256_hex,
    version_content_hash,
};

use super::fsops;
use super::git_gate::GIT_WAIT;
use super::order::{list, version_folder};
use super::paths::{WsPath, path_key};
use super::staging::SEAL_PREFIX;
use super::write::{
    PathLock, Seen, Store, StoreError, blocking, exists, link_refused, lock, remove_if_hash, see,
};

/// `FILE_ATTRIBUTE_READONLY`.
const FILE_ATTRIBUTE_READONLY: u32 = 0x1;

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
    /// The SHA-256 (bare hex) each chosen file must have, by name (the run's recorded
    /// `files[].sha256`): checked in the sealing folder before the final move, so a file
    /// changed outside the suite during the seal is never sealed. A chosen name without an
    /// entry isn't checked.
    pub expected: BTreeMap<String, String>,
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
    /// The bare SHA-256 of `manifest.snapshot.json` as it is in the version.
    pub snapshot_sha256: String,
    /// The run paths the seal moved away (`candidates/<run>/<name>`, and
    /// `candidates/<run>/source`): the Record stage reports them to the index as removals.
    pub left_run: Vec<WsPath>,
    /// An unrecorded `v<version>/` of the same sign-off was adopted (module docs).
    pub adopted: bool,
    /// For an adopted version: the snapshot's `signed_off.date`, the day it was sealed.
    pub signed_off_date: Option<String>,
}

/// A `v<version>/` folder of an asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionDir {
    pub version: Version,
    pub dir: WsPath,
    /// No `manifest.snapshot.json` and no run signed off as this version (CORE-033): read as
    /// it is, never moved.
    pub legacy: bool,
    /// The content hash, computed only from the manifest's `hashes` for the version (ruling
    /// Q32), or from the files on disk for a legacy version that has no `hashes` entries;
    /// `None` when such a legacy version holds a cloud-only file (never downloaded).
    pub content_hash: Option<String>,
    /// A legacy version holds a cloud-only file, so its content hash isn't known.
    pub cloud_only: bool,
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

/// Every file under `dir` (links and OS metadata files such as `Thumbs.db` are skipped, never
/// followed or read; ruling Q32), as (`/`-separated path relative to `dir`, absolute path),
/// sorted by the relative path in byte order. A missing `dir` has none.
pub(super) fn files_under(dir: &Path) -> io::Result<Vec<(String, PathBuf)>> {
    let mut found = Vec::new();
    let mut folders = vec![(String::new(), dir.to_path_buf())];
    while let Some((prefix, folder)) = folders.pop() {
        let entries = match std::fs::read_dir(lil_platform::fs::long_path(&folder)) {
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
            } else if !is_os_metadata(&entry.file_name().to_string_lossy()) {
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
        for entry in std::fs::read_dir(lil_platform::fs::long_path(&folder))? {
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
    let attrs = lil_platform::fs::attributes(path)?;
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
        let records: Vec<CandidateFile> = req
            .snapshot
            .view()
            .candidate_runs
            .into_iter()
            .find(|r| path_key(&r.run_id) == path_key(&req.run_id))
            .map(|r| r.files)
            .unwrap_or_default();
        let want = expected_hashes(&req, &records);
        // 1.
        if self.exists_now(&version_abs).await {
            let found = Found {
                asset: &asset,
                version: &version_rel,
                folder: &folder_name,
                run: &run,
            };
            if let Some(sealed) = self.adopt(&req, &found, &chosen, &records, &want).await? {
                return Ok(sealed);
            }
            return Err(StoreError::VersionExists(req.version.to_string()));
        }

        // The plan: every name is checked before anything moves.
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
                Seen::Found(h) => {
                    // A chosen or derived file edited outside the suite is never sealed.
                    if let Some(expected) = want.get(&path_key(&name))
                        && *expected != h.sha256
                    {
                        return Err(StoreError::Changed {
                            path: rel,
                            expected: expected.clone(),
                            found: h.sha256,
                        });
                    }
                    planned.push((name, copy));
                }
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
        // 5b. Each chosen file, as it is in the sealing folder, is still what the run recorded.
        let staged = match staged {
            Ok(()) => self.check_expected(&want, &items, &run, &sealing).await,
            Err(e) => Err(e),
        };
        // 6. Not while git works on the workspace: the version folder appears in the tracked
        // tree with this move.
        let staged = match staged {
            Ok(()) => self.git_gate().wait_clear(GIT_WAIT).await,
            Err(e) => Err(e),
        };
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
        let mut sealed = self.record_sealed(&version_rel, &folder_name).await?;
        sealed.left_run = self.left_run(&run, &sealed).await;
        // 8.
        self.fs(Some("store.after_seal"), || Ok(()))
            .await
            .map_err(|e| StoreError::io(&version_rel, &e))?;
        tracing::info!(version = %version_rel, files = sealed.files.len(), "version sealed");
        Ok(sealed)
    }

    /// Step 5b: every chosen or derived file in `sealing` that `want` names has its expected
    /// hash, else `store.hash_conflict` naming the run's file (the undo puts it back there).
    async fn check_expected(
        &self,
        want: &BTreeMap<String, String>,
        items: &[Item],
        run: &WsPath,
        sealing: &Path,
    ) -> Result<(), StoreError> {
        for name in items.iter().map(|i| &i.name) {
            let Some(expected) = want.get(&path_key(name)).cloned() else {
                continue;
            };
            let rel = run.join(name)?;
            let found = match self
                .see_hash(&sealing.join(name))
                .await
                .map_err(|e| StoreError::io(&rel, &e))?
            {
                Seen::Found(h) => h.sha256,
                _ => {
                    return Err(StoreError::Io {
                        path: rel.as_str().to_string(),
                        why: "a file being sealed left its sealing folder".to_string(),
                    });
                }
            };
            if found != expected {
                tracing::warn!(path = %rel, "a file changed outside the suite during its seal; it goes back to the run");
                return Err(StoreError::Changed {
                    path: rel,
                    expected,
                    found,
                });
            }
        }
        Ok(())
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
        let mut snapshot_sha256 = String::new();
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
            if inside == SNAPSHOT {
                snapshot_sha256.clone_from(&sha256);
            } else {
                files.push((format!("{folder}/{inside}"), sha256.clone()));
            }
            hashes.push((inside, sha256));
        }
        Ok(Sealed {
            dir: version.clone(),
            files,
            content_hash: version_content_hash(&hashes),
            identities,
            snapshot_sha256,
            left_run: Vec::new(),
            adopted: false,
            signed_off_date: None,
        })
    }

    /// The run paths `sealed` took away: for each top-level name in the version, the run's
    /// `candidates/<run>/<name>` when it is no longer there (a copied file stays).
    async fn left_run(&self, run: &WsPath, sealed: &Sealed) -> Vec<WsPath> {
        let mut tops: Vec<String> = Vec::new();
        for (key, _) in &sealed.files {
            let inside = key.split_once('/').map_or(key.as_str(), |(_, rest)| rest);
            let top = inside.split('/').next().unwrap_or(inside).to_string();
            if !tops.iter().any(|t| path_key(t) == path_key(&top)) {
                tops.push(top);
            }
        }
        let mut gone = Vec::new();
        for top in tops {
            let rel = WsPath::from_disk(format!("{run}/{top}"));
            if !self.exists_now(&rel.abs(self.root())).await {
                gone.push(rel);
            }
        }
        gone
    }

    /// Step 1's adoption (module docs): `Some` with the version as it is when `found` is the
    /// unrecorded seal of this same sign-off, `None` (logged) when anything disagrees.
    async fn adopt(
        &self,
        req: &SealRequest,
        found: &Found<'_>,
        chosen: &[String],
        records: &[CandidateFile],
        want: &BTreeMap<String, String>,
    ) -> Result<Option<Sealed>, StoreError> {
        let version_rel = found.version;
        let refuse = |why: &str| {
            tracing::warn!(version = %version_rel, why, "an existing version folder isn't this sign-off's; it is left as it is");
            Ok(None)
        };
        // Recorded in the manifest: a real collision.
        let manifest = WsPath::from_disk(format!("{}/{}.manifest.json", found.asset, req.asset_id));
        let view = match self.read(&manifest).await {
            Ok(Some(bytes)) => match std::str::from_utf8(&bytes)
                .ok()
                .and_then(|t| Manifest::parse(t).ok())
            {
                Some((m, _)) => m.view(),
                None => return refuse("the manifest doesn't parse"),
            },
            Ok(None) => return refuse("the asset has no manifest"),
            Err(e) => return Err(e),
        };
        let recorded = view.approved_version == Some(req.version)
            || view.candidate_runs.iter().any(|r| {
                r.signed_off
                    .as_ref()
                    .is_some_and(|s| s.version == req.version)
            });
        if recorded {
            return refuse("the manifest records this version");
        }
        let abs = version_rel.abs(self.root());
        // The folder itself first: a link there is never followed, and a file or a
        // placeholder is no seal (ruling Q34).
        let (probe, at) = (self.probe().clone(), abs.clone());
        let itself = blocking(move || see(probe.as_ref(), &at, |_| Ok(())))
            .await
            .map_err(|e| StoreError::io(version_rel, &e))?;
        if !matches!(itself, Seen::Folder) {
            return refuse("it isn't a plain folder (a link, a file or a cloud-only placeholder)");
        }
        let (probe, dir) = (self.probe().clone(), abs.clone());
        let unsafe_inside = blocking(move || unsafe_entry(probe.as_ref(), &dir))
            .await
            .map_err(|e| StoreError::io(version_rel, &e))?;
        if unsafe_inside.is_some() {
            return refuse("it holds a link or a cloud-only file");
        }
        let dir = abs.clone();
        let listed = blocking(move || files_under(&dir))
            .await
            .map_err(|e| StoreError::io(version_rel, &e))?;

        // The snapshot names this run, signed off as this version, with the same files.
        let snapshot_rel = WsPath::from_disk(format!("{version_rel}/{SNAPSHOT}"));
        let Some(snapshot_bytes) = self.read(&snapshot_rel).await? else {
            return refuse("it has no manifest.snapshot.json");
        };
        let Some((snapshot, _)) = std::str::from_utf8(&snapshot_bytes)
            .ok()
            .and_then(|t| Manifest::parse(t).ok())
        else {
            return refuse("its snapshot doesn't parse");
        };
        let keys =
            |names: &[String]| -> BTreeSet<String> { names.iter().map(|n| path_key(n)).collect() };
        let snapshot_view = snapshot.view();
        let signed_off = snapshot_view
            .candidate_runs
            .iter()
            .find(|r| path_key(&r.run_id) == path_key(&req.run_id))
            .filter(|r| keys(&r.selected) == keys(chosen))
            .and_then(|r| r.signed_off.as_ref())
            .filter(|s| s.version == req.version);
        let Some(signed_off) = signed_off else {
            return refuse("its snapshot isn't this run's sign-off as this version");
        };
        let signed_off_date = signed_off.date.clone();

        // Every file is one the seal would have sealed, with the hash the run recorded (or
        // the one the seal recorded as generated).
        let allowed: BTreeSet<String> = plan_names(chosen, records)
            .into_iter()
            .map(|(name, _)| path_key(&name))
            .collect();
        let mut present: BTreeSet<String> = BTreeSet::new();
        let mut hashes = Vec::new();
        for (inside, path) in &listed {
            let rel = WsPath::from_disk(format!("{version_rel}/{inside}"));
            let sha256 = match self
                .see_hash(path)
                .await
                .map_err(|e| StoreError::io(&rel, &e))?
            {
                Seen::Found(h) => h.sha256,
                _ => return refuse("a file in it can't be read"),
            };
            let generated = self
                .state()
                .generated(rel.as_str())
                .await
                .map_err(|e| StoreError::state(&rel, &e))?;
            let expected = if inside == SNAPSHOT {
                Some(sha256.clone())
            } else if let Some((top, _)) = inside.split_once('/') {
                if !top.eq_ignore_ascii_case("source") {
                    return refuse("it holds a folder the seal never makes");
                }
                generated
            } else {
                let key = path_key(inside);
                if !allowed.contains(&key) {
                    return refuse("it holds a file the seal wouldn't have sealed");
                }
                present.insert(key.clone());
                want.get(&key).cloned().or(generated)
            };
            if expected.as_deref() != Some(sha256.as_str()) {
                return refuse("a file in it isn't what the run recorded");
            }
            hashes.push((inside.clone(), rel, sha256));
        }
        if chosen.iter().any(|c| !present.contains(&path_key(c))) {
            return refuse("a chosen file isn't in it");
        }
        // Exactly what the seal makes: a file it would have sealed that the run still holds
        // (a derived file put back, say) but the folder lacks is a mismatch.
        let run_abs = found.run.abs(self.root());
        for (name, _) in plan_names(chosen, records) {
            if !present.contains(&path_key(&name)) && self.exists_now(&run_abs.join(&name)).await {
                return refuse("a file the seal would have sealed is in the run, not in it");
            }
        }

        // Every adopted file is read-only, as the seal leaves it: the bit is only ever set.
        let paths: Vec<PathBuf> = listed.iter().map(|(_, path)| path.clone()).collect();
        blocking(move || {
            for path in &paths {
                if lil_platform::fs::attributes(path)? & FILE_ATTRIBUTE_READONLY == 0 {
                    fsops::set_readonly(path, true)?;
                }
            }
            Ok(())
        })
        .await
        .map_err(|e| StoreError::io(version_rel, &e))?;

        let now = self.now_utc();
        let mut files = Vec::new();
        let mut content = Vec::new();
        let mut identities = Vec::new();
        let mut snapshot_sha256 = String::new();
        for (inside, rel, sha256) in hashes {
            self.state()
                .set_generated(rel.as_str(), &sha256, now)
                .await
                .map_err(|e| StoreError::state(&rel, &e))?;
            identities.push((rel.clone(), self.identity(&rel).await?));
            if inside == SNAPSHOT {
                snapshot_sha256.clone_from(&sha256);
            } else {
                files.push((format!("{}/{inside}", found.folder), sha256.clone()));
            }
            content.push((inside, sha256));
        }
        let mut sealed = Sealed {
            dir: version_rel.clone(),
            files,
            content_hash: version_content_hash(&content),
            identities,
            snapshot_sha256,
            left_run: Vec::new(),
            adopted: true,
            signed_off_date: Some(signed_off_date),
        };
        sealed.left_run = self.left_run(found.run, &sealed).await;
        tracing::info!(version = %version_rel, "an unrecorded seal of the same sign-off is adopted");
        Ok(Some(sealed))
    }

    /// The `v<version>/` folders of an asset, by version. A folder with no
    /// `manifest.snapshot.json`, in an asset whose manifest has no run signed off as that
    /// version, is `legacy` (CORE-033). Nothing is moved or changed.
    ///
    /// The content hash (ruling Q32) is computed only over the files the manifest's `hashes`
    /// lists for that version, with their recorded SHA-256, so no sealed file is opened and a
    /// file added to the folder later never changes it. Only a legacy version with no
    /// `hashes` entries is hashed from disk, skipping OS metadata files; one that holds a
    /// cloud-only file gets no content hash (`cloud_only`), and the file is never read.
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
            let recorded: Vec<(String, String)> = view
                .hashes
                .iter()
                .filter_map(|(key, value)| {
                    let (first, inside) = key.split_once('/')?;
                    if version_folder(first) != Some(version) || is_os_metadata(inside) {
                        return None;
                    }
                    Some((inside.to_string(), bare_sha256(value)?.to_string()))
                })
                .collect();
            let (has_snapshot, content_hash, cloud_only) = if recorded.is_empty() {
                self.hash_legacy(&rel, &folder.path).await?
            } else {
                let snapshot = folder.path.join(SNAPSHOT);
                let has_snapshot = blocking(move || Ok(exists(&snapshot)))
                    .await
                    .unwrap_or(false);
                (has_snapshot, Some(version_content_hash(&recorded)), false)
            };
            let signed = view
                .candidate_runs
                .iter()
                .any(|r| r.signed_off.as_ref().is_some_and(|s| s.version == version));
            found.push(VersionDir {
                version,
                dir: rel,
                legacy: !has_snapshot && !signed,
                content_hash,
                cloud_only,
            });
        }
        found.sort_by_key(|v| v.version);
        Ok(found)
    }

    /// A version folder with no `hashes` entries, read from disk: whether it holds a snapshot,
    /// its content hash (`None` when a file in it is cloud-only, never read) and that flag.
    async fn hash_legacy(
        &self,
        rel: &WsPath,
        path: &Path,
    ) -> Result<(bool, Option<String>, bool), StoreError> {
        let path = path.to_path_buf();
        let listed = blocking(move || files_under(&path))
            .await
            .map_err(|e| StoreError::io(rel, &e))?;
        let has_snapshot = listed.iter().any(|(inside, _)| inside == SNAPSHOT);
        let mut hashes = Vec::new();
        let mut cloud_only = false;
        for (inside, path) in listed {
            let file_rel = WsPath::from_disk(format!("{rel}/{inside}"));
            match self
                .see_hash(&path)
                .await
                .map_err(|e| StoreError::io(&file_rel, &e))?
            {
                Seen::Found(h) => hashes.push((inside, h.sha256)),
                Seen::Placeholder => cloud_only = true,
                _ => {}
            }
        }
        let content_hash = (!cloud_only).then(|| version_content_hash(&hashes));
        Ok((has_snapshot, content_hash, cloud_only))
    }
}

/// Where an existing version folder is (step 1's adoption).
struct Found<'a> {
    /// `projects/<p>/assets/<a>`.
    asset: &'a WsPath,
    /// `<asset>/v<version>`.
    version: &'a WsPath,
    /// `v<version>`.
    folder: &'a str,
    /// `<asset>/candidates/<run>`.
    run: &'a WsPath,
}

/// The SHA-256 each file to seal must have, by its name's `path_key`: the chosen files' from
/// `req.expected`, and the derived ones' from the run's file records (a record without one
/// isn't checked).
fn expected_hashes(req: &SealRequest, records: &[CandidateFile]) -> BTreeMap<String, String> {
    let mut want: BTreeMap<String, String> = records
        .iter()
        .filter(|r| r.role == "derived")
        .filter_map(|r| Some((path_key(bare_name(&r.path)), r.sha256.clone()?)))
        .collect();
    for (name, hex) in &req.expected {
        want.insert(path_key(name), hex.clone());
    }
    want
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
