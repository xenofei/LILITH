//! The `Core` object against a real test-channel core (APP-024, APP-026–029, APP-139, ARCH-118,
//! QA-026). CTest runs it as `lil_qt_e2e` (label `qml`):
//! `cargo test --locked -p lil_qt --features qt --test e2e`, with `QMAKE` naming the Qt pair's
//! qmake and its `bin` on `PATH`.
//!
//! **The Qt test identity (ruling Q4).** A real test core grants `ui:*` only to an app it can
//! verify from the client process's image: `lil-image.exe`, or on a test channel
//! `lil-test-app.exe`, in the folder of the core's own executable (ARCH-045, ARCH-046). A
//! cargo test binary is `cli`, and `cli` can't answer a confirmation (`caller.not_allowed`).
//! So each test runs in two processes:
//! 1. **The launcher** (the test as cargo runs it) copies the test binaries' `lil-core.exe` and
//!    this test binary itself, renamed `lil-test-app.exe`, into a fresh folder, and runs that
//!    copy with `--exact <test> --test-threads=1` and `LIL_QT_E2E_CHILD=1`. It only checks the
//!    copy's exit code and shows its output.
//! 2. **The copy** runs the test's body: it makes its own `TestEnv`, starts the core from its
//!    own folder (`CoreFixture::start_exe`), so that core trusts the folder and takes the copy
//!    for `ui:test`, and starts its own Qt event loop on a thread (`QCoreApplication`, which is
//!    all `CxxQtThread::queue` needs) with a `Core` object connected as `ui:test`. The body
//!    drives `Core` through its invokables on that thread and watches its signals.
//!
//! Tasks 19, 21 and 22 can do the same for any Qt test that needs a real core and an app's
//! answer: [`launch`] and [`Probe`] are the whole route. Tests that need the notification path
//! call `test.desktop {handlers: true}` first (a test core starts with handlers missing).

#![cfg(feature = "qt")]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use cxx_qt::Threading as _;
use cxx_qt_lib::{QCoreApplication, QString};
use lil_client::{ClientConfig, StartPolicy};
use lil_qt::qt::core_object::{Core, new_core};
use lil_testkit::{CoreFixture, FakeComfy, TestEnv, ensure_built};
use serde_json::{Value, json};

/// Set in the copy's environment: run the test's body.
const CHILD: &str = "LIL_QT_E2E_CHILD";
/// A failure deadline for each wait, never a measurement.
const DEADLINE: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------------------------
// The two processes.
// ---------------------------------------------------------------------------------------------

/// Runs `body` in the copy (module docs); in the launcher, starts the copy and checks it.
fn launch(test: &str, body: impl FnOnce()) {
    if std::env::var_os(CHILD).is_some() {
        body();
        // The test passed. Its Qt event loop still runs on its own thread: ExitProcess ends
        // that thread before Qt's DLLs unload, where returning from main would run the exe's
        // static destructors under it.
        std::process::exit(0);
    }
    let folder = tempfile::Builder::new()
        .prefix("lil-qt-e2e-")
        .tempdir()
        .expect("a folder for the copies");
    let core = folder.path().join("lil-core.exe");
    let app = folder.path().join("lil-test-app.exe");
    std::fs::copy(&ensure_built().lil_core, &core).expect("copy lil-core.exe");
    std::fs::copy(std::env::current_exe().expect("this test's exe"), &app)
        .expect("copy this test as lil-test-app.exe");
    let out = folder.path().join("child.out");
    let err = folder.path().join("child.err");
    let mut child = Command::new(&app)
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(&out).expect("child.out"))
        .stderr(std::fs::File::create(&err).expect("child.err"))
        .spawn()
        .expect("start the copy");
    let deadline = Instant::now() + Duration::from_secs(300);
    let status = loop {
        if let Some(status) = child.try_wait().expect("the copy's status") {
            break Some(status);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let read = |p: &Path| std::fs::read_to_string(p).unwrap_or_default();
    let ok = status.is_some_and(|s| s.success());
    assert!(
        ok,
        "the copy running {test} ended {status:?}\n--- stdout\n{}\n--- stderr\n{}",
        read(&out),
        read(&err)
    );
    // The copy's core has drained by now; a scanner may still hold a copy for a moment.
    for _ in 0..50 {
        if std::fs::remove_dir_all(folder.path()).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The copy's own folder, where its core and itself live.
fn own_folder() -> PathBuf {
    std::env::current_exe()
        .expect("this exe")
        .parent()
        .expect("its folder")
        .to_path_buf()
}

/// A test channel with its core started from the copy's folder, and a `Core` connected to it
/// as `ui:test`, ready.
struct Rig {
    probe: Probe,
    _core: CoreFixture,
    env: TestEnv,
}

impl Rig {
    /// `prepare` runs before the core starts (`suite.toml`, a fake ComfyUI).
    fn start(prepare: impl FnOnce(&TestEnv)) -> Rig {
        let env = TestEnv::new();
        prepare(&env);
        let core = CoreFixture::start_exe(&env, &own_folder().join("lil-core.exe"));
        let mut cfg = ClientConfig::for_env(env.env.clone(), "ui:test".parse().unwrap())
            .expect("a client configuration");
        cfg.start = StartPolicy::NoStart;
        let mut probe = Probe::start(cfg);
        probe.wait_for(
            "the connection",
            |s| matches!(s, Seen::Connection(c) if c == "ready"),
        );
        Rig {
            probe,
            _core: core,
            env,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The Qt side: a Core on its own event loop.
// ---------------------------------------------------------------------------------------------

/// A signal of `Core`, as it was seen.
#[derive(Debug, Clone)]
enum Seen {
    Finished(String, Value),
    Failed(String, Value),
    Pending(String, Value),
    Event {
        topic: String,
        kind: String,
        seq: u64,
        data: Value,
        at: Instant,
    },
    Connection(String),
}

/// A `Core` on a Qt event loop of its own thread, driven through its invokables.
struct Probe {
    thread: cxx_qt::CxxQtThread<Core>,
    rx: mpsc::Receiver<Seen>,
    seen: Vec<Seen>,
}

fn parse(json: &QString) -> Value {
    serde_json::from_str(&json.to_string()).unwrap_or(Value::Null)
}

impl Probe {
    fn start(cfg: ClientConfig) -> Probe {
        lil_qt::qt::serve(cfg);
        let (thread_tx, thread_rx) = mpsc::channel();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut app = QCoreApplication::new();
            let mut core = new_core();
            let mut pinned = core.pin_mut();
            let t = tx.clone();
            pinned
                .as_mut()
                .on_call_finished(move |_, id, json| {
                    let _ = t.send(Seen::Finished(id.to_string(), parse(&json)));
                })
                .release();
            let t = tx.clone();
            pinned
                .as_mut()
                .on_call_failed(move |_, id, json| {
                    let _ = t.send(Seen::Failed(id.to_string(), parse(&json)));
                })
                .release();
            let t = tx.clone();
            pinned
                .as_mut()
                .on_call_pending_confirmation(move |_, id, json| {
                    let _ = t.send(Seen::Pending(id.to_string(), parse(&json)));
                })
                .release();
            let t = tx.clone();
            pinned
                .as_mut()
                .on_event(move |_, topic, kind, seq, json| {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let seq = seq as u64;
                    let _ = t.send(Seen::Event {
                        topic: topic.to_string(),
                        kind: kind.to_string(),
                        seq,
                        data: parse(&json),
                        at: Instant::now(),
                    });
                })
                .release();
            let t = tx;
            pinned
                .as_mut()
                .on_connection_changed(move |core| {
                    let _ = t.send(Seen::Connection(core.connection().to_string()));
                })
                .release();
            thread_tx.send(pinned.qt_thread()).expect("the test waits");
            app.pin_mut().exec();
            drop(core);
        });
        Probe {
            thread: thread_rx
                .recv_timeout(DEADLINE)
                .expect("the Qt thread starts"),
            rx,
            seen: Vec::new(),
        }
    }

    /// Runs `f` on the Qt thread and returns what it returned.
    fn on_gui<R: Send + 'static>(
        &self,
        f: impl FnOnce(core::pin::Pin<&mut Core>) -> R + Send + 'static,
    ) -> R {
        let (tx, rx) = mpsc::channel();
        self.thread
            .queue(move |core| {
                let _ = tx.send(f(core));
            })
            .expect("the Core is alive");
        rx.recv_timeout(DEADLINE).expect("the Qt thread answers")
    }

    fn call(&self, tool: &str, args: Value) -> String {
        let (tool, args) = (
            QString::from(tool),
            QString::from(args.to_string().as_str()),
        );
        self.on_gui(move |core| core.call(&tool, &args).to_string())
    }

    fn app_call(&self, method: &str, params: Value) -> String {
        let (method, params) = (
            QString::from(method),
            QString::from(params.to_string().as_str()),
        );
        self.on_gui(move |core| core.app_call(&method, &params).to_string())
    }

    fn subscribe(&self, topic: &str) {
        let topic = QString::from(topic);
        self.on_gui(move |core| core.subscribe(&topic));
    }

    fn last_seq(&self, topic: &str) -> f64 {
        let topic = QString::from(topic);
        self.on_gui(move |core| core.last_seq(&topic))
    }

    /// The first signal (seen before, or within [`DEADLINE`]) that `pred` accepts, taken.
    fn wait_for(&mut self, what: &str, pred: impl Fn(&Seen) -> bool) -> Seen {
        if let Some(i) = self.seen.iter().position(&pred) {
            return self.seen.remove(i);
        }
        let deadline = Instant::now() + DEADLINE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(s) if pred(&s) => return s,
                Ok(s) => self.seen.push(s),
                Err(_) => panic!("no {what} within {DEADLINE:?}; seen: {:#?}", self.seen),
            }
        }
    }

    /// Every signal that arrives within `within`, kept for later waits too.
    fn gather(&mut self, within: Duration) -> Vec<Seen> {
        let deadline = Instant::now() + within;
        while let Ok(s) = self
            .rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            self.seen.push(s);
        }
        self.seen.clone()
    }

    /// The outcome of request `id`.
    fn outcome(&mut self, id: &str) -> Seen {
        self.wait_for(&format!("outcome of {id}"), |s| match s {
            Seen::Finished(rid, _) | Seen::Failed(rid, _) | Seen::Pending(rid, _) => rid == id,
            _ => false,
        })
    }

    /// The result of request `id`, which must finish ok.
    fn finished(&mut self, id: &str) -> Value {
        match self.outcome(id) {
            Seen::Finished(_, v) => v,
            other => panic!("{id} didn't finish ok: {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The tests.
// ---------------------------------------------------------------------------------------------

/// `Core.call` returns a request id at once and its result arrives as `callFinished` for that
/// id, on the GUI thread; the connection properties follow the session.
#[test] // spec: APP-024, APP-026, APP-028
fn call_finishes_through_the_core_object() {
    launch("call_finishes_through_the_core_object", || {
        let mut rig = Rig::start(|_| {});
        let props = rig.probe.on_gui(|core| {
            (
                core.connection().to_string(),
                core.channel_scheme().to_string(),
                core.core_protocol().to_string(),
            )
        });
        assert_eq!(props.0, "ready");
        assert_eq!(props.1, rig.env.env.activation_scheme());
        assert_eq!(props.2, lil_protocol::PROTOCOL.to_string());
        let id = rig.probe.call("test.echo", json!({"text": "hi"}));
        assert!(id.starts_with("rq_"), "{id}");
        let result = rig.probe.finished(&id);
        assert_eq!(result["structured"]["text"], "hi", "{result}");
    });
}

/// A monthly cap of 1 USD: a `test.destructive` call costing 5 asks about the cap.
const MONTHLY_1: &str = "[money]\ndisplay_currency = \"USD\"\n[caps]\nmonthly = \"1\"\n";

/// APP-029: the app's own call that passes a cap gets `pending_confirmation`; the request stays
/// open; once the app answers (here through `Core.appCall`, as the shared dialog does), `Core`
/// sends the call again once with its retry token, and the same request finishes with the
/// result. The call ran once.
#[test] // spec: APP-029, APP-054, CORE-091
fn a_cap_pending_call_retries_after_an_app_answer() {
    launch("a_cap_pending_call_retries_after_an_app_answer", || {
        let mut rig = Rig::start(|env| env.suite_toml_set(MONTHLY_1));
        let id = rig.probe.call("test.destructive", json!({"cost": "5"}));
        let pending = match rig.probe.outcome(&id) {
            Seen::Pending(_, p) => p,
            other => panic!("expected a pending confirmation, got {other:?}"),
        };
        assert_eq!(pending["covers"], json!(["cap"]), "{pending}");
        assert!(
            pending["retry_token"]
                .as_str()
                .is_some_and(|t| t.starts_with("ik_")),
            "the call's own key: {pending}"
        );
        let answer = rig.probe.app_call(
            "app/confirmation.answer",
            json!({"confirmation_id": pending["confirmation_id"], "answer": "approve",
                   "cap": {"action": "allow_once"}}),
        );
        rig.probe.finished(&answer);
        let result = rig.probe.finished(&id);
        assert_eq!(result["structured"]["count"], 1, "it ran once: {result}");
        // Nothing more for the request.
        let later = rig.probe.gather(Duration::from_millis(500));
        assert!(
            !later.iter().any(|s| matches!(s,
                Seen::Finished(rid, _) | Seen::Failed(rid, _) | Seen::Pending(rid, _) if *rid == id)),
            "{later:#?}"
        );
    });
}

/// APP-027, APP-139: a job's progress reaches QML leading-edge, then at most 10 times a second,
/// and the last value it shows is the newest. A burst of 300 steps from the fake ComfyUI comes
/// through as a handful of updates, 100 ms or more apart, the last one at 300 of 300.
#[test] // spec: APP-027, APP-139
fn progress_events_arrive_at_most_10_per_second() {
    launch("progress_events_arrive_at_most_10_per_second", || {
        let fake = FakeComfy::start();
        fake.hold_prompts(true);
        let mut rig = Rig::start(|env| {
            env.suite_toml_set(&format!(
                "[engines.comfyui]\nport = {}\nroot = '{}'\nautostart = false\n",
                fake.port(),
                fake.root().display().to_string().replace('\'', "''")
            ));
        });
        let submit = |probe: &mut Probe, graph: Value| -> String {
            let id = probe.call("test.comfy_submit", json!({"graph": graph}));
            let result = probe.finished(&id);
            result["structured"]["job"]["job_id"]
                .as_str()
                .unwrap_or_else(|| panic!("no job id: {result}"))
                .to_string()
        };
        // A held first job keeps the second one queued in the suite while it is subscribed.
        submit(&mut rig.probe, json!("tiny"));
        let first_prompt = poll("the first prompt in ComfyUI", || {
            posted_prompts(&fake).first()?["prompt_id"]
                .as_str()
                .map(str::to_string)
        });
        let steps = 300;
        let job = submit(
            &mut rig.probe,
            json!({
                "3": {"class_type": "KSampler", "inputs": {"steps": steps, "seed": 1}},
                "5": {"class_type": "SaveImage", "inputs": {"filename_prefix": "ComfyUI"}},
            }),
        );
        let topic = format!("job:{job}");
        rig.probe.subscribe(&topic);
        poll("the job topic's subscription", || {
            (rig.probe.last_seq(&topic) >= 0.0).then_some(())
        });
        fake.release(&first_prompt);

        let progress = |s: &Seen| {
            matches!(s, Seen::Event { topic: t, data, .. }
                if *t == topic && data["kind"] == "progress")
        };
        let at_max = |s: &Seen| {
            matches!(s, Seen::Event { topic: t, data, .. }
                if *t == topic && data["kind"] == "progress" && data["value"] == steps)
        };
        let Seen::Event { at: last_at, .. } =
            rig.probe.wait_for("the last step's progress", at_max)
        else {
            unreachable!()
        };
        let seen = rig.probe.gather(Duration::from_millis(400));
        let mut arrivals: Vec<(Instant, u64)> = seen
            .iter()
            .filter(|s| progress(s))
            .map(|s| match s {
                Seen::Event { at, data, .. } => (*at, data["value"].as_u64().unwrap_or(0)),
                _ => unreachable!(),
            })
            .collect();
        // The last step's update, taken by the wait above.
        arrivals.push((last_at, steps));
        arrivals.sort_by_key(|(at, _)| *at);
        assert!(
            arrivals.len() < usize::try_from(steps).unwrap() / 10,
            "{} updates for {steps} steps: {arrivals:?}",
            arrivals.len()
        );
        for pair in arrivals.windows(2) {
            let gap = pair[1].0.duration_since(pair[0].0);
            // 100 ms apart where they leave the throttle; the GUI thread's delivery jitters.
            assert!(
                gap >= Duration::from_millis(60),
                "two updates {gap:?} apart: {arrivals:?}"
            );
        }
        assert!(
            arrivals.iter().filter(|(_, v)| *v == steps).count() == 1,
            "the newest value once: {arrivals:?}"
        );
        fake.hold_prompts(false);
    });
}

/// ARCH-118: the trace id the app sends with a tool call (`tr_<16 hex>`, minted by `Core`)
/// comes back in the call's error and is on the core's log line for that call.
#[test] // spec: ARCH-118
fn trace_ids_reach_the_core_log() {
    launch("trace_ids_reach_the_core_log", || {
        let mut rig = Rig::start(|_| {});
        let id = rig
            .probe
            .call("test.write", json!({"label": "traced", "fail": true}));
        let error = match rig.probe.outcome(&id) {
            Seen::Failed(_, e) => e,
            other => panic!("expected the failure, got {other:?}"),
        };
        let trace = error["trace_id"].as_str().unwrap_or_default().to_string();
        assert!(
            trace.len() == 19
                && trace.starts_with("tr_")
                && trace[3..].bytes().all(|b| b.is_ascii_hexdigit()),
            "the app's own trace id: {error}"
        );
        let logs = rig.env.env.logs_dir();
        let line = poll("the core's log line with the trace id", || {
            core_log_lines(&logs).into_iter().find(|l| {
                l.contains(&trace) && l.contains("test.write") && l.contains("tool call failed")
            })
        });
        let line: Value = serde_json::from_str(&line).expect("a JSON log line");
        assert_eq!(
            line["fields"]["trace_id"], trace,
            "the line's own trace_id: {line}"
        );
    });
}

/// The session's `note_live` wiring (14b re-review 3, M1): a live-only subscribe sets the
/// topic's last seq from the core's answer, so a topic with history starts at its current seq:
/// the events published before the subscribe never come, the next one does.
#[test] // spec: ARCH-100, APP-033
fn a_live_subscribe_starts_at_the_topics_seq() {
    launch("a_live_subscribe_starts_at_the_topics_seq", || {
        let mut rig = Rig::start(|_| {});
        for n in 1..=3 {
            let id = rig
                .probe
                .call("test.emit", json!({"topic": "live", "data": {"n": n}}));
            rig.probe.finished(&id);
        }
        assert!(rig.probe.last_seq("test:live") < 0.0, "not subscribed yet");
        rig.probe.subscribe("test:live");
        poll("the subscribe's answer", || {
            (rig.probe.last_seq("test:live") >= 0.0).then_some(())
        });
        assert!(
            (rig.probe.last_seq("test:live") - 3.0).abs() < f64::EPSILON,
            "the answer's seq"
        );
        let id = rig
            .probe
            .call("test.emit", json!({"topic": "live", "data": {"n": 4}}));
        rig.probe.finished(&id);
        let ev = rig.probe.wait_for(
            "the live event",
            |s| matches!(s, Seen::Event { topic, .. } if topic == "test:live"),
        );
        let Seen::Event {
            seq, data, kind, ..
        } = ev
        else {
            unreachable!()
        };
        assert_eq!(
            (seq, kind.as_str(), data["n"].as_u64()),
            (4, "event", Some(4))
        );
    });
}

/// P4a-R34: `Core.setWorkspace` says a new hello naming the workspace; its result is that hello,
/// the `workspace` property follows, and a workspace the core doesn't serve fails with the
/// core's own code, `workspace.unsupported`, leaving the session where it was.
#[test] // spec: ARCH-030, ARCH-090
fn set_workspace_switches_or_fails_with_the_cores_code() {
    launch(
        "set_workspace_switches_or_fails_with_the_cores_code",
        || {
            let (mut served, mut other) = (PathBuf::new(), PathBuf::new());
            let mut rig = Rig::start(|env| {
                served = env.make_workspace("served");
                other = env.make_workspace("other");
                env.register_workspace(&served, true);
                env.register_workspace(&other, false);
            });
            let served_id = rig.env.workspace_id(&served);
            let ws = QString::from(served_id.as_str());
            let id = rig
                .probe
                .on_gui(move |core| core.set_workspace(&ws).to_string());
            let hello = rig.probe.finished(&id);
            assert_eq!(hello["workspace"]["id"], served_id.as_str(), "{hello}");
            poll("the workspace property", || {
                let shown = rig.probe.on_gui(|core| {
                    core.workspace()
                        .value::<cxx_qt_lib::QMap<cxx_qt_lib::QMapPair_QString_QVariant>>()
                        .and_then(|m| m.get(&QString::from("id")))
                        .and_then(|v| v.value::<QString>())
                        .map(|s| s.to_string())
                });
                (shown.as_deref() == Some(served_id.as_str())).then_some(())
            });

            let ws = QString::from(rig.env.workspace_id(&other).as_str());
            let id = rig
                .probe
                .on_gui(move |core| core.set_workspace(&ws).to_string());
            match rig.probe.outcome(&id) {
                Seen::Failed(_, error) => {
                    assert_eq!(error["code"], "workspace.unsupported", "{error}");
                }
                other => panic!("expected workspace.unsupported, got {other:?}"),
            }
            let still = rig.probe.call("test.echo", json!({"text": "still here"}));
            assert_eq!(
                rig.probe.finished(&still)["structured"]["text"],
                "still here"
            );
        },
    );
}

// ---------------------------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------------------------

/// Polls `f` every 20 ms until it gives a value, for at most [`DEADLINE`].
fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "no {what} within {DEADLINE:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The bodies of the prompts the suite posted to the fake ComfyUI, in order.
fn posted_prompts(fake: &FakeComfy) -> Vec<Value> {
    fake.requests()
        .into_iter()
        .filter(|r| r.method == "POST" && r.path.trim_start_matches("/api") == "/prompt")
        .map(|r| r.body.unwrap_or(Value::Null))
        .collect()
}

/// Every line of the core's log files in `logs`.
fn core_log_lines(logs: &Path) -> Vec<String> {
    let Ok(dir) = std::fs::read_dir(logs) else {
        return Vec::new();
    };
    dir.filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with("lil-core."))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .flat_map(|text| text.lines().map(str::to_string).collect::<Vec<_>>())
        .collect()
}
