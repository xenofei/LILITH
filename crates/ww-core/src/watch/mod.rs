//! The watcher (CORE-014; ARCH-016, ARCH-073, ARCH-079, ARCH-083, ARCH-085, CORE-034): it
//! notices outside changes to the workspace, has the index reconcile them, and decides what
//! they are ([`reconcile::decide`]): a human edit (recorded in the manifest, the file never
//! touched), a manifest that needs repair, a missing file, an unfetched LFS pointer, or just new
//! content to index. The index never decides any of that itself (CORE-010).
//!
//! **Sources.** `ReadDirectoryChangesW` ([`ww_platform::watcher::DirWatcher`], 64 KiB buffer,
//! the largest a network folder accepts), the paths `ww notify` names ([`Watcher::notify`]),
//! and the files the index's background verification finds different from their manifest
//! ([`Watcher::mismatches`]). Where the folder can't be watched, or the watched folder goes
//! away, the workspace is still served without a watcher (`{kind:"watcher_unavailable"}` once
//! on `workspace`): the store's hash check still refuses to overwrite outside edits, `ww notify`
//! and the reconcile at open still find them, and writes still wait for git because the store's
//! gate checks `index.lock` itself.
//!
//! **Batches.** Changes are collected for [`WatchConfig::batch`] (tokio time). Meanwhile the
//! `DirWatcher` issues no read ([`DirWatcher::hold`]), so a burst larger than its buffer comes
//! as one overflow, and an overflow is a full rescan. Each batch:
//! 1. maps every path ([`reconcile::map_notify_path`]: 8.3 short names resolved, an
//!    unresolvable one reconciles its folder) and drops what isn't watched (of `.worldwright\`
//!    only `fence\`, of `.git\` only `index.lock` and `HEAD`, and never `.trash\`);
//! 2. notes git activity, which pauses the store's writes (below);
//! 3. while git works, defers every path to the post-git rescan; otherwise has the index
//!    reconcile the paths (a folder with its whole subtree, manifests first), re-checks the
//!    paths parked by unstable or locked reads, and decides every changed, missing or
//!    needs-repair path;
//! 4. marks its fences handled ([`Watcher::fence`]).
//!
//! **The suite's own moves are never `missing`.** A removal under an asset whose version lock
//! is held (a sign-off between `seal` and its Record stage) waits on its own task for the lock,
//! then reconciles that asset folder against the manifest the sign-off wrote. A fence waits for
//! those tasks too.
//!
//! **Git** (ARCH-083). `index.lock` appearing (or any event of it) or `HEAD` changing sets the
//! store's git flag and publishes `{kind:"git_paused"}`. Once `index.lock` is gone and nothing
//! of git has moved for [`WatchConfig::git_quiet`]: a full rescan (every manifest re-parsed
//! first), `decide` for every deferred and changed path, `{kind:"git_resumed"}`, the flag
//! cleared, and only then the actions that write or publish, so a human edit the rescan finds is
//! recorded at once rather than after the gate's 60 s wait.
//!
//! **Actions.** `HumanEdit` pushes `{path, sha256, previous_sha256, date}` to the manifest's
//! `human_edits` (asset-relative path, bare hex, UTC date of the detection) and publishes
//! `asset:` `{kind:"human_edit"}`; `NeedsRepair` and `Missing` set the file's state and publish
//! `{kind:"needs_repair"}` and `{kind:"missing", path}`. Every decision runs under one lock, and
//! a hash equal to the latest human edit is recorded already, so an edit is pushed once
//! however often it is seen (a batch, `ww notify`, an overflow rescan, the verification after a
//! cold rebuild).

pub mod git;
pub mod reconcile;

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc::UnboundedReceiver, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use ww_platform::watcher::{DirChange, DirWatcher};
use ww_schema::Manifest;

use crate::events::EventBus;
use crate::index::{FileKind, FileState, Index, Mismatch, ReconcileOrder, ReconcileReport, Scope};
use crate::index::{asset_of, kind_of};
use crate::store::{Store, StoreError, WsPath, path_key};

pub use reconcile::{Action, Observed, Recorded, decide, map_notify_path};

use git::{Class, GitState, classify};

/// The watcher's knobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchConfig {
    /// The `ReadDirectoryChangesW` buffer: 65536, the largest a network folder accepts.
    pub buffer_bytes: u32,
    /// How long changes are collected into one batch: 200 ms.
    pub batch: Duration,
    /// How long git must stay quiet before its pause ends: 2 s (rebase and pull release and
    /// retake `index.lock` between their steps).
    pub git_quiet: Duration,
}

impl Default for WatchConfig {
    fn default() -> Self {
        WatchConfig {
            buffer_bytes: 65536,
            batch: Duration::from_millis(200),
            git_quiet: Duration::from_secs(2),
        }
    }
}

/// What [`Watcher::notify`] did with the paths it was given.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotifyReport {
    /// Reconciled (or, while git works, deferred to the post-git rescan).
    pub reconciled: Vec<String>,
    /// Not watched: `.worldwright/`, `.trash/`, `.git/`.
    pub ignored: Vec<String>,
}

/// The workspace paths `watch.notify` was given (ARCH-016): workspace-relative (`/` or `\`
/// separated) or absolute. An absolute path counts only inside `root` (compared as
/// `folder_key` compares folders, with the spelling on disk kept); anything outside it, or that
/// isn't a valid workspace path, is returned as ignored.
pub fn notify_paths(root: &Path, given: &[String]) -> (Vec<WsPath>, Vec<String>) {
    let mut paths = Vec::new();
    let mut ignored = Vec::new();
    for raw in given {
        let as_path = Path::new(raw);
        let rel = if as_path.has_root() || as_path.is_absolute() {
            ww_platform::paths::relative_inside(root, as_path)
        } else {
            Some(raw.replace('\\', "/"))
        };
        match rel.as_deref().map(WsPath::parse) {
            Some(Ok(path)) => paths.push(path),
            _ => ignored.push(raw.clone()),
        }
    }
    (paths, ignored)
}

/// What a fence waits for.
#[derive(Debug, Clone, Default)]
struct FenceState {
    /// The fence names handled so far.
    seen: HashSet<String>,
    /// A `DirWatcher` feeds the watcher; without one a fence has nothing to wait for.
    watching: bool,
    /// Removals waiting for a version lock.
    deferred: usize,
}

/// One decision, with what it was made from.
#[derive(Debug, Clone)]
struct Decided {
    action: Action,
    obs: Observed,
    rec: Option<Recorded>,
    /// It came from the index's verification: the row still holds the hash it trusted, so a
    /// `Reindex` records the observed hash.
    verify: bool,
}

/// What the watcher's tasks share.
struct Shared {
    store: Arc<Store>,
    index: Arc<Index>,
    events: Arc<EventBus>,
    /// The workspace id, for the `asset:ww://<id>/<project>/<asset_id>` topics.
    ws_id: String,
    cfg: WatchConfig,
    /// The workspace folder's long form, for resolving 8.3 names.
    root_long: Option<PathBuf>,
    /// Every decision and its actions run under it, one at a time.
    apply: tokio::sync::Mutex<()>,
    git: Mutex<GitState>,
    fences: watch::Sender<FenceState>,
    /// Assets whose version lock a task waits for, with the paths it decides after it.
    deferring: Mutex<HashMap<String, Vec<WsPath>>>,
    /// `watcher_unavailable` was published.
    unavailable: AtomicBool,
    /// Wakes the loop when git starts outside a batch (`ww notify`, the verification).
    wake: Notify,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;

/// The asset folder `path` is, or is in: `projects/<p>/assets/<a>[/…]` → `(p, a)`.
fn asset_folder_of(path: &WsPath) -> Option<(&str, &str)> {
    let mut parts = path.as_str().split('/');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(projects), Some(project), Some(assets), Some(asset))
            if projects.eq_ignore_ascii_case("projects")
                && assets.eq_ignore_ascii_case("assets") =>
        {
            Some((project, asset))
        }
        _ => None,
    }
}

/// A workspace's watcher; [`Watcher::stop`] (or the workspace's close) ends it.
pub struct Watcher {
    shared: Arc<Shared>,
    dir: Mutex<Option<Arc<DirWatcher>>>,
    stop: CancellationToken,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher")
            .field("root", &self.shared.store.root())
            .field("watching", &self.watching())
            .finish_non_exhaustive()
    }
}

impl Watcher {
    /// Starts watching `store`'s workspace folder with `ReadDirectoryChangesW`.
    ///
    /// # Errors
    /// `DirWatcher::start`'s: the folder can't be watched (`ERROR_INVALID_FUNCTION`), the buffer
    /// is too large for a network folder (`ERROR_INVALID_PARAMETER`), or the folder can't be
    /// opened. The caller then serves the workspace with [`Watcher::unwatched`].
    pub fn start(
        store: Arc<Store>,
        index: Arc<Index>,
        events: Arc<EventBus>,
        topic_ws: String,
        cfg: WatchConfig,
    ) -> io::Result<Arc<Watcher>> {
        let (dir, rx) = DirWatcher::start(store.root(), cfg.buffer_bytes)?;
        let watcher = Watcher::spawn(
            store,
            index,
            events,
            topic_ws,
            cfg,
            Some(rx),
            Some(Arc::new(dir)),
        );
        // Fence files written before the folder watch was armed never send an event; the
        // first reconcile, which every fence waits for, covers what came before them. One
        // written after arming is both listed here and seen, which is harmless.
        watcher.shared.seed_fences();
        Ok(watcher)
    }

    /// A watcher fed by `changes` instead of a `DirWatcher` (paths relative to the workspace
    /// folder, as `ReadDirectoryChangesW` reports them): the simulated variants and tests.
    pub fn start_from(
        store: Arc<Store>,
        index: Arc<Index>,
        events: Arc<EventBus>,
        topic_ws: String,
        cfg: WatchConfig,
        changes: UnboundedReceiver<DirChange>,
    ) -> Arc<Watcher> {
        Watcher::spawn(store, index, events, topic_ws, cfg, Some(changes), None)
    }

    /// The watcher of a workspace that can't be watched: `ww notify`, the verification and the
    /// git pause still work. Publishes `{kind:"watcher_unavailable", reason}` on `workspace`.
    pub fn unwatched(
        store: Arc<Store>,
        index: Arc<Index>,
        events: Arc<EventBus>,
        topic_ws: String,
        cfg: WatchConfig,
        reason: &str,
    ) -> Arc<Watcher> {
        let watcher = Watcher::spawn(store, index, events, topic_ws, cfg, None, None);
        watcher.shared.lost(reason, None);
        watcher
    }

    fn spawn(
        store: Arc<Store>,
        index: Arc<Index>,
        events: Arc<EventBus>,
        topic_ws: String,
        cfg: WatchConfig,
        changes: Option<UnboundedReceiver<DirChange>>,
        dir: Option<Arc<DirWatcher>>,
    ) -> Arc<Watcher> {
        let root_long = store.probe().long_name(store.root()).ok();
        let shared = Arc::new(Shared {
            store,
            index,
            events,
            ws_id: topic_ws,
            cfg,
            root_long,
            apply: tokio::sync::Mutex::new(()),
            git: Mutex::new(GitState::default()),
            fences: watch::Sender::new(FenceState {
                watching: changes.is_some(),
                ..FenceState::default()
            }),
            deferring: Mutex::new(HashMap::new()),
            unavailable: AtomicBool::new(false),
            wake: Notify::new(),
        });
        let stop = CancellationToken::new();
        tokio::spawn(run(shared.clone(), changes, dir.clone(), stop.clone()));
        Arc::new(Watcher {
            shared,
            dir: Mutex::new(dir),
            stop,
        })
    }

    /// Whether a `DirWatcher` (or a change source) feeds the watcher.
    pub fn watching(&self) -> bool {
        self.shared.fences.borrow().watching
    }

    /// Stops the watcher: the `DirWatcher`'s read is cancelled and the loop ends. Stopping
    /// again does nothing.
    pub fn stop(&self) {
        self.stop.cancel();
        if let Some(dir) = lock(&self.dir).take() {
            dir.stop();
        }
    }

    /// Reconciles `paths` (files, or folders with their subtrees) and decides what changed:
    /// `ww notify` (ARCH-016, ARCH-073). Paths the watcher doesn't watch are `ignored`. While
    /// git works the paths are deferred to the post-git rescan. Waits for the workspace's first
    /// reconcile.
    pub async fn notify(&self, paths: Vec<WsPath>) -> NotifyReport {
        let s = &self.shared;
        s.index.ready().await;
        let mut report = NotifyReport::default();
        let mut keep = Vec::new();
        for path in paths {
            if classify(&path) == Class::Path {
                report.reconciled.push(path.as_str().to_string());
                keep.push(path);
            } else {
                report.ignored.push(path.as_str().to_string());
            }
        }
        if keep.is_empty() {
            return report;
        }
        let _apply = s.apply.lock().await;
        if s.git_busy_now() {
            let mut git = lock(&s.git);
            for path in keep {
                git.defer(path);
            }
            return report;
        }
        s.reconcile_paths(keep).await;
        report
    }

    /// Decides what the files the index's verification found different are (CORE-010: the
    /// index reports, the watcher decides): their observed hash goes through `decide`.
    pub async fn mismatches(&self, found: Vec<Mismatch>) {
        if found.is_empty() {
            return;
        }
        let s = &self.shared;
        let _apply = s.apply.lock().await;
        let busy = s.git_busy_now();
        for m in found {
            if s.stale(&m).await {
                // The file changed after the verification read it (or a batch has decided
                // it since): the batch, `ww notify` or the next verification decides the
                // newer content instead.
                tracing::debug!(path = %m.path, "a verification result is out of date; dropped");
                continue;
            }
            let obs = Observed::Changed {
                path: m.path.clone(),
                lfs_oid: m.lfs.then(|| m.sha256.clone()),
                sha256: m.sha256,
            };
            if busy {
                // The observation is the file's content now; the row still trusts the old hash.
                lock(&s.git).observed.push(obs);
                continue;
            }
            let rec = s.recorded(&obs).await;
            let action = decide(&obs, rec.as_ref(), false);
            s.apply(Decided {
                action,
                obs,
                rec,
                verify: true,
            })
            .await;
        }
    }

    /// Returns once the workspace's first reconcile has finished and the watcher has handled
    /// `.worldwright/fence/<name>` and everything before it, deferred removals included. It
    /// also returns while git works (a path deferred for git counts as handled), and at once
    /// without a `DirWatcher`.
    pub async fn fence(&self, name: &str) {
        self.shared.index.ready().await;
        let mut rx = self.shared.fences.subscribe();
        let _ = rx
            .wait_for(|s| (!s.watching || s.seen.contains(name)) && s.deferred == 0)
            .await;
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The next change, or never once there is no source.
async fn next_change(rx: &mut Option<UnboundedReceiver<DirChange>>) -> Option<DirChange> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// The watcher's loop: batches of changes, and the git pause's end.
async fn run(
    shared: Arc<Shared>,
    mut rx: Option<UnboundedReceiver<DirChange>>,
    mut dir: Option<Arc<DirWatcher>>,
    stop: CancellationToken,
) {
    loop {
        let quiet_at = lock(&shared.git).quiet_at(shared.cfg.git_quiet);
        tokio::select! {
            biased;
            () = stop.cancelled() => break,
            change = next_change(&mut rx) => {
                let Some(first) = change else {
                    // The source ended (after a `Failed`, or a stop).
                    rx = None;
                    if !stop.is_cancelled() {
                        shared.lost("the folder watch ended", None);
                    }
                    continue;
                };
                if let Some(dir) = &dir {
                    dir.hold();
                }
                let mut batch = vec![first];
                tokio::select! {
                    biased;
                    () = stop.cancelled() => break,
                    () = tokio::time::sleep(shared.cfg.batch) => {}
                }
                if let Some(rx) = rx.as_mut() {
                    while let Ok(change) = rx.try_recv() {
                        batch.push(change);
                    }
                }
                let lost = shared.batch(batch).await;
                if let Some(d) = &dir {
                    d.release();
                }
                if lost {
                    if let Some(d) = dir.take() {
                        d.stop();
                    }
                    rx = None;
                }
            }
            () = sleep_until(quiet_at) => shared.git_tick().await,
            () = shared.wake.notified() => {}
        }
    }
}

impl Shared {
    /// Publishes `{kind:"watcher_unavailable", reason, code?}` once and drops to the no-watcher
    /// mode: fences stop waiting for events.
    fn lost(&self, reason: &str, code: Option<i32>) {
        if self.unavailable.swap(true, Ordering::SeqCst) {
            return;
        }
        tracing::warn!(workspace = %self.store.root().display(), reason, ?code, "the workspace is served without a watcher");
        let mut data = json!({"kind": "watcher_unavailable", "reason": reason});
        if let Some(code) = code {
            data["code"] = Value::from(code);
        }
        self.events.publish("workspace", data);
        self.fences.send_modify(|s| s.watching = false);
    }

    fn asset_topic(&self, path: &WsPath) -> Option<String> {
        let (project, asset) = asset_of(path.as_str())?;
        Some(format!("asset:ww://{}/{project}/{asset}", self.ws_id))
    }

    /// `path` relative to its asset folder (`v1.0.0/x.png`), as `human_edits` names it.
    fn in_asset(path: &WsPath) -> String {
        let parts: Vec<&str> = path.as_str().split('/').collect();
        if parts.len() > 4 && asset_of(path.as_str()).is_some() {
            parts[4..].join("/")
        } else {
            path.as_str().to_string()
        }
    }

    fn publish_asset(&self, path: &WsPath, data: Value) {
        if let Some(topic) = self.asset_topic(path) {
            self.events.publish(&topic, data);
        }
    }

    /// `rel` (relative to the workspace folder) with 8.3 names expanded, relative again.
    fn resolve(&self, rel: &Path) -> io::Result<PathBuf> {
        let root_long = self
            .root_long
            .as_ref()
            .ok_or_else(|| io::Error::other("the workspace folder's long name is unknown"))?;
        let long = self.store.probe().long_name(&self.store.root().join(rel))?;
        long.strip_prefix(root_long)
            .map(Path::to_path_buf)
            .map_err(|_| io::Error::other("not under the workspace folder"))
    }

    /// Git did something: a pause starts (the flag set, `git_paused` published) or goes on.
    fn git_activity(&self) {
        let started = lock(&self.git).activity(Instant::now());
        if started {
            self.store.git_gate().set_busy(true);
            tracing::info!(workspace = %self.store.root().display(), "git is working; store writes wait");
            self.events
                .publish("workspace", json!({"kind": "git_paused"}));
            self.wake.notify_one();
        }
    }

    /// Whether git works now: a pause is on, or `index.lock` is there (which starts one).
    fn git_busy_now(&self) -> bool {
        if lock(&self.git).busy {
            return true;
        }
        if self.store.git_gate().lock_exists() {
            self.git_activity();
            return true;
        }
        false
    }

    /// One batch of changes. Returns whether the watch is lost (the folder went away).
    async fn batch(self: &Arc<Self>, changes: Vec<DirChange>) -> bool {
        self.index.ready().await;
        let _apply = self.apply.lock().await;
        let mut overflow = false;
        let mut failed = None;
        let mut fences = Vec::new();
        let mut paths: Vec<WsPath> = Vec::new();
        for change in changes {
            let (rel, modified) = match change {
                DirChange::Overflow => {
                    overflow = true;
                    continue;
                }
                DirChange::Failed(code) => {
                    failed = Some(code);
                    continue;
                }
                DirChange::Modified(p) => (p, true),
                DirChange::Added(p)
                | DirChange::RenamedTo(p)
                | DirChange::Removed(p)
                | DirChange::RenamedFrom(p) => (p, false),
            };
            if modified && self.is_folder(&rel) {
                // A folder's last write or attributes changed: its entries' own events say
                // what changed in it, and a folder is no row of the index.
                continue;
            }
            let text = rel.to_string_lossy();
            let path = match map_notify_path(&text, &|r| self.resolve(r)) {
                Ok(path) => path,
                Err(parent) if !parent.as_str().contains('/') && parent.as_str().contains('~') => {
                    // Not even the first component maps: look at everything.
                    tracing::debug!(path = %text, "an unresolvable short name at the top; rescanning");
                    overflow = true;
                    continue;
                }
                Err(parent) => parent,
            };
            match classify(&path) {
                Class::Fence(name) => fences.push(name),
                Class::GitLock | Class::GitHead => self.git_activity(),
                Class::Ignored => {}
                Class::Path => paths.push(path),
            }
        }
        if matches!(
            ww_platform::fs::attributes(self.store.root()).map_err(|e| e.raw_os_error()),
            Err(Some(2 | 3))
        ) {
            // The watched folder was renamed (which completes no read: its changes would keep
            // coming under the new name) or removed. Checked before anything is reconciled,
            // so its files are never taken for missing.
            self.lost("the workspace folder was renamed or removed", None);
            return true;
        }
        // The lock itself counts, not only its events: git may have taken it before the
        // workspace opened, and then no event of it ever comes.
        let busy = self.git_busy_now();
        if busy {
            let mut git = lock(&self.git);
            for path in paths {
                git.defer(path);
            }
            git.overflowed |= overflow;
        } else if overflow {
            let (_, decided) = self.full_rescan(&[]).await;
            self.apply_all(decided).await;
            self.events.publish("workspace", json!({"kind": "rescan"}));
        } else {
            self.reconcile_paths(paths).await;
        }
        self.fences.send_modify(|s| s.seen.extend(fences));

        if let Some(code) = failed {
            self.lost(
                &format!("the workspace folder can no longer be watched (error {code})"),
                Some(code),
            );
            return true;
        }
        false
    }

    /// Whether the verification's result `m` is out of date: the row has been verified since
    /// (a batch decided it), or the file's skip key (size, last write, ChangeTime: what a
    /// stable read compares, ARCH-079) is no longer the one after its read. The rest of the
    /// identity (attributes, file id) is left out, so a metadata-only change, which FAT and
    /// Explorer's archive bit make, never hides a human edit.
    async fn stale(&self, m: &Mismatch) -> bool {
        if !matches!(self.index.is_verified(&m.path).await, Ok(Some(false))) {
            return true;
        }
        let (probe, abs) = (self.store.probe().clone(), m.path.abs(self.store.root()));
        let now = tokio::task::spawn_blocking(move || probe.identity(&abs))
            .await
            .ok()
            .and_then(Result::ok);
        let key = |id: &ww_platform::fs::FileIdentity| (id.size, id.last_write, id.change_time);
        now.as_ref().map(key) != Some(key(&m.identity))
    }

    /// Whether `project/asset`'s version lock is held now (a sign-off between `seal` and its
    /// Record stage).
    fn version_lock_held(&self, project: &str, asset: &str) -> bool {
        self.store.version_lock(project, asset).try_lock().is_err()
    }

    /// Whether the event path `rel` (relative to the workspace folder) is a folder now.
    fn is_folder(&self, rel: &Path) -> bool {
        ww_platform::fs::attributes(&self.store.root().join(rel))
            .is_ok_and(|a| a & FILE_ATTRIBUTE_DIRECTORY != 0)
    }

    /// Reconciles `paths`, re-checks the parked paths, and decides and applies what changed.
    /// A path in (or of) an asset folder whose version lock is held waits for the lock: the
    /// sign-off moves files there, and a reconcile meanwhile would take its moves for removals.
    async fn reconcile_paths(self: &Arc<Self>, all: Vec<WsPath>) {
        let mut paths = Vec::with_capacity(all.len());
        let mut seen = HashSet::new();
        for path in all {
            if !seen.insert(path_key(path.as_str())) {
                continue;
            }
            match asset_folder_of(&path) {
                Some((project, asset)) if self.version_lock_held(project, asset) => {
                    self.defer_asset(project, asset, path.clone());
                }
                _ => paths.push(path),
            }
        }
        if !paths.is_empty() {
            match self.index.reconcile(Scope::Paths(paths)).await {
                Ok(report) => {
                    let decided = self.decide_report(&report, &[]).await;
                    self.apply_all(decided).await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "changed paths couldn't be reconciled; the next rescan catches up");
                }
            }
        }
        match self.index.retry_parked().await {
            Ok(report) => {
                let decided = self.decide_report(&report, &[]).await;
                self.apply_all(decided).await;
            }
            Err(e) => tracing::warn!(error = %e, "parked paths couldn't be re-checked"),
        }
    }

    /// A whole-workspace reconcile, every manifest re-parsed first, then a decision for every
    /// changed path and for `extra`. The fences written so far are handled by it.
    async fn full_rescan(self: &Arc<Self>, extra: &[WsPath]) -> (ReconcileReport, Vec<Decided>) {
        let report = match self
            .index
            .reconcile_ordered(Scope::All, ReconcileOrder::ManifestsFirst)
            .await
        {
            Ok(report) => report,
            Err(e) => {
                tracing::warn!(error = %e, "the workspace rescan failed");
                ReconcileReport::default()
            }
        };
        self.seed_fences();
        let decided = self.decide_report(&report, extra).await;
        (report, decided)
    }

    /// Marks every fence file in `.worldwright/fence/` handled (test fences, QA-031).
    fn seed_fences(&self) {
        let fence_dir = self.store.root().join(".worldwright").join("fence");
        if let Ok(entries) = std::fs::read_dir(&fence_dir) {
            let names: Vec<String> = entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            self.fences.send_modify(|s| s.seen.extend(names));
        }
    }

    /// Observations for every changed, missing and needs-repair path of `report`, and for
    /// `extra`, decided (with git not busy).
    async fn decide_report(&self, report: &ReconcileReport, extra: &[WsPath]) -> Vec<Decided> {
        let missing: HashSet<String> = report
            .missing
            .iter()
            .map(|p| path_key(p.as_str()))
            .collect();
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let paths = report
            .changed
            .iter()
            .chain(&report.missing)
            .chain(&report.needs_repair)
            .chain(extra);
        for path in paths {
            if !seen.insert(path_key(path.as_str())) {
                continue;
            }
            let Some(obs) = self.observe(path, &missing).await else {
                continue;
            };
            let rec = self.recorded(&obs).await;
            out.push(Decided {
                action: decide(&obs, rec.as_ref(), false),
                obs,
                rec,
                verify: false,
            });
        }
        out
    }

    /// What the index's row says `path` is now; `None` for no row (a removed untracked file, a
    /// folder) or a row without a hash.
    async fn observe(&self, path: &WsPath, missing: &HashSet<String>) -> Option<Observed> {
        if missing.contains(&path_key(path.as_str())) {
            return Some(Observed::Removed(path.clone()));
        }
        let row = self.index.files_of_path(path).await.ok().flatten()?;
        let path = row.path.clone();
        Some(match row.state {
            FileState::Missing => Observed::Removed(path),
            FileState::CloudOnly => Observed::CloudOnly(path),
            FileState::NeedsRepair if kind_of(&path) == FileKind::Manifest => {
                Observed::BadManifest(path)
            }
            FileState::NotFetched => {
                let oid = row.sha256?;
                Observed::Changed {
                    path,
                    sha256: oid.clone(),
                    lfs_oid: Some(oid),
                }
            }
            _ => Observed::Changed {
                path,
                sha256: row.sha256?,
                lfs_oid: None,
            },
        })
    }

    /// The asset's manifest: the one its index row names, else the canonical name.
    async fn manifest_path(&self, path: &WsPath) -> Option<WsPath> {
        let (project, asset) = asset_of(path.as_str())?;
        match self.index.asset(project, asset).await {
            Ok(Some(row)) => Some(row.manifest),
            _ => reconcile::manifest_of(path),
        }
    }

    /// The latest `human_edits[path].sha256` of `manifest`.
    async fn last_human_edit(&self, manifest: &WsPath, path: &WsPath) -> Option<String> {
        let bytes = self.store.read(manifest).await.ok().flatten()?;
        let (m, _) = Manifest::parse(std::str::from_utf8(&bytes).ok()?).ok()?;
        let inside = path_key(&Self::in_asset(path));
        m.view()
            .human_edits
            .iter()
            .rev()
            .find(|e| path_key(&e.path) == inside)
            .map(|e| e.sha256.clone())
    }

    /// What the suite knows about the observed path (module docs of [`reconcile`]).
    async fn recorded(&self, obs: &Observed) -> Option<Recorded> {
        let path = obs.path();
        let kind = kind_of(path);
        match kind {
            FileKind::Work | FileKind::Other => None,
            FileKind::Manifest => Some(Recorded {
                sha256: String::new(),
                kind,
                manifest: Some(path.clone()),
                expected: true,
                pending: Vec::new(),
                last_human_edit: None,
                outside_addition: false,
            }),
            FileKind::Candidate | FileKind::Sealed => {
                let entry = self.index.expected_entry(path).await.ok().flatten();
                let (mut pending, generated) =
                    self.store.own_hashes(path).await.unwrap_or_default();
                if entry.is_none() && generated.is_none() && pending.is_empty() {
                    // Ruling Q32: nothing records a file in a sealed version, so it was added
                    // from outside.
                    if kind == FileKind::Sealed && self.in_sealed_version(path).await {
                        let manifest = self.manifest_path(path).await;
                        let last_human_edit = match (obs, &manifest) {
                            (Observed::Changed { .. }, Some(manifest)) => {
                                self.last_human_edit(manifest, path).await
                            }
                            _ => None,
                        };
                        return Some(Recorded {
                            sha256: String::new(),
                            kind,
                            manifest,
                            expected: false,
                            pending: Vec::new(),
                            last_human_edit,
                            outside_addition: true,
                        });
                    }
                    return None;
                }
                let (sha256, expected) = match entry {
                    Some((Some(sha), tracked)) => {
                        // The core's last write is its own too, as `accepts_as_own` says.
                        pending.extend(generated.filter(|g| *g != sha));
                        (Some(sha), tracked)
                    }
                    Some((None, tracked)) => (generated, tracked),
                    None => (generated, false),
                };
                let manifest = self.manifest_path(path).await;
                let last_human_edit = match (obs, &manifest) {
                    (Observed::Changed { .. }, Some(manifest)) => {
                        self.last_human_edit(manifest, path).await
                    }
                    _ => None,
                };
                Some(Recorded {
                    sha256: sha256.unwrap_or_default(),
                    kind,
                    manifest,
                    expected,
                    pending,
                    last_human_edit,
                    outside_addition: false,
                })
            }
        }
    }

    /// Whether `path` (a [`FileKind::Sealed`] path) is a file other than the snapshot in a
    /// sealed `projects/<p>/assets/<a>/v<semver>/`, one that holds a `manifest.snapshot.json`
    /// (a legacy folder without one is read as it is, CORE-033). Attributes only.
    async fn in_sealed_version(&self, path: &WsPath) -> bool {
        let parts: Vec<&str> = path.as_str().split('/').collect();
        if parts.len() < 6
            || (parts.len() == 6 && parts[5].eq_ignore_ascii_case(crate::store::SNAPSHOT))
        {
            return false;
        }
        let snapshot = WsPath::from_disk(format!(
            "{}/{}",
            parts[..5].join("/"),
            crate::store::SNAPSHOT
        ))
        .abs(self.store.root());
        tokio::task::spawn_blocking(move || ww_platform::fs::attributes(&snapshot).is_ok())
            .await
            .unwrap_or(false)
    }

    async fn apply_all(self: &Arc<Self>, decided: Vec<Decided>) {
        for d in decided {
            self.apply(d).await;
        }
    }

    /// Carries out one decision (module docs).
    async fn apply(self: &Arc<Self>, d: Decided) {
        match d.action {
            Action::Reindex(path) => {
                // A reconcile has indexed it already; a verification's row still trusts the
                // old hash, so it takes the observed one.
                if d.verify
                    && let Observed::Changed {
                        sha256, lfs_oid, ..
                    } = &d.obs
                {
                    let state = if lfs_oid.is_some() {
                        FileState::NotFetched
                    } else if d.rec.as_ref().and_then(|r| r.last_human_edit.as_deref())
                        == Some(sha256.as_str())
                    {
                        FileState::HumanEdit
                    } else {
                        FileState::Ok
                    };
                    self.accept(&path, sha256, state).await;
                }
            }
            Action::NotFetched(path) => {
                if d.verify
                    && let Observed::Changed { sha256, .. } = &d.obs
                {
                    self.accept(&path, sha256, FileState::NotFetched).await;
                }
            }
            Action::HumanEdit {
                manifest,
                path,
                sha256,
                previous,
            } => self.human_edit(&manifest, &path, &sha256, &previous).await,
            Action::NeedsRepair(path) => {
                if let Err(e) = self.index.set_state(&path, FileState::NeedsRepair).await {
                    tracing::warn!(path = %path, error = %e, "a manifest's needs-repair state couldn't be recorded");
                }
                tracing::warn!(path = %path, "a manifest needs repair; it is never rewritten");
                self.publish_asset(
                    &path,
                    json!({"kind": "needs_repair", "path": Self::in_asset(&path)}),
                );
            }
            Action::Missing(path) => {
                if let Some((project, asset)) = asset_of(path.as_str())
                    && self.version_lock_held(project, asset)
                {
                    // The suite's own move (a sign-off in progress): decided after it.
                    self.defer_asset(project, asset, path.clone());
                    return;
                }
                if let Err(e) = self.index.set_state(&path, FileState::Missing).await {
                    tracing::warn!(path = %path, error = %e, "a missing file couldn't be recorded");
                }
                tracing::warn!(path = %path, "a file the suite expects disappeared outside it");
                self.publish_asset(
                    &path,
                    json!({"kind": "missing", "path": Self::in_asset(&path)}),
                );
            }
            Action::DeferUntilGitDone(path) => {
                let mut git = lock(&self.git);
                git.defer(path);
                if d.verify {
                    git.observed.push(d.obs);
                }
            }
        }
    }

    async fn accept(&self, path: &WsPath, sha256: &str, state: FileState) {
        if let Err(e) = self.index.accept(path, sha256, state).await {
            tracing::warn!(path = %path, error = %e, "the index couldn't take the watcher's decision");
        }
    }

    /// Pushes `{path, sha256, previous_sha256, date}` to the manifest's `human_edits`, unless
    /// its latest entry for the path already has this hash; the file itself is never touched
    /// (CORE-034). Then `asset:` `{kind:"human_edit"}`.
    ///
    /// It runs under the decision lock, and the manifest edit waits at the store's git gate
    /// (write step 2). Should git take `index.lock` again between the decision and the write,
    /// the edit waits there for up to 60 s, and the watcher's batches with it; then it fails
    /// with `store.git_busy` and is logged. That fails safe: nothing is overwritten, and the
    /// file stays different from its manifest, so the rescan after git or the next
    /// verification finds it again.
    async fn human_edit(&self, manifest: &WsPath, path: &WsPath, sha256: &str, previous: &str) {
        match self.store.read(manifest).await {
            Ok(Some(_)) => {}
            Ok(None) => {
                tracing::warn!(path = %path, manifest = %manifest, "a human edit can't be recorded: the asset has no manifest");
                return;
            }
            Err(e) => {
                tracing::warn!(path = %path, error = %e, "a human edit can't be recorded: its manifest can't be read");
                return;
            }
        }
        let inside = Self::in_asset(path);
        let date = ww_schema::time::date(self.store.now_utc());
        let entry = json!({
            "path": inside,
            "sha256": sha256,
            "previous_sha256": previous,
            "date": date,
        });
        let key = path_key(&inside);
        let edit = |m: &mut Manifest| -> Result<(), StoreError> {
            let recorded = m
                .view()
                .human_edits
                .iter()
                .rev()
                .find(|e| path_key(&e.path) == key)
                .is_some_and(|e| e.sha256 == sha256);
            if recorded {
                return Ok(());
            }
            m.push("human_edits", entry.clone())
                .map_err(|e| StoreError::manifest_edit(manifest, &e))
        };
        match self.store.edit_manifest(manifest, &edit).await {
            Ok(_) => {
                self.accept(path, sha256, FileState::HumanEdit).await;
                tracing::info!(path = %path, sha256, "an outside edit was recorded as a human edit");
                self.publish_asset(
                    path,
                    json!({
                        "kind": "human_edit",
                        "path": inside,
                        "sha256": sha256,
                        "previous_sha256": previous,
                    }),
                );
            }
            Err(e) => {
                tracing::warn!(path = %path, error = %e, "a human edit couldn't be recorded in its manifest");
            }
        }
    }

    /// Waits on its own task for `project/asset`'s version lock (taking it and dropping it at
    /// once), then reconciles the asset folder against the manifest the sign-off wrote and
    /// decides `path` (and the other paths deferred meanwhile) again. One task per asset; a
    /// fence waits for it.
    fn defer_asset(self: &Arc<Self>, project: &str, asset: &str, path: WsPath) {
        let key = path_key(&format!("{project}/{asset}"));
        {
            let mut deferring = lock(&self.deferring);
            if let Some(paths) = deferring.get_mut(&key) {
                paths.push(path);
                return;
            }
            deferring.insert(key.clone(), vec![path]);
        }
        self.fences.send_modify(|s| s.deferred += 1);
        let shared = self.clone();
        let (project, asset) = (project.to_string(), asset.to_string());
        tokio::spawn(async move {
            let held = shared.store.version_lock(&project, &asset);
            drop(held.lock().await);
            let paths = lock(&shared.deferring).remove(&key).unwrap_or_default();
            let folder = WsPath::from_disk(format!("projects/{project}/assets/{asset}"));
            {
                let _apply = shared.apply.lock().await;
                if lock(&shared.git).busy {
                    lock(&shared.git).defer(folder);
                } else {
                    match shared.index.reconcile(Scope::Paths(vec![folder])).await {
                        Ok(report) => {
                            let decided = shared.decide_report(&report, &paths).await;
                            shared.apply_all(decided).await;
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "an asset folder couldn't be reconciled after its sign-off");
                        }
                    }
                }
            }
            shared.fences.send_modify(|s| s.deferred -= 1);
        });
    }

    /// The git pause's end, once `index.lock` is gone and git has been quiet (module docs):
    /// the full rescan, the decisions, `git_resumed`, the flag cleared, then the actions that
    /// write or publish.
    async fn git_tick(self: &Arc<Self>) {
        let _apply = self.apply.lock().await;
        {
            let git = lock(&self.git);
            match git.quiet_at(self.cfg.git_quiet) {
                Some(at) if Instant::now() >= at => {}
                _ => return,
            }
        }
        if self.store.git_gate().lock_exists() {
            lock(&self.git).last = Some(Instant::now());
            return;
        }
        // Taken now; the pause itself goes on until step 4 (no batch runs meanwhile: this
        // holds the decision lock).
        let (deferred, observed, overflowed) = lock(&self.git).finish();
        lock(&self.git).busy = true;
        // 1 and 2.
        let (report, mut decided) = self.full_rescan(&deferred).await;
        // A verification's observation stands where the rescan saw the file neither change
        // nor go (what the rescan read is newer).
        let fresh: HashSet<String> = report
            .changed
            .iter()
            .chain(&report.missing)
            .map(|p| path_key(p.as_str()))
            .collect();
        for obs in observed {
            let key = path_key(obs.path().as_str());
            if fresh.contains(&key) {
                continue;
            }
            decided.retain(|d| path_key(d.obs.path().as_str()) != key);
            let rec = self.recorded(&obs).await;
            decided.push(Decided {
                action: decide(&obs, rec.as_ref(), false),
                obs,
                rec,
                verify: true,
            });
        }
        let (now, queued): (Vec<Decided>, Vec<Decided>) = decided
            .into_iter()
            .partition(|d| matches!(d.action, Action::Reindex(_) | Action::NotFetched(_)));
        self.apply_all(now).await;
        if overflowed {
            self.events.publish("workspace", json!({"kind": "rescan"}));
        }
        // 3.
        self.events
            .publish("workspace", json!({"kind": "git_resumed"}));
        // 4.
        lock(&self.git).busy = false;
        self.store.git_gate().set_busy(false);
        tracing::info!(workspace = %self.store.root().display(), "git has finished; store writes go on");
        // 5.
        self.apply_all(queued).await;
    }
}

#[cfg(test)]
mod tests;
