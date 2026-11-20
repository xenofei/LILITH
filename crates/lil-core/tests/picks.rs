//! The pick log (CORE-176; Appendix H "Pick log"; P4b-R13): a Review decision from the app (a
//! `ui:*` call of `asset.mark` or `asset.signoff` that carries `review_view`) appends one line to
//! `picks.jsonl` in the workspace's local folder (ARCH-097) and keeps the 96 px thumbnail of a
//! renderable file under `picks\thumbs\<sha256>.png`, and its run's recipe once under
//! `picks\recipes\<hex>.json`; a mark that doesn't change the take's flag, or a decision from
//! anywhere else, appends nothing, and a failed append is a Diagnostics error that never undoes
//! the decision. Driven through the dispatcher of an in-process core with the fake clock and ids
//! (QA-031), on a copy of the spec fixture workspace, and through the pick log's append
//! (`store::append_local_line`) for the concurrent appends. Images are made in the test with `lil_testkit::png`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lil_core::events::EventBus;
use lil_core::interchange::thumbs::{ThumbOptions, ThumbService};
use lil_core::session::Session;
use lil_core::state::StateRegistry;
use lil_core::store::{FailPoints, append_local_line, fsops};
use lil_core::tools::pick_log::{Decision, PickLine};
use lil_core::tools::{
    AssetServices, Dispatcher, ErrorSink, PipelineHooks, TestGates, Thumbnails, core_registry,
};
use lil_core::workspace::{WorkspaceHub, WorkspaceRef};
use lil_platform::fs::{Probe, SystemProbe};
use lil_platform::{Clock, IdSource};
use lil_protocol::{CallerKind, EffectClass, ErrorBody, Outcome, ToolCallParams, ToolOutput};
use lil_testkit::files::write_file;
use lil_testkit::{FakeClock, SeqIds, TestEnv, png, sha256_bytes};
use serde_json::{Value, json};

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);
const KITE: &str = "prop_kite";
const RUN: &str = "r_20261101_0900";

fn key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("k-picks-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

fn file(ws: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(ws.to_path_buf(), |p, c| p.join(c))
}

fn ws_ref(ws: &Path) -> WorkspaceRef {
    let canonical = std::fs::canonicalize(ws).expect("canonicalize");
    let text = canonical.to_string_lossy().into_owned();
    WorkspaceRef {
        id: lil_platform::Env::workspace_hash(&canonical),
        path: PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text)),
    }
}

/// The app the decisions come from.
fn app() -> CallerKind {
    CallerKind::Ui("image".into())
}

/// A picture whose bytes depend on `seed`.
fn picture(w: u32, h: u32, seed: u8) -> Vec<u8> {
    let pixels: Vec<u8> = (0..w * h * 4).map(|i| (i % 251) as u8 ^ seed).collect();
    png::rgba(w, h, &pixels)
}

/// The files of [`write_take_run`]'s run: take 1 is `a.png` (primary) with its derived
/// `a_mask.png`, take 2 is `b.png`, take 3 is `notes.txt` (no renderer).
fn take_files() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("a.png", picture(200, 100, 1)),
        ("a_mask.png", picture(20, 10, 2)),
        ("b.png", picture(120, 160, 3)),
        ("notes.txt", b"take three is a text file".to_vec()),
    ]
}

/// The recipe the run records.
fn recipe() -> Value {
    json!({"tool": "test", "seed": 7})
}

/// The hex of the run's recipe ref: the SHA-256 of `serde_json::to_vec` of [`recipe`].
fn recipe_hex() -> String {
    sha256_bytes(&serde_json::to_vec(&recipe()).unwrap())
}

/// `asset` in `sketchbook` with one open run, [`RUN`], whose files carry takes
/// ([`take_files`]), each on disk with its recorded SHA-256.
fn write_take_run(ws: &Path, asset: &str) {
    let dir = format!("projects/sketchbook/assets/{asset}");
    let layout = [
        ("candidate", None, 1, true),
        ("derived", Some("a.png"), 1, false),
        ("candidate", None, 2, true),
        ("candidate", None, 3, true),
    ];
    let mut files = Vec::new();
    for ((name, bytes), (role, from, take, primary)) in take_files().into_iter().zip(layout) {
        write_file(&file(ws, &format!("{dir}/candidates/{RUN}/{name}")), &bytes);
        let mut f = json!({"path": format!("candidates/{RUN}/{name}"), "role": role,
                           "take": take, "sha256": sha256_bytes(&bytes)});
        if let Some(from) = from {
            f["from"] = json!(from);
        }
        if primary {
            f["primary"] = json!(true);
        }
        files.push(f);
    }
    // Each asset its own creation UUID, so none reads as a copy of another (CORE-154).
    let uuid = format!(
        "0d6c1a52-3b1e-4f7a-9c2d-{}",
        &sha256_bytes(asset.as_bytes())[..12]
    );
    let m = json!({
        "schema_version": 1, "asset_id": asset, "project": "sketchbook", "type": "image",
        "creation_uuid": uuid,
        "version": "1.0.0", "approved_version": null, "status": "candidate",
        "inputs": [], "pinned_inputs": [],
        "candidate_runs": [{
            "run_id": RUN, "path": format!("candidates/{RUN}/"), "status": "open",
            "created": "2026-11-01T09:00:00Z", "target_version": "1.0.0", "files": files,
            "selected": null, "signed_off": null, "recipe": recipe(),
        }],
        "hashes": {},
    });
    write_file(
        &file(ws, &format!("{dir}/{asset}.manifest.json")),
        format!("{}\n", serde_json::to_string_pretty(&m).unwrap()).as_bytes(),
    );
}

/// The SHA-256 of `name` in [`take_files`].
fn sha_of(name: &str) -> String {
    let (_, bytes) = take_files()
        .into_iter()
        .find(|(n, _)| *n == name)
        .expect("a take file");
    sha256_bytes(&bytes)
}

/// The take hash by hand: `<sha256>  <name>\n` per file, sorted by name in byte order.
fn take_hash(names: &[&str]) -> String {
    let mut names = names.to_vec();
    names.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    let text: String = names
        .iter()
        .map(|n| format!("{}  {n}\n", sha_of(n)))
        .collect();
    sha256_bytes(text.as_bytes())
}

/// Every error the dispatcher's sink heard: `(code, tool, message)`.
#[derive(Default)]
struct Errors(Mutex<Vec<(String, Option<String>, String)>>);

impl ErrorSink for Errors {
    fn record_error(&self, code: &str, tool: Option<&str>, message: &str, _at_ms: i64) {
        self.0.lock().unwrap().push((
            code.to_string(),
            tool.map(str::to_string),
            message.to_string(),
        ));
    }
}

/// An in-process core's dispatcher over a copy of the fixture workspace.
struct Fx {
    dispatcher: Dispatcher,
    hub: Arc<WorkspaceHub>,
    errors: Arc<Errors>,
    ws_ref: WorkspaceRef,
    ws: PathBuf,
    _states: Arc<StateRegistry>,
    /// Last: the folder cannot go while the dispatcher holds state.db open.
    env: TestEnv,
}

impl Fx {
    /// The fixture workspace with `assets` written by [`write_take_run`], registered as the
    /// default; the thumbnail service is in its slot when `thumbs`.
    fn new(assets: &[&str], thumbs: bool) -> Fx {
        Fx::with_fp(assets, thumbs, FailPoints::none())
    }

    /// [`Fx::new`], with the store's fail points `fp`.
    fn with_fp(assets: &[&str], thumbs: bool, fp: Arc<FailPoints>) -> Fx {
        let env = TestEnv::new();
        let ws = env.copy_fixture_workspace("assets");
        for asset in assets {
            write_take_run(&ws, asset);
        }
        env.register_workspace(&ws, true);
        let probe: Arc<dyn Probe> = Arc::new(SystemProbe);
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
            lil_core::watch::WatchConfig::default(),
        );
        let services = AssetServices::unwired();
        if thumbs {
            let service: Arc<dyn Thumbnails> =
                ThumbService::new(hub.clone(), events.clone(), ThumbOptions::default());
            let _ = services.thumbs.set(service);
        }
        let reg = core_registry(
            true,
            &Arc::new(TestGates::new()),
            &states,
            &hub,
            &env.env,
            &probe,
            Arc::default(),
            &services,
        );
        let lifecycle =
            lil_core::Lifecycle::new(clock.clone(), lil_core::lifecycle::IDLE_EXIT_AFTER);
        let dispatcher = Dispatcher::new(
            reg,
            PipelineHooks::plan2(),
            states.clone(),
            events,
            hub.clone(),
            ids,
            clock,
            lifecycle,
        );
        let errors = Arc::new(Errors::default());
        dispatcher.set_error_sink(errors.clone());
        Fx {
            dispatcher,
            hub,
            errors,
            ws_ref: ws_ref(&ws),
            ws,
            _states: states,
            env,
        }
    }

    /// Closes the hub and the state actors, then removes the test's folder.
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

    async fn call_as(&self, caller: CallerKind, name: &str, args: Value) -> Outcome<ToolOutput> {
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
            id: "s_picks".into(),
            caller,
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

    async fn ok_as(&self, caller: CallerKind, name: &str, args: Value) -> Value {
        match self.call_as(caller, name, args.clone()).await {
            Outcome::Ok(out) => out.structured,
            other => panic!("{name} {args}: {other:?}"),
        }
    }

    async fn err_as(&self, caller: CallerKind, name: &str, args: Value) -> ErrorBody {
        match self.call_as(caller, name, args.clone()).await {
            Outcome::Error { error } => error,
            other => panic!("{name} {args} should fail: {other:?}"),
        }
    }

    /// The workspace's local data folder (ARCH-097).
    fn local(&self) -> PathBuf {
        self.env.workspace_local_dir(&self.ws)
    }

    fn log(&self) -> PathBuf {
        self.local().join("picks.jsonl")
    }

    fn picks_thumbs(&self) -> PathBuf {
        self.local().join("picks").join("thumbs")
    }

    fn picks_recipes(&self) -> PathBuf {
        self.local().join("picks").join("recipes")
    }

    fn manifest_path(&self, asset: &str) -> PathBuf {
        file(
            &self.ws,
            &format!("projects/sketchbook/assets/{asset}/{asset}.manifest.json"),
        )
    }

    fn manifest(&self, asset: &str) -> Value {
        serde_json::from_slice(&std::fs::read(self.manifest_path(asset)).unwrap()).unwrap()
    }
}

/// `asset.mark` arguments on [`RUN`] of `asset`: `extra` on top.
fn mark(asset: &str, extra: Value) -> Value {
    let mut args = json!({"project": "sketchbook", "asset_id": asset, "run_id": RUN});
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    args
}

/// `asset.signoff` arguments on [`RUN`] of `asset`: `extra` on top.
fn signoff(asset: &str, extra: Value) -> Value {
    let mut args = json!({"project": "sketchbook", "asset_id": asset, "run_id": RUN});
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    args
}

/// The lines of the JSON-lines file `path` (none when it is missing).
fn jsonl(path: &Path) -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l:?}")))
        .collect()
}

/// Waits until `f` gives a value, failing after [`DEADLINE`].
async fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let until = tokio::time::Instant::now() + DEADLINE;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(tokio::time::Instant::now() < until, "waited for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The names in the folder `dir` (none when it is missing).
fn kept_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

/// Opens `path` (created if missing) as another writer of the log would.
#[allow(clippy::disallowed_methods)] // an outside appender holding the log's append lock
fn open_log(path: &Path) -> std::fs::File {
    fsops::ensure_dir(path.parent().unwrap()).unwrap();
    std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(path)
        .unwrap()
}

/// Takes the append lock of `file`, the lock the core's append takes.
#[allow(clippy::disallowed_methods)] // the outside appender's lock
fn hold_append_lock(file: &std::fs::File) -> lil_platform::fs::AppendLock<'_> {
    lil_platform::fs::try_lock_append(file)
        .unwrap()
        .expect("the lock is free")
}

#[tokio::test] // spec: CORE-176
async fn app_review_marks_are_logged_with_take_and_hash() {
    let fx = Fx::new(&[KITE], true);
    fx.ok_as(
        app(),
        "asset.mark",
        mark(
            KITE,
            json!({"take": 1, "flag": "pick", "review_view": "loupe"}),
        ),
    )
    .await;
    // The line is appended before the mark answers.
    let lines = jsonl(&fx.log());
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    assert_eq!(line["decision"], "pick", "{line}");
    assert_eq!(line["view"], "loupe", "{line}");
    assert_eq!(line["project"], "sketchbook", "{line}");
    assert_eq!(line["asset_id"], KITE, "{line}");
    assert_eq!(line["run_id"], RUN, "{line}");
    assert_eq!(line["file"], format!("candidates/{RUN}/a.png"), "{line}");
    assert_eq!(line["sha256"], sha_of("a.png"), "{line}");
    assert_eq!(line["take"], 1, "{line}");
    assert_eq!(line["primary"], true, "{line}");
    // Both files of take 1, found by take, never by name alone.
    assert_eq!(
        line["take_sha256"],
        take_hash(&["a.png", "a_mask.png"]),
        "{line}"
    );
    assert_eq!(line["recipe"], format!("sha256:{}", recipe_hex()), "{line}");
    let utc = line["utc"].as_str().expect("utc");
    assert!(utc.ends_with('Z') && utc.contains('T'), "{line}");
    let keys: BTreeSet<&str> = line
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        BTreeSet::from([
            "utc",
            "project",
            "asset_id",
            "run_id",
            "file",
            "sha256",
            "take",
            "primary",
            "take_sha256",
            "decision",
            "view",
            "recipe"
        ]),
        "Appendix H's fields: {line}"
    );

    // A companion addresses its take: the line names the take's primary file.
    fx.ok_as(
        app(),
        "asset.mark",
        mark(
            KITE,
            json!({"file": "a_mask.png", "flag": "reject", "review_view": "grid"}),
        ),
    )
    .await;
    let lines = jsonl(&fx.log());
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[1]["decision"], "reject", "{}", lines[1]);
    assert_eq!(lines[1]["view"], "grid", "{}", lines[1]);
    assert_eq!(lines[1]["file"], format!("candidates/{RUN}/a.png"));
    assert_eq!(lines[1]["take"], 1);
    // Take 2, one file.
    fx.ok_as(
        app(),
        "asset.mark",
        mark(
            KITE,
            json!({"take": 2, "flag": "pick", "review_view": "survey"}),
        ),
    )
    .await;
    let lines = jsonl(&fx.log());
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[2]["file"], format!("candidates/{RUN}/b.png"));
    assert_eq!(lines[2]["sha256"], sha_of("b.png"));
    assert_eq!(lines[2]["take_sha256"], take_hash(&["b.png"]));
    assert_eq!(lines[2]["view"], "survey");
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-176
async fn agent_and_cli_marks_are_not_logged() {
    let fx = Fx::new(&[KITE], true);
    for (caller, flag) in [
        (CallerKind::Cli, "pick"),
        (CallerKind::Mcp, "reject"),
        (CallerKind::Assistant, "pick"),
    ] {
        let out = fx
            .ok_as(
                caller.clone(),
                "asset.mark",
                mark(
                    KITE,
                    json!({"take": 1, "flag": flag, "review_view": "loupe"}),
                ),
            )
            .await;
        assert_eq!(out["take"]["flag"], flag, "{caller}: {out}");
    }
    assert!(!fx.log().exists(), "no line from an agent or the cli");
    assert!(!fx.local().join("picks").exists(), "no thumbnail copy");
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-176
async fn app_marks_without_review_view_are_not_logged() {
    let fx = Fx::new(&[KITE], true);
    // From the app, but not from Review (the Inspector).
    fx.ok_as(
        app(),
        "asset.mark",
        mark(KITE, json!({"take": 1, "flag": "pick"})),
    )
    .await;
    // From Review, but no pick or reject: a take's rating, the asset's own marks.
    fx.ok_as(
        app(),
        "asset.mark",
        mark(KITE, json!({"take": 1, "rating": 4, "review_view": "grid"})),
    )
    .await;
    fx.ok_as(
        app(),
        "asset.mark",
        json!({"project": "sketchbook", "asset_id": KITE, "label": "red",
               "review_view": "grid"}),
    )
    .await;
    assert!(!fx.log().exists(), "{:?}", jsonl(&fx.log()));
    assert!(!fx.local().join("picks").exists(), "no thumbnail copy");
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-176
async fn a_mark_is_logged_only_when_it_changes_the_flag() {
    let fx = Fx::new(&[KITE], false);
    // Each mark of take 2, and the decisions logged once it answered.
    let steps = [
        (json!(null), vec![]),                            // nothing to clear
        (json!("pick"), vec!["pick"]),                    // none to pick
        (json!("pick"), vec!["pick"]),                    // a repeated pick
        (json!("reject"), vec!["pick", "reject"]),        // pick to reject
        (json!(null), vec!["pick", "reject", "cleared"]), // reject to cleared
        (json!(null), vec!["pick", "reject", "cleared"]), // cleared again
    ];
    for (flag, logged) in steps {
        fx.ok_as(
            app(),
            "asset.mark",
            mark(
                KITE,
                json!({"take": 2, "flag": flag, "review_view": "compare"}),
            ),
        )
        .await;
        let decisions: Vec<Value> = jsonl(&fx.log())
            .iter()
            .map(|l| l["decision"].clone())
            .collect();
        assert_eq!(decisions, logged, "after flag {flag}");
    }
    // A rating change alongside an unchanged flag isn't a decision either.
    fx.ok_as(
        app(),
        "asset.mark",
        mark(
            KITE,
            json!({"take": 2, "flag": null, "rating": 3, "review_view": "compare"}),
        ),
    )
    .await;
    assert_eq!(jsonl(&fx.log()).len(), 3);
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-176, CORE-032
async fn the_recipe_is_kept_once_by_its_hash() {
    const BARE: &str = "prop_kite_bare";
    let fx = Fx::new(&[KITE, BARE], false);
    // BARE's run records no recipe.
    let mut m = fx.manifest(BARE);
    lil_schema::remove_key(m["candidate_runs"][0].as_object_mut().unwrap(), "recipe");
    write_file(
        &fx.manifest_path(BARE),
        format!("{}\n", serde_json::to_string_pretty(&m).unwrap()).as_bytes(),
    );
    for take in [1, 2] {
        fx.ok_as(
            app(),
            "asset.mark",
            mark(
                KITE,
                json!({"take": take, "flag": "pick", "review_view": "grid"}),
            ),
        )
        .await;
    }
    let lines = jsonl(&fx.log());
    assert_eq!(lines.len(), 2, "{lines:?}");
    let hex = recipe_hex();
    for line in &lines {
        assert_eq!(line["recipe"], format!("sha256:{hex}"), "{line}");
    }
    // One file, named by the hash of its own bytes, holding the manifest's recipe (written in
    // its own task, so waited for).
    let one = [format!("{hex}.json")];
    poll("the one recipe file", || {
        (kept_names(&fx.picks_recipes()) == one).then_some(())
    })
    .await;
    let bytes = std::fs::read(fx.picks_recipes().join(format!("{hex}.json"))).unwrap();
    assert_eq!(sha256_bytes(&bytes), hex);
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body, fx.manifest(KITE)["candidate_runs"][0]["recipe"]);

    // A run without a recipe: `recipe` is null and nothing more is kept.
    fx.ok_as(
        app(),
        "asset.mark",
        mark(
            BARE,
            json!({"take": 1, "flag": "reject", "review_view": "grid"}),
        ),
    )
    .await;
    let lines = jsonl(&fx.log());
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[2]["asset_id"], BARE);
    assert_eq!(lines[2]["recipe"], Value::Null, "{}", lines[2]);
    assert_eq!(kept_names(&fx.picks_recipes()), one);
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-176
async fn signoff_from_review_logs_each_signed_take() {
    const BY_CLI: &str = "prop_kite_cli";
    const NO_VIEW: &str = "prop_kite_plain";
    const BY_FILES: &str = "prop_kite_files";
    let fx = Fx::new(&[KITE, BY_CLI, NO_VIEW, BY_FILES], true);
    // Not logged: a sign-off from the cli, one from the app without a Review view, and one
    // that is refused.
    fx.ok_as(
        CallerKind::Cli,
        "asset.signoff",
        signoff(BY_CLI, json!({"takes": [1], "review_view": "grid"})),
    )
    .await;
    fx.ok_as(
        app(),
        "asset.signoff",
        signoff(NO_VIEW, json!({"takes": [1]})),
    )
    .await;
    let refused = fx
        .err_as(
            app(),
            "asset.signoff",
            signoff(KITE, json!({"takes": [9], "review_view": "grid"})),
        )
        .await;
    assert_eq!(refused.code, "asset.unknown", "{refused:?}");

    // From Review: one line per signed take, after the Record step.
    fx.ok_as(
        app(),
        "asset.signoff",
        signoff(KITE, json!({"takes": [2, 1], "review_view": "compare"})),
    )
    .await;
    let lines = poll("two sign-off lines", || {
        let lines = jsonl(&fx.log());
        (lines.len() >= 2).then_some(lines)
    })
    .await;
    assert_eq!(lines.len(), 2, "{lines:?}");
    let mut takes = Vec::new();
    for line in &lines {
        assert_eq!(line["decision"], "signed_off", "{line}");
        assert_eq!(line["view"], "compare", "{line}");
        assert_eq!(line["asset_id"], KITE, "{line}");
        assert_eq!(line["run_id"], RUN, "{line}");
        assert_eq!(line["primary"], true, "{line}");
        assert_eq!(line["recipe"], format!("sha256:{}", recipe_hex()), "{line}");
        takes.push(line["take"].as_u64().unwrap());
    }
    takes.sort_unstable();
    assert_eq!(takes, [1, 2]);
    let one = lines.iter().find(|l| l["take"] == 1).unwrap();
    assert_eq!(one["file"], format!("candidates/{RUN}/a.png"));
    assert_eq!(one["sha256"], sha_of("a.png"));
    assert_eq!(one["take_sha256"], take_hash(&["a.png", "a_mask.png"]));
    let two = lines.iter().find(|l| l["take"] == 2).unwrap();
    assert_eq!(two["file"], format!("candidates/{RUN}/b.png"));
    assert_eq!(two["take_sha256"], take_hash(&["b.png"]));
    // The manifest recorded the sign-off.
    assert_eq!(fx.manifest(KITE)["status"], "approved");
    // The 96 px copy of each signed take's file, from the signed version.
    for name in ["a.png", "b.png"] {
        let copy = fx.picks_thumbs().join(format!("{}.png", sha_of(name)));
        let bytes = poll("the sign-off's copy", || std::fs::read(&copy).ok()).await;
        assert_eq!(png::long_side(&bytes), 96, "{name}");
    }

    // A sign-off by files logs the take of each file.
    fx.ok_as(
        app(),
        "asset.signoff",
        signoff(
            BY_FILES,
            json!({"files": ["b.png"], "review_view": "survey"}),
        ),
    )
    .await;
    let lines = poll("the sign-off by files", || {
        let lines = jsonl(&fx.log());
        (lines.len() >= 3).then_some(lines)
    })
    .await;
    assert_eq!(lines.len(), 3, "{lines:?}");
    let line = &lines[2];
    assert_eq!(line["asset_id"], BY_FILES, "{line}");
    assert_eq!(line["decision"], "signed_off", "{line}");
    assert_eq!(line["take"], 2, "{line}");
    assert_eq!(line["file"], format!("candidates/{RUN}/b.png"), "{line}");
    assert_eq!(line["view"], "survey", "{line}");
    assert!(
        lines
            .iter()
            .all(|l| l["asset_id"] == KITE || l["asset_id"] == BY_FILES),
        "{lines:?}"
    );
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-176
async fn a_signoff_whose_record_fails_logs_nothing() {
    // The manifest changes under the sign-off after its seal (the run is rejected meanwhile),
    // so its Record stage refuses; the lines it left for after the Record are dropped.
    let fp = FailPoints::none();
    let armed = Arc::new(AtomicBool::new(false));
    let target: Arc<Mutex<Option<PathBuf>>> = Arc::default();
    {
        let (armed, target) = (armed.clone(), target.clone());
        fp.on(
            "store.after_seal",
            Box::new(move || {
                if armed.swap(false, Ordering::SeqCst) {
                    let path = target.lock().unwrap().clone().expect("the manifest");
                    let mut m: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
                    m["candidate_runs"][0]["status"] = json!("rejected");
                    write_file(
                        &path,
                        format!("{}\n", serde_json::to_string_pretty(&m)?).as_bytes(),
                    );
                }
                Ok(())
            }),
        );
    }
    let fx = Fx::with_fp(&[KITE], true, fp);
    *target.lock().unwrap() = Some(fx.manifest_path(KITE));
    armed.store(true, Ordering::SeqCst);
    let out = fx
        .call_as(
            app(),
            "asset.signoff",
            signoff(KITE, json!({"takes": [1], "review_view": "grid"})),
        )
        .await;
    assert!(
        matches!(out, Outcome::Error { .. }),
        "the Record stage refuses: {out:?}"
    );
    assert!(!armed.load(Ordering::SeqCst), "the fail point ran");
    // A failure deadline for work that must never come, not a measurement (QA-031).
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(!fx.log().exists(), "{:?}", jsonl(&fx.log()));
    assert!(!fx.local().join("picks").exists(), "no copy, no recipe");
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-176, APP-038
async fn the_96_px_copy_is_kept_by_hash_for_renderable_kinds_only() {
    let fx = Fx::new(&[KITE], true);
    for take in [1, 3] {
        fx.ok_as(
            app(),
            "asset.mark",
            mark(
                KITE,
                json!({"take": take, "flag": "pick", "review_view": "grid"}),
            ),
        )
        .await;
    }
    let copy = fx.picks_thumbs().join(format!("{}.png", sha_of("a.png")));
    let bytes = poll("the 96 px copy", || std::fs::read(&copy).ok()).await;
    assert_eq!(png::long_side(&bytes), 96, "the 96 px tier at scale 1");
    // The text file's line is written, its copy never.
    let lines = jsonl(&fx.log());
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[1]["sha256"], sha_of("notes.txt"));
    let kept: Vec<String> = std::fs::read_dir(fx.picks_thumbs())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(kept, [format!("{}.png", sha_of("a.png"))]);
    fx.shutdown().await;

    // With the thumbnail slot empty the line is still written, without the copy.
    let fx = Fx::new(&[KITE], false);
    fx.ok_as(
        app(),
        "asset.mark",
        mark(
            KITE,
            json!({"take": 1, "flag": "pick", "review_view": "grid"}),
        ),
    )
    .await;
    assert_eq!(jsonl(&fx.log()).len(), 1);
    assert!(!fx.picks_thumbs().exists(), "no copy without the service");
    fx.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)] // spec: CORE-176
async fn marks_are_logged_in_the_manifests_order_under_contention() {
    let fx = Fx::new(&[KITE], false);
    // Take 2's flag as the manifest holds it (null while the file is being replaced).
    let flag = || {
        std::fs::read(fx.manifest_path(KITE))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .map_or(Value::Null, |m| {
                m["candidate_runs"][0]["files"][2]["flag"].clone()
            })
    };
    let file = open_log(&fx.log());
    let lock = hold_append_lock(&file);
    let review = |f: &str| mark(KITE, json!({"take": 2, "flag": f, "review_view": "grid"}));
    let pick = fx.ok_as(app(), "asset.mark", review("pick"));
    let reject_after = async {
        // The pick is in the manifest; its line now waits for the log, under the asset's lock.
        poll("the pick in the manifest", || {
            (flag() == "pick").then_some(())
        })
        .await;
        let reject = fx.ok_as(app(), "asset.mark", review("reject"));
        let release = async {
            // Well inside the pick's 1 s budget: the reject can't edit before the pick's line.
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert_eq!(flag(), "pick", "the reject waits for the pick's line");
            drop(lock);
        };
        tokio::join!(reject, release)
    };
    let (picked, (rejected, ())) = tokio::join!(pick, reject_after);
    assert_eq!(picked["take"]["flag"], "pick", "{picked}");
    assert_eq!(rejected["take"]["flag"], "reject", "{rejected}");
    let decisions: Vec<Value> = jsonl(&fx.log())
        .iter()
        .map(|l| l["decision"].clone())
        .collect();
    assert_eq!(decisions, [json!("pick"), json!("reject")]);
    assert_eq!(flag(), "reject", "the log's last word is the manifest's");
    assert!(fx.errors.0.lock().unwrap().is_empty());
    drop(file);
    fx.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)] // spec: CORE-176, ARCH-121
async fn a_held_log_delays_a_mark_by_its_short_budget_only() {
    let fx = Fx::new(&[KITE], false);
    let file = open_log(&fx.log());
    let lock = hold_append_lock(&file);
    let start = tokio::time::Instant::now();
    let out = fx
        .ok_as(
            app(),
            "asset.mark",
            mark(
                KITE,
                json!({"take": 1, "flag": "pick", "review_view": "loupe"}),
            ),
        )
        .await;
    let took = start.elapsed();
    // A bound on the wait, well above the 1 s budget, not a measurement (QA-031).
    assert!(
        took < Duration::from_secs(3),
        "the mark answered after {took:?}"
    );
    assert_eq!(out["take"]["flag"], "pick", "{out}");
    let files = fx.manifest(KITE)["candidate_runs"][0]["files"].clone();
    assert_eq!(files[0]["flag"], "pick", "the mark stands: {files}");
    let errors = fx.errors.0.lock().unwrap().clone();
    assert!(
        errors
            .iter()
            .any(|(code, tool, _)| code == "pick_log.append_failed"
                && tool.as_deref() == Some("asset.mark")),
        "{errors:?}"
    );
    drop(lock);
    assert!(jsonl(&fx.log()).is_empty());
    drop(file);
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-176, ARCH-121
#[allow(clippy::disallowed_methods)] // makes the folder that stands where the log should be
async fn a_failed_append_is_a_diagnostics_error_and_the_mark_stands() {
    let fx = Fx::new(&[KITE], false);
    std::fs::create_dir_all(fx.log()).unwrap();
    let out = fx
        .ok_as(
            app(),
            "asset.mark",
            mark(
                KITE,
                json!({"take": 1, "flag": "pick", "review_view": "loupe"}),
            ),
        )
        .await;
    assert_eq!(out["take"]["flag"], "pick", "{out}");
    let files = fx.manifest(KITE)["candidate_runs"][0]["files"].clone();
    assert_eq!(files[0]["flag"], "pick", "the mark stands: {files}");
    let errors = fx.errors.0.lock().unwrap().clone();
    let failed: Vec<_> = errors
        .iter()
        .filter(|(code, _, _)| code == "pick_log.append_failed")
        .collect();
    assert_eq!(failed.len(), 1, "{errors:?}");
    assert_eq!(failed[0].1.as_deref(), Some("asset.mark"), "{errors:?}");
    assert!(fx.log().is_dir());
    fx.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)] // spec: CORE-176, ARCH-081
async fn lines_are_whole_under_concurrent_appends() {
    const TASKS: usize = 8;
    const EACH: usize = 25;
    let fx = Fx::new(&[], false);
    let log = fx.log();
    // Lines longer than a page, so a torn write would show.
    let padding = "x".repeat(8 * 1024);
    let mut tasks = Vec::new();
    for t in 0..TASKS {
        let (log, padding) = (log.clone(), padding.clone());
        tasks.push(tokio::spawn(async move {
            for i in 0..EACH {
                let line = PickLine {
                    utc: "2026-11-01T09:00:00Z".into(),
                    project: "sketchbook".into(),
                    asset_id: KITE.into(),
                    run_id: RUN.into(),
                    file: format!("candidates/{RUN}/{t}_{i}.png"),
                    sha256: Some(sha256_bytes(format!("{t}/{i}").as_bytes())),
                    take: 1,
                    primary: true,
                    take_sha256: None,
                    decision: Decision::Pick,
                    view: padding.clone(),
                    recipe: None,
                    recipe_body: None,
                };
                let text = format!("{}\n", serde_json::to_string(&line).unwrap());
                // The pick log's own append, with the store's usual 10 s budget: eight writers
                // without pause can starve one past the pick log's 1 s, which `log_pick`
                // reports as `pick_log.append_failed` (`a_held_log_delays_a_mark_by_...`).
                append_local_line(log.clone(), text, Duration::from_secs(10))
                    .await
                    .unwrap();
            }
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let lines = jsonl(&fx.log());
    assert_eq!(lines.len(), TASKS * EACH);
    let seen: BTreeSet<String> = lines
        .iter()
        .map(|l| {
            assert_eq!(l["view"].as_str().map(str::len), Some(8 * 1024));
            l["file"].as_str().unwrap().to_string()
        })
        .collect();
    assert_eq!(seen.len(), TASKS * EACH, "every line once, whole");
    fx.shutdown().await;
}
