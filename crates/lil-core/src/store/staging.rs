//! Staging handles (ARCH-080, ARCH-033, CORE-003): the write areas the store issues to
//! engines and converters, `<ws>\.worldwright\staging\<sh_16hex>\`. They are inside the
//! workspace, so ingest is a rename on one volume, and the watcher never looks there.
//!
//! The handles this store issued and hasn't released are *live*. At a read-write open, under
//! `core.lock`, every other handle (and every `seal-*` folder, Task 9's sealing area) belongs
//! to a dead core: [`Store::sweep_staging`] removes the empty ones (empty folders only, which
//! the CORE-110 scan allows) and returns the rest for the orphan list.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use lil_platform::fs::{RetryClass, classify};

use super::fsops;
use super::paths::WsPath;
use super::write::{Seen, Store, StoreError, blocking};

/// The staging folder, workspace-relative.
pub const STAGING_DIR: &str = ".worldwright/staging";
/// The prefix of a sealing folder in the staging folder (`seal-<sh_…>`).
pub const SEAL_PREFIX: &str = "seal-";

/// A staging handle: its id `sh_<16 hex>` and its folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagingHandle {
    pub id: String,
    pub dir: PathBuf,
}

/// One entry of the staging folder.
struct Entry {
    name: String,
    path: PathBuf,
    /// A folder (not a link) with nothing in it.
    empty_folder: bool,
}

/// The entries of `dir` (none when it doesn't exist). Links are never followed: a link is
/// listed, never empty, so it is never removed.
fn entries(dir: &Path) -> io::Result<Vec<Entry>> {
    let listed = match std::fs::read_dir(lil_platform::fs::long_path(dir)) {
        Ok(listed) => listed,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut found = Vec::new();
    for entry in listed {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = dir.join(&name);
        let kind = entry.file_type()?;
        let empty_folder = kind.is_dir()
            && !kind.is_symlink()
            && std::fs::read_dir(lil_platform::fs::long_path(&path))?
                .next()
                .is_none();
        found.push(Entry {
            name,
            path,
            empty_folder,
        });
    }
    Ok(found)
}

impl Store {
    /// `<root>\.worldwright\staging`.
    pub(super) fn staging_root(&self) -> PathBuf {
        self.root().join(".worldwright").join("staging")
    }

    /// Whether the staging entry `name` is one of this store's live handles (or its sealing
    /// folder).
    fn is_live(&self, name: &str) -> bool {
        let live = self
            .live_handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        live.contains(name)
            || name
                .strip_prefix(SEAL_PREFIX)
                .is_some_and(|h| live.contains(h))
    }

    fn staging_rel(name: &str) -> WsPath {
        WsPath::from_disk(format!("{STAGING_DIR}/{name}"))
    }

    /// A new, empty staging handle `<ws>\.worldwright\staging\sh_<16 hex>\` (a folder that
    /// didn't exist), live until [`Store::release_handle`].
    ///
    /// # Errors
    /// `workspace.read_only` on a read-only store; `store.io` (or `store.disk_full`) when the
    /// folder can't be created.
    pub async fn staging_handle(&self) -> Result<StagingHandle, StoreError> {
        self.writable_store()?;
        let root = self.staging_root();
        for _ in 0..16 {
            let id = self.ids().id("sh");
            let dir = root.join(&id);
            // Live before it exists, so no sweep ever takes it for a dead core's.
            self.live_handles
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(id.clone());
            let (parent, folder) = (root.clone(), dir.clone());
            let made = blocking(move || {
                fsops::ensure_dir(&parent)?;
                fsops::create_dir_new(&folder)
            })
            .await;
            match made {
                Ok(()) => return Ok(StagingHandle { id, dir }),
                Err(e) => {
                    self.live_handles
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&id);
                    if e.kind() != io::ErrorKind::AlreadyExists {
                        return Err(StoreError::io(&Store::staging_rel(&id), &e));
                    }
                }
            }
        }
        Err(StoreError::Io {
            path: STAGING_DIR.to_string(),
            why: "no free staging handle name".to_string(),
        })
    }

    /// Gives the handle back and removes its folder, which must be empty (its files were
    /// ingested). A handle that still holds files is kept on disk, is no longer live, and so
    /// becomes an orphan; the call is then `store.io`. A folder that is already gone is fine.
    ///
    /// # Errors
    /// `store.path_invalid` for a folder that isn't a staging handle; `store.io` as above.
    pub async fn release_handle(&self, h: StagingHandle) -> Result<(), StoreError> {
        let rel = Store::staging_rel(&h.id);
        if h.dir != self.staging_root().join(&h.id) {
            return Err(StoreError::PathInvalid {
                path: h.dir.display().to_string(),
                why: "it is not a staging handle of this workspace".to_string(),
            });
        }
        let dir = h.dir.clone();
        let removed = blocking(move || fsops::remove_empty_dir(&dir)).await;
        self.live_handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&h.id);
        match removed {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => {
                tracing::warn!(handle = %rel, error = %e, "a released staging handle still holds files; it stays as an orphan");
                Err(StoreError::Io {
                    path: rel.as_str().to_string(),
                    why: format!("the staging handle can't be removed: {e}"),
                })
            }
        }
    }

    /// Copies the file `from` (outside the store's write path, such as a file to import) into
    /// the live handle `h` as `name`, with `CopyFile2(COPY_FILE_FAIL_IF_EXISTS)`, so nothing is
    /// ever overwritten, and returns the staged path and the SHA-256 of the staged copy (not of
    /// the source, which may change meanwhile; CORE-023). Sharing violations are retried under
    /// the store's budget.
    ///
    /// # Errors
    /// `store.path_invalid` for a handle that isn't a live staging handle of this workspace, a
    /// `name` that isn't one path component, or a source that is a link or a folder;
    /// `store.hash_conflict` when `name` is already in the handle (nothing is overwritten);
    /// the errors of [`Store::hash_outside`] for the source; `store.io` (`store.busy`,
    /// `store.disk_full`) for a failed copy or read.
    pub async fn stage_copy(
        &self,
        h: &StagingHandle,
        from: &Path,
        name: &str,
    ) -> Result<(PathBuf, String), StoreError> {
        self.writable_store()?;
        let live = self
            .live_handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&h.id);
        if !live || h.dir != self.staging_root().join(&h.id) {
            return Err(StoreError::PathInvalid {
                path: h.dir.display().to_string(),
                why: "it is not a live staging handle of this workspace".to_string(),
            });
        }
        let staged_rel = WsPath::parse(&format!("{STAGING_DIR}/{}/{name}", h.id))?;
        if name.is_empty() || name.contains('/') {
            return Err(StoreError::PathInvalid {
                path: name.to_string(),
                why: "a staged file name is one path component".to_string(),
            });
        }
        // The source must be a local file that is not a link (never read through one).
        self.hash_outside(from).await?;
        let to = h.dir.join(name);
        let (src, dst) = (from.to_path_buf(), to.clone());
        self.fs_retry(None, move || fsops::copy_new(&src, &dst).map(drop))
            .await
            .map_err(|e| match classify(&e) {
                RetryClass::TargetExists => StoreError::HashConflict {
                    path: staged_rel.clone(),
                    backup: None,
                    cloud_only: false,
                    kept: Vec::new(),
                },
                _ => StoreError::io(&staged_rel, &e),
            })?;
        match self
            .see_hash(&to)
            .await
            .map_err(|e| StoreError::io(&staged_rel, &e))?
        {
            Seen::Found(h) => Ok((to, h.sha256)),
            _ => Err(StoreError::Io {
                path: staged_rel.as_str().to_string(),
                why: "the staged copy can't be read back".to_string(),
            }),
        }
    }

    /// The SHA-256 of a file named by an absolute path that may be outside the workspace (a
    /// link-external file, a file to import), read through the probe: the attributes first,
    /// so a link is never followed and a cloud-only placeholder never downloaded.
    ///
    /// # Errors
    /// `store.path_invalid` for a relative path, a link, a folder or a cloud-only placeholder
    /// (with what to do); `store.io` when there is no file or it can't be read (`store.busy`
    /// while another program holds it).
    pub async fn hash_outside(&self, path: &Path) -> Result<String, StoreError> {
        let shown = path.display().to_string();
        let refuse = |why: &str| StoreError::PathInvalid {
            path: shown.clone(),
            why: why.to_string(),
        };
        if !path.is_absolute() {
            return Err(refuse(
                "a file outside the store is named by its absolute path",
            ));
        }
        let seen = self.see_hash(path).await.map_err(|e| match classify(&e) {
            RetryClass::SharingViolation => StoreError::Busy(WsPath::from_disk(shown.clone())),
            _ => StoreError::Io {
                path: shown.clone(),
                why: e.to_string(),
            },
        })?;
        match seen {
            Seen::Found(h) => Ok(h.sha256),
            Seen::Missing => Err(StoreError::Io {
                path: shown.clone(),
                why: "there is no file there".to_string(),
            }),
            Seen::Link => Err(refuse(
                "it is a symbolic link or junction; the suite never reads through a link, so \
                 name the file itself",
            )),
            Seen::Folder => Err(refuse("it is a folder, not a file")),
            Seen::Placeholder => Err(refuse(
                "it is a cloud-only file, which the suite never downloads: make it available \
                 offline first",
            )),
        }
    }

    /// Removes the empty staging handles that aren't live and returns the other entries that
    /// aren't live (non-empty handles, `seal-*` folders, anything else), sorted, for the
    /// orphan list. [`Store::recover`] calls it at a read-write open, under `core.lock`, when
    /// every existing handle belongs to a dead core.
    ///
    /// # Errors
    /// `workspace.read_only` on a read-only store; `store.io` when the staging folder can't be
    /// listed.
    pub async fn sweep_staging(&self) -> Result<Vec<WsPath>, StoreError> {
        self.writable_store()?;
        Ok(self.sweep().await?.1)
    }

    /// [`Store::sweep_staging`]: (removed, kept).
    pub(super) async fn sweep(&self) -> Result<(Vec<WsPath>, Vec<WsPath>), StoreError> {
        let root = self.staging_root();
        let found = blocking(move || entries(&root))
            .await
            .map_err(|e| StoreError::io(&WsPath::from_disk(STAGING_DIR.to_string()), &e))?;
        let (mut removed, mut kept) = (BTreeSet::new(), BTreeSet::new());
        for entry in found {
            if self.is_live(&entry.name) {
                continue;
            }
            let rel = Store::staging_rel(&entry.name);
            if entry.empty_folder {
                let path = entry.path.clone();
                match blocking(move || fsops::remove_empty_dir(&path)).await {
                    Ok(()) => {
                        removed.insert(rel);
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!(handle = %rel, error = %e, "an empty staging handle stays");
                    }
                }
            }
            kept.insert(rel);
        }
        Ok((removed.into_iter().collect(), kept.into_iter().collect()))
    }

    /// The staging orphans: entries that aren't live and are non-empty, or are `seal-*`
    /// folders. A read-only store lists none: it can't tell the serving core's live handles
    /// from a dead core's.
    pub(super) async fn staging_orphans(&self) -> Result<Vec<WsPath>, StoreError> {
        if self.writable_store().is_err() {
            return Ok(Vec::new());
        }
        let root = self.staging_root();
        let found = blocking(move || entries(&root))
            .await
            .map_err(|e| StoreError::io(&WsPath::from_disk(STAGING_DIR.to_string()), &e))?;
        Ok(found
            .into_iter()
            .filter(|e| !self.is_live(&e.name))
            .filter(|e| !e.empty_folder || e.name.starts_with(SEAL_PREFIX))
            .map(|e| Store::staging_rel(&e.name))
            .collect())
    }
}
