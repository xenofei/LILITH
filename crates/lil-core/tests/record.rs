//! The Record stage (CORE-015, CORE-020, CORE-025, CORE-026, CORE-048; ARCH-075): it runs after
//! the meter stage, commits the tool's plan in the write order, holds a sign-off's version
//! lock until its manifest edit, turns a failed commit into the store error (outputs stay as
//! listed orphans), marks unpinned dependents stale, and puts a hand-off block in every
//! recorded result. Driven through the dispatcher of an in-process core with the fake clock
//! and ids (QA-031); waits are failure deadlines only, never sleeps.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use lil_core::events::EventBus;
use lil_core::pipeline::RecordPlan;
use lil_core::session::Session;
use lil_core::state::StateRegistry;
use lil_core::store::FailPoints;
use lil_core::tools::{
    CallCtx, Dispatcher, MeterStage, PipelineHooks, RecordStage, SettleInput, TestGates,
    WorkspaceRecord, core_registry,
};
use lil_core::workspace::{WorkspaceHub, WorkspaceRef};
use lil_platform::fs::{Probe, SystemProbe};
use lil_platform::paths::folder_key;
use lil_platform::{Clock, IdSource};
use lil_protocol::{
    CallerKind, EffectClass, ErrorBody, Outcome, ToolCallParams, ToolDescriptor, ToolOutput,
};
use lil_testkit::files::write_file;
use lil_testkit::{FakeClock, SeqIds, TestEnv, sha256_bytes, version_content_hash_of};
use serde_json::{Value, json};

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);
const KITE: &str = "prop_kite";
const KITE_DIR: &str = "projects/sketchbook/assets/prop_kite";

fn key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("record-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

fn file(ws: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(ws.to_path_buf(), |p, c| p.join(c))
}

fn read_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn ws_ref(ws: &Path) -> WorkspaceRef {
    let canonical = std::fs::canonicalize(ws).expect("canonicalize");
    let text = canonical.to_string_lossy().into_owned();
    WorkspaceRef {
        id: lil_platform::Env::workspace_hash(&canonical),
        path: PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text)),
    }
}

/// What the stages saw, in order.
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    fn push(&self, s: impl Into<String>) {
        self.0.lock().unwrap().push(s.into());
    }

    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

struct Fx {
    dispatcher: Dispatcher,
    hub: Arc<WorkspaceHub>,
    events: Arc<EventBus>,
    ws_ref: WorkspaceRef,
    ws: PathBuf,
    clock: Arc<FakeClock>,
    _states: Arc<StateRegistry>,
    /// Last: fields drop in order, and the folder cannot go while the dispatcher holds
    /// state.db open.
    env: TestEnv,
}

impl Fx {
    /// Closes the hub (each open workspace's watcher, index, `state.db` and `core.lock`) and
    /// joins the state actors, then removes the test's folder, which nothing holds open any
    /// more, and fails the test if anything in it was left behind.
    async fn shutdown(self) {
        let Fx {
            dispatcher,
            hub,
            _states: states,
            env,
            ..
        } = self;
        hub.close_all().await;
        states.close();
        drop((dispatcher, hub, states));
        if let Err(left) = env.close() {
            panic!("the test's folder was left behind: {left:?}");
        }
    }

    fn new(
        setup: impl FnOnce(&Path),
        hooks: impl FnOnce(PipelineHooks) -> PipelineHooks,
        failpoints: Arc<FailPoints>,
    ) -> Fx {
        Fx::with_probe(setup, hooks, failpoints, |_| Arc::new(SystemProbe))
    }

    /// [`Fx::new`], with the probe `probe` makes for the fixture workspace's folder.
    fn with_probe(
        setup: impl FnOnce(&Path),
        hooks: impl FnOnce(PipelineHooks) -> PipelineHooks,
        failpoints: Arc<FailPoints>,
        probe: impl FnOnce(&Path) -> Arc<dyn Probe>,
    ) -> Fx {
        let env = TestEnv::new();
        let ws = env.copy_fixture_workspace("record");
        setup(&ws);
        env.register_workspace(&ws, true);
        let states = Arc::new(
            StateRegistry::open(
                &env.env,
                Arc::new(|what: &str| panic!("unexpected fatal report: {what}")),
            )
            .expect("the suite state.db"),
        );
        let fake = FakeClock::new();
        let clock: Arc<dyn Clock> = fake.clone();
        let events = Arc::new(EventBus::new(clock.clone()));
        let ids: Arc<dyn IdSource> = Arc::new(SeqIds::new());
        let probe: Arc<dyn Probe> = probe(&ws);
        let hub = WorkspaceHub::new(
            env.env.clone(),
            states.clone(),
            events.clone(),
            clock.clone(),
            ids.clone(),
            failpoints,
            probe.clone(),
            lil_core::watch::WatchConfig::default(),
        );
        let gates = Arc::new(TestGates::new());
        let reg = core_registry(
            true,
            &gates,
            &states,
            &hub,
            &env.env,
            &probe,
            Arc::default(),
            &lil_core::tools::AssetServices::unwired(),
        );
        let lifecycle =
            lil_core::Lifecycle::new(clock.clone(), lil_core::lifecycle::IDLE_EXIT_AFTER);
        let dispatcher = Dispatcher::new(
            reg,
            hooks(PipelineHooks::plan2()),
            states.clone(),
            events.clone(),
            hub.clone(),
            ids,
            clock,
            lifecycle,
        );
        Fx {
            dispatcher,
            hub,
            events,
            ws_ref: ws_ref(&ws),
            ws,
            clock: fake,
            _states: states,
            env,
        }
    }

    fn plain() -> Fx {
        Fx::new(|_| {}, |h| h, FailPoints::none())
    }

    fn call(&self, name: &str, args: Value) -> impl Future<Output = Outcome<ToolOutput>> + use<> {
        self.call_keyed(name, args, key())
    }

    /// A call with the idempotency key `idem` for a write tool (a retry passes the same one).
    fn call_keyed(
        &self,
        name: &str,
        args: Value,
        idem: String,
    ) -> impl Future<Output = Outcome<ToolOutput>> + use<> {
        self.call_by(CallerKind::Cli, name, args, idem)
    }

    /// A call from a session of `caller` (with every capability of this protocol), with the
    /// idempotency key `idem` for a write tool.
    fn call_by(
        &self,
        caller: CallerKind,
        name: &str,
        args: Value,
        idem: String,
    ) -> impl Future<Output = Outcome<ToolOutput>> + use<> {
        let Value::Object(arguments) = args else {
            panic!("arguments must be an object");
        };
        let write = !matches!(
            self.dispatcher
                .registry()
                .get(name)
                .map(|t| t.descriptor().effect),
            Some(EffectClass::Read)
        );
        let session = self.session(caller);
        let params = ToolCallParams {
            name: name.into(),
            arguments,
            idempotency_key: write.then_some(idem),
            trace_id: None,
        };
        let dispatcher = self.dispatcher.clone();
        async move { dispatcher.call(session, params).await }
    }

    fn session(&self, caller: CallerKind) -> Arc<Session> {
        let capabilities = match caller {
            CallerKind::Ui(_) => lil_protocol::CAPABILITIES
                .iter()
                .map(|c| (*c).to_string())
                .collect(),
            _ => Vec::new(),
        };
        Arc::new(Session {
            id: "s_record".into(),
            caller,
            non_interactive: false,
            workspace: Some(self.ws_ref.clone()),
            capabilities,
            client_pid: 1,
            profile_id: None,
        })
    }

    async fn ok(&self, name: &str, args: Value) -> Value {
        match self.call(name, args.clone()).await {
            Outcome::Ok(out) => out.structured,
            other => panic!("{name} {args}: {other:?}"),
        }
    }

    async fn err(&self, name: &str, args: Value) -> ErrorBody {
        match self.call(name, args.clone()).await {
            Outcome::Error { error } => error,
            other => panic!("{name} {args} should fail: {other:?}"),
        }
    }

    fn outside(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.env.root().join("import-src").join(name);
        write_file(&path, bytes);
        path
    }

    fn import_args(&self, asset: &str, name: &str, bytes: &[u8]) -> Value {
        json!({"project": "sketchbook", "asset_id": asset, "type": "image",
               "files": [self.outside(name, bytes).to_string_lossy()], "mode": "copy"})
    }

    async fn import_run(&self, asset: &str, name: &str, bytes: &[u8]) -> String {
        let r = self
            .ok("asset.import", self.import_args(asset, name, bytes))
            .await;
        r["run_id"].as_str().expect("a run id").to_string()
    }

    fn signoff_args(asset: &str, run: &str, name: &str, extra: Value) -> Value {
        let mut args = json!({"project": "sketchbook", "asset_id": asset, "run_id": run,
                              "files": [name]});
        for (k, v) in extra.as_object().unwrap() {
            args[k] = v.clone();
        }
        args
    }

    fn path(&self, rel: &str) -> PathBuf {
        file(&self.ws, rel)
    }

    fn manifest(&self, asset: &str) -> Value {
        let p = self.path(&format!(
            "projects/sketchbook/assets/{asset}/{asset}.manifest.json"
        ));
        serde_json::from_str(&read_text(&p)).unwrap()
    }
}

/// Logs the meter stage with what the workspace holds at that moment.
struct LoggingMeter {
    log: Log,
    ws: Arc<Mutex<PathBuf>>,
}

impl MeterStage for LoggingMeter {
    fn settle<'a>(&'a self, _i: SettleInput<'a>) -> futures_util::future::BoxFuture<'a, ()> {
        let ws = self.ws.lock().unwrap().clone();
        let manifest = file(&ws, &format!("{KITE_DIR}/{KITE}.manifest.json"));
        let staging = file(&ws, ".lilith/staging");
        let staged = std::fs::read_dir(&staging)
            .map(|d| d.flatten().any(|h| h.path().join("kite.png").is_file()))
            .unwrap_or(false);
        self.log.push(format!(
            "meter manifest={} staged={staged}",
            manifest.exists()
        ));
        Box::pin(std::future::ready(()))
    }
}

/// Logs the Record stage around the workspace one.
struct LoggingRecord {
    log: Log,
}

impl RecordStage for LoggingRecord {
    fn record<'a>(
        &'a self,
        ctx: &'a CallCtx,
        d: &'a ToolDescriptor,
        plan: Option<RecordPlan>,
        out: &'a mut Outcome<ToolOutput>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            self.log.push(format!("record plan={}", plan.is_some()));
            WorkspaceRecord.record(ctx, d, plan, out).await;
            self.log.push("record done");
        })
    }
}

/// The `lil/event` lines a client attached with `rx` got, parsed, without waiting.
fn drain_events(rx: &mut tokio::sync::mpsc::Receiver<String>) -> Vec<Value> {
    let mut out = Vec::new();
    while let Ok(line) = rx.try_recv() {
        let v: Value = serde_json::from_str(&line).unwrap();
        out.push(v["params"].clone());
    }
    out
}

#[tokio::test] // spec: CORE-015, CORE-020, CORE-025
async fn record_runs_after_meter_and_applies_the_plan_in_write_order() {
    let log = Log::default();
    let ws_slot = Arc::new(Mutex::new(PathBuf::new()));
    let (meter_log, record_log, slot) = (log.clone(), log.clone(), ws_slot.clone());
    let fx = Fx::new(
        |_| {},
        move |h| PipelineHooks {
            meter: Arc::new(LoggingMeter {
                log: meter_log,
                ws: slot,
            }),
            record: Arc::new(LoggingRecord { log: record_log }),
            ..h
        },
        FailPoints::none(),
    );
    *ws_slot.lock().unwrap() = fx.ws.clone();
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    fx.events.attach(7, tx);
    let topic = format!("asset:lil://{}/sketchbook/{KITE}", fx.ws_ref.id);
    fx.events
        .subscribe(
            7,
            &[topic.clone(), "project:sketchbook".to_string()],
            &Default::default(),
        )
        .await;

    let r = fx
        .ok("asset.import", fx.import_args(KITE, "kite.png", b"kite"))
        .await;
    let run = r["run_id"].as_str().unwrap().to_string();
    // Ingest (Execute staged the file), the empty repair and QA stages, then the meter stage
    // with nothing committed yet, then the Record stage, which commits.
    assert_eq!(
        log.take(),
        [
            "meter manifest=false staged=true",
            "record plan=true",
            "record done"
        ]
    );
    assert!(
        fx.path(&format!("{KITE_DIR}/candidates/{run}/kite.png"))
            .is_file()
    );
    let m = fx.manifest(KITE);
    assert_eq!(m["candidate_runs"][0]["run_id"], run.as_str());
    assert!(read_text(&fx.path("projects/sketchbook/progress.md")).contains(&run));
    // Events go out last: the asset's candidate run, then the project's progress.
    let deadline = tokio::time::Instant::now() + DEADLINE;
    let mut seen = Vec::new();
    while seen.len() < 2 && tokio::time::Instant::now() < deadline {
        seen.extend(drain_events(&mut rx));
        tokio::task::yield_now().await;
    }
    let kinds: Vec<(String, String)> = seen
        .iter()
        .map(|e| {
            (
                e["topic"].as_str().unwrap().to_string(),
                e["data"]["kind"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            (topic.clone(), "candidate_run".to_string()),
            ("project:sketchbook".to_string(), "progress".to_string()),
        ]
    );
    assert_eq!(seen[0]["data"]["run_id"], run.as_str());
    // A read tool leaves no plan.
    fx.ok(
        "asset.get",
        json!({"project": "sketchbook", "asset_id": KITE}),
    )
    .await;
    assert_eq!(
        log.take(),
        [
            "meter manifest=true staged=false",
            "record plan=false",
            "record done"
        ]
    );
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-026
async fn signoff_holds_the_version_lock_through_record() {
    let fp = FailPoints::none();
    let armed = Arc::new(AtomicBool::new(false));
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let entered_tx = Mutex::new(entered_tx);
    let hook_armed = armed.clone();
    // Blocks the first sign-off's commit just before its manifest edit (the hook runs on a
    // blocking thread).
    fp.on(
        "store.after_output",
        Box::new(move || {
            if hook_armed.swap(false, Ordering::SeqCst) {
                let _ = entered_tx.lock().unwrap().send(());
                let _ = release_rx.lock().unwrap().recv_timeout(DEADLINE);
            }
            Ok(())
        }),
    );
    let fx = Fx::new(|_| {}, |h| h, fp);
    let first = fx.import_run(KITE, "a.png", b"a").await;
    let second = fx.import_run(KITE, "b.png", b"b").await;

    armed.store(true, Ordering::SeqCst);
    let one = tokio::spawn(fx.call(
        "asset.signoff",
        Fx::signoff_args(KITE, &first, "a.png", json!({"keep_runs": [second]})),
    ));
    tokio::task::spawn_blocking(move || entered_rx.recv_timeout(DEADLINE))
        .await
        .unwrap()
        .expect("the first sign-off reached its Record stage");
    // The seal is done and the manifest edit hasn't run: the version lock is still held.
    let open = fx.hub.open(&fx.ws_ref).await.unwrap();
    let lock = open.store.version_lock("sketchbook", KITE);
    assert!(
        lock.try_lock().is_err(),
        "the version lock is free during Record"
    );
    assert!(fx.path(&format!("{KITE_DIR}/v1.0.0/a.png")).is_file());
    assert_eq!(fx.manifest(KITE)["approved_version"], Value::Null);

    let two = tokio::spawn(fx.call(
        "asset.signoff",
        Fx::signoff_args(KITE, &second, "b.png", json!({})),
    ));
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert!(
        !two.is_finished(),
        "the second sign-off didn't wait for the lock"
    );

    release_tx.send(()).unwrap();
    let one = tokio::time::timeout(DEADLINE, one).await.unwrap().unwrap();
    match one {
        Outcome::Ok(out) => assert_eq!(out.structured["version"], "1.0.0"),
        other => panic!("{other:?}"),
    }
    // The second read the manifest only after the first's edit: its kept run is signable on
    // the approved asset (ruling Q23) and, with no bump, takes 1.0.0 bumped by minor (ruling
    // Q22). Had it read before, it would have tried v1.0.0 again (`version.exists`).
    let two = tokio::time::timeout(DEADLINE, two).await.unwrap().unwrap();
    match two {
        Outcome::Ok(out) => assert_eq!(out.structured["version"], "1.1.0", "{out:?}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(fx.manifest(KITE)["approved_version"], "1.1.0");
    assert!(lock.try_lock().is_ok());
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-020, ARCH-075, CORE-038
async fn record_failure_turns_ok_into_the_store_error_and_lists_orphans() {
    let fp = Arc::new(FailPoints::parse("store.after_output=io").unwrap());
    let fx = Fx::new(|_| {}, |h| h, fp);
    let progress = read_text(&fx.path("projects/sketchbook/progress.md"));
    let e = fx
        .err("asset.import", fx.import_args(KITE, "kite.png", b"kite"))
        .await;
    assert_eq!(e.code, "store.io", "{e:?}");
    // The output was moved before the failure: it stays, listed as an orphan, never deleted.
    let candidates = fx.path(&format!("{KITE_DIR}/candidates"));
    let runs: Vec<String> = std::fs::read_dir(&candidates)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert!(file(&candidates, &format!("{}/kite.png", runs[0])).is_file());
    assert!(
        !fx.path(&format!("{KITE_DIR}/{KITE}.manifest.json"))
            .exists()
    );
    assert_eq!(
        read_text(&fx.path("projects/sketchbook/progress.md")),
        progress
    );
    let status = fx
        .ok("project.status", json!({"project": "sketchbook"}))
        .await;
    assert!(
        status["orphans"]
            .as_array()
            .unwrap()
            .contains(&json!(format!("{KITE_DIR}/candidates/{}", runs[0]))),
        "{status}"
    );
    fx.shutdown().await;
}

/// A made-up manifest of a dependent of `prop_kite` (I5 shape).
fn dependent(asset: &str, project: &str, status: &str, input: &str, pinned: bool) -> Vec<u8> {
    let pinned_inputs = if pinned {
        json!([{"ref": input, "reason": "kept", "by": "user", "date": "2026-10-01"}])
    } else {
        json!([])
    };
    let mut m = json!({
        "schema_version": 1,
        "asset_id": asset,
        "project": project,
        "type": "image",
        "version": "1.0.0",
        "approved_version": if status == "planned" { Value::Null } else { json!("1.0.0") },
        "status": status,
        "inputs": [{"ref": input, "role": "reference"}],
        "pinned_inputs": pinned_inputs,
        "candidate_runs": [],
        "hashes": {},
    });
    if status != "planned" {
        m["hashes"] = json!({"v1.0.0/d.png": format!("sha256:{}", sha256_bytes(asset.as_bytes()))});
    }
    format!("{}\n", serde_json::to_string_pretty(&m).unwrap()).into_bytes()
}

#[tokio::test] // spec: CORE-048, CORE-020, I6
async fn signoff_marks_unpinned_dependents_stale_and_reports_them() {
    let fx = Fx::new(
        |ws| {
            let put = |project: &str, asset: &str, bytes: Vec<u8>| {
                let dir = format!("projects/{project}/assets/{asset}");
                write_file(&file(ws, &format!("{dir}/{asset}.manifest.json")), &bytes);
                write_file(&file(ws, &format!("{dir}/v1.0.0/d.png")), asset.as_bytes());
            };
            put(
                "sketchbook",
                "dep_stale",
                dependent(
                    "dep_stale",
                    "sketchbook",
                    "approved",
                    "prop_kite@1.0.0",
                    false,
                ),
            );
            put(
                "sketchbook",
                "dep_pinned",
                dependent(
                    "dep_pinned",
                    "sketchbook",
                    "approved",
                    "prop_kite@1.0.0",
                    true,
                ),
            );
            put(
                "sketchbook",
                "dep_newer",
                dependent(
                    "dep_newer",
                    "sketchbook",
                    "released",
                    "prop_kite@2.0.0",
                    false,
                ),
            );
            put(
                "sketchbook",
                "dep_waiting",
                dependent(
                    "dep_waiting",
                    "sketchbook",
                    "planned",
                    "prop_kite@2.0.0",
                    false,
                ),
            );
            // An archived project is skipped until it is restored (I6).
            write_file(
                &file(ws, "projects/harbor/project.yaml"),
                b"# A made-up archived project.\nname: Harbor\nslug: harbor\nstatus: archived\nlinks: []\n",
            );
            put(
                "harbor",
                "dep_archived",
                dependent(
                    "dep_archived",
                    "harbor",
                    "approved",
                    "proj:sketchbook/prop_kite@1.0.0",
                    false,
                ),
            );
        },
        |h| h,
        FailPoints::none(),
    );
    let first = fx.import_run(KITE, "kite.png", b"one").await;
    let r = fx
        .ok(
            "asset.signoff",
            Fx::signoff_args(KITE, &first, "kite.png", json!({})),
        )
        .await;
    // 1.0.0 is what every dependent names: nothing goes stale, nothing is unlocked yet.
    assert_eq!(r["handoff"]["made_stale"], json!([]), "{r}");
    assert_eq!(r["handoff"]["unlocks"], json!([]), "{r}");

    let second = fx.import_run(KITE, "kite_two.png", b"two").await;
    let r = fx
        .ok(
            "asset.signoff",
            Fx::signoff_args(KITE, &second, "kite_two.png", json!({"bump": "major"})),
        )
        .await;
    assert_eq!(r["version"], "2.0.0");
    let hash = version_content_hash_of(&fx.path("projects/sketchbook/assets/dep_stale/v1.0.0"));
    assert_eq!(
        r["handoff"]["made_stale"],
        json!([format!(
            "lil://{}/sketchbook/dep_stale@1.0.0#sha256={hash}",
            fx.ws_ref.id
        )]),
        "{r}"
    );
    assert_eq!(
        r["handoff"]["unlocks"],
        json!([format!("lil://{}/sketchbook/dep_waiting", fx.ws_ref.id)]),
        "{r}"
    );
    let stale = fx.manifest("dep_stale");
    assert_eq!(stale["status"], "stale");
    assert_eq!(stale["status_before_stale"], "approved");
    assert_eq!(stale["status_reason"], "input prop_kite 2.0.0 signed off");
    assert_eq!(fx.manifest("dep_pinned")["status"], "approved");
    assert_eq!(fx.manifest("dep_newer")["status"], "released");
    let archived: Value = serde_json::from_str(&read_text(
        &fx.path("projects/harbor/assets/dep_archived/dep_archived.manifest.json"),
    ))
    .unwrap();
    assert_eq!(archived["status"], "approved");
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-020
async fn handoff_is_in_every_recorded_result() {
    let fx = Fx::plain();
    let check = |v: &Value| {
        for list in ["unlocks", "made_stale", "refs"] {
            assert!(v["handoff"][list].is_array(), "handoff.{list}: {v}");
        }
    };
    let imported = fx
        .ok("asset.import", fx.import_args(KITE, "kite.png", b"one"))
        .await;
    check(&imported);
    let run = imported["run_id"].as_str().unwrap();
    let signed = fx
        .ok(
            "asset.signoff",
            Fx::signoff_args(KITE, run, "kite.png", json!({})),
        )
        .await;
    check(&signed);
    assert_eq!(signed["handoff"]["refs"], json!([signed["ref"].clone()]));
    let source = fx.outside("lamp.png", b"lamp");
    let linked = fx
        .ok(
            "asset.import",
            json!({"project": "sketchbook", "asset_id": "prop_lamp", "type": "image",
                   "files": [source.to_string_lossy()], "mode": "link_external"}),
        )
        .await;
    check(&linked);
    let collected = fx
        .ok(
            "asset.collect",
            json!({"project": "sketchbook", "asset_id": "prop_lamp"}),
        )
        .await;
    check(&collected);
    assert_eq!(collected["handoff"]["refs"].as_array().unwrap().len(), 1);
    fx.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)] // spec: CORE-023, CORE-047
async fn concurrent_imports_get_distinct_runs() {
    let fx = Fx::plain();
    // The fake clock stands still: every run starts in the same minute.
    for asset in ["prop_kite", "prop_lamp", "prop_sail"] {
        let one = tokio::spawn(fx.call("asset.import", fx.import_args(asset, "a.png", b"a")));
        let two = tokio::spawn(fx.call("asset.import", fx.import_args(asset, "b.png", b"b")));
        let mut runs = Vec::new();
        for call in [one, two] {
            match tokio::time::timeout(DEADLINE, call).await.unwrap().unwrap() {
                Outcome::Ok(out) => {
                    runs.push(out.structured["run_id"].as_str().unwrap().to_string())
                }
                other => panic!("{asset}: {other:?}"),
            }
        }
        assert_ne!(runs[0], runs[1], "{asset}: one run id for two imports");
        let m = fx.manifest(asset);
        let ids: Vec<&str> = m["candidate_runs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["run_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), 2, "{m}");
        assert_ne!(ids[0], ids[1], "{m}");
        for (run, name) in runs.iter().zip(["a.png", "b.png"]) {
            let dir = fx.path(&format!(
                "projects/sketchbook/assets/{asset}/candidates/{run}"
            ));
            let names: Vec<String> = std::fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            assert_eq!(names, [name], "{asset}/{run}");
        }
    }
    fx.shutdown().await;
}

/// Fail points whose `store.after_seal` runs `edit` once on the asset's manifest JSON, as an
/// outside writer would, between the seal and the Record stage's manifest edit.
fn edit_after_seal(fp: &FailPoints, manifest: PathBuf, edit: fn(&mut Value)) {
    let once = AtomicBool::new(true);
    fp.on(
        "store.after_seal",
        Box::new(move || {
            if once.swap(false, Ordering::SeqCst) {
                let mut m: Value = serde_json::from_str(&read_text(&manifest)).unwrap();
                edit(&mut m);
                write_file(
                    &manifest,
                    format!("{}\n", serde_json::to_string_pretty(&m).unwrap()).as_bytes(),
                );
            }
            Ok(())
        }),
    );
}

/// A sign-off whose manifest is edited between its seal and its Record stage by `edit`;
/// returns the error and the fixture.
async fn signoff_after_outside_edit(edit: fn(&mut Value)) -> (ErrorBody, Fx) {
    let fp = FailPoints::none();
    let fx = Fx::new(|_| {}, |h| h, fp.clone());
    let run = fx.import_run(KITE, "a.png", b"a").await;
    edit_after_seal(
        &fp,
        fx.path(&format!("{KITE_DIR}/{KITE}.manifest.json")),
        edit,
    );
    let e = fx
        .err(
            "asset.signoff",
            Fx::signoff_args(KITE, &run, "a.png", json!({})),
        )
        .await;
    (e, fx)
}

#[tokio::test] // spec: CORE-026, I5 (ruling Q17)
async fn a_chosen_hash_that_disagrees_with_the_seal_is_a_conflict() {
    let (e, fx) = signoff_after_outside_edit(|m| {
        m["candidate_runs"][0]["files"][0]["sha256"] = json!("0".repeat(64));
    })
    .await;
    assert_eq!(e.code, "store.hash_conflict", "{e:?}");
    let m = fx.manifest(KITE);
    assert_eq!(m["approved_version"], Value::Null, "{m}");
    assert!(m["hashes"].get("v1.0.0/a.png").is_none(), "{m}");
}

#[tokio::test] // spec: CORE-026, I5 (ruling Q17)
async fn a_stray_hash_key_in_the_sealed_version_is_a_conflict() {
    let (e, fx) = signoff_after_outside_edit(|m| {
        m["hashes"]["v1.0.0/stray.png"] = json!(format!("sha256:{}", "1".repeat(64)));
    })
    .await;
    assert_eq!(e.code, "store.hash_conflict", "{e:?}");
    let m = fx.manifest(KITE);
    assert_eq!(m["approved_version"], Value::Null, "{m}");
    assert_eq!(m["status"], "candidate", "{m}");
}

#[tokio::test] // spec: CORE-026, ARCH-079
async fn a_candidate_edited_during_the_seal_is_put_back_and_refused() {
    let fp = FailPoints::none();
    let fx = Fx::new(|_| {}, |h| h, fp.clone());
    let run = fx.import_run(KITE, "a.png", b"a").await;
    let candidate = fx.path(&format!("{KITE_DIR}/candidates/{run}/a.png"));
    // After the pre-seal check: an outside writer saves the chosen file just before the seal
    // moves it into its sealing folder (the first `store.replace` of the seal).
    let armed = Arc::new(AtomicBool::new(true));
    let (hook_armed, target) = (armed.clone(), candidate.clone());
    fp.on(
        "store.replace",
        Box::new(move || {
            if hook_armed.swap(false, Ordering::SeqCst) {
                write_file(&target, b"saved outside mid-seal");
            }
            Ok(())
        }),
    );
    let e = fx
        .err(
            "asset.signoff",
            Fx::signoff_args(KITE, &run, "a.png", json!({})),
        )
        .await;
    assert!(!armed.load(Ordering::SeqCst), "the hook never ran");
    assert_eq!(e.code, "store.hash_conflict", "{e:?}");
    let data = e.data.clone().expect("data");
    assert_eq!(
        data["path"],
        format!("{KITE_DIR}/candidates/{run}/a.png"),
        "{e:?}"
    );
    assert_eq!(data["expected"], sha256_bytes(b"a"), "{e:?}");
    assert_eq!(
        data["found"],
        sha256_bytes(b"saved outside mid-seal"),
        "{e:?}"
    );
    // Nothing sealed, nothing left in staging, the outside bytes back in the run (writable).
    assert!(!fx.path(&format!("{KITE_DIR}/v1.0.0")).exists());
    assert_eq!(
        std::fs::read(&candidate).unwrap(),
        b"saved outside mid-seal"
    );
    assert!(
        !std::fs::metadata(&candidate)
            .unwrap()
            .permissions()
            .readonly()
    );
    let staging: Vec<String> = std::fs::read_dir(fx.path(".lilith/staging"))
        .map(|d| {
            d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(staging.is_empty(), "{staging:?}");
    let m = fx.manifest(KITE);
    assert_eq!(m["approved_version"], Value::Null, "{m}");
    // A fresh run then signs off.
    let fresh = fx
        .import_run(KITE, "a.png", b"saved outside mid-seal")
        .await;
    let r = fx
        .ok(
            "asset.signoff",
            Fx::signoff_args(KITE, &fresh, "a.png", json!({})),
        )
        .await;
    assert_eq!(r["version"], "1.0.0", "{r}");
    fx.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Ruling Q31: a Record failure after the seal is retryable, and what follows the manifest edit
// is best-effort.
// ---------------------------------------------------------------------------------------------

/// Fail points whose `point` fails once (with a plain I/O error) after `arm` is set.
fn fail_once(fp: &FailPoints, point: &str, arm: Arc<AtomicBool>) {
    fp.on(
        point,
        Box::new(move || {
            if arm.swap(false, Ordering::SeqCst) {
                return Err(std::io::Error::other("a test failure"));
            }
            Ok(())
        }),
    );
}

/// The `v<semver>` folders of the kite asset.
fn version_folders(fx: &Fx) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(fx.path(KITE_DIR))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('v'))
        .collect();
    found.sort();
    found
}

/// A sign-off that fails at `point` once, after its seal, then is retried with the same
/// idempotency key: the retry adopts the sealed folder and records it.
async fn retried_signoff_adopts_the_seal(point: &str) {
    let fp = FailPoints::none();
    let arm = Arc::new(AtomicBool::new(false));
    fail_once(&fp, point, arm.clone());
    let fx = Fx::new(|_| {}, |h| h, fp);
    let run = fx.import_run(KITE, "a.png", b"a").await;
    let args = Fx::signoff_args(KITE, &run, "a.png", json!({}));
    let idem = key();

    arm.store(true, Ordering::SeqCst);
    let e = match fx
        .call_keyed("asset.signoff", args.clone(), idem.clone())
        .await
    {
        Outcome::Error { error } => error,
        other => panic!("{point}: the first sign-off should fail: {other:?}"),
    };
    assert_eq!(e.code, "store.io", "{point}: {e:?}");
    assert_eq!(
        e.retry,
        lil_protocol::RetryPolicy::SameIdempotencyKey,
        "{point}: {e:?}"
    );
    // The seal took effect and the manifest doesn't record it.
    let sealed = fx.path(&format!("{KITE_DIR}/v1.0.0/a.png"));
    assert_eq!(std::fs::read(&sealed).unwrap(), b"a", "{point}");
    assert!(
        !fx.path(&format!("{KITE_DIR}/candidates/{run}/a.png"))
            .exists()
    );
    let m = fx.manifest(KITE);
    assert_eq!(m["approved_version"], Value::Null, "{point}: {m}");
    assert_eq!(m["candidate_runs"][0]["status"], "open", "{point}: {m}");

    // The retry, with the same key, records the version that is there.
    let r = match fx.call_keyed("asset.signoff", args, idem).await {
        Outcome::Ok(out) => out.structured,
        other => panic!("{point}: the retry should succeed: {other:?}"),
    };
    let hash = version_content_hash_of(&fx.path(&format!("{KITE_DIR}/v1.0.0")));
    assert_eq!(r["version"], "1.0.0", "{point}: {r}");
    assert_eq!(
        r["ref"],
        format!(
            "lil://{}/sketchbook/{KITE}@1.0.0#sha256={hash}",
            fx.ws_ref.id
        ),
        "{point}: {r}"
    );
    assert_eq!(r["warnings"], json!([]), "{point}: {r}");
    let m = fx.manifest(KITE);
    assert_eq!(m["approved_version"], "1.0.0", "{point}: {m}");
    assert_eq!(m["status"], "approved", "{point}: {m}");
    assert_eq!(m["candidate_runs"][0]["status"], "selected", "{point}: {m}");
    assert_eq!(
        m["hashes"]["v1.0.0/a.png"],
        format!("sha256:{}", sha256_bytes(b"a")),
        "{point}: {m}"
    );
    // Exactly one sealed version, untouched and still read-only, and no orphan.
    assert_eq!(version_folders(&fx), ["v1.0.0"], "{point}");
    assert_eq!(std::fs::read(&sealed).unwrap(), b"a", "{point}");
    assert!(std::fs::metadata(&sealed).unwrap().permissions().readonly());
    let status = fx
        .ok("project.status", json!({"project": "sketchbook"}))
        .await;
    assert_eq!(status["orphans"], json!([]), "{point}: {status}");
    let detail = fx
        .ok(
            "asset.get",
            json!({"project": "sketchbook", "asset_id": KITE}),
        )
        .await;
    assert_eq!(detail["versions"].as_array().unwrap().len(), 1, "{detail}");
    assert_eq!(detail["versions"][0]["ref"], r["ref"], "{detail}");
}

#[tokio::test] // spec: CORE-026, CORE-020, ARCH-075
async fn a_record_failure_after_the_seal_is_retried_with_the_same_key() {
    // The Record stage's commit fails before its manifest edit.
    retried_signoff_adopts_the_seal("store.after_output").await;
}

#[tokio::test] // spec: CORE-026, QA-066
async fn a_failure_right_after_the_seal_is_retried_with_the_same_key() {
    // The seal itself fails after its final move (Execute).
    retried_signoff_adopts_the_seal("store.after_seal").await;
}

#[tokio::test] // spec: CORE-026
async fn a_version_folder_that_differs_is_never_adopted() {
    let fp = FailPoints::none();
    let arm = Arc::new(AtomicBool::new(false));
    fail_once(&fp, "store.after_output", arm.clone());
    let fx = Fx::new(|_| {}, |h| h, fp);
    let run = fx.import_run(KITE, "a.png", b"a").await;
    let args = Fx::signoff_args(KITE, &run, "a.png", json!({}));
    arm.store(true, Ordering::SeqCst);
    assert_eq!(fx.err("asset.signoff", args.clone()).await.code, "store.io");
    // Something else lands in the unrecorded version folder.
    let extra = fx.path(&format!("{KITE_DIR}/v1.0.0/extra.png"));
    write_file(&extra, b"not the seal's");
    let before: Vec<(String, Vec<u8>)> = ["a.png", "extra.png", "manifest.snapshot.json"]
        .iter()
        .map(|n| {
            let path = fx.path(&format!("{KITE_DIR}/v1.0.0/{n}"));
            ((*n).to_string(), std::fs::read(path).unwrap())
        })
        .collect();
    let e = fx.err("asset.signoff", args).await;
    assert_eq!(e.code, "version.exists", "{e:?}");
    // The sealed folder is never touched.
    for (name, bytes) in before {
        let path = fx.path(&format!("{KITE_DIR}/v1.0.0/{name}"));
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "{name}");
    }
    assert!(
        std::fs::metadata(fx.path(&format!("{KITE_DIR}/v1.0.0/a.png")))
            .unwrap()
            .permissions()
            .readonly()
    );
    let m = fx.manifest(KITE);
    assert_eq!(m["approved_version"], Value::Null, "{m}");
    assert_eq!(version_folders(&fx), ["v1.0.0"]);
    fx.shutdown().await;
}

/// The real probe, except that one path reports the cloud placeholder attributes and its data
/// is never opened.
///
/// Paths are compared as `folder_key` compares them, not by spelling: the core asks about a
/// workspace file under the canonical workspace folder, while the test spells it under the
/// temp folder, which may be spelled with a short name (the runner's `RUNNER~1` on CI).
#[derive(Debug)]
struct PlaceholderAt(PathBuf);

impl PlaceholderAt {
    fn is_placeholder(&self, path: &Path) -> bool {
        folder_key(path) == folder_key(&self.0)
    }
}

impl Probe for PlaceholderAt {
    fn identity(&self, path: &Path) -> std::io::Result<lil_platform::fs::FileIdentity> {
        let mut id = SystemProbe.identity(path)?;
        if self.is_placeholder(path) {
            id.attributes |= lil_platform::fs::PLACEHOLDER_ATTRIBUTES;
        }
        Ok(id)
    }

    fn volume_info(&self, path: &Path) -> std::io::Result<lil_platform::fs::VolumeInfo> {
        SystemProbe.volume_info(path)
    }

    fn open_read(&self, path: &Path) -> std::io::Result<std::fs::File> {
        assert!(
            !self.is_placeholder(path),
            "a placeholder's data was opened: {}",
            path.display()
        );
        SystemProbe.open_read(path)
    }
}

#[tokio::test] // spec: CORE-020, CORE-039, ARCH-075
async fn a_cloud_only_progress_md_is_a_warning_and_the_operation_is_recorded() {
    let fx = Fx::with_probe(
        |ws| {
            let put = |project: &str, asset: &str, bytes: Vec<u8>| {
                let dir = format!("projects/{project}/assets/{asset}");
                write_file(&file(ws, &format!("{dir}/{asset}.manifest.json")), &bytes);
                write_file(&file(ws, &format!("{dir}/v1.0.0/d.png")), asset.as_bytes());
            };
            put(
                "sketchbook",
                "dep_stale",
                dependent(
                    "dep_stale",
                    "sketchbook",
                    "approved",
                    "prop_kite@1.0.0",
                    false,
                ),
            );
        },
        |h| h,
        FailPoints::none(),
        |ws| Arc::new(PlaceholderAt(file(ws, "projects/sketchbook/progress.md"))),
    );
    let progress = fx.path("projects/sketchbook/progress.md");
    let progress_before = std::fs::read(&progress).unwrap();

    // An import: `ok` with a warning, and the index knows the run.
    let r = fx
        .ok("asset.import", fx.import_args(KITE, "kite.png", b"one"))
        .await;
    let warnings = r["warnings"].as_array().expect("warnings").clone();
    assert_eq!(warnings.len(), 1, "{r}");
    assert!(warnings[0].as_str().unwrap().contains("progress.md"), "{r}");
    let run = r["run_id"].as_str().unwrap().to_string();
    let detail = fx
        .ok(
            "asset.get",
            json!({"project": "sketchbook", "asset_id": KITE}),
        )
        .await;
    assert_eq!(detail["status"], "candidate", "{detail}");
    assert!(
        detail["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == format!("candidates/{run}/kite.png") && f["state"] == "ok"),
        "{detail}"
    );

    // Two sign-offs: the second makes the dependent stale although progress.md can't be
    // written, and the index learns the sealed version.
    fx.ok(
        "asset.signoff",
        Fx::signoff_args(KITE, &run, "kite.png", json!({})),
    )
    .await;
    let second = fx.import_run(KITE, "kite_two.png", b"two").await;
    let r = fx
        .ok(
            "asset.signoff",
            Fx::signoff_args(KITE, &second, "kite_two.png", json!({"bump": "major"})),
        )
        .await;
    assert_eq!(r["version"], "2.0.0", "{r}");
    assert_eq!(r["warnings"].as_array().unwrap().len(), 1, "{r}");
    assert_eq!(fx.manifest("dep_stale")["status"], "stale");
    assert_eq!(
        r["handoff"]["made_stale"].as_array().unwrap().len(),
        1,
        "{r}"
    );
    let detail = fx
        .ok(
            "asset.get",
            json!({"project": "sketchbook", "asset_id": KITE}),
        )
        .await;
    assert_eq!(detail["approved_version"], "2.0.0", "{detail}");
    assert!(
        detail["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == "v2.0.0/kite_two.png" && f["state"] == "ok"),
        "{detail}"
    );
    // The placeholder itself was never read or written.
    assert_eq!(std::fs::read(&progress).unwrap(), progress_before);
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-026, CORE-051 (ruling Q34)
async fn an_adopted_signoff_keeps_the_snapshots_date() {
    let fp = FailPoints::none();
    let arm = Arc::new(AtomicBool::new(false));
    fail_once(&fp, "store.after_output", arm.clone());
    let fx = Fx::new(|_| {}, |h| h, fp);
    let run = fx.import_run(KITE, "a.png", b"a").await;
    let args = Fx::signoff_args(KITE, &run, "a.png", json!({}));
    let idem = key();
    arm.store(true, Ordering::SeqCst);
    assert!(matches!(
        fx.call_keyed("asset.signoff", args.clone(), idem.clone())
            .await,
        Outcome::Error { .. }
    ));
    let snapshot: Value = serde_json::from_str(&read_text(
        &fx.path(&format!("{KITE_DIR}/v1.0.0/manifest.snapshot.json")),
    ))
    .unwrap();
    let sealed_on = snapshot["candidate_runs"][0]["signed_off"]["date"].clone();
    assert!(sealed_on.is_string(), "{snapshot}");
    // The retry comes two days later: the manifest still says when the version was sealed.
    fx.clock.advance(Duration::from_secs(2 * 86_400));
    match fx.call_keyed("asset.signoff", args, idem).await {
        Outcome::Ok(_) => {}
        other => panic!("the retry should succeed: {other:?}"),
    }
    let m = fx.manifest(KITE);
    assert_eq!(
        m["candidate_runs"][0]["signed_off"]["date"], sealed_on,
        "{m}"
    );
    assert_eq!(
        m["candidate_runs"][0]["signed_off"], snapshot["candidate_runs"][0]["signed_off"],
        "{m}"
    );
    fx.shutdown().await;
}

/// The newest status-log entry of the manifest `m`.
fn newest_entry(m: &Value) -> Value {
    m["status_log"]
        .as_array()
        .and_then(|log| log.last())
        .cloned()
        .unwrap_or_else(|| panic!("no status_log entry: {m}"))
}

#[tokio::test] // spec: CORE-156, CORE-048, I6
async fn core_set_stale_names_the_changed_input() {
    let fx = Fx::new(
        |ws| {
            let put = |project: &str, asset: &str, bytes: Vec<u8>| {
                let dir = format!("projects/{project}/assets/{asset}");
                write_file(&file(ws, &format!("{dir}/{asset}.manifest.json")), &bytes);
                write_file(&file(ws, &format!("{dir}/v1.0.0/d.png")), asset.as_bytes());
            };
            put(
                "sketchbook",
                "dep_stale",
                dependent(
                    "dep_stale",
                    "sketchbook",
                    "approved",
                    "prop_kite@1.0.0",
                    false,
                ),
            );
            write_file(
                &file(ws, "projects/harbor/project.yaml"),
                b"# A made-up archived project.\nname: Harbor\nslug: harbor\nstatus: archived\nlinks: []\n",
            );
            put(
                "harbor",
                "dep_archived",
                dependent(
                    "dep_archived",
                    "harbor",
                    "exported",
                    "proj:sketchbook/prop_kite@1.0.0",
                    false,
                ),
            );
        },
        |h| h,
        FailPoints::none(),
    );
    let first = fx.import_run(KITE, "kite.png", b"one").await;
    fx.ok(
        "asset.signoff",
        Fx::signoff_args(KITE, &first, "kite.png", json!({})),
    )
    .await;
    let second = fx.import_run(KITE, "kite_two.png", b"two").await;
    fx.ok(
        "asset.signoff",
        Fx::signoff_args(KITE, &second, "kite_two.png", json!({"bump": "major"})),
    )
    .await;
    // The sign-off's pass: the dependent's one entry names the input it took the change from.
    let stale = fx.manifest("dep_stale");
    assert_eq!(
        stale["status_log"].as_array().map(Vec::len),
        Some(1),
        "{stale}"
    );
    let e = newest_entry(&stale);
    assert_eq!(
        (&e["from"], &e["to"], &e["by"]),
        (&json!("approved"), &json!("stale"), &json!("core")),
        "{e}"
    );
    assert_eq!(
        e["reason"],
        json!({"code": "input_changed", "ref": "prop_kite@1.0.0"})
    );
    assert_eq!(stale["status_reason"], "input prop_kite 2.0.0 signed off");

    // The restore pass (`projects/staleness.rs`): the archived project catches up.
    let archived = || -> Value {
        serde_json::from_str(&read_text(
            &fx.path("projects/harbor/assets/dep_archived/dep_archived.manifest.json"),
        ))
        .unwrap()
    };
    assert!(archived().get("status_log").is_none());
    fx.ok(
        "project.set_status",
        json!({"project": "harbor", "status": "active"}),
    )
    .await;
    let m = archived();
    assert_eq!(m["status_log"].as_array().map(Vec::len), Some(1), "{m}");
    let e = newest_entry(&m);
    assert_eq!(
        (&e["from"], &e["to"], &e["by"]),
        (&json!("exported"), &json!("stale"), &json!("core")),
        "{e}"
    );
    assert_eq!(
        e["reason"],
        json!({"code": "input_changed", "ref": "proj:sketchbook/prop_kite@1.0.0"})
    );
    assert_eq!(m["status_reason"], "input prop_kite 2.0.0 signed off");
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-170
async fn deprecate_publishes_previous() {
    let fx = Fx::plain();
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    fx.events.attach(7, tx);
    let asset = format!("asset:lil://{}/sketchbook/{KITE}", fx.ws_ref.id);
    let project = "project:sketchbook".to_string();
    fx.events
        .subscribe(7, &[asset.clone(), project.clone()], &Default::default())
        .await;
    fx.import_run(KITE, "kite.png", b"kite").await;
    let deprecate = |deprecated: bool| json!({"project": "sketchbook", "asset_id": KITE, "deprecated": deprecated});
    let r = fx.ok("asset.deprecate", deprecate(true)).await;
    assert_eq!(r, json!({"status": "deprecated"}));
    // Deprecating again is no change: no event, no entry.
    let r = fx.ok("asset.deprecate", deprecate(true)).await;
    assert_eq!(r, json!({"status": "deprecated"}));
    let r = fx.ok("asset.deprecate", deprecate(false)).await;
    assert_eq!(r, json!({"status": "candidate"}));
    let log = fx.manifest(KITE)["status_log"].as_array().unwrap().len();
    assert_eq!(log, 3, "the import, the deprecation and the return");

    // The deprecation's events: `deprecated` and `status` on the asset's topic, `deprecated`
    // on the project's (nothing else here publishes those kinds).
    let ours = |e: &Value| {
        let kind = e["data"]["kind"].as_str().unwrap_or_default();
        kind == "deprecated" || (kind == "status" && e["topic"] == asset.as_str())
    };
    let deadline = tokio::time::Instant::now() + DEADLINE;
    let mut seen = Vec::new();
    while seen.len() < 6 && tokio::time::Instant::now() < deadline {
        seen.extend(drain_events(&mut rx).into_iter().filter(|e| ours(e)));
        tokio::task::yield_now().await;
    }
    seen.extend(drain_events(&mut rx).into_iter().filter(|e| ours(e)));
    let got: Vec<(&str, &Value)> = seen
        .iter()
        .map(|e| (e["topic"].as_str().unwrap(), &e["data"]))
        .collect();
    let on = |deprecated: bool, status: &str, previous: &str| {
        [
            json!({"kind": "deprecated", "deprecated": deprecated, "status": status,
                   "previous": previous}),
            json!({"kind": "status", "status": status}),
            json!({"kind": "deprecated", "asset_id": KITE, "deprecated": deprecated,
                   "status": status, "previous": previous}),
        ]
    };
    let (down, up) = (
        on(true, "deprecated", "candidate"),
        on(false, "candidate", "deprecated"),
    );
    let want: Vec<(&str, &Value)> = [&down, &up]
        .into_iter()
        .flat_map(|events| {
            [
                (asset.as_str(), &events[0]),
                (asset.as_str(), &events[1]),
                (project.as_str(), &events[2]),
            ]
        })
        .collect();
    assert_eq!(got, want, "{seen:?}");
    fx.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Attribution (CORE-023, CORE-034, CORE-051, ARCH-103; P4b-R16, P4b-R17; Appendix H
// "Attribution").
// ---------------------------------------------------------------------------------------------

/// The agent kinds of `Actor`: an entry with one of them is an agent edit, never a human one.
const AGENT_KINDS: [&str; 3] = ["claude_code", "tether", "director"];

fn ui() -> CallerKind {
    CallerKind::Ui("test".into())
}

/// A core whose workspace can't be watched (a fail point): only `watch.notify` records an
/// outside edit, so a refused call is seen to record nothing.
fn unwatched() -> Fx {
    Fx::new(
        |_| {},
        |h| h,
        Arc::new(FailPoints::parse("watch.start=unsupported").unwrap()),
    )
}

/// Replaces a workspace file's bytes as an outside program would (its read-only bit cleared
/// first); returns their SHA-256.
#[allow(clippy::disallowed_methods)]
fn edit_outside(path: &Path, bytes: &[u8]) -> String {
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    #[allow(
        clippy::permissions_set_readonly_false,
        reason = "Windows: this clears FILE_ATTRIBUTE_READONLY, as an editor would"
    )]
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions).unwrap();
    write_file(path, bytes);
    sha256_bytes(bytes)
}

impl Fx {
    async fn ok_by(&self, caller: CallerKind, name: &str, args: Value) -> Value {
        match self
            .call_by(caller.clone(), name, args.clone(), key())
            .await
        {
            Outcome::Ok(out) => out.structured,
            other => panic!("{name} {args} by {caller}: {other:?}"),
        }
    }

    async fn err_by(&self, caller: CallerKind, name: &str, args: Value) -> ErrorBody {
        match self
            .call_by(caller.clone(), name, args.clone(), key())
            .await
        {
            Outcome::Error { error } => error,
            other => panic!("{name} {args} by {caller} should fail: {other:?}"),
        }
    }

    /// A candidate of a new run of `asset` (imported by `cli`): its workspace path.
    async fn candidate(&self, asset: &str) -> String {
        let run = self.import_run(asset, "kite.png", b"kite").await;
        format!("projects/sketchbook/assets/{asset}/candidates/{run}/kite.png")
    }

    /// The manifest's `human_edits` entry of `asset` with `sha256`, if any.
    fn edit_with(&self, asset: &str, sha256: &str) -> Option<Value> {
        self.manifest(asset)["human_edits"]
            .as_array()
            .and_then(|edits| edits.iter().find(|e| e["sha256"] == sha256).cloned())
    }

    fn edit_count(&self, asset: &str) -> usize {
        self.manifest(asset)["human_edits"]
            .as_array()
            .map_or(0, Vec::len)
    }

    /// `watch.notify {paths: [rel], by?}` from `caller`.
    async fn notify(&self, caller: CallerKind, rel: &str, by: Option<Value>) -> Value {
        let mut args = json!({ "paths": [rel] });
        if let Some(by) = by {
            args["by"] = by;
        }
        self.ok_by(caller, "watch.notify", args).await
    }
}

/// Each caller kind and the `actor.kind` it maps to (P4b-R16).
fn callers() -> [(CallerKind, &'static str); 4] {
    [
        (CallerKind::Cli, "cli"),
        (CallerKind::Mcp, "claude_code"),
        (CallerKind::Assistant, "tether"),
        (ui(), "owner"),
    ]
}

#[tokio::test] // spec: CORE-023
async fn runs_and_files_record_the_callers_actor() {
    let fx = Fx::plain();
    for (caller, kind) in callers() {
        let asset = format!("prop_by_{kind}");
        let r = fx
            .ok_by(
                caller,
                "asset.import",
                fx.import_args(&asset, "kite.png", b"kite"),
            )
            .await;
        let m = fx.manifest(&asset);
        let run = m["candidate_runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|run| run["run_id"] == r["run_id"])
            .unwrap_or_else(|| panic!("{m}"))
            .clone();
        assert_eq!(run["actor"], json!({ "kind": kind }), "{run}");
        let files = run["files"].as_array().unwrap();
        assert!(!files.is_empty(), "{run}");
        for f in files {
            assert_eq!(f["actor"], json!({ "kind": kind }), "{f}");
        }
    }
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-034, ARCH-103
async fn notify_with_by_is_an_agent_edit_and_never_counted_as_human() {
    let fx = unwatched();
    let rel = fx.candidate(KITE).await;
    let sha = edit_outside(&fx.path(&rel), b"edited by Claude Code");
    let by = json!({"kind": "claude_code", "session_id": "s1"});
    let r = fx.notify(CallerKind::Cli, &rel, Some(by.clone())).await;
    assert_eq!(r["reconciled"], json!([rel]), "{r}");
    let entry = fx.edit_with(KITE, &sha).expect("the edit is recorded");
    assert_eq!(entry["actor"], by, "{entry}");
    assert!(AGENT_KINDS.iter().any(|k| entry["actor"]["kind"] == *k));
    assert!(entry.get("relabelled").is_none(), "{entry}");

    // Reported again, now without `by` (as yours): it is recorded already, and stays the
    // agent's edit.
    fx.notify(CallerKind::Cli, &rel, None).await;
    fx.notify(ui(), &rel, None).await;
    assert_eq!(fx.edit_count(KITE), 1, "{}", fx.manifest(KITE));
    assert_eq!(fx.edit_with(KITE, &sha).unwrap()["actor"], by);

    // An app or `lil` may name any agent.
    let rel = fx.candidate("prop_by_app").await;
    let sha = edit_outside(&fx.path(&rel), b"edited by the director");
    fx.notify(
        ui(),
        &rel,
        Some(json!({"kind": "director", "task_id": "t1"})),
    )
    .await;
    let entry = fx.edit_with("prop_by_app", &sha).unwrap();
    assert_eq!(
        entry["actor"],
        json!({"kind": "director", "task_id": "t1"}),
        "{entry}"
    );
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-034
async fn notify_without_by_stays_a_human_edit() {
    let fx = unwatched();
    for (caller, kind) in [(CallerKind::Cli, "cli"), (ui(), "owner")] {
        let asset = format!("prop_hand_{kind}");
        let rel = fx.candidate(&asset).await;
        let sha = edit_outside(&fx.path(&rel), format!("edited by hand, {kind}").as_bytes());
        fx.notify(caller, &rel, None).await;
        let entry = fx.edit_with(&asset, &sha).expect("the edit is recorded");
        assert_eq!(entry["actor"], json!({ "kind": kind }), "{entry}");
        assert!(
            !AGENT_KINDS.iter().any(|k| entry["actor"]["kind"] == *k),
            "{entry}"
        );
        assert!(entry.get("relabelled").is_none(), "{entry}");
    }
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-034
async fn mcp_notify_without_by_is_an_agent_edit() {
    let fx = unwatched();
    for (caller, kind) in [
        (CallerKind::Mcp, "claude_code"),
        (CallerKind::Assistant, "tether"),
    ] {
        let asset = format!("prop_agent_{kind}");
        let rel = fx.candidate(&asset).await;
        let sha = edit_outside(&fx.path(&rel), format!("edited by {kind}").as_bytes());
        fx.notify(caller.clone(), &rel, None).await;
        let entry = fx.edit_with(&asset, &sha).expect("the edit is recorded");
        assert_eq!(entry["actor"], json!({ "kind": kind }), "{entry}");
        // Its own kind with a session is accepted too.
        let sha = edit_outside(&fx.path(&rel), format!("edited again by {kind}").as_bytes());
        let by = json!({"kind": kind, "session_id": "s2"});
        fx.notify(caller, &rel, Some(by.clone())).await;
        assert_eq!(fx.edit_with(&asset, &sha).unwrap()["actor"], by);
    }
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-034
async fn an_agent_by_of_another_kind_is_refused() {
    let fx = unwatched();
    let rel = fx.candidate(KITE).await;
    edit_outside(&fx.path(&rel), b"edited by an agent");
    for (caller, kind) in [
        (CallerKind::Mcp, "tether"),
        (CallerKind::Mcp, "director"),
        (CallerKind::Mcp, "cli"),
        (CallerKind::Mcp, "core"),
        (CallerKind::Assistant, "claude_code"),
        (CallerKind::Assistant, "director"),
        (CallerKind::Assistant, "cli"),
        (CallerKind::Assistant, "core"),
    ] {
        let e = fx
            .err_by(
                caller.clone(),
                "watch.notify",
                json!({"paths": [rel], "by": {"kind": kind}}),
            )
            .await;
        assert_eq!(e.code, "params.invalid", "{caller} as {kind}: {e:?}");
        assert_eq!(
            fx.edit_count(KITE),
            0,
            "{caller} as {kind} recorded nothing"
        );
    }
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-034
async fn by_owner_is_refused() {
    let fx = unwatched();
    let rel = fx.candidate(KITE).await;
    edit_outside(&fx.path(&rel), b"edited by someone");
    for (caller, _) in callers() {
        let e = fx
            .err_by(
                caller.clone(),
                "watch.notify",
                json!({"paths": [rel], "by": {"kind": "owner"}}),
            )
            .await;
        assert_eq!(e.code, "params.invalid", "{caller}: {e:?}");
        assert_eq!(fx.edit_count(KITE), 0, "{caller} recorded nothing");
    }
    // An unknown kind, or an unknown key, is no actor either.
    for by in [
        json!({"kind": "someone"}),
        json!({"kind": "claude_code", "name": "x"}),
    ] {
        let e = fx
            .err_by(
                CallerKind::Cli,
                "watch.notify",
                json!({"paths": [rel], "by": by}),
            )
            .await;
        assert_eq!(e.code, "params.invalid", "{by}: {e:?}");
    }
    assert_eq!(fx.edit_count(KITE), 0);
    fx.shutdown().await;
}

/// No tool argument signs off as `auto` (ruling 4): `asset.signoff` has no `by`. The sign-off
/// path with `auto` is `pipeline::signoff`'s unit test
/// `auto_approve_signoff_is_by_auto_with_via_rule`.
#[tokio::test] // spec: CORE-051
async fn no_tool_argument_signs_off_as_auto() {
    let fx = Fx::plain();
    let run = fx.import_run(KITE, "kite.png", b"kite").await;
    let e = fx
        .err(
            "asset.signoff",
            Fx::signoff_args(KITE, &run, "kite.png", json!({"by": "auto"})),
        )
        .await;
    assert_eq!(e.code, "params.invalid", "{e:?}");
    let m = fx.manifest(KITE);
    assert!(
        m["candidate_runs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["signed_off"].is_null()),
        "{m}"
    );
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-034, APP-039
async fn ai_edit_relabels_one_entry_from_the_app_only() {
    let fx = unwatched();
    let rel = fx.candidate(KITE).await;
    let inside = rel.split_once(&format!("{KITE}/")).unwrap().1.to_string();
    let one = edit_outside(&fx.path(&rel), b"first outside edit");
    fx.notify(CallerKind::Cli, &rel, None).await;
    let two = edit_outside(&fx.path(&rel), b"second outside edit");
    fx.notify(CallerKind::Cli, &rel, None).await;
    let before = fx.manifest(KITE);

    let params = |sha256: &str| lil_protocol::app::AiEditParams {
        project: Some("sketchbook".to_string()),
        asset_id: KITE.to_string(),
        path: inside.clone(),
        sha256: sha256.to_string(),
    };
    let app = fx.session(ui());
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    fx.events.attach(15, tx);
    let topic = format!("asset:lil://{}/sketchbook/{KITE}", fx.ws_ref.id);
    fx.events
        .subscribe(15, std::slice::from_ref(&topic), &Default::default())
        .await;
    drain_events(&mut rx);
    let r = fx
        .dispatcher
        .ai_edit(&app, params(&one), "t_ai_edit")
        .await
        .unwrap();
    // Fix round 1, M1: the asset's views hear that its manifest changed.
    let manifest_event =
        |e: &Value| e["topic"] == topic.as_str() && e["data"] == json!({"kind": "manifest"});
    let deadline = tokio::time::Instant::now() + DEADLINE;
    let mut published = drain_events(&mut rx);
    while !published.iter().any(manifest_event) && tokio::time::Instant::now() < deadline {
        tokio::task::yield_now().await;
        published.extend(drain_events(&mut rx));
    }
    assert!(published.iter().any(manifest_event), "{published:?}");
    let edit = &r.human_edit;
    assert_eq!(edit["sha256"], one.as_str(), "{edit}");
    assert_eq!(edit["path"], inside.as_str(), "{edit}");
    assert_eq!(edit["actor"], json!({"kind": "claude_code"}), "{edit}");
    assert!(edit["relabelled"]["utc"].is_string(), "{edit}");
    assert_eq!(fx.edit_with(KITE, &one).as_ref(), Some(edit));
    // Nothing relabels in bulk: the other entry is as it was.
    let other = |m: &Value| m["human_edits"].as_array().unwrap()[1].clone();
    assert_eq!(other(&fx.manifest(KITE)), other(&before));
    assert_eq!(
        fx.edit_with(KITE, &two).unwrap()["actor"],
        json!({"kind": "cli"})
    );

    // No entry with that path and hash, or no such asset: nothing changes.
    let after = fx.manifest(KITE);
    for p in [
        params(&sha256_bytes(b"never written")),
        lil_protocol::app::AiEditParams {
            path: "candidates/elsewhere/kite.png".to_string(),
            ..params(&two)
        },
        lil_protocol::app::AiEditParams {
            asset_id: "prop_never".to_string(),
            ..params(&two)
        },
    ] {
        let e = fx
            .dispatcher
            .ai_edit(&app, p, "t_ai_edit")
            .await
            .unwrap_err();
        assert_eq!(e.code, "asset.unknown", "{e:?}");
        assert_eq!(e.trace_id, "t_ai_edit");
    }
    assert_eq!(fx.manifest(KITE), after);
    // Only a verified app may relabel: the method's own check, besides the router's.
    let e = fx
        .dispatcher
        .ai_edit(&fx.session(CallerKind::Cli), params(&two), "t_ai_edit")
        .await
        .unwrap_err();
    assert_eq!(e.code, "caller.not_allowed", "{e:?}");
    assert_eq!(fx.manifest(KITE), after);
    fx.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Task 15, audit fix round 1: a report attributes only the files it names (M1); `by` never
// names the core, and its ids are capped (L3).
// ---------------------------------------------------------------------------------------------

const KITE_FOLDER: &str = "projects/sketchbook/assets/prop_kite";

/// M1 (the audit's probe 1a): your hand edit, reported an hour later by an agent that names
/// only the asset's folder, stays a human edit: a folder report reconciles its subtree but
/// attributes nothing.
#[tokio::test] // spec: CORE-034, ARCH-103
async fn a_folder_report_never_attributes_a_hand_edit() {
    let fx = unwatched();
    let rel = fx.candidate(KITE).await;
    let sha = edit_outside(&fx.path(&rel), b"your hand edit");
    fx.clock.advance(Duration::from_secs(3600));
    let r = fx.notify(CallerKind::Mcp, KITE_FOLDER, None).await;
    assert_eq!(r["reconciled"], json!([KITE_FOLDER]), "{r}");
    let entry = fx.edit_with(KITE, &sha).expect("the edit is recorded");
    assert!(entry.get("actor").is_none(), "still yours: {entry}");
    fx.shutdown().await;
}

/// M1 (the audit's probe 1b): an app's report of the asset's folder doesn't make Claude Code's
/// write yours, and the hook's report of the file, which comes next, still claims it.
#[tokio::test] // spec: CORE-034, ARCH-103
async fn a_folder_report_leaves_the_claim_to_the_files_reporter() {
    let fx = unwatched();
    let rel = fx.candidate(KITE).await;
    let sha = edit_outside(&fx.path(&rel), b"written by Claude Code");
    fx.notify(ui(), KITE_FOLDER, None).await;
    let entry = fx.edit_with(KITE, &sha).expect("the edit is recorded");
    assert!(
        entry.get("actor").is_none(),
        "no owner from a folder: {entry}"
    );
    let by = json!({"kind": "claude_code", "session_id": "s1"});
    fx.notify(CallerKind::Cli, &rel, Some(by.clone())).await;
    assert_eq!(fx.edit_with(KITE, &sha).unwrap()["actor"], by);
    fx.shutdown().await;
}

/// M1: a report that names the file still attributes its change, also when it names the
/// file's folder too.
#[tokio::test] // spec: CORE-034, ARCH-103
async fn an_exact_file_report_still_attributes() {
    let fx = unwatched();
    let rel = fx.candidate(KITE).await;
    let sha = edit_outside(&fx.path(&rel), b"written by Claude Code, named");
    fx.ok_by(
        CallerKind::Mcp,
        "watch.notify",
        json!({"paths": [KITE_FOLDER, rel]}),
    )
    .await;
    let entry = fx.edit_with(KITE, &sha).expect("the edit is recorded");
    assert_eq!(entry["actor"], json!({"kind": "claude_code"}), "{entry}");
    fx.shutdown().await;
}

/// L3: `by` never names the core, from any caller, and records nothing; each of its ids is
/// at most 200 characters.
#[tokio::test] // spec: CORE-034
async fn by_core_is_refused_and_by_ids_are_capped() {
    let fx = unwatched();
    let rel = fx.candidate(KITE).await;
    let sha = edit_outside(&fx.path(&rel), b"edited by someone");
    for (caller, _) in callers() {
        let e = fx
            .err_by(
                caller.clone(),
                "watch.notify",
                json!({"paths": [rel], "by": {"kind": "core"}}),
            )
            .await;
        assert_eq!(e.code, "params.invalid", "{caller}: {e:?}");
        assert_eq!(fx.edit_count(KITE), 0, "{caller} recorded nothing");
    }
    let long = "x".repeat(201);
    for key in ["task_id", "conversation_id", "session_id"] {
        let mut by = json!({"kind": "claude_code"});
        by[key] = json!(long);
        let e = fx
            .err_by(
                CallerKind::Cli,
                "watch.notify",
                json!({"paths": [rel], "by": by}),
            )
            .await;
        assert_eq!(e.code, "params.invalid", "{key}: {e:?}");
        assert_eq!(fx.edit_count(KITE), 0, "{key}: nothing recorded");
    }
    let at_cap = "é".repeat(200);
    let by = json!({"kind": "claude_code", "session_id": at_cap});
    fx.notify(CallerKind::Cli, &rel, Some(by.clone())).await;
    assert_eq!(fx.edit_with(KITE, &sha).unwrap()["actor"], by);
    fx.shutdown().await;
}
