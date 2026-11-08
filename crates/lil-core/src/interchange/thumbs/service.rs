//! [`ThumbService`]: the core's thumbnail cache (APP-038; Appendix H "Thumbnails"; P4b-R12,
//! P4b-R36), the [`Thumbnails`] that `app/thumbs.request` and the pick log reach.
//!
//! **The workspace per call** (pre-flight A67): the core serves one default workspace, which
//! can change, and neither [`Thumbnails`] method names one, so every call resolves the
//! default workspace anew; nothing is kept from construction. Its local data folder
//! (`Env::workspace_local_dir_for_canonical`, ARCH-097) holds the cache.
//!
//! **One item.** The media kind comes from the extension; a kind without a renderer of its
//! own, or an image `image@1` doesn't decode (a `.gif`), gets `icon@1`. The expected hash is
//! the index's for the path (`app/thumbs.request`) or the caller's (`ensure`). A known hash
//! whose cache entry exists is `ready` at once, without reading the file; an `icon@1` item
//! with a known hash is `icon` at once. Anything else is a **job** on a blocking thread, which
//! opens the file (never a cloud placeholder, and only when it really is inside the
//! workspace), checks its size, reads it, hashes the bytes it read and renders those, under a
//! permit of [`default_permits`] and the [`DECODE_DEADLINE`]; a renderer runs under
//! `catch_unwind`. Its PNG is filed under the hash of the bytes read
//! (`fsops::write_workspace_local_file`). When that hash differs from the expected one, the
//! answer is `pending`, the index is asked to re-check the file (the watcher's `lil notify`
//! path), and the result is announced. Any failure, a passed deadline among them, gives the
//! `icon@1` state, never an error; a late result is discarded.
//!
//! **Waiting.** Jobs of one cache key are shared. `app/thumbs.request` waits for its jobs up to
//! [`INLINE_WAIT`] in all; an item not done by then answers `pending`, and its result is
//! published on topic `thumbs` as `{kind: "ready", sha256, renderer, px, file}` or
//! `{kind: "icon", sha256, media_kind}` once it comes. `ensure` waits for its job.
//!
//! **Clean-up** ([`ThumbService::clean_up`], [`ThumbService::spawn_clean_up`]): after the
//! default workspace's first finished reconcile of each core run and every 24 h, skipped while
//! the index rebuilds; an entry whose hash neither the index's files (unverified rows
//! included) nor a [`LiveSource`] (the inbox, Task 18) hold, and whose file was last written
//! more than [`KEEP_UNTOUCHED`] ago on the core's clock, is removed through
//! `fsops::remove_workspace_local_file`.

use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use futures_util::future::BoxFuture;
use lil_platform::fs::Probe;
use lil_protocol::app::ThumbItem;
use lil_schema::display::{media_kind_of, renderable};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::{Notify, Semaphore};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::cache::{Entry, KEEP_UNTOUCHED, entry_path, is_sha256, list_entries};
use super::renderers::{ICON, MAX_FILE_BYTES, Renderer, default_renderers};
use crate::events::EventBus;
use crate::index::{FileState, Scope};
use crate::store::{WsPath, fsops, path_key};
use crate::tools::{ThumbState, Thumbnails};
use crate::workspace::{OpenWorkspace, WorkspaceHub};

/// Each decode's deadline (P4b-R36): after it the item answers `icon` and its permit is
/// released.
pub const DECODE_DEADLINE: Duration = Duration::from_secs(10);
/// How long `app/thumbs.request` waits for its renders before it answers `pending`.
pub const INLINE_WAIT: Duration = Duration::from_millis(150);
/// How often clean-up runs while the core serves (after its first pass).
pub const CLEAN_UP_PERIOD: Duration = Duration::from_secs(24 * 60 * 60);
/// The topic the results of `pending` items are announced on.
pub const THUMBS_TOPIC: &str = "thumbs";

/// How many decodes run at once: `min(4, cores / 2)`, at least one.
pub fn default_permits() -> usize {
    std::thread::available_parallelism().map_or(1, |n| (n.get() / 2).clamp(1, 4))
}

/// More hashes clean-up keeps, read from the open workspace (the inbox's, Task 18).
pub type LiveSource =
    Arc<dyn Fn(Arc<OpenWorkspace>) -> BoxFuture<'static, HashSet<String>> + Send + Sync>;

/// How a [`ThumbService`] renders.
#[derive(Clone)]
pub struct ThumbOptions {
    /// The registry, in order: the first that renders a kind draws it ([`default_renderers`]).
    pub renderers: Vec<Arc<dyn Renderer>>,
    /// How many renders run at once ([`default_permits`]).
    pub permits: usize,
    /// Each render's deadline ([`DECODE_DEADLINE`]).
    pub deadline: Duration,
    /// Each render waits this long first (`LIL_TEST_THUMB_SLOW_MS`; `None` in production).
    pub slow: Option<Duration>,
    /// How long a request waits for its renders ([`INLINE_WAIT`]).
    pub inline_wait: Duration,
}

impl Default for ThumbOptions {
    fn default() -> ThumbOptions {
        ThumbOptions {
            renderers: default_renderers(),
            permits: default_permits(),
            deadline: DECODE_DEADLINE,
            slow: None,
            inline_wait: INLINE_WAIT,
        }
    }
}

/// The thumbnail cache (module docs).
pub struct ThumbService {
    me: Weak<ThumbService>,
    hub: Arc<WorkspaceHub>,
    events: Arc<EventBus>,
    renderers: Vec<Arc<dyn Renderer>>,
    permits: Arc<Semaphore>,
    permit_count: usize,
    deadline: Duration,
    slow: Option<Duration>,
    inline_wait: Duration,
    /// The jobs running, by cache key.
    inflight: Mutex<HashMap<String, Arc<Inflight>>>,
    live_sources: Mutex<Vec<LiveSource>>,
}

impl std::fmt::Debug for ThumbService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThumbService")
            .field("permits", &self.permit_count)
            .field("deadline", &self.deadline)
            .finish_non_exhaustive()
    }
}

/// A running job's result, shared by everyone waiting for it.
#[derive(Default)]
struct Inflight {
    slot: Mutex<Slot>,
    done: Notify,
}

#[derive(Default)]
struct Slot {
    outcome: Option<Outcome>,
    /// Someone answered `pending` for it: its result is published.
    announce: bool,
}

/// A job's result: the item's final state, and whether the bytes read had another hash than
/// the expected one (then the answer is `pending` and the result is published anyway).
#[derive(Debug, Clone)]
struct Outcome {
    state: ThumbState,
    mismatch: bool,
}

impl Outcome {
    /// What a caller is answered: the state, or `pending` after a mismatch.
    fn answer(&self) -> ThumbState {
        if self.mismatch {
            ThumbState {
                state: "pending".to_string(),
                file: None,
                ..self.state.clone()
            }
        } else {
            self.state.clone()
        }
    }
}

/// What a blocking job found.
enum Work {
    /// The file can't be opened or read, or isn't inside the workspace.
    Unreadable,
    /// No image: the hash of the bytes read.
    Icon(String),
    /// The cache already has the entry of the hash read.
    Cached(String),
    /// The rendered PNG of the bytes of this hash.
    Rendered(String, Vec<u8>),
}

/// What one item needs.
enum Plan {
    Done(ThumbState),
    Wait {
        inflight: Arc<Inflight>,
        pending: ThumbState,
    },
}

/// The open default workspace and its local data folder.
struct Ctx {
    open: Arc<OpenWorkspace>,
    local: PathBuf,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An expected hash as the cache keys it: 64 hex digits, lowercased. Anything else (a
/// manifest is a workspace file anyone can edit) counts as unknown, so the file is hashed on
/// demand; it never becomes part of a cache path.
fn checked_sha(sha: &str) -> Option<String> {
    let sha = sha.to_ascii_lowercase();
    is_sha256(&sha).then_some(sha)
}

fn label(renderer: &dyn Renderer) -> String {
    format!("{}@{}", renderer.id(), renderer.version())
}

impl ThumbService {
    /// A service over `hub`'s default workspace, publishing on `events`.
    pub fn new(
        hub: Arc<WorkspaceHub>,
        events: Arc<EventBus>,
        options: ThumbOptions,
    ) -> Arc<ThumbService> {
        let permit_count = options.permits.max(1);
        Arc::new_cyclic(|me| ThumbService {
            me: me.clone(),
            hub,
            events,
            renderers: options.renderers,
            permits: Arc::new(Semaphore::new(permit_count)),
            permit_count,
            deadline: options.deadline,
            slow: options.slow,
            inline_wait: options.inline_wait,
            inflight: Mutex::new(HashMap::new()),
            live_sources: Mutex::new(Vec::new()),
        })
    }

    /// How many renders run at once.
    pub fn permits(&self) -> usize {
        self.permit_count
    }

    /// Adds hashes clean-up keeps (the inbox's entries, Task 18).
    pub fn add_live_source(&self, source: LiveSource) {
        lock(&self.live_sources).push(source);
    }

    /// The renderer that draws `name`: the first registered one other than `icon@1` that
    /// renders its kind, for a file `image@1` decodes (`lil_schema::display::renderable`);
    /// else `icon@1`.
    fn renderer_for(&self, name: &str) -> Option<Arc<dyn Renderer>> {
        let kind = media_kind_of(name);
        let drawn = renderable(name)
            .then(|| {
                self.renderers
                    .iter()
                    .find(|r| r.id() != ICON && r.renders(kind))
            })
            .flatten();
        drawn
            .or_else(|| self.renderers.iter().find(|r| r.id() == ICON))
            .cloned()
    }

    /// The default workspace, opened, and its local data folder; `None` when there is none or
    /// it can't be opened.
    async fn ctx(&self) -> Option<Ctx> {
        let ws = self.hub.default_ref()?;
        let open = match self.hub.open(&ws).await {
            Ok(open) => open,
            Err(e) => {
                tracing::debug!(code = %e.code, "no thumbnails: the default workspace can't be opened");
                return None;
            }
        };
        let local = self
            .hub
            .env()
            .workspace_local_dir_for_canonical(&open.ws.path)
            .inspect_err(
                |e| tracing::warn!(error = %e, "no thumbnails: the workspace has no local folder"),
            )
            .ok()?;
        Some(Ctx { open, local })
    }

    /// The local data folder of the default workspace, without opening it.
    fn default_local_dir(&self) -> Option<PathBuf> {
        let ws = self.hub.default_ref()?;
        self.hub
            .env()
            .workspace_local_dir_for_canonical(&ws.path)
            .ok()
    }

    /// The `icon@1` state of `path`, with `sha256` if known.
    fn icon(&self, path: &str, sha256: Option<String>, px: u32) -> ThumbState {
        let icon = self
            .renderers
            .iter()
            .find(|r| r.id() == ICON)
            .map_or_else(|| "icon@1".to_string(), |r| label(r.as_ref()));
        ThumbState {
            path: path.to_string(),
            sha256,
            media_kind: media_kind_of(path).as_str().to_string(),
            renderer: icon,
            state: "icon".to_string(),
            file: None,
            px,
        }
    }

    /// The states of `items` at `px`; each waits for its job until `wait` has passed
    /// (`None`: until it is done). `expected` is the caller's hash for every item (`ensure`),
    /// or `None` to ask the index.
    async fn states(
        &self,
        items: &[String],
        expected: Option<Option<&str>>,
        px: u32,
        wait: Option<Duration>,
    ) -> Vec<ThumbState> {
        let Some(ctx) = self.ctx().await else {
            return items.iter().map(|p| self.icon(p, None, px)).collect();
        };
        let until = wait.map(|w| Instant::now() + w);
        let mut plans = Vec::with_capacity(items.len());
        for path in items {
            plans.push(self.plan(&ctx, path, expected, px).await);
        }
        let mut out = Vec::with_capacity(items.len());
        for plan in plans {
            out.push(match plan {
                Plan::Done(state) => state,
                Plan::Wait { inflight, pending } => wait_for(&inflight, until)
                    .await
                    .map_or(pending, |o| o.answer()),
            });
        }
        out
    }

    /// What `path` needs (module docs "One item").
    async fn plan(&self, ctx: &Ctx, path: &str, expected: Option<Option<&str>>, px: u32) -> Plan {
        let (Ok(ws_path), Some(me)) = (WsPath::parse(path), self.me.upgrade()) else {
            return Plan::Done(self.icon(path, None, px));
        };
        let Some(renderer) = self.renderer_for(path) else {
            return Plan::Done(self.icon(path, None, px));
        };
        let expected: Option<String> = match expected {
            Some(given) => given.and_then(checked_sha),
            None => match ctx.open.index.files_of_path(&ws_path).await {
                Ok(Some(row)) => {
                    if matches!(
                        row.state,
                        FileState::CloudOnly | FileState::NotFetched | FileState::Missing
                    ) {
                        // Its bytes aren't here: never read (a placeholder would download).
                        let sha = row.sha256.as_deref().and_then(checked_sha);
                        return Plan::Done(self.icon(path, sha, px));
                    }
                    row.sha256.as_deref().and_then(checked_sha)
                }
                Ok(None) => None,
                Err(e) => {
                    tracing::debug!(error = %e, "the index can't say a thumbnail's hash; it is hashed on demand");
                    None
                }
            },
        };
        let icon_only = renderer.id() == ICON;
        if icon_only && let Some(sha) = &expected {
            return Plan::Done(self.icon(path, Some(sha.clone()), px));
        }
        let key = match &expected {
            Some(sha) => {
                let entry = entry_path(&ctx.local, sha, renderer.id(), renderer.version(), px);
                if entry.is_file() {
                    return Plan::Done(ThumbState {
                        path: path.to_string(),
                        sha256: Some(sha.clone()),
                        media_kind: media_kind_of(path).as_str().to_string(),
                        renderer: label(renderer.as_ref()),
                        state: "ready".to_string(),
                        file: Some(entry),
                        px,
                    });
                }
                format!("{sha}.{}@{}.{px}", renderer.id(), renderer.version())
            }
            None => format!(
                "path:{}:{}@{}.{px}",
                path_key(ws_path.as_str()),
                renderer.id(),
                renderer.version()
            ),
        };
        let pending = ThumbState {
            path: path.to_string(),
            sha256: expected.clone(),
            media_kind: media_kind_of(path).as_str().to_string(),
            renderer: label(renderer.as_ref()),
            state: "pending".to_string(),
            file: None,
            px,
        };
        let (inflight, start) = {
            let mut map = lock(&self.inflight);
            match map.get(&key) {
                Some(running) => (running.clone(), false),
                None => {
                    let made = Arc::new(Inflight::default());
                    map.insert(key.clone(), made.clone());
                    (made, true)
                }
            }
        };
        if start {
            let job = Job {
                path: path.to_string(),
                abs: ws_path.abs(&ctx.open.ws.path),
                ws_path,
                root: ctx.open.ws.path.clone(),
                probe: ctx.open.store.probe().clone(),
                local: ctx.local.clone(),
                expected,
                renderer,
                px,
                open: Arc::downgrade(&ctx.open),
            };
            let inflight = inflight.clone();
            tokio::spawn(async move {
                let outcome = me.run(job).await;
                me.finish(&key, &inflight, outcome);
            });
        }
        Plan::Wait { inflight, pending }
    }

    /// Runs `job` under a permit and the deadline, then files its PNG.
    async fn run(&self, job: Job) -> Outcome {
        let Ok(permit) = self.permits.clone().acquire_owned().await else {
            return self.failed(&job, None);
        };
        let (slow, renderer) = (self.slow, job.renderer.clone());
        let (abs, root, probe, local, px) = (
            job.abs.clone(),
            job.root.clone(),
            job.probe.clone(),
            job.local.clone(),
            job.px,
        );
        let blocking = tokio::task::spawn_blocking(move || {
            work(
                &abs,
                &root,
                probe.as_ref(),
                &local,
                renderer.as_ref(),
                px,
                slow,
            )
        });
        let worked = tokio::time::timeout(self.deadline, blocking).await;
        // The permit goes back now, also when the blocking thread is still running (P4b-R36).
        drop(permit);
        let work = match worked {
            Ok(Ok(work)) => work,
            Ok(Err(e)) => {
                tracing::warn!(path = %job.path, error = %e, "a thumbnail render stopped; the item gets the icon");
                return self.failed(&job, job.expected.clone());
            }
            Err(_) => {
                tracing::warn!(path = %job.path, deadline = ?self.deadline, "a thumbnail render passed its deadline; the item gets the icon and its late result is discarded");
                return self.failed(&job, job.expected.clone());
            }
        };
        let (sha, file) = match work {
            Work::Unreadable => return self.failed(&job, None),
            Work::Icon(sha) => (sha, None),
            Work::Cached(sha) => {
                let entry = entry_path(
                    &job.local,
                    &sha,
                    job.renderer.id(),
                    job.renderer.version(),
                    job.px,
                );
                (sha, Some(entry))
            }
            Work::Rendered(sha, png) => {
                let entry = entry_path(
                    &job.local,
                    &sha,
                    job.renderer.id(),
                    job.renderer.version(),
                    job.px,
                );
                let (local, target) = (job.local.clone(), entry.clone());
                let written = tokio::task::spawn_blocking(move || {
                    fsops::write_workspace_local_file(&local, &target, &png)
                })
                .await
                .unwrap_or_else(|e| Err(std::io::Error::other(e.to_string())));
                match written {
                    Ok(()) => (sha, Some(entry)),
                    Err(e) => {
                        tracing::warn!(path = %job.path, error = %e, "a thumbnail could not be written to the cache; the item gets the icon");
                        (sha, None)
                    }
                }
            }
        };
        let mismatch = job.expected.as_ref().is_some_and(|e| *e != sha);
        if mismatch {
            tracing::info!(path = %job.path, expected = ?job.expected, read = %sha, "a file's bytes differ from the index's hash; the index re-checks it");
            recheck(job.open.clone(), job.ws_path.clone());
        }
        let state = match file {
            Some(file) => ThumbState {
                path: job.path.clone(),
                sha256: Some(sha),
                media_kind: media_kind_of(&job.path).as_str().to_string(),
                renderer: label(job.renderer.as_ref()),
                state: "ready".to_string(),
                file: Some(file),
                px: job.px,
            },
            None => self.icon(&job.path, Some(sha), job.px),
        };
        Outcome { state, mismatch }
    }

    fn failed(&self, job: &Job, sha256: Option<String>) -> Outcome {
        Outcome {
            state: self.icon(&job.path, sha256, job.px),
            mismatch: false,
        }
    }

    /// Records `outcome` for everyone waiting on `key`, and publishes it when someone answered
    /// `pending` (or the hash differed).
    fn finish(&self, key: &str, inflight: &Arc<Inflight>, outcome: Outcome) {
        let announce = {
            let mut slot = lock(&inflight.slot);
            slot.outcome = Some(outcome.clone());
            slot.announce || outcome.mismatch
        };
        {
            let mut map = lock(&self.inflight);
            if map
                .get(key)
                .is_some_and(|running| Arc::ptr_eq(running, inflight))
            {
                map.remove(key);
            }
        }
        inflight.done.notify_waiters();
        if announce {
            let s = &outcome.state;
            let data = match &s.file {
                Some(file) if s.state == "ready" => json!({
                    "kind": "ready",
                    "sha256": s.sha256,
                    "renderer": s.renderer,
                    "px": s.px,
                    "file": file.to_string_lossy(),
                }),
                _ => json!({
                    "kind": "icon",
                    "sha256": s.sha256,
                    "media_kind": s.media_kind,
                }),
            };
            self.events.publish(THUMBS_TOPIC, data);
        }
    }

    /// Removes the default workspace's cache entries whose hash `live` doesn't hold and whose
    /// file was last written more than [`KEEP_UNTOUCHED`] ago on the core's clock; nothing
    /// while the index isn't settled (`index_settled` false: it rebuilds). Returns how many
    /// were removed.
    pub async fn clean_up(&self, live: &dyn Fn(&str) -> bool, index_settled: bool) -> u64 {
        if !index_settled {
            tracing::info!("the thumbnail clean-up skips this pass: the index rebuilds");
            return 0;
        }
        let Some(local) = self.default_local_dir() else {
            return 0;
        };
        let entries = entries_of(&local).await;
        let doomed = self.doomed(entries, live);
        remove_entries(&local, doomed).await
    }

    /// The entries clean-up removes: a hash `live` doesn't hold, last written more than
    /// [`KEEP_UNTOUCHED`] ago on the core's clock (one written "later" than now is kept).
    fn doomed(&self, entries: Vec<Entry>, live: &dyn Fn(&str) -> bool) -> Vec<PathBuf> {
        let now = self.hub.clock().utc_now();
        entries
            .into_iter()
            .filter(|e| {
                !live(&e.sha256)
                    && now
                        .duration_since(e.modified)
                        .is_ok_and(|age| age > KEEP_UNTOUCHED)
            })
            .map(|e| e.path)
            .collect()
    }
}

/// The cache entries of the local data folder `local`, listed on a blocking thread.
async fn entries_of(local: &Path) -> Vec<Entry> {
    let dir = local.to_path_buf();
    tokio::task::spawn_blocking(move || list_entries(&dir))
        .await
        .unwrap_or_default()
}

/// Removes `doomed` from the cache of `local`; returns how many went.
async fn remove_entries(local: &Path, doomed: Vec<PathBuf>) -> u64 {
    if doomed.is_empty() {
        return 0;
    }
    let dir = local.to_path_buf();
    let removed = tokio::task::spawn_blocking(move || {
        doomed
            .iter()
            .filter(|path| match fsops::remove_workspace_local_file(&dir, path) {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!(file = %path.display(), error = %e, "a thumbnail cache entry could not be removed; the next pass tries again");
                    false
                }
            })
            .count()
    })
    .await
    .unwrap_or(0);
    tracing::info!(removed, "thumbnail cache entries cleaned up");
    u64::try_from(removed).unwrap_or(u64::MAX)
}

impl ThumbService {
    /// One scheduled clean-up pass over the default workspace: once its index has finished its
    /// first reconcile, with the index's file hashes (unverified rows included) and every
    /// [`LiveSource`]'s as live. `None` when it was skipped: no default workspace, an index
    /// that rebuilds or failed, or hashes that can't be read.
    pub async fn clean_up_pass(&self) -> Option<u64> {
        let ctx = self.ctx().await?;
        let index = ctx.open.index.clone();
        index.ready().await;
        if index.failed() || index.rebuilding() {
            tracing::info!("the thumbnail clean-up skips this pass: the index rebuilds or failed");
            return None;
        }
        let mut live = match index.file_hashes().await {
            Ok(live) => live,
            Err(e) => {
                tracing::warn!(error = %e, "the thumbnail clean-up skips this pass: the index's hashes can't be read");
                return None;
            }
        };
        let sources = lock(&self.live_sources).clone();
        for source in sources {
            live.extend(source(ctx.open.clone()).await);
        }
        let local = ctx.local.clone();
        drop(ctx);
        let entries = entries_of(&local).await;
        let doomed = self.doomed(entries, &|sha| live.contains(sha));
        Some(remove_entries(&local, doomed).await)
    }

    /// Runs [`ThumbService::clean_up_pass`] in the background: at once (it waits for the first
    /// reconcile), then every [`CLEAN_UP_PERIOD`] (on tokio's clock), until `stop`.
    pub fn spawn_clean_up(self: &Arc<Self>, stop: CancellationToken) {
        let me = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(CLEAN_UP_PERIOD);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    () = stop.cancelled() => break,
                    _ = ticks.tick() => {}
                }
                let Some(service) = me.upgrade() else {
                    break;
                };
                tokio::select! {
                    () = stop.cancelled() => break,
                    _ = service.clean_up_pass() => {}
                }
            }
        });
    }
}

/// Waits for `inflight`'s outcome until `until` (`None`: until it comes). On the deadline the
/// caller answers `pending`, so the outcome is marked to be published; both are decided under
/// the slot's lock, so no outcome is missed between them.
async fn wait_for(inflight: &Inflight, until: Option<Instant>) -> Option<Outcome> {
    loop {
        let notified = inflight.done.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if let Some(outcome) = &lock(&inflight.slot).outcome {
            return Some(outcome.clone());
        }
        match until {
            None => notified.await,
            Some(until) => {
                if tokio::time::timeout_at(until, notified).await.is_err() {
                    let mut slot = lock(&inflight.slot);
                    if let Some(outcome) = &slot.outcome {
                        return Some(outcome.clone());
                    }
                    slot.announce = true;
                    return None;
                }
            }
        }
    }
}

/// Asks the index to re-check `path` (P4b-R36): through the watcher, as `lil notify` does, or
/// a reconcile of the path without one.
fn recheck(open: Weak<OpenWorkspace>, path: WsPath) {
    tokio::spawn(async move {
        let Some(open) = open.upgrade() else {
            return;
        };
        match &open.watch {
            Some(watch) => {
                watch.notify(vec![path]).await;
            }
            None => {
                if let Err(e) = open.index.reconcile(Scope::Paths(vec![path])).await {
                    tracing::debug!(error = %e, "a thumbnail's re-check of the index failed");
                }
            }
        }
    });
}

/// One render's inputs.
struct Job {
    /// As the caller gave it.
    path: String,
    ws_path: WsPath,
    abs: PathBuf,
    root: PathBuf,
    probe: Arc<dyn Probe>,
    local: PathBuf,
    expected: Option<String>,
    renderer: Arc<dyn Renderer>,
    px: u32,
    open: Weak<OpenWorkspace>,
}

/// Opens `abs` for reading when it is a file inside `root` (a link leading out of the
/// workspace isn't followed out) and not a cloud placeholder (reading one would download it).
fn open_inside(abs: &Path, root: &Path, probe: &dyn Probe) -> Option<std::fs::File> {
    let identity = probe.identity(abs).ok()?;
    if identity.is_cloud_placeholder() {
        return None;
    }
    let file = probe.open_read(abs).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let real = lil_platform::paths::strip_verbatim(&lil_platform::fs::final_path(&file).ok()?);
    lil_platform::paths::relative_inside(root, &real)?;
    Some(file)
}

fn hex(digest: &[u8]) -> String {
    use std::fmt::Write as _;
    digest.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// The blocking half of a render (module docs "One item").
fn work(
    abs: &Path,
    root: &Path,
    probe: &dyn Probe,
    local: &Path,
    renderer: &dyn Renderer,
    px: u32,
    slow: Option<Duration>,
) -> Work {
    let Some(file) = open_inside(abs, root, probe) else {
        return Work::Unreadable;
    };
    let mut hasher = Sha256::new();
    let mut reader = std::io::BufReader::new(file);
    // At most one byte more than the limit is kept in memory; the rest is only hashed.
    let mut bytes = Vec::new();
    let keep = if renderer.id() == ICON {
        0
    } else {
        MAX_FILE_BYTES + 1
    };
    if (&mut reader).take(keep).read_to_end(&mut bytes).is_err() {
        return Work::Unreadable;
    }
    hasher.update(&bytes);
    let mut buf = vec![0u8; 1 << 16];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Work::Unreadable,
        }
    }
    let sha = hex(&hasher.finalize());
    // The size guard comes first: the file as read, never what a header says.
    let over = u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_FILE_BYTES;
    if renderer.id() == ICON || over {
        return Work::Icon(sha);
    }
    if entry_path(local, &sha, renderer.id(), renderer.version(), px).is_file() {
        return Work::Cached(sha);
    }
    if let Some(slow) = slow {
        std::thread::sleep(slow);
    }
    match std::panic::catch_unwind(AssertUnwindSafe(|| renderer.render(&bytes, px))) {
        Ok(Ok(png)) => Work::Rendered(sha, png),
        Ok(Err(e)) => {
            tracing::debug!(file = %abs.display(), error = %e, "no thumbnail; the item gets the icon");
            Work::Icon(sha)
        }
        Err(_) => {
            tracing::warn!(file = %abs.display(), "a thumbnail renderer panicked; the item gets the icon");
            Work::Icon(sha)
        }
    }
}

impl Thumbnails for ThumbService {
    fn request<'a>(&'a self, items: &'a [ThumbItem], px: u32) -> BoxFuture<'a, Vec<ThumbState>> {
        Box::pin(async move {
            let paths: Vec<String> = items.iter().map(|i| i.path.clone()).collect();
            self.states(&paths, None, px, Some(self.inline_wait)).await
        })
    }

    fn ensure<'a>(
        &'a self,
        path: &'a WsPath,
        expected_sha256: Option<&'a str>,
        px: u32,
    ) -> BoxFuture<'a, ThumbState> {
        Box::pin(async move {
            let paths = [path.as_str().to_string()];
            self.states(&paths, Some(expected_sha256), px, None)
                .await
                .pop()
                .unwrap_or_else(|| self.icon(path.as_str(), None, px))
        })
    }
}
