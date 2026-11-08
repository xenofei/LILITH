//! The watcher (CORE-014; ARCH-016, ARCH-073, ARCH-079, ARCH-083, ARCH-085, CORE-034): it
//! notices outside changes to the workspace, has the index reconcile them, and decides what
//! they are ([`reconcile::decide`]): a human edit (recorded in the manifest, the file never
//! touched), a manifest that needs repair, a missing file, an unfetched LFS pointer, or just new
//! content to index. The index never decides any of that itself (CORE-010).
//!
//! **Sources.** `ReadDirectoryChangesW` ([`lil_platform::watcher::DirWatcher`], 64 KiB buffer,
//! the largest a network folder accepts), the paths `lil notify` names ([`Watcher::notify`]),
//! and the files the index's background verification finds different from their manifest
//! ([`Watcher::mismatches`]). Where the folder can't be watched, or the watched folder goes
//! away, the workspace is still served without a watcher (`{kind:"watcher_unavailable"}` once
//! on `workspace`): the store's hash check still refuses to overwrite outside edits, `lil notify`
//! and the reconcile at open still find them, and writes still wait for git because the store's
//! gate checks `index.lock` itself.
//!
//! **Batches.** Changes are collected for [`WatchConfig::batch`] (tokio time). Meanwhile the
//! `DirWatcher` issues no read ([`DirWatcher::hold`]), so a burst larger than its buffer comes
//! as one overflow, and an overflow is a full rescan. Each batch:
//! 1. maps every path ([`reconcile::map_notify_path`]: 8.3 short names resolved, an
//!    unresolvable one reconciles its folder) and drops what isn't watched (of `.lilith\`
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
//! store's git flag and publishes `{kind:"git_paused"}`. Where `.git` is a `gitdir:` file (a
//! linked worktree, a submodule, `--separate-git-dir`), the git folder it names is outside the
//! workspace's watch, so [`Watcher::start`] watches that folder too, for its `index.lock` and
//! `HEAD` ([`GitGate::git_dir`](crate::store::GitGate::git_dir)), and the store's gate checks
//! the `index.lock` there. Once `index.lock` is gone and nothing
//! of git has moved for [`WatchConfig::git_quiet`]: a full rescan (every manifest re-parsed
//! first), `decide` for every deferred and changed path, `{kind:"git_resumed"}`, the flag
//! cleared, and only then the actions that write or publish, so a human edit the rescan finds is
//! recorded at once rather than after the gate's 60 s wait.
//!
//! **Actions.** `HumanEdit` pushes `{path, sha256, previous_sha256, date, actor?}` to the
//! manifest's `human_edits` (asset-relative path, bare hex, UTC date of the detection; `actor`
//! when the change was reported with one, below) and publishes `asset:` `{kind:"human_edit"}`;
//! `NeedsRepair` and `Missing` set the file's state and publish `{kind:"needs_repair"}` and
//! `{kind:"missing", path}`. Every decision runs under one lock, and
//! a hash equal to the latest human edit is recorded already, so an edit is pushed once
//! however often it is seen (a batch, `lil notify`, an overflow rescan, the verification after a
//! cold rebuild).
//!
//! **Who made a change** (CORE-034; P4b-R16). Only a report names one: [`Watcher::notify`]
//! takes the reporter's actor (`watch.notify` computes it; the watcher never maps a caller).
//! - The human edits recorded by the decisions of the paths given (a folder: its subtree) get
//!   that `actor`. That holds for a decision made later too: the actor goes with a path
//!   deferred to the post-git rescan, with a path waiting for a held version lock, and with a
//!   read the index parked (unstable or locked), in the report's own reconcile or in the
//!   deferred one, until its path is next decided. A later deferral of the same path without
//!   an actor (the watcher's own batch) never replaces it.
//! - Known limit (counts as human): the index gives up on a read still unstable after its
//!   re-checks (`PARKED_RETRIES`); the reporter goes with it, and the later rescan's decision
//!   of that path records no actor. Your relabel (`app/asset.ai_edit`) is the way back.
//! - A **claim** (fix round 1, I2): the watcher's own batch usually records an agent's write
//!   before the agent's report comes, with no actor. The report's actor is filled into that
//!   entry only when all of these hold: the actor is an agent's (`claude_code`, `tether`,
//!   `director`); the entry has no actor; its hash is the file's current one; and the watcher
//!   recorded it at most [`CLAIM_WINDOW`] before the report (the core's clock, kept in
//!   memory). The window counts to the report, also when the report waits for git or a
//!   version lock and the claim runs after it. Otherwise it stays a human edit, which only
//!   your relabel changes (OD-10). The first claim wins.
//! - An entry that has an actor keeps it: an agent edit is never later made a human one. What
//!   the watcher sees by itself and the verification record no actor.

pub mod git;
pub mod reconcile;

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use lil_platform::names::WORKSPACE_DIR;
use lil_platform::watcher::{DirChange, DirWatcher};
use lil_schema::{Actor, ActorKind, Manifest, SCHEME, bare_sha256};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc::UnboundedReceiver, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::events::EventBus;
use crate::index::{FileKind, FileState, Index, Mismatch, ReconcileOrder, ReconcileReport, Scope};
use crate::index::{asset_of, kind_of};
use crate::store::{Store, StoreError, WsPath, path_key};

pub use reconcile::{Action, Observed, Recorded, decide, map_notify_path};

use git::{Class, GitState, Reported, Reporter, classify};

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

/// How long after the watcher recorded a human edit without an actor an agent's report may
/// still claim it (module docs, "Who made a change"): well above the batch window and a
/// hook's start, short enough that a report never takes over an older edit of yours.
pub const CLAIM_WINDOW: Duration = Duration::from_secs(10);

/// Whether `actor` is an agent's: an edit with this actor is an agent edit (CORE-034).
fn is_agent(actor: &Actor) -> bool {
    matches!(
        actor.kind,
        ActorKind::ClaudeCode | ActorKind::Tether | ActorKind::Director
    )
}

/// The reporter of `path` among `reported` (paths deferred with their reporters): the path
/// itself, or a folder above it.
fn reporter_of(path: &WsPath, reported: &[Reported]) -> Option<Actor> {
    let key = path_key(path.as_str());
    reported.iter().find_map(|(p, reporter)| {
        let folder = path_key(p.as_str());
        let under = key == folder || key.starts_with(&format!("{folder}/"));
        if under {
            reporter.as_ref().map(|r| r.actor.clone())
        } else {
            None
        }
    })
}

/// Gives each of `decided` without an actor its reporter among `reported`.
fn stamp(decided: &mut [Decided], reported: &[Reported]) {
    for d in decided.iter_mut().filter(|d| d.actor.is_none()) {
        d.actor = reporter_of(d.obs.path(), reported);
    }
}

/// What [`Watcher::notify`] did with the paths it was given.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotifyReport {
    /// Reconciled (or, while git works, deferred to the post-git rescan).
    pub reconciled: Vec<String>,
    /// Not watched: the workspace data folders, `.trash/`, `.git/`.
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
            lil_platform::paths::relative_inside(root, as_path)
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
    /// Who made the change, when a report named them (a `HumanEdit` records it).
    actor: Option<Actor>,
}

/// What the watcher's tasks share.
struct Shared {
    store: Arc<Store>,
    index: Arc<Index>,
    events: Arc<EventBus>,
    /// The workspace id, for the `asset:lil://<id>/<project>/<asset_id>` topics.
    ws_id: String,
    cfg: WatchConfig,
    /// The workspace folder's long form, for resolving 8.3 names.
    root_long: Option<PathBuf>,
    /// Every decision and its actions run under it, one at a time.
    apply: tokio::sync::Mutex<()>,
    git: Mutex<GitState>,
    fences: watch::Sender<FenceState>,
    /// Assets whose version lock a task waits for, with the paths it decides after it, each
    /// with the actor of the report that gave it, if any.
    deferring: Mutex<HashMap<String, Vec<Reported>>>,
    /// The reporters of the reads a report's reconcile parked, by path key: the path's next
    /// decision, whatever it is, takes the actor (module docs, "Who made a change").
    parked_actors: Mutex<HashMap<String, Actor>>,
    /// The human edits recorded with no actor, by workspace path key: their hash and when (the
    /// core's monotonic clock), for a claim within [`CLAIM_WINDOW`].
    unclaimed: Mutex<HashMap<String, (String, Duration)>>,
    /// `watcher_unavailable` was published.
    unavailable: AtomicBool,
    /// Wakes the loop when git starts outside a batch (`lil notify`, the verification).
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

/// The latest `human_edits` entry of `m` whose path's key is `key`, as written.
fn latest_edit<'a>(m: &'a Manifest, key: &str) -> Option<&'a serde_json::Map<String, Value>> {
    m.get("human_edits")?
        .as_array()?
        .iter()
        .rev()
        .filter_map(Value::as_object)
        .find(|e| {
            e.get("path")
                .and_then(Value::as_str)
                .is_some_and(|p| path_key(p) == key)
        })
}

/// Sets `actor` on the latest `human_edits` entry of `m` whose path's key is `key`.
fn set_latest_actor(m: &mut Manifest, key: &str, actor: &Actor) {
    let Some(Value::Array(mut edits)) = m.get("human_edits").cloned() else {
        return;
    };
    let latest = edits
        .iter_mut()
        .rev()
        .filter_map(Value::as_object_mut)
        .find(|e| {
            e.get("path")
                .and_then(Value::as_str)
                .is_some_and(|p| path_key(p) == key)
        });
    if let Some(entry) = latest {
        entry.insert("actor".into(), actor.to_value());
        m.set("human_edits", Value::Array(edits));
    }
}

/// A workspace's watcher; [`Watcher::stop`] (or the workspace's close) ends it.
pub struct Watcher {
    shared: Arc<Shared>,
    dir: Mutex<Option<Arc<DirWatcher>>>,
    /// The watch of a git folder outside the workspace (a `gitdir:` file's), if any.
    git_dir: Mutex<Option<Arc<DirWatcher>>>,
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
        watcher.watch_outside_git_dir();
        Ok(watcher)
    }

    /// Watches the git folder a `.git` file names when it isn't the workspace's own `.git`
    /// folder (module docs): its `index.lock` and `HEAD` (or a lost event there) count as git
    /// activity. A folder that can't be watched is logged; the gate still checks its
    /// `index.lock` before every write.
    fn watch_outside_git_dir(&self) {
        let root = self.shared.store.root();
        let git_dir = self.shared.store.git_gate().git_dir().to_path_buf();
        if git_dir == root.join(".git") || git_dir.starts_with(root) {
            return;
        }
        match DirWatcher::start(&git_dir, 16 * 1024) {
            Ok((dir, rx)) => {
                *lock(&self.git_dir) = Some(Arc::new(dir));
                tokio::spawn(watch_git(self.shared.clone(), rx, self.stop.clone()));
            }
            Err(e) => {
                tracing::warn!(git_dir = %git_dir.display(), error = %e, "the repository's git folder can't be watched; writes still wait while its index.lock exists");
            }
        }
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

    /// The watcher of a workspace that can't be watched: `lil notify`, the verification and the
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
            parked_actors: Mutex::new(HashMap::new()),
            unclaimed: Mutex::new(HashMap::new()),
            unavailable: AtomicBool::new(false),
            wake: Notify::new(),
        });
        let stop = CancellationToken::new();
        tokio::spawn(run(shared.clone(), changes, dir.clone(), stop.clone()));
        Arc::new(Watcher {
            shared,
            dir: Mutex::new(dir),
            git_dir: Mutex::new(None),
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
        if let Some(dir) = lock(&self.git_dir).take() {
            dir.stop();
        }
    }

    /// Reconciles `paths` (files, or folders with their subtrees) and decides what changed:
    /// `lil notify` (ARCH-016, ARCH-073), reported by `actor` (module docs, "Who made a
    /// change"). Paths the watcher doesn't watch are `ignored`. While git works the paths are
    /// deferred to the post-git rescan. Waits for the workspace's first reconcile.
    pub async fn notify(&self, paths: Vec<WsPath>, actor: Option<Actor>) -> NotifyReport {
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
            let reporters: Vec<Option<Reporter>> = keep
                .iter()
                .map(|path| actor.as_ref().map(|a| s.reporter(path, a)))
                .collect();
            let mut git = lock(&s.git);
            for (path, reporter) in keep.into_iter().zip(reporters) {
                git.defer(path, reporter);
            }
            return report;
        }
        s.reconcile_paths(keep, actor.as_ref()).await;
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
                // it since): the batch, `lil notify` or the next verification decides the
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
                actor: None,
            })
            .await;
        }
    }

    /// Returns once the workspace's first reconcile has finished and the watcher has handled
    /// `.lilith/fence/<name>` and everything before it, deferred removals included. It
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

/// The loop of an outside git folder's watch: `index.lock` and `HEAD` at its top (or lost
/// events) are git activity; nothing else there is looked at.
async fn watch_git(
    shared: Arc<Shared>,
    mut rx: UnboundedReceiver<DirChange>,
    stop: CancellationToken,
) {
    loop {
        let change = tokio::select! {
            biased;
            () = stop.cancelled() => return,
            change = rx.recv() => change,
        };
        let path = match change {
            None => return,
            Some(DirChange::Overflow) => {
                shared.git_activity();
                continue;
            }
            Some(DirChange::Failed(code)) => {
                tracing::warn!(code, "the repository's git folder can no longer be watched");
                return;
            }
            Some(
                DirChange::Added(p)
                | DirChange::Removed(p)
                | DirChange::Modified(p)
                | DirChange::RenamedFrom(p)
                | DirChange::RenamedTo(p),
            ) => p,
        };
        let mut parts = path.components();
        let (Some(name), None) = (parts.next(), parts.next()) else {
            continue;
        };
        let name = name.as_os_str().to_string_lossy();
        if name.eq_ignore_ascii_case("index.lock") || name.eq_ignore_ascii_case("HEAD") {
            shared.git_activity();
        }
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
        Some(format!("asset:{SCHEME}{}/{project}/{asset}", self.ws_id))
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
            lil_platform::fs::attributes(self.store.root()).map_err(|e| e.raw_os_error()),
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
                git.defer(path, None);
            }
            git.overflowed |= overflow;
        } else if overflow {
            let (_, decided) = self.full_rescan(&[]).await;
            self.apply_all(decided).await;
            self.events.publish("workspace", json!({"kind": "rescan"}));
        } else {
            self.reconcile_paths(paths, None).await;
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
        let key = |id: &lil_platform::fs::FileIdentity| (id.size, id.last_write, id.change_time);
        now.as_ref().map(key) != Some(key(&m.identity))
    }

    /// Whether `project/asset`'s version lock is held now (a sign-off between `seal` and its
    /// Record stage).
    fn version_lock_held(&self, project: &str, asset: &str) -> bool {
        self.store.version_lock(project, asset).try_lock().is_err()
    }

    /// Whether the event path `rel` (relative to the workspace folder) is a folder now.
    fn is_folder(&self, rel: &Path) -> bool {
        lil_platform::fs::attributes(&self.store.root().join(rel))
            .is_ok_and(|a| a & FILE_ATTRIBUTE_DIRECTORY != 0)
    }

    /// Reconciles `paths`, re-checks the parked paths, and decides and applies what changed.
    /// A path in (or of) an asset folder whose version lock is held waits for the lock: the
    /// sign-off moves files there, and a reconcile meanwhile would take its moves for removals.
    /// With a reporter's `actor`, the human edits the reconcile of `paths` records get it (also
    /// later, for a path deferred to a version lock or a read parked), and an agent's actor may
    /// claim the entry of each file given ([`Shared::claim`]).
    async fn reconcile_paths(self: &Arc<Self>, all: Vec<WsPath>, actor: Option<&Actor>) {
        let mut paths = Vec::with_capacity(all.len());
        let mut seen = HashSet::new();
        for path in all {
            if !seen.insert(path_key(path.as_str())) {
                continue;
            }
            match asset_folder_of(&path) {
                Some((project, asset)) if self.version_lock_held(project, asset) => {
                    let reporter = actor.map(|a| self.reporter(&path, a));
                    self.defer_asset(project, asset, path.clone(), reporter);
                }
                _ => paths.push(path),
            }
        }
        if !paths.is_empty() {
            match self.index.reconcile(Scope::Paths(paths.clone())).await {
                Ok(report) => {
                    if let Some(actor) = actor {
                        let mut parked = lock(&self.parked_actors);
                        for path in &report.parked {
                            parked.insert(path_key(path.as_str()), actor.clone());
                        }
                    }
                    let mut decided = self.decide_report(&report, &[]).await;
                    for d in &mut decided {
                        d.actor = actor.cloned();
                    }
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
                // A read no longer parked was decided (and took its reporter) or dropped by the
                // index: its reporter goes, so it never labels a later edit.
                let still: HashSet<String> =
                    report.parked.iter().map(|p| path_key(p.as_str())).collect();
                lock(&self.parked_actors).retain(|key, _| still.contains(key));
            }
            Err(e) => tracing::warn!(error = %e, "parked paths couldn't be re-checked"),
        }
        if let Some(actor) = actor {
            for path in &paths {
                self.claim(path, actor).await;
            }
        }
    }

    /// The hash of the human edit of `path` (a workspace path) the watcher recorded with no
    /// actor at most [`CLAIM_WINDOW`] ago, if any. Older records are dropped.
    fn recorded_claim(&self, path: &WsPath) -> Option<String> {
        let now = self.store.clock().monotonic();
        let mut unclaimed = lock(&self.unclaimed);
        unclaimed.retain(|_, (_, at)| now.saturating_sub(*at) <= CLAIM_WINDOW);
        unclaimed
            .get(&path_key(path.as_str()))
            .map(|(recorded, _)| recorded.clone())
    }

    /// Whether the watcher recorded a human edit of `path` with `sha256` and no actor at most
    /// [`CLAIM_WINDOW`] ago.
    fn claimable(&self, path: &WsPath, sha256: &str) -> bool {
        self.recorded_claim(path).as_deref() == Some(sha256)
    }

    /// The reporter of a path a report defers: `actor`, with the claim window measured now,
    /// at the report (N1).
    fn reporter(&self, path: &WsPath, actor: &Actor) -> Reporter {
        Reporter {
            actor: actor.clone(),
            claim: is_agent(actor).then(|| self.recorded_claim(path)).flatten(),
        }
    }

    /// The claim of a deferred path's reporter, after the deferral: the window was measured at
    /// the report when it found a recorded edit, else it is measured now.
    async fn claim_deferred(&self, path: &WsPath, reporter: &Reporter) {
        self.claim_as(path, &reporter.actor, reporter.claim.as_deref())
            .await;
    }

    async fn claim(&self, path: &WsPath, actor: &Actor) {
        self.claim_as(path, actor, None).await;
    }

    /// Fills the agent's `actor` into the latest `human_edits` entry of the file `path` when
    /// the claim's conditions hold (module docs, "Who made a change"): an agent's actor, an
    /// entry with the file's current hash and no actor, recorded by the watcher within
    /// [`CLAIM_WINDOW`] (before now, or `measured`: the hash of the edit recorded within the
    /// window before a deferred report). Anything else is left as it is. A claim that wrote
    /// publishes `{kind: "manifest"}` on the asset's topic.
    async fn claim_as(&self, path: &WsPath, actor: &Actor, measured: Option<&str>) {
        if !is_agent(actor) || !matches!(kind_of(path), FileKind::Candidate | FileKind::Sealed) {
            return;
        }
        let row = match self.index.files_of_path(path).await {
            Ok(Some(row)) if row.state == FileState::HumanEdit => row,
            _ => return,
        };
        let (Some(sha256), Some(manifest)) = (row.sha256, self.manifest_path(path).await) else {
            return;
        };
        let in_window = match measured {
            Some(recorded) => recorded == sha256,
            None => self.claimable(path, &sha256),
        };
        if !in_window {
            return;
        }
        let key = path_key(&Self::in_asset(path));
        let unclaimed = |m: &Manifest| {
            latest_edit(m, &key).is_some_and(|e| {
                e.get("sha256")
                    .and_then(Value::as_str)
                    .and_then(bare_sha256)
                    == Some(sha256.as_str())
                    && e.get("actor").is_none_or(Value::is_null)
            })
        };
        let Some(bytes) = self.store.read(&manifest).await.ok().flatten() else {
            return;
        };
        let current = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|t| Manifest::parse(t).ok());
        if !current.is_some_and(|(m, _)| unclaimed(&m)) {
            return;
        }
        let claimed = AtomicBool::new(false);
        let edit = |m: &mut Manifest| -> Result<(), StoreError> {
            let fill = unclaimed(m);
            if fill {
                set_latest_actor(m, &key, actor);
            }
            claimed.store(fill, Ordering::SeqCst);
            Ok(())
        };
        match self.store.edit_manifest(&manifest, &edit).await {
            Ok(_) if claimed.load(Ordering::SeqCst) => {
                lock(&self.unclaimed).remove(&path_key(path.as_str()));
                tracing::info!(path = %path, kind = actor.kind.as_str(), "a recorded human edit was claimed by the agent that reported it");
                self.publish_asset(path, json!({"kind": "manifest"}));
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(path = %path, error = %e, "the actor of a recorded human edit couldn't be written");
            }
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

    /// Marks every fence file in `.lilith/fence/` handled (test fences, QA-031).
    fn seed_fences(&self) {
        let fence_dir = self.store.root().join(WORKSPACE_DIR).join("fence");
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
                actor: None,
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
        tokio::task::spawn_blocking(move || lil_platform::fs::attributes(&snapshot).is_ok())
            .await
            .unwrap_or(false)
    }

    async fn apply_all(self: &Arc<Self>, decided: Vec<Decided>) {
        for d in decided {
            self.apply(d).await;
        }
    }

    /// Carries out one decision (module docs). A decision of a path whose read a report parked
    /// takes that reporter's actor.
    async fn apply(self: &Arc<Self>, mut d: Decided) {
        let parked = lock(&self.parked_actors).remove(&path_key(d.obs.path().as_str()));
        if d.actor.is_none() {
            d.actor = parked;
        }
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
            } => {
                self.human_edit(&manifest, &path, &sha256, &previous, d.actor.as_ref())
                    .await;
            }
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
                    let reporter = d.actor.clone().map(|actor| Reporter { actor, claim: None });
                    self.defer_asset(project, asset, path.clone(), reporter);
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
                let reporter = d.actor.clone().map(|actor| Reporter { actor, claim: None });
                git.defer(path, reporter);
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

    /// Pushes `{path, sha256, previous_sha256, date, actor?}` to the manifest's `human_edits`,
    /// unless its latest entry for the path already has this hash (then an agent's `actor` is
    /// filled in only as a claim allows: [`Shared::claim`]); the file itself is never touched
    /// (CORE-034). Then `asset:` `{kind:"human_edit"}` (or `{kind:"manifest"}` for a claim).
    /// An entry pushed with no actor is remembered for a claim within [`CLAIM_WINDOW`].
    ///
    /// It runs under the decision lock, and the manifest edit waits at the store's git gate
    /// (write step 2). Should git take `index.lock` again between the decision and the write,
    /// the edit waits there for up to 60 s, and the watcher's batches with it; then it fails
    /// with `store.git_busy` and is logged. That fails safe: nothing is overwritten, and the
    /// file stays different from its manifest, so the rescan after git or the next
    /// verification finds it again.
    async fn human_edit(
        &self,
        manifest: &WsPath,
        path: &WsPath,
        sha256: &str,
        previous: &str,
        actor: Option<&Actor>,
    ) {
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
        let date = lil_schema::time::date(self.store.now_utc());
        let mut entry = json!({
            "path": inside,
            "sha256": sha256,
            "previous_sha256": previous,
            "date": date,
        });
        if let Some(actor) = actor {
            entry["actor"] = actor.to_value();
        }
        let key = path_key(&inside);
        let may_claim = actor.is_some_and(is_agent) && self.claimable(path, sha256);
        let (pushed, claimed) = (AtomicBool::new(false), AtomicBool::new(false));
        let edit = |m: &mut Manifest| -> Result<(), StoreError> {
            pushed.store(false, Ordering::SeqCst);
            claimed.store(false, Ordering::SeqCst);
            let recorded = m
                .view()
                .human_edits
                .iter()
                .rev()
                .find(|e| path_key(&e.path) == key)
                .is_some_and(|e| e.sha256 == sha256);
            if recorded {
                if let Some(actor) = actor
                    && may_claim
                    && latest_edit(m, &key)
                        .is_some_and(|e| e.get("actor").is_none_or(Value::is_null))
                {
                    set_latest_actor(m, &key, actor);
                    claimed.store(true, Ordering::SeqCst);
                }
                return Ok(());
            }
            pushed.store(true, Ordering::SeqCst);
            m.push("human_edits", entry.clone())
                .map_err(|e| StoreError::manifest_edit(manifest, &e))
        };
        match self.store.edit_manifest(manifest, &edit).await {
            Ok(_) if claimed.load(Ordering::SeqCst) => {
                lock(&self.unclaimed).remove(&path_key(path.as_str()));
                self.accept(path, sha256, FileState::HumanEdit).await;
                tracing::info!(path = %path, "a recorded human edit was claimed by the agent that reported it");
                self.publish_asset(path, json!({"kind": "manifest"}));
            }
            Ok(_) => {
                if actor.is_none() && pushed.load(Ordering::SeqCst) {
                    let now = self.store.clock().monotonic();
                    lock(&self.unclaimed)
                        .insert(path_key(path.as_str()), (sha256.to_string(), now));
                }
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
    ///
    /// `reporter` is the report that gave `path`, if any: the later decisions of the path (or
    /// of its subtree) record its actor, a read the reconcile parks keeps it, and an agent's
    /// may claim the path's entry then.
    fn defer_asset(
        self: &Arc<Self>,
        project: &str,
        asset: &str,
        path: WsPath,
        reporter: Option<Reporter>,
    ) {
        let key = path_key(&format!("{project}/{asset}"));
        {
            let mut deferring = lock(&self.deferring);
            if let Some(paths) = deferring.get_mut(&key) {
                paths.push((path, reporter));
                return;
            }
            deferring.insert(key.clone(), vec![(path, reporter)]);
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
                    let mut git = lock(&shared.git);
                    git.defer(folder, None);
                    for (path, reporter) in &paths {
                        git.defer(path.clone(), reporter.clone());
                    }
                } else {
                    match shared.index.reconcile(Scope::Paths(vec![folder])).await {
                        Ok(report) => {
                            let extra: Vec<WsPath> = paths.iter().map(|(p, _)| p.clone()).collect();
                            let mut decided = shared.decide_report(&report, &extra).await;
                            shared.keep_parked_reporters(&report, &paths, &mut decided);
                            stamp(&mut decided, &paths);
                            shared.apply_all(decided).await;
                            for (path, reporter) in &paths {
                                if let Some(reporter) = reporter {
                                    shared.claim_deferred(path, reporter).await;
                                }
                            }
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
        let paths: Vec<WsPath> = deferred.iter().map(|(p, _)| p.clone()).collect();
        let (report, mut decided) = self.full_rescan(&paths).await;
        self.keep_parked_reporters(&report, &deferred, &mut decided);
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
                actor: None,
            });
        }
        // The reports made while git worked name their paths' actors.
        stamp(&mut decided, &deferred);
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
        for (path, reporter) in &deferred {
            if let Some(reporter) = reporter {
                self.claim_deferred(path, reporter).await;
            }
        }
    }

    /// The reads a deferred reconcile parked keep the reporter of their path (or of a folder
    /// above it) until their next decision (N2), and leave `decided`: a parked read is decided
    /// by its re-check, not from the row it left as it was.
    fn keep_parked_reporters(
        &self,
        report: &ReconcileReport,
        reported: &[Reported],
        decided: &mut Vec<Decided>,
    ) {
        let keys: HashSet<String> = report.parked.iter().map(|p| path_key(p.as_str())).collect();
        decided.retain(|d| !keys.contains(&path_key(d.obs.path().as_str())));
        let mut parked = lock(&self.parked_actors);
        for path in &report.parked {
            if let Some(actor) = reporter_of(path, reported) {
                parked.insert(path_key(path.as_str()), actor);
            }
        }
    }
}

#[cfg(test)]
mod tests;
