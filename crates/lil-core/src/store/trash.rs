//! The trash (ARCH-058, CORE-037, CORE-110) and the core's retention maintenance (CORE-032).
//!
//! [`Store::trash`] moves a file or folder to
//! `<ws>\.trash\<YYYY-MM-DD>\<te_16hex>\payload\<original name>` and writes `restore.json`
//! beside `payload`: `{"entry_id", "original_path", "moved_utc", "purge_after", "reason",
//! "sha256"?}`. `original_path` is workspace-relative with `/` separators, and `purge_after`
//! is 30 days after `moved_utc`. The record is written first, so a payload never sits in the
//! trash without one; a move that fails takes the record back out.
//!
//! [`Store::trash_restore`] moves the payload back to its original path, never over a file
//! that is there now (`trash.restore_conflict`), and sets the read-only attribute again on
//! every restored file under `projects/*/assets/*/v<semver>/` (CORE-026).
//!
//! [`Store::maintenance`], which the core runs in the background, right after a workspace
//! opens and then every 24 hours ([`Store::maintenance_until`] stops between entries and
//! assets once the workspace closes):
//! - purges the entries whose `purge_after` has passed. This purge is one of the only hard
//!   deletes in `lil-core` (CORE-110), with stale temps and settled backups, empty staging
//!   handles, a failed seal's own copies and the index rebuild. `fsops::remove_dir_all` clears
//!   the read-only attribute first, so a sealed payload is purged too. An entry whose record
//!   can't be read, or is a cloud-only placeholder (never downloaded), is kept;
//! - moves the folders of `rejected` runs to the trash 90 days after their version's sign-off
//!   date, unless `pinned` (CORE-032), and records `trashed_utc` on the run with
//!   `edit_manifest` (the one change I5 allows on a signed-off run). A run's sign-off date is
//!   the `signed_off.date` of the run signed off as its `rejected_at_version` (the version
//!   whose sign-off rejected it), else as its `target_version`, or else of the
//!   first sign-off on or after the day the run was created (a sign-off with a confirmed bump
//!   seals another version than the run's target); a run with neither is kept. A due run
//!   whose folder is already in the trash without `trashed_utc` (the manifest edit failed, or
//!   the core stopped, after the move) gets the entry's `moved_utc` as its `trashed_utc`.
//!   Assets without a `candidates/` folder are skipped before their manifest is read.

use std::io;
use std::path::{Path, PathBuf};

use lil_schema::{CandidateRun, Manifest, ManifestView};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::fsops;
use super::git_gate::GIT_WAIT;
use super::order::{list, version_folder};
use super::paths::{WsPath, long_rel, path_key, writable};
use super::versions::{files_under, set_readonly_all};
use super::write::{
    ChangeKind, Seen, Store, StoreError, StoreEvent, blocking, link_refused, read_all, see,
};
use lil_platform::fs::Probe;

/// Days a trash entry is kept before it is purged (ARCH-058, CORE-037).
pub const TRASH_KEEP_DAYS: i64 = 30;
/// Days after its version's sign-off before a rejected run goes to the trash (CORE-032).
pub const REJECTED_KEEP_DAYS: i64 = 90;
/// The trash folder, workspace-relative.
pub const TRASH_DIR: &str = ".trash";
/// The record beside each entry's payload.
const RECORD: &str = "restore.json";
const PAYLOAD: &str = "payload";
const DAY: i64 = 86_400;

/// A trash entry's `restore.json` (Appendix E "Workspace areas").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TrashRecord {
    /// `te_<16 hex>`, also the name of the entry's folder.
    pub entry_id: String,
    /// Workspace-relative, `/` separators.
    pub original_path: String,
    /// RFC 3339 UTC.
    pub moved_utc: String,
    /// RFC 3339 UTC: `moved_utc` + 30 days.
    pub purge_after: String,
    pub reason: String,
    /// The file's SHA-256 (bare hex); absent for a folder or a cloud-only file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// What one [`Store::maintenance`] run did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaintenanceReport {
    /// The `entry_id`s of the purged trash entries.
    pub purged: Vec<String>,
    /// The rejected run folders moved to the trash.
    pub runs_trashed: Vec<WsPath>,
}

/// A trash entry found on disk.
#[derive(Debug, Clone)]
struct Entry {
    dir: PathBuf,
    day: PathBuf,
    record: TrashRecord,
}

/// Unix seconds of a `YYYY-MM-DD` date (the first ten characters of `s`), at 00:00 UTC.
fn parse_date(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = s.get(r)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let days_in = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        _ => return None,
    };
    if d < 1 || d > days_in {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * DAY)
}

/// Unix seconds of an RFC 3339 UTC time `YYYY-MM-DDTHH:MM:SS[.fraction]Z`.
fn parse_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[10] != b'T' || b[13] != b':' || b[16] != b':' || b.last() != Some(&b'Z') {
        return None;
    }
    match &b[19..b.len() - 1] {
        [] => {}
        [b'.', digits @ ..] if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) => {}
        _ => return None,
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = s.get(r)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    Some(parse_date(s)? + h * 3600 + mi * 60 + sec)
}

/// Whether `id` has the shape of an entry id, `te_<16 hex>`.
fn is_entry_id(id: &str) -> bool {
    id.strip_prefix("te_")
        .is_some_and(|hex| hex.len() == 16 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Every entry under `<root>\.trash` whose `restore.json` parses. Links are never followed,
/// and each `restore.json` goes through `probe.identity` first: a cloud-only placeholder (or a
/// link) is skipped, never read, so listing the trash never downloads anything.
fn entries(root: &Path, probe: &dyn Probe) -> io::Result<Vec<Entry>> {
    let mut found = Vec::new();
    for day in list(&root.join(TRASH_DIR))?
        .into_iter()
        .filter(|d| d.folder)
    {
        for entry in list(&day.path)?.into_iter().filter(|e| e.folder) {
            let record = entry.path.join(RECORD);
            let parsed = match see(probe, &record, read_all) {
                Ok(Seen::Found(bytes)) => serde_json::from_slice::<TrashRecord>(&bytes).ok(),
                Ok(Seen::Placeholder) => {
                    tracing::warn!(entry = %entry.path.display(), "a trash entry whose restore.json is cloud-only is skipped; make it available offline to list it");
                    continue;
                }
                _ => None,
            };
            match parsed {
                Some(record) => found.push(Entry {
                    dir: entry.path,
                    day: day.path.clone(),
                    record,
                }),
                None => {
                    tracing::warn!(entry = %entry.path.display(), "a trash entry without a readable restore.json is kept as it is");
                }
            }
        }
    }
    found.sort_by(|a, b| {
        (&a.record.moved_utc, &a.record.entry_id).cmp(&(&b.record.moved_utc, &b.record.entry_id))
    });
    Ok(found)
}

/// Whether `rel` is in a sealed version folder, `projects/<p>/assets/<a>/v<semver>/…`, or is
/// that folder.
fn in_version_folder(rel: &WsPath) -> bool {
    let parts: Vec<&str> = rel.as_str().split('/').collect();
    parts.len() >= 5
        && parts[0].eq_ignore_ascii_case("projects")
        && parts[2].eq_ignore_ascii_case("assets")
        && version_folder(parts[4]).is_some()
}

/// The sign-off date (Unix seconds) that starts a rejected run's 90 days (module docs).
fn signoff_date(view: &ManifestView, run: &CandidateRun) -> Option<i64> {
    let signed = || {
        view.candidate_runs
            .iter()
            .filter_map(|r| r.signed_off.as_ref())
            .filter_map(|s| parse_date(&s.date).map(|d| (s, d)))
    };
    // The sign-off that rejected it, which records its version on the run (Task 13).
    if let Some(rejecting) = run.rejected_at_version
        && let Some(d) = signed()
            .filter(|(s, _)| s.version == rejecting)
            .map(|(_, d)| d)
            .min()
    {
        return Some(d);
    }
    if let Some(target) = run.target_version
        && let Some(d) = signed()
            .filter(|(s, _)| s.version == target)
            .map(|(_, d)| d)
            .min()
    {
        return Some(d);
    }
    match run.created.as_deref().and_then(parse_date) {
        Some(created) => signed().map(|(_, d)| d).filter(|d| *d >= created).min(),
        // No creation day: the latest sign-off, so a run is never trashed early.
        None => signed().map(|(_, d)| d).max(),
    }
}

/// Removes `path` if it is an empty folder; anything else (a folder that isn't empty, one
/// that is gone) is left as it is.
fn remove_if_empty(path: &Path) {
    let _ = fsops::remove_empty_dir(path);
}

impl Store {
    /// Moves `rel` (a file or a folder) to the trash with `reason` (module docs). A sealed
    /// version folder, or anything in one, is never moved (CORE-026; it is refused as every
    /// write there is, also through an 8.3 name). A folder above versions, such as a whole
    /// asset, moves with its versions intact, and a restore makes their files read-only again.
    /// It waits at the git gate like every write (ARCH-083).
    ///
    /// # Errors
    /// `workspace.read_only`; `store.path_invalid` for the trash itself, either workspace data
    /// folder, `.git`, `registry/ledger.csv`, a sealed version folder or a path in one, by any
    /// name (an 8.3 short name too, or a long form that can't be read), or a link (also a
    /// linked folder on the way);
    /// `store.locked` for a path in its manifest's `locks`, by any name, or a folder holding one
    /// (ruling P3-R7: a whole asset or project too); `store.git_busy` when
    /// git is still working after 60 s; `store.io` when there is nothing at `rel` or a file
    /// call fails (`store.busy` when it stays held).
    pub async fn trash(&self, rel: &WsPath, reason: &str) -> Result<TrashRecord, StoreError> {
        self.writable_store()?;
        let long = self.reserved_on_disk(rel, "moved to the trash").await?;
        writable(rel)?;
        writable(&long)?;
        self.check_locks(rel).await?;
        self.check_long_locks(rel, &long).await?;
        // A folder holding a locked path is never moved either (ruling P3-R7).
        self.refuse_locks_within(rel, &long).await?;
        // The long form doesn't see through a junction: a linked folder on the way could reach
        // a sealed folder, `.lilith\` or `.git\`. Attributes only.
        self.refuse_links(rel).await?;
        self.git_gate().wait_clear(GIT_WAIT).await?;
        self.trash_now(rel, reason).await
    }

    /// Ruling P3-R7: `store.locked` when a manifest `locks` entry (CORE-035) and `long`, the
    /// long form of `rel`, overlap: the entry is `long` or under it (a folder holding a locked
    /// path, a whole asset or project too), or `long` is under the entry. Both sides are long
    /// forms: each entry is resolved under its asset folder with `probe.long_form`, so one
    /// written as an 8.3 alias still counts. The error names the locked path (`rel` when it is
    /// under the entry). The manifest is never locked by its own list, and an entry that isn't
    /// a valid path is skipped with a warning; a manifest, a listing or a long form that can't
    /// be read refuses the trash (fail closed).
    async fn refuse_locks_within(&self, rel: &WsPath, long: &WsPath) -> Result<(), StoreError> {
        let parts: Vec<&str> = long.as_str().split('/').collect();
        if !parts[0].eq_ignore_ascii_case("projects")
            || (parts.len() >= 3 && !parts[2].eq_ignore_ascii_case("assets"))
        {
            return Ok(());
        }
        let assets = if parts.len() >= 4 {
            vec![parts[..4].join("/")]
        } else {
            self.assets_under(&parts).await?
        };
        let target = long.key();
        for asset in assets {
            let asset_id = asset.rsplit('/').next().unwrap_or_default().to_string();
            let manifest = WsPath::from_disk(format!("{asset}/{asset_id}.manifest.json"));
            let Some(bytes) = self.read(&manifest).await? else {
                continue;
            };
            let Some((m, _)) = std::str::from_utf8(&bytes)
                .ok()
                .and_then(|t| Manifest::parse(t).ok())
            else {
                tracing::warn!(manifest = %manifest, "a manifest that doesn't parse; its locks aren't known");
                continue;
            };
            let mut entries = Vec::new();
            for entry in m.view().locks {
                let trimmed = entry.trim_end_matches('/');
                match WsPath::parse(&format!("{asset}/{trimmed}")) {
                    Ok(path) if !trimmed.is_empty() && path.key() != manifest.key() => {
                        entries.push(path);
                    }
                    Ok(_) => {}
                    Err(_) => {
                        tracing::warn!(manifest = %manifest, entry = %entry, "a locks entry that isn't a workspace path is skipped");
                    }
                }
            }
            if entries.is_empty() {
                continue;
            }
            let (root, probe) = (self.root().to_path_buf(), self.probe().clone());
            let longs = blocking(move || {
                Ok(entries
                    .iter()
                    .map(|e| long_rel(&root, e, &|p| probe.long_form(p)))
                    .collect::<Result<Vec<_>, _>>())
            })
            .await
            .map_err(|e| StoreError::io(&manifest, &e))??;
            for entry in longs {
                let key = entry.key();
                if key == target || key.starts_with(&format!("{target}/")) {
                    return Err(StoreError::Locked(entry));
                }
                if target.starts_with(&format!("{key}/")) {
                    return Err(StoreError::Locked(rel.clone()));
                }
            }
        }
        Ok(())
    }

    /// The asset folders, workspace-relative, under `parts`: `projects`, `projects/<p>` or
    /// `projects/<p>/assets` (links are never followed).
    async fn assets_under(&self, parts: &[&str]) -> Result<Vec<String>, StoreError> {
        let root = self.root().to_path_buf();
        let at = WsPath::from_disk(parts.join("/"));
        let parts: Vec<String> = parts.iter().map(|p| (*p).to_string()).collect();
        blocking(move || {
            let projects = if parts.len() == 1 {
                list(&root.join(&parts[0]))?
                    .into_iter()
                    .filter(|e| e.folder)
                    .map(|e| format!("{}/{}", parts[0], e.name))
                    .collect()
            } else {
                vec![format!("{}/{}", parts[0], parts[1])]
            };
            let mut assets = Vec::new();
            for project in projects {
                let dir = WsPath::from_disk(format!("{project}/assets")).abs(&root);
                for e in list(&dir)?.into_iter().filter(|e| e.folder) {
                    assets.push(format!("{project}/assets/{}", e.name));
                }
            }
            Ok(assets)
        })
        .await
        .map_err(|e| StoreError::io(&at, &e))
    }

    /// [`Store::trash`] after its checks.
    async fn trash_now(&self, rel: &WsPath, reason: &str) -> Result<TrashRecord, StoreError> {
        let abs = rel.abs(self.root());
        let _held = self.lock_path(rel).await;
        let sha256 = match self
            .see_hash(&abs)
            .await
            .map_err(|e| StoreError::io(rel, &e))?
        {
            Seen::Found(h) => Some(h.sha256),
            Seen::Folder | Seen::Placeholder => None,
            Seen::Link => return Err(link_refused(rel)),
            Seen::Missing => {
                return Err(StoreError::Io {
                    path: rel.as_str().to_string(),
                    why: "there is nothing at this path to move to the trash".to_string(),
                });
            }
        };
        let now = self.now_utc();
        let day = self
            .root()
            .join(TRASH_DIR)
            .join(lil_schema::time::date(now));
        let mut made = None;
        for _ in 0..16 {
            let entry_id = self.ids().id("te");
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
            let (day_dir, dir, fp) = (day.clone(), day.join(&entry_id), self.failpoints().clone());
            let created = blocking(move || {
                fsops::ensure_dir(&day_dir)?;
                fsops::create_dir_new(&dir)?;
                fsops::ensure_dir(&dir.join(PAYLOAD))?;
                fsops::write_temp(&dir, RECORD, text.as_bytes(), &fp)?;
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
        let to = dir.join(PAYLOAD).join(rel.file_name());
        let (from, target) = (abs.clone(), to.clone());
        // A folder (a rejected run, a trashed folder) also retries error 5 (ruling Q19).
        let moved = self
            .fs_retry_move(Some("store.replace"), &abs, move || {
                fsops::move_new(&from, &target)
            })
            .await;
        if let Err(e) = moved {
            // Nothing moved: the record goes, and the empty folders with it.
            let (dir, day) = (dir.clone(), day.clone());
            let _ = blocking(move || {
                fsops::remove_file(&dir.join(RECORD))?;
                remove_if_empty(&dir.join(PAYLOAD));
                remove_if_empty(&dir);
                remove_if_empty(&day);
                Ok(())
            })
            .await;
            return Err(StoreError::io(rel, &e));
        }
        tracing::info!(path = %rel, entry = %record.entry_id, "moved to the trash");
        self.emit(vec![StoreEvent {
            path: rel.clone(),
            kind: ChangeKind::Removed,
            sha256: None,
            identity: None,
        }])
        .await;
        Ok(record)
    }

    /// The trash entries, oldest first. Entries whose record can't be read aren't listed (and
    /// are never purged).
    ///
    /// # Errors
    /// `store.io` when the trash can't be listed.
    pub async fn trash_list(&self) -> Result<Vec<TrashRecord>, StoreError> {
        let root = self.root().to_path_buf();
        let probe = self.probe().clone();
        let found = blocking(move || entries(&root, probe.as_ref()))
            .await
            .map_err(|e| StoreError::io(&WsPath::from_disk(TRASH_DIR.to_string()), &e))?;
        Ok(found.into_iter().map(|e| e.record).collect())
    }

    /// Moves trash entry `entry_id` back to its original path and returns that path (module
    /// docs). The entry's folder goes once it is empty.
    ///
    /// # Errors
    /// `workspace.read_only`; `trash.unknown` for no such entry; `trash.restore_conflict`
    /// (naming the original path) when something is at the original path now;
    /// `store.locked` when the original path (by any name) is in its manifest's `locks`;
    /// `store.path_invalid` when the original path is the trash, either workspace data folder,
    /// `.git`, the ledger, or any other path a write refuses (also through an 8.3 name); a
    /// `restore.json` edited by hand can name any of them. A path in a sealed version folder is
    /// the exception: it is restored, read-only again (module docs). Also `store.path_invalid`
    /// when a folder on the way to it is now a link. On every refusal the payload and
    /// `restore.json` stay in the trash, untouched. `store.io` when a file call fails.
    pub async fn trash_restore(&self, entry_id: &str) -> Result<WsPath, StoreError> {
        self.writable_store()?;
        let unknown = || StoreError::TrashUnknown(entry_id.to_string());
        if !is_entry_id(entry_id) {
            return Err(unknown());
        }
        let root = self.root().to_path_buf();
        let probe = self.probe().clone();
        let found = blocking(move || entries(&root, probe.as_ref()))
            .await
            .map_err(|e| StoreError::io(&WsPath::from_disk(TRASH_DIR.to_string()), &e))?;
        let entry = found
            .into_iter()
            .find(|e| {
                e.dir
                    .file_name()
                    .is_some_and(|n| path_key(&n.to_string_lossy()) == path_key(entry_id))
            })
            .ok_or_else(unknown)?;
        let rel = WsPath::parse(&entry.record.original_path)?;
        // The trash's own refusals (by the long name too, so an 8.3 short name is refused like
        // the long one), then a write's, before anything moves; the entry stays as it is. A
        // sealed path is the one exception: it restores, read-only again (CORE-026).
        let long = self.reserved_on_disk(&rel, "restored to").await?;
        if !in_version_folder(&rel) {
            writable(&rel)?;
            writable(&long)?;
        }
        // Manifest `locks` (CORE-035), by the path as recorded and by its long form.
        self.check_locks(&rel).await?;
        self.check_long_locks(&rel, &long).await?;
        let payload = {
            let dir = entry.dir.join(PAYLOAD);
            let items = blocking(move || list(&dir))
                .await
                .map_err(|e| StoreError::io(&rel, &e))?;
            let wanted = path_key(rel.file_name());
            let named = items.iter().position(|i| path_key(&i.name) == wanted);
            match (named, items.len()) {
                (Some(i), _) => items[i].path.clone(),
                (None, 1) => items[0].path.clone(),
                _ => {
                    return Err(StoreError::Io {
                        path: rel.as_str().to_string(),
                        why: format!("the trash entry {entry_id} has no payload to restore"),
                    });
                }
            }
        };
        // The move checks only its target: a linked folder above it would let it land outside
        // the workspace.
        self.refuse_links(&rel).await?;
        let target = rel.abs(self.root());
        let _held = self.lock_path(&rel).await;
        let (from, to) = (payload.clone(), target.clone());
        // A restored folder also retries error 5 (ruling Q19).
        let moved = self
            .fs_retry_move(Some("store.replace"), &payload, move || {
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
        // Every restored file in a sealed version folder is read-only again, whether the
        // restored path is the file, its version folder, or a folder above it.
        let (path, restored) = (target.clone(), rel.clone());
        let sealed = blocking(move || {
            if in_version_folder(&restored) {
                return set_readonly_all(&path, true);
            }
            if lil_platform::fs::attributes(&path)? & 0x10 != 0 {
                for (inside, file) in files_under(&path)? {
                    let rel = WsPath::from_disk(format!("{restored}/{inside}"));
                    if in_version_folder(&rel) {
                        fsops::set_readonly(&file, true)?;
                    }
                }
            }
            Ok(())
        })
        .await;
        if let Err(e) = sealed {
            tracing::warn!(path = %rel, error = %e, "a restored sealed file couldn't be made read-only again");
        }
        let (dir, day) = (entry.dir.clone(), entry.day.clone());
        let cleaned = blocking(move || {
            fsops::remove_file(&dir.join(RECORD))?;
            remove_if_empty(&dir.join(PAYLOAD));
            remove_if_empty(&dir);
            remove_if_empty(&day);
            Ok(())
        })
        .await;
        if let Err(e) = cleaned {
            tracing::warn!(entry = %entry_id, error = %e, "a restored trash entry's record stays");
        }
        tracing::info!(path = %rel, entry = %entry_id, "restored from the trash");
        self.emit(vec![StoreEvent {
            path: rel.clone(),
            kind: ChangeKind::Created,
            sha256: entry.record.sha256.clone(),
            identity: None,
        }])
        .await;
        Ok(rel)
    }

    /// The core's own retention work at `now_utc` (Unix seconds; module docs): purges the
    /// trash entries whose `purge_after` has passed, and moves rejected runs to the trash 90
    /// days after their sign-off. A problem with one entry or asset is logged and the rest go
    /// on. It is [`Store::maintenance_until`] with a `stop` that never fires.
    ///
    /// # Errors
    /// `workspace.read_only`; `store.io` when the trash or the projects can't be listed.
    pub async fn maintenance(&self, now_utc: i64) -> Result<MaintenanceReport, StoreError> {
        self.maintenance_until(now_utc, &|| false).await
    }

    /// [`Store::maintenance`], checking `stop` before each trash entry and each asset: once it
    /// returns true the pass ends there with what it did so far (the core passes its
    /// workspace's "closed" flag, so a close never waits for a pass).
    ///
    /// # Errors
    /// As [`Store::maintenance`].
    pub async fn maintenance_until(
        &self,
        now_utc: i64,
        stop: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<MaintenanceReport, StoreError> {
        self.writable_store()?;
        let mut report = MaintenanceReport::default();
        let root = self.root().to_path_buf();
        let trash_rel = WsPath::from_disk(TRASH_DIR.to_string());

        // The purge.
        let found = {
            let root = root.clone();
            let probe = self.probe().clone();
            blocking(move || entries(&root, probe.as_ref()))
                .await
                .map_err(|e| StoreError::io(&trash_rel, &e))?
        };
        for entry in &found {
            if stop() {
                return Ok(report);
            }
            let Some(due) = parse_rfc3339(&entry.record.purge_after) else {
                tracing::warn!(entry = %entry.dir.display(), "a trash entry whose purge_after can't be read is kept");
                continue;
            };
            if now_utc < due {
                continue;
            }
            let (dir, day) = (entry.dir.clone(), entry.day.clone());
            match blocking(move || {
                fsops::remove_dir_all(&dir)?;
                remove_if_empty(&day);
                Ok(())
            })
            .await
            {
                Ok(()) => {
                    tracing::info!(entry = %entry.record.entry_id, "trash entry purged");
                    report.purged.push(entry.record.entry_id.clone());
                }
                Err(e) => {
                    tracing::warn!(entry = %entry.dir.display(), error = %e, "a trash entry couldn't be purged; the next run tries again");
                }
            }
        }
        // Every entry seen (purged in this pass or not), for runs whose folder went to the
        // trash without `trashed_utc`.
        let seen: Vec<TrashRecord> = found.into_iter().map(|e| e.record).collect();

        // Rejected runs.
        let projects = {
            let dir = root.join("projects");
            blocking(move || list(&dir))
                .await
                .map_err(|e| StoreError::io(&WsPath::from_disk("projects".to_string()), &e))?
        };
        for project in projects.into_iter().filter(|p| p.folder) {
            let assets = {
                let dir = project.path.join("assets");
                blocking(move || list(&dir)).await.unwrap_or_default()
            };
            for asset in assets.into_iter().filter(|a| a.folder) {
                if stop() {
                    return Ok(report);
                }
                // An asset without a `candidates/` folder has no run folder to move: its
                // manifest isn't even read.
                let candidates = asset.path.join("candidates");
                let has_runs = blocking(move || {
                    Ok(lil_platform::fs::attributes(&candidates).is_ok_and(|a| a & 0x10 != 0))
                })
                .await
                .unwrap_or(false);
                if !has_runs {
                    continue;
                }
                match self
                    .trash_rejected_runs(&project.name, &asset.name, now_utc, &seen)
                    .await
                {
                    Ok(trashed) => report.runs_trashed.extend(trashed),
                    Err(e) => {
                        tracing::warn!(project = %project.name, asset = %asset.name, error = %e, "rejected runs of an asset weren't checked");
                    }
                }
            }
        }
        Ok(report)
    }

    /// Moves the due rejected runs of one asset to the trash and records their `trashed_utc`,
    /// under the asset's version lock. A due run whose folder is already in the trash
    /// (`in_trash` has an entry from that folder: the manifest edit failed, or the core
    /// stopped, after the move) only gets its `trashed_utc`, the entry's `moved_utc`.
    async fn trash_rejected_runs(
        &self,
        project: &str,
        asset_id: &str,
        now_utc: i64,
        in_trash: &[TrashRecord],
    ) -> Result<Vec<WsPath>, StoreError> {
        let asset = WsPath::parse(&format!("projects/{project}/assets/{asset_id}"))?;
        let manifest_rel = asset.join(&format!("{asset_id}.manifest.json"))?;
        let _guard = self.version_lock(project, asset_id).lock_owned().await;
        let Some(bytes) = self.read(&manifest_rel).await? else {
            return Ok(Vec::new());
        };
        let Some((manifest, _)) = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|t| Manifest::parse(t).ok())
        else {
            return Ok(Vec::new());
        };
        let view = manifest.view();
        let mut trashed = Vec::new();
        for run in &view.candidate_runs {
            let due = signoff_date(&view, run).map(|d| d + REJECTED_KEEP_DAYS * DAY);
            let wanted = run.status.as_deref() == Some("rejected")
                && !run.pinned
                && run.trashed_utc.is_none()
                && due.is_some_and(|due| now_utc >= due);
            if !wanted {
                continue;
            }
            let folder = asset.join(&format!("candidates/{}", run.run_id))?;
            let abs = folder.abs(self.root());
            let present = blocking(move || Ok(lil_platform::fs::attributes(&abs).is_ok()))
                .await
                .unwrap_or(false);
            let stamp = if present {
                let locked = match self.check_locks(&folder).await {
                    Ok(()) => self.refuse_locks_within(&folder, &folder).await,
                    Err(e) => Err(e),
                };
                if let Err(e) = locked {
                    tracing::warn!(run = %folder, error = %e, "a locked rejected run stays");
                    continue;
                }
                self.trash_now(&folder, "rejected candidates, 90 days after sign-off")
                    .await?;
                tracing::info!(run = %folder, "a rejected run went to the trash");
                trashed.push(folder.clone());
                lil_schema::time::rfc3339(now_utc)
            } else {
                let key = path_key(folder.as_str());
                let Some(entry) = in_trash.iter().find(|r| path_key(&r.original_path) == key)
                else {
                    continue;
                };
                tracing::info!(run = %folder, entry = %entry.entry_id, "a rejected run already in the trash gets its trashed_utc");
                entry.moved_utc.clone()
            };
            let id = run.run_id.clone();
            self.edit_manifest(&manifest_rel, &|m: &mut Manifest| {
                for entry in m.runs_mut() {
                    let this = entry.get("run_id").and_then(Value::as_str) == Some(id.as_str());
                    if this && !entry.contains_key("trashed_utc") {
                        entry.insert("trashed_utc".to_string(), Value::from(stamp.as_str()));
                    }
                }
                Ok(())
            })
            .await?;
        }
        Ok(trashed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: ARCH-058
    fn dates_parse_as_utc() {
        assert_eq!(parse_date("1970-01-01"), Some(0));
        assert_eq!(parse_date("2027-01-15"), Some(1_799_971_200));
        assert_eq!(parse_date("2028-02-29"), Some(1_835_395_200));
        assert_eq!(parse_date("2027-02-29"), None);
        assert_eq!(parse_date("2027-13-01"), None);
        assert_eq!(parse_date("27-01-15"), None);
        assert_eq!(parse_rfc3339("2027-01-15T08:00:00Z"), Some(1_800_000_000));
        assert_eq!(
            parse_rfc3339("2027-01-15T08:00:00.250Z"),
            Some(1_800_000_000)
        );
        for bad in [
            "2027-01-15T08:00:00",
            "2027-01-15 08:00:00Z",
            "2027-01-15T24:00:00Z",
            "2027-01-15T08:00:00+01:00",
            "2027-01-15T08:00:00.Z",
        ] {
            assert_eq!(parse_rfc3339(bad), None, "{bad}");
        }
        for (unix, text) in [
            (0, "1970-01-01T00:00:00Z"),
            (1_800_000_000, "2027-01-15T08:00:00Z"),
            (1_835_438_400, "2028-02-29T12:00:00Z"),
        ] {
            assert_eq!(lil_schema::time::rfc3339(unix), text);
            assert_eq!(parse_rfc3339(text), Some(unix));
        }
    }

    #[test] // spec: ARCH-058
    fn entry_ids_are_te_and_16_hex() {
        assert!(is_entry_id("te_0123456789abcdef"));
        assert!(!is_entry_id("te_0123456789abcde"));
        assert!(!is_entry_id("sh_0123456789abcdef"));
        assert!(!is_entry_id("te_0123456789abcdeg"));
        assert!(!is_entry_id("../te_0123456789abcdef"));
    }

    #[test] // spec: CORE-032
    fn a_rejected_run_counts_from_the_sign_off_that_rejected_it() {
        let text = serde_json::json!({
            "asset_id": "a", "project": "p",
            "candidate_runs": [
                {"run_id": "r_1", "status": "selected", "created": "2026-10-01T00:00:00Z",
                 "target_version": "1.0.0", "files": [],
                 "signed_off": {"by": "cli", "date": "2026-10-02", "version": "1.0.0"}},
                {"run_id": "r_2", "status": "selected", "created": "2026-10-05T00:00:00Z",
                 "target_version": "1.1.0", "files": [],
                 "signed_off": {"by": "cli", "date": "2026-11-20", "version": "2.0.0"}},
                {"run_id": "r_3", "status": "rejected", "created": "2026-10-01T00:00:00Z",
                 "target_version": "1.0.0", "rejected_at_version": "2.0.0", "files": []},
                {"run_id": "r_4", "status": "rejected", "created": "2026-10-01T00:00:00Z",
                 "target_version": "1.0.0", "files": []},
            ],
        })
        .to_string();
        let view = Manifest::parse(&text).unwrap().0.view();
        // Recorded: the sign-off of 2.0.0 rejected it, though it targeted 1.0.0.
        assert_eq!(
            signoff_date(&view, &view.candidate_runs[2]),
            parse_date("2026-11-20")
        );
        // Not recorded (older manifests): the sign-off of its target version.
        assert_eq!(
            signoff_date(&view, &view.candidate_runs[3]),
            parse_date("2026-10-02")
        );
    }

    #[test] // spec: CORE-026
    fn version_folders_are_recognised() {
        for yes in [
            "projects/p/assets/a/v1.0.0",
            "projects/p/assets/a/V2.1.3/x.png",
        ] {
            assert!(in_version_folder(&WsPath::parse(yes).unwrap()), "{yes}");
        }
        for no in [
            "projects/p/assets/a/candidates/v1.0.0/x.png",
            "projects/p/assets/a/v1.0/x.png",
            "notes/v1.0.0/x.png",
        ] {
            assert!(!in_version_folder(&WsPath::parse(no).unwrap()), "{no}");
        }
    }
}
