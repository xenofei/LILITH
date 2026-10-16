//! The core's lifecycle (ARCH-020, ARCH-021, ARCH-023, QA-012): when it is idle, how it
//! drains, and the order of its shutdown. Infrastructure, not a CORE-014 module.
//!
//! **Idle** (ARCH-020). The core is idle while it has no counted connection, no call in
//! flight and no busy [`IdleBlocker`]. Any change restarts the idle period, measured with
//! `Clock::monotonic` (a wall-clock jump changes nothing, CORE-012); a blocker that frees up
//! restarts it at the first [`Lifecycle::tick`] that sees it free. `tick` reports
//! [`DrainReason::Idle`] once the period reaches `idle_after`; the core ticks every
//! [`IDLE_TICK`]. Plan 3 registers jobs, pending confirmations and outside ComfyUI jobs as
//! blockers.
//!
//! **Draining** (ARCH-021 step 1). [`Lifecycle::begin_drain`] records the reason; from then
//! on no new call is admitted ([`Lifecycle::try_call_started`]). [`Lifecycle::run_shutdown`]
//! waits until no call is in flight and no blocker is busy, until the drain's deadline:
//!
//! | Reason | Wait |
//! |---|---|
//! | `Requested` (`core/drain`, "exit when safe") | no timeout; connected clients do not hold it |
//! | `SessionEnd` | at most [`SESSION_END_BUDGET`] |
//! | `Fatal`, `TestParentGone` | at most [`FATAL_BUDGET`] |
//! | `Idle` | none |
//!
//! A later reason only escalates a drain: `Requested` or `Idle` < `TestParentGone` <
//! `SessionEnd` < `Fatal`. The deadline then becomes the earlier of the old one and the new
//! reason's budget from now, so an escalation never extends the time box: a logoff during an
//! unlimited requested drain still gets its 4.5 s, a fatal error during a session end keeps
//! the session end's deadline, and a fatal error at any point exits 4.
//!
//! The core's idle exit is [`Lifecycle::drain_if_idle`]: the idle check and the drain under
//! one lock, so a client that connects meanwhile never meets an idle exit.
//!
//! Then the [`ShutdownStep`]s run grouped by [`ShutdownPhase`], in the ARCH-021 order whatever
//! the order they were registered in (registration order within a phase). The core then
//! closes its state databases and its pipe, and exits with [`ShutdownReport::exit_code`].

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use ww_platform::Clock;

/// A core with nothing to do exits after this long (ARCH-020).
pub const IDLE_EXIT_AFTER: Duration = Duration::from_secs(600);
/// How often the core checks whether it is idle.
pub const IDLE_TICK: Duration = Duration::from_secs(15);
/// How long a session-end drain waits for running work (ARCH-021).
pub const SESSION_END_BUDGET: Duration = ww_platform::session_end::SESSION_END_BUDGET;
/// How long a fatal (or test-parent-gone) drain waits for running work (ARCH-021).
pub const FATAL_BUDGET: Duration = Duration::from_secs(5);
/// How often a waiting drain looks at its blockers again (calls ending wake it at once).
const BLOCKER_POLL: Duration = Duration::from_millis(250);

/// Something that keeps the core from being idle and holds a drain (ARCH-020, ARCH-021).
pub trait IdleBlocker: Send + Sync {
    fn name(&self) -> &'static str;
    fn busy(&self) -> bool;
}

/// The phases of ARCH-021's shutdown, in their order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ShutdownPhase {
    Drain,
    CancelEnginePrompts,
    PersistInterrupted,
    StopEngines,
}

/// One shutdown step, run once in its phase.
pub trait ShutdownStep: Send + Sync {
    fn phase(&self) -> ShutdownPhase;
    fn name(&self) -> &'static str;
    fn run(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

/// Why the core drains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrainReason {
    /// Nothing to do for `idle_after` (ARCH-020).
    Idle,
    /// `core/drain` (ARCH-023).
    Requested,
    /// The Windows session ends (ARCH-021).
    SessionEnd,
    /// Something died; what (QA-012).
    Fatal(String),
    /// Test channels: the test process is gone.
    TestParentGone,
}

impl fmt::Display for DrainReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DrainReason::Idle => f.write_str("idle"),
            DrainReason::Requested => f.write_str("requested"),
            DrainReason::SessionEnd => f.write_str("session end"),
            DrainReason::Fatal(what) => write!(f, "fatal ({what})"),
            DrainReason::TestParentGone => f.write_str("test parent gone"),
        }
    }
}

/// What a shutdown did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownReport {
    pub reason: DrainReason,
    /// The names of the steps that ran, in order. The running core appends `state.close`
    /// and `pipe.close` (`RunningCore::wait`).
    pub steps: Vec<&'static str>,
    /// Calls still in flight when the wait ended (a time-boxed drain abandons them).
    pub abandoned_calls: usize,
}

impl ShutdownReport {
    /// The process exit code: 4 after a fatal error, else 0 (spec Appendix D).
    pub fn exit_code(&self) -> u8 {
        match self.reason {
            DrainReason::Fatal(_) => 4,
            _ => 0,
        }
    }
}

/// A call in flight; dropping it ends the call.
#[must_use = "the call counts as in flight until this is dropped"]
pub struct CallGuard {
    shared: Arc<Shared>,
}

impl fmt::Debug for CallGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallGuard").finish_non_exhaustive()
    }
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        {
            let mut inner = self.shared.lock();
            inner.in_flight -= 1;
            self.shared.restart_idle(&mut inner);
        }
        self.shared.changed.notify_waiters();
    }
}

/// The core's lifecycle (module docs).
pub struct Lifecycle {
    shared: Arc<Shared>,
}

struct Shared {
    clock: Arc<dyn Clock>,
    idle_after: Duration,
    inner: Mutex<Inner>,
    /// Woken whenever a call ends or a connection closes.
    changed: Notify,
    /// Cancelled once a drain has begun.
    draining: CancellationToken,
}

struct Inner {
    connections: usize,
    in_flight: usize,
    blockers: Vec<Arc<dyn IdleBlocker>>,
    steps: Vec<Arc<dyn ShutdownStep>>,
    /// Monotonic time the current idle period began; `None` while busy.
    idle_since: Option<Duration>,
    reason: Option<DrainReason>,
    /// When the wait for running work ends; `None` with a reason: no limit. An escalation
    /// can only bring it closer (`DrainReason::budget` from the escalation, or the old one).
    deadline: Option<tokio::time::Instant>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        // Every update is a few plain assignments, so the state stays usable after a panic.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// After a change: a new idle period begins now if nothing is connected or running,
    /// else there is none. (Blockers are looked at by `tick`.)
    fn restart_idle(&self, inner: &mut Inner) {
        inner.idle_since =
            (inner.connections == 0 && inner.in_flight == 0).then(|| self.clock.monotonic());
    }
}

impl fmt::Debug for Lifecycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.shared.lock();
        f.debug_struct("Lifecycle")
            .field("connections", &inner.connections)
            .field("in_flight", &inner.in_flight)
            .field("reason", &inner.reason)
            .finish_non_exhaustive()
    }
}

impl Lifecycle {
    /// A lifecycle whose first idle period begins now.
    pub fn new(clock: Arc<dyn Clock>, idle_after: Duration) -> Arc<Lifecycle> {
        let now = clock.monotonic();
        Arc::new(Lifecycle {
            shared: Arc::new(Shared {
                clock,
                idle_after,
                inner: Mutex::new(Inner {
                    connections: 0,
                    in_flight: 0,
                    blockers: Vec::new(),
                    steps: Vec::new(),
                    idle_since: Some(now),
                    reason: None,
                    deadline: None,
                }),
                changed: Notify::new(),
                draining: CancellationToken::new(),
            }),
        })
    }

    /// A connection that counts towards idle opened.
    pub fn connection_opened(&self) {
        let mut inner = self.shared.lock();
        inner.connections += 1;
        self.shared.restart_idle(&mut inner);
    }

    /// A counted connection closed (or stopped counting: a refused one, ruling P23).
    pub fn connection_closed(&self) {
        {
            let mut inner = self.shared.lock();
            inner.connections = inner.connections.saturating_sub(1);
            self.shared.restart_idle(&mut inner);
        }
        self.shared.changed.notify_waiters();
    }

    /// A call starts, draining or not; it is in flight until the guard is dropped. Unit tests
    /// only: the dispatcher admits calls with [`Lifecycle::try_call_started`].
    #[cfg(test)]
    pub(crate) fn call_started(&self) -> CallGuard {
        let mut inner = self.shared.lock();
        inner.in_flight += 1;
        inner.idle_since = None;
        CallGuard {
            shared: self.shared.clone(),
        }
    }

    /// A call starts: it is in flight until the guard is dropped. Unless a drain has begun:
    /// then `None`, atomically with the drain (a call is either admitted before the drain and
    /// waited for, or refused).
    pub fn try_call_started(&self) -> Option<CallGuard> {
        let mut inner = self.shared.lock();
        if inner.reason.is_some() {
            return None;
        }
        inner.in_flight += 1;
        inner.idle_since = None;
        Some(CallGuard {
            shared: self.shared.clone(),
        })
    }

    /// Adds something that keeps the core from being idle and holds a drain.
    pub fn add_blocker(&self, b: Arc<dyn IdleBlocker>) {
        self.shared.lock().blockers.push(b);
    }

    /// Adds a step to the shutdown.
    pub fn add_shutdown_step(&self, s: Arc<dyn ShutdownStep>) {
        self.shared.lock().steps.push(s);
    }

    /// `Some(Idle)` once the core has been idle for `idle_after`; `None` while it is busy, not
    /// idle long enough, or draining. It begins no drain itself.
    pub fn tick(&self) -> Option<DrainReason> {
        let blocked = self.any_blocker_busy();
        let mut inner = self.shared.lock();
        self.idle_long_enough(&mut inner, blocked)
            .then_some(DrainReason::Idle)
    }

    /// [`Lifecycle::tick`] and [`Lifecycle::begin_drain`]`(Idle)` in one step: the counts
    /// are checked and the drain begun under one lock, so a client that connects meanwhile
    /// is never met by an idle exit. True if the idle drain began.
    pub fn drain_if_idle(&self) -> bool {
        let blocked = self.any_blocker_busy();
        {
            let mut inner = self.shared.lock();
            if !self.idle_long_enough(&mut inner, blocked) {
                return false;
            }
            Self::set_reason(&mut inner, DrainReason::Idle);
        }
        self.drain_begun();
        true
    }

    /// Whether some blocker is busy. Looked at outside the lock: a blocker may take locks of
    /// its own.
    fn any_blocker_busy(&self) -> bool {
        let blockers = self.shared.lock().blockers.clone();
        blockers.iter().any(|b| b.busy())
    }

    /// The idle decision, under the lock (module docs).
    fn idle_long_enough(&self, inner: &mut Inner, blocked: bool) -> bool {
        if inner.reason.is_some() {
            return false;
        }
        if blocked || inner.connections > 0 || inner.in_flight > 0 {
            inner.idle_since = None;
            return false;
        }
        let now = self.shared.clock.monotonic();
        let since = *inner.idle_since.get_or_insert(now);
        now.saturating_sub(since) >= self.shared.idle_after
    }

    /// Begins draining for `r`. On a core that already drains, `r` only escalates the drain
    /// (module docs): a requested or idle drain becomes a session-end, test-parent or fatal
    /// one, and any drain becomes fatal; anything else changes nothing.
    pub fn begin_drain(&self, r: DrainReason) {
        if Self::set_reason(&mut self.shared.lock(), r) {
            self.drain_begun();
        }
    }

    /// Records `r` if it begins or escalates the drain (false: nothing changed). An
    /// escalation keeps the earlier deadline when that comes first, so it never extends the
    /// time box: a session end's window still returns after its 4.5 s.
    fn set_reason(inner: &mut Inner, r: DrainReason) -> bool {
        match &inner.reason {
            None => tracing::info!(reason = %r, "the core drains"),
            Some(current) if r.urgency() > current.urgency() => {
                tracing::info!(from = %current, to = %r, "the drain escalates");
            }
            Some(current) => {
                tracing::debug!(current = %current, ignored = %r, "the core is already draining");
                return false;
            }
        }
        let new = r.budget().map(|b| tokio::time::Instant::now() + b);
        inner.deadline = match (inner.reason.is_some(), inner.deadline, new) {
            (true, Some(old), Some(new)) => Some(old.min(new)),
            (true, Some(old), None) => Some(old),
            (_, _, new) => new,
        };
        inner.reason = Some(r);
        true
    }

    /// After a reason was recorded: wakes everything waiting for a drain or watching it.
    fn drain_begun(&self) {
        self.shared.draining.cancel();
        self.shared.changed.notify_waiters();
    }

    pub fn is_draining(&self) -> bool {
        self.shared.lock().reason.is_some()
    }

    /// The reason of the drain, once one has begun.
    pub fn drain_reason(&self) -> Option<DrainReason> {
        self.shared.lock().reason.clone()
    }

    /// A token cancelled once a drain has begun (cancelling it does nothing to the drain).
    pub fn drain_token(&self) -> CancellationToken {
        self.shared.draining.child_token()
    }

    /// Counted connections.
    pub fn connections(&self) -> usize {
        self.shared.lock().connections
    }

    /// Calls in flight.
    pub fn in_flight(&self) -> usize {
        self.shared.lock().in_flight
    }

    /// Waits for a drain to begin, waits for running work as its reason allows (module docs),
    /// then runs the shutdown steps in phase order.
    pub async fn run_shutdown(&self) -> ShutdownReport {
        self.shared.draining.cancelled().await;
        self.wait_for_work().await;
        let (abandoned_calls, mut steps) = {
            let inner = self.shared.lock();
            (inner.in_flight, inner.steps.clone())
        };
        // A stable sort: registration order within a phase.
        steps.sort_by_key(|s| s.phase());
        let mut ran = Vec::with_capacity(steps.len());
        for step in steps {
            tracing::info!(step = step.name(), phase = ?step.phase(), "shutdown step");
            step.run().await;
            ran.push(step.name());
        }
        // The reason as it stands now: a fatal error during the steps still exits 4.
        let reason = self
            .drain_reason()
            .expect("the drain token is cancelled only after the reason is set");
        ShutdownReport {
            reason,
            steps: ran,
            abandoned_calls,
        }
    }

    /// Returns once no call is in flight and no blocker is busy, or once the drain's deadline
    /// has passed (module docs).
    async fn wait_for_work(&self) {
        loop {
            let changed = self.shared.changed.notified();
            tokio::pin!(changed);
            // Registered before the check, so a call ending (or an escalation) in between is
            // not missed.
            changed.as_mut().enable();
            let (reason, deadline, in_flight, blockers) = {
                let inner = self.shared.lock();
                (
                    inner.reason.clone(),
                    inner.deadline,
                    inner.in_flight,
                    inner.blockers.clone(),
                )
            };
            let Some(reason) = reason else {
                return;
            };
            let blocked = blockers.iter().any(|b| b.busy());
            if in_flight == 0 && !blocked {
                return;
            }
            if deadline.is_some_and(|d| tokio::time::Instant::now() >= d) {
                if !reason.budget().is_some_and(|b| b.is_zero()) {
                    tracing::warn!(
                        reason = %reason,
                        in_flight,
                        blocked,
                        "running work did not finish in time"
                    );
                }
                return;
            }
            let out_of_time = async {
                match deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending().await,
                }
            };
            let look_again = async {
                if blocked {
                    tokio::time::sleep(BLOCKER_POLL).await;
                } else {
                    std::future::pending::<()>().await;
                }
            };
            tokio::select! {
                () = changed => {}
                () = out_of_time => {}
                () = look_again => {}
            }
        }
    }
}

impl DrainReason {
    /// How long a drain for this reason waits for running work; `None`: no limit.
    fn budget(&self) -> Option<Duration> {
        match self {
            DrainReason::Requested => None,
            DrainReason::SessionEnd => Some(SESSION_END_BUDGET),
            DrainReason::Fatal(_) | DrainReason::TestParentGone => Some(FATAL_BUDGET),
            DrainReason::Idle => Some(Duration::ZERO),
        }
    }

    /// A later reason replaces the current one only when it is more urgent.
    fn urgency(&self) -> u8 {
        match self {
            DrainReason::Idle | DrainReason::Requested => 0,
            DrainReason::TestParentGone => 1,
            DrainReason::SessionEnd => 2,
            DrainReason::Fatal(_) => 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use ww_testkit::FakeClock;

    use super::*;

    fn lifecycle(clock: &Arc<FakeClock>) -> Arc<Lifecycle> {
        Lifecycle::new(clock.clone(), IDLE_EXIT_AFTER)
    }

    /// A blocker whose busy flag the test sets.
    struct Flag(AtomicBool);

    impl Flag {
        fn new(busy: bool) -> Arc<Flag> {
            Arc::new(Flag(AtomicBool::new(busy)))
        }
        fn set(&self, busy: bool) {
            self.0.store(busy, Ordering::SeqCst);
        }
    }

    impl IdleBlocker for Flag {
        fn name(&self) -> &'static str {
            "flag"
        }
        fn busy(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }

    /// A step that logs its name.
    struct Logged {
        phase: ShutdownPhase,
        name: &'static str,
        log: Arc<Mutex<Vec<&'static str>>>,
    }

    impl ShutdownStep for Logged {
        fn phase(&self) -> ShutdownPhase {
            self.phase
        }
        fn name(&self) -> &'static str {
            self.name
        }
        fn run(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            Box::pin(async move { self.log.lock().unwrap().push(self.name) })
        }
    }

    #[test] // spec: ARCH-020
    fn idle_exit_after_ten_minutes_with_nothing_running() {
        assert_eq!(IDLE_EXIT_AFTER, Duration::from_secs(600));
        assert_eq!(IDLE_TICK, Duration::from_secs(15));
        let clock = FakeClock::new();
        let lc = lifecycle(&clock);
        assert_eq!(lc.tick(), None);
        clock.advance(Duration::from_secs(599));
        assert_eq!(lc.tick(), None);
        clock.advance(Duration::from_secs(1));
        assert_eq!(lc.tick(), Some(DrainReason::Idle));
        // `tick` only reports; the drain begins when the caller says so.
        assert!(!lc.is_draining());
        lc.begin_drain(DrainReason::Idle);
        assert!(lc.is_draining());
        assert_eq!(lc.tick(), None, "a draining core is not idle again");
    }

    #[test] // spec: ARCH-020
    fn connection_call_or_blocker_restarts_idle() {
        let clock = FakeClock::new();
        let lc = lifecycle(&clock);
        let minutes = |m: u64| Duration::from_secs(60 * m);

        // A connection holds the core, and its close restarts the idle period.
        clock.advance(minutes(9));
        lc.connection_opened();
        clock.advance(minutes(30));
        assert_eq!(lc.tick(), None, "an open connection");
        lc.connection_closed();
        clock.advance(Duration::from_secs(599));
        assert_eq!(lc.tick(), None, "the idle period restarted at the close");
        clock.advance(Duration::from_secs(1));
        assert_eq!(lc.tick(), Some(DrainReason::Idle));

        // So does a call in flight.
        let guard = lc.call_started();
        assert_eq!(lc.tick(), None, "a call in flight");
        clock.advance(minutes(30));
        assert_eq!(lc.tick(), None, "a call in flight");
        drop(guard);
        clock.advance(Duration::from_secs(599));
        assert_eq!(
            lc.tick(),
            None,
            "the idle period restarted when the call ended"
        );
        clock.advance(Duration::from_secs(1));
        assert_eq!(lc.tick(), Some(DrainReason::Idle));

        // And a busy blocker; the period restarts at the first tick that sees it free.
        let blocker = Flag::new(true);
        lc.add_blocker(blocker.clone());
        assert_eq!(lc.tick(), None, "a busy blocker");
        clock.advance(minutes(30));
        assert_eq!(lc.tick(), None, "a busy blocker");
        blocker.set(false);
        assert_eq!(lc.tick(), None, "the idle period restarts now");
        clock.advance(Duration::from_secs(599));
        assert_eq!(lc.tick(), None);
        clock.advance(Duration::from_secs(1));
        assert_eq!(lc.tick(), Some(DrainReason::Idle));
    }

    #[test] // spec: CORE-012, ARCH-020
    fn utc_jump_does_not_trigger_idle_exit() {
        let clock = FakeClock::new();
        let lc = lifecycle(&clock);
        clock.shift_utc(24 * 60 * 60);
        assert_eq!(lc.tick(), None, "a wall clock jump forward");
        clock.shift_utc(-365 * 24 * 60 * 60);
        assert_eq!(lc.tick(), None, "a wall clock jump back");
        clock.advance(Duration::from_secs(599));
        assert_eq!(lc.tick(), None);
        clock.advance(Duration::from_secs(1));
        assert_eq!(lc.tick(), Some(DrainReason::Idle));
    }

    #[tokio::test] // spec: ARCH-021
    async fn shutdown_runs_phases_in_arch_021_order() {
        let clock = FakeClock::new();
        let lc = lifecycle(&clock);
        let log = Arc::new(Mutex::new(Vec::new()));
        // Registered in reverse order, with two steps in one phase.
        for (phase, name) in [
            (ShutdownPhase::StopEngines, "stop engines"),
            (ShutdownPhase::PersistInterrupted, "persist interrupted"),
            (ShutdownPhase::CancelEnginePrompts, "cancel prompts"),
            (ShutdownPhase::Drain, "drain a"),
            (ShutdownPhase::Drain, "drain b"),
        ] {
            lc.add_shutdown_step(Arc::new(Logged {
                phase,
                name,
                log: log.clone(),
            }));
        }
        lc.begin_drain(DrainReason::Idle);
        let report = lc.run_shutdown().await;
        let order = [
            "drain a",
            "drain b",
            "cancel prompts",
            "persist interrupted",
            "stop engines",
        ];
        assert_eq!(*log.lock().unwrap(), order);
        assert_eq!(report.steps, order);
        assert_eq!(report.reason, DrainReason::Idle);
        assert_eq!(report.abandoned_calls, 0);
    }

    #[tokio::test(start_paused = true)] // spec: ARCH-021, ARCH-023
    async fn requested_drain_waits_for_in_flight_calls_and_blockers() {
        let clock = FakeClock::new();
        let lc = lifecycle(&clock);
        let blocker = Flag::new(true);
        lc.add_blocker(blocker.clone());
        let guard = lc.call_started();
        // A connected client does not hold a requested drain up.
        lc.connection_opened();
        lc.begin_drain(DrainReason::Requested);
        assert!(lc.is_draining());

        let running = lc.clone();
        let mut shutdown = tokio::spawn(async move { running.run_shutdown().await });
        let an_hour = Duration::from_secs(3600);
        assert!(
            tokio::time::timeout(an_hour, &mut shutdown).await.is_err(),
            "a requested drain has no timeout"
        );
        drop(guard);
        assert!(
            tokio::time::timeout(an_hour, &mut shutdown).await.is_err(),
            "a busy blocker holds a requested drain"
        );
        blocker.set(false);
        let report = tokio::time::timeout(an_hour, shutdown)
            .await
            .expect("the drain ends once nothing runs")
            .unwrap();
        assert_eq!(report.reason, DrainReason::Requested);
        assert_eq!(report.abandoned_calls, 0);
        assert_eq!(report.exit_code(), 0);
    }

    #[tokio::test(start_paused = true)] // spec: ARCH-021
    async fn session_end_drain_is_time_boxed() {
        assert_eq!(SESSION_END_BUDGET, Duration::from_millis(4500));
        assert_eq!(FATAL_BUDGET, Duration::from_secs(5));
        for (reason, budget) in [
            (DrainReason::SessionEnd, SESSION_END_BUDGET),
            (DrainReason::Fatal("state".into()), FATAL_BUDGET),
            (DrainReason::TestParentGone, FATAL_BUDGET),
            (DrainReason::Idle, Duration::ZERO),
        ] {
            let clock = FakeClock::new();
            let lc = lifecycle(&clock);
            let blocker = Flag::new(true);
            lc.add_blocker(blocker);
            let _stuck = lc.call_started();
            lc.begin_drain(reason.clone());
            let started = tokio::time::Instant::now();
            let report = lc.run_shutdown().await;
            assert_eq!(started.elapsed(), budget, "{reason:?}");
            assert_eq!(report.reason, reason);
            assert_eq!(report.abandoned_calls, 1, "{reason:?}");
        }
    }

    #[test] // spec: QA-012, ARCH-021
    fn fatal_shutdown_exits_4_and_others_exit_0() {
        let report = |reason| ShutdownReport {
            reason,
            steps: Vec::new(),
            abandoned_calls: 0,
        };
        assert_eq!(report(DrainReason::Fatal("state".into())).exit_code(), 4);
        for reason in [
            DrainReason::Idle,
            DrainReason::Requested,
            DrainReason::SessionEnd,
            DrainReason::TestParentGone,
        ] {
            assert_eq!(report(reason.clone()).exit_code(), 0, "{reason:?}");
        }
    }

    #[test] // spec: ARCH-021
    fn a_later_drain_reason_only_escalates_and_calls_are_refused_while_draining() {
        let fatal = || DrainReason::Fatal("state".into());
        let lc = lifecycle(&FakeClock::new());
        assert!(lc.try_call_started().is_some());
        lc.begin_drain(DrainReason::Requested);
        assert!(
            lc.try_call_started().is_none(),
            "a draining core admits no call"
        );
        // Requested < TestParentGone, SessionEnd < Fatal; never back down.
        lc.begin_drain(DrainReason::Idle);
        assert_eq!(lc.drain_reason(), Some(DrainReason::Requested));
        lc.begin_drain(DrainReason::TestParentGone);
        assert_eq!(lc.drain_reason(), Some(DrainReason::TestParentGone));
        lc.begin_drain(DrainReason::SessionEnd);
        assert_eq!(lc.drain_reason(), Some(DrainReason::SessionEnd));
        lc.begin_drain(fatal());
        assert_eq!(lc.drain_reason(), Some(fatal()));
        for later in [
            DrainReason::Requested,
            DrainReason::SessionEnd,
            DrainReason::Fatal("other".into()),
        ] {
            lc.begin_drain(later);
            assert_eq!(lc.drain_reason(), Some(fatal()));
        }

        // An idle drain becomes fatal too (exit code 4).
        let lc = lifecycle(&FakeClock::new());
        lc.begin_drain(DrainReason::Idle);
        lc.begin_drain(DrainReason::Requested);
        assert_eq!(lc.drain_reason(), Some(DrainReason::Idle));
        lc.begin_drain(fatal());
        assert_eq!(lc.drain_reason(), Some(fatal()));
    }

    #[tokio::test(start_paused = true)] // spec: ARCH-021
    async fn escalation_never_extends_the_time_box() {
        // The session ends; a fatal error 4 s later keeps the session end's deadline (the
        // window gives up after 4.5 s, and Windows ends the process then).
        let lc = lifecycle(&FakeClock::new());
        let _stuck = lc.call_started();
        let started = tokio::time::Instant::now();
        lc.begin_drain(DrainReason::SessionEnd);
        let running = lc.clone();
        let shutdown = tokio::spawn(async move { running.run_shutdown().await });
        tokio::time::sleep(Duration::from_secs(4)).await;
        lc.begin_drain(DrainReason::Fatal("state".into()));
        let report = shutdown.await.unwrap();
        assert_eq!(started.elapsed(), SESSION_END_BUDGET);
        assert_eq!(report.reason, DrainReason::Fatal("state".into()));
        assert_eq!(report.exit_code(), 4);

        // The test parent goes; the session ends 1 s later: the earlier 5 s deadline holds.
        let lc = lifecycle(&FakeClock::new());
        let _stuck = lc.call_started();
        let started = tokio::time::Instant::now();
        lc.begin_drain(DrainReason::TestParentGone);
        let running = lc.clone();
        let shutdown = tokio::spawn(async move { running.run_shutdown().await });
        tokio::time::sleep(Duration::from_secs(1)).await;
        lc.begin_drain(DrainReason::SessionEnd);
        let report = shutdown.await.unwrap();
        assert_eq!(started.elapsed(), FATAL_BUDGET);
        assert_eq!(report.reason, DrainReason::SessionEnd);

        // A requested drain has no deadline: the session end's budget counts from the end.
        let lc = lifecycle(&FakeClock::new());
        let _stuck = lc.call_started();
        lc.begin_drain(DrainReason::Requested);
        let running = lc.clone();
        let shutdown = tokio::spawn(async move { running.run_shutdown().await });
        tokio::time::sleep(Duration::from_secs(3600)).await;
        let ended = tokio::time::Instant::now();
        lc.begin_drain(DrainReason::SessionEnd);
        shutdown.await.unwrap();
        assert_eq!(ended.elapsed(), SESSION_END_BUDGET);
    }

    /// A blocker that opens a connection the first time it is looked at: a client that
    /// connects while the idle check runs.
    struct ConnectsOnce(std::sync::Weak<Lifecycle>, AtomicBool);

    impl IdleBlocker for ConnectsOnce {
        fn name(&self) -> &'static str {
            "connects once"
        }
        fn busy(&self) -> bool {
            if !self.1.swap(true, Ordering::SeqCst)
                && let Some(lc) = self.0.upgrade()
            {
                lc.connection_opened();
            }
            false
        }
    }

    #[test] // spec: ARCH-020
    fn idle_drain_is_decided_under_the_lock() {
        let clock = FakeClock::new();
        let lc = lifecycle(&clock);
        clock.advance(Duration::from_secs(599));
        assert!(!lc.drain_if_idle(), "not idle long enough");
        clock.advance(Duration::from_secs(1));
        lc.add_blocker(Arc::new(ConnectsOnce(
            Arc::downgrade(&lc),
            AtomicBool::new(false),
        )));
        assert!(
            !lc.drain_if_idle(),
            "a client connected while the idle check ran"
        );
        assert!(!lc.is_draining());
        lc.connection_closed();
        clock.advance(Duration::from_secs(600));
        assert!(lc.drain_if_idle());
        assert_eq!(lc.drain_reason(), Some(DrainReason::Idle));
        assert!(!lc.drain_if_idle(), "already draining");
    }

    #[tokio::test(start_paused = true)] // spec: ARCH-021
    async fn session_end_time_boxes_a_requested_drain_already_waiting() {
        let lc = lifecycle(&FakeClock::new());
        let _stuck = lc.call_started();
        lc.begin_drain(DrainReason::Requested);
        let running = lc.clone();
        let mut shutdown = tokio::spawn(async move { running.run_shutdown().await });
        assert!(
            tokio::time::timeout(Duration::from_secs(3600), &mut shutdown)
                .await
                .is_err(),
            "a requested drain has no timeout"
        );
        // The session ends meanwhile: its budget counts from now.
        lc.begin_drain(DrainReason::SessionEnd);
        let ended = tokio::time::Instant::now();
        let report = shutdown.await.unwrap();
        assert_eq!(ended.elapsed(), SESSION_END_BUDGET);
        assert_eq!(report.reason, DrainReason::SessionEnd);
        assert_eq!(report.abandoned_calls, 1);
    }
}
