//! The notifier (ARCH-005, ARCH-055, CORE-113, APP-022, APP-146; Appendix G "Notifications";
//! P4a-R7, P4a-R8, P4a-R9, P4a-R36, P4a-R41): the core's Windows toasts and the notification
//! path that lets a confirmation wait with no app open.
//!
//! - **What it posts.**
//!   - `job_finished` or `job_failed` once per batch, when its last job ends ([`batch_end`]),
//!     from the `jobs` topic (its own bus client).
//!   - `confirmation_waiting` for each pending confirmation, removed by its Tag whenever it
//!     ends, whichever core run posted it. [`Confirm`] tells the notifier straight away
//!     ([`NotificationPath::pending`], [`NotificationPath::ended`]) through an unbounded
//!     channel, so no burst of other events can drop one.
//!   - `cap_alert` for each `caps` alert (a bus client of its own).
//!   - `notify.claude_code` ([`Notifier::claude_code`]), an agent's `open_in` and `ui.show`
//!     ([`Notifier::agent_opened`]) and each recovery that held jobs not counted in a
//!     restart-hold toast yet ([`Notifier::restart_held`]; once per held batch, kept in the suite
//!     `state.db`, owner decision 2026-10-26).
//!   - No toast is ever a progress toast or updated in place (P4a-R36).
//! - **As posted** (Checkpoint A walkthrough): every toast's title and body are redacted as a
//!   diagnostics bundle's entries are (BUG-1), and while the handlers aren't this channel's, a
//!   toast with a target is posted without it, its body ending [`OPEN_LILITH`] (ODD-6). A
//!   confirmation's toast posted so holds no live token, and the next reconcile that finds the
//!   handlers `ok` posts it again with its buttons and a fresh token.
//! - **When.** A toast is posted only when its category is on, the channel's toasts can post
//!   (`ready` or `not_yet_shown`) with the shortcut `ok`, and no connected `ui:*` app owns the
//!   foreground window (P4a-R7); otherwise nothing is posted or queued. While the fullscreen
//!   poll says fullscreen and `quiet_while_fullscreen` is on, every toast is quiet
//!   (`SuppressPopup`), confirmations included; none is dropped for it. A post that fails
//!   recomputes the notification path, so Reachable sees it at once.
//! - **Batch ends, in O(n).** A final job event marks its batch; the batch is read (narrowly,
//!   [`Jobs::batch_jobs`]) only once none of its jobs is queued, running or held any more
//!   ([`Jobs::batch_live`]), and again on the jobs' change watch until its newest attempts are
//!   all final. When the bus dropped `jobs` events (`resync_required`), every batch
//!   with a job finished since a little before the newest event handled (by the event's own
//!   time, never before this run subscribed) is checked again; each batch end is toasted once
//!   (the newest attempts' ids). Whether a suite window was in front is judged as each job
//!   ends, through the jobs' end hook ([`Notifier::job_ending`]), before its final row.
//! - **Reconcile** (plan 4a clarification, 2026-10-25): at start, after each confirmation load
//!   and when the last app that can answer confirmations closes, each pending confirmation
//!   without a toast from this run gets one, with a fresh token (the same Tag replaces an
//!   earlier run's toast and its dead Deny), and a confirmation toast in the notification
//!   centre whose confirmation isn't pending is removed.
//! - **Tags.** A confirmation's toast is tagged with its id, so it is never replaced by
//!   another's; a batch from a panel with its batch id; any other batch's with `o_<16 hex>` of
//!   its origin, so a newer one from the same origin replaces the older one (a new post, never
//!   an update); the restart hold's `restart_<first held job>`; a cap alert's `cap_<16 hex>`;
//!   each agent-activity toast a new `n_<16 hex>`. The Group is the category.
//! - **Toast tokens** (P4a-R9). Each confirmation toast gets a fresh 128-bit token from the
//!   injected id source (the OS random source in production), bound to the confirmation in
//!   [`Confirm`] before the toast is posted; only its Deny URL holds it, and the log line of
//!   the post cuts it (`deny/<id>/…`).
//! - **The notification path** ([`NotificationPath::reachable`]): `ready` (or `not_yet_shown`),
//!   the shortcut and handlers `ok`, and `confirmation_waiting` on, from the desktop settings'
//!   one 10 s cache (P4a-R33), which `desktop.get {fresh: true}` recomputes; a cached
//!   `not_yet_shown` is read again on each call, so its end as `no_platform` answers at once
//!   (plan 4a clarification, 2026-10-25).

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, UNIX_EPOCH};

use lil_platform::desktop::{Desktop, Toast};
use lil_platform::{Channel, Clock, Env, IdSource};
use lil_protocol::app::{
    ConfirmationView, DesktopSettings, HandlersWire, NotificationPath as PathWire, ShortcutWire,
    ToastsWire,
};
use lil_protocol::{CallerKind, Cover, EventKind, EventParams};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use super::toast_xml::{SHOWN_CHARS, ToastActions, ToastText, cut_token, shown_plain, toast_xml};
use crate::confirm::{Confirm, ListState, NotificationPath};
use crate::desktop::{DesktopSettingsStore, DesktopWatch, UiApps};
use crate::events::{ClientId, EventBus};
use crate::jobs::{BatchJob, JobId, JobStatus, Jobs, Priority, Surface};
use crate::state::{StateDb, StateError};

/// The notifier's bus client for the `jobs` topic: no connection has it.
pub const JOBS_CLIENT: ClientId = u64::MAX;
/// The notifier's bus client for the `caps` topic, apart from the jobs' volume.
pub const CAPS_CLIENT: ClientId = u64::MAX - 1;
/// The same Claude Code message within this long is a duplicate (Appendix G).
pub const DUPLICATE_WITHIN: Duration = Duration::from_secs(10);
/// How many ended batches the notifier remembers, so each ends in one toast.
const BATCHES_KEPT: usize = 512;
/// How many event lines may wait for the notifier, per client.
const EVENT_QUEUE: usize = 1024;
/// After a `jobs` resync, batches with a job finished this long before the last event handled
/// are checked again too.
const RESYNC_LOOKBACK_SECS: i64 = 5;
/// The most pending confirmations a reconcile reads.
const RECONCILE_LIMIT: u32 = 1000;
/// The error code of a job a core's start or stop interrupted.
const CORE_STOPPED: &str = "core.draining";
/// How many jobs that ended with a suite window in front the notifier remembers.
const ENDED_IN_FRONT_KEPT: usize = 4096;
/// The longest `lil:` URL argument (Appendix G "`lil:` URLs"): a longer `open` launch is left out.
const MAX_URL_CHARS: usize = 8192;
/// How a toast's body ends when its target was left out because the handlers aren't this
/// channel's (Checkpoint A walkthrough, ODD-6).
pub const OPEN_LILITH: &str = " · Open LILITH to see it";
/// How many times a toast's text is normalised and redacted at most before it settles.
const SETTLE_ROUNDS: usize = 4;
/// How many job ids counted in restart-hold toasts the suite `state.db` keeps.
const RESTART_TOASTS_KEPT: i64 = 4096;

/// The categories (Appendix G).
pub const JOB_FINISHED: &str = "job_finished";
pub const JOB_FAILED: &str = "job_failed";
pub const CONFIRMATION_WAITING: &str = "confirmation_waiting";
pub const CAP_ALERT: &str = "cap_alert";
pub const AGENT_ACTIVITY: &str = "agent_activity";

/// `url` as a toast's launch, unless it passes the longest `lil:` argument. A valid target
/// (4,096 bytes at most) never makes one that long; this is the bound should that change.
fn within_url_limit(url: String) -> Option<String> {
    (url.chars().count() <= MAX_URL_CHARS).then_some(url)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What became of a toast (`notify.claude_code`'s `{posted, reason?}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyOutcome {
    Posted,
    CategoryOff,
    Foreground,
    Duplicate,
    Unavailable,
}

impl NotifyOutcome {
    /// `notify.claude_code`'s `reason`: `None` when it was posted.
    pub fn reason(self) -> Option<&'static str> {
        match self {
            NotifyOutcome::Posted => None,
            NotifyOutcome::CategoryOff => Some("category_off"),
            NotifyOutcome::Foreground => Some("foreground"),
            NotifyOutcome::Duplicate => Some("duplicate"),
            NotifyOutcome::Unavailable => Some("unavailable"),
        }
    }
}

/// What the notifier needs from the core.
pub struct NotifierParts {
    pub desktop: Arc<dyn Desktop>,
    /// The fullscreen poll (quiet mode) and the foreground check.
    pub watch: Arc<DesktopWatch>,
    /// The categories, quiet mode and the notification path's one cache.
    pub settings: Arc<DesktopSettingsStore>,
    pub events: Arc<EventBus>,
    /// The connected apps' processes (P4a-R7).
    pub apps: Arc<UiApps>,
    pub env: Env,
    pub clock: Arc<dyn Clock>,
    /// The toast tokens and agent-activity tags come from here.
    pub ids: Arc<dyn IdSource>,
}

/// What [`Confirm`] tells the notifier, in order.
enum ConfirmNews {
    Pending(Box<ConfirmationView>),
    Ended(String),
    Reconcile,
}

/// What the notifier remembers.
#[derive(Default)]
struct Memory {
    /// Claude Code messages posted (or being posted), with when (monotonic).
    recent: VecDeque<(Duration, String)>,
    /// The confirmations whose toast this run posted.
    confirmations: HashSet<String>,
    /// The confirmations whose toast this run posted without its target (the handlers weren't
    /// this channel's): a reconcile posts each again once they are.
    bare: HashSet<String>,
    /// The ended batches toasted: `(batch id, the newest attempts' ids)`, newest last.
    batches: VecDeque<(String, Vec<String>)>,
    /// The batches settled (toasted or not): `(batch id, every job id then)`, newest last. A
    /// final event of one of those jobs doesn't mark the batch again.
    settled: VecDeque<(String, HashSet<String>)>,
    /// The jobs that ended while a suite window was in front (P4a-R7), newest last.
    ended_in_front: VecDeque<String>,
}

/// The notifier (module docs).
pub struct Notifier {
    desktop: Arc<dyn Desktop>,
    watch: Arc<DesktopWatch>,
    settings: Arc<DesktopSettingsStore>,
    events: Arc<EventBus>,
    current: watch::Receiver<DesktopSettings>,
    fullscreen: watch::Receiver<bool>,
    apps: Arc<UiApps>,
    scheme: String,
    /// The redaction's names, profile folder and computer names come from here.
    env: Env,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdSource>,
    confirm: OnceLock<Weak<Confirm>>,
    /// The suite `state.db`: the job ids counted in restart-hold toasts.
    state: OnceLock<Arc<StateDb>>,
    jobs: OnceLock<Weak<Jobs>>,
    news: mpsc::UnboundedSender<ConfirmNews>,
    /// Holds the handling of `jobs` events while `true` (the overflow test's lever, on a test
    /// channel only).
    jobs_paused: watch::Sender<bool>,
    /// The core's channel: [`Notifier::pause_job_events`] works only on a test one.
    channel: Channel,
    /// A reconcile is queued and not yet begun: another one is not queued.
    reconcile_queued: AtomicBool,
    memory: Mutex<Memory>,
    /// Ends the notifier's tasks when it is dropped.
    stop: CancellationToken,
}

impl std::fmt::Debug for Notifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Notifier")
            .field("scheme", &self.scheme)
            .finish_non_exhaustive()
    }
}

impl Drop for Notifier {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// The parameters of one bus line, if it is a `lil/event`.
fn event_of(line: &str) -> Option<EventParams> {
    let note = serde_json::from_str::<Value>(line).ok()?;
    serde_json::from_value(note.get("params")?.clone()).ok()
}

impl Notifier {
    /// Starts the notifier on the current runtime: its confirmation news and its `caps` bus
    /// client are in place when this returns; the `jobs` client starts with
    /// [`Notifier::set_jobs`]. Its tasks end when the notifier is dropped.
    pub async fn start(parts: NotifierParts) -> Arc<Notifier> {
        let NotifierParts {
            desktop,
            watch,
            settings,
            events,
            apps,
            env,
            clock,
            ids,
        } = parts;
        let (news, mut news_rx) = mpsc::unbounded_channel();
        let me = Arc::new(Notifier {
            current: settings.current(),
            fullscreen: watch.fullscreen(),
            desktop,
            watch,
            settings,
            events: events.clone(),
            apps,
            scheme: env.activation_scheme(),
            channel: env.channel().clone(),
            env,
            clock,
            ids,
            confirm: OnceLock::new(),
            state: OnceLock::new(),
            jobs: OnceLock::new(),
            news,
            jobs_paused: watch::Sender::new(false),
            reconcile_queued: AtomicBool::new(false),
            memory: Mutex::new(Memory::default()),
            stop: CancellationToken::new(),
        });

        // The confirmations, in order, never dropped.
        let (weak, stop) = (Arc::downgrade(&me), me.stop.clone());
        tokio::spawn(async move {
            loop {
                let news = tokio::select! {
                    () = stop.cancelled() => return,
                    news = news_rx.recv() => match news {
                        Some(news) => news,
                        None => return,
                    },
                };
                let Some(me) = weak.upgrade() else {
                    return;
                };
                match news {
                    ConfirmNews::Pending(view) => me.confirmation_pending(&view, false).await,
                    ConfirmNews::Ended(id) => me.confirmation_ended(&id).await,
                    ConfirmNews::Reconcile => {
                        me.reconcile_queued.store(false, Ordering::SeqCst);
                        me.reconcile_confirmations().await;
                    }
                }
            }
        });

        // The caps alerts.
        let (tx, mut rx) = mpsc::channel(EVENT_QUEUE);
        events.attach(CAPS_CLIENT, tx);
        events
            .subscribe(CAPS_CLIENT, &["caps".to_string()], &BTreeMap::new())
            .await;
        let (weak, stop) = (Arc::downgrade(&me), me.stop.clone());
        tokio::spawn(async move {
            loop {
                let line = tokio::select! {
                    () = stop.cancelled() => break,
                    line = rx.recv() => match line {
                        Some(line) => line,
                        None => break,
                    },
                };
                let Some(me) = weak.upgrade() else {
                    break;
                };
                let Some(p) = event_of(&line) else { continue };
                if p.kind == EventKind::ResyncRequired {
                    tracing::warn!(
                        "the notifier missed caps events; a cap alert's toast may be lost"
                    );
                    continue;
                }
                if p.kind == EventKind::Event
                    && p.data.get("kind").and_then(Value::as_str) == Some("alert")
                {
                    let text = |k: &str| p.data.get(k).and_then(Value::as_str).unwrap_or_default();
                    let pct = p
                        .data
                        .get("threshold_pct")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let spent = p
                        .data
                        .get("spent_pct")
                        .and_then(Value::as_u64)
                        .unwrap_or(pct);
                    me.cap_alert(text("cap"), pct, spent, text("period")).await;
                }
            }
            events.drop_client(CAPS_CLIENT);
        });
        me
    }

    /// The confirmations whose toasts it posts and whose tokens it binds.
    pub fn set_confirm(&self, confirm: &Arc<Confirm>) {
        let _ = self.confirm.set(Arc::downgrade(confirm));
    }

    /// The suite `state.db`, which keeps the job ids counted in restart-hold toasts (once; set
    /// it before any recovery).
    pub fn set_state(&self, state: Arc<StateDb>) {
        let _ = self.state.set(state);
    }

    /// The jobs whose batches it reads: subscribes its `jobs` bus client and starts handling
    /// their events (once; a second call is ignored).
    pub async fn set_jobs(self: &Arc<Self>, jobs: &Arc<Jobs>) {
        if self.jobs.set(Arc::downgrade(jobs)).is_err() {
            return;
        }
        let (tx, mut rx) = mpsc::channel(EVENT_QUEUE);
        self.events.attach(JOBS_CLIENT, tx);
        self.events
            .subscribe(JOBS_CLIENT, &["jobs".to_string()], &BTreeMap::new())
            .await;
        let mut changes = jobs.changes();
        let mut paused = self.jobs_paused.subscribe();
        // A resync never looks back before this run's subscription.
        let subscribed = self.utc_secs();
        let (weak, stop, events) = (Arc::downgrade(self), self.stop.clone(), self.events.clone());
        tokio::spawn(async move {
            let mut marked: BTreeSet<String> = BTreeSet::new();
            let mut last_utc = subscribed;
            loop {
                tokio::select! {
                    () = stop.cancelled() => break,
                    held = paused.wait_for(|p| !*p) => if held.is_err() { break },
                }
                tokio::select! {
                    () = stop.cancelled() => break,
                    line = rx.recv() => match line {
                        Some(line) => {
                            let Some(me) = weak.upgrade() else { break };
                            me.on_jobs_line(&line, &mut marked, &mut last_utc, subscribed)
                                .await;
                        }
                        None => break,
                    },
                    changed = changes.changed() => if changed.is_err() { break },
                }
                let Some(me) = weak.upgrade() else { break };
                me.settle_batches(&mut marked).await;
            }
            events.drop_client(JOBS_CLIENT);
        });
    }

    /// Holds (`true`) or lets go the handling of `jobs` events: `true` when done. Only the bus
    /// overflow test uses it (events queue up meanwhile, as when the notifier falls behind),
    /// so it works only on a test channel, as the `test.*` tools exist only there; elsewhere
    /// it is refused (`false`) and changes nothing (Checkpoint A ruling).
    #[doc(hidden)]
    #[must_use]
    pub fn pause_job_events(&self, on: bool) -> bool {
        hold_job_events(&self.channel, &self.jobs_paused, on)
    }

    fn utc_ms(&self) -> i64 {
        self.clock
            .utc_now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
    }

    fn utc_secs(&self) -> i64 {
        self.clock
            .utc_now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
    }

    /// One `jobs` line: a batch to look at, or after a resync every batch with a job finished
    /// since a little before the newest event handled (by the event's own time, never before
    /// `subscribed`, this run's subscription).
    async fn on_jobs_line(
        &self,
        line: &str,
        marked: &mut BTreeSet<String>,
        last_utc: &mut i64,
        subscribed: i64,
    ) {
        let Some(p) = event_of(line) else {
            return;
        };
        match p.kind {
            EventKind::ResyncRequired => {
                let since = (*last_utc - RESYNC_LOOKBACK_SECS).max(subscribed);
                if let Some(jobs) = self.jobs.get().and_then(Weak::upgrade) {
                    let found = jobs.batches_finished_since(since).await;
                    tracing::info!(
                        batches = found.len(),
                        "the notifier missed jobs events; it checks the batches again"
                    );
                    marked.extend(found);
                }
            }
            EventKind::Event if p.data.get("kind").and_then(Value::as_str) == Some("job") => {
                let job = &p.data["job"];
                let at = ["finished", "updated"]
                    .iter()
                    .filter_map(|k| job[*k].as_str())
                    .filter_map(crate::telemetry::parse_ts)
                    .max()
                    .map(|ms| ms.div_euclid(1000));
                if let Some(at) = at {
                    *last_utc = (*last_utc).max(at);
                }
                let status = job["status"].as_str().and_then(JobStatus::parse);
                let stopped = job["substatus"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("interrupted"))
                    && job["error"]["code"].as_str() == Some(CORE_STOPPED);
                if let (Some(status), Some(batch), Some(id)) =
                    (status, job["batch_id"].as_str(), job["job_id"].as_str())
                    && status.is_final()
                    && !stopped
                    && !self.settled(batch, id)
                {
                    marked.insert(batch.to_string());
                }
            }
            _ => {}
        }
    }

    /// Each marked batch none of whose jobs is queued, running or held any more: its end, read
    /// once its newest attempts are all final (until then it stays marked, and the jobs' change
    /// watch brings it back).
    async fn settle_batches(&self, marked: &mut BTreeSet<String>) {
        let Some(jobs) = self.jobs.get().and_then(Weak::upgrade) else {
            return;
        };
        let candidates: Vec<String> = marked
            .iter()
            .filter(|b| !jobs.batch_live(b))
            .cloned()
            .collect();
        for batch in candidates {
            // Settled, or not final with none of its jobs in the queue any more (one waits
            // for you, `interrupted_unknown`): its resolve marks it again.
            if self.batch_ended(&jobs, &batch).await || !jobs.batch_in_queue(&batch) {
                marked.remove(&batch);
            }
        }
    }

    /// `notify.claude_code`: an `agent_activity` toast, "Claude Code" (or "Claude Code:
    /// <title>") over the message, with no launch target. The same message within
    /// [`DUPLICATE_WITHIN`] of one posted is a duplicate.
    pub async fn claude_code(&self, message: &str, title: Option<&str>) -> NotifyOutcome {
        let now = self.clock.monotonic();
        {
            let mut memory = lock(&self.memory);
            memory
                .recent
                .retain(|(at, _)| now.saturating_sub(*at) < DUPLICATE_WITHIN);
            if memory.recent.iter().any(|(_, m)| m == message) {
                return NotifyOutcome::Duplicate;
            }
            memory.recent.push_back((now, message.to_string()));
        }
        let text = ToastText {
            title: title.map_or_else(
                || "Claude Code".to_string(),
                |t| format!("Claude Code: {t}"),
            ),
            body: message.to_string(),
            launch: None,
            actions: ToastActions::None,
        };
        let outcome = self.post(AGENT_ACTIVITY, &self.ids.id("n"), &text).await;
        if outcome != NotifyOutcome::Posted {
            let mut memory = lock(&self.memory);
            if let Some(at) = memory
                .recent
                .iter()
                .position(|(at, m)| *at == now && m == message)
            {
                memory.recent.remove(at);
            }
        }
        outcome
    }

    /// An agent's `open_in` or `ui.show` (APP-065): "<who>" / "<who> opened <target title>",
    /// who as for a confirmation (plan 4a ruling, 2026-10-25), launching
    /// `<scheme>:open/<base64url of target>`, or nothing when that URL would pass the longest
    /// `lil:` argument.
    pub async fn agent_opened(
        &self,
        caller: &CallerKind,
        target_title: &str,
        target: &str,
    ) -> NotifyOutcome {
        let who = who(&caller.to_string()).to_string();
        let url = lil_parse::open_url(&self.scheme, target);
        let text = ToastText {
            body: format!("{who} opened {target_title}"),
            title: who,
            launch: within_url_limit(url),
            actions: ToastActions::None,
        };
        self.post(AGENT_ACTIVITY, &self.ids.id("n"), &text).await
    }

    /// The restart hold (P4a-R41): one `job_failed` toast for a recovery that held `held` (in
    /// queue order), launching the first. Once per held batch (owner decision 2026-10-26): when
    /// every one of them was counted in a restart-hold toast already, of this run or an earlier
    /// one, nothing is posted (`Duplicate`); a toast posted counts them all. With none held
    /// nothing is posted (`Unavailable`). Without the suite `state.db`, or when it fails (logged),
    /// the toast is posted.
    pub async fn restart_held(&self, held: &[JobId]) -> NotifyOutcome {
        let Some(first) = held.first() else {
            return NotifyOutcome::Unavailable;
        };
        let ids: Vec<String> = held.iter().map(ToString::to_string).collect();
        let state = self.state.get().cloned();
        if let Some(db) = &state {
            match all_toasted(db, ids.clone()).await {
                Ok(true) => return NotifyOutcome::Duplicate,
                Ok(false) => {}
                Err(e) => tracing::warn!(error = %e, "the restart-hold toasts could not be read"),
            }
        }
        let body = if held.len() == 1 {
            "1 job waits until you resume it in Jobs".to_string()
        } else {
            format!("{} jobs wait until you resume them in Jobs", held.len())
        };
        let text = ToastText {
            title: "Jobs held after a restart".to_string(),
            body,
            launch: Some(lil_parse::task_url(&self.scheme, first.as_str())),
            actions: ToastActions::None,
        };
        let outcome = self
            .post(JOB_FAILED, &format!("restart_{first}"), &text)
            .await;
        if outcome == NotifyOutcome::Posted
            && let Some(db) = &state
            && let Err(e) = count_toasted(db, ids, self.utc_ms()).await
        {
            tracing::warn!(error = %e, "the restart-hold toast could not be recorded");
        }
        outcome
    }

    /// The end of `batch`: its toast, once. `false` while one of its newest attempts isn't
    /// final yet (a job still recording its end).
    async fn batch_ended(&self, jobs: &Jobs, batch: &str) -> bool {
        let rows = jobs.batch_jobs(batch).await;
        if !all_final(&rows) {
            return false;
        }
        {
            let mut memory = lock(&self.memory);
            memory.settled.retain(|(b, _)| b != batch);
            memory.settled.push_back((
                batch.to_string(),
                rows.iter().map(|j| j.job_id.clone()).collect(),
            ));
            while memory.settled.len() > BATCHES_KEPT {
                memory.settled.pop_front();
            }
        }
        let Some(end) = batch_end(&rows) else {
            return true;
        };
        {
            let mut memory = lock(&self.memory);
            if memory
                .batches
                .iter()
                .any(|(b, ids)| b == batch && *ids == end.newest)
            {
                return true;
            }
            memory
                .batches
                .push_back((batch.to_string(), end.newest.clone()));
            while memory.batches.len() > BATCHES_KEPT {
                memory.batches.pop_front();
            }
        }
        let (category, text) = batch_text(&self.scheme, &end);
        // Judged when the batch ended (P4a-R7): a suite window was in front then.
        let in_front = lock(&self.memory).ended_in_front.contains(&end.last);
        let outcome = if in_front {
            NotifyOutcome::Foreground
        } else {
            self.post(category, &end.tag, &text).await
        };
        tracing::info!(batch_id = %batch, jobs = end.counted.len(), category, outcome = ?outcome, "a batch ended");
        true
    }

    /// Whether batch `batch` was settled with job `id` in it.
    fn settled(&self, batch: &str, id: &str) -> bool {
        lock(&self.memory)
            .settled
            .iter()
            .any(|(b, ids)| b == batch && ids.contains(id))
    }

    /// The jobs' end hook (before a job's final row): whether a suite window owns the
    /// foreground as it ends, kept for its batch's toast (P4a-R7). Never blocks for long.
    pub fn job_ending(&self, id: &JobId) {
        if self.watch.foreground_is_one_of(&self.apps.pids()) {
            let mut memory = lock(&self.memory);
            memory.ended_in_front.push_back(id.to_string());
            while memory.ended_in_front.len() > ENDED_IN_FRONT_KEPT {
                memory.ended_in_front.pop_front();
            }
        }
    }

    /// A pending confirmation: its toast with Open and Deny and a fresh token, unless this run
    /// posted one already.
    /// `reconciling`: an earlier run's toast of a refused re-post is looked for and removed.
    async fn confirmation_pending(&self, view: &ConfirmationView, reconciling: bool) {
        let (posted, bare) = {
            let memory = lock(&self.memory);
            (
                memory.confirmations.contains(&view.id),
                memory.bare.contains(&view.id),
            )
        };
        // Posted, or posted bare and the handlers still aren't this channel's.
        if posted || (bare && !self.handlers_ok().await) {
            return;
        }
        let Some(confirm) = self.confirm.get().and_then(Weak::upgrade) else {
            return;
        };
        let token = toast_token(self.ids.as_ref());
        if !confirm.bind_toast_token(&view.id, token.clone()) {
            // It ended meanwhile: nothing to ask.
            return;
        }
        let open_url = lil_parse::confirm_url(&self.scheme, &view.id);
        let deny_url = lil_parse::deny_url(&self.scheme, &view.id, &token);
        let body = if view.covers.contains(&Cover::Confirm) {
            let mut body = format!("{} wants to run {}", who(&view.caller_kind), view.tool);
            // It names the cap too (Checkpoint A walkthrough, ODD-5).
            if view.covers.contains(&Cover::Cap) {
                body.push_str(&format!(", over your {} cap", cap_name(view.cap.as_ref())));
            }
            body
        } else {
            format!(
                "A call would go over your {} cap",
                cap_name(view.cap.as_ref())
            )
        };
        let text = ToastText {
            title: "Confirmation waiting".to_string(),
            body,
            launch: Some(open_url.clone()),
            actions: ToastActions::OpenAndDeny {
                open_url,
                deny_url: deny_url.clone(),
            },
        };
        let (outcome, posted_bare) = self.post_toast(CONFIRMATION_WAITING, &view.id, &text).await;
        if outcome == NotifyOutcome::Posted && posted_bare {
            // No Deny shows its token: none lives.
            confirm.forget_toast_token(&view.id, &token);
            lock(&self.memory).bare.insert(view.id.clone());
            tracing::info!(confirmation = %view.id, "a confirmation toast was posted without its target");
        } else if outcome == NotifyOutcome::Posted {
            let mut memory = lock(&self.memory);
            memory.bare.remove(&view.id);
            memory.confirmations.insert(view.id.clone());
            drop(memory);
            tracing::info!(confirmation = %view.id, deny = %cut_token(&deny_url), "a confirmation toast was posted");
        } else {
            confirm.forget_toast_token(&view.id, &token);
            tracing::info!(confirmation = %view.id, outcome = ?outcome, "no confirmation toast");
            // An earlier run's toast of it is still there with a dead Deny: it goes. A bare
            // toast of this run stays, as it was (CA-walk review M-3).
            if !reconciling || bare {
                return;
            }
            let desktop = self.desktop.clone();
            let shown = tokio::task::spawn_blocking(move || desktop.history())
                .await
                .unwrap_or_default();
            if shown
                .iter()
                .any(|(tag, group)| *tag == view.id && group == CONFIRMATION_WAITING)
            {
                self.remove_toast(&view.id).await;
            }
        }
    }

    /// A confirmation answered, expired or cancelled: its toast goes, whichever run posted it.
    async fn confirmation_ended(&self, id: &str) {
        {
            let mut memory = lock(&self.memory);
            memory.confirmations.remove(id);
            memory.bare.remove(id);
        }
        self.remove_toast(id).await;
    }

    async fn remove_toast(&self, id: &str) {
        let (desktop, tag) = (self.desktop.clone(), id.to_string());
        let removed = tokio::task::spawn_blocking(move || {
            desktop.remove(&tag, CONFIRMATION_WAITING);
        })
        .await;
        if let Err(e) = removed {
            tracing::warn!(confirmation = %id, error = %e, "a confirmation toast could not be removed");
        }
    }

    /// The confirmation toasts against the pending confirmations (module docs, "Reconcile").
    async fn reconcile_confirmations(&self) {
        let Some(confirm) = self.confirm.get().and_then(Weak::upgrade) else {
            return;
        };
        let rows = match confirm.list(ListState::Pending, RECONCILE_LIMIT).await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(code = %e.code, "the confirmation toasts could not be reconciled");
                return;
            }
        };
        // Still pending: every one this core knows of (complete, whatever the list's cap).
        let mut pending = confirm.tracked_ids();
        pending.extend(rows.iter().map(|r| r.id.clone()));
        for row in &rows {
            self.confirmation_pending(&row.view(), true).await;
        }
        let gone: Vec<String> = {
            let memory = lock(&self.memory);
            memory
                .confirmations
                .iter()
                .chain(&memory.bare)
                .filter(|id| !pending.contains(*id))
                .cloned()
                .collect()
        };
        for id in gone {
            self.confirmation_ended(&id).await;
        }
        let desktop = self.desktop.clone();
        let shown = tokio::task::spawn_blocking(move || desktop.history())
            .await
            .unwrap_or_default();
        for (tag, group) in shown {
            if group == CONFIRMATION_WAITING && !pending.contains(&tag) {
                tracing::info!(confirmation = %tag, "a stale confirmation toast is removed");
                self.remove_toast(&tag).await;
            }
        }
        tracing::info!(
            pending = pending.len(),
            "the confirmation toasts were reconciled"
        );
    }

    /// A cap alert: "Spending at <spent> % of your <cap> cap (<period>)" / "Alert at <pct> %",
    /// with no launch target: one per threshold crossed, each with the spend as it was
    /// (owner decision 2026-10-26).
    async fn cap_alert(&self, cap: &str, pct: u64, spent: u64, period: &str) {
        let text = ToastText {
            title: format!("Spending at {spent} % of your {cap} cap ({period})"),
            body: format!("Alert at {pct} %"),
            launch: None,
            actions: ToastActions::None,
        };
        let tag = format!("cap_{}", hex16(&format!("{cap}:{period}:{pct}")));
        let outcome = self.post(CAP_ALERT, &tag, &text).await;
        tracing::info!(cap, threshold_pct = pct, outcome = ?outcome, "a cap alert toast");
    }

    /// Posts `text` tagged `tag` in `category` (module docs, "When"): what became of it.
    async fn post(&self, category: &str, tag: &str, text: &ToastText) -> NotifyOutcome {
        self.post_toast(category, tag, text).await.0
    }

    /// Whether the notification path's handlers are this channel's (the cached reading).
    async fn handlers_ok(&self) -> bool {
        let settings = self.settings.clone();
        tokio::task::spawn_blocking(move || {
            settings.notification_path(false).handlers == HandlersWire::Ok
        })
        .await
        .unwrap_or(false)
    }

    /// [`Notifier::post`], and whether the toast went without its target (module docs, "As
    /// posted").
    async fn post_toast(
        &self,
        category: &str,
        tag: &str,
        text: &ToastText,
    ) -> (NotifyOutcome, bool) {
        let (on, quiet) = {
            let s = self.current.borrow();
            (
                s.notifications.get(category).copied().unwrap_or(true),
                s.quiet_while_fullscreen,
            )
        };
        if !on {
            return (NotifyOutcome::CategoryOff, false);
        }
        let silent = quiet && *self.fullscreen.borrow();
        let (settings, watch, apps, desktop, env) = (
            self.settings.clone(),
            self.watch.clone(),
            self.apps.clone(),
            self.desktop.clone(),
            self.env.clone(),
        );
        let (tag, group, text) = (tag.to_string(), category.to_string(), text.clone());
        let posted = tokio::task::spawn_blocking(move || {
            let path = settings.notification_path(false);
            if !can_post(&path) {
                return (NotifyOutcome::Unavailable, false);
            }
            if watch.foreground_is_one_of(&apps.pids()) {
                return (NotifyOutcome::Foreground, false);
            }
            let bare = path.handlers != HandlersWire::Ok
                && (text.launch.is_some() || text.actions != ToastActions::None);
            let toast = Toast {
                tag,
                group,
                xml: toast_xml(&as_posted(&env, &path.handlers, text)),
                silent,
            };
            match desktop.post(&toast) {
                Ok(()) => (NotifyOutcome::Posted, bare),
                Err(e) => {
                    tracing::warn!(tag = %toast.tag, group = %toast.group, error = %e, "a toast could not be posted");
                    // Reachable sees the failure at once (Appendix G "Ready").
                    settings.notification_path(true);
                    (NotifyOutcome::Unavailable, false)
                }
            }
        })
        .await;
        posted.unwrap_or_else(|e| {
            tracing::error!(error = %e, "posting a toast failed");
            (NotifyOutcome::Unavailable, false)
        })
    }
}

impl NotificationPath for Notifier {
    /// Appendix G "Reachable" (module docs). It may read the desktop.
    fn reachable(&self) -> bool {
        let mut path = self.settings.notification_path(false);
        if path.toasts == ToastsWire::NotYetShown {
            path = self.settings.notification_path(true);
        }
        let on = self
            .current
            .borrow()
            .notifications
            .get(CONFIRMATION_WAITING)
            .copied()
            .unwrap_or(true);
        on && can_post(&path) && path.handlers == HandlersWire::Ok
    }

    fn pending(&self, view: &ConfirmationView) {
        let _ = self.news.send(ConfirmNews::Pending(Box::new(view.clone())));
    }

    fn ended(&self, id: &str) {
        let _ = self.news.send(ConfirmNews::Ended(id.to_string()));
    }

    fn reconcile(&self) {
        // One queued at a time: it reads everything when it runs.
        if !self.reconcile_queued.swap(true, Ordering::SeqCst) {
            let _ = self.news.send(ConfirmNews::Reconcile);
        }
    }
}

/// Whether a toast can be posted on `path`: `ready` or `not_yet_shown`, with the shortcut `ok`.
fn can_post(path: &PathWire) -> bool {
    matches!(path.toasts, ToastsWire::Ready | ToastsWire::NotYetShown)
        && path.shortcut == ShortcutWire::Ok
}

/// `text` as it is posted (module docs, "As posted"): its title and body as the toast shows
/// them ([`shown_plain`]), then redacted as a diagnostics bundle's entries are, with this PC's
/// names, redaction always last ([`settled`]); and, while `handlers` isn't `ok`, a toast with a
/// target without it, its body cut so that it always ends [`OPEN_LILITH`]. The URLs are never
/// redacted. It reads `suite.toml` and may ask Windows for a short name: call it off the
/// runtime.
fn as_posted(env: &Env, handlers: &HandlersWire, mut text: ToastText) -> ToastText {
    let suite_toml = std::fs::read_to_string(env.suite_toml_to_read()).ok();
    let redactor = crate::diagnostics::bundle::redactor_for(env, suite_toml.as_deref()).compile();
    let redact = |s: &str| redactor.text(s);
    let targeted = text.launch.is_some() || text.actions != ToastActions::None;
    let bare = *handlers != HandlersWire::Ok && targeted;
    text.title = settled(&text.title, SHOWN_CHARS, &redact);
    if bare {
        text.launch = None;
        text.actions = ToastActions::None;
        let room = SHOWN_CHARS - OPEN_LILITH.chars().count();
        text.body = format!("{}{OPEN_LILITH}", settled(&text.body, room, &redact));
    } else {
        text.body = settled(&text.body, SHOWN_CHARS, &redact);
    }
    text
}

/// `s` as a toast shows it, at most `max` `char`s, redacted last (CA-walk review I-1): it is
/// normalised first ([`shown_plain`], so a control character inside a key goes and the key is
/// whole), then redacted; when that changed it and its result isn't normal any more, again,
/// at most [`SETTLE_ROUNDS`] times. A text that doesn't settle is shown as `<redacted>`.
fn settled(s: &str, max: usize, redact: &dyn Fn(&str) -> (String, u32)) -> String {
    let mut shown = shown_plain(s, max);
    for _ in 0..SETTLE_ROUNDS {
        let (redacted, n) = redact(&shown);
        if n == 0 {
            return shown;
        }
        let again = shown_plain(&redacted, max);
        if again == redacted {
            return redacted;
        }
        shown = again;
    }
    crate::diagnostics::redact::REDACTED.to_string()
}

/// Whether every job of `ids` was counted in a restart-hold toast already.
async fn all_toasted(db: &StateDb, ids: Vec<String>) -> Result<bool, StateError> {
    db.call(move |conn| {
        let mut known = conn
            .prepare_cached("SELECT 1 FROM restart_hold_toasts WHERE job_id = ?1")
            .map_err(|e| StateError::Io(e.to_string()))?;
        for id in &ids {
            if !known
                .exists([id])
                .map_err(|e| StateError::Io(e.to_string()))?
            {
                return Ok(false);
            }
        }
        Ok(true)
    })
    .await
}

/// Counts `ids` as shown in a restart-hold toast at `at_ms`, keeping the newest
/// [`RESTART_TOASTS_KEPT`].
async fn count_toasted(db: &StateDb, ids: Vec<String>, at_ms: i64) -> Result<(), StateError> {
    db.call(move |conn| {
        let mut run = || -> rusqlite::Result<()> {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            for id in &ids {
                tx.execute(
                    "INSERT OR REPLACE INTO restart_hold_toasts (job_id, toasted_ms) VALUES (?1, ?2)",
                    rusqlite::params![id, at_ms],
                )?;
            }
            tx.execute(
                "DELETE FROM restart_hold_toasts WHERE job_id NOT IN \
                 (SELECT job_id FROM restart_hold_toasts \
                  ORDER BY toasted_ms DESC, job_id DESC LIMIT ?1)",
                [RESTART_TOASTS_KEPT],
            )?;
            tx.commit()
        };
        run().map_err(|e| StateError::Io(e.to_string()))
    })
    .await
}

/// A fresh 128-bit toast token, 32 lowercase hex, from `ids` (two of its 64-bit ids).
fn toast_token(ids: &dyn IdSource) -> String {
    let hex = |id: String| id.rsplit('_').next().unwrap_or_default().to_string();
    format!("{}{}", hex(ids.id("tt")), hex(ids.id("tt")))
}

/// The first 16 hex of SHA-256 over `text`.
fn hex16(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// A failed job marked `interrupted` by a core's start or stop (its error is
/// [`CORE_STOPPED`]); an interruption of the engine's doesn't count.
fn interrupted(job: &BatchJob) -> bool {
    job.status == JobStatus::Failed
        && job
            .substatus
            .as_deref()
            .is_some_and(|s| s.starts_with("interrupted"))
        && job.error_code.as_deref() == Some(CORE_STOPPED)
}

/// Who asks (a confirmation toast) or opened something (an agent-activity toast), by caller
/// kind (Appendix G "Copy").
fn who(caller_kind: &str) -> &str {
    match caller_kind {
        "mcp" => "Claude Code",
        "assistant" => "The assistant",
        "cli" => "A script (lil)",
        k if k.starts_with("ui:") => "LILITH",
        other => other,
    }
}

/// The first cap a confirmation's `cap` names.
fn cap_name(cap: Option<&Value>) -> String {
    cap.and_then(|c| c.get(0))
        .and_then(|hit| hit.get("cap"))
        .and_then(Value::as_str)
        .unwrap_or("spending")
        .to_string()
}

/// A batch that ended: what its toast says (Appendix G "Copy", plan 4a clarification
/// 2026-10-25).
#[derive(Debug, Clone, PartialEq)]
pub struct BatchEnd {
    /// The jobs it counts: the newest attempt at each index, not cancelled, by index.
    pub counted: Vec<BatchJob>,
    /// The newest attempts' ids, by index: one batch end is toasted once.
    pub newest: Vec<String>,
    /// Its first failed job, if any.
    pub failed: Option<BatchJob>,
    pub tag: String,
    /// The newest attempt that ended last: whether a suite window was in front then decides.
    pub last: String,
}

/// The newest attempt at each index of `batch` (every attempt), by index.
fn newest(batch: &[BatchJob]) -> Vec<&BatchJob> {
    let retried: HashSet<&str> = batch.iter().filter_map(|j| j.retry_of.as_deref()).collect();
    let mut newest: Vec<&BatchJob> = batch
        .iter()
        .filter(|j| !retried.contains(j.job_id.as_str()))
        .collect();
    newest.sort_by_key(|j| j.index);
    newest
}

/// Whether every newest attempt of `batch` is final (an empty batch counts as ended).
fn all_final(batch: &[BatchJob]) -> bool {
    newest(batch).iter().all(|j| j.status.is_final())
}

/// The end of a batch, from all its jobs (every attempt): `None` while one of its newest
/// attempts isn't final, when the one that ended last was marked `interrupted` by a core's
/// start or stop, or when nothing in it toasts: every one cancelled, or none of priority
/// `user` or `agent` with an origin other than `core`.
pub fn batch_end(batch: &[BatchJob]) -> Option<BatchEnd> {
    let newest = newest(batch);
    if newest.is_empty() || newest.iter().any(|j| !j.status.is_final()) {
        return None;
    }
    let last = newest
        .iter()
        .max_by_key(|j| (j.finished_utc.unwrap_or(i64::MIN), j.index))?;
    if interrupted(last) {
        return None;
    }
    let counted: Vec<BatchJob> = newest
        .iter()
        .filter(|j| j.status != JobStatus::Cancelled)
        .filter(|j| matches!(j.priority, Priority::User | Priority::Agent))
        .filter(|j| {
            j.origin
                .as_ref()
                .is_some_and(|o| o.surface != Surface::Core)
        })
        .map(|j| (*j).clone())
        .collect();
    let first = counted.first()?;
    let origin = first.origin.as_ref()?;
    let tag = if origin.surface == Surface::Panel {
        first.batch_id.clone()
    } else {
        let key = match &origin.id {
            Some(id) => format!("{}:{id}", origin.surface.wire()),
            None => origin.surface.wire().to_string(),
        };
        format!("o_{}", hex16(&key))
    };
    Some(BatchEnd {
        failed: counted
            .iter()
            .find(|j| j.status == JobStatus::Failed)
            .cloned(),
        newest: newest.iter().map(|j| j.job_id.clone()).collect(),
        last: last.job_id.clone(),
        counted,
        tag,
    })
}

/// The category and text of `end`'s toast.
fn batch_text(scheme: &str, end: &BatchEnd) -> (&'static str, ToastText) {
    let first = &end.counted[0];
    if let Some(failed) = &end.failed {
        let message = failed.error_message.as_deref().unwrap_or("it failed");
        return (
            JOB_FAILED,
            ToastText {
                title: "Job failed".to_string(),
                body: format!("{}: {message}", failed.kind),
                launch: Some(lil_parse::task_url(scheme, &failed.job_id)),
                actions: ToastActions::None,
            },
        );
    }
    let n = end.counted.len();
    let title = if n == 1 {
        "Job finished".to_string()
    } else {
        format!("Batch finished ({n} jobs)")
    };
    let start = end
        .counted
        .iter()
        .map(|j| j.started_utc.unwrap_or(j.created_utc))
        .min();
    let stop = end.counted.iter().filter_map(|j| j.finished_utc).max();
    let elapsed = match (start, stop) {
        (Some(a), Some(b)) => elapsed_text(b - a),
        _ => elapsed_text(0),
    };
    let project = first.project.as_deref().unwrap_or("no project");
    (
        JOB_FINISHED,
        ToastText {
            title,
            body: format!("{} · {project} · {elapsed}", first.kind),
            launch: Some(lil_parse::task_url(scheme, &first.job_id)),
            actions: ToastActions::None,
        },
    )
}

/// `secs` as "45 s", "3 min 4 s" or "1 h 5 min".
pub fn elapsed_text(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        0..60 => format!("{s} s"),
        60..3600 if s % 60 == 0 => format!("{} min", s / 60),
        60..3600 => format!("{} min {} s", s / 60, s % 60),
        _ if (s % 3600) / 60 == 0 => format!("{} h", s / 3600),
        _ => format!("{} h {} min", s / 3600, (s % 3600) / 60),
    }
}

/// [`Notifier::pause_job_events`]: sets `held` to `on` on a test channel (`true`); on any
/// other channel nothing changes (`false`).
fn hold_job_events(channel: &Channel, held: &watch::Sender<bool>, on: bool) -> bool {
    if !matches!(channel, Channel::Test(_)) {
        tracing::warn!(%channel, "the job events lever is for test channels only; refused");
        return false;
    }
    held.send_replace(on);
    true
}

#[cfg(test)]
mod tests {
    #[test] // spec: APP-065
    fn a_launch_url_over_8192_characters_is_left_out() {
        let at = "u".repeat(super::MAX_URL_CHARS);
        assert_eq!(super::within_url_limit(at.clone()), Some(at));
        assert_eq!(
            super::within_url_limit("é".repeat(super::MAX_URL_CHARS + 1)),
            None
        );
    }

    use super::*;

    /// Checkpoint A ruling (conformance M-7): the overflow test's lever holds the `jobs`
    /// events only on a test channel, as the `test.*` tools exist only there; on stable and
    /// dev it is refused and changes nothing.
    #[test] // spec: QA-040, APP-146
    fn the_job_events_lever_works_only_on_test_channels() {
        use lil_platform::Channel;
        for channel in [Channel::Stable, Channel::Dev] {
            let held = watch::Sender::new(false);
            assert!(!hold_job_events(&channel, &held, true), "{channel}");
            assert!(!*held.borrow(), "{channel}: nothing held");
        }
        let held = watch::Sender::new(false);
        let test = Channel::Test("ab12cd34".into());
        assert!(hold_job_events(&test, &held, true));
        assert!(*held.borrow());
        assert!(hold_job_events(&test, &held, false));
        assert!(!*held.borrow());
    }

    #[test] // spec: APP-146
    fn elapsed_reads_seconds_minutes_and_hours() {
        assert_eq!(elapsed_text(0), "0 s");
        assert_eq!(elapsed_text(45), "45 s");
        assert_eq!(elapsed_text(184), "3 min 4 s");
        assert_eq!(elapsed_text(120), "2 min");
        assert_eq!(elapsed_text(3900), "1 h 5 min");
        assert_eq!(elapsed_text(7200), "2 h");
    }

    #[test] // spec: APP-022
    fn a_toast_token_is_32_hex_from_two_ids() {
        let token = toast_token(&lil_platform::RandomIds);
        assert_eq!(token.len(), 32);
        assert!(token.bytes().all(|b| b.is_ascii_hexdigit()), "{token}");
    }

    #[test] // spec: APP-065, APP-022
    fn the_agent_names_follow_the_confirmation_copy() {
        assert_eq!(who(&CallerKind::Mcp.to_string()), "Claude Code");
        assert_eq!(who(&CallerKind::Assistant.to_string()), "The assistant");
        assert_eq!(who(&CallerKind::Cli.to_string()), "A script (lil)");
    }
}
