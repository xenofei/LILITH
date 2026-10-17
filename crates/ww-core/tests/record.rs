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

use serde_json::{Value, json};
use ww_core::events::EventBus;
use ww_core::pipeline::RecordPlan;
use ww_core::session::Session;
use ww_core::state::StateRegistry;
use ww_core::store::FailPoints;
use ww_core::tools::{
    CallCtx, Dispatcher, MeterStage, PipelineHooks, RecordStage, TestGates, WorkspaceRecord,
    core_registry,
};
use ww_core::workspace::{WorkspaceHub, WorkspaceRef};
use ww_platform::fs::{Probe, SystemProbe};
use ww_platform::{Clock, IdSource};
use ww_protocol::{
    CallerKind, EffectClass, ErrorBody, Outcome, ToolCallParams, ToolDescriptor, ToolOutput,
};
use ww_testkit::files::write_file;
use ww_testkit::{FakeClock, SeqIds, TestEnv, sha256_bytes, version_content_hash_of};

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
        id: ww_platform::Env::workspace_hash(&canonical),
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
    env: TestEnv,
    _states: Arc<StateRegistry>,
}

impl Fx {
    fn new(
        setup: impl FnOnce(&Path),
        hooks: impl FnOnce(PipelineHooks) -> PipelineHooks,
        failpoints: Arc<FailPoints>,
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
        let clock: Arc<dyn Clock> = FakeClock::new();
        let events = Arc::new(EventBus::new(clock.clone()));
        let ids: Arc<dyn IdSource> = Arc::new(SeqIds::new());
        let probe: Arc<dyn Probe> = Arc::new(SystemProbe);
        let hub = WorkspaceHub::new(
            env.env.clone(),
            states.clone(),
            events.clone(),
            clock.clone(),
            ids.clone(),
            failpoints,
            probe.clone(),
            ww_core::watch::WatchConfig::default(),
        );
        let gates = Arc::new(TestGates::new());
        let reg = core_registry(true, &gates, &states, &hub, &env.env, &probe);
        let lifecycle = ww_core::Lifecycle::new(clock.clone(), ww_core::lifecycle::IDLE_EXIT_AFTER);
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
            env,
            _states: states,
        }
    }

    fn plain() -> Fx {
        Fx::new(|_| {}, |h| h, FailPoints::none())
    }

    fn call(&self, name: &str, args: Value) -> impl Future<Output = Outcome<ToolOutput>> + use<> {
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
        let session = Arc::new(Session {
            id: "s_record".into(),
            caller: CallerKind::Cli,
            non_interactive: false,
            workspace: Some(self.ws_ref.clone()),
            capabilities: Vec::new(),
            client_pid: 1,
            profile_id: None,
        });
        let params = ToolCallParams {
            name: name.into(),
            arguments,
            idempotency_key: write.then(key),
            trace_id: None,
        };
        let dispatcher = self.dispatcher.clone();
        async move { dispatcher.call(session, params).await }
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
    fn settle(&self, _s: &Session, _out: &Outcome<ToolOutput>) {
        let ws = self.ws.lock().unwrap().clone();
        let manifest = file(&ws, &format!("{KITE_DIR}/{KITE}.manifest.json"));
        let staging = file(&ws, ".worldwright/staging");
        let staged = std::fs::read_dir(&staging)
            .map(|d| d.flatten().any(|h| h.path().join("kite.png").is_file()))
            .unwrap_or(false);
        self.log.push(format!(
            "meter manifest={} staged={staged}",
            manifest.exists()
        ));
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

/// The `ww/event` lines a client attached with `rx` got, parsed, without waiting.
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
    let topic = format!("asset:ww://{}/sketchbook/{KITE}", fx.ws_ref.id);
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
    // The second read the manifest only after the first's edit: it sees `approved`, which
    // can't take another sign-off. Had it read before, it would have tried v1.0.0 again.
    let two = tokio::time::timeout(DEADLINE, two).await.unwrap().unwrap();
    match two {
        Outcome::Error { error } => {
            assert_eq!(error.code, "status.invalid_transition", "{error:?}");
            assert!(error.message.contains("approved"), "{error:?}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(fx.manifest(KITE)["approved_version"], "1.0.0");
    assert!(lock.try_lock().is_ok());
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
            "ww://{}/sketchbook/dep_stale@1.0.0#sha256={hash}",
            fx.ws_ref.id
        )]),
        "{r}"
    );
    assert_eq!(
        r["handoff"]["unlocks"],
        json!([format!("ww://{}/sketchbook/dep_waiting", fx.ws_ref.id)]),
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
}
