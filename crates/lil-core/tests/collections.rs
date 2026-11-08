//! Saved searches and collections (CORE-155; P4b-R27; Appendix H "Organising tools" and
//! "`registry/collections.yaml`"): `collections.list`, `collection.save` and `collection.delete`
//! over `registry/collections.yaml`, `assets.query {collection}`, the quick collection, and the
//! trash and restore of a deleted entry. Driven through an in-process dispatcher with the
//! confirmations' stage over a copy of the fixture workspace (QA-031: the fake clock and ids).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lil_core::Lifecycle;
use lil_core::confirm::{
    AgentConfirm, Confirm, ListState, McpConfirmation, NoNotificationPath, UiPresence,
};
use lil_core::events::EventBus;
use lil_core::session::Session;
use lil_core::state::StateRegistry;
use lil_core::store::{FailPoints, WsPath};
use lil_core::tools::{AssetServices, Dispatcher, PipelineHooks, TestGates, core_registry};
use lil_core::workspace::{OpenWorkspace, WorkspaceHub, WorkspaceRef};
use lil_platform::Clock;
use lil_platform::fs::{Probe, SystemProbe};
use lil_protocol::app::{Answer, ConfirmationAnswerParams};
use lil_protocol::{CallerKind, ErrorBody, Outcome, ToolCallParams, ToolOutput};
use lil_testkit::files::write_file;
use lil_testkit::{FakeClock, SeqIds, TestEnv};
use serde_json::{Value, json};

const QUICK: &str = "00000000-0000-0000-0000-000000000000";
const FILE: &str = "registry/collections.yaml";
const CRANE: &str = "chr_paper_crane";
const LAMP: &str = "prop_signal_lamp";
const CRANE_REF: &str = "sketchbook/chr_paper_crane";
const LAMP_REF: &str = "sketchbook/prop_signal_lamp";

fn no_fatal() -> Arc<dyn Fn(&str) + Send + Sync> {
    Arc::new(|what: &str| panic!("unexpected fatal report: {what}"))
}

fn key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "collections-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn ws_file(ws: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(ws.to_path_buf(), |p, c| p.join(c))
}

/// An in-process core's dispatcher over a copy of the fixture workspace, with the
/// confirmations' stage (an app is connected, so questions can be answered) and the store's
/// fail points at hand for hooks.
struct Fx {
    dispatcher: Dispatcher,
    confirm: Arc<Confirm>,
    hub: Arc<WorkspaceHub>,
    events: Arc<EventBus>,
    failpoints: Arc<FailPoints>,
    ws_ref: WorkspaceRef,
    ws: PathBuf,
    states: Arc<StateRegistry>,
    env: TestEnv,
}

impl Fx {
    async fn new(setup: impl FnOnce(&Path)) -> Fx {
        let env = TestEnv::new();
        let ws = env.copy_fixture_workspace("collections");
        setup(&ws);
        env.register_workspace(&ws, true);
        let canonical = std::fs::canonicalize(&ws).expect("canonicalize");
        let text = canonical.to_string_lossy().into_owned();
        let ws_ref = WorkspaceRef {
            id: lil_platform::Env::workspace_hash(&canonical),
            path: PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text)),
        };
        let states =
            Arc::new(StateRegistry::open(&env.env, no_fatal()).expect("the suite state.db"));
        let clock: Arc<dyn Clock> = FakeClock::new();
        let events = Arc::new(EventBus::new(clock.clone()));
        let ids: Arc<dyn lil_platform::IdSource> = Arc::new(SeqIds::new());
        let probe: Arc<dyn Probe> = Arc::new(SystemProbe);
        let failpoints = FailPoints::none();
        let hub = WorkspaceHub::new(
            env.env.clone(),
            states.clone(),
            events.clone(),
            clock.clone(),
            ids.clone(),
            failpoints.clone(),
            probe.clone(),
            lil_core::watch::WatchConfig::default(),
        );
        let reg = core_registry(
            true,
            &Arc::new(TestGates::new()),
            &states,
            &hub,
            &env.env,
            &probe,
            Arc::default(),
            &AssetServices::unwired(),
        );
        let confirm = Confirm::new(
            states.clone(),
            events.clone(),
            clock.clone(),
            ids.clone(),
            UiPresence::new(),
            Arc::new(NoNotificationPath),
            Duration::from_secs(30 * 60),
            McpConfirmation::Core,
            FailPoints::none(),
        );
        confirm.presence().opened();
        let hooks = PipelineHooks {
            confirm: AgentConfirm::new(confirm.clone()),
            ..PipelineHooks::plan2()
        };
        let lifecycle = Lifecycle::new(clock.clone(), lil_core::lifecycle::IDLE_EXIT_AFTER);
        let dispatcher = Dispatcher::new(
            reg,
            hooks,
            states.clone(),
            events.clone(),
            hub.clone(),
            ids,
            clock,
            lifecycle,
        );
        let fx = Fx {
            dispatcher,
            confirm,
            hub,
            events,
            failpoints,
            ws_ref,
            ws,
            states,
            env,
        };
        fx.open().await.index.ready().await;
        fx
    }

    async fn shutdown(self) {
        let Fx {
            dispatcher,
            hub,
            states,
            env,
            confirm,
            ..
        } = self;
        hub.close_all().await;
        states.close();
        drop((dispatcher, hub, states, confirm));
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

    fn session(&self, caller: CallerKind, id: &str) -> Arc<Session> {
        Arc::new(Session {
            id: id.into(),
            caller,
            non_interactive: false,
            workspace: Some(self.ws_ref.clone()),
            capabilities: lil_protocol::CAPABILITIES
                .iter()
                .map(|c| (*c).to_string())
                .collect(),
            client_pid: 1,
            profile_id: None,
        })
    }

    fn app(&self) -> Arc<Session> {
        self.session(CallerKind::Ui("test".into()), "s_app")
    }

    fn cli(&self) -> Arc<Session> {
        self.session(CallerKind::Cli, "s_cli")
    }

    async fn call_as(
        &self,
        s: &Arc<Session>,
        name: &str,
        args: Value,
        key: Option<&str>,
    ) -> Outcome<ToolOutput> {
        let Value::Object(arguments) = args else {
            panic!("arguments must be an object");
        };
        self.dispatcher
            .call(
                s.clone(),
                ToolCallParams {
                    name: name.into(),
                    arguments,
                    idempotency_key: key.map(str::to_string),
                    trace_id: None,
                },
            )
            .await
    }

    /// `name` called by the app (a fresh key for a tool that isn't `read`).
    async fn call(&self, name: &str, args: Value) -> Outcome<ToolOutput> {
        let read = matches!(
            self.dispatcher
                .registry()
                .get(name)
                .map(|t| t.descriptor().effect),
            Some(lil_protocol::EffectClass::Read)
        );
        let k = (!read).then(key);
        self.call_as(&self.app(), name, args, k.as_deref()).await
    }

    async fn ok(&self, name: &str, args: Value) -> Value {
        ok_of(
            &self.call(name, args.clone()).await,
            &format!("{name} {args}"),
        )
    }

    async fn err(&self, name: &str, args: Value) -> ErrorBody {
        err_of(
            &self.call(name, args.clone()).await,
            &format!("{name} {args}"),
        )
    }

    async fn answer(&self, id: &str, answer: Answer) {
        self.confirm
            .answer(
                &self.app(),
                ConfirmationAnswerParams {
                    confirmation_id: id.into(),
                    answer,
                    toast_token: None,
                    cap: None,
                },
            )
            .await
            .unwrap_or_else(|e| panic!("the answer was refused: {e:?}"));
    }

    async fn pending_view(&self, id: &str) -> Value {
        let row = self
            .confirm
            .list(ListState::Pending, 500)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("{id} is not pending"));
        serde_json::to_value(row.view()).unwrap()
    }

    /// `collection.save` by the app: the saved collection.
    async fn save(&self, args: Value) -> Value {
        self.ok("collection.save", args).await["collection"].clone()
    }

    /// The id of a new entry saved by the app.
    async fn save_id(&self, args: Value) -> String {
        self.save(args).await["id"].as_str().unwrap().to_string()
    }

    async fn list(&self) -> Vec<Value> {
        self.ok("collections.list", json!({})).await["collections"]
            .as_array()
            .unwrap()
            .clone()
    }

    async fn listed(&self, id: &str) -> Option<Value> {
        self.list().await.into_iter().find(|e| e["id"] == id)
    }

    async fn query_ids(&self, args: Value) -> Vec<String> {
        let page = self.ok("assets.query", args).await;
        page["assets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["asset_id"].as_str().unwrap().to_string())
            .collect()
    }

    async fn trash_entries(&self) -> Vec<Value> {
        self.ok("trash.list", json!({"scope": "workspace"})).await["entries"]
            .as_array()
            .unwrap()
            .clone()
    }

    fn file(&self) -> Option<String> {
        std::fs::read_to_string(ws_file(&self.ws, FILE)).ok()
    }

    fn write_file(&self, rel: &str, text: &str) {
        write_file(&ws_file(&self.ws, rel), text.as_bytes());
    }

    /// The payload file of trash entry `entry_id`: the one file in its `payload` folder.
    fn payload_of(&self, entry_id: &str) -> PathBuf {
        let trash = self.ws.join(".trash");
        for day in std::fs::read_dir(&trash).unwrap() {
            let dir = day.unwrap().path().join(entry_id);
            if dir.is_dir() {
                let files: Vec<PathBuf> = std::fs::read_dir(dir.join("payload"))
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .collect();
                assert_eq!(files.len(), 1, "{files:?}");
                return files[0].clone();
            }
        }
        panic!("no trash entry {entry_id}");
    }
}

fn ok_of(out: &Outcome<ToolOutput>, what: &str) -> Value {
    match out {
        Outcome::Ok(out) => out.structured.clone(),
        other => panic!("{what}: expected ok, got {other:?}"),
    }
}

fn err_of(out: &Outcome<ToolOutput>, what: &str) -> ErrorBody {
    match out {
        Outcome::Error { error } => error.clone(),
        other => panic!("{what}: expected an error, got {other:?}"),
    }
}

/// The `(ref, state, to)` of each of an entry's `refs`.
fn ref_states(entry: &Value) -> Vec<(String, String, Option<String>)> {
    entry["refs"]
        .as_array()
        .unwrap_or_else(|| panic!("no refs: {entry}"))
        .iter()
        .map(|r| {
            (
                r["ref"].as_str().unwrap().to_string(),
                r["state"].as_str().unwrap().to_string(),
                r["to"].as_str().map(str::to_string),
            )
        })
        .collect()
}

fn refs_of(entry: &Value) -> Vec<String> {
    ref_states(entry).into_iter().map(|(r, _, _)| r).collect()
}

/// A redirect from `from` to `to` in `registry/redirects.yaml`.
fn redirect(ws: &Path, from: &str, to: &str) {
    let text = format!(
        "redirects:\n  - from: {from}\n    to: {to}\n    moved_utc: 2026-11-01T10:00:00Z\n    by: cli\n"
    );
    write_file(&ws_file(ws, "registry/redirects.yaml"), text.as_bytes());
}

/// CORE-155: a saved search filters `assets.query` as its rule does, combined with the call's
/// other filters by AND.
#[tokio::test] // spec: CORE-155, CORE-152
async fn a_saved_search_filters_like_its_rule() {
    let fx = Fx::new(|_| {}).await;
    let props = fx
        .save_id(json!({"name": "Props", "rule": {"type": "prop_2d"}}))
        .await;
    let entry = fx.listed(&props).await.expect("the search is listed");
    assert_eq!(entry["kind"], "search", "{entry}");
    assert_eq!(entry["rule"], json!({"type": "prop_2d"}), "{entry}");
    assert!(entry.get("refs").is_none(), "{entry}");
    assert_eq!(
        fx.query_ids(json!({"collection": props})).await,
        fx.query_ids(json!({"type": "prop_2d"})).await
    );
    assert_eq!(fx.query_ids(json!({"collection": props})).await, [LAMP]);
    // Lists and the other filters work in a rule as in a query.
    let both = fx
        .save_id(
            json!({"name": "Both", "rule": {"type": ["prop_2d", "image"], "deprecated": "hide"}}),
        )
        .await;
    assert_eq!(
        fx.query_ids(json!({"collection": both})).await,
        [CRANE, LAMP]
    );
    // AND with the call's own filters.
    assert_eq!(
        fx.query_ids(json!({"collection": both, "type": "image"}))
            .await,
        [CRANE]
    );
    // An id no entry has.
    let unknown = fx
        .err(
            "assets.query",
            json!({"collection": "11111111-2222-4333-8444-555555555555"}),
        )
        .await;
    assert_eq!(unknown.code, "collection.unknown", "{unknown:?}");
    fx.shutdown().await;
}

/// CORE-155, ARCH-125: a collection's refs go through the redirects; a ref that resolves
/// nowhere matches nothing and breaks nothing.
#[tokio::test] // spec: CORE-155, ARCH-125
async fn a_collection_filters_by_its_refs_through_redirects() {
    let fx = Fx::new(|ws| redirect(ws, "sketchbook/old_crane", CRANE_REF)).await;
    let picked = fx
        .save_id(
            json!({"name": "Picked", "refs": ["sketchbook/old_crane", "sketchbook/nothing_here"]}),
        )
        .await;
    assert_eq!(fx.query_ids(json!({"collection": picked})).await, [CRANE]);
    let lamp = fx
        .save_id(json!({"name": "Lamp", "refs": [LAMP_REF]}))
        .await;
    assert_eq!(fx.query_ids(json!({"collection": lamp})).await, [LAMP]);
    // An empty collection matches nothing.
    let empty = fx.save_id(json!({"name": "Empty", "refs": []})).await;
    assert!(fx.query_ids(json!({"collection": empty})).await.is_empty());
    fx.shutdown().await;
}

/// Appendix H: a rule is an `assets.query` filter set without `sort`, `limit`, `cursor` and
/// `facets`, validated as one: anything else is `params.invalid`, and nothing is written.
#[tokio::test] // spec: CORE-155
async fn rule_with_sort_or_unknown_key_is_params_invalid() {
    let fx = Fx::new(|_| {}).await;
    for rule in [
        json!({"type": "prop_2d", "sort": {"field": "asset_id", "dir": "asc"}}),
        json!({"limit": 10}),
        json!({"cursor": "00"}),
        json!({"facets": true}),
        json!({"colour": "red"}),
        json!({"rating_min": 9}),
        json!({"label": ["teal"]}),
        json!({"status": 3}),
    ] {
        let e = fx
            .err("collection.save", json!({"name": "Bad", "rule": rule}))
            .await;
        assert_eq!(e.code, "params.invalid", "{rule}: {e:?}");
    }
    // Exactly one of rule and refs; a name for a new entry.
    for args in [
        json!({"name": "Bad", "rule": {}, "refs": []}),
        json!({"name": "Bad"}),
        json!({"refs": [CRANE_REF]}),
        json!({"name": "Bad", "refs": ["not a ref"]}),
        json!({"name": "  ", "refs": []}),
    ] {
        let e = fx.err("collection.save", args.clone()).await;
        assert_eq!(e.code, "params.invalid", "{args}: {e:?}");
    }
    assert_eq!(fx.file(), None, "nothing was written");
    fx.shutdown().await;
}

/// Appendix H: names are unique case-insensitively, the quick collection's included, also on
/// a rename; an entry may keep or recase its own name.
#[tokio::test] // spec: CORE-155
async fn names_are_unique_case_insensitively() {
    let fx = Fx::new(|_| {}).await;
    let cranes = fx
        .save_id(json!({"name": "Cranes", "refs": [CRANE_REF]}))
        .await;
    let lamps = fx
        .save_id(json!({"name": "Lamps", "refs": [LAMP_REF]}))
        .await;
    for args in [
        json!({"name": "CRANES", "refs": [LAMP_REF]}),
        json!({"name": "cranes", "rule": {"type": "image"}}),
        json!({"id": lamps, "name": "cRaNeS"}),
        json!({"name": "quick COLLECTION", "refs": []}),
    ] {
        let e = fx.err("collection.save", args.clone()).await;
        assert_eq!(e.code, "collection.name_taken", "{args}: {e:?}");
        assert!(e.message.contains("is already called"), "{e:?}");
    }
    let renamed = fx.save(json!({"id": cranes, "name": "CRANES"})).await;
    assert_eq!(renamed["name"], "CRANES", "{renamed}");
    assert_eq!(fx.list().await.len(), 3, "the quick collection and two");
    fx.shutdown().await;
}

/// Appendix H: `add` and `remove` change a collection's refs (no ref twice), only a
/// collection's; each change is published on `workspace` with the previous entry.
#[tokio::test] // spec: CORE-155
async fn add_and_remove_toggle_refs() {
    let fx = Fx::new(|_| {}).await;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    fx.events.attach(9, tx);
    fx.events
        .subscribe(9, &["workspace".to_string()], &Default::default())
        .await;

    let picks = fx
        .save_id(json!({"name": "Picks", "refs": [CRANE_REF]}))
        .await;
    let added = fx
        .save(json!({"id": picks, "add": [LAMP_REF, CRANE_REF]}))
        .await;
    assert_eq!(refs_of(&added), [CRANE_REF, LAMP_REF]);
    let removed = fx
        .save(json!({"id": picks, "remove": [CRANE_REF, "sketchbook/not_there"]}))
        .await;
    assert_eq!(refs_of(&removed), [LAMP_REF]);
    let both = fx
        .save(json!({"id": picks, "add": [CRANE_REF], "remove": [LAMP_REF]}))
        .await;
    assert_eq!(refs_of(&both), [CRANE_REF]);
    let file = fx.file().unwrap();
    assert!(
        file.contains(CRANE_REF) && !file.contains(LAMP_REF),
        "{file}"
    );

    let search = fx
        .save_id(json!({"name": "Search", "rule": {"type": "image"}}))
        .await;
    let e = fx
        .err("collection.save", json!({"id": search, "add": [CRANE_REF]}))
        .await;
    assert_eq!(e.code, "params.invalid", "{e:?}");
    let e = fx
        .err(
            "collection.save",
            json!({"id": picks, "add": [CRANE_REF], "refs": []}),
        )
        .await;
    assert_eq!(e.code, "params.invalid", "{e:?}");

    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while seen.len() < 5 && tokio::time::Instant::now() < deadline {
        while let Ok(line) = rx.try_recv() {
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["params"]["data"]["kind"] == "collection" {
                seen.push(v["params"]["data"].clone());
            }
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(seen.len(), 5, "{seen:?}");
    assert_eq!(seen[0], json!({"kind": "collection", "id": picks}));
    assert_eq!(seen[1]["id"], picks.as_str());
    assert_eq!(seen[1]["previous"]["refs"], json!([CRANE_REF]), "{seen:?}");
    assert_eq!(seen[2]["previous"]["refs"], json!([CRANE_REF, LAMP_REF]));
    fx.shutdown().await;
}

/// P4b-R27: the quick collection (the nil UUID, "Quick collection", `builtin: quick`) is listed
/// first before anything is saved, takes refs like any collection, is saved on its first
/// change, and is never deleted (`params.invalid`) nor made a saved search.
#[tokio::test] // spec: CORE-155
async fn quick_collection_is_listed_before_its_first_save_and_never_deleted() {
    let fx = Fx::new(|_| {}).await;
    let list = fx.list().await;
    assert_eq!(
        list,
        [
            json!({"id": QUICK, "name": "Quick collection", "kind": "collection",
                "builtin": "quick", "refs": []})
        ]
    );
    assert_eq!(fx.file(), None);
    let e = fx.err("collection.delete", json!({"id": QUICK})).await;
    assert_eq!(e.code, "params.invalid", "{e:?}");

    let quick = fx.save(json!({"id": QUICK, "add": [CRANE_REF]})).await;
    assert_eq!(quick["id"], QUICK);
    assert_eq!(quick["builtin"], "quick");
    assert_eq!(refs_of(&quick), [CRANE_REF]);
    let file = fx.file().expect("saved on its first change");
    assert!(
        file.contains(QUICK) && file.contains("builtin: quick"),
        "{file}"
    );
    let other = fx.save_id(json!({"name": "Other", "refs": []})).await;
    let ids: Vec<Value> = fx
        .list()
        .await
        .into_iter()
        .map(|e| e["id"].clone())
        .collect();
    assert_eq!(ids, [json!(QUICK), json!(other)]);

    let e = fx
        .err(
            "collection.save",
            json!({"id": QUICK, "rule": {"type": "image"}}),
        )
        .await;
    assert_eq!(e.code, "params.invalid", "{e:?}");
    let e = fx.err("collection.delete", json!({"id": QUICK})).await;
    assert_eq!(e.code, "params.invalid", "{e:?}");
    assert!(fx.listed(QUICK).await.is_some());
    assert_eq!(fx.trash_entries().await, Vec::<Value>::new());
    fx.shutdown().await;
}

/// CORE-155, ARCH-052, APP-115: `collection.delete` is `destructive`: an agent's call waits for
/// a confirmation that lists the collections file (`write`), and nothing changes before the
/// answer; the app's call runs at once.
#[tokio::test] // spec: CORE-155, ARCH-052, APP-115
async fn delete_from_an_agent_asks_and_from_the_app_runs() {
    let fx = Fx::new(|_| {}).await;
    let a = fx.save_id(json!({"name": "A", "refs": []})).await;
    let b = fx.save_id(json!({"name": "B", "refs": []})).await;

    let k = key();
    let args = json!({"id": a});
    let first = fx
        .call_as(&fx.cli(), "collection.delete", args.clone(), Some(&k))
        .await;
    let Outcome::PendingConfirmation(p) = &first else {
        panic!("expected a confirmation, got {first:?}");
    };
    let view = fx.pending_view(&p.confirmation_id).await;
    assert_eq!(
        view["files"],
        json!([{"path": FILE, "access": "write"}]),
        "{view}"
    );
    assert!(fx.listed(&a).await.is_some(), "nothing changed yet");
    assert!(fx.trash_entries().await.is_empty());
    fx.answer(&p.confirmation_id, Answer::Approve).await;
    let done = ok_of(
        &fx.call_as(&fx.cli(), "collection.delete", args, Some(&k))
            .await,
        "the approved retry",
    );
    assert!(
        done["entry_id"].as_str().unwrap().starts_with("te_"),
        "{done}"
    );
    assert!(fx.listed(&a).await.is_none());

    let by_app = fx.ok("collection.delete", json!({"id": b})).await;
    assert!(by_app["entry_id"].is_string(), "{by_app}");
    assert!(fx.listed(&b).await.is_none());
    let e = fx.err("collection.delete", json!({"id": b})).await;
    assert_eq!(e.code, "collection.unknown", "{e:?}");
    fx.shutdown().await;
}

/// CORE-155, Appendix E: the payload is in the trash before the YAML edit; a failure between
/// them (the fail point `collections.after_payload`) leaves the entry in both, and the retry
/// reuses the payload: one trash entry, and the entry gone from the file.
#[tokio::test] // spec: CORE-155, ARCH-058
async fn delete_writes_the_trash_payload_before_the_yaml_edit() {
    let fx = Fx::new(|_| {}).await;
    let keep = fx
        .save_id(json!({"name": "Keep", "refs": [LAMP_REF]}))
        .await;
    let gone = fx
        .save_id(json!({"name": "Gone", "refs": [CRANE_REF]}))
        .await;
    let fail = Arc::new(AtomicBool::new(true));
    let seen: Arc<Mutex<Vec<(usize, bool)>>> = Arc::default();
    {
        let (fail, seen, ws, gone) = (fail.clone(), seen.clone(), fx.ws.clone(), gone.clone());
        fx.failpoints.on(
            "collections.after_payload",
            Box::new(move || {
                let trash = ws.join(".trash");
                let payloads = walk(&trash)
                    .into_iter()
                    .filter(|p| p.ends_with(format!("collection-{gone}.yaml")))
                    .count();
                let in_file = std::fs::read_to_string(ws_file(&ws, FILE))
                    .unwrap()
                    .contains(&gone);
                seen.lock().unwrap().push((payloads, in_file));
                if fail.swap(false, Ordering::SeqCst) {
                    return Err(std::io::Error::other(
                        "fail point collections.after_payload",
                    ));
                }
                Ok(())
            }),
        );
    }
    let failed = fx.err("collection.delete", json!({"id": gone})).await;
    assert_eq!(failed.code, "store.io", "{failed:?}");
    assert!(fx.listed(&gone).await.is_some(), "still in the file");
    let entries = fx.trash_entries().await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    let first = entries[0]["entry_id"].as_str().unwrap().to_string();
    assert_eq!(entries[0]["reason"], "collection.delete");
    assert_eq!(entries[0]["original_path"], FILE);
    let payload = fx.payload_of(&first);
    assert_eq!(
        payload.file_name().unwrap().to_string_lossy(),
        format!("collection-{gone}.yaml")
    );
    let one = std::fs::read_to_string(&payload).unwrap();
    assert!(
        one.contains(&gone) && one.contains("Gone") && one.contains(CRANE_REF),
        "{one}"
    );
    assert!(!one.contains(&keep), "one entry only: {one}");

    let done = fx.ok("collection.delete", json!({"id": gone})).await;
    assert_eq!(
        done["entry_id"],
        first.as_str(),
        "the earlier payload is reused"
    );
    let entries = fx.trash_entries().await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert!(!fx.file().unwrap().contains(&gone));
    assert!(fx.listed(&keep).await.is_some());
    // At the fail point, both times: the payload in the trash, the entry still in the file.
    assert_eq!(*seen.lock().unwrap(), [(1, true), (1, true)]);
    fx.shutdown().await;
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(read) = std::fs::read_dir(dir) {
        for e in read {
            let path = e.unwrap().path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path);
            }
        }
    }
    out
}

/// CORE-155: `trash.restore` of a deleted entry merges it back into the file by id (`to`
/// doesn't apply), after the entries there; a used name or id is `trash.restore_conflict` and
/// the entry stays in the trash.
#[tokio::test] // spec: CORE-155, CORE-037
async fn restore_merges_by_id_and_conflicts_on_a_used_name() {
    let fx = Fx::new(|_| {}).await;
    let cranes = fx
        .save_id(json!({"name": "Cranes", "refs": [CRANE_REF]}))
        .await;
    let props = fx
        .save_id(json!({"name": "Props", "rule": {"type": "prop_2d"}}))
        .await;
    let entry = fx.ok("collection.delete", json!({"id": cranes})).await["entry_id"]
        .as_str()
        .unwrap()
        .to_string();
    let e = fx
        .err("trash.restore", json!({"entry_id": entry, "to": "other"}))
        .await;
    assert_eq!(e.code, "params.invalid", "{e:?}");
    let restored = fx.ok("trash.restore", json!({"entry_id": entry})).await;
    assert_eq!(restored["path"], FILE, "{restored}");
    let ids: Vec<Value> = fx
        .list()
        .await
        .into_iter()
        .map(|e| e["id"].clone())
        .collect();
    assert_eq!(ids, [json!(QUICK), json!(props), json!(cranes)]);
    let back = fx.listed(&cranes).await.unwrap();
    assert_eq!(back["name"], "Cranes");
    assert_eq!(refs_of(&back), [CRANE_REF]);
    assert!(
        fx.trash_entries().await.is_empty(),
        "the entry left the trash"
    );
    assert_eq!(fx.query_ids(json!({"collection": cranes})).await, [CRANE]);

    // Its name is used meanwhile.
    let again = fx.ok("collection.delete", json!({"id": cranes})).await["entry_id"]
        .as_str()
        .unwrap()
        .to_string();
    let other = fx
        .save_id(json!({"name": "CRANES", "refs": [LAMP_REF]}))
        .await;
    let before = fx.file().unwrap();
    let e = fx.err("trash.restore", json!({"entry_id": again})).await;
    assert_eq!(e.code, "trash.restore_conflict", "{e:?}");
    assert_eq!(fx.file().unwrap(), before, "nothing replaced");
    assert_eq!(fx.trash_entries().await.len(), 1);

    // Its id is used meanwhile (written by hand, under another name).
    fx.ok("collection.delete", json!({"id": other})).await;
    let mut text = fx.file().unwrap();
    text.push_str(&format!(
        "  - id: {cranes}\n    name: Hand made\n    refs: []\n"
    ));
    fx.write_file(FILE, &text);
    let e = fx.err("trash.restore", json!({"entry_id": again})).await;
    assert_eq!(e.code, "trash.restore_conflict", "{e:?}");
    assert_eq!(fx.trash_entries().await.len(), 2);
    fx.shutdown().await;
}

/// ARCH-076, CORE-155: saves, renames and deletes are targeted edits: comments, blank lines,
/// the other entries and their order stay byte for byte, and an entry's unknown keys stay.
#[tokio::test] // spec: CORE-155, ARCH-076
async fn collections_yaml_keeps_comments_and_order() {
    const A: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const B: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    const C: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
    let start = format!(
        "# My collections, kept by hand too.\n\
         collections:\n\
         \x20 # The first one.\n\
         \x20 - id: {A}\n\
         \x20   name: First   # a trailing comment\n\
         \x20   refs:\n\
         \x20     - {CRANE_REF}\n\
         \n\
         \x20 # The second one, a search.\n\
         \x20 - id: {B}\n\
         \x20   name: Second\n\
         \x20   note: kept\n\
         \x20   rule: {{type: image}}\n\
         \x20 - id: {C}\n\
         \x20   name: Third\n\
         \x20   refs: []\n\
         # The end.\n"
    );
    let fx = Fx::new(|ws| write_file(&ws_file(ws, FILE), start.as_bytes())).await;
    let ids: Vec<Value> = fx
        .list()
        .await
        .into_iter()
        .map(|e| e["id"].clone())
        .collect();
    assert_eq!(ids, [json!(QUICK), json!(A), json!(B), json!(C)]);

    // Rename the middle one: only its own lines change, its unknown key stays.
    fx.save(json!({"id": B, "name": "Second, renamed"})).await;
    let after = fx.file().unwrap();
    let want = start.replace(
        &format!("  - id: {B}\n    name: Second\n    note: kept\n    rule: {{type: image}}\n"),
        &format!("  - id: {B}\n    name: \"Second, renamed\"\n    rule:\n      type: image\n    note: kept\n"),
    );
    assert_ne!(want, start);
    assert_eq!(after, want);

    // Add one: appended after the last entry.
    let d = fx
        .save_id(json!({"name": "Fourth", "refs": [LAMP_REF]}))
        .await;
    let after = fx.file().unwrap();
    let want = want.replace(
        "    refs: []\n# The end.\n",
        &format!("    refs: []\n  - id: {d}\n    name: Fourth\n    refs:\n      - {LAMP_REF}\n# The end.\n"),
    );
    assert_eq!(after, want);

    // Delete the first: its lines go, the comments around it stay.
    fx.ok("collection.delete", json!({"id": A})).await;
    let after = fx.file().unwrap();
    let want = want.replace(
        &format!(
            "  - id: {A}\n    name: First   # a trailing comment\n    refs:\n      - {CRANE_REF}\n"
        ),
        "",
    );
    assert_eq!(after, want);
    let ids: Vec<Value> = fx
        .list()
        .await
        .into_iter()
        .map(|e| e["id"].clone())
        .collect();
    assert_eq!(ids, [json!(QUICK), json!(B), json!(C), json!(d)]);

    // Deleting every entry leaves an empty list, comments kept.
    for id in [B, C, d.as_str()] {
        fx.ok("collection.delete", json!({"id": id})).await;
    }
    let after = fx.file().unwrap();
    assert!(
        after.starts_with("# My collections, kept by hand too.\ncollections: []\n"),
        "{after}"
    );
    assert!(
        after.contains("# The second one, a search.") && after.ends_with("# The end.\n"),
        "{after}"
    );
    fx.shutdown().await;
}

/// Appendix H: `collections.list` gives each ref's state: `ok`, `redirected` (with `to`),
/// `trashed` or `missing`.
#[tokio::test] // spec: CORE-155, ARCH-125, CORE-037
async fn list_reports_missing_trashed_and_redirected_refs() {
    let fx = Fx::new(|ws| {
        redirect(ws, "sketchbook/old_lamp", LAMP_REF);
        write_file(
            &ws_file(ws, "projects/sketchbook/assets/old_kite/old_kite.manifest.json"),
            br#"{"asset_id": "old_kite", "project": "sketchbook", "type": "image", "status": "candidate"}"#,
        );
    })
    .await;
    let open = fx.open().await;
    open.store
        .trash(
            &WsPath::parse("projects/sketchbook/assets/old_kite").unwrap(),
            "test",
        )
        .await
        .expect("the kite goes to the trash");
    let mixed = fx
        .save(json!({"name": "Mixed", "refs": [
            CRANE_REF, "sketchbook/old_lamp", "sketchbook/old_kite", "sketchbook/never_was"]}))
        .await;
    let want = vec![
        (CRANE_REF.to_string(), "ok".to_string(), None),
        (
            "sketchbook/old_lamp".to_string(),
            "redirected".to_string(),
            Some(LAMP_REF.to_string()),
        ),
        (
            "sketchbook/old_kite".to_string(),
            "trashed".to_string(),
            None,
        ),
        (
            "sketchbook/never_was".to_string(),
            "missing".to_string(),
            None,
        ),
    ];
    assert_eq!(ref_states(&mixed), want);
    let listed = fx.listed(mixed["id"].as_str().unwrap()).await.unwrap();
    assert_eq!(listed["kind"], "collection");
    assert_eq!(ref_states(&listed), want);
    assert_eq!(
        fx.query_ids(json!({"collection": mixed["id"]})).await,
        [CRANE, LAMP]
    );
    fx.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Fix round 1 (review C-1, I-1, M-1, M-3, M-4, M-6).
// ---------------------------------------------------------------------------------------------

/// Review C-1: once the last entry is deleted (`collections: []`), the next entries are written
/// as a block list again, so the B key on the quick collection keeps working, as do its later
/// edits and a delete.
#[tokio::test] // spec: CORE-155, ARCH-076
async fn the_list_stays_editable_after_it_empties() {
    let fx = Fx::new(|_| {}).await;
    let a = fx
        .save_id(json!({"name": "A", "rule": {"type": "image"}}))
        .await;
    fx.ok("collection.delete", json!({"id": a})).await;
    assert_eq!(fx.file().unwrap(), "collections: []\n");
    // The B key twice, then once more to take one out.
    fx.save(json!({"id": QUICK, "add": [CRANE_REF]})).await;
    let quick = fx.save(json!({"id": QUICK, "add": [LAMP_REF]})).await;
    assert_eq!(refs_of(&quick), [CRANE_REF, LAMP_REF]);
    let quick = fx.save(json!({"id": QUICK, "remove": [CRANE_REF]})).await;
    assert_eq!(refs_of(&quick), [LAMP_REF]);
    let b = fx.save_id(json!({"name": "B", "refs": []})).await;
    fx.save(json!({"id": b, "name": "B2"})).await;
    fx.ok("collection.delete", json!({"id": b})).await;
    let file = fx.file().unwrap();
    assert!(file.starts_with("collections:\n  - id: "), "{file}");
    assert!(!file.contains(&b), "{file}");
    fx.shutdown().await;
}

/// Review C-1: the only entry, deleted and restored, can be renamed and deleted again.
#[tokio::test] // spec: CORE-155, CORE-037, ARCH-076
async fn a_restored_only_entry_can_be_renamed_and_deleted() {
    let fx = Fx::new(|_| {}).await;
    let only = fx
        .save_id(json!({"name": "Only", "refs": [CRANE_REF]}))
        .await;
    let entry = fx.ok("collection.delete", json!({"id": only})).await["entry_id"]
        .as_str()
        .unwrap()
        .to_string();
    fx.ok("trash.restore", json!({"entry_id": entry})).await;
    let renamed = fx.save(json!({"id": only, "name": "Only, renamed"})).await;
    assert_eq!(renamed["name"], "Only, renamed");
    fx.ok("collection.delete", json!({"id": only})).await;
    assert!(fx.listed(&only).await.is_none());
    assert_eq!(fx.file().unwrap(), "collections: []\n");
    fx.shutdown().await;
}

/// Review I-1, Appendix H: the cursor hashes `collection` as given, its id, so a change to the
/// collection between pages (a ref taken out, the B key) keeps an outstanding cursor valid.
#[tokio::test] // spec: CORE-155, APP-138
async fn a_cursor_survives_a_change_to_its_collection() {
    let fx = Fx::new(|_| {}).await;
    let picks = fx
        .save_id(json!({"name": "Picks", "refs": [CRANE_REF, LAMP_REF]}))
        .await;
    fx.save(json!({"id": QUICK, "add": [CRANE_REF, LAMP_REF]}))
        .await;
    for (id, change) in [
        (picks.as_str(), json!({"id": picks, "remove": [CRANE_REF]})),
        (QUICK, json!({"id": QUICK, "remove": [CRANE_REF]})),
    ] {
        let first = fx
            .ok("assets.query", json!({"collection": id, "limit": 1}))
            .await;
        assert_eq!(first["assets"][0]["asset_id"], CRANE, "{first}");
        let cursor = first["next_cursor"].as_str().unwrap().to_string();
        fx.save(change).await;
        let next = fx
            .ok(
                "assets.query",
                json!({"collection": id, "limit": 1, "cursor": cursor}),
            )
            .await;
        assert_eq!(next["assets"][0]["asset_id"], LAMP, "{next}");
        // The B key back on: the cursor still pages.
        fx.save(json!({"id": id, "add": [CRANE_REF]})).await;
        let again = fx
            .call(
                "assets.query",
                json!({"collection": id, "limit": 1, "cursor": cursor}),
            )
            .await;
        ok_of(&again, "the cursor after the B key");
    }
    fx.shutdown().await;
}

/// Review M-1: a save that changes nothing writes nothing and publishes nothing, the quick
/// collection before its first save included.
#[tokio::test] // spec: CORE-155
async fn a_save_that_changes_nothing_writes_nothing() {
    let fx = Fx::new(|_| {}).await;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    fx.events.attach(9, tx);
    fx.events
        .subscribe(9, &["workspace".to_string()], &Default::default())
        .await;
    let quick = fx
        .save(json!({"id": QUICK, "remove": ["sketchbook/not_there"]}))
        .await;
    assert_eq!(quick["id"], QUICK);
    assert_eq!(fx.file(), None, "nothing written");
    let picks = fx
        .save_id(json!({"name": "Picks", "refs": [CRANE_REF]}))
        .await;
    let before = fx.file().unwrap();
    fx.save(json!({"id": picks, "add": [CRANE_REF]})).await;
    assert_eq!(fx.file().unwrap(), before);
    // A later change: its event follows the new entry's, with nothing between them.
    fx.save(json!({"id": picks, "name": "Picks 2"})).await;
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while seen.len() < 2 && tokio::time::Instant::now() < deadline {
        while let Ok(line) = rx.try_recv() {
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["params"]["data"]["kind"] == "collection" {
                seen.push(v["params"]["data"].clone());
            }
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[0], json!({"kind": "collection", "id": picks}));
    assert_eq!(seen[1]["previous"]["name"], "Picks", "{seen:?}");
    fx.shutdown().await;
}

/// Review M-4: a rule holding a null is `params.invalid`, naming the key; nothing is written.
#[tokio::test] // spec: CORE-155
async fn a_rule_with_a_null_is_params_invalid() {
    let fx = Fx::new(|_| {}).await;
    let e = fx
        .err(
            "collection.save",
            json!({"name": "Nulls", "rule": {"label": null, "type": "image"}}),
        )
        .await;
    assert_eq!(e.code, "params.invalid", "{e:?}");
    assert!(e.message.contains("label"), "{e:?}");
    assert_eq!(fx.file(), None);
    fx.shutdown().await;
}

/// Review M-3: an entry changed by an outside edit after the delete read it (here at the fail
/// point, between the payload and the YAML edit) is never taken out: `store.hash_conflict`,
/// the file as the outside edit left it, and the payload out of the trash again.
#[tokio::test] // spec: CORE-155, ARCH-058
async fn a_delete_refuses_an_entry_changed_meanwhile() {
    let fx = Fx::new(|_| {}).await;
    let id = fx
        .save_id(json!({"name": "Before", "refs": [CRANE_REF]}))
        .await;
    {
        let ws = fx.ws.clone();
        fx.failpoints.on(
            "collections.after_payload",
            Box::new(move || {
                let path = ws_file(&ws, FILE);
                let text = std::fs::read_to_string(&path).unwrap();
                write_file(
                    &path,
                    text.replace("name: Before", "name: After").as_bytes(),
                );
                Ok(())
            }),
        );
    }
    let e = fx.err("collection.delete", json!({"id": id})).await;
    assert_eq!(e.code, "store.hash_conflict", "{e:?}");
    let listed = fx.listed(&id).await.expect("the newer entry stays");
    assert_eq!(listed["name"], "After");
    assert!(
        fx.trash_entries().await.is_empty(),
        "the payload went again"
    );
    fx.shutdown().await;
}

/// Review M-6: a restore cut short after its merge edit (the entry back in the file exactly as
/// it was, the trash entry still there) finishes on the retry: the entry leaves the trash and
/// the file is left as it is.
#[tokio::test] // spec: CORE-155, CORE-037
async fn a_restore_cut_short_after_its_edit_finishes() {
    let fx = Fx::new(|_| {}).await;
    let id = fx
        .save_id(json!({"name": "Back", "refs": [CRANE_REF]}))
        .await;
    let entry = fx.ok("collection.delete", json!({"id": id})).await["entry_id"]
        .as_str()
        .unwrap()
        .to_string();
    // The merge edit as a cut-short restore left it: the one-entry file's entry in the file.
    let payload = std::fs::read_to_string(fx.payload_of(&entry)).unwrap();
    fx.write_file(FILE, &payload);
    let restored = fx.ok("trash.restore", json!({"entry_id": entry})).await;
    assert_eq!(restored["path"], FILE, "{restored}");
    assert!(restored.get("warnings").is_none(), "{restored}");
    assert_eq!(fx.file().unwrap(), payload, "left as it is");
    assert!(fx.trash_entries().await.is_empty());
    let ids: Vec<Value> = fx
        .list()
        .await
        .into_iter()
        .map(|e| e["id"].clone())
        .collect();
    assert_eq!(ids, [json!(QUICK), json!(id)]);
    fx.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Audit fix round 1 (audit H-1, M-1, M-2, L-1, L-2).
// ---------------------------------------------------------------------------------------------

/// The `collection.delete` trash entries, newest last.
async fn delete_entries(fx: &Fx) -> Vec<Value> {
    fx.trash_entries()
        .await
        .into_iter()
        .filter(|e| e["reason"] == "collection.delete")
        .collect()
}

/// Whether `id` is listed, else restored from its newest `collection.delete` trash entry and
/// listed then.
async fn listed_or_restorable(fx: &Fx, id: &str) -> bool {
    if fx.listed(id).await.is_some() {
        return true;
    }
    for entry in delete_entries(fx).await.into_iter().rev() {
        let out = fx
            .call("trash.restore", json!({"entry_id": entry["entry_id"]}))
            .await;
        if matches!(out, Outcome::Ok(_)) && fx.listed(id).await.is_some() {
            return true;
        }
    }
    false
}

/// Audit H-1 (probe G): the store fails after its replace during a delete (`store.after_replace`,
/// once). The entry is in the file or restorable from the trash, also after `Store::recover()`.
#[tokio::test] // spec: CORE-155, ARCH-058, QA-055
async fn a_delete_failing_after_its_replace_loses_nothing() {
    let fx = Fx::new(|_| {}).await;
    let id = fx
        .save_id(json!({"name": "Gone", "refs": [CRANE_REF]}))
        .await;
    let armed = Arc::new(AtomicBool::new(false));
    {
        let armed = armed.clone();
        fx.failpoints.on(
            "store.after_replace",
            Box::new(move || {
                if armed.swap(false, Ordering::SeqCst) {
                    return Err(std::io::Error::other("fail point store.after_replace"));
                }
                Ok(())
            }),
        );
    }
    armed.store(true, Ordering::SeqCst);
    let out = fx.call("collection.delete", json!({"id": id})).await;
    assert!(!armed.load(Ordering::SeqCst), "the fail point was hit");
    // The entry left the file, so the delete answers as made, with a warning.
    let done = ok_of(&out, "a delete whose store failed after the replace");
    assert!(
        done["warnings"].as_array().is_some_and(|w| !w.is_empty()),
        "{done}"
    );
    let open = fx.open().await;
    open.store.recover().await.expect("recovery");
    assert!(listed_or_restorable(&fx, &id).await, "the entry is lost");
    fx.shutdown().await;
}

/// Audit M-1 (probe A): a trash record whose payload is gone (as a crash inside the trash write
/// leaves it) is never reused: the retry writes a fresh payload, and the entry is restorable.
#[tokio::test] // spec: CORE-155, ARCH-058
async fn a_retry_never_reuses_a_record_without_its_payload() {
    let fx = Fx::new(|_| {}).await;
    let id = fx
        .save_id(json!({"name": "Kept", "refs": [CRANE_REF]}))
        .await;
    let fail = Arc::new(AtomicBool::new(true));
    {
        let fail = fail.clone();
        fx.failpoints.on(
            "collections.after_payload",
            Box::new(move || {
                if fail.swap(false, Ordering::SeqCst) {
                    return Err(std::io::Error::other(
                        "fail point collections.after_payload",
                    ));
                }
                Ok(())
            }),
        );
    }
    fx.err("collection.delete", json!({"id": id})).await;
    let first = delete_entries(&fx).await[0]["entry_id"]
        .as_str()
        .unwrap()
        .to_string();
    let payload = fx.payload_of(&first);
    remove_by_hand(&payload);
    let done = fx.ok("collection.delete", json!({"id": id})).await;
    assert_ne!(done["entry_id"], first.as_str(), "a fresh entry: {done}");
    assert!(fx.listed(&id).await.is_none());
    fx.ok("trash.restore", json!({"entry_id": done["entry_id"]}))
        .await;
    assert_eq!(fx.listed(&id).await.unwrap()["name"], "Kept");
    fx.shutdown().await;
}

/// Removes a file as an outside program (or a crash) would.
#[allow(clippy::disallowed_methods)]
fn remove_by_hand(path: &Path) {
    std::fs::remove_file(path).unwrap();
}

/// Audit M-2 (probe B): a `registry/redirects.yaml` that doesn't parse never turns a save into
/// an error after it is made, the list still lists every entry, and a query by a collection
/// answers with its live refs.
#[tokio::test] // spec: CORE-155, ARCH-125
async fn a_broken_redirects_file_never_breaks_the_collections() {
    let fx = Fx::new(|ws| {
        write_file(&ws_file(ws, "registry/redirects.yaml"), b"redirects: 5\n");
    })
    .await;
    let search = fx
        .save_id(json!({"name": "Search", "rule": {"type": "image"}}))
        .await;
    let saved = fx
        .ok(
            "collection.save",
            json!({"id": QUICK, "add": ["sketchbook/never_was", CRANE_REF]}),
        )
        .await;
    assert_eq!(
        ref_states(&saved["collection"]),
        [
            (
                "sketchbook/never_was".to_string(),
                "missing".to_string(),
                None
            ),
            (CRANE_REF.to_string(), "ok".to_string(), None),
        ]
    );
    assert!(
        saved["warnings"].as_array().is_some_and(|w| !w.is_empty()),
        "{saved}"
    );
    let list = fx.ok("collections.list", json!({})).await;
    let ids: Vec<Value> = list["collections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].clone())
        .collect();
    assert_eq!(ids, [json!(QUICK), json!(search)], "{list}");
    assert!(
        list["warnings"].as_array().is_some_and(|w| !w.is_empty()),
        "{list}"
    );
    assert_eq!(fx.query_ids(json!({"collection": QUICK})).await, [CRANE]);
    fx.shutdown().await;
}

/// Audit L-1 (probe C): an earlier attempt's trash entry older than a day is never reused, so
/// the deleted entry is still restorable after a maintenance pass two days on.
#[tokio::test] // spec: CORE-155, CORE-037, ARCH-058
async fn an_old_earlier_payload_is_not_reused() {
    use lil_core::store::trash::parse_rfc3339;
    let fx = Fx::new(|_| {}).await;
    let id = fx
        .save_id(json!({"name": "Aged", "refs": [CRANE_REF]}))
        .await;
    let fail = Arc::new(AtomicBool::new(true));
    {
        let fail = fail.clone();
        fx.failpoints.on(
            "collections.after_payload",
            Box::new(move || {
                if fail.swap(false, Ordering::SeqCst) {
                    return Err(std::io::Error::other(
                        "fail point collections.after_payload",
                    ));
                }
                Ok(())
            }),
        );
    }
    fx.err("collection.delete", json!({"id": id})).await;
    let old = delete_entries(&fx).await[0].clone();
    let old_id = old["entry_id"].as_str().unwrap().to_string();
    // Age the earlier attempt's record: moved 29 days ago, purged in a day.
    let now = parse_rfc3339(old["moved_utc"].as_str().unwrap()).unwrap();
    let record = fx
        .payload_of(&old_id)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("restore.json");
    let mut json: Value = serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
    json["moved_utc"] = json!(lil_schema::time::rfc3339(now - 29 * 86_400));
    json["purge_after"] = json!(lil_schema::time::rfc3339(now + 86_400));
    write_file(
        &record,
        serde_json::to_string_pretty(&json).unwrap().as_bytes(),
    );

    let done = fx.ok("collection.delete", json!({"id": id})).await;
    assert_ne!(done["entry_id"], old_id.as_str(), "a fresh entry: {done}");
    let open = fx.open().await;
    let report = open.store.maintenance(now + 2 * 86_400).await.unwrap();
    assert_eq!(
        report.purged,
        [old_id],
        "the old entry purges on its own date"
    );
    fx.ok("trash.restore", json!({"entry_id": done["entry_id"]}))
        .await;
    assert_eq!(fx.listed(&id).await.unwrap()["name"], "Aged");
    fx.shutdown().await;
}

/// Audit L-2 (probe F): an entry with a hand-written null (`note:`) and a float deletes,
/// restores and renames through the tools, those keys kept.
#[tokio::test] // spec: CORE-155, ARCH-076
async fn an_entry_with_a_null_or_float_deletes_and_restores() {
    const ID: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
    let start = format!(
        "collections:\n  - id: {ID}\n    name: Hand\n    refs: []\n    note:\n    weight: 1.5\n"
    );
    let fx = Fx::new(|ws| write_file(&ws_file(ws, FILE), start.as_bytes())).await;
    let entry = fx.ok("collection.delete", json!({"id": ID})).await["entry_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(fx.listed(ID).await.is_none());
    fx.ok("trash.restore", json!({"entry_id": entry})).await;
    fx.save(json!({"id": ID, "name": "Hand 2"})).await;
    let file = fx.file().unwrap();
    assert!(
        file.contains("name: Hand 2") && file.contains("note: ~") && file.contains("weight: 1.5"),
        "{file}"
    );
    fx.shutdown().await;
}
