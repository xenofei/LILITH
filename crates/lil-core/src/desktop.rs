//! Desktop state (infrastructure, not a CORE-014 module, P4a-R24; ARCH-127, CORE-160, APP-146;
//! Appendix G "Fullscreen", "Keep-awake" and "Notifications").
//!
//! - [`DesktopWatch`]: one poll of [`Desktop::fullscreen`] every [`POLL_EVERY`] (P4a-R6), shared
//!   by the auto-pause and the notifications' quiet mode. `QUNS_RUNNING_D3D_FULL_SCREEN`,
//!   `QUNS_BUSY` and `QUNS_PRESENTATION_MODE` are fullscreen at once; leaving counts after
//!   [`LEAVE_AFTER_POLLS`] polls in a row that aren't (a failed call isn't fullscreen), so an
//!   alt-tab doesn't flap the queue. Each change is published on [`TOPIC`] as
//!   `{kind: "fullscreen", on}`. The same task checks for a sleep every [`SLEEP_CHECK_EVERY`]
//!   (below).
//! - [`DesktopSettingsStore`]: the desktop settings, read from `suite.toml` at start (a hand
//!   edit is read at the next start) and changed only by `app/desktop.set`, which writes them
//!   with targeted edits and publishes `{kind: "settings", settings}`. `desktop.get` adds what
//!   the desktop says now: `keep_awake_available` and the notification path, the latter cached
//!   for [`PATH_CACHE_FOR`] (P4a-R33) unless `fresh`.
//! - [`start_autopause`]: with `[jobs] fullscreen_autopause` on, the `fullscreen` reason is set
//!   on the `gpu-heavy` lane exactly while the poll says fullscreen; it only ever adds and
//!   removes its own reason, so your own pause (`manual`) is never undone (P4a-R5), and the
//!   prompt executing is never interrupted (the jobs' pause).
//! - [`start_keep_awake`]: while a `gpu-heavy` job is `running` and `[power] keep_awake_gpu` is
//!   on, the one system-required power request is held; it is set only when "should hold"
//!   changes, never through a power setting. A release that fails is tried again every
//!   [`RELEASE_RETRY_EVERY`]; dropping [`KeepAwake`] releases the request, and nothing holds it
//!   afterwards.
//! - The sleep log: the hidden window's `PBT_APMSUSPEND` and `PBT_APMRESUMEAUTOMATIC`
//!   ([`DesktopWatch::power_event`]), and, as the backstop for a sleep no broadcast reported,
//!   the growth of the time asleep ([`Clock::slept`], the interrupt-time bias) by more than
//!   [`SLEEP_GAP`] between two checks: a sleep of that length ending at the check (plan 4a
//!   clarification, 2026-10-25). A wall-clock step never counts. Each sleep is logged once
//!   (`slept`, with `from` and `to`) and published once as `{kind: "resumed", slept_from, at}`,
//!   whether a broadcast, the backstop or both saw it; a `PBT_APMSUSPEND` older than the newest
//!   resume is discarded.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lil_platform::desktop::{Desktop, Fullscreen, HandlerState, ShortcutState, ToastsState};
use lil_platform::session_end::PowerEvent;
use lil_platform::{Clock, Env, IdSource};
use lil_protocol::app::{
    DesktopSetParams, DesktopSettings, HandlersWire, NotificationPath, ShortcutWire, ToastsWire,
};
use lil_protocol::{CallerKind, ErrorBody, codes};
use lil_schema::suite_edit::DesktopEdit;
use lil_schema::{DesktopToml, NOTIFICATION_CATEGORIES, SuiteToml};
use serde_json::json;
use tokio::sync::watch;
use tokio::task::AbortHandle;
use tokio_util::sync::CancellationToken;

use crate::events::EventBus;
use crate::jobs::{Jobs, Lane, PauseReason};
use crate::session::Session;
use crate::store::FailPoints;
use crate::transport::ConnObserver;

/// The module `desktop.get` carries (always available, P4a-R24).
pub const DESKTOP_MODULE: &str = "desktop";
/// The topic of the desktop's events.
pub const TOPIC: &str = "desktop";
/// How often the fullscreen state is polled (P4a-R6); a test channel may set it shorter
/// (`LIL_TEST_DESKTOP_POLL_MS`).
pub const POLL_EVERY: Duration = Duration::from_secs(2);
/// Leaving fullscreen counts after this many polls in a row that aren't fullscreen (P4a-R6).
pub const LEAVE_AFTER_POLLS: u32 = 2;
/// How often the time asleep is checked (ARCH-127).
pub const SLEEP_CHECK_EVERY: Duration = Duration::from_secs(30);
/// A growth of the time asleep by more than this between two checks is a sleep; a resume
/// broadcast this soon after the backstop reported a sleep is that sleep.
pub const SLEEP_GAP: Duration = Duration::from_secs(60);
/// How soon keep-awake tries again a release that failed.
pub const RELEASE_RETRY_EVERY: Duration = Duration::from_secs(2);
/// How long the notification path is served from its cache (P4a-R33).
pub const PATH_CACHE_FOR: Duration = Duration::from_secs(10);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// RFC 3339 UTC, to the second, as the core writes times.
fn utc_text(t: SystemTime) -> String {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    lil_schema::time::rfc3339(secs)
}

// ---------------------------------------------------------------------------------------------
// The poll.

/// The fullscreen poll and the sleep log (module docs).
pub struct DesktopWatch {
    desktop: Arc<dyn Desktop>,
    clock: Arc<dyn Clock>,
    events: Arc<EventBus>,
    fullscreen: watch::Sender<bool>,
    sleep: Mutex<SleepLog>,
    /// Ends the poll task when the watch is dropped.
    stop: CancellationToken,
}

/// What the sleep log knows.
struct SleepLog {
    /// When the newest `PBT_APMSUSPEND` came, until a sleep is reported.
    suspended_at: Option<SystemTime>,
    /// The time asleep ([`Clock::slept`]) at the last check or report; `None` until a read
    /// succeeds (and after a report whose read failed), so the next good read is only a baseline.
    slept_seen: Option<Duration>,
    /// When the last sleep reported ended.
    reported_to: Option<SystemTime>,
}

impl std::fmt::Debug for DesktopWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DesktopWatch")
            .field("fullscreen", &*self.fullscreen.borrow())
            .finish_non_exhaustive()
    }
}

impl DesktopWatch {
    /// Starts the poll on the current runtime: a first poll now, then one every `every`, and
    /// the sleep check every [`SLEEP_CHECK_EVERY`]. The task ends when the watch is dropped.
    pub fn start(
        desktop: Arc<dyn Desktop>,
        clock: Arc<dyn Clock>,
        every: Duration,
        events: Arc<EventBus>,
    ) -> Arc<DesktopWatch> {
        let slept_seen = clock.slept();
        let me = Arc::new(DesktopWatch {
            desktop,
            clock,
            events,
            fullscreen: watch::Sender::new(false),
            sleep: Mutex::new(SleepLog {
                suspended_at: None,
                slept_seen,
                reported_to: None,
            }),
            stop: CancellationToken::new(),
        });
        let (weak, stop) = (Arc::downgrade(&me), me.stop.clone());
        tokio::spawn(async move {
            use tokio::time::{Instant, MissedTickBehavior, interval, interval_at};
            let mut poll = interval(every);
            poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
            let mut check = interval_at(Instant::now() + SLEEP_CHECK_EVERY, SLEEP_CHECK_EVERY);
            check.set_missed_tick_behavior(MissedTickBehavior::Delay);
            let mut out = 0u32;
            loop {
                tokio::select! {
                    () = stop.cancelled() => return,
                    _ = poll.tick() => match weak.upgrade() {
                        Some(me) => me.poll(&mut out),
                        None => return,
                    },
                    _ = check.tick() => match weak.upgrade() {
                        Some(me) => me.check_sleep(),
                        None => return,
                    },
                }
            }
        });
        me
    }

    /// Whether the newest polls say fullscreen (P4a-R6).
    pub fn fullscreen(&self) -> watch::Receiver<bool> {
        self.fullscreen.subscribe()
    }

    /// Whether the foreground window belongs to one of `pids` (P4a-R7), read now.
    pub fn foreground_is_one_of(&self, pids: &[u32]) -> bool {
        self.desktop
            .foreground_pid()
            .is_some_and(|pid| pids.contains(&pid))
    }

    /// One poll; `out` counts the polls in a row that weren't fullscreen.
    fn poll(&self, out: &mut u32) {
        let full = matches!(
            self.desktop.fullscreen(),
            Fullscreen::D3d | Fullscreen::Busy | Fullscreen::Presentation
        );
        let was = *self.fullscreen.borrow();
        let on = if full {
            *out = 0;
            true
        } else if was {
            *out += 1;
            *out < LEAVE_AFTER_POLLS
        } else {
            false
        };
        if !on {
            *out = 0;
        }
        if on != was {
            self.fullscreen.send_replace(on);
            tracing::info!(on, "the fullscreen state changed");
            self.events
                .publish(TOPIC, json!({"kind": "fullscreen", "on": on}));
        }
    }

    /// A sleep broadcast of the hidden window (ARCH-127). A resume reports the sleep from the
    /// newest suspend; with none, from the growth of the time asleep since the last check,
    /// unless the backstop reported this sleep within [`SLEEP_GAP`] or the time asleep didn't
    /// grow (or can't be read): then nothing is reported.
    pub fn power_event(&self, event: PowerEvent) {
        let now = self.clock.utc_now();
        let mut log = lock(&self.sleep);
        match event {
            PowerEvent::Suspending => {
                // The newest wins: a suspend whose resume never came is stale.
                log.suspended_at = Some(now);
                tracing::info!("the PC is going to sleep");
            }
            PowerEvent::Resumed => {
                // Every report clears the suspend, so one still here belongs to this resume.
                let from = match log.suspended_at.take() {
                    Some(from) => from,
                    None => {
                        let slept = self.clock.slept();
                        let grown = match (slept, log.slept_seen) {
                            (Some(now), Some(seen)) => now.saturating_sub(seen),
                            _ => Duration::ZERO,
                        };
                        let backstop_had_it = log.reported_to.is_some_and(|to| {
                            now.duration_since(to).unwrap_or_default() <= SLEEP_GAP
                        });
                        if backstop_had_it || grown.is_zero() {
                            if slept.is_some() {
                                log.slept_seen = slept;
                            }
                            return;
                        }
                        now.checked_sub(grown).unwrap_or(now)
                    }
                };
                self.report(&mut log, from, now);
            }
        }
    }

    /// The backstop (every [`SLEEP_CHECK_EVERY`]): the time asleep grew by more than
    /// [`SLEEP_GAP`] since the last check, so the PC slept that long, ending now. A wall-clock
    /// step changes nothing here. A read that fails keeps the last baseline and reports
    /// nothing; the first good read after none is only a baseline.
    pub fn check_sleep(&self) {
        let Some(slept) = self.clock.slept() else {
            return;
        };
        let mut log = lock(&self.sleep);
        let Some(seen) = log.slept_seen.replace(slept) else {
            return;
        };
        let grown = slept.saturating_sub(seen);
        if grown > SLEEP_GAP {
            let to = self.clock.utc_now();
            let from = to.checked_sub(grown).unwrap_or(to);
            log.suspended_at = None;
            self.report(&mut log, from, to);
        }
    }

    /// Logs and publishes one sleep from `from` to `to`.
    fn report(&self, log: &mut SleepLog, from: SystemTime, to: SystemTime) {
        // A failed read leaves no baseline, so this sleep is never counted again.
        log.slept_seen = self.clock.slept();
        log.reported_to = Some(to);
        let (from, to) = (utc_text(from), utc_text(to));
        tracing::info!(from = %from, to = %to, "slept");
        self.events.publish(
            TOPIC,
            json!({"kind": "resumed", "slept_from": from, "at": to}),
        );
    }
}

impl Drop for DesktopWatch {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

// ---------------------------------------------------------------------------------------------
// The settings.

/// The notification path, cached for [`PATH_CACHE_FOR`] (P4a-R33).
struct PathCache {
    desktop: Arc<dyn Desktop>,
    env: Env,
    /// The channel's `lil-image.exe`, which the shortcut and the handlers name.
    exe: PathBuf,
    clock: Arc<dyn Clock>,
    cached: Mutex<Option<(Duration, NotificationPath)>>,
}

impl PathCache {
    fn get(&self, fresh: bool) -> NotificationPath {
        let now = self.clock.monotonic();
        if !fresh
            && let Some((at, path)) = &*lock(&self.cached)
            && now.saturating_sub(*at) < PATH_CACHE_FOR
        {
            return path.clone();
        }
        let path = self.read();
        *lock(&self.cached) = Some((now, path.clone()));
        path
    }

    /// The path as the desktop says it now (Appendix G "Ready").
    fn read(&self) -> NotificationPath {
        let (toasts, toasts_reason) = match self.desktop.toasts_state() {
            ToastsState::Ready => (ToastsWire::Ready, None),
            ToastsState::NotYetShown => (ToastsWire::NotYetShown, None),
            ToastsState::Unavailable(why) => (ToastsWire::Unavailable, Some(why.to_string())),
        };
        let shortcut = match self.desktop.shortcut_state(&self.env, &self.exe) {
            ShortcutState::Ok => ShortcutWire::Ok,
            ShortcutState::Missing => ShortcutWire::Missing,
            ShortcutState::WrongTarget | ShortcutState::WrongAumid | ShortcutState::WrongClsid => {
                ShortcutWire::Wrong
            }
        };
        let handlers = match self.desktop.handler_state(&self.env, &self.exe) {
            HandlerState::Ok => HandlersWire::Ok,
            HandlerState::Missing => HandlersWire::Missing,
            HandlerState::OtherChannel(_) => HandlersWire::OtherChannel,
        };
        NotificationPath {
            toasts,
            toasts_reason,
            shortcut,
            handlers,
        }
    }
}

/// The category of `p.notifications` that `desktop.get` doesn't list, if any (plan 4a ruling,
/// 2026-10-25: such a call is -32602).
pub fn unknown_category(p: &DesktopSetParams) -> Option<&str> {
    p.notifications
        .iter()
        .flatten()
        .map(|(category, _)| category.as_str())
        .find(|category| !NOTIFICATION_CATEGORIES.contains(category))
}

/// The desktop settings (module docs).
pub struct DesktopSettingsStore {
    env: Env,
    desktop: Arc<dyn Desktop>,
    ids: Arc<dyn IdSource>,
    events: Arc<EventBus>,
    failpoints: Arc<FailPoints>,
    path: Arc<PathCache>,
    current: watch::Sender<DesktopSettings>,
    /// One `app/desktop.set` at a time.
    setting: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for DesktopSettingsStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DesktopSettingsStore")
            .field("current", &*self.current.borrow())
            .finish_non_exhaustive()
    }
}

impl DesktopSettingsStore {
    /// The settings of `env`'s `suite.toml` (the defaults for no file; a file that can't be
    /// read or doesn't parse is logged and the defaults apply). `exe` is the channel's
    /// `lil-image.exe`, which the notification path checks the shortcut and handlers against.
    pub fn load(
        env: Env,
        desktop: Arc<dyn Desktop>,
        exe: PathBuf,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdSource>,
        events: Arc<EventBus>,
        failpoints: Arc<FailPoints>,
    ) -> Arc<DesktopSettingsStore> {
        let toml = read_desktop_toml(&env);
        let path = Arc::new(PathCache {
            desktop: desktop.clone(),
            env: env.clone(),
            exe,
            clock,
            cached: Mutex::new(None),
        });
        let settings = DesktopSettings {
            notifications: toml
                .notifications
                .iter()
                .map(|(category, on)| ((*category).to_string(), *on))
                .collect(),
            quiet_while_fullscreen: toml.quiet_while_fullscreen,
            fullscreen_autopause: toml.fullscreen_autopause,
            keep_awake_gpu: toml.keep_awake_gpu,
            keep_awake_available: desktop.power_request_available(),
            notification_path: path.get(false),
        };
        Arc::new(DesktopSettingsStore {
            env,
            desktop,
            ids,
            events,
            failpoints,
            path,
            current: watch::Sender::new(settings),
            setting: tokio::sync::Mutex::new(()),
        })
    }

    /// The settings, changed by each `app/desktop.set`. Its `keep_awake_available` and
    /// `notification_path` are as they were at that change: read them through
    /// [`DesktopSettingsStore::get`] or [`DesktopSettingsStore::notification_path`].
    pub fn current(&self) -> watch::Receiver<DesktopSettings> {
        self.current.subscribe()
    }

    /// The notification path, from the 10 s cache unless `fresh` (P4a-R33). It may read the
    /// desktop (the shortcut, the registry): call it off the async threads.
    pub fn notification_path(&self, fresh: bool) -> NotificationPath {
        self.path.get(fresh)
    }

    /// `desktop.get {fresh?}`: the settings, with `keep_awake_available` and the notification
    /// path as the desktop says them now.
    pub async fn get(&self, fresh: bool) -> DesktopSettings {
        let mut settings = self.current.borrow().clone();
        let (desktop, path) = (self.desktop.clone(), self.path.clone());
        let read = tokio::task::spawn_blocking(move || {
            (desktop.power_request_available(), path.get(fresh))
        })
        .await;
        match read {
            Ok((available, path)) => {
                settings.keep_awake_available = available;
                settings.notification_path = path;
            }
            Err(e) => tracing::error!(error = %e, "reading the desktop failed"),
        }
        settings
    }

    /// `app/desktop.set` (APP-039, APP-146): writes each field given to `suite.toml` with
    /// targeted edits, keeping comments and every other key; then the settings change at once
    /// and `{kind: "settings", settings}` is published. A field left out stays as this core
    /// read it. The router checks the caller (a verified app with `desktop.v1`).
    ///
    /// # Errors
    /// `params.invalid` for a category `desktop.get` doesn't list (the router answers that
    /// -32602 first); the store's error when `suite.toml` can't be written (nothing changes).
    pub async fn set(&self, p: DesktopSetParams) -> Result<DesktopSettings, ErrorBody> {
        let trace_id = self.ids.id("t");
        if let Some(category) = unknown_category(&p) {
            return Err(ErrorBody::new(
                &codes::PARAMS_INVALID,
                format!("{category:?} is not a notification category."),
                trace_id,
            ));
        }
        let edit = DesktopEdit {
            notifications: p
                .notifications
                .iter()
                .flatten()
                .map(|(category, on)| (category.clone(), *on))
                .collect(),
            quiet_while_fullscreen: p.quiet_while_fullscreen,
            fullscreen_autopause: p.fullscreen_autopause,
            keep_awake_gpu: p.keep_awake_gpu,
        };
        let settings = {
            let _one_at_a_time = self.setting.lock().await;
            crate::store::set_desktop(&self.env, &edit, &self.failpoints)
                .await
                .map_err(|e| e.to_error(&trace_id))?;
            let mut next = self.get(false).await;
            for (category, on) in edit.notifications {
                next.notifications.insert(category, on);
            }
            if let Some(on) = p.quiet_while_fullscreen {
                next.quiet_while_fullscreen = on;
            }
            if let Some(on) = p.fullscreen_autopause {
                next.fullscreen_autopause = on;
            }
            if let Some(on) = p.keep_awake_gpu {
                next.keep_awake_gpu = on;
            }
            self.current.send_replace(next.clone());
            next
        };
        tracing::info!(
            quiet_while_fullscreen = settings.quiet_while_fullscreen,
            fullscreen_autopause = settings.fullscreen_autopause,
            keep_awake_gpu = settings.keep_awake_gpu,
            "the desktop settings changed"
        );
        self.events
            .publish(TOPIC, json!({"kind": "settings", "settings": settings}));
        Ok(settings)
    }
}

/// The desktop settings of `env`'s `suite.toml`: the defaults for no file, and for a file that
/// can't be read or doesn't parse, which is logged (a malformed file never turns a setting off
/// silently).
fn read_desktop_toml(env: &Env) -> DesktopToml {
    let path = env.suite_toml_to_read();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return DesktopToml::default(),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "suite.toml can't be read; the desktop settings' defaults apply");
            return DesktopToml::default();
        }
    };
    match SuiteToml::parse(&text) {
        Ok(suite) => suite.desktop(),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "suite.toml doesn't parse; the desktop settings' defaults apply");
            DesktopToml::default()
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The auto-pause and keep-awake.

/// The fullscreen auto-pause (CORE-160): runs until dropped.
#[derive(Debug)]
pub struct AutoPause {
    task: AbortHandle,
}

impl Drop for AutoPause {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Starts the auto-pause: the `fullscreen` reason is set on `gpu-heavy` exactly while
/// `fullscreen_autopause` is on and `w` says fullscreen, and removed otherwise. It adds and
/// removes only that reason (P4a-R5).
pub fn start_autopause(
    w: &Arc<DesktopWatch>,
    jobs: Arc<Jobs>,
    s: watch::Receiver<DesktopSettings>,
) -> Arc<AutoPause> {
    let (mut fullscreen, mut settings) = (w.fullscreen(), s);
    let task = tokio::spawn(async move {
        let mut applied = false;
        loop {
            let want = settings.borrow_and_update().fullscreen_autopause
                && *fullscreen.borrow_and_update();
            if want != applied {
                if want {
                    jobs.pause(Lane::GpuHeavy, PauseReason::Fullscreen).await;
                } else {
                    jobs.resume(Lane::GpuHeavy, PauseReason::Fullscreen).await;
                }
                applied = want;
            }
            tokio::select! {
                changed = settings.changed() => if changed.is_err() { return },
                changed = fullscreen.changed() => if changed.is_err() { return },
            }
        }
    })
    .abort_handle();
    Arc::new(AutoPause { task })
}

/// Keep-awake (ARCH-127): runs until dropped, and releases the request then.
pub struct KeepAwake {
    power: Arc<PowerHold>,
    task: AbortHandle,
}

/// The power request and what keep-awake did with it. Every call to the desktop's request is
/// made under the one lock, so a release when keep-awake stops can't race a hold.
struct PowerHold {
    desktop: Arc<dyn Desktop>,
    state: Mutex<HoldState>,
}

#[derive(Default)]
struct HoldState {
    held: bool,
    /// Keep-awake was dropped: nothing is held any more.
    stopped: bool,
    /// The last call failed (logged once per streak).
    refused: bool,
}

impl PowerHold {
    /// Holds (`want`) or releases the request, unless it already is so or keep-awake stopped.
    /// `false` while it isn't as wanted (the call failed).
    fn apply(&self, want: bool) -> bool {
        let mut s = lock(&self.state);
        if s.stopped || s.held == want {
            return true;
        }
        match self.desktop.set_power_request(want) {
            Ok(()) => {
                s.held = want;
                s.refused = false;
                tracing::info!(held = want, "keep-awake: the power request changed");
                true
            }
            Err(e) => {
                if !s.refused {
                    tracing::warn!(held = want, error = %e, "keep-awake: the power request can't be set on this PC");
                }
                s.refused = true;
                false
            }
        }
    }

    /// Stops keep-awake: the request is released (the platform calls nothing when it isn't
    /// held), and nothing holds it afterwards.
    fn stop(&self) {
        let mut s = lock(&self.state);
        s.stopped = true;
        match self.desktop.set_power_request(false) {
            Ok(()) => s.held = false,
            Err(e) if s.held => {
                tracing::warn!(error = %e, "keep-awake: the power request could not be released");
            }
            Err(_) => {}
        }
    }
}

impl std::fmt::Debug for KeepAwake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeepAwake")
            .field("held", &self.held())
            .finish_non_exhaustive()
    }
}

impl KeepAwake {
    /// Whether the power request is held.
    pub fn held(&self) -> bool {
        lock(&self.power.state).held
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        self.task.abort();
        self.power.stop();
    }
}

/// Starts keep-awake: the one system-required power request is held while a `gpu-heavy` job
/// is `running` and `keep_awake_gpu` is on, and released as soon as either ends. The request
/// is set only when that changes; a release that failed is tried again every
/// [`RELEASE_RETRY_EVERY`] (a hold that failed waits for the next change). A PC that refuses
/// it is logged once per refusal streak.
pub fn start_keep_awake(
    d: Arc<dyn Desktop>,
    jobs: Arc<Jobs>,
    s: watch::Receiver<DesktopSettings>,
) -> Arc<KeepAwake> {
    let power = Arc::new(PowerHold {
        desktop: d,
        state: Mutex::new(HoldState::default()),
    });
    let task = {
        let (power, mut settings) = (power.clone(), s);
        tokio::spawn(async move {
            let mut changes = jobs.changes();
            loop {
                let want =
                    settings.borrow_and_update().keep_awake_gpu && jobs.any_running(Lane::GpuHeavy);
                let release_failed = !power.apply(want) && !want;
                tokio::select! {
                    changed = changes.changed() => if changed.is_err() { return },
                    changed = settings.changed() => if changed.is_err() { return },
                    () = tokio::time::sleep(RELEASE_RETRY_EVERY), if release_failed => {}
                }
            }
        })
        .abort_handle()
    };
    Arc::new(KeepAwake { power, task })
}

// ---------------------------------------------------------------------------------------------
// The connected apps.

/// The processes of the verified `ui:*` connections, while they are connected: the foreground
/// check (P4a-R7) and `test.desktop {foreground: "suite"}` read them.
#[derive(Debug, Default)]
pub struct UiApps {
    conns: Mutex<HashMap<u64, u32>>,
}

impl UiApps {
    /// The connected apps' process ids, each once, lowest first.
    pub fn pids(&self) -> Vec<u32> {
        let mut pids: Vec<u32> = lock(&self.conns).values().copied().collect();
        pids.sort_unstable();
        pids.dedup();
        pids
    }
}

impl ConnObserver for UiApps {
    fn ready(&self, conn_id: u64, session: &Session) {
        if matches!(session.caller, CallerKind::Ui(_)) && session.client_pid != 0 {
            lock(&self.conns).insert(conn_id, session.client_pid);
        }
    }

    fn closed(&self, conn_id: u64) {
        lock(&self.conns).remove(&conn_id);
    }
}
