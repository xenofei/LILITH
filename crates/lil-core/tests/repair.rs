//! Moved and copied asset folders (CORE-154, CORE-047, ARCH-125; Appendix H "Moved and copied
//! folders"; P4b-R34): the `repair` the index computes from `creation_uuid`, `project.status`'s
//! `moved` and `duplicates`, `asset.accept_move` (redirect first, then the manifest; a retry
//! after a crash finishes the work) and `asset.make_independent`, and that nothing repairs by
//! itself. Driven through the dispatcher of an in-process core with the fake clock and ids
//! (QA-031), on a copy of the spec fixture workspace; folders are moved and copied as Explorer
//! would, and a core restart is a new core over the same suite state and workspace.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use lil_core::events::EventBus;
use lil_core::index::Scope;
use lil_core::session::Session;
use lil_core::state::StateRegistry;
use lil_core::store::FailPoints;
use lil_core::tools::{Dispatcher, PipelineHooks, TestGates, core_registry};
use lil_core::watch::WatchConfig;
use lil_core::workspace::{OpenWorkspace, WorkspaceHub, WorkspaceRef};
use lil_platform::fs::{Probe, SystemProbe};
use lil_platform::{Clock, IdSource};
use lil_protocol::{CallerKind, EffectClass, ErrorBody, Outcome, ToolCallParams};
use lil_testkit::files::write_file;
use lil_testkit::{FakeClock, SeqIds, TestEnv};
use serde_json::{Value, json};

const P: &str = "sketchbook";
const CRANE: &str = "chr_paper_crane";
const LAMP: &str = "prop_signal_lamp";
const ORIGAMI: &str = "chr_origami";
const COPY: &str = "chr_paper_crane_copy";
/// The crane's `creation_uuid` in the fixture.
const CRANE_UUID: &str = "6f1c2a3e-8b4d-4c5e-9a7b-1d2e3f405162";

fn key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("repair-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

fn file(ws: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(ws.to_path_buf(), |p, c| p.join(c))
}

fn asset_dir_in(ws: &Path, project: &str, asset: &str) -> PathBuf {
    file(ws, &format!("projects/{project}/assets/{asset}"))
}

fn asset_dir(ws: &Path, asset: &str) -> PathBuf {
    asset_dir_in(ws, P, asset)
}

fn read_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

/// The one `*.manifest.json` of a folder (a moved or copied one keeps its old file name).
fn manifest_in(dir: &Path) -> PathBuf {
    let found: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(".manifest.json"))
        .collect();
    assert_eq!(found.len(), 1, "{found:?}");
    found.into_iter().next().unwrap()
}

/// Renames a file or folder as an outside program (Explorer) would.
#[allow(clippy::disallowed_methods)]
fn outside_rename(from: &Path, to: &Path) {
    std::fs::rename(from, to).unwrap_or_else(|e| panic!("cannot rename {}: {e}", from.display()));
}

/// Makes a folder (and those above it) as an outside program would.
#[allow(clippy::disallowed_methods)]
fn outside_mkdir(path: &Path) {
    std::fs::create_dir_all(path).unwrap_or_else(|e| panic!("cannot make {}: {e}", path.display()));
}

/// Copies a folder as an outside program would.
#[allow(clippy::disallowed_methods)]
fn outside_copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap_or_else(|e| panic!("{}: {e}", to.display()));
    for entry in std::fs::read_dir(from).unwrap_or_else(|e| panic!("{}: {e}", from.display())) {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            outside_copy(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target)
                .unwrap_or_else(|e| panic!("{}: {e}", entry.path().display()));
        }
    }
}

/// Edits the one manifest of a folder by hand.
fn hand_edit(dir: &Path, f: impl FnOnce(&mut Value)) {
    let path = manifest_in(dir);
    let mut m = read_json(&path);
    f(&mut m);
    write_file(&path, serde_json::to_string_pretty(&m).unwrap().as_bytes());
}

fn ws_ref(ws: &Path) -> WorkspaceRef {
    let canonical = std::fs::canonicalize(ws).expect("canonicalize");
    let text = canonical.to_string_lossy().into_owned();
    WorkspaceRef {
        id: lil_platform::Env::workspace_hash(&canonical),
        path: PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text)),
    }
}

/// An in-process core's dispatcher over a copy of the fixture workspace.
struct Fx {
    dispatcher: Dispatcher,
    hub: Arc<WorkspaceHub>,
    events: Arc<EventBus>,
    ws_ref: WorkspaceRef,
    ws: PathBuf,
    states: Arc<StateRegistry>,
    env: TestEnv,
}

impl Fx {
    fn with(setup: impl FnOnce(&Path)) -> Fx {
        Fx::with_fp(setup, FailPoints::none())
    }

    fn with_fp(setup: impl FnOnce(&Path), fp: Arc<FailPoints>) -> Fx {
        Fx::build(setup, fp, WatchConfig::default())
    }

    /// [`Fx::with`], with a watcher that collects changes for an hour: it never applies one
    /// during the test, so the index sees an outside change only when a call reconciles it.
    fn with_idle_watcher(setup: impl FnOnce(&Path)) -> Fx {
        let watch = WatchConfig {
            batch: Duration::from_secs(3600),
            ..WatchConfig::default()
        };
        Fx::build(setup, FailPoints::none(), watch)
    }

    /// [`Fx::with`], the store, index and watcher seeing the disk through `probe`.
    fn with_probe(setup: impl FnOnce(&Path), probe: Arc<dyn Probe>) -> Fx {
        let env = TestEnv::new();
        let ws = env.copy_fixture_workspace("repair");
        setup(&ws);
        env.register_workspace(&ws, true);
        Fx::start_with(env, ws, FailPoints::none(), WatchConfig::default(), probe)
    }

    /// [`Fx::with_probe`], with the idle watcher of [`Fx::with_idle_watcher`].
    fn with_probe_idle(setup: impl FnOnce(&Path), probe: Arc<dyn Probe>) -> Fx {
        let env = TestEnv::new();
        let ws = env.copy_fixture_workspace("repair");
        setup(&ws);
        env.register_workspace(&ws, true);
        let watch = WatchConfig {
            batch: Duration::from_secs(3600),
            ..WatchConfig::default()
        };
        Fx::start_with(env, ws, FailPoints::none(), watch, probe)
    }

    fn build(setup: impl FnOnce(&Path), fp: Arc<FailPoints>, watch: WatchConfig) -> Fx {
        let env = TestEnv::new();
        let ws = env.copy_fixture_workspace("repair");
        setup(&ws);
        env.register_workspace(&ws, true);
        Fx::start_with(env, ws, fp, watch, Arc::new(SystemProbe))
    }

    /// A core over `env`'s suite state and the workspace `ws`.
    fn start(env: TestEnv, ws: PathBuf, fp: Arc<FailPoints>) -> Fx {
        Fx::start_with(env, ws, fp, WatchConfig::default(), Arc::new(SystemProbe))
    }

    fn start_with(
        env: TestEnv,
        ws: PathBuf,
        fp: Arc<FailPoints>,
        watch: WatchConfig,
        probe: Arc<dyn Probe>,
    ) -> Fx {
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
        let hub = WorkspaceHub::new(
            env.env.clone(),
            states.clone(),
            events.clone(),
            clock.clone(),
            ids.clone(),
            fp,
            probe.clone(),
            watch,
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
        );
        let lifecycle =
            lil_core::Lifecycle::new(clock.clone(), lil_core::lifecycle::IDLE_EXIT_AFTER);
        let dispatcher = Dispatcher::new(
            reg,
            PipelineHooks::plan2(),
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
            states,
            env,
        }
    }

    /// Stops this core; returns the suite's env and the workspace.
    async fn stop(self) -> (TestEnv, PathBuf) {
        let Fx {
            dispatcher,
            hub,
            states,
            env,
            ws,
            ..
        } = self;
        hub.close_all().await;
        states.close();
        drop((dispatcher, hub, states));
        (env, ws)
    }

    /// A core restart: a new core (no fail points) over the same suite state and workspace.
    async fn restart(self) -> Fx {
        let (env, ws) = self.stop().await;
        Fx::start(env, ws, FailPoints::none())
    }

    async fn shutdown(self) {
        let (env, _) = self.stop().await;
        if let Err(left) = env.close() {
            panic!("the test's folder was left behind: {left:?}");
        }
    }

    async fn open(&self) -> Arc<OpenWorkspace> {
        self.hub
            .open(&self.ws_ref)
            .await
            .expect("the workspace opens")
    }

    /// A whole reconcile, as the watcher's rescan runs it.
    async fn reconcile(&self) {
        let open = self.open().await;
        open.index.ready().await;
        open.index.reconcile(Scope::All).await.expect("a reconcile");
    }

    async fn call(&self, name: &str, args: Value) -> Outcome<lil_protocol::ToolOutput> {
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
            id: "s_repair".into(),
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
        self.dispatcher.call(session, params).await
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

    async fn get(&self, asset: &str) -> Value {
        self.ok("asset.get", json!({"project": P, "asset_id": asset}))
            .await
    }

    async fn status(&self) -> Value {
        self.status_of(P).await
    }

    async fn status_of(&self, project: &str) -> Value {
        self.ok("project.status", json!({ "project": project }))
            .await
    }

    async fn accept(&self, asset: &str) -> Outcome<lil_protocol::ToolOutput> {
        self.call(
            "asset.accept_move",
            json!({"project": P, "asset_id": asset}),
        )
        .await
    }

    async fn independent(&self, asset: &str) -> Outcome<lil_protocol::ToolOutput> {
        self.call(
            "asset.make_independent",
            json!({"project": P, "asset_id": asset}),
        )
        .await
    }

    /// The project's asset ids, as `assets.query` lists them.
    async fn ids(&self) -> Vec<String> {
        let page = self.ok("assets.query", json!({"project": P})).await;
        page["assets"]
            .as_array()
            .unwrap_or_else(|| panic!("{page}"))
            .iter()
            .map(|a| a["asset_id"].as_str().unwrap().to_string())
            .collect()
    }

    fn redirects(&self) -> Option<String> {
        std::fs::read_to_string(file(&self.ws, "registry/redirects.yaml")).ok()
    }

    fn ws_id(&self) -> String {
        self.ws_ref.id.clone()
    }
}

fn structured(o: Outcome<lil_protocol::ToolOutput>) -> Value {
    match o {
        Outcome::Ok(out) => out.structured,
        other => panic!("expected success: {other:?}"),
    }
}

fn error(o: Outcome<lil_protocol::ToolOutput>) -> ErrorBody {
    match o {
        Outcome::Error { error } => error,
        other => panic!("expected an error: {other:?}"),
    }
}

fn moved_entry(from: &str, to: &str) -> Value {
    json!({"from": format!("{P}/{from}"), "to": format!("{P}/{to}")})
}

fn crane_renamed(ws: &Path) {
    outside_rename(&asset_dir(ws, CRANE), &asset_dir(ws, ORIGAMI));
}

fn crane_copied(ws: &Path) {
    outside_copy(&asset_dir(ws, CRANE), &asset_dir(ws, COPY));
}

/// The redirect entries' count in `text`.
fn entries(text: &str) -> usize {
    text.matches("- from:").count()
}

#[tokio::test] // spec: CORE-154, CORE-047
async fn a_folder_renamed_outside_is_moved() {
    // Renamed while no core ran (the first open is a cold rebuild).
    let fx = Fx::with(crane_renamed);
    let detail = fx.get(ORIGAMI).await;
    assert_eq!(
        detail["repair"],
        json!({"kind": "moved", "from": format!("{P}/{CRANE}"), "to": format!("{P}/{ORIGAMI}")}),
        "{detail}"
    );
    assert_eq!(detail["needs_repair"], true);
    let status = fx.status().await;
    assert_eq!(
        status["moved"],
        json!([moved_entry(CRANE, ORIGAMI)]),
        "{status}"
    );
    assert_eq!(status["duplicates"], json!([]), "{status}");
    let manifest = format!("projects/{P}/assets/{ORIGAMI}/{CRANE}.manifest.json");
    assert!(
        status["needs_repair"]
            .as_array()
            .unwrap()
            .contains(&json!(manifest)),
        "{status}"
    );
    // The summary carries it too.
    let page = fx.ok("assets.query", json!({"project": P})).await;
    let row = page["assets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["asset_id"] == ORIGAMI)
        .cloned()
        .unwrap_or_else(|| panic!("{page}"));
    assert_eq!(row["repair"]["kind"], "moved", "{row}");

    // Renamed while a core runs: the index keeps the gone folder's row (its manifest
    // `missing`), and it never counts as a live folder with the UUID.
    outside_rename(
        &asset_dir(&fx.ws, ORIGAMI),
        &asset_dir(&fx.ws, "chr_crane_two"),
    );
    fx.reconcile().await;
    let status = fx.status().await;
    assert_eq!(
        status["moved"],
        json!([moved_entry(CRANE, "chr_crane_two")]),
        "{status}"
    );
    assert_eq!(status["duplicates"], json!([]), "{status}");
    assert_eq!(fx.get("chr_crane_two").await["repair"]["kind"], "moved");
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154, CORE-047
async fn a_folder_copied_outside_is_a_copy_and_the_original_stays_valid() {
    let fx = Fx::with(crane_copied);
    let original_bytes = std::fs::read(manifest_in(&asset_dir(&fx.ws, CRANE))).unwrap();
    let copy = fx.get(COPY).await;
    assert_eq!(
        copy["repair"],
        json!({"kind": "copy", "other": format!("{P}/{CRANE}")}),
        "{copy}"
    );
    assert_eq!(copy["needs_repair"], true);
    // The original needs repair too (you choose), but stays what refs reach.
    let original = fx.get(CRANE).await;
    assert_eq!(
        original["repair"],
        json!({"kind": "duplicate_uuid", "other": format!("{P}/{COPY}")}),
        "{original}"
    );
    assert_eq!(original["needs_repair"], true);
    let resolved = fx
        .ok(
            "ref.resolve",
            json!({"ref": format!("lil://{}/{P}/{CRANE}@1.0.0", fx.ws_id())}),
        )
        .await;
    assert_eq!(resolved["asset_id"], CRANE, "{resolved}");
    assert_eq!(resolved["redirected_from"], json!([]), "{resolved}");
    let status = fx.status().await;
    assert_eq!(
        status["duplicates"],
        json!([[
            format!("projects/{P}/assets/{CRANE}"),
            format!("projects/{P}/assets/{COPY}")
        ]]),
        "{status}"
    );
    assert_eq!(status["moved"], json!([]), "{status}");
    // A copy is no move: accept_move refuses it, and nothing is written.
    let e = error(fx.accept(COPY).await);
    assert_eq!(e.code, "repair.not_needed", "{e:?}");
    assert!(
        e.message
            .starts_with("This asset doesn't need that repair."),
        "{}",
        e.message
    );
    assert!(fx.redirects().is_none());
    assert_eq!(
        std::fs::read(manifest_in(&asset_dir(&fx.ws, CRANE))).unwrap(),
        original_bytes
    );
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154
async fn both_agreeing_are_duplicate_uuid() {
    let fx = Fx::with(|ws| {
        crane_copied(ws);
        let dir = asset_dir(ws, COPY);
        outside_rename(
            &dir.join(format!("{CRANE}.manifest.json")),
            &dir.join(format!("{COPY}.manifest.json")),
        );
        hand_edit(&dir, |m| m["asset_id"] = json!(COPY));
    });
    for (asset, other) in [(CRANE, COPY), (COPY, CRANE)] {
        let d = fx.get(asset).await;
        assert_eq!(
            d["repair"],
            json!({"kind": "duplicate_uuid", "other": format!("{P}/{other}")}),
            "{d}"
        );
        assert_eq!(d["needs_repair"], true);
    }
    let status = fx.status().await;
    assert_eq!(
        status["duplicates"].as_array().unwrap().len(),
        1,
        "{status}"
    );
    // Either may be made independent; the other is then fine.
    let made = structured(fx.independent(CRANE).await);
    assert_ne!(made["creation_uuid"], CRANE_UUID);
    assert!(fx.get(COPY).await["repair"].is_null());
    assert!(fx.get(CRANE).await["repair"].is_null());
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154, ARCH-125
async fn accept_move_edits_only_project_and_asset_id_and_records_a_redirect() {
    let fx = Fx::with(|ws| {
        // The lamp uses the crane's sealed version.
        hand_edit(&asset_dir(ws, LAMP), |m| {
            m["inputs"] = json!([{"ref": format!("{CRANE}@1.0.0"), "role": "reference"}]);
        });
        crane_renamed(ws);
    });
    let lamp_manifest = manifest_in(&asset_dir(&fx.ws, LAMP));
    let lamp_bytes = std::fs::read(&lamp_manifest).unwrap();
    let before = read_json(&manifest_in(&asset_dir(&fx.ws, ORIGAMI)));

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    fx.events.attach(9, tx);
    let topics = [
        format!("asset:lil://{}/{P}/{ORIGAMI}", fx.ws_id()),
        format!("project:{P}"),
    ];
    fx.events.subscribe(9, &topics, &Default::default()).await;

    let r = structured(fx.accept(ORIGAMI).await);
    assert_eq!(
        r,
        json!({"project": P, "asset_id": ORIGAMI,
               "redirect": {"from": format!("{P}/{CRANE}"), "to": format!("{P}/{ORIGAMI}")},
               "warnings": []})
    );
    // The redirect, by the caller.
    let redirects = fx.redirects().expect("registry/redirects.yaml");
    assert_eq!(entries(&redirects), 1, "{redirects}");
    assert!(
        redirects.contains(&format!("from: {P}/{CRANE}"))
            && redirects.contains(&format!("to: {P}/{ORIGAMI}"))
            && redirects.contains("by: cli"),
        "{redirects}"
    );
    // The manifest: renamed to its folder's name, only `asset_id` changed (its project is
    // the same).
    let dir = asset_dir(&fx.ws, ORIGAMI);
    assert!(!dir.join(format!("{CRANE}.manifest.json")).exists());
    let after = read_json(&dir.join(format!("{ORIGAMI}.manifest.json")));
    let mut expected = before.clone();
    expected["asset_id"] = json!(ORIGAMI);
    assert_eq!(after, expected);
    assert_eq!(
        after.as_object().unwrap().keys().collect::<Vec<_>>(),
        before.as_object().unwrap().keys().collect::<Vec<_>>(),
        "the keys keep their order"
    );
    // Healthy now.
    let d = fx.get(ORIGAMI).await;
    assert!(d["repair"].is_null(), "{d}");
    assert_eq!(d["needs_repair"], false);
    let status = fx.status().await;
    assert_eq!(status["moved"], json!([]), "{status}");
    // Dependents untouched; they resolve through the redirect.
    assert_eq!(std::fs::read(&lamp_manifest).unwrap(), lamp_bytes);
    let lamp = fx.get(LAMP).await;
    let used = &lamp["uses"][0]["resolved"];
    assert_eq!(used["asset_id"], ORIGAMI, "{lamp}");
    assert_eq!(
        used["redirected_from"],
        json!([format!("{P}/{CRANE}")]),
        "{lamp}"
    );
    let users = fx.get(ORIGAMI).await["used_by"].clone();
    assert!(
        users
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["ref"] == format!("{P}/{LAMP}")),
        "{users}"
    );
    // The `moved` event, on the asset's topic at its new name and on the project's.
    let want =
        json!({"kind": "moved", "from": format!("{P}/{CRANE}"), "to": format!("{P}/{ORIGAMI}")});
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    while seen.len() < 2 && tokio::time::Instant::now() < deadline {
        while let Ok(line) = rx.try_recv() {
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["params"]["data"]["kind"] == "moved" {
                seen.push((
                    v["params"]["topic"].as_str().unwrap().to_string(),
                    v["params"]["data"].clone(),
                ));
            }
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        seen,
        [(topics[0].clone(), want.clone()), (topics[1].clone(), want)]
    );
    // A second call: nothing to do.
    let e = error(fx.accept(ORIGAMI).await);
    assert_eq!(e.code, "repair.not_needed");
    assert_eq!(entries(&fx.redirects().unwrap()), 1);
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154
async fn accept_move_on_a_healthy_asset_is_repair_not_needed() {
    let fx = Fx::with(crane_renamed);
    let before: Vec<(PathBuf, Vec<u8>)> = [LAMP, ORIGAMI]
        .iter()
        .map(|a| {
            let p = manifest_in(&asset_dir(&fx.ws, a));
            let b = std::fs::read(&p).unwrap();
            (p, b)
        })
        .collect();
    for (tool, asset) in [
        ("asset.accept_move", LAMP),
        ("asset.make_independent", LAMP),
        ("asset.make_independent", ORIGAMI),
    ] {
        let e = fx.err(tool, json!({"project": P, "asset_id": asset})).await;
        assert_eq!(e.code, "repair.not_needed", "{tool} {asset}: {e:?}");
        assert_eq!(e.category, lil_protocol::ErrorCategory::UserFixable);
        assert_eq!(e.retry, lil_protocol::RetryPolicy::Never);
    }
    // An asset that isn't there.
    let e = error(fx.accept("chr_nobody").await);
    assert_eq!(e.code, "asset.unknown", "{e:?}");
    assert!(fx.redirects().is_none(), "nothing written");
    for (p, b) in before {
        assert_eq!(std::fs::read(&p).unwrap(), b, "{}", p.display());
    }
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154, CORE-047
async fn make_independent_gives_a_fresh_uuid_and_clears_both_flags() {
    let fx = Fx::with(crane_copied);
    let crane_bytes = std::fs::read(manifest_in(&asset_dir(&fx.ws, CRANE))).unwrap();
    let before = read_json(&manifest_in(&asset_dir(&fx.ws, COPY)));
    let made = structured(fx.independent(COPY).await);
    let uuid = made["creation_uuid"].as_str().unwrap().to_string();
    assert_ne!(uuid, CRANE_UUID);
    assert_eq!(uuid.len(), 36, "{uuid}");
    let dir = asset_dir(&fx.ws, COPY);
    let path = manifest_in(&dir);
    assert_eq!(path, dir.join(format!("{COPY}.manifest.json")), "renamed");
    let after = read_json(&path);
    let mut expected = before;
    expected["asset_id"] = json!(COPY);
    expected["creation_uuid"] = json!(uuid);
    assert_eq!(after, expected);
    assert_eq!(
        std::fs::read(manifest_in(&asset_dir(&fx.ws, CRANE))).unwrap(),
        crane_bytes,
        "the original is untouched"
    );
    for asset in [CRANE, COPY] {
        let d = fx.get(asset).await;
        assert!(d["repair"].is_null(), "{d}");
        assert_eq!(d["needs_repair"], false, "{d}");
    }
    let status = fx.status().await;
    assert_eq!(status["duplicates"], json!([]), "{status}");
    assert_eq!(status["needs_repair"], json!([]), "{status}");
    // Done: a second call has nothing to do.
    assert_eq!(error(fx.independent(COPY).await).code, "repair.not_needed");
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154
async fn nothing_repairs_by_itself() {
    let fx = Fx::with(|ws| {
        crane_copied(ws);
        outside_rename(&asset_dir(ws, LAMP), &asset_dir(ws, "prop_lamp_moved"));
        // The lamp is a legacy manifest: give the moved one a UUID so it can be `moved`.
        hand_edit(&asset_dir(ws, "prop_lamp_moved"), |m| {
            m["creation_uuid"] = json!("11111111-2222-4333-8444-555555555555");
        });
    });
    let files: Vec<PathBuf> = [CRANE, COPY, "prop_lamp_moved"]
        .iter()
        .map(|a| manifest_in(&asset_dir(&fx.ws, a)))
        .collect();
    let bytes: Vec<Vec<u8>> = files.iter().map(|f| std::fs::read(f).unwrap()).collect();
    let expect = |status: &Value| {
        assert_eq!(
            status["moved"],
            json!([moved_entry(LAMP, "prop_lamp_moved")]),
            "{status}"
        );
        assert_eq!(
            status["duplicates"].as_array().unwrap().len(),
            1,
            "{status}"
        );
    };
    expect(&fx.status().await);
    fx.reconcile().await;
    expect(&fx.status().await);
    let fx = fx.restart().await;
    expect(&fx.status().await);
    fx.reconcile().await;
    fx.open().await.index.rebuild().await.expect("a rebuild");
    expect(&fx.status().await);
    assert_eq!(fx.get(COPY).await["repair"]["kind"], "copy");
    for (f, b) in files.iter().zip(&bytes) {
        assert_eq!(&std::fs::read(f).unwrap(), b, "{}", f.display());
    }
    assert!(fx.redirects().is_none(), "no redirect is written by itself");
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154
async fn a_moved_folder_back_to_its_place_clears_moved() {
    let fx = Fx::with(|_| {});
    fx.open().await.index.ready().await;
    outside_rename(&asset_dir(&fx.ws, CRANE), &asset_dir(&fx.ws, ORIGAMI));
    fx.reconcile().await;
    assert_eq!(
        fx.status().await["moved"],
        json!([moved_entry(CRANE, ORIGAMI)])
    );
    outside_rename(&asset_dir(&fx.ws, ORIGAMI), &asset_dir(&fx.ws, CRANE));
    fx.reconcile().await;
    let status = fx.status().await;
    assert_eq!(status["moved"], json!([]), "{status}");
    assert_eq!(status["duplicates"], json!([]), "{status}");
    assert_eq!(
        status["missing"],
        json!([]),
        "no ghost of the other name: {status}"
    );
    assert_eq!(fx.ids().await, [CRANE, LAMP]);
    let crane = fx.get(CRANE).await;
    assert!(crane["repair"].is_null(), "{crane}");
    assert_eq!(crane["needs_repair"], false);
    // After an accepted move, the folder moved back is `moved` again (to its old name).
    outside_rename(&asset_dir(&fx.ws, CRANE), &asset_dir(&fx.ws, ORIGAMI));
    fx.reconcile().await;
    structured(fx.accept(ORIGAMI).await);
    outside_rename(&asset_dir(&fx.ws, ORIGAMI), &asset_dir(&fx.ws, CRANE));
    fx.reconcile().await;
    assert_eq!(
        fx.status().await["moved"],
        json!([moved_entry(ORIGAMI, CRANE)])
    );
    structured(fx.accept(CRANE).await);
    assert_eq!(entries(&fx.redirects().unwrap()), 2);
    let crane = fx.get(CRANE).await;
    assert!(crane["repair"].is_null(), "{crane}");
    assert_eq!(fx.ids().await, [CRANE, LAMP], "no ghost of chr_origami");
    assert_eq!(fx.status().await["missing"], json!([]));
    // Both old names reach the crane.
    for name in [CRANE, ORIGAMI] {
        let r = fx
            .ok(
                "ref.resolve",
                json!({"ref": format!("lil://{}/{P}/{name}@1.0.0", fx.ws_id())}),
            )
            .await;
        assert_eq!(r["asset_id"], CRANE, "{r}");
    }
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154, CORE-047
async fn assets_without_creation_uuid_are_never_moved_or_copies() {
    let fx = Fx::with(|ws| {
        outside_rename(&asset_dir(ws, LAMP), &asset_dir(ws, "prop_lamp_moved"));
        outside_copy(
            &asset_dir(ws, "prop_lamp_moved"),
            &asset_dir(ws, "prop_lamp_copy"),
        );
    });
    for asset in ["prop_lamp_moved", "prop_lamp_copy"] {
        let d = fx.get(asset).await;
        assert_eq!(d["repair"], json!({"kind": "invalid"}), "{d}");
        assert_eq!(d["needs_repair"], true);
        assert_eq!(error(fx.accept(asset).await).code, "repair.not_needed");
        assert_eq!(error(fx.independent(asset).await).code, "repair.not_needed");
    }
    let status = fx.status().await;
    assert_eq!(status["moved"], json!([]), "{status}");
    assert_eq!(status["duplicates"], json!([]), "{status}");
    assert!(fx.redirects().is_none());
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154, ARCH-125 (P4b-R34)
async fn crash_between_redirect_and_manifest_is_finished_by_the_retry() {
    // A crash right after the redirect file is replaced (`store.after_replace`): the call
    // stops there as a crash would, and the core is restarted.
    let fp = FailPoints::none();
    let armed = Arc::new(AtomicBool::new(false));
    let fire = armed.clone();
    fp.on(
        "store.after_replace",
        Box::new(move || {
            if fire.swap(false, Ordering::SeqCst) {
                Err(std::io::Error::other("killed after the redirect"))
            } else {
                Ok(())
            }
        }),
    );
    let fx = Fx::with_fp(crane_renamed, fp);
    let old_file = asset_dir(&fx.ws, ORIGAMI).join(format!("{CRANE}.manifest.json"));
    let bytes = std::fs::read(&old_file).unwrap();
    armed.store(true, Ordering::SeqCst);
    let e = error(fx.accept(ORIGAMI).await);
    assert_eq!(e.code, "store.io", "{e:?}");
    assert_eq!(entries(&fx.redirects().unwrap()), 1);
    assert_eq!(
        std::fs::read(&old_file).unwrap(),
        bytes,
        "the manifest waits"
    );

    let fx = fx.restart().await;
    assert_eq!(fx.get(ORIGAMI).await["repair"]["kind"], "moved");
    assert_eq!(entries(&fx.redirects().unwrap()), 1);
    let r = structured(fx.accept(ORIGAMI).await);
    assert_eq!(r["redirect"]["from"], format!("{P}/{CRANE}"));
    assert_eq!(entries(&fx.redirects().unwrap()), 1, "no second redirect");
    let m = read_json(&asset_dir(&fx.ws, ORIGAMI).join(format!("{ORIGAMI}.manifest.json")));
    assert_eq!(m["asset_id"], ORIGAMI);
    assert!(fx.get(ORIGAMI).await["repair"].is_null());
    let (env, _) = fx.stop().await;
    env.close().unwrap();

    // A crash after the manifest's rename, before its edit: the second atomic write of the
    // call (the redirect file's is the first) fails before anything moves.
    let fp = FailPoints::none();
    let writes = Arc::new(AtomicUsize::new(usize::MAX));
    let count = writes.clone();
    fp.on(
        "store.write_temp",
        Box::new(move || {
            if count.fetch_add(1, Ordering::SeqCst) == 1 {
                Err(std::io::Error::other("killed before the manifest edit"))
            } else {
                Ok(())
            }
        }),
    );
    let fx = Fx::with_fp(crane_renamed, fp);
    fx.get(ORIGAMI).await;
    writes.store(0, Ordering::SeqCst);
    let e = error(fx.accept(ORIGAMI).await);
    assert!(e.code.starts_with("store."), "{e:?}");
    writes.store(usize::MAX, Ordering::SeqCst);
    let dir = asset_dir(&fx.ws, ORIGAMI);
    let renamed = dir.join(format!("{ORIGAMI}.manifest.json"));
    assert!(renamed.exists() && !dir.join(format!("{CRANE}.manifest.json")).exists());
    assert_eq!(read_json(&renamed)["asset_id"], CRANE, "not edited yet");

    let fx = fx.restart().await;
    assert_eq!(fx.get(ORIGAMI).await["repair"]["kind"], "moved");
    structured(fx.accept(ORIGAMI).await);
    assert_eq!(entries(&fx.redirects().unwrap()), 1, "no second redirect");
    assert_eq!(read_json(&renamed)["asset_id"], ORIGAMI);
    assert!(fx.get(ORIGAMI).await["repair"].is_null());
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154, ARCH-125
async fn a_move_into_another_project_is_accepted_there() {
    let fx = Fx::with(|ws| {
        write_file(
            &file(ws, "projects/harbor/project.yaml"),
            b"name: Harbor\nslug: harbor\ntype: media\nstatus: active\nlinks: []\ncreated: 2026-10-01\n",
        );
        outside_mkdir(&file(ws, "projects/harbor/assets"));
        outside_rename(&asset_dir(ws, CRANE), &asset_dir_in(ws, "harbor", CRANE));
    });
    let entry = json!({"from": format!("{P}/{CRANE}"), "to": format!("harbor/{CRANE}")});
    assert_eq!(fx.status().await["moved"], json!([entry.clone()]));
    assert_eq!(fx.status_of("harbor").await["moved"], json!([entry]));
    let r = fx
        .ok(
            "asset.accept_move",
            json!({"project": "harbor", "asset_id": CRANE}),
        )
        .await;
    assert_eq!(r["redirect"]["to"], format!("harbor/{CRANE}"), "{r}");
    let m = read_json(&manifest_in(&asset_dir_in(&fx.ws, "harbor", CRANE)));
    assert_eq!(
        (m["project"].clone(), m["asset_id"].clone()),
        (json!("harbor"), json!(CRANE))
    );
    let resolved = fx
        .ok(
            "ref.resolve",
            json!({"ref": format!("lil://{}/{P}/{CRANE}@1.0.0", fx.ws_id())}),
        )
        .await;
    assert_eq!(resolved["project"], "harbor", "{resolved}");
    assert_eq!(fx.status().await["moved"], json!([]));
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154
async fn folder_names_that_arent_valid_are_refused_before_anything_is_written() {
    // A case-only rename names the same folder: invalid, not moved.
    let fx = Fx::with(|ws| {
        outside_rename(&asset_dir(ws, CRANE), &asset_dir(ws, "Chr_Paper_Crane"));
        outside_rename(&asset_dir(ws, LAMP), &asset_dir(ws, "prop-lamp"));
        hand_edit(&asset_dir(ws, "prop-lamp"), |m| {
            m["creation_uuid"] = json!("11111111-2222-4333-8444-555555555555");
        });
    });
    let status = fx.status().await;
    assert_eq!(
        status["moved"],
        json!([{"from": format!("{P}/{LAMP}"), "to": format!("{P}/prop-lamp")}]),
        "{status}"
    );
    let page = fx.ok("assets.query", json!({"project": P})).await;
    let crane = page["assets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["asset_id"] == "Chr_Paper_Crane")
        .cloned()
        .unwrap_or_else(|| panic!("{page}"));
    assert_eq!(crane["repair"], json!({"kind": "invalid"}), "{crane}");
    for (tool, asset, folder) in [
        ("asset.accept_move", CRANE, "Chr_Paper_Crane"),
        ("asset.make_independent", CRANE, "Chr_Paper_Crane"),
        ("asset.accept_move", "Chr_Paper_Crane", "Chr_Paper_Crane"),
        ("asset.accept_move", "prop-lamp", "prop-lamp"),
    ] {
        let e = fx.err(tool, json!({"project": P, "asset_id": asset})).await;
        assert_eq!(e.code, "params.invalid", "{tool} {asset}: {e:?}");
        assert!(
            e.message.contains(&format!("projects/{P}/assets/{folder}"))
                && e.message.contains("Rename the folder"),
            "{}",
            e.message
        );
    }
    assert!(fx.redirects().is_none());
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154
async fn a_move_and_a_copy_at_once_need_a_choice() {
    let fx = Fx::with(|ws| {
        crane_copied(ws);
        crane_renamed(ws);
    });
    for (asset, other) in [(ORIGAMI, COPY), (COPY, ORIGAMI)] {
        let d = fx.get(asset).await;
        let mut sorted = [asset, other];
        sorted.sort_unstable();
        assert_eq!(d["repair"]["kind"], "duplicate_uuid", "{d}");
        assert_eq!(d["repair"]["other"], format!("{P}/{other}"), "{d}");
        assert_eq!(error(fx.accept(asset).await).code, "repair.not_needed");
    }
    structured(fx.independent(COPY).await);
    assert_eq!(fx.get(ORIGAMI).await["repair"]["kind"], "moved");
    structured(fx.accept(ORIGAMI).await);
    for asset in [ORIGAMI, COPY] {
        assert!(fx.get(asset).await["repair"].is_null());
    }
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154
async fn a_stale_index_row_never_decides() {
    // The original comes back at its old place while the index still says `moved`: the repair
    // looks at the disk first, and the folder is a copy now.
    let fx = Fx::with(crane_renamed);
    assert_eq!(fx.get(ORIGAMI).await["repair"]["kind"], "moved");
    outside_copy(&asset_dir(&fx.ws, ORIGAMI), &asset_dir(&fx.ws, CRANE));
    let e = error(fx.accept(ORIGAMI).await);
    assert_eq!(e.code, "repair.not_needed", "{e:?}");
    assert!(fx.redirects().is_none());
    assert_eq!(fx.get(ORIGAMI).await["repair"]["kind"], "copy");
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154, CORE-047 (T8 I1, probe P8)
async fn a_copy_with_a_hand_edited_uuid_is_a_copy_of_its_live_original() {
    // Only the copy's manifest changes (its creation UUID, by hand): it is then alone with its
    // UUID, but the place its manifest names is a live folder, so it is no move (CORE-154:
    // "while its old folder is gone") but a copy of that folder; the crane needs no repair.
    let fx = Fx::with(crane_copied);
    assert_eq!(fx.get(COPY).await["repair"]["kind"], "copy");
    hand_edit(&asset_dir(&fx.ws, COPY), |m| {
        m["creation_uuid"] = json!("11111111-2222-4333-8444-555555555555");
    });
    fx.reconcile().await;
    let crane = fx.get(CRANE).await;
    assert!(crane["repair"].is_null(), "{crane}");
    assert_eq!(crane["needs_repair"], false, "{crane}");
    assert_eq!(
        fx.get(COPY).await["repair"],
        json!({"kind": "copy", "other": format!("{P}/{CRANE}")})
    );
    let status = fx.status().await;
    assert_eq!(status["moved"], json!([]), "{status}");
    assert_eq!(
        status["duplicates"],
        json!([[
            format!("projects/{P}/assets/{CRANE}"),
            format!("projects/{P}/assets/{COPY}")
        ]]),
        "{status}"
    );
    assert_eq!(error(fx.accept(COPY).await).code, "repair.not_needed");
    assert!(fx.redirects().is_none());
    // The copy's repair is making it independent.
    structured(fx.independent(COPY).await);
    assert!(fx.get(COPY).await["repair"].is_null());
    assert_eq!(fx.status().await["duplicates"], json!([]));
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154, ARCH-125 (T8 I1, probe P8b)
async fn an_original_that_leaves_never_sends_its_refs_to_a_copy() {
    let fx = Fx::with(crane_copied);
    hand_edit(&asset_dir(&fx.ws, COPY), |m| {
        m["creation_uuid"] = json!("11111111-2222-4333-8444-555555555555");
    });
    fx.reconcile().await;
    assert_eq!(error(fx.accept(COPY).await).code, "repair.not_needed");
    assert!(
        fx.redirects().is_none(),
        "no redirect from the live original"
    );
    outside_rename(&asset_dir(&fx.ws, CRANE), &file(&fx.ws, "elsewhere_crane"));
    fx.reconcile().await;
    let r = fx
        .call(
            "ref.resolve",
            json!({"ref": format!("lil://{}/{P}/{CRANE}@1.0.0", fx.ws_id())}),
        )
        .await;
    if let Outcome::Ok(out) = &r {
        assert_ne!(
            out.structured["asset_id"], COPY,
            "the crane's refs reach the copy: {r:?}"
        );
    }
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154 (T8 I1, probe P10)
async fn make_independent_refuses_the_original_of_a_clear_copy() {
    let fx = Fx::with(crane_copied);
    let crane_bytes = std::fs::read(manifest_in(&asset_dir(&fx.ws, CRANE))).unwrap();
    let e = error(fx.independent(CRANE).await);
    assert_eq!(e.code, "repair.not_needed", "{e:?}");
    assert!(
        e.message.contains(&format!("{P}/{COPY}")),
        "names the copy to change instead: {}",
        e.message
    );
    assert_eq!(
        std::fs::read(manifest_in(&asset_dir(&fx.ws, CRANE))).unwrap(),
        crane_bytes
    );
    assert_eq!(fx.get(COPY).await["repair"]["kind"], "copy");
    // The copy is the one to change.
    structured(fx.independent(COPY).await);
    assert!(fx.get(CRANE).await["repair"].is_null());
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154, CORE-038 (T8 I2, probe P13)
async fn an_accepted_move_leaves_no_ghost_across_reconciles_and_a_restart() {
    let fx = Fx::with(|_| {});
    fx.open().await.index.ready().await;
    outside_rename(&asset_dir(&fx.ws, CRANE), &asset_dir(&fx.ws, ORIGAMI));
    fx.reconcile().await;
    structured(fx.accept(ORIGAMI).await);
    for _ in 0..3 {
        fx.reconcile().await;
    }
    let fx = fx.restart().await;
    fx.reconcile().await;
    assert!(fx.get(ORIGAMI).await["repair"].is_null());
    assert_eq!(entries(&fx.redirects().unwrap()), 1);
    assert_eq!(fx.ids().await, [ORIGAMI, LAMP]);
    let status = fx.status().await;
    assert_eq!(status["missing"], json!([]), "{status}");
    assert_eq!(status["needs_repair"], json!([]), "{status}");
    let e = fx
        .err("asset.get", json!({"project": P, "asset_id": CRANE}))
        .await;
    assert_eq!(e.code, "asset.unknown", "no \"Put it back\": {e:?}");
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-038, CORE-154 (T8 I2)
async fn a_folder_gone_with_its_uuid_nowhere_stays_missing() {
    // No live folder holds the crane's creation UUID: it is lost, not moved, and stays listed
    // with its files `missing` and the advice to put it back.
    let fx = Fx::with(|_| {});
    fx.open().await.index.ready().await;
    outside_rename(&asset_dir(&fx.ws, CRANE), &file(&fx.ws, "elsewhere_crane"));
    fx.reconcile().await;
    let fx = fx.restart().await;
    fx.reconcile().await;
    assert_eq!(fx.ids().await, [CRANE, LAMP]);
    let status = fx.status().await;
    let manifest = format!("projects/{P}/assets/{CRANE}/{CRANE}.manifest.json");
    assert!(
        status["missing"]
            .as_array()
            .unwrap()
            .contains(&json!(manifest)),
        "{status}"
    );
    let e = fx
        .err("asset.get", json!({"project": P, "asset_id": CRANE}))
        .await;
    assert_eq!(e.code, "manifest.needs_repair", "{e:?}");
    assert!(e.message.contains("Put it back"), "{}", e.message);
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154 (T8 M1, probe P5)
async fn a_move_into_a_project_folder_spelled_by_case_is_listed_there() {
    let fx = Fx::with(|ws| {
        write_file(
            &file(ws, "projects/Harbor/project.yaml"),
            b"name: Harbor\nslug: harbor\ntype: media\nstatus: active\nlinks: []\ncreated: 2026-10-01\n",
        );
        outside_mkdir(&file(ws, "projects/Harbor/assets"));
        outside_rename(&asset_dir(ws, CRANE), &asset_dir_in(ws, "Harbor", CRANE));
    });
    let entry = json!({"from": format!("{P}/{CRANE}"), "to": format!("Harbor/{CRANE}")});
    assert_eq!(fx.status().await["moved"], json!([entry.clone()]));
    assert_eq!(fx.status_of("harbor").await["moved"], json!([entry]));
    let e = fx
        .err(
            "asset.accept_move",
            json!({"project": "harbor", "asset_id": CRANE}),
        )
        .await;
    assert_eq!(e.code, "params.invalid", "{e:?}");
    assert!(fx.redirects().is_none());
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154 (T8 M3, probe P14)
async fn concurrent_repairs_of_one_uuid_group_never_both_succeed() {
    // Both folders agree: either may be made independent, but only one of them.
    let fx = Fx::with(|ws| {
        crane_copied(ws);
        let dir = asset_dir(ws, COPY);
        outside_rename(
            &dir.join(format!("{CRANE}.manifest.json")),
            &dir.join(format!("{COPY}.manifest.json")),
        );
        hand_edit(&dir, |m| m["asset_id"] = json!(COPY));
    });
    let (a, b) = tokio::join!(fx.independent(CRANE), fx.independent(COPY));
    let ok = [&a, &b]
        .iter()
        .filter(|o| matches!(o, Outcome::Ok(_)))
        .count();
    assert_eq!(ok, 1, "{a:?} / {b:?}");
    let refused = if matches!(a, Outcome::Ok(_)) { b } else { a };
    assert_eq!(error(refused).code, "repair.not_needed");
    let uuids: Vec<Value> = [CRANE, COPY]
        .iter()
        .map(|x| read_json(&manifest_in(&asset_dir(&fx.ws, x)))["creation_uuid"].clone())
        .collect();
    assert!(
        uuids.contains(&json!(CRANE_UUID)),
        "one keeps it: {uuids:?}"
    );
    assert_ne!(uuids[0], uuids[1]);
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154 (T8 M5)
async fn accept_move_right_after_an_outside_rename_looks_at_the_disk() {
    // The folder is renamed while the core runs and the move accepted before any rescan saw it
    // (the watcher here never applies a change): the old folder's row still looks live in the
    // index, which would make the new folder a copy. The repair reconciles the folders it
    // depends on (the place its manifest names) first.
    let fx = Fx::with_idle_watcher(|_| {});
    fx.open().await.index.ready().await;
    outside_rename(&asset_dir(&fx.ws, CRANE), &asset_dir(&fx.ws, ORIGAMI));
    let r = structured(fx.accept(ORIGAMI).await);
    assert_eq!(r["redirect"]["from"], format!("{P}/{CRANE}"), "{r}");
    assert!(fx.get(ORIGAMI).await["repair"].is_null());
    assert_eq!(fx.status().await["moved"], json!([]));
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154, CORE-047
async fn the_index_judges_a_manifest_edited_alone_again() {
    // The index alone, no watcher: a warm reconcile in which only a manifest changed (it is
    // written before the other files) still computes every row's repair again.
    let env = TestEnv::new();
    let ws = env.copy_fixture_workspace("repair-index");
    crane_copied(&ws);
    let index =
        lil_core::index::Index::open(&env.root().join("index.db"), &ws, Arc::new(SystemProbe))
            .expect("index.db");
    index.rebuild().await.expect("a rebuild");
    let kind = |row: Option<lil_core::index::AssetRow>| {
        row.and_then(|r| r.repair)
            .map(|r| r["kind"].as_str().unwrap().to_string())
    };
    assert_eq!(
        kind(index.asset(P, COPY).await.unwrap()).as_deref(),
        Some("copy")
    );
    hand_edit(&asset_dir(&ws, COPY), |m| {
        m["creation_uuid"] = json!("11111111-2222-4333-8444-555555555555");
    });
    index.reconcile(Scope::All).await.expect("a reconcile");
    let crane = index.asset(P, CRANE).await.unwrap().unwrap();
    assert!(crane.repair.is_none() && !crane.needs_repair, "{crane:?}");
    // Alone with its UUID now, but naming the live crane: a copy of it (T8 I1).
    assert_eq!(
        kind(index.asset(P, COPY).await.unwrap()).as_deref(),
        Some("copy")
    );
    index.close();
    drop(index);
    if let Err(left) = env.close() {
        panic!("the test's folder was left behind: {left:?}");
    }
}

#[tokio::test] // spec: CORE-154 (T8 M5)
async fn a_case_only_rename_beside_a_copy_adds_no_folder_to_the_index() {
    // The original renamed by case only (`invalid`), and a copy of it with a UUID of its own
    // (`copy` of the original's live folder). The original's manifest then changes while the
    // core runs, unseen (the watcher here never applies a change). Making the copy independent
    // reconciles the original's folder by the name it has on disk: reconciled by the name the
    // copy's manifest gives, its changed manifest would be indexed as a second asset.
    let fx = Fx::with_idle_watcher(|ws| {
        crane_copied(ws);
        hand_edit(&asset_dir(ws, COPY), |m| {
            m["creation_uuid"] = json!("11111111-2222-4333-8444-555555555555");
        });
        outside_rename(&asset_dir(ws, CRANE), &asset_dir(ws, "Chr_Paper_Crane"));
    });
    assert_eq!(
        fx.get(COPY).await["repair"],
        json!({"kind": "copy", "other": format!("{P}/Chr_Paper_Crane")})
    );
    hand_edit(&asset_dir(&fx.ws, "Chr_Paper_Crane"), |m| {
        m["notes"] = json!("edited while the core runs");
    });
    structured(fx.independent(COPY).await);
    assert_eq!(fx.ids().await, ["Chr_Paper_Crane", COPY, LAMP]);
    let page = fx.ok("assets.query", json!({"project": P})).await;
    assert_eq!(
        page["assets"][0]["repair"],
        json!({"kind": "invalid"}),
        "{page}"
    );
    assert!(fx.get(COPY).await["repair"].is_null());
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154, ARCH-125
async fn a_redirects_file_that_doesnt_parse_stops_accept_move_before_any_write() {
    let fx = Fx::with(|ws| {
        crane_renamed(ws);
        write_file(&file(ws, "registry/redirects.yaml"), b"redirects: [\n");
    });
    let old_file = asset_dir(&fx.ws, ORIGAMI).join(format!("{CRANE}.manifest.json"));
    let bytes = std::fs::read(&old_file).unwrap();
    let e = error(fx.accept(ORIGAMI).await);
    assert_eq!(e.code, "manifest.needs_repair", "{e:?}");
    assert_eq!(fx.redirects().as_deref(), Some("redirects: [\n"));
    assert_eq!(
        std::fs::read(&old_file).unwrap(),
        bytes,
        "the manifest is untouched"
    );
    assert_eq!(fx.get(ORIGAMI).await["repair"]["kind"], "moved");
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154 (T8 M5, R3)
async fn a_repair_looks_at_the_place_a_manifest_names_on_disk() {
    // A copy with a UUID of its own is a copy of the live crane. The crane then leaves the
    // workspace while the core runs, unseen (the watcher here never applies a change): the
    // index still has it live. The tool refuses either way (`repair.not_needed`: a copy, or
    // `invalid`), so this guards what the index shows after the call, not the tool's decision:
    // the repair reconciles the place the copy's manifest names, so afterwards the index shows
    // the crane gone and the copy `invalid` (another asset held that place, T8 round 2), not
    // still a `copy` of a folder that is no longer there. Nothing is written.
    let fx = Fx::with_idle_watcher(|ws| {
        crane_copied(ws);
        hand_edit(&asset_dir(ws, COPY), |m| {
            m["creation_uuid"] = json!("11111111-2222-4333-8444-555555555555");
        });
    });
    assert_eq!(fx.get(COPY).await["repair"]["kind"], "copy");
    outside_rename(&asset_dir(&fx.ws, CRANE), &file(&fx.ws, "elsewhere_crane"));
    assert_eq!(error(fx.accept(COPY).await).code, "repair.not_needed");
    assert_eq!(fx.get(COPY).await["repair"], json!({"kind": "invalid"}));
    assert!(fx.redirects().is_none());
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154, ARCH-125 (T8 round 2 ruling)
async fn a_copy_whose_original_left_outside_is_invalid_not_moved() {
    // A copy with a UUID of its own; its original's folder then leaves the workspace outside
    // the suite. The index keeps the original as `missing` (its UUID lives nowhere), so it
    // knows a different asset at the place the copy names: the copy is no move (`invalid`),
    // and neither repair writes anything. A redirect never sends one asset's refs to another.
    let fx = Fx::with(|ws| {
        crane_copied(ws);
        hand_edit(&asset_dir(ws, COPY), |m| {
            m["creation_uuid"] = json!("11111111-2222-4333-8444-555555555555");
        });
    });
    fx.open().await.index.ready().await;
    assert_eq!(fx.get(COPY).await["repair"]["kind"], "copy");
    outside_rename(&asset_dir(&fx.ws, CRANE), &file(&fx.ws, "elsewhere_crane"));
    fx.reconcile().await;
    let fx = fx.restart().await;
    fx.reconcile().await;
    assert_eq!(
        fx.ids().await,
        [CRANE, COPY, LAMP],
        "the crane stays, missing"
    );
    assert_eq!(fx.get(COPY).await["repair"], json!({"kind": "invalid"}));
    let status = fx.status().await;
    assert_eq!(status["moved"], json!([]), "{status}");
    let copy_file = manifest_in(&asset_dir(&fx.ws, COPY));
    let bytes = std::fs::read(&copy_file).unwrap();
    assert_eq!(error(fx.accept(COPY).await).code, "repair.not_needed");
    assert_eq!(error(fx.independent(COPY).await).code, "repair.not_needed");
    assert!(fx.redirects().is_none(), "no redirect");
    assert_eq!(std::fs::read(&copy_file).unwrap(), bytes, "nothing written");
    let r = fx
        .call(
            "ref.resolve",
            json!({"ref": format!("lil://{}/{P}/{CRANE}@1.0.0", fx.ws_id())}),
        )
        .await;
    if let Outcome::Ok(out) = &r {
        assert_ne!(out.structured["asset_id"], COPY, "{r:?}");
    }
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154 (T8 round 2 ruling: the genuine move is unchanged)
async fn a_genuine_move_seen_while_the_core_runs_is_still_moved() {
    // The old place's row is still indexed (missing) when the move is judged, and holds the
    // same UUID: it is a move.
    let fx = Fx::with_idle_watcher(|_| {});
    fx.open().await.index.ready().await;
    outside_rename(&asset_dir(&fx.ws, CRANE), &asset_dir(&fx.ws, ORIGAMI));
    fx.reconcile().await;
    assert_eq!(
        fx.get(ORIGAMI).await["repair"],
        json!({"kind": "moved", "from": format!("{P}/{CRANE}"), "to": format!("{P}/{ORIGAMI}")})
    );
    structured(fx.accept(ORIGAMI).await);
    assert_eq!(entries(&fx.redirects().unwrap()), 1);
    assert_eq!(fx.ids().await, [ORIGAMI, LAMP]);
    fx.shutdown().await;
}

/// The folder name whose listing [`DenyProbe`] refuses (`PermissionDenied`), if any.
static DENY: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// [`SystemProbe`], except that listing the folder named in [`DENY`] fails with
/// `PermissionDenied`: a folder that is there but can't be read.
#[derive(Debug)]
struct DenyProbe;

impl Probe for DenyProbe {
    fn identity(&self, p: &Path) -> std::io::Result<lil_platform::fs::FileIdentity> {
        SystemProbe.identity(p)
    }
    fn volume_info(&self, p: &Path) -> std::io::Result<lil_platform::fs::VolumeInfo> {
        SystemProbe.volume_info(p)
    }
    fn open_read(&self, p: &Path) -> std::io::Result<std::fs::File> {
        SystemProbe.open_read(p)
    }
    fn long_name(&self, p: &Path) -> std::io::Result<PathBuf> {
        SystemProbe.long_name(p)
    }
    fn reparse_tag(&self, p: &Path) -> std::io::Result<Option<u32>> {
        SystemProbe.reparse_tag(p)
    }
    fn read_dir(&self, dir: &Path) -> std::io::Result<std::fs::ReadDir> {
        let denied = DENY.lock().unwrap().clone();
        if let Some(name) = denied
            && dir.file_name().is_some_and(|n| n.to_string_lossy() == name)
        {
            return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        }
        SystemProbe.read_dir(dir)
    }
}

#[tokio::test] // spec: CORE-154, ARCH-125 (T8 R1)
async fn an_unreadable_original_is_never_dropped_nor_its_copy_a_move() {
    // The crane beside a plain copy (same UUID). The crane's folder then can't be listed: the
    // index marks its files `missing`, but "can't read" is not "gone", so its rows stay, and
    // the copy's move is never accepted (no redirect sends the crane's refs to the copy).
    *DENY.lock().unwrap() = None;
    let fx = Fx::with_probe(crane_copied, Arc::new(DenyProbe));
    fx.open().await.index.ready().await;
    assert_eq!(fx.get(COPY).await["repair"]["kind"], "copy");
    *DENY.lock().unwrap() = Some(CRANE.to_string());
    fx.reconcile().await;
    let ids = fx.ids().await;
    let accepted = fx.accept(COPY).await;
    let wrote = fx.redirects().is_some();
    *DENY.lock().unwrap() = None;
    assert!(ids.contains(&CRANE.to_string()), "{ids:?}");
    assert!(matches!(accepted, Outcome::Error { .. }), "{accepted:?}");
    assert!(!wrote, "no redirect");
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154, ARCH-125 (T8 R1, part 2)
async fn accept_move_refuses_while_the_old_folder_is_on_disk_whatever_the_index_says() {
    // The index believes the crane gone (its folder can't be listed, so its files are
    // `missing`; the row is kept, with the copy's UUID): the copy reads `moved`. The crane's
    // folder is still on disk, so accept_move refuses and writes nothing.
    *DENY.lock().unwrap() = None;
    let fx = Fx::with_probe(crane_copied, Arc::new(DenyProbe));
    fx.open().await.index.ready().await;
    *DENY.lock().unwrap() = Some(CRANE.to_string());
    fx.reconcile().await;
    let kind = fx.get(COPY).await["repair"]["kind"].clone();
    let copy_file = manifest_in(&asset_dir(&fx.ws, COPY));
    let bytes = std::fs::read(&copy_file).unwrap();
    let accepted = fx.accept(COPY).await;
    let wrote = fx.redirects().is_some();
    *DENY.lock().unwrap() = None;
    assert_eq!(kind, "moved", "the index believes the old folder gone");
    let e = error(accepted);
    assert_eq!(e.code, "repair.not_needed", "{e:?}");
    assert!(
        e.message
            .contains(&format!("folder projects/{P}/assets/{CRANE}"))
            && e.message.contains("remove or rename"),
        "{}",
        e.message
    );
    assert!(!wrote, "no redirect");
    assert_eq!(std::fs::read(&copy_file).unwrap(), bytes, "nothing written");
    fx.shutdown().await;
}

/// The paths (`/`-separated, matched as the end of a folder's path) whose listing
/// [`PathDenyProbe`] refuses.
static DENY_PATHS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// [`SystemProbe`], except that listing a folder whose path ends with an entry of
/// [`DENY_PATHS`] fails with `PermissionDenied`.
#[derive(Debug)]
struct PathDenyProbe;

impl Probe for PathDenyProbe {
    fn identity(&self, p: &Path) -> std::io::Result<lil_platform::fs::FileIdentity> {
        SystemProbe.identity(p)
    }
    fn volume_info(&self, p: &Path) -> std::io::Result<lil_platform::fs::VolumeInfo> {
        SystemProbe.volume_info(p)
    }
    fn open_read(&self, p: &Path) -> std::io::Result<std::fs::File> {
        SystemProbe.open_read(p)
    }
    fn long_name(&self, p: &Path) -> std::io::Result<PathBuf> {
        SystemProbe.long_name(p)
    }
    fn reparse_tag(&self, p: &Path) -> std::io::Result<Option<u32>> {
        SystemProbe.reparse_tag(p)
    }
    fn read_dir(&self, dir: &Path) -> std::io::Result<std::fs::ReadDir> {
        let path = dir.to_string_lossy().replace('\\', "/");
        let path = path.trim_end_matches('/');
        let denied = DENY_PATHS.lock().unwrap().clone();
        if denied.iter().any(|d| path.ends_with(d.as_str())) {
            return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        }
        SystemProbe.read_dir(dir)
    }
}

#[tokio::test] // spec: CORE-154, ARCH-125 (T8 R2-1, M-a, M-b)
async fn accept_move_refuses_when_the_old_assets_folder_cant_be_listed() {
    // The crane copied into another project. The old project's `assets` folder and the
    // crane's own can't be listed: the index keeps the crane as a gone row with the copy's
    // UUID, so the harbor copy reads `moved`, and no listing can show the crane is there.
    // The guard asks the old folder itself (its attributes) and fails closed.
    *DENY_PATHS.lock().unwrap() = Vec::new();
    let fx = Fx::with_probe(
        |ws| {
            write_file(
                &file(ws, "projects/harbor/project.yaml"),
                b"name: Harbor\nslug: harbor\ntype: media\nstatus: active\nlinks: []\ncreated: 2026-10-01\n",
            );
            outside_mkdir(&file(ws, "projects/harbor/assets"));
            outside_copy(&asset_dir(ws, CRANE), &asset_dir_in(ws, "harbor", CRANE));
        },
        Arc::new(PathDenyProbe),
    );
    fx.open().await.index.ready().await;
    *DENY_PATHS.lock().unwrap() = vec![
        "projects/sketchbook/assets".into(),
        "sketchbook/assets/chr_paper_crane".into(),
    ];
    fx.reconcile().await;
    let accepted = fx
        .call(
            "asset.accept_move",
            json!({"project": "harbor", "asset_id": CRANE}),
        )
        .await;
    let wrote = fx.redirects();
    *DENY_PATHS.lock().unwrap() = Vec::new();
    let e = error(accepted);
    assert_eq!(e.code, "repair.not_needed", "{e:?}");
    assert!(wrote.is_none(), "{wrote:?}");
    assert!(
        e.message
            .contains(&format!("folder projects/{P}/assets/{CRANE}"))
            && e.message.contains("remove or rename"),
        "names the folder and what to do: {}",
        e.message
    );
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154 (T8 M-b)
async fn accept_move_says_what_to_do_about_a_file_at_the_old_place() {
    // A genuine move, but a file now sits at the old folder's name: refused, naming the file
    // and what to do; once it is removed, the same call goes ahead.
    let fx = Fx::with(crane_renamed);
    let stray = asset_dir(&fx.ws, CRANE);
    write_file(&stray, b"not a folder");
    let e = error(fx.accept(ORIGAMI).await);
    assert_eq!(e.code, "repair.not_needed", "{e:?}");
    assert!(
        e.message
            .contains(&format!("file projects/{P}/assets/{CRANE}"))
            && e.message.contains("remove or rename")
            && e.message.contains("asset.accept_move again"),
        "{}",
        e.message
    );
    assert!(fx.redirects().is_none());
    outside_remove_file(&stray);
    structured(fx.accept(ORIGAMI).await);
    assert_eq!(entries(&fx.redirects().unwrap()), 1);
    fx.shutdown().await;
}

/// Removes a file as an outside program would.
#[allow(clippy::disallowed_methods)]
fn outside_remove_file(path: &Path) {
    std::fs::remove_file(path).unwrap_or_else(|e| panic!("cannot remove {}: {e}", path.display()));
}

const XENON: &str = "chr_xenon";
/// A second creation UUID, for an asset other than the crane.
const OTHER_UUID: &str = "22222222-3333-4444-8555-666666666666";

/// Asserts that neither folder is `moved`, both are `invalid`, that accept_move refuses each
/// naming the other claimant, and that no redirect is written.
async fn two_claimants_are_refused(fx: &Fx, a: &str, b: &str) {
    let status = fx.status().await;
    assert_eq!(status["moved"], json!([]), "{status}");
    for (me, other) in [(a, b), (b, a)] {
        assert_eq!(fx.get(me).await["repair"]["kind"], "invalid", "{me}");
        let e = error(fx.accept(me).await);
        assert_eq!(e.code, "repair.not_needed", "{me}: {e:?}");
        assert!(
            e.message.contains(&format!(
                "{P}/{other} also names {P}/{CRANE} as the folder it moved from"
            )),
            "{me}: names the other claimant: {}",
            e.message
        );
    }
    assert!(fx.redirects().is_none(), "no redirect from the crane");
}

#[tokio::test] // spec: CORE-154, ARCH-125 (T8 A-H1, warm)
async fn two_folders_claiming_one_old_place_are_neither_moved_warm() {
    // The crane (its UUID) renamed to chr_origami; a new asset (another UUID) made at the
    // crane's name, then renamed too, to chr_xenon. Both name the crane's place: neither is a
    // move, from one refresh to the next and after a restart.
    let fx = Fx::with(|_| {});
    fx.open().await.index.ready().await;
    outside_rename(&asset_dir(&fx.ws, CRANE), &asset_dir(&fx.ws, ORIGAMI));
    fx.reconcile().await;
    assert_eq!(fx.get(ORIGAMI).await["repair"]["kind"], "moved");
    outside_copy(&asset_dir(&fx.ws, ORIGAMI), &asset_dir(&fx.ws, CRANE));
    hand_edit(&asset_dir(&fx.ws, CRANE), |m| {
        m["creation_uuid"] = json!(OTHER_UUID);
    });
    fx.reconcile().await;
    assert_eq!(fx.get(ORIGAMI).await["repair"]["kind"], "copy");
    outside_rename(&asset_dir(&fx.ws, CRANE), &asset_dir(&fx.ws, XENON));
    fx.reconcile().await;
    two_claimants_are_refused(&fx, ORIGAMI, XENON).await;
    fx.reconcile().await;
    two_claimants_are_refused(&fx, ORIGAMI, XENON).await;
    let fx = fx.restart().await;
    fx.reconcile().await;
    two_claimants_are_refused(&fx, ORIGAMI, XENON).await;
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154, ARCH-125 (T8 A-H1, cold)
async fn two_folders_claiming_one_old_place_are_neither_moved_cold() {
    // Both renames happened while no core ran: a cold rebuild has no gone row at all.
    let fx = Fx::with(|ws| {
        crane_renamed(ws);
        outside_copy(&asset_dir(ws, ORIGAMI), &asset_dir(ws, XENON));
        hand_edit(&asset_dir(ws, XENON), |m| {
            m["creation_uuid"] = json!(OTHER_UUID);
        });
    });
    two_claimants_are_refused(&fx, ORIGAMI, XENON).await;
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154, ARCH-125 (T8 A-H1, part 3)
async fn accept_move_never_redirects_an_old_place_to_another_asset() {
    // The crane is renamed to chr_xenon and the move accepted (crane -> xenon). Then a folder
    // of another asset that names the crane's place appears (restored from a backup, say): the
    // index reads it `moved` (no folder, no row at the crane's place any more), but old refs to
    // the crane already lead to xenon, a live asset with another creation UUID: refused.
    let fx = Fx::with(|ws| {
        outside_copy(&asset_dir(ws, CRANE), &file(ws, "backup_crane"));
        hand_edit(&file(ws, "backup_crane"), |m| {
            m["creation_uuid"] = json!(OTHER_UUID);
        });
    });
    fx.open().await.index.ready().await;
    outside_rename(&asset_dir(&fx.ws, CRANE), &asset_dir(&fx.ws, XENON));
    fx.reconcile().await;
    structured(fx.accept(XENON).await);
    assert_eq!(entries(&fx.redirects().unwrap()), 1);
    outside_rename(&file(&fx.ws, "backup_crane"), &asset_dir(&fx.ws, ORIGAMI));
    fx.reconcile().await;
    let e = error(fx.accept(ORIGAMI).await);
    assert_eq!(e.code, "repair.not_needed", "{e:?}");
    assert!(
        e.message.contains(&format!("{P}/{XENON}"))
            && e.message.contains("rename this folder back"),
        "names the asset the old refs reach and the way out: {}",
        e.message
    );
    assert_eq!(
        entries(&fx.redirects().unwrap()),
        1,
        "no second redirect from the crane"
    );
    let manifest = read_json(&manifest_in(&asset_dir(&fx.ws, ORIGAMI)));
    assert_eq!(manifest["asset_id"], CRANE, "nothing written");
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154, ARCH-125 (T8 A-L2)
async fn accept_move_warns_when_the_folder_takes_a_redirected_name() {
    // crane -> origami accepted; the folder renamed back to the crane's name: accepting that
    // makes old refs to the crane reach it again, and the call says so (ARCH-125).
    let fx = Fx::with(crane_renamed);
    let first = structured(fx.accept(ORIGAMI).await);
    assert_eq!(first["warnings"], json!([]), "{first}");
    outside_rename(&asset_dir(&fx.ws, ORIGAMI), &asset_dir(&fx.ws, CRANE));
    fx.reconcile().await;
    let r = structured(fx.accept(CRANE).await);
    assert_eq!(
        r["warnings"],
        json!([format!(
            "Old refs to {P}/{CRANE} will now reach this asset (registry/redirects.yaml)."
        )]),
        "{r}"
    );
    fx.shutdown().await;
}

/// Whether [`FlipProbe`] rewrites the creation UUID of [`COPY`]'s manifest on each listing.
static FLIP: AtomicBool = AtomicBool::new(false);
static FLIPS: AtomicU64 = AtomicU64::new(0);

/// [`SystemProbe`], except that while [`FLIP`] is set, every listing of the copy's folder first
/// gives its manifest a new creation UUID, as an outside editor would between two looks.
#[derive(Debug)]
struct FlipProbe;

impl Probe for FlipProbe {
    fn identity(&self, p: &Path) -> std::io::Result<lil_platform::fs::FileIdentity> {
        SystemProbe.identity(p)
    }
    fn volume_info(&self, p: &Path) -> std::io::Result<lil_platform::fs::VolumeInfo> {
        SystemProbe.volume_info(p)
    }
    fn open_read(&self, p: &Path) -> std::io::Result<std::fs::File> {
        SystemProbe.open_read(p)
    }
    fn long_name(&self, p: &Path) -> std::io::Result<PathBuf> {
        SystemProbe.long_name(p)
    }
    fn reparse_tag(&self, p: &Path) -> std::io::Result<Option<u32>> {
        SystemProbe.reparse_tag(p)
    }
    fn read_dir(&self, dir: &Path) -> std::io::Result<std::fs::ReadDir> {
        if FLIP.load(Ordering::SeqCst)
            && dir.file_name().is_some_and(|n| n.to_string_lossy() == COPY)
        {
            let n = FLIPS.fetch_add(1, Ordering::SeqCst);
            hand_edit(dir, |m| {
                m["creation_uuid"] = json!(format!("33333333-4444-4555-8666-{n:012}"));
            });
        }
        SystemProbe.read_dir(dir)
    }
}

#[tokio::test] // spec: CORE-154 (T8 A-L3)
async fn a_repair_never_decides_under_the_lock_of_a_uuid_the_folder_no_longer_has() {
    // The copy's creation UUID changes between every two looks (an outside editor that never
    // stops): the lock taken for the first look's UUID never covers the second look, so the
    // call retries under the new UUID's lock at most twice, then refuses; nothing is written.
    FLIP.store(false, Ordering::SeqCst);
    let fx = Fx::with_probe_idle(crane_copied, Arc::new(FlipProbe));
    fx.open().await.index.ready().await;
    FLIP.store(true, Ordering::SeqCst);
    let made = fx.independent(COPY).await;
    FLIP.store(false, Ordering::SeqCst);
    let e = error(made);
    assert_eq!(e.code, "store.hash_conflict", "{e:?}");
    let uuid = read_json(&manifest_in(&asset_dir(&fx.ws, COPY)))["creation_uuid"].clone();
    assert!(
        uuid.as_str().is_some_and(|u| u.starts_with("33333333-")),
        "only the outside editor wrote it: {uuid}"
    );
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154, ARCH-125 (T8 A-H1, part 2)
async fn the_evidence_of_another_asset_at_an_old_place_is_kept() {
    // As in the warm case, then chr_xenon (the second asset that lived at the crane's name)
    // leaves the workspace. chr_origami is the only folder naming the crane's place now, but the
    // index still knows another asset held it (the crane's gone row, with xenon's UUID): kept
    // while origami (another UUID) names it, so origami is never `moved` from there.
    let fx = Fx::with(|_| {});
    fx.open().await.index.ready().await;
    outside_rename(&asset_dir(&fx.ws, CRANE), &asset_dir(&fx.ws, ORIGAMI));
    fx.reconcile().await;
    outside_copy(&asset_dir(&fx.ws, ORIGAMI), &asset_dir(&fx.ws, CRANE));
    hand_edit(&asset_dir(&fx.ws, CRANE), |m| {
        m["creation_uuid"] = json!(OTHER_UUID);
    });
    fx.reconcile().await;
    outside_rename(&asset_dir(&fx.ws, CRANE), &asset_dir(&fx.ws, XENON));
    fx.reconcile().await;
    outside_rename(&asset_dir(&fx.ws, XENON), &file(&fx.ws, "elsewhere_xenon"));
    fx.reconcile().await;
    let fx = fx.restart().await;
    fx.reconcile().await;
    assert_eq!(fx.get(ORIGAMI).await["repair"]["kind"], "invalid");
    assert_eq!(error(fx.accept(ORIGAMI).await).code, "repair.not_needed");
    assert!(fx.redirects().is_none(), "no redirect from the crane");
    fx.shutdown().await;
}

/// Asserts that `asset` isn't `moved` but `invalid`, that accept_move refuses it naming each of
/// `others`, and that no redirect is written.
async fn claimant_is_refused(fx: &Fx, asset: &str, others: &[&str]) {
    let status = fx.status().await;
    assert_eq!(status["moved"], json!([]), "{status}");
    assert_eq!(fx.get(asset).await["repair"]["kind"], "invalid", "{asset}");
    let e = error(fx.accept(asset).await);
    assert_eq!(e.code, "repair.not_needed", "{asset}: {e:?}");
    for other in others {
        assert!(
            e.message.contains(&format!("{P}/{other}")),
            "{asset}: names {other}: {}",
            e.message
        );
    }
    assert!(fx.redirects().is_none(), "no redirect");
}

const LAMP_OLD: &str = "prop_lamp_old";
const LAMP_NEW: &str = "prop_lamp_new";

#[tokio::test] // spec: CORE-154, ARCH-125 (T8 A-RM1, warm)
async fn a_new_asset_at_a_moved_and_copied_name_is_no_move_warm() {
    // The crane (its UUID) is copied and renamed outside: a duplicate pair, both naming the
    // crane. A new asset (another UUID) made at the crane's name is then renamed away too: it
    // isn't alone in naming the crane's place, so it is no move.
    let fx = Fx::with(|_| {});
    fx.open().await.index.ready().await;
    outside_copy(&asset_dir(&fx.ws, CRANE), &asset_dir(&fx.ws, COPY));
    outside_rename(&asset_dir(&fx.ws, CRANE), &asset_dir(&fx.ws, ORIGAMI));
    fx.reconcile().await;
    outside_copy(&asset_dir(&fx.ws, ORIGAMI), &asset_dir(&fx.ws, CRANE));
    hand_edit(&asset_dir(&fx.ws, CRANE), |m| {
        m["creation_uuid"] = json!(OTHER_UUID);
    });
    fx.reconcile().await;
    outside_rename(&asset_dir(&fx.ws, CRANE), &asset_dir(&fx.ws, XENON));
    fx.reconcile().await;
    claimant_is_refused(&fx, XENON, &[ORIGAMI, COPY]).await;
    let fx = fx.restart().await;
    fx.reconcile().await;
    claimant_is_refused(&fx, XENON, &[ORIGAMI, COPY]).await;
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154, ARCH-125 (T8 A-RM1, cold)
async fn a_new_asset_at_a_moved_and_copied_name_is_no_move_cold() {
    let fx = Fx::with(|ws| {
        outside_copy(&asset_dir(ws, CRANE), &asset_dir(ws, COPY));
        crane_renamed(ws);
        outside_copy(&asset_dir(ws, ORIGAMI), &asset_dir(ws, XENON));
        hand_edit(&asset_dir(ws, XENON), |m| {
            m["creation_uuid"] = json!(OTHER_UUID);
        });
    });
    claimant_is_refused(&fx, XENON, &[ORIGAMI, COPY]).await;
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154, CORE-047, ARCH-125 (T8 A-RM1, warm)
async fn a_new_asset_at_a_legacy_assets_old_name_is_no_move_warm() {
    // The lamp (a legacy manifest, no UUID) is renamed outside; a new asset (a UUID) made at
    // the lamp's name is renamed away too. The legacy folder still names that place: another
    // asset, so the new one is no move.
    let fx = Fx::with(|_| {});
    fx.open().await.index.ready().await;
    outside_rename(&asset_dir(&fx.ws, LAMP), &asset_dir(&fx.ws, LAMP_OLD));
    fx.reconcile().await;
    outside_copy(&asset_dir(&fx.ws, LAMP_OLD), &asset_dir(&fx.ws, LAMP));
    hand_edit(&asset_dir(&fx.ws, LAMP), |m| {
        m["creation_uuid"] = json!(OTHER_UUID);
    });
    fx.reconcile().await;
    outside_rename(&asset_dir(&fx.ws, LAMP), &asset_dir(&fx.ws, LAMP_NEW));
    fx.reconcile().await;
    claimant_is_refused(&fx, LAMP_NEW, &[LAMP_OLD]).await;
    let fx = fx.restart().await;
    fx.reconcile().await;
    claimant_is_refused(&fx, LAMP_NEW, &[LAMP_OLD]).await;
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154, CORE-047, ARCH-125 (T8 A-RM1, cold)
async fn a_new_asset_at_a_legacy_assets_old_name_is_no_move_cold() {
    let fx = Fx::with(|ws| {
        outside_rename(&asset_dir(ws, LAMP), &asset_dir(ws, LAMP_OLD));
        outside_copy(&asset_dir(ws, LAMP_OLD), &asset_dir(ws, LAMP_NEW));
        hand_edit(&asset_dir(ws, LAMP_NEW), |m| {
            m["creation_uuid"] = json!(OTHER_UUID);
        });
    });
    claimant_is_refused(&fx, LAMP_NEW, &[LAMP_OLD]).await;
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154, ARCH-125, CORE-157 (T8 A-RM2)
async fn accept_move_never_redirects_a_trashed_assets_old_name() {
    // crane -> xenon accepted, then xenon trashed: old refs to the crane answer `asset.trashed`.
    // A folder of another asset naming the crane then appears: accepting it would send those
    // refs to it, so it is refused, naming the trash entry.
    let fx = Fx::with(|ws| {
        hand_edit(&asset_dir(ws, CRANE), |m| m["locks"] = json!([]));
        outside_copy(&asset_dir(ws, CRANE), &file(ws, "backup_crane"));
        hand_edit(&file(ws, "backup_crane"), |m| {
            m["creation_uuid"] = json!(OTHER_UUID);
        });
    });
    fx.open().await.index.ready().await;
    outside_rename(&asset_dir(&fx.ws, CRANE), &asset_dir(&fx.ws, XENON));
    fx.reconcile().await;
    structured(fx.accept(XENON).await);
    let trashed = fx
        .ok(
            "test.trash",
            json!({ "path": format!("projects/{P}/assets/{XENON}") }),
        )
        .await;
    let entry = trashed["entry_id"].as_str().unwrap().to_string();
    outside_rename(&file(&fx.ws, "backup_crane"), &asset_dir(&fx.ws, ORIGAMI));
    fx.reconcile().await;
    let e = error(fx.accept(ORIGAMI).await);
    assert_eq!(e.code, "repair.not_needed", "{e:?}");
    assert!(
        e.message.contains(&entry) && e.message.contains("trash.restore"),
        "names the trash entry: {}",
        e.message
    );
    assert_eq!(entries(&fx.redirects().unwrap()), 1, "no second redirect");
    let r = fx
        .call(
            "ref.resolve",
            json!({"ref": format!("lil://{}/{P}/{CRANE}@1.0.0", fx.ws_id())}),
        )
        .await;
    assert_eq!(error(r).code, "asset.trashed");
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154 (T8 A-RL1)
async fn the_claimants_refusal_reads_right_and_suggests_only_valid_names() {
    // Three claimants of the crane's place, one naming it by case only (`Chr_Paper_Crane`):
    // each refusal names the other two in a sentence that reads right for several, and never
    // suggests a name that isn't a valid asset id.
    let fx = Fx::with(|ws| {
        crane_renamed(ws);
        outside_copy(&asset_dir(ws, ORIGAMI), &asset_dir(ws, XENON));
        hand_edit(&asset_dir(ws, XENON), |m| {
            m["creation_uuid"] = json!(OTHER_UUID);
        });
        outside_copy(&asset_dir(ws, ORIGAMI), &asset_dir(ws, "chr_third"));
        hand_edit(&asset_dir(ws, "chr_third"), |m| {
            m["creation_uuid"] = json!("44444444-5555-4666-8777-888888888888");
            m["asset_id"] = json!("Chr_Paper_Crane");
        });
    });
    let e = error(fx.accept("chr_third").await);
    assert_eq!(e.code, "repair.not_needed", "{e:?}");
    let m = &e.message;
    assert!(
        m.contains(&format!("{P}/{ORIGAMI} and {P}/{XENON} also name")),
        "plural: {m}"
    );
    assert!(
        !m.contains("Chr_Paper_Crane"),
        "no invalid name suggested: {m}"
    );
    assert!(m.contains(&format!("back to {P}/{CRANE}")), "{m}");
    let e = error(fx.accept(XENON).await);
    assert!(
        e.message
            .contains(&format!("{P}/{ORIGAMI} and {P}/chr_third also name")),
        "{}",
        e.message
    );
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154, ARCH-125, CORE-157 (T8 A-R2M1)
async fn accept_move_never_takes_over_a_name_trashed_with_no_redirect() {
    // The crane, never moved, is trashed through the suite: old refs to it answer
    // `asset.trashed`. A folder of another asset naming the crane then appears: no folder, row
    // or redirect is at the crane, but accepting would send the trashed asset's refs to it, so it
    // is refused, naming the trash entry (one rule with or without a redirect).
    let fx = Fx::with(|ws| {
        hand_edit(&asset_dir(ws, CRANE), |m| m["locks"] = json!([]));
        outside_copy(&asset_dir(ws, CRANE), &file(ws, "backup_crane"));
        hand_edit(&file(ws, "backup_crane"), |m| {
            m["creation_uuid"] = json!(OTHER_UUID);
        });
    });
    fx.open().await.index.ready().await;
    let trashed = fx
        .ok(
            "test.trash",
            json!({ "path": format!("projects/{P}/assets/{CRANE}") }),
        )
        .await;
    let entry = trashed["entry_id"].as_str().unwrap().to_string();
    outside_rename(&file(&fx.ws, "backup_crane"), &asset_dir(&fx.ws, ORIGAMI));
    fx.reconcile().await;
    let e = error(fx.accept(ORIGAMI).await);
    assert_eq!(e.code, "repair.not_needed", "{e:?}");
    assert!(
        e.message.contains(&entry) && e.message.contains("trash.restore"),
        "names the trash entry: {}",
        e.message
    );
    assert!(fx.redirects().is_none(), "no redirect");
    let r = fx
        .call(
            "ref.resolve",
            json!({"ref": format!("lil://{}/{P}/{CRANE}@1.0.0", fx.ws_id())}),
        )
        .await;
    assert_eq!(error(r).code, "asset.trashed");
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-154, CORE-047, ARCH-125 (T8 A-R2L1)
async fn a_claimant_whose_manifest_breaks_still_counts() {
    // chr_origami and chr_xenon (another asset) both name the crane: neither is a move. Then
    // xenon's manifest is broken by hand: the index keeps its last place and UUID, so it still
    // claims the crane's place, and origami is still no move.
    let fx = Fx::with(|ws| {
        crane_renamed(ws);
        outside_copy(&asset_dir(ws, ORIGAMI), &asset_dir(ws, XENON));
        hand_edit(&asset_dir(ws, XENON), |m| {
            m["creation_uuid"] = json!(OTHER_UUID);
        });
    });
    fx.open().await.index.ready().await;
    assert_eq!(fx.get(ORIGAMI).await["repair"]["kind"], "invalid");
    write_file(
        &manifest_in(&asset_dir(&fx.ws, XENON)),
        b"{ this is not json",
    );
    fx.reconcile().await;
    for _ in 0..2 {
        fx.reconcile().await;
        assert_eq!(fx.get(ORIGAMI).await["repair"]["kind"], "invalid");
        let e = error(fx.accept(ORIGAMI).await);
        assert_eq!(e.code, "repair.not_needed", "{e:?}");
        assert!(
            e.message.contains(&format!("{P}/{XENON}")),
            "names the broken claimant: {}",
            e.message
        );
        assert!(fx.redirects().is_none(), "no redirect");
    }
    fx.shutdown().await;
}
