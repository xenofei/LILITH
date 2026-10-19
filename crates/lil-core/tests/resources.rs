//! Libraries (APP-078–080, APP-082, CORE-110; Appendix F "Resources"): typed resources in the
//! workspace and personal scopes, saves that move the old file to the trash, soft deletes, the
//! 30-day purge of the personal trash and the `resources` topic, through the dispatcher of an
//! in-process core with the fake clock and ids (QA-031). Fixture folders live under the test's
//! `TestEnv` root.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lil_core::events::EventBus;
use lil_core::resources::{RESOURCE_KINDS, Resources, register_resource_tools};
use lil_core::session::Session;
use lil_core::state::StateRegistry;
use lil_core::store::{Personal, TRASH_KEEP_DAYS};
use lil_core::tools::{Dispatcher, PipelineHooks, TestGates, core_registry};
use lil_core::workspace::{WorkspaceHub, WorkspaceRef};
use lil_platform::fs::{Probe, SystemProbe};
use lil_platform::{Clock, IdSource};
use lil_protocol::{CallerKind, ErrorBody, Outcome, ToolCallParams};
use lil_testkit::{FakeClock, SeqIds, TestEnv};
use serde_json::{Value, json};
use tokio::sync::mpsc;

const HOUR: u64 = 3_600;
const DAY: u64 = 24 * HOUR;

fn key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("resources-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

fn read_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&read_text(path))
        .unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

/// An in-process core's dispatcher over an empty workspace.
struct Fx {
    dispatcher: Dispatcher,
    hub: Arc<WorkspaceHub>,
    clock: Arc<FakeClock>,
    events: Arc<EventBus>,
    personal: Arc<Personal>,
    ws_ref: WorkspaceRef,
    ws: PathBuf,
    _states: Arc<StateRegistry>,
    /// Last: fields drop in order, and the folder cannot go while the dispatcher holds
    /// state.db open.
    env: TestEnv,
}

impl Fx {
    fn new() -> Fx {
        let env = TestEnv::new();
        let ws = env.make_workspace("library");
        env.register_workspace(&ws, true);
        let canonical = std::fs::canonicalize(&ws).expect("canonicalize");
        let text = canonical.to_string_lossy().into_owned();
        let ws_ref = WorkspaceRef {
            id: lil_platform::Env::workspace_hash(&canonical),
            path: PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text)),
        };
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
        let probe: Arc<dyn Probe> = Arc::new(SystemProbe);
        let hub = WorkspaceHub::new(
            env.env.clone(),
            states.clone(),
            events.clone(),
            clock.clone(),
            ids.clone(),
            lil_core::store::FailPoints::none(),
            probe.clone(),
            lil_core::watch::WatchConfig::default(),
        );
        let mut reg = core_registry(
            true,
            &Arc::new(TestGates::new()),
            &states,
            &hub,
            &env.env,
            &probe,
            Arc::default(),
        );
        let personal = Arc::new(Personal::new(
            env.env.data_dir().join("resources"),
            clock.clone(),
            ids.clone(),
            probe,
        ));
        register_resource_tools(
            &mut reg,
            &Arc::new(Resources::new(personal.clone(), clock.clone())),
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
            clock: fake,
            events,
            personal,
            ws_ref,
            ws,
            _states: states,
            env,
        }
    }

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

    /// `library/resources/<kind>/<id>.json` in the workspace.
    fn workspace_file(&self, kind: &str, id: &str) -> PathBuf {
        self.ws
            .join("library")
            .join("resources")
            .join(kind)
            .join(format!("{id}.json"))
    }

    /// `<data_dir>\resources\<kind>\<id>.json`.
    fn personal_file(&self, kind: &str, id: &str) -> PathBuf {
        self.env
            .env
            .data_dir()
            .join("resources")
            .join(kind)
            .join(format!("{id}.json"))
    }

    async fn call_as(
        &self,
        caller: CallerKind,
        workspace: bool,
        name: &str,
        args: Value,
    ) -> Outcome<lil_protocol::ToolOutput> {
        let Value::Object(arguments) = args else {
            panic!("arguments must be an object");
        };
        let read = self
            .dispatcher
            .registry()
            .get(name)
            .is_some_and(|t| t.descriptor().effect == lil_protocol::EffectClass::Read);
        let session = Arc::new(Session {
            id: "s_resources".into(),
            caller,
            non_interactive: false,
            workspace: workspace.then(|| self.ws_ref.clone()),
            capabilities: Vec::new(),
            client_pid: 1,
            profile_id: None,
        });
        let params = ToolCallParams {
            name: name.into(),
            arguments,
            idempotency_key: (!read).then(key),
            trace_id: None,
        };
        self.dispatcher.call(session, params).await
    }

    /// A call by the person in an app, in the workspace.
    async fn ok(&self, name: &str, args: Value) -> Value {
        match self
            .call_as(CallerKind::Ui("studio".into()), true, name, args.clone())
            .await
        {
            Outcome::Ok(out) => out.structured,
            other => panic!("{name} {args}: {other:?}"),
        }
    }

    /// An error from a call by the person in an app, in the workspace.
    async fn err(&self, name: &str, args: Value) -> ErrorBody {
        self.err_as(CallerKind::Ui("studio".into()), true, name, args)
            .await
    }

    async fn err_as(
        &self,
        caller: CallerKind,
        workspace: bool,
        name: &str,
        args: Value,
    ) -> ErrorBody {
        match self.call_as(caller, workspace, name, args.clone()).await {
            Outcome::Error { error } => error,
            other => panic!("{name} {args} should fail: {other:?}"),
        }
    }

    async fn save(&self, scope: &str, id: &str, a: u64) -> Value {
        self.ok(
            "resource.save",
            json!({"kind": "palette", "id": id, "name": "Sunset", "data": {"a": a},
                   "scope": scope}),
        )
        .await
    }

    fn after(&self, secs: u64) {
        self.clock.advance(Duration::from_secs(secs));
    }
}

/// The trash entry `entry` of `scope`, from `trash.list`.
async fn trash_entry(fx: &Fx, scope: &str, entry: &str) -> Value {
    let list = fx.ok("trash.list", json!({"scope": scope})).await;
    list["entries"]
        .as_array()
        .and_then(|e| e.iter().find(|e| e["entry_id"] == entry))
        .cloned()
        .unwrap_or_else(|| panic!("{scope} trash.list has no {entry}: {list}"))
}

#[tokio::test] // spec: APP-078, APP-079
async fn paths_and_format_follow_appendix_f() {
    let fx = Fx::new();
    let saved = fx.save("workspace", "sunset", 1).await;
    assert_eq!(saved["path"], "palette/sunset.json", "{saved}");
    assert_eq!(saved["resource"]["schema_version"], 1, "{saved}");
    let file = fx.workspace_file("palette", "sunset");
    let on_disk = read_json(&file);
    let keys: Vec<&String> = on_disk.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        [
            "schema_version",
            "kind",
            "id",
            "name",
            "created",
            "updated",
            "data"
        ],
        "{on_disk}"
    );
    assert_eq!(on_disk["schema_version"], 1);
    assert_eq!(on_disk["data"], json!({"a": 1}));

    let saved = fx
        .ok(
            "resource.save",
            json!({"kind": "model_profile", "id": "zimage_int8", "name": "Z", "data": {}}),
        )
        .await;
    assert_eq!(saved["path"], "model_profile/zimage_int8.json", "{saved}");
    assert!(fx.personal_file("model_profile", "zimage_int8").is_file());
    assert!(!fx.workspace_file("model_profile", "zimage_int8").exists());
    fx.shutdown().await;
}

#[tokio::test] // spec: APP-078, APP-079
async fn default_scope_rules() {
    let fx = Fx::new();
    let save = |kind: &'static str| json!({"kind": kind, "id": "one", "name": "One", "data": {}});
    fx.ok("resource.save", save("palette")).await;
    fx.ok("resource.save", save("model_profile")).await;
    assert!(fx.workspace_file("palette", "one").is_file());
    assert!(fx.personal_file("model_profile", "one").is_file());
    assert!(!fx.personal_file("palette", "one").exists());

    // Without a workspace, both go to the personal scope.
    for kind in ["palette", "model_profile"] {
        let args = json!({"kind": kind, "id": "two", "name": "Two", "data": {}});
        match fx
            .call_as(CallerKind::Cli, false, "resource.save", args)
            .await
        {
            Outcome::Ok(_) => {}
            other => panic!("{kind}: {other:?}"),
        }
        assert!(fx.personal_file(kind, "two").is_file(), "{kind}");
    }
    assert!(!fx.workspace_file("palette", "two").exists());

    // An explicit workspace scope needs the workspace.
    let args = json!({"kind": "palette", "id": "three", "name": "T", "data": {},
                      "scope": "workspace"});
    let refused = fx
        .err_as(CallerKind::Cli, false, "resource.save", args)
        .await;
    assert_eq!(refused.code, "workspace.required");
    fx.shutdown().await;
}

#[tokio::test] // spec: APP-079
async fn unknown_kind_and_bad_id_are_refused() {
    let fx = Fx::new();
    assert!(RESOURCE_KINDS.contains(&"palette") && !RESOURCE_KINDS.contains(&"brush"));
    let save = |kind: &str, id: &str| json!({"kind": kind, "id": id, "name": "N", "data": 1});
    let refused = fx.err("resource.save", save("brush", "round")).await;
    assert_eq!(refused.code, "resource.kind_unknown");
    for id in ["Sunset", "1sunset", &"a".repeat(64), "", "a b", "a/b"] {
        let refused = fx.err("resource.save", save("palette", id)).await;
        assert_eq!(refused.code, "params.invalid", "{id:?}");
    }
    let longest = "a".repeat(63);
    fx.ok("resource.save", save("palette", &longest)).await;
    assert!(fx.workspace_file("palette", &longest).is_file());
    for tool in ["resource.get", "resource.delete"] {
        let refused = fx.err(tool, json!({"kind": "brush", "id": "round"})).await;
        assert_eq!(refused.code, "resource.kind_unknown", "{tool}");
    }
    let refused = fx.err("resource.list", json!({"kind": "brush"})).await;
    assert_eq!(refused.code, "resource.kind_unknown");
    fx.shutdown().await;
}

/// A save over a resource moves the old file to the scope's trash, keeps `created` and
/// refuses to restore over the new file.
async fn save_over_trashes_and_restores(scope: &str) {
    let fx = Fx::new();
    let file = |fx: &Fx| match scope {
        "workspace" => fx.workspace_file("palette", "sunset"),
        _ => fx.personal_file("palette", "sunset"),
    };
    let first = fx.save(scope, "sunset", 1).await;
    assert!(first.get("replaced_entry_id").is_none(), "{first}");
    fx.after(HOUR);
    let second = fx.save(scope, "sunset", 2).await;
    let entry = second["replaced_entry_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no replaced_entry_id: {second}"))
        .to_string();

    let listed = trash_entry(&fx, scope, &entry).await;
    assert_eq!(listed["reason"], "resource.save", "{listed}");
    assert!(
        listed["original_path"]
            .as_str()
            .unwrap()
            .ends_with("palette/sunset.json")
    );

    // The new file keeps the first `created` and has a later `updated`.
    let new = read_json(&file(&fx));
    assert_eq!(new["data"], json!({"a": 2}));
    assert_eq!(new["created"], first["resource"]["created"], "{new}");
    assert!(
        new["updated"].as_str() > first["resource"]["updated"].as_str(),
        "{new}"
    );

    let blocked = fx
        .err("trash.restore", json!({"entry_id": entry, "scope": scope}))
        .await;
    assert_eq!(blocked.code, "trash.restore_conflict", "{blocked:?}");
    assert_eq!(
        read_json(&file(&fx))["data"],
        json!({"a": 2}),
        "nothing moved"
    );

    fx.ok(
        "resource.delete",
        json!({"kind": "palette", "id": "sunset", "scope": scope}),
    )
    .await;
    fx.ok("trash.restore", json!({"entry_id": entry, "scope": scope}))
        .await;
    assert_eq!(read_json(&file(&fx))["data"], json!({"a": 1}));
    fx.shutdown().await;
}

#[tokio::test] // spec: APP-078, CORE-110
async fn save_over_an_existing_resource_trashes_the_old_one_and_restores() {
    save_over_trashes_and_restores("workspace").await;
    save_over_trashes_and_restores("personal").await;
}

#[tokio::test] // spec: APP-082, CORE-110
async fn delete_is_soft_destructive_and_restorable_in_both_scopes() {
    for scope in ["workspace", "personal"] {
        let fx = Fx::new();
        fx.save(scope, "sunset", 1).await;
        let file = match scope {
            "workspace" => fx.workspace_file("palette", "sunset"),
            _ => fx.personal_file("palette", "sunset"),
        };
        let target = json!({"kind": "palette", "id": "sunset"});

        // An agent has no way to confirm yet: nothing is deleted.
        for caller in [CallerKind::Cli, CallerKind::Assistant, CallerKind::Mcp] {
            let refused = fx
                .err_as(caller, true, "resource.delete", target.clone())
                .await;
            assert_eq!(refused.code, "confirm.unavailable");
            assert!(
                file.is_file(),
                "{scope}: an unconfirmed delete removed the file"
            );
        }

        let deleted = fx.ok("resource.delete", target.clone()).await;
        let entry = deleted["entry_id"]
            .as_str()
            .expect("an entry id")
            .to_string();
        assert!(
            !file.exists(),
            "{scope}: the deleted resource is still there"
        );
        let listed = trash_entry(&fx, scope, &entry).await;
        assert_eq!(listed["reason"], "resource.delete", "{listed}");
        let gone = fx.err("resource.get", target.clone()).await;
        assert_eq!(gone.code, "resource.unknown");

        // With no scope given, the entry is found in either trash.
        fx.ok("trash.restore", json!({"entry_id": entry})).await;
        assert_eq!(read_json(&file)["data"], json!({"a": 1}), "{scope}");
        fx.shutdown().await;
    }
}

#[tokio::test] // spec: CORE-110, CORE-037
async fn personal_trash_purges_after_30_days() {
    let fx = Fx::new();
    fx.save("personal", "sunset", 1).await;
    fx.ok(
        "resource.delete",
        json!({"kind": "palette", "id": "sunset", "scope": "personal"}),
    )
    .await;
    assert_eq!(TRASH_KEEP_DAYS, 30);
    let entries = || async { fx.personal.trash_list().await.unwrap().len() };

    fx.after(29 * DAY + 23 * HOUR);
    assert!(fx.personal.maintain().await.unwrap().is_empty());
    assert_eq!(entries().await, 1, "kept at 29 days 23 hours");

    fx.after(2 * HOUR);
    assert_eq!(fx.personal.maintain().await.unwrap().len(), 1);
    assert_eq!(entries().await, 0, "gone at 30 days 1 hour");
    let trash = fx.personal.root().join(".trash");
    let left: Vec<_> = std::fs::read_dir(&trash)
        .map(|d| d.flatten().collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "the purge left {left:?}");
    fx.shutdown().await;
}

#[tokio::test] // spec: APP-078
async fn no_workspace_session_uses_personal() {
    let fx = Fx::new();
    let args = json!({"kind": "swatch", "id": "red", "name": "Red", "data": {"hex": "#f00"}});
    match fx
        .call_as(CallerKind::Cli, false, "resource.save", args)
        .await
    {
        Outcome::Ok(out) => assert_eq!(out.structured["path"], "swatch/red.json"),
        other => panic!("{other:?}"),
    }
    assert!(fx.personal_file("swatch", "red").is_file());
    let ctx_less = |name: &'static str, args: Value| {
        let fx = &fx;
        async move {
            match fx.call_as(CallerKind::Cli, false, name, args).await {
                Outcome::Ok(out) => out.structured,
                other => panic!("{name}: {other:?}"),
            }
        }
    };
    let list = ctx_less("resource.list", json!({})).await;
    assert_eq!(list["resources"][0]["scope"], "personal", "{list}");
    assert_eq!(list["resources"].as_array().unwrap().len(), 1, "{list}");
    let got = ctx_less("resource.get", json!({"kind": "swatch", "id": "red"})).await;
    assert_eq!(got["resource"]["data"], json!({"hex": "#f00"}));
    // The trash tools work in the personal scope too.
    let trash = ctx_less("trash.list", json!({})).await;
    assert_eq!(trash["entries"], json!([]));
    // A workspace saved nothing.
    assert!(!fx.ws.join("library").exists());
    fx.shutdown().await;
}

#[tokio::test] // spec: APP-078
async fn list_and_get_see_both_scopes_and_the_workspace_wins() {
    let fx = Fx::new();
    fx.save("personal", "sunset", 1).await;
    fx.save("workspace", "sunset", 2).await;
    fx.save("workspace", "dawn", 3).await;
    let list = fx.ok("resource.list", json!({})).await;
    let rows: Vec<(String, String)> = list["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["id"].as_str().unwrap().to_string(),
                r["scope"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("dawn", "workspace"),
            ("sunset", "workspace"),
            ("sunset", "personal")
        ]
        .map(|(a, b)| (a.to_string(), b.to_string())),
        "{list}"
    );
    let only = fx.ok("resource.list", json!({"scope": "personal"})).await;
    assert_eq!(only["resources"].as_array().unwrap().len(), 1, "{only}");
    let got = fx
        .ok("resource.get", json!({"kind": "palette", "id": "sunset"}))
        .await;
    assert_eq!(
        got["resource"]["data"],
        json!({"a": 2}),
        "the workspace wins"
    );
    let got = fx
        .ok(
            "resource.get",
            json!({"kind": "palette", "id": "sunset", "scope": "personal"}),
        )
        .await;
    assert_eq!(got["resource"]["data"], json!({"a": 1}));
    fx.shutdown().await;
}

#[tokio::test] // spec: APP-078
async fn changes_publish_on_resources() {
    let fx = Fx::new();
    let (tx, mut rx) = mpsc::channel(64);
    fx.events.attach(1, tx);
    fx.events
        .subscribe(1, &["resources".to_string()], &Default::default())
        .await;
    fx.save("workspace", "sunset", 1).await;
    fx.save("workspace", "sunset", 2).await;
    let deleted = fx
        .ok(
            "resource.delete",
            json!({"kind": "palette", "id": "sunset"}),
        )
        .await;

    let mut seen = Vec::new();
    while let Ok(Some(line)) = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
        let event: Value = serde_json::from_str(&line).unwrap();
        if event["params"]["topic"] == "resources" {
            seen.push(event["params"]["data"].clone());
        }
        if seen.len() == 3 {
            break;
        }
    }
    let kinds: Vec<&Value> = seen.iter().map(|e| &e["kind"]).collect();
    assert_eq!(kinds, ["saved", "saved", "deleted"], "{seen:?}");
    assert!(seen[0].get("replaced_entry_id").is_none(), "{:?}", seen[0]);
    assert!(seen[1]["replaced_entry_id"].is_string(), "{:?}", seen[1]);
    assert_eq!(seen[1]["resource_kind"], "palette");
    assert_eq!(seen[2]["entry_id"], deleted["entry_id"]);
    fx.shutdown().await;
}
