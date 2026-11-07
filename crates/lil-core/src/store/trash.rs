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
//! every restored file under `projects/*/assets/*/v<semver>/` (CORE-026); for a whole asset
//! folder, a sealed file its manifest's `hashes` lists is made read-only only where its SHA-256
//! still matches (CORE-157), and a mismatch is logged and left as it is.
//! [`Store::trash_restore_to`] restores under another name in the same folder ("restore as…",
//! CORE-037), optionally rewriting a restored file's bytes (a resource's new id): the rewritten
//! bytes are written as a new file at the target (never over one), and only then does the
//! entry go.
//!
//! **A whole asset** (CORE-157, ruling PF-12): `asset.trash` moves `projects/<p>/assets/<a>`
//! with [`Store::trash`] and the reason [`ASSET_TRASH_REASON`], as one entry, sealed version
//! folders included and unmodified. Like any folder, an asset holding a path its manifest
//! `locks` is never moved (ruling P3-R7, `store.locked`); [`Store::trash_locks_within`] lets
//! the tool check that before it asks you. Right after the move the fail point
//! `store.after_asset_trash` is hit (before the index hears of it). After a crash there, the next maintenance pass (right
//! after the workspace opens) tells the index the folder is gone, so its rows go rather than
//! stay as `missing`.
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
use super::paths::{WsPath, path_key, writable};
use super::versions::{files_under, set_readonly_all};
use super::write::{
    ChangeKind, Seen, Store, StoreError, StoreEvent, blocking, hash_file, link_refused, read_all,
    see,
};
use lil_platform::fs::Probe;

/// Days a trash entry is kept before it is purged (ARCH-058, CORE-037).
pub const TRASH_KEEP_DAYS: i64 = 30;
/// Days after its version's sign-off before a rejected run goes to the trash (CORE-032).
pub const REJECTED_KEEP_DAYS: i64 = 90;
/// The trash folder, workspace-relative.
pub const TRASH_DIR: &str = ".trash";
/// The `reason` of a whole asset moved to the trash by `asset.trash` (CORE-157).
pub const ASSET_TRASH_REASON: &str = "asset.trash";
/// A transform of a restored file's bytes ([`Store::trash_restore_to`]): a resource's new id
/// written inside the file (CORE-037). Its `Err` refuses the restore with `params.invalid`.
pub type RestoreRewrite = std::sync::Arc<dyn Fn(&[u8]) -> Result<Vec<u8>, String> + Send + Sync>;
/// The record beside each entry's payload.
pub(super) const RECORD: &str = "restore.json";
pub(super) const PAYLOAD: &str = "payload";
pub(super) const DAY: i64 = 86_400;

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
pub(super) struct Entry {
    pub(super) dir: PathBuf,
    pub(super) day: PathBuf,
    pub(super) record: TrashRecord,
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
pub fn parse_rfc3339(s: &str) -> Option<i64> {
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
pub(super) fn is_entry_id(id: &str) -> bool {
    id.strip_prefix("te_")
        .is_some_and(|hex| hex.len() == 16 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Fails closed (`PermissionDenied`) when `<root>\.trash` is a link (a symbolic link, junction
/// or mount point): the trash would be somewhere the suite doesn't own, so nothing is listed,
/// restored, purged or moved into it (CORE-110). A trash that doesn't exist is fine.
pub(super) fn refuse_linked_trash(root: &Path, probe: &dyn Probe) -> io::Result<()> {
    refuse_link_at(&root.join(TRASH_DIR), probe)
}

/// Fails closed (`PermissionDenied`) when `trash`, a folder in the trash path (`.trash`, a day
/// folder, an entry folder or its `payload`), is a link; one that doesn't exist is fine.
pub(super) fn refuse_link_at(trash: &Path, probe: &dyn Probe) -> io::Result<()> {
    let id = match probe.identity(trash) {
        Ok(id) => id,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if id.attributes & 0x400 != 0
        && probe
            .reparse_tag(trash)?
            .is_some_and(lil_platform::fs::is_name_surrogate)
    {
        tracing::warn!(trash = %trash.display(), "a folder in the trash path is a link, so the trash is not used; remove the link");
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} is a link, so the trash is not used; remove the link",
                trash.display()
            ),
        ));
    }
    Ok(())
}

/// Every entry under `<root>\.trash` whose `restore.json` parses. Links are never followed,
/// and each `restore.json` goes through `probe.identity` first: a cloud-only placeholder (or a
/// link) is skipped, never read, so listing the trash never downloads anything.
pub(super) fn entries(root: &Path, probe: &dyn Probe) -> io::Result<Vec<Entry>> {
    refuse_linked_trash(root, probe)?;
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

/// The path `name` in `original`'s folder ("restore as…", CORE-037): one path component.
///
/// # Errors
/// `params.invalid` for a name with a separator, or `.`/`..`; `store.path_invalid` for a path
/// the workspace refuses.
pub(super) fn restore_name(original: &WsPath, name: &str) -> Result<WsPath, StoreError> {
    if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
        return Err(StoreError::Invalid(format!(
            "to is {name:?}: one name in the folder the entry came from, without / or \\"
        )));
    }
    match original.parent() {
        Some(dir) => WsPath::parse(&format!("{dir}/{name}")),
        None => WsPath::parse(name),
    }
}

/// Whether `rel` is exactly an asset folder, `projects/<p>/assets/<a>`.
fn is_asset_folder(rel: &WsPath) -> bool {
    let parts: Vec<&str> = rel.as_str().split('/').collect();
    parts.len() == 4
        && parts[0].eq_ignore_ascii_case("projects")
        && parts[2].eq_ignore_ascii_case("assets")
}

/// The manifest of the asset folder `dir` as found on disk: `<asset_id>.manifest.json` for
/// one of `ids` (tried in order), else the first `*.manifest.json` directly in it; parsed.
fn folder_manifest(probe: &dyn Probe, dir: &Path, ids: &[&str]) -> Option<ManifestView> {
    let mut names: Vec<String> = ids.iter().map(|id| format!("{id}.manifest.json")).collect();
    let mut others: Vec<String> = list(dir)
        .ok()?
        .into_iter()
        .filter(|e| !e.folder && e.name.to_ascii_lowercase().ends_with(".manifest.json"))
        .map(|e| e.name)
        .collect();
    others.sort();
    names.extend(others);
    names.into_iter().find_map(|name| {
        let Ok(Seen::Found(bytes)) = see(probe, &dir.join(name), read_all) else {
            return None;
        };
        let text = std::str::from_utf8(&bytes).ok()?;
        Manifest::parse(text).ok().map(|(m, _)| m.view())
    })
}

/// Re-applies read-only to the sealed files of the restored asset folder `dir` (CORE-157): a
/// file in a version folder that the manifest's `hashes` lists is made read-only only when its
/// SHA-256 matches; any other file in a version folder (the snapshot, a legacy version's) is
/// made read-only as every restored sealed file is. `restored` is the folder's
/// workspace-relative path, `ids` the asset ids its manifest may be named after.
fn reseal_asset_folder(
    probe: &dyn Probe,
    dir: &Path,
    restored: &WsPath,
    ids: &[&str],
) -> io::Result<()> {
    let hashes = folder_manifest(probe, dir, ids)
        .map(|v| v.hashes)
        .unwrap_or_default();
    for (inside, file) in files_under(dir)? {
        let rel = WsPath::from_disk(format!("{restored}/{inside}"));
        if !in_version_folder(&rel) {
            continue;
        }
        let listed = hashes
            .iter()
            .find(|(key, _)| path_key(key) == path_key(&inside))
            .and_then(|(_, value)| lil_schema::bare_sha256(value).map(str::to_ascii_lowercase));
        if let Some(want) = listed {
            let found = match see(probe, &file, hash_file)? {
                Seen::Found(h) => Some(h.sha256),
                _ => None,
            };
            if found.as_deref() != Some(want.as_str()) {
                tracing::warn!(path = %rel, "a restored sealed file doesn't match its recorded hash; it isn't made read-only");
                continue;
            }
        }
        fsops::set_readonly(&file, true)?;
    }
    Ok(())
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
pub(super) fn remove_if_empty(path: &Path) {
    let _ = fsops::remove_empty_dir(path);
}

/// Takes an entry that holds no payload out of the trash again: its record, then its empty
/// folders (and the day's, when that is empty too).
pub(super) fn drop_empty_entry(dir: &Path, day: &Path) -> io::Result<()> {
    fsops::remove_file(&dir.join(RECORD))?;
    remove_if_empty(&dir.join(PAYLOAD));
    remove_if_empty(dir);
    remove_if_empty(day);
    Ok(())
}

/// Takes an entry restored with a rewrite out of the trash: its payload file `payload`, whose
/// bytes are now at the target with the new id, then the record and the empty folders.
pub(super) fn drop_rewritten_entry(payload: &Path, dir: &Path, day: &Path) -> io::Result<()> {
    fsops::remove_file(payload)?;
    drop_empty_entry(dir, day)
}

/// The purge of one entry: its whole folder, then the day's when that is empty (CORE-110).
///
/// The trash path is checked for links again right before the delete (`.trash`, the day and the
/// entry folder), so a link made since the entries were listed is never followed.
pub(super) fn purge_entry(
    root: &Path,
    probe: &dyn Probe,
    dir: &Path,
    day: &Path,
) -> io::Result<()> {
    refuse_linked_trash(root, probe)?;
    refuse_link_at(day, probe)?;
    refuse_link_at(dir, probe)?;
    fsops::remove_dir_all(dir)?;
    remove_if_empty(day);
    Ok(())
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
    /// (ruling P3-R7: a whole asset or project too); `store.git_busy` when git is still working
    /// after 60 s; `store.io` when there is nothing at `rel` or a file call fails (`store.busy` when
    /// it stays held, also a folder with a handle open inside it past the retries; nothing
    /// moves then).
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

    /// [`Store::trash`]'s lock refusals for `rel` alone, without moving anything: `store.locked`
    /// when `rel` (by any name) is in its manifest's `locks`, or is a folder holding a locked
    /// path (ruling P3-R7), naming that path. `asset.trash` checks this before it asks you, so
    /// you are never asked about a move that would be refused; the move checks it again.
    ///
    /// # Errors
    /// `store.locked`; `store.path_invalid` for a path the trash refuses; `store.io` when a
    /// manifest or a long form can't be read (fail closed).
    pub async fn trash_locks_within(&self, rel: &WsPath) -> Result<(), StoreError> {
        let long = self.reserved_on_disk(rel, "moved to the trash").await?;
        self.check_locks(rel).await?;
        self.check_long_locks(rel, &long).await?;
        self.refuse_locks_within(rel, &long).await
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
            let longs = self
                .long_lock_entries(&asset, &manifest, &m.view().locks)
                .await?;
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
        let (root, probe) = (self.root().to_path_buf(), self.probe().clone());
        blocking(move || refuse_linked_trash(&root, probe.as_ref()))
            .await
            .map_err(|e| StoreError::io(rel, &e))?;
        let seen = self
            .see_hash(&abs)
            .await
            .map_err(|e| StoreError::io(rel, &e))?;
        let folder = matches!(seen, Seen::Folder);
        let sha256 = match seen {
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
            let probe = self.probe().clone();
            let created = blocking(move || {
                fsops::ensure_dir(&day_dir)?;
                refuse_link_at(&day_dir, probe.as_ref())?;
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
            // A folder still refused with error 5 after the retries has a handle open inside
            // it (MS-FSA 2.1.5.15.12): it is held, `store.busy`, as a held file is.
            if lil_platform::fs::classify_move(&e, folder)
                == lil_platform::fs::RetryClass::SharingViolation
            {
                return Err(StoreError::Busy(rel.clone()));
            }
            return Err(StoreError::io(rel, &e));
        }
        tracing::info!(path = %rel, entry = %record.entry_id, "moved to the trash");
        if reason == ASSET_TRASH_REASON
            && let Err(e) = self.failpoints().hit("store.after_asset_trash")
        {
            tracing::warn!(path = %rel, error = %e, "fail point store.after_asset_trash");
        }
        self.emit(vec![StoreEvent {
            path: rel.clone(),
            kind: ChangeKind::Removed,
            sha256: None,
            identity: None,
        }])
        .await;
        Ok(record)
    }

    /// After a crash between `asset.trash`'s move and the index hearing of it (the fail point
    /// `store.after_asset_trash`): every entry of `records` with the reason
    /// [`ASSET_TRASH_REASON`] whose asset folder has nothing on disk now is told to the change
    /// sink as removed again, so the index drops the asset's rows instead of keeping them as
    /// `missing` (CORE-157). A removal the index has already applied changes nothing. Each
    /// maintenance pass does it first, in the background, so no open waits for the trash.
    async fn settle_trashed_assets(&self, records: &[TrashRecord]) {
        let mut gone: Vec<WsPath> = Vec::new();
        for record in records.iter().filter(|r| r.reason == ASSET_TRASH_REASON) {
            let Ok(rel) = WsPath::parse(&record.original_path) else {
                continue;
            };
            if !is_asset_folder(&rel) || gone.contains(&rel) {
                continue;
            }
            let abs = rel.abs(self.root());
            let absent = blocking(move || {
                Ok(matches!(
                    lil_platform::fs::attributes(&abs),
                    Err(e) if e.kind() == io::ErrorKind::NotFound
                ))
            })
            .await
            .unwrap_or(false);
            if absent {
                gone.push(rel);
            }
        }
        if gone.is_empty() {
            return;
        }
        tracing::info!(count = gone.len(), "trashed assets settled in the index");
        self.emit(
            gone.into_iter()
                .map(|path| StoreEvent {
                    path,
                    kind: ChangeKind::Removed,
                    sha256: None,
                    identity: None,
                })
                .collect(),
        )
        .await;
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
    /// docs): [`Store::trash_restore_to`] without `to` or a rewrite.
    ///
    /// # Errors
    /// As [`Store::trash_restore_to`].
    pub async fn trash_restore(&self, entry_id: &str) -> Result<WsPath, StoreError> {
        self.trash_restore_to(entry_id, None, None).await
    }

    /// The record of trash entry `entry_id`.
    ///
    /// # Errors
    /// `trash.unknown` for no such entry; `store.io` when the trash can't be listed.
    pub async fn trash_entry(&self, entry_id: &str) -> Result<TrashRecord, StoreError> {
        self.find_entry(entry_id).await.map(|e| e.record)
    }

    async fn find_entry(&self, entry_id: &str) -> Result<Entry, StoreError> {
        let unknown = || StoreError::TrashUnknown(entry_id.to_string());
        if !is_entry_id(entry_id) {
            return Err(unknown());
        }
        let root = self.root().to_path_buf();
        let probe = self.probe().clone();
        let found = blocking(move || entries(&root, probe.as_ref()))
            .await
            .map_err(|e| StoreError::io(&WsPath::from_disk(TRASH_DIR.to_string()), &e))?;
        found
            .into_iter()
            .find(|e| {
                e.dir
                    .file_name()
                    .is_some_and(|n| path_key(&n.to_string_lossy()) == path_key(entry_id))
            })
            .ok_or_else(unknown)
    }

    /// Moves trash entry `entry_id` back and returns the restored path (module docs): to its
    /// original path, or with `to` to the name `to` in the original's folder ("restore as…",
    /// CORE-037; `to` is one path component, `params.invalid` otherwise). With `rewrite`, the
    /// payload (a file) is read, rewritten and written as a new file at the target, and the
    /// entry goes only after that; a folder payload is then `params.invalid`. The entry's
    /// folder goes once it is empty.
    ///
    /// # Errors
    /// `workspace.read_only`; `trash.unknown` for no such entry; `trash.restore_conflict`
    /// (naming the target path) when something is at the target now; `params.invalid` for a
    /// bad `to`, or the rewrite's own refusal; `store.locked` when the target path (by any
    /// name) is in its manifest's `locks`; `store.path_invalid` when the target path is the
    /// trash, either workspace data folder, `.git`, the ledger, or any other path a write
    /// refuses (also through an 8.3 name); a `restore.json` edited by hand can name any of
    /// them. A path in a sealed version folder is the exception: it is restored, read-only
    /// again (module docs). Also `store.path_invalid` when a folder on the way to it is now a
    /// link. On every refusal the payload and `restore.json` stay in the trash, untouched.
    /// `store.io` when a file call fails.
    pub async fn trash_restore_to(
        &self,
        entry_id: &str,
        to: Option<&str>,
        rewrite: Option<RestoreRewrite>,
    ) -> Result<WsPath, StoreError> {
        self.writable_store()?;
        let entry = self.find_entry(entry_id).await?;
        let original = WsPath::parse(&entry.record.original_path)?;
        let rel = match to {
            None => original.clone(),
            Some(name) => restore_name(&original, name)?,
        };
        // A sealed path comes back only where it was sealed: never under another name.
        if to.is_some()
            && rel != original
            && (in_version_folder(&original) || in_version_folder(&rel))
        {
            return Err(StoreError::Invalid(format!(
                "{original} is in a sealed version folder, restored only under its own name"
            )));
        }
        // The trash's own refusals (by the long name too, so an 8.3 short name is refused like
        // the long one), then a write's, before anything moves; the entry stays as it is. A
        // sealed path is the one exception: it restores, read-only again (CORE-026).
        let long = self.reserved_on_disk(&rel, "restored to").await?;
        if !in_version_folder(&rel) {
            writable(&rel)?;
            writable(&long)?;
        }
        // Manifest `locks` (CORE-035), by the path as recorded and by its long form; a folder
        // with a locked entry beneath it now is never restored either (ruling P3-R7).
        self.check_locks(&rel).await?;
        self.check_long_locks(&rel, &long).await?;
        self.refuse_locks_within(&rel, &long).await?;
        let payload = {
            let dir = entry.dir.join(PAYLOAD);
            let probe = self.probe().clone();
            let items = blocking(move || {
                refuse_link_at(&dir, probe.as_ref())?;
                list(&dir)
            })
            .await
            .map_err(|e| StoreError::io(&rel, &e))?;
            let wanted = path_key(original.file_name());
            let named = items.iter().position(|i| path_key(&i.name) == wanted);
            match (named, items.len()) {
                (Some(i), _) => (items[i].path.clone(), items[i].folder),
                (None, 1) => (items[0].path.clone(), items[0].folder),
                _ => {
                    return Err(StoreError::Io {
                        path: rel.as_str().to_string(),
                        why: format!("the trash entry {entry_id} has no payload to restore"),
                    });
                }
            }
        };
        let (payload, payload_is_folder) = payload;
        if rewrite.is_some() && payload_is_folder {
            return Err(StoreError::Invalid(format!(
                "the trash entry {entry_id} holds a folder; only a file is restored with a new id \
                 inside it"
            )));
        }
        // The move checks only its target: a linked folder above it would let it land outside
        // the workspace.
        self.refuse_links(&rel).await?;
        let target = rel.abs(self.root());
        let _held = self.lock_path(&rel).await;
        let (from, to_abs) = (payload.clone(), target.clone());
        let mut written_sha256 = None;
        let moved = match rewrite {
            // A restored folder also retries error 5 (ruling Q19).
            None => {
                self.fs_retry_move(Some("store.replace"), &payload, move || {
                    if lil_platform::fs::attributes(&to_abs).is_ok() {
                        return Err(io::Error::from_raw_os_error(183));
                    }
                    if let Some(parent) = to_abs.parent() {
                        fsops::ensure_dir(parent)?;
                    }
                    fsops::move_new(&from, &to_abs)
                })
                .await
            }
            Some(rewrite) => {
                let probe = self.probe().clone();
                let read = blocking(move || match see(probe.as_ref(), &from, read_all)? {
                    Seen::Found(bytes) => Ok(bytes),
                    _ => Err(io::Error::other("the payload can't be read")),
                })
                .await
                .map_err(|e| StoreError::io(&rel, &e))?;
                let bytes = rewrite(&read).map_err(StoreError::Invalid)?;
                written_sha256 = Some(lil_schema::sha256_hex(&bytes));
                let tmp = format!(".{}.tmp", self.ids().id("tmp"));
                let fp = self.failpoints().clone();
                blocking(move || {
                    if lil_platform::fs::attributes(&to_abs).is_ok() {
                        return Err(io::Error::from_raw_os_error(183));
                    }
                    fsops::write_new_file(&to_abs, &tmp, &bytes, &fp)
                })
                .await
            }
        };
        match moved {
            Ok(()) => {}
            Err(e) if e.raw_os_error().is_some_and(|c| c == 80 || c == 183) => {
                return Err(StoreError::RestoreConflict(rel));
            }
            Err(e) => return Err(StoreError::io(&rel, &e)),
        }
        // Every restored file in a sealed version folder is read-only again, whether the
        // restored path is the file, its version folder, or a folder above it; a whole asset
        // folder's listed sealed files only where their hashes match (module docs).
        let (path, restored, probe) = (target.clone(), rel.clone(), self.probe().clone());
        let ids = [
            rel.file_name().to_string(),
            original.file_name().to_string(),
        ];
        let sealed = blocking(move || {
            if in_version_folder(&restored) {
                return set_readonly_all(&path, true);
            }
            if lil_platform::fs::attributes(&path)? & 0x10 == 0 {
                return Ok(());
            }
            if is_asset_folder(&restored) {
                let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
                return reseal_asset_folder(probe.as_ref(), &path, &restored, &ids);
            }
            for (inside, file) in files_under(&path)? {
                let rel = WsPath::from_disk(format!("{restored}/{inside}"));
                if in_version_folder(&rel) {
                    fsops::set_readonly(&file, true)?;
                }
            }
            Ok(())
        })
        .await;
        if let Err(e) = sealed {
            tracing::warn!(path = %rel, error = %e, "a restored sealed file couldn't be made read-only again");
        }
        // A rewritten restore leaves the payload behind: it goes with the entry, now that its
        // bytes are at the target with the new id.
        let rewritten = written_sha256.is_some().then(|| payload.clone());
        let (dir, day) = (entry.dir.clone(), entry.day.clone());
        let cleaned = blocking(move || match rewritten {
            Some(file) => drop_rewritten_entry(&file, &dir, &day),
            None => drop_empty_entry(&dir, &day),
        })
        .await;
        if let Err(e) = cleaned {
            tracing::warn!(entry = %entry_id, error = %e, "a restored trash entry's record stays");
        }
        tracing::info!(path = %rel, entry = %entry_id, "restored from the trash");
        self.emit(vec![StoreEvent {
            path: rel.clone(),
            kind: ChangeKind::Created,
            sha256: written_sha256.or_else(|| entry.record.sha256.clone()),
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
        // Trashed assets the index may still hold after a crash (module docs), before the
        // purge.
        let records: Vec<TrashRecord> = found.iter().map(|e| e.record.clone()).collect();
        self.settle_trashed_assets(&records).await;
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
            let (root, probe) = (root.clone(), self.probe().clone());
            match blocking(move || purge_entry(&root, probe.as_ref(), &dir, &day)).await {
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
    /// under the asset's version lock, taken only when a run is due (a first read without it
    /// decides). A due run whose folder is already in the trash
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
        let read_view = || async {
            let Some(bytes) = self.read(&manifest_rel).await? else {
                return Ok::<_, StoreError>(None);
            };
            Ok(std::str::from_utf8(&bytes)
                .ok()
                .and_then(|t| Manifest::parse(t).ok())
                .map(|(m, _)| m.view()))
        };
        let wanted = |view: &ManifestView, run: &CandidateRun| {
            let due = signoff_date(view, run).map(|d| d + REJECTED_KEEP_DAYS * DAY);
            run.status.as_deref() == Some("rejected")
                && !run.pinned
                && run.trashed_utc.is_none()
                && due.is_some_and(|due| now_utc >= due)
        };
        // The version lock only when a run is due: a pass over an asset with nothing to move
        // never holds it, so it never looks like a sign-off to `asset.trash`'s in-use check
        // (CORE-157). The manifest is read again under the lock.
        let due_now = read_view()
            .await?
            .is_some_and(|view| view.candidate_runs.iter().any(|r| wanted(&view, r)));
        if !due_now {
            return Ok(Vec::new());
        }
        let _guard = self.version_lock(project, asset_id).lock_owned().await;
        let Some(view) = read_view().await? else {
            return Ok(Vec::new());
        };
        let mut trashed = Vec::new();
        for run in &view.candidate_runs {
            if !wanted(&view, run) {
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
