//! Desktop state in process (ARCH-127, CORE-160, APP-146; Appendix G "Fullscreen",
//! "Keep-awake" and "Notifications"; rulings P4a-R5 and P4a-R6): the 2 s fullscreen poll and
//! its two-poll leave, the opt-in auto-pause that adds and removes only its own reason,
//! keep-awake held only while a `gpu-heavy` job runs, the sleep log, and `desktop.get`'s
//! notification path. Everything goes through `FakeDesktop` and `FakeClock` (QA-038, QA-066);
//! nothing touches the real desktop.
//!
//! The tests run on a paused runtime: tokio's clock moves only when a test advances it, so the
//! poll's 2 s is exact.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lil_core::desktop::{
    DesktopSettingsStore, DesktopWatch, POLL_EVERY, RELEASE_RETRY_EVERY, start_autopause,
    start_keep_awake,
};
use lil_core::events::EventBus;
use lil_core::jobs::{
    JobCaller, JobCtx, JobDone, JobFailure, JobFuture, JobHandler, JobId, JobOrigin, JobSpec,
    JobStatus, Jobs, Lane, PauseReason, Priority, SubmittedSlot,
};
use lil_core::state::StateRegistry;
use lil_core::store::FailPoints;
use lil_core::workspace::WorkspaceHub;
use lil_platform::desktop::{Fullscreen, HandlerState, ShortcutState, ToastsState};
use lil_platform::desktop_fake::FakeDesktop;
use lil_platform::fs::SystemProbe;
use lil_platform::session_end::PowerEvent;
use lil_platform::{Clock, IdSource};
use lil_protocol::CallerKind;
use lil_protocol::app::{DesktopSetParams, HandlersWire, ShortcutWire, ToastsWire};
use lil_testkit::{FakeClock, SeqIds, TestEnv};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

// ---------------------------------------------------------------------------------------------
// The fixture.
// ---------------------------------------------------------------------------------------------

/// Runs jobs of kind `t.desk`: each waits until its gate (its own job id) opens.
#[derive(Default)]
struct Gated {
    open: watch::Sender<Vec<String>>,
}

impl Gated {
    fn open(&self, id: &str) {
        self.open.send_modify(|o| o.push(id.to_string()));
    }
}

impl JobHandler for Gated {
    fn kind(&self) -> &'static str {
        "t.desk"
    }

    fn run<'a>(&'a self, job: &'a JobCtx) -> JobFuture<'a, Result<JobDone, JobFailure>> {
        Box::pin(async move {
            let id = job.id.to_string();
            let mut open = self.open.subscribe();
            let _ = open.wait_for(|o| o.contains(&id)).await;
            Ok(JobDone {
                structured: json!({}),
                artifacts: Vec::new(),
                record: None,
                windows: Vec::new(),
            })
        })
    }

    fn cancel_in_engine<'a>(&'a self, _job: &'a JobCtx) -> JobFuture<'a, ()> {
        Box::pin(std::future::ready(()))
    }
}

struct Fx {
    fake: Arc<FakeDesktop>,
    clock: Arc<FakeClock>,
    jobs: Arc<Jobs>,
    handler: Arc<Gated>,
    store: Arc<DesktopSettingsStore>,
    watch: Arc<DesktopWatch>,
    rx: Mutex<mpsc::Receiver<String>>,
    _states: Arc<StateRegistry>,
    env: TestEnv,
}

impl Fx {
    /// The desktop state of a core with `suite_toml` (none: no file), its watch polling every
    /// [`POLL_EVERY`], and the `desktop` topic subscribed.
    async fn new(suite_toml: Option<&str>) -> Fx {
        let env = TestEnv::new();
        if let Some(text) = suite_toml {
            lil_testkit::files::write_file(&env.env.suite_toml(), text.as_bytes());
        }
        let states = Arc::new(
            StateRegistry::open(&env.env, Arc::new(|what: &str| panic!("fatal: {what}"))).unwrap(),
        );
        let clock = FakeClock::new();
        let dyn_clock: Arc<dyn Clock> = clock.clone();
        let events = Arc::new(EventBus::new(dyn_clock.clone()));
        let ids: Arc<dyn IdSource> = Arc::new(SeqIds::new());
        let failpoints = FailPoints::none();
        let hub = WorkspaceHub::new(
            env.env.clone(),
            states.clone(),
            events.clone(),
            dyn_clock.clone(),
            ids.clone(),
            failpoints.clone(),
            Arc::new(SystemProbe),
            lil_core::watch::WatchConfig::default(),
        );
        let jobs = Jobs::new(
            states.clone(),
            hub,
            events.clone(),
            dyn_clock.clone(),
            ids.clone(),
            failpoints.clone(),
        );
        let handler = Arc::new(Gated::default());
        jobs.register(handler.clone());
        let fake = Arc::new(FakeDesktop::new());
        let store = DesktopSettingsStore::load(
            env.env.clone(),
            fake.clone(),
            env.root().join("lil-image.exe"),
            dyn_clock.clone(),
            ids,
            events.clone(),
            failpoints,
        );
        let (tx, rx) = mpsc::channel(1024);
        events.attach(1, tx);
        events
            .subscribe(1, &["desktop".to_string()], &BTreeMap::new())
            .await;
        let watch = DesktopWatch::start(fake.clone(), dyn_clock, POLL_EVERY, events.clone());
        settle().await;
        Fx {
            fake,
            clock,
            jobs,
            handler,
            store,
            watch,
            rx: Mutex::new(rx),
            _states: states,
            env,
        }
    }

    fn caller(&self) -> JobCaller {
        JobCaller {
            idem_scope: "cli|none".into(),
            caller: CallerKind::Cli,
            workspace: None,
            trace_id: "t_desktop".into(),
            idem_key: None,
            call_id: "c_0000000000000001".into(),
            reservation: None,
            submitted: Arc::new(SubmittedSlot::new()),
            origin: Some(JobOrigin {
                tool: "t.submit".into(),
                arguments: json!({}),
            }),
            retry_of: None,
        }
    }

    /// Submits a gated `t.desk` job in `lane`.
    async fn submit(&self, lane: Lane) -> String {
        let spec = JobSpec {
            kind: "t.desk",
            lane,
            priority: Priority::User,
            module: "test".to_string(),
            args: json!({}),
            project: None,
            asset_id: None,
            doc_id: None,
            expected_secs: 10,
            expected_output_bytes: 0,
            vram_need_mib: None,
            engine: None,
            group: None,
            external_side_effects: false,
        };
        let job = self.jobs.submit(&self.caller(), spec).await.unwrap();
        job.job_id.clone()
    }

    async fn job(&self, id: &str) -> lil_core::jobs::JobSummary {
        self.jobs
            .get(&self.caller(), &JobId(id.to_string()))
            .await
            .unwrap()
    }

    /// Waits (no timer) until job `id` passes `f`.
    async fn until_job(
        &self,
        id: &str,
        f: impl Fn(&lil_core::jobs::JobSummary) -> bool,
    ) -> lil_core::jobs::JobSummary {
        let mut changes = self.jobs.changes();
        loop {
            let job = self.job(id).await;
            if f(&job) {
                return job;
            }
            changes.changed().await.unwrap();
        }
    }

    /// Moves tokio's paused clock by `d` and lets every task run.
    async fn advance(&self, d: Duration) {
        tokio::time::advance(d).await;
        settle().await;
    }

    /// One more poll.
    async fn poll(&self) {
        self.advance(POLL_EVERY).await;
    }

    /// The `desktop` events published so far, oldest first, as their `data`.
    async fn events(&self) -> Vec<Value> {
        settle().await;
        let mut rx = self.rx.lock().unwrap();
        let mut out = Vec::new();
        while let Ok(line) = rx.try_recv() {
            let event: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(event["params"]["topic"], "desktop", "{event}");
            out.push(event["params"]["data"].clone());
        }
        out
    }

    fn reasons(&self) -> Vec<String> {
        let mut r = self.jobs.queue_state(Lane::GpuHeavy).reasons;
        r.sort();
        r
    }

    async fn set(&self, p: Value) {
        let p: DesktopSetParams = serde_json::from_value(p).unwrap();
        self.store.set(p).await.unwrap();
        settle().await;
    }
}

/// Lets every ready task run.
async fn settle() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}

/// RFC 3339 UTC, to the second, as the core writes times.
fn utc_text(t: std::time::SystemTime) -> String {
    let secs = t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    lil_schema::time::rfc3339(i64::try_from(secs).unwrap())
}

fn running(job: &lil_core::jobs::JobSummary) -> bool {
    job.status == JobStatus::Running
}

fn finished(job: &lil_core::jobs::JobSummary) -> bool {
    job.status.is_final()
}

// ---------------------------------------------------------------------------------------------
// The fullscreen poll (P4a-R6).
// ---------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)] // spec: CORE-160, APP-146
async fn the_poll_runs_every_2_s() {
    assert_eq!(POLL_EVERY, Duration::from_secs(2), "P4a-R6");
    let fx = Fx::new(None).await;
    assert_eq!(fx.fake.polls(), 1, "the first poll is at the start");
    fx.advance(Duration::from_millis(1999)).await;
    assert_eq!(fx.fake.polls(), 1);
    fx.advance(Duration::from_millis(1)).await;
    assert_eq!(fx.fake.polls(), 2);
    for n in 3..=7 {
        fx.advance(Duration::from_secs(2)).await;
        assert_eq!(fx.fake.polls(), n);
    }
}

#[tokio::test(start_paused = true)] // spec: CORE-160, APP-146
async fn leaving_fullscreen_counts_after_two_polls() {
    let fx = Fx::new(None).await;
    let fullscreen = fx.watch.fullscreen();
    assert!(!*fullscreen.borrow());
    for state in [Fullscreen::D3d, Fullscreen::Busy, Fullscreen::Presentation] {
        fx.fake.set_fullscreen(state);
        fx.poll().await;
        assert!(*fullscreen.borrow(), "{state:?} is fullscreen at once");
    }
    // One poll that isn't fullscreen is an alt-tab; a fullscreen one in between starts again.
    fx.fake.set_fullscreen(Fullscreen::No);
    fx.poll().await;
    assert!(*fullscreen.borrow(), "one poll out isn't leaving");
    fx.fake.set_fullscreen(Fullscreen::Busy);
    fx.poll().await;
    fx.fake.set_fullscreen(Fullscreen::No);
    fx.poll().await;
    assert!(*fullscreen.borrow(), "the count starts again");
    fx.poll().await;
    assert!(!*fullscreen.borrow(), "two polls in a row out are leaving");
    // A failed call isn't fullscreen.
    fx.fake.set_fullscreen(Fullscreen::Unknown);
    fx.poll().await;
    assert!(!*fullscreen.borrow());

    let events = fx.events().await;
    assert_eq!(
        events,
        [
            json!({"kind": "fullscreen", "on": true}),
            json!({"kind": "fullscreen", "on": false})
        ],
        "one event per change"
    );
}

// ---------------------------------------------------------------------------------------------
// The auto-pause (CORE-160, P4a-R5).
// ---------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)] // spec: CORE-160
async fn autopause_is_off_by_default() {
    let fx = Fx::new(None).await;
    assert!(!fx.store.current().borrow().fullscreen_autopause);
    let _auto = start_autopause(&fx.watch, fx.jobs.clone(), fx.store.current());
    fx.fake.set_fullscreen(Fullscreen::D3d);
    fx.poll().await;
    assert!(*fx.watch.fullscreen().borrow());
    let lane = fx.jobs.queue_state(Lane::GpuHeavy);
    assert!(!lane.paused, "{lane:?}");
    let id = fx.submit(Lane::GpuHeavy).await;
    fx.until_job(&id, running).await;
}

#[tokio::test(start_paused = true)] // spec: CORE-160
async fn autopause_adds_and_removes_the_fullscreen_reason() {
    let fx = Fx::new(Some("[jobs]\nfullscreen_autopause = true\n")).await;
    let _auto = start_autopause(&fx.watch, fx.jobs.clone(), fx.store.current());
    settle().await;
    assert!(fx.reasons().is_empty());

    fx.fake.set_fullscreen(Fullscreen::D3d);
    fx.poll().await;
    assert_eq!(fx.reasons(), ["fullscreen"]);
    assert!(fx.jobs.queue_state(Lane::GpuHeavy).paused);
    // Only the gpu-heavy lane.
    assert!(!fx.jobs.queue_state(Lane::Cpu).paused);
    let id = fx.submit(Lane::GpuHeavy).await;
    let held = fx
        .until_job(&id, |j| j.substatus.as_deref() == Some("queue_paused"))
        .await;
    assert_eq!(held.pause_reasons, ["fullscreen"]);

    fx.fake.set_fullscreen(Fullscreen::No);
    fx.poll().await;
    assert_eq!(fx.reasons(), ["fullscreen"], "one poll out isn't leaving");
    fx.poll().await;
    assert!(fx.reasons().is_empty());
    fx.until_job(&id, running).await;
    fx.handler.open(&id);
    fx.until_job(&id, finished).await;

    // The setting follows at once: off during fullscreen removes the reason, on adds it.
    fx.fake.set_fullscreen(Fullscreen::Busy);
    fx.poll().await;
    assert_eq!(fx.reasons(), ["fullscreen"]);
    fx.set(json!({"fullscreen_autopause": false})).await;
    assert!(fx.reasons().is_empty());
    fx.set(json!({"fullscreen_autopause": true})).await;
    assert_eq!(fx.reasons(), ["fullscreen"]);
}

/// Review Focus 4: an automatic resume never undoes your own pause.
#[tokio::test(start_paused = true)] // spec: CORE-160
async fn fullscreen_reason_never_clears_a_manual_pause() {
    let fx = Fx::new(Some("[jobs]\nfullscreen_autopause = true\n")).await;
    let _auto = start_autopause(&fx.watch, fx.jobs.clone(), fx.store.current());
    fx.jobs.pause(Lane::GpuHeavy, PauseReason::Manual).await;
    let id = fx.submit(Lane::GpuHeavy).await;

    fx.fake.set_fullscreen(Fullscreen::D3d);
    fx.poll().await;
    assert_eq!(fx.reasons(), ["fullscreen", "manual"]);
    fx.fake.set_fullscreen(Fullscreen::No);
    fx.poll().await;
    fx.poll().await;
    assert_eq!(
        fx.reasons(),
        ["manual"],
        "leaving removes only its own reason"
    );
    let lane = fx.jobs.queue_state(Lane::GpuHeavy);
    assert!(lane.paused, "{lane:?}");
    let still = fx.job(&id).await;
    assert_eq!(still.substatus.as_deref(), Some("queue_paused"));
    assert_eq!(still.pause_reasons, ["manual"]);
    // Turning the auto-pause off doesn't touch it either.
    fx.set(json!({"fullscreen_autopause": false})).await;
    assert_eq!(fx.reasons(), ["manual"]);

    fx.jobs.resume(Lane::GpuHeavy, PauseReason::Manual).await;
    fx.until_job(&id, running).await;
}

#[tokio::test(start_paused = true)] // spec: CORE-160
async fn autopause_never_interrupts_the_executing_prompt() {
    let fx = Fx::new(Some("[jobs]\nfullscreen_autopause = true\n")).await;
    let _auto = start_autopause(&fx.watch, fx.jobs.clone(), fx.store.current());
    let first = fx.submit(Lane::GpuHeavy).await;
    let job = fx.until_job(&first, running).await;
    assert_eq!(job.substatus.as_deref(), Some("executing"));
    let second = fx.submit(Lane::GpuHeavy).await;

    fx.fake.set_fullscreen(Fullscreen::D3d);
    fx.poll().await;
    assert_eq!(fx.reasons(), ["fullscreen"]);
    let job = fx.job(&first).await;
    assert_eq!(job.status, JobStatus::Running, "{job:?}");
    assert_eq!(job.substatus.as_deref(), Some("executing"));
    fx.handler.open(&first);
    let done = fx.until_job(&first, finished).await;
    assert_eq!(done.status, JobStatus::Succeeded, "{done:?}");
    let held = fx
        .until_job(&second, |j| j.substatus.as_deref() == Some("queue_paused"))
        .await;
    assert_eq!(held.status, JobStatus::Queued);
}

// ---------------------------------------------------------------------------------------------
// Keep-awake (ARCH-127).
// ---------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)] // spec: ARCH-127
async fn keep_awake_is_held_only_while_a_gpu_heavy_job_runs() {
    let fx = Fx::new(None).await;
    let _awake = start_keep_awake(fx.fake.clone(), fx.jobs.clone(), fx.store.current());
    settle().await;
    assert!(!fx.fake.power_held(), "nothing runs");

    let gpu = fx.submit(Lane::GpuHeavy).await;
    fx.until_job(&gpu, running).await;
    settle().await;
    assert!(fx.fake.power_held(), "held at a gpu-heavy job's start");
    fx.handler.open(&gpu);
    fx.until_job(&gpu, finished).await;
    settle().await;
    assert!(!fx.fake.power_held(), "released at its end");

    let cpu = fx.submit(Lane::Cpu).await;
    fx.until_job(&cpu, running).await;
    settle().await;
    assert!(!fx.fake.power_held(), "never for a cpu job");
    fx.handler.open(&cpu);
    fx.until_job(&cpu, finished).await;
    settle().await;
    assert!(!fx.fake.power_held());
}

#[tokio::test(start_paused = true)] // spec: ARCH-127
async fn keep_awake_off_releases_at_once() {
    let fx = Fx::new(None).await;
    let _awake = start_keep_awake(fx.fake.clone(), fx.jobs.clone(), fx.store.current());
    let gpu = fx.submit(Lane::GpuHeavy).await;
    fx.until_job(&gpu, running).await;
    settle().await;
    assert!(fx.fake.power_held());

    fx.set(json!({"keep_awake_gpu": false})).await;
    assert!(!fx.fake.power_held(), "released while the job still runs");
    fx.set(json!({"keep_awake_gpu": true})).await;
    assert!(fx.fake.power_held(), "held again at once");
    fx.handler.open(&gpu);
    fx.until_job(&gpu, finished).await;
    settle().await;
    assert!(!fx.fake.power_held());
}

#[tokio::test(start_paused = true)] // spec: ARCH-127
async fn keep_awake_unavailable_is_reported() {
    let fx = Fx::new(None).await;
    assert!(fx.store.get(true).await.keep_awake_available);
    fx.fake.set_power_available(false);
    let got = fx.store.get(true).await;
    assert!(!got.keep_awake_available, "{got:?}");
    assert!(got.keep_awake_gpu, "the setting itself stays on");

    // A job still runs; nothing is held, and nothing breaks.
    let _awake = start_keep_awake(fx.fake.clone(), fx.jobs.clone(), fx.store.current());
    let gpu = fx.submit(Lane::GpuHeavy).await;
    fx.until_job(&gpu, running).await;
    settle().await;
    assert!(!fx.fake.power_held());
    fx.handler.open(&gpu);
    assert_eq!(
        fx.until_job(&gpu, finished).await.status,
        JobStatus::Succeeded
    );
}

/// Dropping keep-awake (the core's shutdown) releases a held request, and nothing holds it
/// afterwards.
#[tokio::test(start_paused = true)] // spec: ARCH-127
async fn dropping_keep_awake_releases_the_request() {
    let fx = Fx::new(None).await;
    let awake = start_keep_awake(fx.fake.clone(), fx.jobs.clone(), fx.store.current());
    let gpu = fx.submit(Lane::GpuHeavy).await;
    fx.until_job(&gpu, running).await;
    settle().await;
    assert!(fx.fake.power_held() && awake.held());

    drop(awake);
    assert!(!fx.fake.power_held(), "released while the job still runs");
    fx.set(json!({"keep_awake_gpu": true})).await;
    let second = fx.submit(Lane::GpuHeavy).await;
    fx.handler.open(&gpu);
    fx.until_job(&second, running).await;
    settle().await;
    assert!(!fx.fake.power_held(), "nothing holds it after the drop");
    fx.handler.open(&second);
    fx.until_job(&second, finished).await;
}

/// A release the PC refused is tried again soon, with nothing else happening.
#[tokio::test(start_paused = true)] // spec: ARCH-127
async fn a_failed_release_is_tried_again() {
    let fx = Fx::new(None).await;
    let _awake = start_keep_awake(fx.fake.clone(), fx.jobs.clone(), fx.store.current());
    let gpu = fx.submit(Lane::GpuHeavy).await;
    fx.until_job(&gpu, running).await;
    settle().await;
    assert!(fx.fake.power_held());

    fx.fake.set_power_available(false);
    fx.handler.open(&gpu);
    fx.until_job(&gpu, finished).await;
    settle().await;
    assert!(fx.fake.power_held(), "the release was refused");
    fx.fake.set_power_available(true);
    fx.advance(RELEASE_RETRY_EVERY).await;
    assert!(!fx.fake.power_held(), "released on the retry");
}

/// A log event: its message and its other fields, as `(name, value)` pairs.
type LogEvent = (String, Vec<(String, String)>);

/// Every log event.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<LogEvent>>>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Captured {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        #[derive(Default)]
        struct Fields(String, Vec<(String, String)>);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                } else {
                    self.1
                        .push((field.name().to_string(), format!("{value:?}")));
                }
            }
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        self.0.lock().unwrap().push((fields.0, fields.1));
    }
}

impl Captured {
    /// The process's only log subscriber (nextest runs each test in its own process).
    fn install() -> Captured {
        use tracing_subscriber::layer::SubscriberExt as _;
        let captured = Captured::default();
        tracing::subscriber::set_global_default(
            tracing_subscriber::registry().with(captured.clone()),
        )
        .expect("the only global subscriber of this test");
        captured
    }

    fn slept(&self) -> Vec<Vec<(String, String)>> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|(message, _)| message == "slept")
            .map(|(_, fields)| fields.clone())
            .collect()
    }
}

/// The value of the log field `name`, or empty.
fn field(fields: &[(String, String)], name: &str) -> String {
    fields
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
}

#[tokio::test(start_paused = true)] // spec: ARCH-127
async fn a_sleep_gap_is_logged_and_published() {
    let captured = Captured::install();
    let fx = Fx::new(None).await;
    fx.events().await;

    // The hidden window's broadcasts.
    let from = utc_text(fx.clock.utc_now());
    fx.watch.power_event(PowerEvent::Suspending);
    fx.clock.sleep(Duration::from_secs(3600));
    fx.watch.power_event(PowerEvent::Resumed);
    let at = utc_text(fx.clock.utc_now());
    let slept = captured.slept();
    assert_eq!(slept.len(), 1, "{slept:?}");
    assert!(field(&slept[0], "from").contains(&from), "{slept:?}");
    assert!(field(&slept[0], "to").contains(&at), "{slept:?}");
    assert_eq!(
        fx.events().await,
        [json!({"kind": "resumed", "slept_from": from, "at": at})]
    );

    // The backstop, the 30 s check: the time asleep grew by more than 60 s with no broadcast.
    // Since the broadcast reported the hour, the next check sees nothing new.
    fx.advance(Duration::from_secs(30)).await;
    assert_eq!(captured.slept().len(), 1, "the hour is reported once");
    fx.clock.sleep(Duration::from_secs(61));
    let to = fx.clock.utc_now();
    fx.advance(Duration::from_secs(30)).await;
    let slept = captured.slept();
    assert_eq!(slept.len(), 2, "{slept:?}");
    let events = fx.events().await;
    assert_eq!(
        events,
        [json!({"kind": "resumed",
                "slept_from": utc_text(to - Duration::from_secs(61)),
                "at": utc_text(to)})],
        "a sleep of the bias's growth, ending at the check"
    );
    // 60 s or less isn't a sleep, and a wall-clock step (NTP, or you) never is.
    fx.clock.sleep(Duration::from_secs(60));
    fx.watch.check_sleep();
    fx.clock.shift_utc(7200);
    fx.watch.check_sleep();
    fx.clock.shift_utc(-7200);
    fx.watch.check_sleep();
    assert_eq!(captured.slept().len(), 2);
    assert!(fx.events().await.is_empty());
}

/// A sleep that a broadcast and the backstop both see is reported once, whichever comes first.
#[tokio::test(start_paused = true)] // spec: ARCH-127
async fn a_sleep_seen_by_the_broadcast_and_the_backstop_is_reported_once() {
    let captured = Captured::install();
    let fx = Fx::new(None).await;
    fx.events().await;

    // The backstop first: the resume broadcast that comes after it is the same sleep.
    fx.watch.power_event(PowerEvent::Suspending);
    fx.clock.sleep(Duration::from_secs(600));
    fx.watch.check_sleep();
    fx.clock.advance(Duration::from_secs(5));
    fx.watch.power_event(PowerEvent::Resumed);
    assert_eq!(captured.slept().len(), 1, "{:?}", captured.slept());
    assert_eq!(fx.events().await.len(), 1);

    // The broadcast first: the next check sees no new time asleep.
    fx.clock.advance(Duration::from_secs(120));
    fx.watch.power_event(PowerEvent::Suspending);
    fx.clock.sleep(Duration::from_secs(600));
    fx.watch.power_event(PowerEvent::Resumed);
    fx.watch.check_sleep();
    fx.advance(Duration::from_secs(30)).await;
    assert_eq!(captured.slept().len(), 2, "{:?}", captured.slept());
    assert_eq!(fx.events().await.len(), 1);
}

/// A suspend whose resume never came doesn't start the next sleep: the newest one does.
#[tokio::test(start_paused = true)] // spec: ARCH-127
async fn a_stale_suspend_is_discarded() {
    let captured = Captured::install();
    let fx = Fx::new(None).await;
    fx.watch.power_event(PowerEvent::Suspending);
    fx.clock.advance(Duration::from_secs(86_400));
    let from = utc_text(fx.clock.utc_now());
    fx.watch.power_event(PowerEvent::Suspending);
    fx.clock.sleep(Duration::from_secs(120));
    fx.watch.power_event(PowerEvent::Resumed);
    let slept = captured.slept();
    assert_eq!(slept.len(), 1, "{slept:?}");
    assert!(field(&slept[0], "from").contains(&from), "{slept:?}");
}

// ---------------------------------------------------------------------------------------------
// `desktop.get`'s notification path (Appendix G "Ready").
// ---------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)] // spec: APP-146, APP-022
async fn desktop_get_reports_the_notification_path() {
    let fx = Fx::new(None).await;
    let got = fx.store.get(true).await;
    assert_eq!(got.notification_path.toasts, ToastsWire::Ready);
    assert_eq!(got.notification_path.toasts_reason, None);
    assert_eq!(got.notification_path.shortcut, ShortcutWire::Ok);
    assert_eq!(got.notification_path.handlers, HandlersWire::Ok);
    let categories: Vec<&str> = got.notifications.keys().map(String::as_str).collect();
    assert_eq!(
        categories,
        [
            "agent_activity",
            "cap_alert",
            "confirmation_waiting",
            "director_decision",
            "job_failed",
            "job_finished"
        ]
    );
    assert!(got.notifications.values().all(|on| *on));
    assert!(got.quiet_while_fullscreen && !got.fullscreen_autopause && got.keep_awake_gpu);

    fx.fake
        .set_toasts_state(ToastsState::Unavailable("disabled_for_user"));
    fx.fake.set_shortcut(ShortcutState::WrongClsid);
    fx.fake
        .set_handlers(HandlerState::OtherChannel(fx.env.root().join("other.exe")));
    // Within 10 s the cached path is served; `fresh` recomputes it.
    let cached = fx.store.get(false).await;
    assert_eq!(cached.notification_path.toasts, ToastsWire::Ready);
    let fresh = fx.store.get(true).await;
    let path = &fresh.notification_path;
    assert_eq!(path.toasts, ToastsWire::Unavailable);
    assert_eq!(path.toasts_reason.as_deref(), Some("disabled_for_user"));
    assert_eq!(path.shortcut, ShortcutWire::Wrong);
    assert_eq!(path.handlers, HandlersWire::OtherChannel);

    fx.fake.set_toasts_state(ToastsState::NotYetShown);
    fx.fake.set_shortcut(ShortcutState::Missing);
    fx.fake.set_handlers(HandlerState::Missing);
    fx.clock.advance(Duration::from_secs(9));
    assert_eq!(
        fx.store.get(false).await.notification_path.toasts,
        ToastsWire::Unavailable
    );
    fx.clock.advance(Duration::from_secs(1));
    let later = fx.store.get(false).await.notification_path;
    assert_eq!(
        later.toasts,
        ToastsWire::NotYetShown,
        "the cache lasts 10 s"
    );
    assert_eq!(later.toasts_reason, None);
    assert_eq!(later.shortcut, ShortcutWire::Missing);
    assert_eq!(later.handlers, HandlersWire::Missing);
}
