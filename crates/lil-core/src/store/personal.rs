//! Personal resource files and their trash (APP-078, CORE-037, CORE-110): the file side of the
//! personal scope of `resources`, whose workspace scope goes through [`Store`](super::Store).
//!
//! The root is `%APPDATA%\LILITH\resources\`, a folder that is not a workspace. A [`Personal`]
//! has its own trash in `<root>\.trash\`, with the workspace trash's layout and record
//! ([`TrashRecord`]; `original_path` is relative to the root, with `/` separators) and its
//! 30-day purge ([`Personal::maintenance`]). Like the workspace trash it never replaces: a
//! restore onto a path that is taken is `trash.restore_conflict`. Paths are checked as
//! [`WsPath`]s, a link is never followed, and every file-mutating call is in
//! [`fsops`](super::fsops).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lil_platform::fs::Probe;
use lil_platform::{Clock, IdSource};

use super::failpoints::FailPoints;
use super::fsops;
use super::order::list_with;
use super::paths::{WsPath, path_key};
use super::trash::{
    DAY, PAYLOAD, RECORD, TRASH_DIR, TRASH_KEEP_DAYS, TrashRecord, drop_empty_entry, entries,
    is_entry_id, parse_rfc3339, purge_entry, refuse_link_at, refuse_linked_trash, remove_if_empty,
};
use super::write::{Seen, StoreError, blocking, link_refused, read_all, see};

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// The names of the files directly in `dir` (none when it doesn't exist), listed through
/// `probe`; folders and links are left out.
///
/// # Errors
/// The listing's.
pub fn list_files(probe: &dyn Probe, dir: &Path) -> io::Result<Vec<String>> {
    Ok(list_with(probe, dir)?
        .into_iter()
        .filter(|e| !e.folder)
        .map(|e| e.name)
        .collect())
}

/// The personal resource folder and its trash (module docs).
#[derive(Debug, Clone)]
pub struct Personal {
    root: PathBuf,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdSource>,
    probe: Arc<dyn Probe>,
}

impl Personal {
    /// The personal folder `root` (`<data_dir>\resources`; it is made when first written).
    pub fn new(
        root: PathBuf,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdSource>,
        probe: Arc<dyn Probe>,
    ) -> Personal {
        Personal {
            root,
            clock,
            ids,
            probe,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn probe(&self) -> &Arc<dyn Probe> {
        &self.probe
    }

    fn now_utc(&self) -> i64 {
        self.clock
            .utc_now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
    }

    fn trash_rel() -> WsPath {
        WsPath::from_disk(TRASH_DIR.to_string())
    }

    /// Why `rel` can't be written or moved to, or `None`: it is in the trash, or a folder on
    /// its way is a link (which could lead outside the root).
    async fn refusal(&self, rel: &WsPath) -> Result<(), StoreError> {
        let first = rel.as_str().split('/').next().unwrap_or_default();
        // The name as written, and its long form (an 8.3 short name of `.trash`).
        let (at, probe) = (self.root.join(first), self.probe.clone());
        let long = blocking(move || probe.long_form(&at))
            .await
            .map_err(|e| StoreError::io(rel, &e))?;
        let long_first = long
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if path_key(first) == path_key(TRASH_DIR) || path_key(&long_first) == path_key(TRASH_DIR) {
            return Err(StoreError::PathInvalid {
                path: rel.as_str().to_string(),
                why: "the personal trash is the suite's own folder".to_string(),
            });
        }
        let (root, probe, rel_owned) = (self.root.clone(), self.probe.clone(), rel.clone());
        let linked = blocking(move || {
            let mut at = root;
            let parts: Vec<&str> = rel_owned.as_str().split('/').collect();
            for part in &parts[..parts.len() - 1] {
                at.push(part);
                let id = match probe.identity(&at) {
                    Ok(id) => id,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
                    Err(e) => return Err(e),
                };
                if id.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
                    && probe
                        .reparse_tag(&at)?
                        .is_some_and(lil_platform::fs::is_name_surrogate)
                {
                    return Ok(true);
                }
            }
            Ok(false)
        })
        .await
        .map_err(|e| StoreError::io(rel, &e))?;
        if linked {
            return Err(link_refused(rel));
        }
        Ok(())
    }

    /// The bytes of the file at `rel`, `None` when there is none.
    ///
    /// # Errors
    /// `store.path_invalid` for a link or a folder, `store.hash_conflict` (`cloud_only`) for a
    /// cloud-only placeholder (never read), `store.io` otherwise.
    pub async fn read(&self, rel: &WsPath) -> Result<Option<Vec<u8>>, StoreError> {
        self.refusal(rel).await?;
        let (path, probe) = (rel.abs(&self.root), self.probe.clone());
        match blocking(move || see(probe.as_ref(), &path, read_all))
            .await
            .map_err(|e| StoreError::io(rel, &e))?
        {
            Seen::Found(bytes) => Ok(Some(bytes)),
            Seen::Missing => Ok(None),
            Seen::Placeholder => Err(StoreError::cloud_only(rel)),
            Seen::Link | Seen::Folder => Err(link_refused(rel)),
        }
    }

    /// The names of the files directly in the folder `rel` (relative to the root).
    ///
    /// # Errors
    /// `store.path_invalid` for the trash or a link on the way (the folder itself too);
    /// `store.io` when the folder can't be listed.
    pub async fn list(&self, rel: &str) -> Result<Vec<String>, StoreError> {
        self.refusal(&WsPath::from_disk(format!("{rel}/x"))).await?;
        let (dir, probe) = (
            rel.split('/').fold(self.root.clone(), |p, c| p.join(c)),
            self.probe.clone(),
        );
        blocking(move || list_files(probe.as_ref(), &dir))
            .await
            .map_err(|e| StoreError::io(&WsPath::from_disk(rel.to_string()), &e))
    }

    /// Writes `bytes` to a new file at `rel`, flushed to disk (a temp file moved into place,
    /// so a reader never sees half a file). Nothing is replaced: a file there already is an
    /// error.
    ///
    /// # Errors
    /// `store.path_invalid` for the trash or a link on the way, `store.io` (`store.busy`,
    /// `store.disk_full`) when a file call fails, also when a file is at `rel` already.
    pub async fn write_new(&self, rel: &WsPath, bytes: Vec<u8>) -> Result<(), StoreError> {
        self.refusal(rel).await?;
        let (path, tmp) = (rel.abs(&self.root), format!(".{}.tmp", self.ids.id("tmp")));
        blocking(move || fsops::write_new_file(&path, &tmp, &bytes, &FailPoints::none()))
            .await
            .map_err(|e| StoreError::io(rel, &e))
    }

    /// Moves the file `rel` to the personal trash with `reason` (module docs). The record is
    /// written first, so a payload never sits in the trash without one, and a move that fails
    /// takes it back out.
    ///
    /// # Errors
    /// `store.path_invalid` for the trash, a link or a folder; `store.io` when there is nothing
    /// at `rel` or a file call fails.
    pub async fn trash(&self, rel: &WsPath, reason: &str) -> Result<TrashRecord, StoreError> {
        self.refusal(rel).await?;
        let (root, probe) = (self.root.clone(), self.probe.clone());
        blocking(move || refuse_linked_trash(&root, probe.as_ref()))
            .await
            .map_err(|e| StoreError::io(rel, &e))?;
        let abs = rel.abs(&self.root);
        let sha256 = {
            let (path, probe) = (abs.clone(), self.probe.clone());
            let seen = blocking(move || see(probe.as_ref(), &path, read_all))
                .await
                .map_err(|e| StoreError::io(rel, &e))?;
            match seen {
                Seen::Found(bytes) => Some(lil_schema::sha256_hex(&bytes)),
                Seen::Placeholder => None,
                Seen::Link | Seen::Folder => return Err(link_refused(rel)),
                Seen::Missing => {
                    return Err(StoreError::Io {
                        path: rel.as_str().to_string(),
                        why: "there is nothing at this path to move to the trash".to_string(),
                    });
                }
            }
        };
        let now = self.now_utc();
        let day = self.root.join(TRASH_DIR).join(lil_schema::time::date(now));
        let mut made = None;
        for _ in 0..16 {
            let entry_id = self.ids.id("te");
            let record = TrashRecord {
                entry_id: entry_id.clone(),
                original_path: rel.as_str().to_string(),
                moved_utc: lil_schema::time::rfc3339(now),
                purge_after: lil_schema::time::rfc3339(now + TRASH_KEEP_DAYS * DAY),
                reason: reason.to_string(),
                sha256: sha256.clone(),
            };
            let text = serde_json::to_string_pretty(&record).map_err(|e| StoreError::Io {
                path: rel.as_str().to_string(),
                why: format!("the restore record can't be written: {e}"),
            })? + "\n";
            let (day_dir, dir) = (day.clone(), day.join(&entry_id));
            let probe = self.probe.clone();
            let created = blocking(move || {
                fsops::ensure_dir(&day_dir)?;
                refuse_link_at(&day_dir, probe.as_ref())?;
                fsops::create_dir_new(&dir)?;
                fsops::ensure_dir(&dir.join(PAYLOAD))?;
                fsops::write_temp(&dir, RECORD, text.as_bytes(), &FailPoints::none())?;
                Ok(dir)
            })
            .await;
            match created {
                Ok(dir) => {
                    made = Some((dir, record));
                    break;
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => {
                    let dir = day.join(&entry_id);
                    let _ = blocking(move || {
                        remove_if_empty(&dir.join(PAYLOAD));
                        remove_if_empty(&dir);
                        Ok(())
                    })
                    .await;
                    return Err(StoreError::io(rel, &e));
                }
            }
        }
        let Some((dir, record)) = made else {
            return Err(StoreError::Io {
                path: TRASH_DIR.to_string(),
                why: "no free trash entry name".to_string(),
            });
        };
        let target = dir.join(PAYLOAD).join(rel.file_name());
        let from = abs.clone();
        let to = target.clone();
        if let Err(e) = blocking(move || fsops::move_new(&from, &to)).await {
            let (dir, day) = (dir.clone(), day.clone());
            let _ = blocking(move || drop_empty_entry(&dir, &day)).await;
            return Err(StoreError::io(rel, &e));
        }
        tracing::info!(path = %rel, entry = %record.entry_id, "a personal resource moved to the trash");
        Ok(record)
    }

    /// The personal trash entries, oldest first (entries whose record can't be read aren't
    /// listed, and are never purged).
    ///
    /// # Errors
    /// `store.io` when the trash can't be listed.
    pub async fn trash_list(&self) -> Result<Vec<TrashRecord>, StoreError> {
        Ok(self.found().await?.into_iter().map(|e| e.record).collect())
    }

    async fn found(&self) -> Result<Vec<super::trash::Entry>, StoreError> {
        let (root, probe) = (self.root.clone(), self.probe.clone());
        blocking(move || entries(&root, probe.as_ref()))
            .await
            .map_err(|e| StoreError::io(&Personal::trash_rel(), &e))
    }

    /// Moves trash entry `entry_id` back to its original path and returns that path. Nothing
    /// is replaced.
    ///
    /// # Errors
    /// `trash.unknown` for no such entry; `trash.restore_conflict` when something is at the
    /// original path now; `store.path_invalid` for a record (edited by hand) that names the
    /// trash or a path through a link; `store.io` when a file call fails.
    pub async fn trash_restore(&self, entry_id: &str) -> Result<WsPath, StoreError> {
        let unknown = || StoreError::TrashUnknown(entry_id.to_string());
        if !is_entry_id(entry_id) {
            return Err(unknown());
        }
        let entry = self
            .found()
            .await?
            .into_iter()
            .find(|e| {
                e.dir
                    .file_name()
                    .is_some_and(|n| path_key(&n.to_string_lossy()) == path_key(entry_id))
            })
            .ok_or_else(unknown)?;
        let rel = WsPath::parse(&entry.record.original_path)?;
        self.refusal(&rel).await?;
        let payload = {
            let dir = entry.dir.join(PAYLOAD);
            let probe = self.probe.clone();
            let items = blocking(move || {
                refuse_link_at(&dir, probe.as_ref())?;
                list_with(probe.as_ref(), &dir)
            })
            .await
            .map_err(|e| StoreError::io(&rel, &e))?;
            let wanted = path_key(rel.file_name());
            match items.iter().find(|i| path_key(&i.name) == wanted) {
                Some(item) => item.path.clone(),
                None if items.len() == 1 => items[0].path.clone(),
                None => {
                    return Err(StoreError::Io {
                        path: rel.as_str().to_string(),
                        why: format!("the trash entry {entry_id} has no payload to restore"),
                    });
                }
            }
        };
        let target = rel.abs(&self.root);
        let (from, to) = (payload, target);
        let moved = blocking(move || {
            if lil_platform::fs::attributes(&to).is_ok() {
                return Err(io::Error::from_raw_os_error(183));
            }
            if let Some(parent) = to.parent() {
                fsops::ensure_dir(parent)?;
            }
            fsops::move_new(&from, &to)
        })
        .await;
        match moved {
            Ok(()) => {}
            Err(e) if e.raw_os_error().is_some_and(|c| c == 80 || c == 183) => {
                return Err(StoreError::RestoreConflict(rel));
            }
            Err(e) => return Err(StoreError::io(&rel, &e)),
        }
        let (dir, day) = (entry.dir.clone(), entry.day.clone());
        let cleaned = blocking(move || drop_empty_entry(&dir, &day)).await;
        if let Err(e) = cleaned {
            tracing::warn!(entry = %entry_id, error = %e, "a restored trash entry's record stays");
        }
        tracing::info!(path = %rel, entry = %entry_id, "a personal resource restored from the trash");
        Ok(rel)
    }

    /// The purge at `now_utc` (Unix seconds): removes the trash entries whose `purge_after` has
    /// passed, one of the only hard deletes in `lil-core` (CORE-110), and returns their
    /// `entry_id`s. A problem with one entry is logged and the rest go on.
    ///
    /// # Errors
    /// `store.io` when the trash can't be listed.
    pub async fn maintenance(&self, now_utc: i64) -> Result<Vec<String>, StoreError> {
        let mut purged = Vec::new();
        for entry in self.found().await? {
            let Some(due) = parse_rfc3339(&entry.record.purge_after) else {
                tracing::warn!(entry = %entry.dir.display(), "a trash entry whose purge_after can't be read is kept");
                continue;
            };
            if now_utc < due {
                continue;
            }
            let (dir, day) = (entry.dir.clone(), entry.day.clone());
            let (root, probe) = (self.root.clone(), self.probe.clone());
            match blocking(move || purge_entry(&root, probe.as_ref(), &dir, &day)).await {
                Ok(()) => {
                    tracing::info!(entry = %entry.record.entry_id, "personal trash entry purged");
                    purged.push(entry.record.entry_id.clone());
                }
                Err(e) => {
                    tracing::warn!(entry = %entry.dir.display(), error = %e, "a personal trash entry couldn't be purged; the next run tries again");
                }
            }
        }
        Ok(purged)
    }

    /// The purge now, on the store's clock.
    ///
    /// # Errors
    /// As [`Personal::maintenance`].
    pub async fn maintain(&self) -> Result<Vec<String>, StoreError> {
        self.maintenance(self.now_utc()).await
    }
}
