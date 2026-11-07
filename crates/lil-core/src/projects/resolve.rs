//! The one resolver every ref-to-folder lookup goes through (ARCH-125, APP-019, SP-08;
//! Appendix H "`registry/redirects.yaml`" and "`ref.resolve`").
//!
//! **Resolution** of a name `<project>/<asset_id>`:
//! 1. a live asset folder at the name wins. Live is decided on disk: the folder
//!    `projects/<p>/assets/<a>/` holds its manifest as the index finds one
//!    (`<asset_id>.manifest.json`, else a `*.manifest.json`: a folder moved or restored under
//!    another name keeps its manifest's file name), and neither it nor a folder on the way to
//!    it is a junction or symbolic link. The index can lag behind an outside change, so it
//!    never decides liveness;
//! 2. else the newest entry of `registry/redirects.yaml` whose `from` is the name is followed
//!    (the greatest `moved_utc`; on a tie, the later entry in the file; a `moved_utc` that
//!    doesn't parse sorts before any that does), and the next name is resolved the same way,
//!    to at most 64 redirects;
//! 3. a name seen twice is `ref.redirect_broken` with `reason: "cycle"` and the names visited
//!    as `chain` (the repeated name last). A chain that needs a 65th redirect is broken the
//!    same way: the limit exists to stop loops, and Appendix D's `reason` is closed;
//! 4. a name that ends with no live folder and no redirect: the trash is listed (only now,
//!    once per resolver), and the newest entry whose original path is that asset's folder is
//!    `asset.trashed` with its `entry_id`; else, after at least one redirect,
//!    `ref.redirect_broken` with `reason: "target_missing"` and `to` the end; a name that
//!    was never redirected is `asset.unknown`.
//!
//! A sealed ref's `#sha256` is checked last, at the end of the chain, against the version's
//! content hash as `Store::versions` gives it (the CORE-047 hash `asset.get` lists):
//! `ref.hash_mismatch` when they differ. A redirect never blocks anything; it is only read.
//!
//! [`Resolver::load`] reads the redirects once, and checks liveness on disk once for every
//! name the file mentions, so [`Resolver::names_resolving_to`] and [`Resolver::broken_from`]
//! need no I/O. A name the file doesn't mention is checked when it is first resolved.
//!
//! **A redirects file that can't be read or parsed** blocks only what needs it: `load` keeps
//! its error aside ([`Resolver::file_error`]: `manifest.needs_repair` for one that doesn't
//! parse, the store's error, such as `store.io` for a cloud-only placeholder or a folder at the
//! path, for one that can't be read); a live name still resolves (step 1 never reads the
//! file), and only a name that would have to follow a redirect gets that error, never a silent
//! "no redirects". [`Resolver::names_resolving_to`] is then the asset's own name only. `load`
//! never fails because of the file.

use std::collections::{BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use lil_platform::fs::Probe;
use lil_protocol::ErrorBody;
use lil_protocol::codes::{
    ASSET_TRASHED, ASSET_UNKNOWN, MANIFEST_NEEDS_REPAIR, REF_HASH_MISMATCH, REF_INVALID,
    REF_REDIRECT_BROKEN,
};
use lil_schema::{AssetRef, LinkRef, Version, is_asset_id, is_run_id, is_slug};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;
use tokio::sync::OnceCell;

use super::redirects::{REDIRECTS_YAML, Redirect, name_of, parse_redirects, redirects_path};
use crate::index::{EdgeRow, Index};
use crate::store::trash::parse_rfc3339;
use crate::store::{Store, TrashRecord, WsPath};

/// The most redirects one resolution follows (Appendix H).
pub const MAX_HOPS: usize = 64;

/// `(project, asset_id)`.
type Name = (String, String);

/// Where a name resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub project: String,
    pub asset_id: String,
    /// The names followed to get there, `<project>/<asset_id>`, the asked name first; empty
    /// when the name itself is live.
    pub redirected_from: Vec<String>,
}

/// `Resolved.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedKind {
    Sealed,
    Candidate,
    Asset,
    Inbox,
    Outside,
}

/// `ref.resolve`'s result, and `asset.get`'s `uses[].resolved` (Appendix H).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Resolved {
    /// `sealed`, `candidate`, `asset`, `inbox` or `outside`.
    pub kind: ResolvedKind,
    /// The ref at the place it resolved to: a sealed version's carries the version's content
    /// hash when it is known.
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Workspace-relative, `/` separators.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// A sealed version is read-only.
    pub read_only: bool,
    /// The names followed to get there (`<project>/<asset_id>`), the asked name first.
    pub redirected_from: Vec<String>,
    /// For a ref that carries `#sha256=`: whether it matched (a mismatch is an error). Absent
    /// when the ref carries none, or the version's content hash isn't known (a legacy version
    /// holding a cloud-only file).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256_ok: Option<bool>,
}

impl Resolved {
    fn outside() -> Resolved {
        Resolved {
            kind: ResolvedKind::Outside,
            r#ref: None,
            project: None,
            asset_id: None,
            version: None,
            file: None,
            read_only: false,
            redirected_from: Vec::new(),
            sha256_ok: None,
        }
    }

    fn at(kind: ResolvedKind, name: &Name) -> Resolved {
        Resolved {
            kind,
            project: Some(name.0.clone()),
            asset_id: Some(name.1.clone()),
            read_only: kind == ResolvedKind::Sealed,
            ..Resolved::outside()
        }
    }
}

/// How a walk along the redirects ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Walk {
    /// At a live folder; `chain` is every name visited, `end` last.
    Live(Vec<Name>),
    /// At a name with no live folder and no redirect (the last of `chain`).
    DeadEnd(Vec<Name>),
    /// A name came again (the last of `chain` is the repeat).
    Cycle(Vec<Name>),
    /// A 65th redirect was needed.
    TooLong(Vec<Name>),
}

/// The index, the redirects and, on demand, the trash list, read once (module docs).
pub struct Resolver<'a> {
    store: &'a Store,
    index: &'a Index,
    redirects: Vec<Redirect>,
    /// Why the redirects file can't be read or parsed (module docs); its entries are then
    /// empty.
    broken: Option<ErrorBody>,
    /// Each `from` and the entry followed from it.
    newest: HashMap<Name, usize>,
    /// Liveness on disk, checked once per name.
    live: Mutex<HashMap<Name, bool>>,
    trash: OnceCell<Vec<TrashRecord>>,
    trash_listed: AtomicBool,
}

impl std::fmt::Debug for Resolver<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resolver")
            .field("redirects", &self.redirects.len())
            .field("trash_listed", &self.trash_was_listed())
            .finish_non_exhaustive()
    }
}

const DIRECTORY: u32 = 0x10;
const REPARSE_POINT: u32 = 0x400;

/// Whether `path`, with `attributes`, is a link: a reparse point whose tag names another file
/// (a junction or symbolic link), as the store decides (`store/write.rs`). Other reparse
/// points (a OneDrive cloud file or folder, a deduplicated file) hold their own data and are
/// not links. A tag that can't be read counts as a link (fail closed).
fn is_link(probe: &dyn Probe, path: &Path, attributes: u32) -> bool {
    attributes & REPARSE_POINT != 0
        && probe.reparse_tag(path).map_or(true, |tag| {
            tag.is_some_and(lil_platform::fs::is_name_surrogate)
        })
}

/// Whether `name` has a live folder on disk (module docs): `projects/<p>/assets/<a>/` is a
/// folder, neither it nor a folder on the way to it is a link (a junction or symbolic link
/// leads outside the workspace's own folders), and it holds its manifest as the index finds
/// one, `<asset_id>.manifest.json`, else a `*.manifest.json` (a folder moved, copied or
/// restored under another name keeps its manifest's file name). Attributes, reparse tags and
/// the folder's listing only; any failure is not live.
fn on_disk(probe: &dyn Probe, root: &Path, name: &Name) -> bool {
    let folder = |rel: String| -> Option<PathBuf> {
        let path = WsPath::parse(&rel).ok()?.abs(root);
        let id = probe.identity(&path).ok()?;
        (id.attributes & DIRECTORY != 0 && !is_link(probe, &path, id.attributes)).then_some(path)
    };
    let (p, a) = (&name.0, &name.1);
    let on_the_way = [
        "projects".to_string(),
        format!("projects/{p}"),
        format!("projects/{p}/assets"),
    ];
    if on_the_way.into_iter().any(|rel| folder(rel).is_none()) {
        return false;
    }
    let Some(dir) = folder(format!("projects/{p}/assets/{a}")) else {
        return false;
    };
    manifest_in(probe, &dir, a).is_some()
}

/// The manifest file of the asset folder `dir` (of `asset_id`) as the index finds it:
/// `<asset_id>.manifest.json`, else the first `*.manifest.json` directly in it; a file, not a
/// link.
fn manifest_in(probe: &dyn Probe, dir: &Path, asset_id: &str) -> Option<String> {
    let is_file = |path: &Path| {
        probe
            .identity(path)
            .is_ok_and(|id| id.attributes & DIRECTORY == 0 && !is_link(probe, path, id.attributes))
    };
    let canonical = format!("{asset_id}.manifest.json");
    if is_file(&dir.join(&canonical)) {
        return Some(canonical);
    }
    let mut found: Vec<String> = probe
        .read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .ends_with(".manifest.json")
                && is_file(&e.path())
        })
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    found.sort();
    found.into_iter().next()
}

/// `names`' liveness, checked on a blocking thread.
async fn check_live(store: &Store, names: Vec<Name>) -> HashMap<Name, bool> {
    let (probe, root): (Arc<dyn Probe>, PathBuf) = (store.probe().clone(), store.root().into());
    tokio::task::spawn_blocking(move || {
        names
            .into_iter()
            .map(|n| {
                let live = on_disk(probe.as_ref(), &root, &n);
                (n, live)
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

fn names(chain: &[Name]) -> Vec<String> {
    chain.iter().map(name_of).collect()
}

/// An RFC 3339 UTC time (`YYYY-MM-DDTHH:MM:SS[.fraction]Z`) as `(seconds, nanoseconds)`, so a
/// fraction orders too; `None` when it doesn't parse, which sorts before any time.
fn instant(utc: &str) -> Option<(i64, u32)> {
    let secs = parse_rfc3339(utc)?;
    let nanos = match utc.split_once('.') {
        None => 0,
        Some((_, fraction)) => {
            let digits: String = fraction
                .trim_end_matches('Z')
                .chars()
                .chain(std::iter::repeat('0'))
                .take(9)
                .collect();
            digits.parse().ok()?
        }
    };
    Some((secs, nanos))
}

/// `ref.redirect_broken` for a walk that loops, is too long or ends nowhere.
fn broken(chain: &[Name], reason: &str, why: String, to: Option<&Name>) -> ErrorBody {
    let mut e = ErrorBody::new(
        &REF_REDIRECT_BROKEN,
        format!(
            "The redirect for {} leads nowhere: {why}.",
            name_of(&chain[0])
        ),
        "",
    );
    let mut data = json!({ "chain": names(chain), "reason": reason });
    if let Some(to) = to {
        data["to"] = json!(name_of(to));
    }
    e.data = Some(data);
    e
}

fn arrows(chain: &[Name]) -> String {
    names(chain).join(" → ")
}

/// `asset.trashed` for trash entry `entry_id`.
fn trashed(entry_id: &str) -> ErrorBody {
    let mut e = ErrorBody::new(
        &ASSET_TRASHED,
        format!("This asset is in the trash (entry {entry_id}). Restore it with trash.restore."),
        "",
    );
    e.data = Some(json!({ "entry_id": entry_id }));
    e
}

/// `asset.unknown` for a name that has no folder, no redirect and no trash entry.
fn unknown(name: &Name) -> ErrorBody {
    let mut e = ErrorBody::new(
        &ASSET_UNKNOWN,
        format!(
            "There is no asset `{}` in the project `{}`, no redirect from it and nothing of it \
             in the trash; assets.query lists the assets there are.",
            name.1, name.0
        ),
        "",
    );
    e.data = Some(json!({ "project": name.0, "asset_id": name.1 }));
    e
}

/// The error of a redirects file that can't be read: `manifest.needs_repair` naming it.
fn needs_repair(why: &str) -> ErrorBody {
    let mut e = ErrorBody::new(
        &MANIFEST_NEEDS_REPAIR,
        format!(
            "{REDIRECTS_YAML} needs repair: {why}. No ref can be resolved through it until it is \
             fixed by hand (or taken back from git); project.status lists it."
        ),
        "",
    );
    e.data = Some(json!({ "path": REDIRECTS_YAML, "why": why }));
    e
}

impl<'a> Resolver<'a> {
    /// A resolver that reads no redirects file: for what never follows a redirect (a path
    /// names a place, [`Resolver::resolve_path`]).
    pub fn without_redirects(index: &'a Index, store: &'a Store) -> Resolver<'a> {
        Resolver {
            store,
            index,
            redirects: Vec::new(),
            broken: None,
            newest: HashMap::new(),
            live: Mutex::new(HashMap::new()),
            trash: OnceCell::new(),
            trash_listed: AtomicBool::new(false),
        }
    }

    /// Reads the redirects and checks every name they mention on disk (module docs). The
    /// trash is listed only when a resolution needs it. A file that isn't redirects
    /// (`manifest.needs_repair`, `data: {path: "registry/redirects.yaml", why}`) or can't be
    /// read (the store's error) is kept aside as [`Resolver::file_error`], returned by each
    /// resolution that needs the file.
    ///
    /// # Errors
    /// None today: the `Result` is the interface's (Task 7), kept for callers.
    pub async fn load(index: &'a Index, store: &'a Store) -> Result<Resolver<'a>, ErrorBody> {
        let (redirects, broken) = match store.read(&redirects_path()).await {
            Ok(None) => (Vec::new(), None),
            Ok(Some(bytes)) => match std::str::from_utf8(&bytes)
                .map_err(|_| "it is not UTF-8".to_string())
                .and_then(parse_redirects)
            {
                Ok(redirects) => (redirects, None),
                Err(why) => (Vec::new(), Some(needs_repair(&why))),
            },
            // Unreadable (a cloud-only placeholder, a folder, held by another program): set
            // aside like a file that doesn't parse, so a live name still resolves.
            Err(e) => (Vec::new(), Some(e.to_error(""))),
        };
        let mut newest: HashMap<Name, usize> = HashMap::new();
        let key = |r: &Redirect| instant(&r.moved_utc);
        for (i, r) in redirects.iter().enumerate() {
            // Later entries win ties; one whose time doesn't parse never beats one that does.
            let take = newest
                .get(&r.from)
                .is_none_or(|&j| key(r) >= key(&redirects[j]));
            if take {
                newest.insert(r.from.clone(), i);
            }
        }
        let mentioned: BTreeSet<Name> = redirects
            .iter()
            .flat_map(|r| [r.from.clone(), r.to.clone()])
            .collect();
        let live = check_live(store, mentioned.into_iter().collect()).await;
        Ok(Resolver {
            store,
            index,
            redirects,
            broken,
            newest,
            live: Mutex::new(live),
            trash: OnceCell::new(),
            trash_listed: AtomicBool::new(false),
        })
    }

    /// The redirects, in file order (none when the file doesn't parse).
    pub fn redirects(&self) -> &[Redirect] {
        &self.redirects
    }

    /// Why the redirects file can't be read or parsed, when it can't (`manifest.needs_repair`
    /// for a file that doesn't parse, else the store's read error).
    pub fn file_error(&self) -> Option<&ErrorBody> {
        self.broken.as_ref()
    }

    /// Whether `e` is the redirects file's own error: the resolution needed the file and it
    /// doesn't parse.
    pub fn is_file_error(&self, e: &ErrorBody) -> bool {
        self.broken.as_ref() == Some(e)
    }

    /// Whether this resolver has listed the trash (it does so only for a name that ends with
    /// no live folder).
    pub fn trash_was_listed(&self) -> bool {
        self.trash_listed.load(Ordering::SeqCst)
    }

    fn cached_live(&self, name: &Name) -> Option<bool> {
        self.live
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .copied()
    }

    /// Whether `name` has a live folder (checked on disk once).
    async fn is_live(&self, name: &Name) -> bool {
        if let Some(live) = self.cached_live(name) {
            return live;
        }
        let found = check_live(self.store, vec![name.clone()]).await;
        let live = found.get(name).copied().unwrap_or(false);
        self.live
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.clone(), live);
        live
    }

    /// Follows the redirects from `start`, with liveness from the cache (every name after the
    /// first is one the file mentions, checked at load; the caller checked `start`).
    fn walk(&self, start: &Name) -> Walk {
        let mut chain = vec![start.clone()];
        loop {
            let at = chain.last().expect("a chain is never empty");
            if self.cached_live(at).unwrap_or(false) {
                return Walk::Live(chain);
            }
            let Some(&i) = self.newest.get(at) else {
                return Walk::DeadEnd(chain);
            };
            let next = self.redirects[i].to.clone();
            if chain.contains(&next) {
                chain.push(next);
                return Walk::Cycle(chain);
            }
            if chain.len() > MAX_HOPS {
                return Walk::TooLong(chain);
            }
            chain.push(next);
        }
    }

    /// The trash entry holding `name`'s folder, the newest when several do.
    async fn trash_entry(&self, name: &Name) -> Result<Option<String>, ErrorBody> {
        let list = self
            .trash
            .get_or_try_init(|| async {
                self.trash_listed.store(true, Ordering::SeqCst);
                self.store.trash_list().await.map_err(|e| e.to_error(""))
            })
            .await?;
        let folder = format!("projects/{}/assets/{}", name.0, name.1);
        Ok(list
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                r.original_path
                    .trim_end_matches(['/', '\\'])
                    .replace('\\', "/")
                    .eq_ignore_ascii_case(&folder)
            })
            .max_by_key(|(i, r)| (instant(&r.moved_utc), *i))
            .map(|(_, r)| r.entry_id.clone()))
    }

    /// Resolves `project`/`asset_id` (module docs).
    ///
    /// # Errors
    /// `ref.redirect_broken`, `asset.trashed` or `asset.unknown` (module docs), with an empty
    /// trace id; the file's `manifest.needs_repair` for a name that isn't live while the
    /// redirects file doesn't parse; `store.io` when the trash can't be listed.
    pub async fn resolve_name(
        &self,
        project: &str,
        asset_id: &str,
    ) -> Result<Resolution, ErrorBody> {
        let start = (project.to_string(), asset_id.to_string());
        let live = self.is_live(&start).await;
        if let Some(broken) = &self.broken
            && !live
        {
            return Err(broken.clone());
        }
        match self.walk(&start) {
            Walk::Live(chain) => {
                let (end, before) = chain.split_last().expect("a chain is never empty");
                Ok(Resolution {
                    project: end.0.clone(),
                    asset_id: end.1.clone(),
                    redirected_from: names(before),
                })
            }
            Walk::Cycle(chain) => {
                let repeat = name_of(chain.last().expect("a chain is never empty"));
                Err(broken(
                    &chain,
                    "cycle",
                    format!("it loops back to {repeat} ({})", arrows(&chain)),
                    None,
                ))
            }
            Walk::TooLong(chain) => Err(broken(
                &chain,
                "cycle",
                format!(
                    "the chain is longer than {MAX_HOPS} redirects ({} → …)",
                    arrows(&chain[..3])
                ),
                None,
            )),
            Walk::DeadEnd(chain) => {
                let end = chain.last().expect("a chain is never empty");
                if let Some(entry) = self.trash_entry(end).await? {
                    return Err(trashed(&entry));
                }
                if chain.len() == 1 {
                    return Err(unknown(end));
                }
                Err(broken(
                    &chain,
                    "target_missing",
                    format!(
                        "{} has no asset folder and isn't in the trash ({})",
                        name_of(end),
                        arrows(&chain)
                    ),
                    Some(end),
                ))
            }
        }
    }

    /// Every name that resolves to `project`/`asset_id` through the redirects, that name
    /// included, sorted (`asset.get`'s `used_by`); the name alone while the redirects file
    /// doesn't parse.
    pub fn names_resolving_to(&self, project: &str, asset_id: &str) -> Vec<(String, String)> {
        let target = (project.to_string(), asset_id.to_string());
        let mut found: BTreeSet<Name> = BTreeSet::from([target.clone()]);
        for from in self.newest.keys() {
            if let Walk::Live(chain) = self.walk(from)
                && chain.last() == Some(&target)
            {
                found.insert(from.clone());
            }
        }
        found.into_iter().collect()
    }

    /// The index's edges to every name that resolves to `project`/`asset_id` (depth 1).
    ///
    /// # Errors
    /// `store.io` when the index fails.
    pub async fn edges_to(&self, project: &str, asset_id: &str) -> Result<Vec<EdgeRow>, ErrorBody> {
        self.index
            .edges_to_any(&self.names_resolving_to(project, asset_id))
            .await
            .map_err(|e| e.to_error(""))
    }

    /// The redirect entries of `project` whose name loops or needs a 65th redirect, as
    /// `registry/redirects.yaml#<from>`, sorted (`project.status.needs_repair`). A `from` with
    /// a live folder isn't followed, so it never is.
    pub fn broken_from(&self, project: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .newest
            .keys()
            .filter(|from| from.0 == project)
            .filter(|from| matches!(self.walk(from), Walk::Cycle(_) | Walk::TooLong(_)))
            .map(|from| format!("{REDIRECTS_YAML}#{}", name_of(from)))
            .collect();
        out.sort();
        out
    }

    /// Resolves a canonical ref: its name through the redirects, then, for a sealed ref, its
    /// version and `#sha256` at the end (module docs).
    ///
    /// # Errors
    /// As [`Resolver::resolve_name`]; `asset.unknown` when the asset it reaches has no such
    /// sealed version; `ref.hash_mismatch` (`data: {ref, expected, found, redirected_from}`)
    /// when the version's content hash isn't the ref's; the store's read error.
    pub async fn resolve_ref(&self, r: &AssetRef) -> Result<Resolved, ErrorBody> {
        match r {
            AssetRef::Sealed {
                ws,
                project,
                asset,
                version,
                sha256,
            } => {
                let res = self.resolve_name(project, asset).await?;
                let name = (res.project.clone(), res.asset_id.clone());
                let Some(found) = self.content_hash(&name, *version).await? else {
                    let mut e = ErrorBody::new(
                        &ASSET_UNKNOWN,
                        format!(
                            "{} has no sealed version {version} on this machine; asset.get \
                             lists its versions.",
                            name_of(&name)
                        ),
                        "",
                    );
                    e.data = Some(json!({
                        "project": name.0, "asset_id": name.1, "version": version.to_string(),
                        "redirected_from": res.redirected_from,
                    }));
                    return Err(e);
                };
                let sha256_ok = match (sha256, &found) {
                    (Some(given), Some(actual)) if given != actual => {
                        let mut e = ErrorBody::new(
                            &REF_HASH_MISMATCH,
                            format!(
                                "The ref names {}@{version} with a content hash that version \
                                 doesn't have: it was renumbered or its files changed. asset.get \
                                 lists its versions with their hashes.",
                                name_of(&name)
                            ),
                            "",
                        );
                        e.data = Some(json!({
                            "ref": r.to_string(), "expected": given, "found": actual,
                            "redirected_from": res.redirected_from,
                        }));
                        return Err(e);
                    }
                    (Some(_), Some(_)) => Some(true),
                    _ => None,
                };
                let out = AssetRef::Sealed {
                    ws: ws.clone(),
                    project: name.0.clone(),
                    asset: name.1.clone(),
                    version: *version,
                    sha256: found.or_else(|| sha256.clone()),
                };
                Ok(Resolved {
                    r#ref: Some(out.to_string()),
                    version: Some(version.to_string()),
                    redirected_from: res.redirected_from,
                    sha256_ok,
                    ..Resolved::at(ResolvedKind::Sealed, &name)
                })
            }
            AssetRef::Candidate {
                ws,
                project,
                asset,
                run,
                file,
            } => {
                let res = self.resolve_name(project, asset).await?;
                let name = (res.project.clone(), res.asset_id.clone());
                let out = AssetRef::Candidate {
                    ws: ws.clone(),
                    project: name.0.clone(),
                    asset: name.1.clone(),
                    run: run.clone(),
                    file: file.clone(),
                };
                Ok(Resolved {
                    r#ref: Some(out.to_string()),
                    file: Some(format!(
                        "projects/{}/assets/{}/candidates/{run}/{file}",
                        name.0, name.1
                    )),
                    redirected_from: res.redirected_from,
                    ..Resolved::at(ResolvedKind::Candidate, &name)
                })
            }
        }
    }

    /// `Some(content hash)` of `name`'s version `version` (`Some(None)` when it isn't known),
    /// `None` when there is no such version.
    async fn content_hash(
        &self,
        name: &Name,
        version: Version,
    ) -> Result<Option<Option<String>>, ErrorBody> {
        Ok(self
            .store
            .versions(&name.0, &name.1)
            .await
            .map_err(|e| e.to_error(""))?
            .into_iter()
            .find(|v| v.version == version)
            .map(|v| v.content_hash))
    }

    /// An input as a manifest or a run writes it (`asset.get`'s `uses`), in workspace `ws_id`
    /// from `project`: a link to an asset's version resolves its name through the redirects
    /// and names that version: `kind: "sealed"` (read-only, with the content hash) when the
    /// store lists it, else `kind: "asset"` (not read-only, no hash), which is not an error (a
    /// planned dependent names the version it waits for); a `lil://` ref resolves as [`Resolver::resolve_ref`]. `None`
    /// for a library or style input, which isn't an asset of the workspace.
    ///
    /// # Errors
    /// As [`Resolver::resolve_name`] and [`Resolver::resolve_ref`]; `ref.invalid` for text
    /// that is neither a link nor a ref, or a ref of another workspace.
    pub async fn resolve_input(
        &self,
        ws_id: &str,
        project: &str,
        text: &str,
    ) -> Option<Result<Resolved, ErrorBody>> {
        let (name, version) = match text.parse::<LinkRef>() {
            Ok(LinkRef::Local { asset, version }) => ((project.to_string(), asset), version),
            Ok(LinkRef::Project {
                slug,
                asset,
                version,
            }) => ((slug, asset), version),
            Ok(LinkRef::Library { .. } | LinkRef::Style(_)) => return None,
            Err(_) => {
                return Some(match text.parse::<AssetRef>() {
                    Ok(AssetRef::Sealed { ws, .. } | AssetRef::Candidate { ws, .. })
                        if ws != ws_id =>
                    {
                        Err(ErrorBody::new(
                            &REF_INVALID,
                            format!("`{text}` is a ref of the workspace {ws}, not of this one"),
                            "",
                        ))
                    }
                    Ok(r) => self.resolve_ref(&r).await,
                    Err(why) => Err(ErrorBody::new(&REF_INVALID, why, "")),
                });
            }
        };
        Some(self.resolve_link(ws_id, &name, version).await)
    }

    async fn resolve_link(
        &self,
        ws_id: &str,
        name: &Name,
        version: Version,
    ) -> Result<Resolved, ErrorBody> {
        let res = self.resolve_name(&name.0, &name.1).await?;
        let end = (res.project.clone(), res.asset_id.clone());
        // A version the store doesn't list isn't sealed (yet): the input names the asset.
        let (kind, sha256) = match self.content_hash(&end, version).await? {
            Some(sha256) => (ResolvedKind::Sealed, sha256),
            None => (ResolvedKind::Asset, None),
        };
        let out = AssetRef::Sealed {
            ws: ws_id.to_string(),
            project: end.0.clone(),
            asset: end.1.clone(),
            version,
            sha256,
        };
        Ok(Resolved {
            r#ref: Some(out.to_string()),
            version: Some(version.to_string()),
            redirected_from: res.redirected_from,
            ..Resolved::at(kind, &end)
        })
    }

    /// A file path, absolute or workspace-relative (APP-019), in workspace `ws_id` (module
    /// docs of `tools::ref_tools`): under `projects/<p>/assets/<a>/` of a live asset,
    /// `v<version>/…` is `sealed` (read-only), `candidates/<run>/…` is `candidate`, the folder
    /// or its manifest is `asset`; under `inbox/` it is `inbox`; anything else is `outside`,
    /// with no project or asset fields. A `v<version>/…` of a version the asset doesn't have
    /// is `outside`; the folder's manifest is the one liveness finds (an old file name too).
    /// Paths compare the Windows way (case-insensitive, either separator, `.` and `..`
    /// resolved by name after a relative path is joined to the workspace); one whose real
    /// location is outside the workspace, through `..` or through a junction or symbolic
    /// link, is `outside`. A path names a place, so no redirect is followed: use
    /// [`Resolver::without_redirects`].
    ///
    /// # Errors
    /// The store's read error of the asset's versions.
    pub async fn resolve_path(&self, ws_id: &str, path: &str) -> Result<Resolved, ErrorBody> {
        let Some(rel) = self.inside(path).await else {
            return Ok(Resolved::outside());
        };
        let parts: Vec<&str> = rel.split('/').collect();
        let is = |i: usize, word: &str| parts.get(i).is_some_and(|p| p.eq_ignore_ascii_case(word));
        if is(0, "inbox") && parts.len() >= 2 {
            return Ok(Resolved {
                kind: ResolvedKind::Inbox,
                file: Some(rel.clone()),
                ..Resolved::outside()
            });
        }
        if !(is(0, "projects") && is(2, "assets") && parts.len() >= 4) {
            return Ok(Resolved::outside());
        }
        let name = (parts[1].to_lowercase(), parts[3].to_lowercase());
        if !is_slug(&name.0) || !is_asset_id(&name.1) || !self.is_live(&name).await {
            return Ok(Resolved::outside());
        }
        let rest = &parts[4..];
        match rest {
            [] => Ok(Resolved::at(ResolvedKind::Asset, &name)),
            [m] if self
                .folder_manifest(&name)
                .await
                .is_some_and(|f| m.eq_ignore_ascii_case(&f)) =>
            {
                Ok(Resolved {
                    file: Some(rel.clone()),
                    ..Resolved::at(ResolvedKind::Asset, &name)
                })
            }
            [v, inner @ ..] if v.starts_with(['v', 'V']) && v[1..].parse::<Version>().is_ok() => {
                let version: Version = v[1..].parse().expect("checked");
                // Only a version the asset has is sealed.
                let Some(sha256) = self.content_hash(&name, version).await? else {
                    return Ok(Resolved::outside());
                };
                let r = AssetRef::Sealed {
                    ws: ws_id.to_string(),
                    project: name.0.clone(),
                    asset: name.1.clone(),
                    version,
                    sha256,
                };
                Ok(Resolved {
                    r#ref: Some(r.to_string()),
                    version: Some(version.to_string()),
                    file: (!inner.is_empty()).then(|| rel.clone()),
                    ..Resolved::at(ResolvedKind::Sealed, &name)
                })
            }
            [c, run, file @ ..] if c.eq_ignore_ascii_case("candidates") && is_run_id(run) => {
                // The ref names a file of the run, when the path is one a ref can name.
                let r = (!file.is_empty())
                    .then(|| {
                        AssetRef::Candidate {
                            ws: ws_id.to_string(),
                            project: name.0.clone(),
                            asset: name.1.clone(),
                            run: (*run).to_string(),
                            file: file.join("/"),
                        }
                        .to_string()
                    })
                    .filter(|r| r.parse::<AssetRef>().is_ok());
                Ok(Resolved {
                    r#ref: r,
                    file: Some(rel.clone()),
                    ..Resolved::at(ResolvedKind::Candidate, &name)
                })
            }
            _ => Ok(Resolved::outside()),
        }
    }

    /// The file name of the manifest the live folder of `name` holds, as liveness finds it.
    async fn folder_manifest(&self, name: &Name) -> Option<String> {
        let (probe, root, name) = (
            self.store.probe().clone(),
            self.store.root().to_path_buf(),
            name.clone(),
        );
        tokio::task::spawn_blocking(move || {
            let dir = WsPath::parse(&format!("projects/{}/assets/{}", name.0, name.1))
                .ok()?
                .abs(&root);
            manifest_in(probe.as_ref(), &dir, &name.1)
        })
        .await
        .ok()
        .flatten()
    }

    /// `path` relative to the workspace (`/` separators, the spelling on disk), when its real
    /// location is inside it; `None` otherwise (module docs of [`Resolver::resolve_path`]).
    async fn inside(&self, path: &str) -> Option<String> {
        let root = self.store.root().to_path_buf();
        // A relative path is joined to the workspace first, then `.` and `..` are resolved by
        // name, so one that climbs out and back in is inside. Whether the result is inside is
        // decided on its real (canonical) location, which also catches a junction or symbolic
        // link that leads out.
        let given = PathBuf::from(path.replace('/', "\\"));
        let rooted = given
            .components()
            .any(|c| matches!(c, Component::Prefix(_) | Component::RootDir));
        let joined = lexical(&if rooted { given } else { root.join(given) })?;
        tokio::task::spawn_blocking(move || lil_platform::paths::relative_inside(&root, &joined))
            .await
            .ok()
            .flatten()
    }
}

/// `path` with `.` and `..` resolved by name (as Windows does before it looks); `None` when a
/// `..` climbs above where `path` starts.
fn lexical(path: &Path) -> Option<PathBuf> {
    let mut out: Vec<Component<'_>> = Vec::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                _ => return None,
            },
            other => out.push(other),
        }
    }
    Some(out.iter().collect())
}
