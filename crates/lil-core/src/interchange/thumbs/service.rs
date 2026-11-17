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
//! whose cache entry is a plain file is `ready` at once, without reading the file (a hit; it
//! counts as a touch, below); an `icon@1` item with a known hash is `icon` at once. A cache
//! name that isn't a plain file (a link planted there) is a miss, never served. Anything else
//! is a **job**, keyed by the workspace's local folder, the path, the renderer and the size, so
//! every waiter's answer is built from its own item (its path and media kind) and only the
//! job's result (hash, state, file) is shared:
//!
//! 1. **Hash** (a path without a known hash). Only what can become a thumbnail is hashed: an
//!    `icon@1` kind, or an image over 256 MiB by its metadata, answers `icon` at once with
//!    `sha256: null` ("not read or not hashed"; nothing is drawn from it). An image is hashed
//!    on a blocking thread under a hasher slot (2 × the permits, held until the thread ends,
//!    not a decode permit) and the [`DECODE_DEADLINE`]: the file is opened (never a cloud
//!    placeholder, only when it really is inside the workspace) and streamed through SHA-256
//!    in fixed 64 KiB chunks. The hash is known to the waiters from then on, so a `pending`
//!    answer carries it and the app can match the `thumbs` event. A hash past its deadline
//!    answers `icon` with `sha256: null`. A hit under the hash read: `ready`.
//! 2. **Decode**: under a capacity slot (2 × the permits, held until the blocking thread really
//!    ends) and a decode permit of [`default_permits`] (held until the [`DECODE_DEADLINE`]),
//!    the file is opened again, its size taken from its metadata (over 256 MiB: hashed only,
//!    the `icon`), then read into a buffer reserved at exactly its size + 1 with `take(size +
//!    1)`, so a file that grows during the read is caught and nothing is ever doubled; the
//!    bytes read are hashed and rendered under `catch_unwind`. Its PNG is filed under the hash
//!    of the bytes read (`fsops::write_workspace_local_file`, which refuses to write through
//!    anything but a plain file). When that hash differs from the expected one (or from the
//!    one step 1 read), the answer is `pending`, the index is asked to re-check the file (the
//!    watcher's `lil notify` path), and the result is announced. Any failure, a passed
//!    deadline among them, gives the `icon@1` state, never an error; a late result is
//!    discarded. A decode past its deadline gives its permit back at once (P4b-R36) but keeps
//!    its capacity slot until its thread ends, so running plus abandoned decodes never pass
//!    twice the permits: new decodes wait meanwhile (their items answer `pending`).
//!
//! **Deadlines and stops** (audit fix rounds 1 and 2, M-1, M-4, N-1): an item waits for a slot
//! (a hasher, a capacity slot, a decode permit) as long as slots keep coming back: each slot
//! given back restarts its wait of one [`DECODE_DEADLINE`]. It answers `icon` for want of a slot
//! only when no slot came back during a whole deadline, or when every slot is held by a thread
//! already past its own deadline (stuck or abandoned); with its known hash or `null`, and
//! announced as any other.
//! A step past its deadline sets its job's stop flag, as the drain sets every job's
//! ([`ThumbService::stop_on`]): the hash and the read check it between 64 KiB chunks, and the
//! render before it starts, so an abandoned thread ends at its next check and gives its slot
//! back (a read blocked inside one call still ends only with that call; the core's `main`
//! leaves such threads behind after a short grace).
//!
//! **Waiting.** `app/thumbs.request` waits for its jobs up to [`INLINE_WAIT`] in all; an item
//! not done by then answers `pending` with its hash (waiting on for step 1 if needed), and its
//! result is published on topic `thumbs` as `{kind: "ready", sha256, renderer, px, file}` or
//! `{kind: "icon", sha256, media_kind}` once it comes. `ensure` waits for its job.
//!
//! **Touches.** A hit sets the entry's last write to the core's clock (at most once a day per
//! entry, `fsops::touch_workspace_local_file`), so "untouched for 7 days" counts use.
//!
//! **Clean-up** ([`ThumbService::clean_up`], [`ThumbService::spawn_clean_up`]): the first pass
//! runs once a default workspace exists (whenever it appears) and its index has finished its
//! first reconcile, then every 24 h on the core's clock, skipped while the index rebuilds; an
//! entry whose hash neither the index's files (unverified rows included) nor a [`LiveSource`]
//! (the inbox, Task 18) hold, and that was untouched for more than [`KEEP_UNTOUCHED`] on the
//! core's clock, is removed through `fsops::remove_workspace_local_file`.

use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, SystemTime};

use futures_util::future::BoxFuture;
use lil_platform::fs::Probe;
use lil_protocol::app::ThumbItem;
use lil_schema::display::{media_kind_of, renderable};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::cache::{Entry, KEEP_UNTOUCHED, entry_path, is_sha256, list_entries};
use super::renderers::{ICON, MAX_FILE_BYTES, Renderer, default_renderers};

/// The longest wait between two clean-up tries while the default workspace can't be opened.
pub const CLEAN_UP_BACKOFF_MAX: Duration = Duration::from_secs(60);

/// The wait after a clean-up try that couldn't open the default workspace, given the last one:
/// doubled, up to [`CLEAN_UP_BACKOFF_MAX`].
pub fn clean_up_backoff(last: Duration) -> Duration {
    (last * 2).min(CLEAN_UP_BACKOFF_MAX)
}

/// What one clean-up try did.
enum Pass {
    Ran(u64),
    /// No default workspace, or its index rebuilds, failed or can't be read.
    Skipped,
    /// The default workspace exists but can't be opened.
    Unopened,
}
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
/// How often clean-up runs while the core serves (after its first pass), on the core's clock.
pub const CLEAN_UP_PERIOD: Duration = Duration::from_secs(24 * 60 * 60);
/// How often the clean-up loop looks at the core's clock and the default workspace.
pub const CLEAN_UP_TICK: Duration = Duration::from_secs(1);
/// A hit refreshes its entry's last write when that is older than this.
pub const TOUCH_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// The chunk a file is hashed in.
const HASH_CHUNK: usize = 64 * 1024;
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
    /// Decodes in their deadline: `permit_count`.
    permits: Arc<Pool>,
    /// Decodes whose thread hasn't ended, abandoned ones included: 2 × `permit_count`.
    capacity: Arc<Pool>,
    /// Files being hashed before their decode: 2 × `permit_count`.
    hashers: Arc<Pool>,
    permit_count: usize,
    deadline: Duration,
    slow: Option<Duration>,
    inline_wait: Duration,
    /// The jobs running, by cache key.
    inflight: Mutex<HashMap<String, Arc<Inflight>>>,
    live_sources: Mutex<Vec<LiveSource>>,
    /// Set at the drain ([`ThumbService::stop_on`]): every read, hash and render stops at its
    /// next check.
    stopping: Arc<AtomicBool>,
}

impl std::fmt::Debug for ThumbService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThumbService")
            .field("permits", &self.permit_count)
            .field("deadline", &self.deadline)
            .finish_non_exhaustive()
    }
}

/// A running job's hash and result, shared by everyone waiting for it.
#[derive(Default)]
struct Inflight {
    slot: Mutex<Slot>,
    done: Notify,
}

#[derive(Default)]
struct Slot {
    /// The hash a `pending` answer carries: the expected one, or the one step 1 read.
    hash: Option<String>,
    outcome: Option<Outcome>,
    /// Someone answered `pending` for it: its result is published.
    announce: bool,
}

impl Inflight {
    fn set_hash(&self, sha: &str) {
        lock(&self.slot).hash = Some(sha.to_string());
        self.done.notify_waiters();
    }
}

/// A job's result. Each waiter's answer takes its own path, media kind and size from its own
/// item ([`Outcome::answer`]); only these are the job's.
#[derive(Debug, Clone)]
struct Outcome {
    sha256: Option<String>,
    /// `ready` or `icon`.
    state: &'static str,
    renderer: String,
    file: Option<PathBuf>,
    /// The bytes read had another hash than the expected one: the answer is `pending`, and
    /// the result is published anyway.
    mismatch: bool,
    /// For the `thumbs` event (the job's path is every waiter's path).
    media_kind: String,
    px: u32,
}

impl Outcome {
    /// What the item `mine` (its `pending` state) is answered.
    fn answer(&self, mine: &ThumbState) -> ThumbState {
        if self.mismatch {
            ThumbState {
                sha256: self.sha256.clone(),
                state: "pending".to_string(),
                file: None,
                ..mine.clone()
            }
        } else {
            ThumbState {
                sha256: self.sha256.clone(),
                state: self.state.to_string(),
                renderer: self.renderer.clone(),
                file: self.file.clone(),
                ..mine.clone()
            }
        }
    }
}

/// What [`wait_for`] saw.
enum Waited {
    Done(Outcome),
    /// Not done by the deadline: `pending`, with this hash.
    Pending(String),
}

/// What a blocking job found.
enum Work {
    /// The file can't be opened or read, or isn't inside the workspace.
    Unreadable,
    /// No image: the hash of the bytes read.
    Icon(String),
    /// The cache already has the entry of the hash read (a plain file).
    Cached(String, PathBuf),
    /// The rendered PNG of the bytes of this hash.
    Rendered(String, Vec<u8>),
    /// Over the size guard with a known hash: never read.
    Oversized,
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
            permits: Pool::new(permit_count),
            capacity: Pool::new(2 * permit_count),
            hashers: Pool::new(2 * permit_count),
            permit_count,
            deadline: options.deadline,
            slow: options.slow,
            inline_wait: options.inline_wait,
            inflight: Mutex::new(HashMap::new()),
            live_sources: Mutex::new(Vec::new()),
            stopping: Arc::new(AtomicBool::new(false)),
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

    /// The `icon@1` renderer's label.
    fn icon_label(&self) -> String {
        self.renderers
            .iter()
            .find(|r| r.id() == ICON)
            .map_or_else(|| "icon@1".to_string(), |r| label(r.as_ref()))
    }

    /// The cache entry of `sha256` drawn by `renderer` at `px` when it is a plain file (a link or
    /// anything else there is a miss), touched if it was last written more than
    /// [`TOUCH_EVERY`] ago on the core's clock. On a blocking thread.
    async fn hit(
        &self,
        local: &Path,
        sha256: &str,
        renderer: &dyn Renderer,
        px: u32,
    ) -> Option<PathBuf> {
        self.look(None, local, Some(sha256), renderer, px).await.1
    }

    /// On one blocking thread: the size of `abs` from its metadata (when given), and the hit of
    /// `sha256` ([`ThumbService::hit`], when given).
    async fn look(
        &self,
        abs: Option<PathBuf>,
        local: &Path,
        sha256: Option<&str>,
        renderer: &dyn Renderer,
        px: u32,
    ) -> (Option<u64>, Option<PathBuf>) {
        let entry = sha256.map(|sha| entry_path(local, sha, renderer.id(), renderer.version(), px));
        let (dir, now) = (local.to_path_buf(), self.hub.clock().utc_now());
        tokio::task::spawn_blocking(move || {
            let size = abs.and_then(|abs| std::fs::metadata(abs).ok()).map(|m| m.len());
            let hit = entry.and_then(|entry| {
                let modified = plain_file(&entry)?;
                if now
                    .duration_since(modified)
                    .is_ok_and(|age| age > TOUCH_EVERY)
                    && let Err(e) = fsops::touch_workspace_local_file(&dir, &entry, now)
                {
                    tracing::debug!(file = %entry.display(), error = %e, "a thumbnail cache hit could not be touched");
                }
                Some(entry)
            });
            (size, hit)
        })
        .await
        .unwrap_or_default()
    }

    /// The `icon@1` state of `path`, with `sha256` if known.
    fn icon(&self, path: &str, sha256: Option<String>, px: u32) -> ThumbState {
        ThumbState {
            path: path.to_string(),
            sha256,
            media_kind: media_kind_of(path).as_str().to_string(),
            renderer: self.icon_label(),
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
                Plan::Wait { inflight, pending } => match wait_for(&inflight, until).await {
                    Waited::Done(outcome) => outcome.answer(&pending),
                    Waited::Pending(sha) => ThumbState {
                        sha256: Some(sha),
                        ..pending
                    },
                },
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
        if renderer.id() == ICON {
            // Nothing is drawn from it: never read for a hash.
            return Plan::Done(self.icon(path, expected, px));
        }
        let (size, hit) = self
            .look(
                Some(ws_path.abs(&ctx.open.ws.path)),
                &ctx.local,
                expected.as_deref(),
                renderer.as_ref(),
                px,
            )
            .await;
        if size.is_some_and(|len| len > MAX_FILE_BYTES) {
            // Over the size guard, whatever its hash: it can't become a thumbnail, so it is
            // never read (M-2).
            return Plan::Done(self.icon(path, expected, px));
        }
        if let Some(sha) = &expected
            && let Some(entry) = hit
        {
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
        // One job per workspace, path, renderer and size (I1): never shared across paths.
        let key = format!(
            "{}|{}|{}@{}|{px}",
            path_key(&ctx.local.to_string_lossy()),
            path_key(ws_path.as_str()),
            renderer.id(),
            renderer.version()
        );
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
                    if let Some(sha) = &expected {
                        made.set_hash(sha);
                    }
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
                let outcome = me.run(job, &inflight).await;
                me.finish(&key, &inflight, outcome);
            });
        }
        Plan::Wait { inflight, pending }
    }

    /// A fresh stop check for one blocking step.
    fn stop(&self) -> Stop {
        Stop {
            job: Arc::new(AtomicBool::new(false)),
            all: self.stopping.clone(),
        }
    }

    /// Sets the stop flag every job checks once `token` is cancelled (the core's drain), so
    /// reads, hashes and renders still running stop at their next check.
    pub fn stop_on(&self, token: CancellationToken) {
        let stopping = self.stopping.clone();
        tokio::spawn(async move {
            token.cancelled().await;
            stopping.store(true, Ordering::Relaxed);
        });
    }

    /// Runs `job`: its hash first when it has none (step 1), then its decode under a capacity
    /// slot, a permit and the deadline (step 2), then files its PNG. Every slot is acquired
    /// under the deadline (M-1).
    async fn run(&self, job: Job, inflight: &Inflight) -> Outcome {
        let mut known = job.expected.clone();
        if known.is_none() {
            let Some(hasher) = self.hashers.acquire(self.deadline).await else {
                tracing::warn!(path = %job.path, deadline = ?self.deadline, "no hash slot came back within the deadline, or every one is stuck; the item gets the icon");
                return self.icon_outcome(&job, None);
            };
            let until = Instant::now() + self.deadline;
            let abandon = hasher.marker();
            let stop = self.stop();
            let (abs, root, probe, check) = (
                job.abs.clone(),
                job.root.clone(),
                job.probe.clone(),
                stop.clone(),
            );
            let blocking = tokio::task::spawn_blocking(move || {
                // The hasher slot goes back only when this thread ends, past its deadline too.
                let _hasher = hasher;
                hash_file(&abs, &root, probe.as_ref(), &check)
            });
            let hashed = match tokio::time::timeout_at(until, blocking).await {
                Ok(hashed) => hashed.ok().flatten(),
                Err(_) => {
                    stop.job.store(true, Ordering::Relaxed);
                    abandon.abandon();
                    tracing::warn!(path = %job.path, deadline = ?self.deadline, "a thumbnail's hash passed its deadline; the item gets the icon");
                    None
                }
            };
            let Some((sha, len)) = hashed else {
                return self.icon_outcome(&job, None);
            };
            inflight.set_hash(&sha);
            if len > MAX_FILE_BYTES {
                // It grew past the guard since the plan read its size.
                return self.icon_outcome(&job, Some(sha));
            }
            if let Some(entry) = self
                .hit(&job.local, &sha, job.renderer.as_ref(), job.px)
                .await
            {
                return self.ready_outcome(&job, sha, entry, false);
            }
            known = Some(sha);
        }
        // The slots: waited for while they keep coming back, never for stuck threads (N-1).
        let slots = async {
            let capacity = self.capacity.acquire(self.deadline).await?;
            let permit = self.permits.acquire(self.deadline).await?;
            Some((capacity, permit))
        };
        let Some((capacity, permit)) = slots.await else {
            tracing::warn!(path = %job.path, deadline = ?self.deadline, "no decode slot came back within the deadline, or every one is stuck; the item gets the icon");
            return self.icon_outcome(&job, known);
        };
        let abandon = capacity.marker();
        let stop = self.stop();
        let (slow, renderer, check) = (self.slow, job.renderer.clone(), stop.clone());
        let (abs, root, probe, local, px, expect) = (
            job.abs.clone(),
            job.root.clone(),
            job.probe.clone(),
            job.local.clone(),
            job.px,
            known.clone(),
        );
        let blocking = tokio::task::spawn_blocking(move || {
            // The capacity slot goes back only when this thread ends, past its deadline too.
            let _capacity = capacity;
            work(
                &Inputs {
                    abs: &abs,
                    root: &root,
                    probe: probe.as_ref(),
                    local: &local,
                    renderer: renderer.as_ref(),
                    px,
                    slow,
                    known: expect.as_deref(),
                },
                &check,
            )
        });
        let worked = tokio::time::timeout(self.deadline, blocking).await;
        // The permit goes back now, also when the blocking thread is still running (P4b-R36).
        drop(permit);
        let work = match worked {
            Ok(Ok(work)) => work,
            Ok(Err(e)) => {
                tracing::warn!(path = %job.path, error = %e, "a thumbnail render stopped; the item gets the icon");
                return self.icon_outcome(&job, known);
            }
            Err(_) => {
                stop.job.store(true, Ordering::Relaxed);
                abandon.abandon();
                tracing::warn!(path = %job.path, deadline = ?self.deadline, "a thumbnail render passed its deadline; the item gets the icon and its late result is discarded");
                return self.icon_outcome(&job, known);
            }
        };
        let (sha, file) = match work {
            // Announced under the hash its waiters were given (M-3).
            Work::Unreadable => return self.icon_outcome(&job, known),
            Work::Oversized => return self.icon_outcome(&job, known),
            Work::Icon(sha) => (sha, None),
            Work::Cached(sha, entry) => (sha, Some(entry)),
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
        let mismatch = known.as_ref().is_some_and(|k| *k != sha);
        if mismatch {
            tracing::info!(path = %job.path, expected = ?known, read = %sha, "a file's bytes differ from the hash expected; the index re-checks it");
            recheck(job.open.clone(), job.ws_path.clone());
        }
        match file {
            Some(entry) => self.ready_outcome(&job, sha, entry, mismatch),
            None => Outcome {
                mismatch,
                ..self.icon_outcome(&job, Some(sha))
            },
        }
    }

    fn icon_outcome(&self, job: &Job, sha256: Option<String>) -> Outcome {
        Outcome {
            sha256,
            state: "icon",
            renderer: self.icon_label(),
            file: None,
            mismatch: false,
            media_kind: media_kind_of(&job.path).as_str().to_string(),
            px: job.px,
        }
    }

    fn ready_outcome(&self, job: &Job, sha256: String, file: PathBuf, mismatch: bool) -> Outcome {
        Outcome {
            sha256: Some(sha256),
            state: "ready",
            renderer: label(job.renderer.as_ref()),
            file: Some(file),
            mismatch,
            media_kind: media_kind_of(&job.path).as_str().to_string(),
            px: job.px,
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
            let o = &outcome;
            let data = match &o.file {
                Some(file) if o.state == "ready" => json!({
                    "kind": "ready",
                    "sha256": o.sha256,
                    "renderer": o.renderer,
                    "px": o.px,
                    "file": file.to_string_lossy(),
                }),
                _ => json!({
                    "kind": "icon",
                    "sha256": o.sha256,
                    "media_kind": o.media_kind,
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
        match self.pass().await {
            Pass::Ran(removed) => Some(removed),
            Pass::Skipped | Pass::Unopened => None,
        }
    }

    async fn pass(&self) -> Pass {
        if self.hub.default_ref().is_none() {
            return Pass::Skipped;
        }
        let Some(ctx) = self.ctx().await else {
            return Pass::Unopened;
        };
        // Only the index is kept while the first reconcile runs: the workspace itself is let go
        // of, so a switch can close it meanwhile (L-4).
        let (index, ws_id) = (ctx.open.index.clone(), ctx.open.ws.id.clone());
        drop(ctx);
        index.ready().await;
        if index.failed() || index.rebuilding() {
            tracing::info!("the thumbnail clean-up skips this pass: the index rebuilds or failed");
            return Pass::Skipped;
        }
        let mut live = match index.file_hashes().await {
            Ok(live) => live,
            Err(e) => {
                tracing::warn!(error = %e, "the thumbnail clean-up skips this pass: the index's hashes can't be read");
                return Pass::Skipped;
            }
        };
        drop(index);
        // The workspace again, for the live sources: still the default one, or no pass.
        let Some(ctx) = self.ctx().await else {
            return Pass::Unopened;
        };
        if ctx.open.ws.id != ws_id {
            return Pass::Skipped;
        }
        let sources = lock(&self.live_sources).clone();
        for source in sources {
            live.extend(source(ctx.open.clone()).await);
        }
        let local = ctx.local.clone();
        drop(ctx);
        let entries = entries_of(&local).await;
        let doomed = self.doomed(entries, &|sha| live.contains(sha));
        Pass::Ran(remove_entries(&local, doomed).await)
    }

    /// Runs [`ThumbService::clean_up_pass`] in the background until `stop`: every
    /// [`CLEAN_UP_TICK`] it looks at the core's clock, and a pass is due until the first one has
    /// run (so it runs once a default workspace exists and its index has finished its first
    /// reconcile, even one that appears after the start), then [`CLEAN_UP_PERIOD`] after the
    /// last on the core's clock (or at once if the clock went back before it). While the
    /// default workspace exists but can't be opened, the tries back off ([`clean_up_backoff`],
    /// up to a minute); any other outcome goes back to the tick.
    pub fn spawn_clean_up(self: &Arc<Self>, stop: CancellationToken) {
        let me = Arc::downgrade(self);
        let clock = self.hub.clock().clone();
        tokio::spawn(async move {
            let mut last: Option<SystemTime> = None;
            let mut wait = CLEAN_UP_TICK;
            loop {
                let now = clock.utc_now();
                let due = last.is_none_or(|t| {
                    now.duration_since(t)
                        .map_or(true, |since| since >= CLEAN_UP_PERIOD)
                });
                if due {
                    let Some(service) = me.upgrade() else {
                        break;
                    };
                    tokio::select! {
                        () = stop.cancelled() => break,
                        done = service.pass() => match done {
                            Pass::Ran(_) => {
                                last = Some(now);
                                wait = CLEAN_UP_TICK;
                            }
                            Pass::Skipped => wait = CLEAN_UP_TICK,
                            Pass::Unopened => wait = clean_up_backoff(wait),
                        }
                    }
                }
                tokio::select! {
                    () = stop.cancelled() => break,
                    () = tokio::time::sleep(wait) => {}
                }
            }
        });
    }
}

/// Waits for `inflight`'s outcome until `until` (`None`: until it comes). Past `until`, the
/// caller answers `pending` with the job's hash, waiting on for the hash if step 1 hasn't read
/// it yet (bounded by one file's read); the outcome is then marked to be published. Both are
/// decided under the slot's lock, so no outcome is missed between them.
async fn wait_for(inflight: &Inflight, until: Option<Instant>) -> Waited {
    loop {
        let notified = inflight.done.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        {
            let mut slot = lock(&inflight.slot);
            if let Some(outcome) = &slot.outcome {
                return Waited::Done(outcome.clone());
            }
            if until.is_some_and(|u| Instant::now() >= u)
                && let Some(sha) = slot.hash.clone()
            {
                slot.announce = true;
                return Waited::Pending(sha);
            }
        }
        match until {
            Some(u) if Instant::now() < u => {
                let _ = tokio::time::timeout_at(u, notified).await;
            }
            _ => notified.await,
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

/// Slots whose holders may outlive their deadline (a hash's or a decode's thread): the slots,
/// how many are held by threads already past their deadline, and a signal each time a slot
/// comes back (or one more is abandoned).
struct Pool {
    slots: Arc<Semaphore>,
    total: usize,
    abandoned: Mutex<usize>,
    changed: Notify,
}

/// One slot of a [`Pool`]. Dropping it gives the slot back and signals the pool; a holder past
/// its deadline is marked with [`Held::abandon`] until it ends.
struct Held {
    permit: Option<OwnedSemaphorePermit>,
    pool: Arc<Pool>,
    mark: Arc<Mutex<Mark>>,
}

#[derive(Default)]
struct Mark {
    ended: bool,
    abandoned: bool,
}

impl Pool {
    fn new(total: usize) -> Arc<Pool> {
        Arc::new(Pool {
            slots: Arc::new(Semaphore::new(total)),
            total,
            abandoned: Mutex::new(0),
            changed: Notify::new(),
        })
    }

    fn held(self: &Arc<Self>, permit: OwnedSemaphorePermit) -> Held {
        Held {
            permit: Some(permit),
            pool: self.clone(),
            mark: Arc::default(),
        }
    }

    /// A slot, waiting while slots keep coming back: each one given back (to anyone) restarts
    /// the wait of `window`. `None` when none came back during a whole `window`, or when every
    /// slot is held by a thread already past its deadline (N-1).
    async fn acquire(self: &Arc<Self>, window: Duration) -> Option<Held> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Ok(permit) = self.slots.clone().try_acquire_owned() {
                return Some(self.held(permit));
            }
            if *lock(&self.abandoned) >= self.total {
                return None;
            }
            tokio::select! {
                permit = self.slots.clone().acquire_owned() => {
                    return permit.ok().map(|p| self.held(p));
                }
                () = &mut changed => {}
                () = tokio::time::sleep(window) => return None,
            }
        }
    }
}

impl Held {
    /// The handle that marks this slot's holder as past its deadline.
    fn marker(&self) -> Abandon {
        Abandon {
            pool: self.pool.clone(),
            mark: self.mark.clone(),
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        {
            let mut mark = lock(&self.mark);
            mark.ended = true;
            if mark.abandoned {
                *lock(&self.pool.abandoned) -= 1;
            }
        }
        drop(self.permit.take());
        self.pool.changed.notify_waiters();
    }
}

/// Marks a slot's holder as past its deadline (while it hasn't ended).
struct Abandon {
    pool: Arc<Pool>,
    mark: Arc<Mutex<Mark>>,
}

impl Abandon {
    fn abandon(&self) {
        let mut mark = lock(&self.mark);
        if !mark.ended && !mark.abandoned {
            mark.abandoned = true;
            *lock(&self.pool.abandoned) += 1;
        }
        drop(mark);
        self.pool.changed.notify_waiters();
    }
}

/// When a blocking step stops early: its job passed its deadline, or the core drains.
#[derive(Clone)]
struct Stop {
    job: Arc<AtomicBool>,
    all: Arc<AtomicBool>,
}

impl Stop {
    fn stopped(&self) -> bool {
        self.job.load(Ordering::Relaxed) || self.all.load(Ordering::Relaxed)
    }
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

/// The modified time of `path` when it is a plain file (not a link, a folder or anything else).
fn plain_file(path: &Path) -> Option<SystemTime> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return None;
    }
    meta.modified().ok()
}

/// Feeds the rest of `reader` to `hasher` in [`HASH_CHUNK`]s; `None` on a read error or once
/// `stop` says so (checked between chunks).
fn hash_rest(reader: &mut impl std::io::Read, mut hasher: Sha256, stop: &Stop) -> Option<String> {
    let mut buf = vec![0u8; HASH_CHUNK];
    loop {
        if stop.stopped() {
            return None;
        }
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    Some(hex(&hasher.finalize()))
}

/// Step 1: the SHA-256 and size of `abs`, streamed in fixed chunks (nothing kept); `None` when
/// it can't be opened or read, isn't inside the workspace, or `stop` says so.
fn hash_file(abs: &Path, root: &Path, probe: &dyn Probe, stop: &Stop) -> Option<(String, u64)> {
    let mut file = open_inside(abs, root, probe)?;
    let len = file.metadata().ok()?.len();
    Some((hash_rest(&mut file, Sha256::new(), stop)?, len))
}

/// What step 2 reads and renders.
struct Inputs<'a> {
    abs: &'a Path,
    root: &'a Path,
    probe: &'a dyn Probe,
    local: &'a Path,
    renderer: &'a dyn Renderer,
    px: u32,
    slow: Option<Duration>,
    /// The hash the item was given (the expected one, or step 1's).
    known: Option<&'a str>,
}

/// Step 2, the blocking half of a render (module docs "One item"): the size from the metadata
/// first, then the bytes, in [`HASH_CHUNK`]s with `stop` checked between them, into a buffer of
/// exactly that size + 1, their hash, a hit check, then the render.
fn work(i: &Inputs<'_>, stop: &Stop) -> Work {
    let Some(mut file) = open_inside(i.abs, i.root, i.probe) else {
        return Work::Unreadable;
    };
    let Ok(len) = file.metadata().map(|m| m.len()) else {
        return Work::Unreadable;
    };
    // The size guard, before any buffer: over the limit with a known hash, never read (M-2);
    // without one, only hashed.
    if len > MAX_FILE_BYTES {
        if i.known.is_some() {
            return Work::Oversized;
        }
        return hash_rest(&mut file, Sha256::new(), stop).map_or(Work::Unreadable, Work::Icon);
    }
    // At most 256 MiB + 1, so the conversion can't fail on a 64-bit target.
    let Ok(want) = usize::try_from(len + 1) else {
        return Work::Unreadable;
    };
    let mut bytes = Vec::new();
    if bytes.try_reserve_exact(want).is_err() {
        return Work::Unreadable;
    }
    // One byte more than the size: a file that grew during the read is caught, and the buffer
    // never grows past what was reserved.
    bytes.resize(want, 0);
    let mut filled = 0;
    while filled < want {
        if stop.stopped() {
            return Work::Unreadable;
        }
        let end = (filled + HASH_CHUNK).min(want);
        match file.read(&mut bytes[filled..end]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Work::Unreadable,
        }
    }
    bytes.truncate(filled);
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    if bytes.len() > want - 1 {
        // It grew while read: only hashed, the icon.
        return hash_rest(&mut file, hasher, stop).map_or(Work::Unreadable, Work::Icon);
    }
    drop(file);
    let sha = hex(&hasher.finalize());
    let renderer = i.renderer;
    let entry = entry_path(i.local, &sha, renderer.id(), renderer.version(), i.px);
    if plain_file(&entry).is_some() {
        return Work::Cached(sha, entry);
    }
    if let Some(slow) = i.slow {
        // The test override's wait, in short steps so a stop ends it.
        let until = std::time::Instant::now() + slow;
        while std::time::Instant::now() < until {
            if stop.stopped() {
                return Work::Unreadable;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    if stop.stopped() {
        return Work::Unreadable;
    }
    match std::panic::catch_unwind(AssertUnwindSafe(|| renderer.render(&bytes, i.px))) {
        Ok(Ok(png)) => Work::Rendered(sha, png),
        Ok(Err(e)) => {
            tracing::debug!(file = %i.abs.display(), error = %e, "no thumbnail; the item gets the icon");
            Work::Icon(sha)
        }
        Err(_) => {
            tracing::warn!(file = %i.abs.display(), "a thumbnail renderer panicked; the item gets the icon");
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Fix round 2: while the default workspace can't be opened, clean-up's tries back off,
    /// doubling from the tick up to a minute.
    #[test] // spec: APP-038
    fn clean_up_backoff_doubles_up_to_a_minute() {
        let mut wait = CLEAN_UP_TICK;
        let mut seen = Vec::new();
        for _ in 0..8 {
            wait = clean_up_backoff(wait);
            seen.push(wait.as_secs());
        }
        assert_eq!(seen, [2, 4, 8, 16, 32, 60, 60, 60]);
    }
}
