//! The notifier (ARCH-005, ARCH-055, CORE-113, APP-022, APP-146; Appendix G "Notifications";
//! P4a-R7, P4a-R8, P4a-R9, P4a-R36, P4a-R41): the core's Windows toasts and the notification
//! path that lets a confirmation wait with no app open.
//!
//! - **What it posts.** It subscribes in-process to the `jobs`, `confirmations` and `caps`
//!   topics: `job_finished` or `job_failed` once per batch, when its last job ends
//!   ([`batch_end`]); `confirmation_waiting` for each pending confirmation, removed when it is
//!   answered, expires or is cancelled; `cap_alert` for each `caps` alert. It is called for
//!   `notify.claude_code` ([`Notifier::claude_code`]), an agent's `open_in` and `ui.show`
//!   ([`Notifier::agent_opened`]) and a start that held jobs ([`Notifier::restart_held`]).
//!   No toast is ever a progress toast or updated in place (P4a-R36).
//! - **When.** A toast is posted only when its category is on, the channel's toasts can post
//!   (`ready` or `not_yet_shown`) with the shortcut `ok`, and no connected `ui:*` app owns the
//!   foreground window (P4a-R7); otherwise nothing is posted or queued. While the fullscreen
//!   poll says fullscreen and `quiet_while_fullscreen` is on, every toast is quiet
//!   (`SuppressPopup`), confirmations included; none is dropped for it.
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

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::Duration;

use lil_platform::desktop::{Desktop, Toast};
use lil_platform::{Clock, Env, IdSource};
use lil_protocol::app::{
    ConfirmationView, DesktopSettings, HandlersWire, NotificationPath as PathWire, ShortcutWire,
    ToastsWire,
};
use lil_protocol::{Cover, EventKind, EventParams};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use super::toast_xml::{ToastActions, ToastText, cut_token, toast_xml};
use crate::confirm::{self, Confirm};
use crate::desktop::{DesktopSettingsStore, DesktopWatch, UiApps};
use crate::events::{ClientId, EventBus};
use crate::jobs::{JobId, JobStatus, JobSummary, Jobs, Priority, Surface};

/// The notifier's client id on the event bus: no connection has it.
pub const NOTIFIER_CLIENT: ClientId = u64::MAX;
/// The same Claude Code message within this long is a duplicate (Appendix G).
pub const DUPLICATE_WITHIN: Duration = Duration::from_secs(10);
/// How many ended batches the notifier remembers, so each ends in one toast.
const BATCHES_KEPT: usize = 512;
/// How many event lines may wait for the notifier.
const EVENT_QUEUE: usize = 1024;

/// The categories (Appendix G).
pub const JOB_FINISHED: &str = "job_finished";
pub const JOB_FAILED: &str = "job_failed";
pub const CONFIRMATION_WAITING: &str = "confirmation_waiting";
pub const CAP_ALERT: &str = "cap_alert";
pub const AGENT_ACTIVITY: &str = "agent_activity";

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

/// What the notifier remembers.
#[derive(Default)]
struct Memory {
    /// Claude Code messages posted (or being posted), with when (monotonic).
    recent: VecDeque<(Duration, String)>,
    /// The confirmations whose toast is posted.
    confirmations: HashSet<String>,
    /// The ended batches toasted: `(batch id, the newest attempts' ids)`, newest last.
    batches: VecDeque<(String, Vec<String>)>,
}

/// The notifier (module docs).
pub struct Notifier {
    desktop: Arc<dyn Desktop>,
    watch: Arc<DesktopWatch>,
    settings: Arc<DesktopSettingsStore>,
    current: watch::Receiver<DesktopSettings>,
    fullscreen: watch::Receiver<bool>,
    apps: Arc<UiApps>,
    scheme: String,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdSource>,
    confirm: OnceLock<Weak<Confirm>>,
    jobs: OnceLock<Weak<Jobs>>,
    memory: Mutex<Memory>,
    /// Ends the event task when the notifier is dropped.
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

impl Notifier {
    /// Starts the notifier on the current runtime: it is subscribed to `jobs`, `confirmations`
    /// and `caps` when this returns. Its task ends when the notifier is dropped.
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
        let me = Arc::new(Notifier {
            current: settings.current(),
            fullscreen: watch.fullscreen(),
            desktop,
            watch,
            settings,
            apps,
            scheme: env.activation_scheme(),
            clock,
            ids,
            confirm: OnceLock::new(),
            jobs: OnceLock::new(),
            memory: Mutex::new(Memory::default()),
            stop: CancellationToken::new(),
        });
        let (tx, mut rx) = mpsc::channel(EVENT_QUEUE);
        events.attach(NOTIFIER_CLIENT, tx);
        let topics = ["jobs", confirm::CONFIRMATIONS_TOPIC, "caps"].map(str::to_string);
        events
            .subscribe(NOTIFIER_CLIENT, &topics, &BTreeMap::new())
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
                me.on_line(&line).await;
            }
            events.drop_client(NOTIFIER_CLIENT);
        });
        me
    }

    /// The confirmations whose toasts it posts and whose tokens it binds.
    pub fn set_confirm(&self, confirm: &Arc<Confirm>) {
        let _ = self.confirm.set(Arc::downgrade(confirm));
    }

    /// The jobs whose batches it reads.
    pub fn set_jobs(&self, jobs: &Arc<Jobs>) {
        let _ = self.jobs.set(Arc::downgrade(jobs));
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

    /// An agent's `open_in` or `ui.show` (APP-065): "Claude Code opened <target title>",
    /// launching `<scheme>:open/<base64url of target>`.
    pub async fn agent_opened(&self, target_title: &str, target: &str) -> NotifyOutcome {
        let text = ToastText {
            title: "Claude Code".to_string(),
            body: format!("Claude Code opened {target_title}"),
            launch: Some(lil_parse::open_url(&self.scheme, target)),
            actions: ToastActions::None,
        };
        self.post(AGENT_ACTIVITY, &self.ids.id("n"), &text).await
    }

    /// The restart hold (P4a-R41): one `job_failed` toast for a start that held `held` jobs,
    /// launching the first. With `held` 0 nothing is posted (`Unavailable`).
    pub async fn restart_held(&self, held: usize, first: &JobId) -> NotifyOutcome {
        if held == 0 {
            return NotifyOutcome::Unavailable;
        }
        let body = if held == 1 {
            "1 job waits until you resume it in Jobs".to_string()
        } else {
            format!("{held} jobs wait until you resume them in Jobs")
        };
        let text = ToastText {
            title: "Jobs held after a restart".to_string(),
            body,
            launch: Some(lil_parse::task_url(&self.scheme, first.as_str())),
            actions: ToastActions::None,
        };
        self.post(JOB_FAILED, &format!("restart_{first}"), &text)
            .await
    }

    /// One event line of the bus.
    async fn on_line(&self, line: &str) {
        let Ok(note) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let Some(params) = note
            .get("params")
            .cloned()
            .and_then(|p| serde_json::from_value::<EventParams>(p).ok())
        else {
            return;
        };
        if params.kind != EventKind::Event {
            return;
        }
        let kind = params.data.get("kind").and_then(Value::as_str);
        match (params.topic.as_str(), kind) {
            ("jobs", Some("job")) => {
                if let Some(job) = params
                    .data
                    .get("job")
                    .cloned()
                    .and_then(|j| serde_json::from_value::<JobSummary>(j).ok())
                {
                    self.job_changed(&job).await;
                }
            }
            (confirm::CONFIRMATIONS_TOPIC, Some(kind)) => {
                let Some(view) = params
                    .data
                    .get("confirmation")
                    .cloned()
                    .and_then(|c| serde_json::from_value::<ConfirmationView>(c).ok())
                else {
                    return;
                };
                if kind == "pending" {
                    self.confirmation_pending(&view).await;
                } else {
                    self.confirmation_ended(&view.id).await;
                }
            }
            ("caps", Some("alert")) => {
                let d = &params.data;
                let text = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or_default();
                let pct = d.get("threshold_pct").and_then(Value::as_u64).unwrap_or(0);
                self.cap_alert(text("cap"), pct, text("period")).await;
            }
            _ => {}
        }
    }

    /// A job's new state: when it ended its batch, the batch's toast (module docs).
    async fn job_changed(&self, job: &JobSummary) {
        if !job.status.is_final() || interrupted(job) {
            return;
        }
        let Some(jobs) = self.jobs.get().and_then(Weak::upgrade) else {
            return;
        };
        let batch = jobs.batch_summaries(&job.batch_id).await;
        let Some(end) = batch_end(&batch) else {
            return;
        };
        {
            let mut memory = lock(&self.memory);
            if memory
                .batches
                .iter()
                .any(|(b, ids)| *b == job.batch_id && *ids == end.newest)
            {
                return;
            }
            memory
                .batches
                .push_back((job.batch_id.clone(), end.newest.clone()));
            while memory.batches.len() > BATCHES_KEPT {
                memory.batches.pop_front();
            }
        }
        let (category, text) = batch_text(&self.scheme, &end);
        let outcome = self.post(category, &end.tag, &text).await;
        tracing::info!(batch_id = %job.batch_id, jobs = end.counted.len(), category, outcome = ?outcome, "a batch ended");
    }

    /// A pending confirmation: its toast with Open and Deny and a fresh token.
    async fn confirmation_pending(&self, view: &ConfirmationView) {
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
            format!("{} wants to run {}", who(&view.caller_kind), view.tool)
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
        let outcome = self.post(CONFIRMATION_WAITING, &view.id, &text).await;
        if outcome == NotifyOutcome::Posted {
            lock(&self.memory).confirmations.insert(view.id.clone());
            tracing::info!(confirmation = %view.id, deny = %cut_token(&deny_url), "a confirmation toast was posted");
        } else {
            confirm.forget_toast_token(&view.id, &token);
            tracing::info!(confirmation = %view.id, outcome = ?outcome, "no confirmation toast");
        }
    }

    /// A confirmation answered, expired or cancelled: its toast goes.
    async fn confirmation_ended(&self, id: &str) {
        if !lock(&self.memory).confirmations.remove(id) {
            return;
        }
        let (desktop, tag) = (self.desktop.clone(), id.to_string());
        let removed = tokio::task::spawn_blocking(move || {
            desktop.remove(&tag, CONFIRMATION_WAITING);
        })
        .await;
        if let Err(e) = removed {
            tracing::warn!(confirmation = %id, error = %e, "a confirmation toast could not be removed");
        }
    }

    /// A cap alert: "Spending at <pct> %" / "<cap> cap, <period>", with no launch target.
    async fn cap_alert(&self, cap: &str, pct: u64, period: &str) {
        let text = ToastText {
            title: format!("Spending at {pct} %"),
            body: format!("{cap} cap, {period}"),
            launch: None,
            actions: ToastActions::None,
        };
        let tag = format!("cap_{}", hex16(&format!("{cap}:{period}:{pct}")));
        let outcome = self.post(CAP_ALERT, &tag, &text).await;
        tracing::info!(cap, threshold_pct = pct, outcome = ?outcome, "a cap alert toast");
    }

    /// Posts `text` tagged `tag` in `category` (module docs, "When"): what became of it.
    async fn post(&self, category: &str, tag: &str, text: &ToastText) -> NotifyOutcome {
        let (on, quiet) = {
            let s = self.current.borrow();
            (
                s.notifications.get(category).copied().unwrap_or(true),
                s.quiet_while_fullscreen,
            )
        };
        if !on {
            return NotifyOutcome::CategoryOff;
        }
        let silent = quiet && *self.fullscreen.borrow();
        let toast = Toast {
            tag: tag.to_string(),
            group: category.to_string(),
            xml: toast_xml(text),
            silent,
        };
        let (settings, watch, apps, desktop) = (
            self.settings.clone(),
            self.watch.clone(),
            self.apps.clone(),
            self.desktop.clone(),
        );
        let posted = tokio::task::spawn_blocking(move || {
            let path = settings.notification_path(false);
            if !can_post(&path) {
                return NotifyOutcome::Unavailable;
            }
            if watch.foreground_is_one_of(&apps.pids()) {
                return NotifyOutcome::Foreground;
            }
            match desktop.post(&toast) {
                Ok(()) => NotifyOutcome::Posted,
                Err(e) => {
                    tracing::warn!(tag = %toast.tag, group = %toast.group, error = %e, "a toast could not be posted");
                    NotifyOutcome::Unavailable
                }
            }
        })
        .await;
        posted.unwrap_or_else(|e| {
            tracing::error!(error = %e, "posting a toast failed");
            NotifyOutcome::Unavailable
        })
    }
}

impl confirm::NotificationPath for Notifier {
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
}

/// Whether a toast can be posted on `path`: `ready` or `not_yet_shown`, with the shortcut `ok`.
fn can_post(path: &PathWire) -> bool {
    matches!(path.toasts, ToastsWire::Ready | ToastsWire::NotYetShown)
        && path.shortcut == ShortcutWire::Ok
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

/// A failed job marked `interrupted` by a core's start or stop: it ends no batch's toast.
fn interrupted(job: &JobSummary) -> bool {
    job.status == JobStatus::Failed
        && job
            .substatus
            .as_deref()
            .is_some_and(|s| s.starts_with("interrupted"))
}

/// Who asks, in a confirmation toast (Appendix G "Copy").
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
    pub counted: Vec<JobSummary>,
    /// The newest attempts' ids, by index: one batch end is toasted once.
    pub newest: Vec<String>,
    /// Its first failed job, if any.
    pub failed: Option<JobSummary>,
    pub tag: String,
}

/// The end of a batch, from all its jobs (every attempt): `None` while one of its newest
/// attempts isn't final, or when nothing in it toasts: every one cancelled, or none of
/// priority `user` or `agent` with an origin other than `core`.
pub fn batch_end(batch: &[JobSummary]) -> Option<BatchEnd> {
    let retried: HashSet<&str> = batch.iter().filter_map(|j| j.retry_of.as_deref()).collect();
    let mut newest: Vec<&JobSummary> = batch
        .iter()
        .filter(|j| !retried.contains(j.job_id.as_str()))
        .collect();
    newest.sort_by_key(|j| j.index);
    if newest.is_empty() || newest.iter().any(|j| !j.status.is_final()) {
        return None;
    }
    let counted: Vec<JobSummary> = newest
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
        counted,
        tag,
    })
}

/// The category and text of `end`'s toast.
fn batch_text(scheme: &str, end: &BatchEnd) -> (&'static str, ToastText) {
    let first = &end.counted[0];
    if let Some(failed) = &end.failed {
        let message = failed
            .error
            .as_ref()
            .map_or("it failed", |e| e.message.as_str());
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
    let ms = |t: Option<&String>| t.and_then(|t| crate::telemetry::parse_ts(t));
    let start = end
        .counted
        .iter()
        .filter_map(|j| ms(j.started.as_ref()).or_else(|| ms(Some(&j.created))))
        .min();
    let stop = end
        .counted
        .iter()
        .filter_map(|j| ms(j.finished.as_ref()))
        .max();
    let elapsed = match (start, stop) {
        (Some(a), Some(b)) => elapsed_text((b - a).max(0) / 1000),
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
