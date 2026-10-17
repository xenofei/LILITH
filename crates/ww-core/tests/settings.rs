//! The settings cascade (CORE-057, CORE-054, CORE-049; Appendix E "Settings cascade"):
//! workspace `defaults:` → `project.yaml` → the request layer, through the dispatcher's
//! settings stage and `test.settings`, on a copy of the spec fixture workspace under the
//! test's `TestEnv` root; `has_working_doc` through the store's manifest edits.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value, json};
use ww_core::session::Session;
use ww_core::settings::{SettingSource, cascade};
use ww_core::state::StateRegistry;
use ww_core::tools::{Dispatcher, PipelineHooks, TestGates, WORKSPACE_MODULE, core_registry};
use ww_core::workspace::{WorkspaceHub, WorkspaceRef};
use ww_platform::fs::{Probe, SystemProbe};
use ww_platform::{Clock, IdSource};
use ww_protocol::{CallerKind, EffectClass, Outcome, ToolCallParams, ToolOutput};
use ww_schema::workspace_yaml::{WorkspaceYaml, read_workspace_yaml};
use ww_testkit::files::write_file;
use ww_testkit::{FakeClock, SeqIds, TestEnv};

fn key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("settings-{}", NEXT.fetch_add(1, Ordering::Relaxed))
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

struct Fx {
    dispatcher: Dispatcher,
    ws: WorkspaceRef,
    _states: Arc<StateRegistry>,
}

/// A copy of the fixture workspace with `workspace_defaults` replacing the fixture's
/// `defaults:` mapping and `project_extra` appended to the sketchbook's `project.yaml`,
/// registered as the default, and a dispatcher over it with the core's registry.
fn fx(env: &TestEnv, workspace_defaults: &str, project_extra: &str) -> (Fx, PathBuf) {
    let ws = env.copy_fixture_workspace("settings");
    let yaml_path = file(&ws, "worldwright.yaml");
    let yaml = read_text(&yaml_path);
    let start = yaml.find("defaults:").expect("the fixture has defaults");
    let end = yaml.find("routing:").expect("the fixture has routing");
    let yaml = format!(
        "{}defaults:\n{workspace_defaults}{}",
        &yaml[..start],
        &yaml[end..]
    );
    write_file(&yaml_path, yaml.as_bytes());
    let project_path = file(&ws, "projects/sketchbook/project.yaml");
    let mut project = read_text(&project_path);
    project.push_str(project_extra);
    write_file(&project_path, project.as_bytes());
    env.register_workspace(&ws, true);

    let states = Arc::new(
        StateRegistry::open(
            &env.env,
            Arc::new(|what: &str| panic!("unexpected fatal report: {what}")),
        )
        .expect("the suite state.db"),
    );
    let clock: Arc<dyn Clock> = FakeClock::new();
    let events = Arc::new(ww_core::events::EventBus::new(clock.clone()));
    let ids: Arc<dyn IdSource> = Arc::new(SeqIds::new());
    let probe: Arc<dyn Probe> = Arc::new(SystemProbe);
    let hub = WorkspaceHub::new(
        env.env.clone(),
        states.clone(),
        events.clone(),
        clock.clone(),
        ids.clone(),
        ww_core::store::FailPoints::none(),
        probe.clone(),
        ww_core::watch::WatchConfig::default(),
    );
    let gates = Arc::new(TestGates::new());
    let reg = core_registry(true, &gates, &states, &hub, &env.env, &probe);
    let lifecycle = ww_core::Lifecycle::new(clock.clone(), ww_core::lifecycle::IDLE_EXIT_AFTER);
    let dispatcher = Dispatcher::new(
        reg,
        PipelineHooks::plan2(),
        states.clone(),
        events,
        hub,
        ids,
        clock,
        lifecycle,
    );
    (
        Fx {
            dispatcher,
            ws: ws_ref(&ws),
            _states: states,
        },
        ws,
    )
}

impl Fx {
    async fn call(&self, name: &str, args: Value) -> Outcome<ToolOutput> {
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
            id: "s_1".into(),
            caller: CallerKind::Cli,
            non_interactive: false,
            workspace: Some(self.ws.clone()),
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

    async fn error(&self, name: &str, args: Value) -> (String, String) {
        match self.call(name, args.clone()).await {
            Outcome::Error { error } => (error.code, error.message),
            other => panic!("{name} {args} should fail: {other:?}"),
        }
    }
}

#[tokio::test] // spec: CORE-057
async fn cascade_is_a_plain_override_chain() {
    let env = TestEnv::new();
    let (fx, _ws) = fx(
        &env,
        "  vcs: git+lfs\n  models: {image_generate: a, vision_qa: q}\n  qa_profile: lenient\n  \
         pipeline:\n    auto_approve: [icon]\n",
        "models: {image_generate: b}\npipeline:\n  skip_stages: [export]\n  extra_reviews: []\n",
    );

    let s = fx
        .ok("test.settings", json!({"project": "sketchbook"}))
        .await;
    assert_eq!(
        s,
        json!({
            "values": {
                "auto_approve": ["icon"],
                "models.image_generate": "b",
                "models.vision_qa": "q",
                "qa_profile": "lenient",
                "skip_stages": ["export"],
            },
            "sources": {
                "auto_approve": "workspace",
                "models.image_generate": "project",
                "models.vision_qa": "workspace",
                "qa_profile": "workspace",
                "skip_stages": "project",
            },
        })
    );

    // The request layer overrides both; a list replaces the whole list.
    let s = fx
        .ok(
            "test.settings",
            json!({"project": "sketchbook", "settings": {
                "models.image_generate": "c",
                "auto_approve": ["sprite", "icon_2"],
            }}),
        )
        .await;
    assert_eq!(s["values"]["models.image_generate"], "c");
    assert_eq!(s["sources"]["models.image_generate"], "request");
    assert_eq!(s["values"]["auto_approve"], json!(["sprite", "icon_2"]));
    assert_eq!(s["values"]["models.vision_qa"], "q");

    // Another project has no project layer of its own.
    fx.ok(
        "project.create",
        json!({"project": "harbor", "name": "Harbor"}),
    )
    .await;
    let s = fx.ok("test.settings", json!({"project": "harbor"})).await;
    assert_eq!(s["values"]["models.image_generate"], "a");
    assert_eq!(s["sources"]["models.image_generate"], "workspace");
    // And the session's active project is the default for the layer.
    let s = fx.ok("test.settings", json!({})).await;
    assert_eq!(s["values"]["models.image_generate"], "a");
}

#[tokio::test] // spec: CORE-057
async fn request_cannot_set_suite_level_or_unknown_keys() {
    let env = TestEnv::new();
    let (fx, _ws) = fx(&env, "  models: {}\n", "");
    for (settings, why) in [
        (json!({"caps.monthly_usd": 1}), "suite-level"),
        (json!({"caps": {"monthly_usd": 1}}), "suite-level"),
        (json!({"currency": "EUR"}), "suite-level"),
        (json!({"electricity_rate": 0.3}), "suite-level"),
        (json!({"assistant_engine": "local"}), "suite-level"),
        (json!({"vcs": "none"}), "not a setting"),
        (json!({"name": "x"}), "not a setting"),
        (json!({"qa_profile.strict": true}), "not a setting"),
        (json!({"qa_profile": 3}), "string"),
        (json!({"skip_stages": "export"}), "list"),
    ] {
        let (code, message) = fx
            .error(
                "test.settings",
                json!({"project": "sketchbook", "settings": settings}),
            )
            .await;
        assert_eq!(code, "params.invalid", "{settings}");
        assert!(message.contains(why), "{settings}: {message}");
    }
    // A valid one passes.
    let s = fx
        .ok(
            "test.settings",
            json!({"settings": {"providers.image.order": ["local"]}}),
        )
        .await;
    assert_eq!(s["values"]["providers.image.order"], json!(["local"]));
    assert_eq!(s["sources"]["providers.image.order"], "request");
}

#[tokio::test] // spec: CORE-057
async fn product_tools_resolve_settings_without_a_request_layer() {
    let env = TestEnv::new();
    let (fx, _ws) = fx(
        &env,
        "  models: {image_generate: a}\n",
        "qa_profile: strict\n",
    );

    // No product tool declares a `settings` argument, so none has a request layer.
    let product: Vec<_> = fx
        .dispatcher
        .registry()
        .descriptors()
        .into_iter()
        .filter(|d| d.module == WORKSPACE_MODULE)
        .collect();
    assert!(product.iter().any(|d| d.name == "project.context"));
    for d in &product {
        assert!(
            d.input_schema["properties"].get("settings").is_none(),
            "{} declares a settings argument",
            d.name
        );
    }
    // `test.settings` is the one tool that does (CORE-057 clarification).
    let test_settings = fx.dispatcher.registry().get("test.settings").unwrap();
    assert!(
        test_settings.descriptor().input_schema["properties"]
            .get("settings")
            .is_some()
    );

    let context = fx
        .ok("project.context", json!({"project": "sketchbook"}))
        .await;
    let sources = context["settings"]["sources"].as_object().unwrap();
    assert_eq!(sources["models.image_generate"], "workspace");
    assert_eq!(sources["qa_profile"], "project");
    assert!(sources.values().all(|s| s != "request"), "{context}");
    // A product tool refuses a `settings` argument like any unknown field.
    let (code, _) = fx
        .error(
            "project.context",
            json!({"project": "sketchbook", "settings": {"qa_profile": "x"}}),
        )
        .await;
    assert_eq!(code, "params.invalid");
}

#[tokio::test] // spec: CORE-054
async fn check_and_record_cannot_be_skipped() {
    let env = TestEnv::new();
    let (fx, _ws) = fx(
        &env,
        "  pipeline:\n    skip_stages: [record, review]\n",
        "pipeline:\n  skip_stages: [export, check, record]\n",
    );
    let s = fx
        .ok("test.settings", json!({"project": "sketchbook"}))
        .await;
    assert_eq!(s["values"]["skip_stages"], json!(["export"]));
    assert_eq!(s["sources"]["skip_stages"], "project");

    let s = fx
        .ok(
            "test.settings",
            json!({"project": "sketchbook", "settings": {"skip_stages": ["check", "report"]}}),
        )
        .await;
    assert_eq!(s["values"]["skip_stages"], json!(["report"]));
    let s = fx
        .ok(
            "test.settings",
            json!({"project": "sketchbook", "settings": {"pipeline.skip_stages": ["record"]}}),
        )
        .await;
    assert_eq!(s["values"]["skip_stages"], json!([]));

    // The pure cascade says why.
    let ws = read_workspace_yaml("defaults:\n  pipeline:\n    skip_stages: [check]\n").unwrap();
    let (settings, warnings) = cascade(&ws, None, None).unwrap();
    assert!(settings.skip_stages().is_empty());
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].message.contains("CORE-054"), "{warnings:?}");
}

#[tokio::test] // spec: CORE-049
async fn auto_candidate_on_save_defaults_off_and_has_working_doc_survives_edits() {
    // Off unless a layer turns it on.
    let (settings, _) = cascade(&WorkspaceYaml::default(), None, None).unwrap();
    assert!(!settings.auto_candidate_on_save());
    assert!(settings.get("auto_candidate_on_save").is_none());
    let on = read_workspace_yaml("defaults:\n  auto_candidate_on_save: true\n").unwrap();
    let (settings, _) = cascade(&on, None, None).unwrap();
    assert!(settings.auto_candidate_on_save());
    assert_eq!(
        settings.sources["auto_candidate_on_save"],
        SettingSource::Workspace
    );
    let mut request = Map::new();
    request.insert("auto_candidate_on_save".into(), json!(false));
    let (settings, _) = cascade(&on, None, Some(&request)).unwrap();
    assert!(!settings.auto_candidate_on_save());

    let env = TestEnv::new();
    let (fx, ws) = fx(&env, "  models: {}\n", "");
    let s = fx.ok("test.settings", json!({})).await;
    assert!(s["values"].get("auto_candidate_on_save").is_none(), "{s}");

    // An asset with a working document: a candidate run (a manifest edit through the store)
    // keeps `has_working_doc` where it was.
    let rel = "projects/sketchbook/assets/img_sea/img_sea.manifest.json";
    let manifest = json!({
        "schema_version": 1,
        "asset_id": "img_sea",
        "project": "sketchbook",
        "creation_uuid": "3d2e8f74-6b9f-4c4d-9e5a-7f8a9b0c1d23",
        "type": "image",
        "status": "in_progress",
        "has_working_doc": true,
        "inputs": [],
        "candidate_runs": [],
        "created": "2026-10-01T10:00:00Z",
    });
    write_file(
        &file(&ws, rel),
        &serde_json::to_vec_pretty(&manifest).unwrap(),
    );
    fx.ok(
        "test.ingest_run",
        json!({"asset_id": "img_sea", "files": [{"name": "a.png", "text": "a"}], "job_line": false}),
    )
    .await;
    let after: Value = serde_json::from_str(&read_text(&file(&ws, rel))).unwrap();
    assert_eq!(after["has_working_doc"], true);
    assert_eq!(after["status"], "candidate");
    let keys: Vec<&String> = after.as_object().unwrap().keys().collect();
    let before: Vec<&String> = manifest.as_object().unwrap().keys().collect();
    assert_eq!(keys, before, "no key moved or was added");
}
