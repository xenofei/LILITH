//! Projects (ARCH-088, ARCH-098, CORE-038, CORE-039, CORE-048, CORE-055, CORE-058): the
//! per-session active project and its `state.json` hint, `progress.md` entries, the project
//! tools, the staleness check on restore, dependents that skip archived projects, and cycles.
//!
//! Driven through the dispatcher of an in-process core, with the fake clock and ids (QA-031),
//! on a copy of the spec fixture workspace under the test's `TestEnv` root. Wall-clock
//! durations here are failure deadlines only, never measurements.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use ww_core::graph::cycles;
use ww_core::projects::{ActiveProjects, HintWrite, add_done_entry, dependents, remember_project};
use ww_core::session::Session;
use ww_core::state::StateRegistry;
use ww_core::tools::{
    Dispatcher, PipelineHooks, Registry, TestGates, core_registry, register_project_tools,
    register_workspace_tools, typed_tool,
};
use ww_core::workspace::{Access, WorkspaceHub, WorkspaceRef};
use ww_core::{CoreConfig, start_in_thread};
use ww_platform::fs::{Probe, SystemProbe};
use ww_platform::{Clock, IdSource};
use ww_protocol::{CallerKind, EffectClass, Outcome, ToolCallParams, ToolOutput};
use ww_schema::workspace_yaml::new_project_yaml;
use ww_testkit::files::write_file;
use ww_testkit::{FakeClock, RawPipe, SeqIds, TestEnv, sha256_bytes, version_content_hash_of};

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);

fn key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("projects-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// The file at the workspace-relative `rel`.
fn file(ws: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(ws.to_path_buf(), |p, c| p.join(c))
}

fn read_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Removes a file as an outside program would.
#[allow(clippy::disallowed_methods)]
fn outside_remove(path: &Path) {
    std::fs::remove_file(path).unwrap_or_else(|e| panic!("cannot remove {}: {e}", path.display()));
}

fn ws_ref(ws: &Path) -> WorkspaceRef {
    let canonical = std::fs::canonicalize(ws).expect("canonicalize");
    let text = canonical.to_string_lossy().into_owned();
    WorkspaceRef {
        id: ww_platform::Env::workspace_hash(&canonical),
        path: PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text)),
    }
}

/// A dispatcher over `env`, its hub and its state registry. `registry` builds its tools
/// (`None`: the core's own registry, test tools included).
struct Fx {
    dispatcher: Dispatcher,
    hub: Arc<WorkspaceHub>,
    ws: WorkspaceRef,
    _states: Arc<StateRegistry>,
}

type MakeRegistry = dyn Fn(&Arc<WorkspaceHub>, &TestEnv, &Arc<dyn Probe>) -> Registry;

fn fx_with(env: &TestEnv, ws: &Path, registry: Option<&MakeRegistry>) -> Fx {
    fx_full(env, ws, registry, ww_core::store::FailPoints::none())
}

fn fx_full(
    env: &TestEnv,
    ws: &Path,
    registry: Option<&MakeRegistry>,
    failpoints: Arc<ww_core::store::FailPoints>,
) -> Fx {
    let states = Arc::new(
        StateRegistry::open(
            &env.env,
            Arc::new(|what: &str| panic!("unexpected fatal report: {what}")),
        )
        .expect("the suite state.db"),
    );
    let gates = Arc::new(TestGates::new());
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
        failpoints,
        probe.clone(),
        ww_core::watch::WatchConfig::default(),
    );
    let reg = match registry {
        Some(make) => make(&hub, env, &probe),
        None => core_registry(
            true,
            &gates,
            &states,
            &hub,
            &env.env,
            &probe,
            Arc::default(),
        ),
    };
    let lifecycle = ww_core::Lifecycle::new(clock.clone(), ww_core::lifecycle::IDLE_EXIT_AFTER);
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
    Fx {
        dispatcher,
        hub,
        ws: ws_ref(ws),
        _states: states,
    }
}

/// A copy of the fixture workspace (with its `sketchbook` project), after `setup` has written
/// into it, registered as the channel's default.
fn workspace(env: &TestEnv, setup: impl FnOnce(&Path)) -> PathBuf {
    let ws = env.copy_fixture_workspace("projects");
    setup(&ws);
    env.register_workspace(&ws, true);
    ws
}

impl Fx {
    async fn call(&self, session: &str, name: &str, args: Value) -> Outcome<ToolOutput> {
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
            id: session.into(),
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

    async fn ok(&self, session: &str, name: &str, args: Value) -> Value {
        match self.call(session, name, args.clone()).await {
            Outcome::Ok(out) => out.structured,
            other => panic!("{name} {args}: {other:?}"),
        }
    }

    async fn code(&self, session: &str, name: &str, args: Value) -> String {
        match self.call(session, name, args.clone()).await {
            Outcome::Error { error } => error.code,
            other => panic!("{name} {args} should fail: {other:?}"),
        }
    }
}

#[tokio::test] // spec: ARCH-088, ARCH-098
async fn resolve_explicit_then_session_then_hint_then_sketchbook() {
    let env = TestEnv::new();
    let ws = workspace(&env, |_| {});
    let fx = fx_with(&env, &ws, None);
    let project_of = |v: &Value| v["project"].as_str().unwrap().to_string();

    // Nothing chosen and no hint: sketchbook (ARCH-098).
    assert_eq!(
        project_of(&fx.ok("s_a", "project.status", json!({})).await),
        "sketchbook"
    );
    assert!(!file(&ws, ".worldwright/state.json").exists());

    // A call that names a project makes it the session's and the hint.
    fx.ok(
        "s_a",
        "project.create",
        json!({"project": "harbor", "name": "Harbor"}),
    )
    .await;
    assert_eq!(
        project_of(&fx.ok("s_a", "project.status", json!({})).await),
        "harbor"
    );
    let hint: Value =
        serde_json::from_str(&read_text(&file(&ws, ".worldwright/state.json"))).unwrap();
    assert_eq!(hint, json!({"active_project": "harbor"}));

    // Another session has chosen nothing: the hint.
    assert_eq!(
        project_of(&fx.ok("s_b", "project.status", json!({})).await),
        "harbor"
    );
    // Its own choice wins over the hint, and stays its own.
    assert_eq!(
        project_of(
            &fx.ok("s_b", "project.status", json!({"project": "sketchbook"}))
                .await
        ),
        "sketchbook"
    );
    assert_eq!(
        project_of(&fx.ok("s_b", "project.status", json!({})).await),
        "sketchbook"
    );
    assert_eq!(
        project_of(&fx.ok("s_a", "project.status", json!({})).await),
        "harbor"
    );
    // An explicit project beats the session's.
    assert_eq!(
        project_of(
            &fx.ok("s_a", "project.status", json!({"project": "sketchbook"}))
                .await
        ),
        "sketchbook"
    );

    // A failed call doesn't change the session's project; `assets.query`'s `project` is a
    // filter and never does either.
    assert_eq!(
        fx.code("s_c", "project.status", json!({"project": "nowhere"}))
            .await,
        "project.unknown"
    );
    fx.ok("s_c", "assets.query", json!({"project": "harbor"}))
        .await;
    // The hint is the last project named: sketchbook (s_a, above).
    assert_eq!(
        project_of(&fx.ok("s_c", "project.status", json!({})).await),
        "sketchbook"
    );

    // A hint that isn't a slug, or a file that isn't JSON, is no hint.
    write_file(
        &file(&ws, ".worldwright/state.json"),
        b"{\"active_project\": \"Not A Slug\"}",
    );
    assert_eq!(
        project_of(&fx.ok("s_d", "project.status", json!({})).await),
        "sketchbook"
    );
    write_file(&file(&ws, ".worldwright/state.json"), b"not json");
    assert_eq!(
        project_of(&fx.ok("s_e", "project.status", json!({})).await),
        "sketchbook"
    );
    // The next named project is written over it.
    fx.ok("s_e", "project.status", json!({"project": "harbor"}))
        .await;
    let hint: Value =
        serde_json::from_str(&read_text(&file(&ws, ".worldwright/state.json"))).unwrap();
    assert_eq!(hint, json!({"active_project": "harbor"}));
}

#[test] // spec: ARCH-088
fn active_project_is_forgotten_when_the_connection_closes() {
    let env = TestEnv::new();
    let ws = workspace(&env, |_| {});
    let core = start_in_thread(CoreConfig {
        clock: FakeClock::new(),
        ids: Arc::new(SeqIds::new()),
        test_parent_pid: None,
        ..CoreConfig::for_env(env.env.clone()).expect("core config")
    });
    let active = core.core().active_projects().clone();

    let mut pipe = RawPipe::connect(&env);
    let hello = pipe.hello(json!({"workspace": env.workspace_id(&ws)}));
    assert_eq!(hello["result"]["status"], "ok", "{hello}");
    let session = hello["result"]["session_id"].as_str().unwrap().to_string();
    let status = pipe.tool_ok("project.status", json!({"project": "sketchbook"}), None);
    assert_eq!(status["project"], "sketchbook", "{status}");
    assert_eq!(active.get(&session).as_deref(), Some("sketchbook"));

    // Another connection's session is its own.
    let mut other = RawPipe::connect(&env);
    let hello = other.hello(json!({"workspace": env.workspace_id(&ws)}));
    let other_session = hello["result"]["session_id"].as_str().unwrap().to_string();
    assert_ne!(other_session, session);
    assert_eq!(active.get(&other_session), None);

    drop(pipe);
    let deadline = Instant::now() + DEADLINE;
    while active.get(&session).is_some() {
        assert!(
            Instant::now() < deadline,
            "the closed session's active project is still there after {DEADLINE:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // The hint stays: it is the workspace's last-used project, not the session's.
    let hint: Value =
        serde_json::from_str(&read_text(&file(&ws, ".worldwright/state.json"))).unwrap();
    assert_eq!(hint["active_project"], "sketchbook");
}

#[tokio::test] // spec: ARCH-088, ARCH-028
async fn hint_is_written_only_on_read_write_workspaces() {
    let env = TestEnv::new();
    let ws = workspace(&env, |_| {});
    let state = file(&ws, ".worldwright/state.json");

    // The first dispatcher's hub holds `core.lock`; the second serves the workspace read-only.
    let first = fx_with(&env, &ws, None);
    let rw = first.hub.open(&first.ws).await.unwrap();
    assert_eq!(rw.access, Access::ReadWrite);
    let second = fx_with(&env, &ws, None);
    let ro = second.hub.open(&second.ws).await.unwrap();
    assert_eq!(ro.access, Access::ReadOnly);

    let status = second
        .ok("s_ro", "project.status", json!({"project": "sketchbook"}))
        .await;
    assert_eq!(status["project"], "sketchbook");
    assert!(!state.exists(), "a read-only core wrote state.json");
    // The session's active project is still set there (it is in memory only).
    assert_eq!(
        second.ok("s_ro", "project.status", json!({})).await["project"],
        "sketchbook"
    );
    assert!(!state.exists());

    first
        .ok("s_rw", "project.status", json!({"project": "sketchbook"}))
        .await;
    let hint: Value = serde_json::from_str(&read_text(&state)).unwrap();
    assert_eq!(hint, json!({"active_project": "sketchbook"}));

    // The dispatcher's step itself: on the read-only workspace no write is even tried (a
    // try would come back as `workspace.read_only`), and the session's project is set.
    let active = ActiveProjects::new();
    assert_eq!(
        remember_project(&active, "s_x", &ro, "harbor").await,
        Ok(HintWrite::ReadOnly)
    );
    assert_eq!(active.get("s_x").as_deref(), Some("harbor"));
    assert_eq!(
        serde_json::from_str::<Value>(&read_text(&state)).unwrap(),
        json!({"active_project": "sketchbook"})
    );
    // On the read-write one it writes the hint, keeping keys it doesn't know.
    write_file(
        &state,
        br#"{"active_project": "sketchbook", "panel": {"open": true}, "zoom": 2}"#,
    );
    assert_eq!(
        remember_project(&active, "s_x", &rw, "harbor").await,
        Ok(HintWrite::Written)
    );
    let hint: Value = serde_json::from_str(&read_text(&state)).unwrap();
    assert_eq!(
        hint,
        json!({"active_project": "harbor", "panel": {"open": true}, "zoom": 2})
    );
    let keys: Vec<&String> = hint.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        ["active_project", "panel", "zoom"],
        "the key order is kept"
    );
}

/// The lines of `text`, each with its line ending.
fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

#[tokio::test] // spec: CORE-039
async fn progress_entry_goes_first_under_done_and_other_sections_are_untouched() {
    const CRLF: &str = "# Harbor\r\n\r\nNotes about the harbour.\r\n\r\n## Done\r\n- 2026-10-01: \
                        sketched the pier\r\n\r\n## Next\r\n- lighthouse\r\n\r\n## Open questions\r\n";
    let env = TestEnv::new();
    let ws = workspace(&env, |ws| {
        // A project folder that already has a CRLF progress.md, but no project.yaml yet.
        write_file(&file(ws, "projects/harbor/progress.md"), CRLF.as_bytes());
    });
    let fx = fx_with(&env, &ws, None);

    // LF: a new project's progress.md is the template plus the entry, directly under Done.
    fx.ok(
        "s_1",
        "project.create",
        json!({"project": "pier", "name": "Pier"}),
    )
    .await;
    let text = read_text(&file(&ws, "projects/pier/progress.md"));
    assert_eq!(
        text,
        "# Pier\n\n## Done\n- 2027-01-15: project created\n\n## Next\n\n## Open questions\n"
    );
    // A second entry goes above the first; the blank line before `## Next` stays.
    let two = add_done_entry(&text, "2027-01-16", "harbour wall").unwrap();
    let two_lines = lines(&two);
    let done = two_lines.iter().position(|l| *l == "## Done\n").unwrap();
    assert_eq!(two_lines[done + 1], "- 2027-01-16: harbour wall\n");
    assert_eq!(two_lines[done + 2], "- 2027-01-15: project created\n");
    assert_eq!(two_lines[done + 3], "\n");
    assert_eq!(two_lines[done + 4], "## Next\n");
    let mut without = two_lines.clone();
    without.remove(done + 1);
    assert_eq!(without.concat(), text, "only the one line was added");

    // CRLF: the file stays CRLF; only the entry line is new.
    fx.ok(
        "s_1",
        "project.create",
        json!({"project": "harbor", "name": "Harbor"}),
    )
    .await;
    let text = read_text(&file(&ws, "projects/harbor/progress.md"));
    let got = lines(&text);
    let done = got.iter().position(|l| *l == "## Done\r\n").unwrap();
    assert_eq!(got[done + 1], "- 2027-01-15: project created\r\n");
    assert_eq!(got[done + 2], "- 2026-10-01: sketched the pier\r\n");
    let mut without = got.clone();
    without.remove(done + 1);
    assert_eq!(without.concat(), CRLF);
    assert!(
        !text.replace("\r\n", "").contains('\n'),
        "a bare LF in {text:?}"
    );
    let two = add_done_entry(&text, "2027-01-16", "harbour wall").unwrap();
    assert!(
        two.contains("## Done\r\n- 2027-01-16: harbour wall\r\n- 2027-01-15: project created\r\n")
    );
    assert!(!two.replace("\r\n", "").contains('\n'));
}

#[tokio::test] // spec: CORE-055
async fn project_create_refuses_existing_and_bad_slugs() {
    let env = TestEnv::new();
    let ws = workspace(&env, |_| {});
    let fx = fx_with(&env, &ws, None);
    let before = read_text(&file(&ws, "projects/sketchbook/project.yaml"));

    assert_eq!(
        fx.code(
            "s_1",
            "project.create",
            json!({"project": "sketchbook", "name": "Again"})
        )
        .await,
        "project.exists"
    );
    assert_eq!(
        read_text(&file(&ws, "projects/sketchbook/project.yaml")),
        before
    );
    for bad in ["Harbor", "-harbor", "har bor", "har_bor", ""] {
        assert_eq!(
            fx.code(
                "s_1",
                "project.create",
                json!({"project": bad, "name": "X"})
            )
            .await,
            "params.invalid",
            "{bad:?}"
        );
    }
    for (name, kind) in [
        ("", "game"),
        ("Two\nlines", "game"),
        ("Harbor", "spaceship"),
    ] {
        assert_eq!(
            fx.code(
                "s_1",
                "project.create",
                json!({"project": "harbor", "name": name, "type": kind})
            )
            .await,
            "params.invalid",
            "{name:?} {kind:?}"
        );
    }
    assert!(
        !file(&ws, "projects/harbor").exists(),
        "nothing was written"
    );

    let created = fx
        .ok(
            "s_1",
            "project.create",
            json!({"project": "harbor", "name": "Harbor", "type": "game"}),
        )
        .await;
    assert_eq!(
        created,
        json!({"project": "harbor", "path": "projects/harbor"})
    );
    let yaml = read_text(&file(&ws, "projects/harbor/project.yaml"));
    assert_eq!(
        yaml,
        new_project_yaml("harbor", "Harbor", "game", "2027-01-15")
    );
    assert_eq!(
        fx.code(
            "s_1",
            "project.create",
            json!({"project": "harbor", "name": "Harbor"})
        )
        .await,
        "project.exists"
    );
    // Unknown arguments are refused.
    assert_eq!(
        fx.code(
            "s_1",
            "project.create",
            json!({"project": "x", "name": "X", "extra": 1})
        )
        .await,
        "params.invalid"
    );
}

/// An approved sketchbook manifest (made up; I5 shape) with `inputs` and `pinned_inputs`.
fn approved(asset: &str, status: &str, inputs: Value, pinned: Value) -> Vec<u8> {
    let manifest = json!({
        "schema_version": 1,
        "asset_id": asset,
        "project": "sketchbook",
        "creation_uuid": format!("00000000-0000-4000-8000-{:012}", asset.len()),
        "type": "image",
        "version": "1.0.0",
        "approved_version": "1.0.0",
        "status": status,
        "status_reason": null,
        "inputs": inputs,
        "pinned_inputs": pinned,
        "candidate_runs": [],
        "human_edits": [],
        "has_working_doc": true,
        "hashes": {},
        "created": "2026-10-01T10:00:00Z",
    });
    serde_json::to_vec_pretty(&manifest).unwrap()
}

fn input(r: &str) -> Value {
    json!([{"ref": r, "role": "reference"}])
}

fn manifest_rel(project: &str, asset: &str) -> String {
    format!("projects/{project}/assets/{asset}/{asset}.manifest.json")
}

fn read_manifest(ws: &Path, project: &str, asset: &str) -> Value {
    serde_json::from_str(&read_text(&file(ws, &manifest_rel(project, asset)))).unwrap()
}

#[tokio::test] // spec: CORE-055, CORE-048
async fn restore_marks_unpinned_dependents_stale_with_status_before_stale() {
    let env = TestEnv::new();
    let ws = workspace(&env, |ws| {
        let put = |asset: &str, bytes: Vec<u8>| {
            write_file(&file(ws, &manifest_rel("sketchbook", asset)), &bytes);
        };
        put(
            "env_arch",
            approved("env_arch", "approved", json!([]), json!([])),
        );
        // Unpinned, approved: goes stale.
        put(
            "chr_golem",
            approved("chr_golem", "approved", input("env_arch@1.0.0"), json!([])),
        );
        // Exported: goes stale too, and remembers it.
        put(
            "chr_troll",
            approved("chr_troll", "exported", input("env_arch@1.0.0"), json!([])),
        );
        // Pinned: stays.
        put(
            "chr_imp",
            approved(
                "chr_imp",
                "approved",
                input("env_arch@1.0.0"),
                json!([{"ref": "env_arch@1.0.0", "reason": "keeps the arch", "by": "user", "date": "2026-10-01"}]),
            ),
        );
        // Still in progress: can't go stale (CORE-048).
        put(
            "chr_elf",
            approved("chr_elf", "in_progress", input("env_arch@1.0.0"), json!([])),
        );
        // Already on the newer version.
        put(
            "chr_orc",
            approved("chr_orc", "approved", input("env_arch@1.1.0"), json!([])),
        );
        // A `proj:` input whose source project moved on, unpinned and pinned.
        put(
            "chr_wisp",
            approved(
                "chr_wisp",
                "approved",
                input("proj:harbor/env_tower@1.0.0"),
                json!([]),
            ),
        );
        put(
            "chr_ghost",
            approved(
                "chr_ghost",
                "released",
                input("proj:harbor/env_tower@1.0.0"),
                json!([{"ref": "proj:harbor/env_tower@1.0.0", "reason": "kept", "by": "user", "date": "2026-10-01"}]),
            ),
        );
        write_file(
            &file(ws, "projects/harbor/project.yaml"),
            new_project_yaml("harbor", "Harbor", "game", "2026-10-01").as_bytes(),
        );
        let mut tower: Value =
            serde_json::from_slice(&approved("env_tower", "approved", json!([]), json!([])))
                .unwrap();
        tower["project"] = json!("harbor");
        tower["approved_version"] = json!("1.2.0");
        write_file(
            &file(ws, &manifest_rel("harbor", "env_tower")),
            &serde_json::to_vec_pretty(&tower).unwrap(),
        );
        // chr_golem's approved version is sealed on disk, so its stale ref carries the
        // version's content hash; chr_troll's folder isn't here, so its ref can't.
        let golem_v1 = file(ws, "projects/sketchbook/assets/chr_golem/v1.0.0");
        write_file(&golem_v1.join("golem.png"), b"stone golem");
        write_file(&golem_v1.join("manifest.snapshot.json"), b"{}");
    });
    let fx = fx_with(&env, &ws, None);
    let yaml_before = read_text(&file(&ws, "projects/sketchbook/project.yaml"));

    let archived = fx
        .ok(
            "s_1",
            "project.set_status",
            json!({"project": "sketchbook", "status": "archived"}),
        )
        .await;
    assert_eq!(
        archived,
        json!({"project": "sketchbook", "status": "archived", "stale": []})
    );
    let yaml = read_text(&file(&ws, "projects/sketchbook/project.yaml"));
    assert_eq!(
        yaml,
        yaml_before.replace("status: active", "status: archived")
    );

    // While it is archived, the input gets a newer approved version.
    let mut newer: Value = read_manifest(&ws, "sketchbook", "env_arch");
    newer["approved_version"] = json!("1.1.0");
    newer["version"] = json!("1.1.0");
    write_file(
        &file(&ws, &manifest_rel("sketchbook", "env_arch")),
        &serde_json::to_vec_pretty(&newer).unwrap(),
    );
    let golem_before = read_manifest(&ws, "sketchbook", "chr_golem");

    let restored = fx
        .ok(
            "s_1",
            "project.set_status",
            json!({"project": "sketchbook", "status": "active"}),
        )
        .await;
    let ws_id = &fx.ws.id;
    let golem_hash =
        version_content_hash_of(&file(&ws, "projects/sketchbook/assets/chr_golem/v1.0.0"));
    assert_eq!(
        restored,
        json!({
            "project": "sketchbook",
            "status": "active",
            "stale": [
                format!("ww://{ws_id}/sketchbook/chr_golem@1.0.0#sha256={golem_hash}"),
                format!("ww://{ws_id}/sketchbook/chr_troll@1.0.0"),
                format!("ww://{ws_id}/sketchbook/chr_wisp@1.0.0"),
            ],
        })
    );
    assert_eq!(
        read_text(&file(&ws, "projects/sketchbook/project.yaml")),
        yaml_before
    );

    let golem = read_manifest(&ws, "sketchbook", "chr_golem");
    assert_eq!(golem["status"], "stale");
    assert_eq!(golem["status_before_stale"], "approved");
    assert_eq!(golem["status_reason"], "input env_arch 1.1.0 signed off");
    assert_eq!(
        golem["has_working_doc"], true,
        "has_working_doc survives the edit"
    );
    // Existing keys keep their order; the new one is appended.
    let keys = |m: &Value| m.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    let mut expected = keys(&golem_before);
    expected.push("status_before_stale".into());
    assert_eq!(keys(&golem), expected);
    let troll = read_manifest(&ws, "sketchbook", "chr_troll");
    assert_eq!(
        (
            troll["status"].as_str(),
            troll["status_before_stale"].as_str()
        ),
        (Some("stale"), Some("exported"))
    );
    let wisp = read_manifest(&ws, "sketchbook", "chr_wisp");
    assert_eq!(wisp["status"], "stale");
    assert_eq!(wisp["status_reason"], "input env_tower 1.2.0 signed off");
    for (asset, status) in [
        ("chr_imp", "approved"),
        ("chr_elf", "in_progress"),
        ("chr_orc", "approved"),
        ("env_arch", "approved"),
        ("chr_ghost", "released"),
    ] {
        let m = read_manifest(&ws, "sketchbook", asset);
        assert_eq!(m["status"], status, "{asset}");
        assert!(m.get("status_before_stale").is_none(), "{asset}");
    }

    // Every status but archived checks again, and finds nothing new: the stale assets stay
    // stale, nothing is marked twice.
    let again = fx
        .ok(
            "s_1",
            "project.set_status",
            json!({"project": "sketchbook", "status": "active"}),
        )
        .await;
    assert_eq!(again["stale"], json!([]));
    fx.ok(
        "s_1",
        "project.set_status",
        json!({"project": "sketchbook", "status": "paused"}),
    )
    .await;
    let resumed = fx
        .ok(
            "s_1",
            "project.set_status",
            json!({"project": "sketchbook", "status": "active"}),
        )
        .await;
    assert_eq!(resumed["stale"], json!([]));
    assert_eq!(
        read_manifest(&ws, "sketchbook", "chr_golem")["status_before_stale"],
        "approved"
    );

    assert_eq!(
        fx.code(
            "s_1",
            "project.set_status",
            json!({"project": "nowhere", "status": "active"})
        )
        .await,
        "project.unknown"
    );
    assert_eq!(
        fx.code(
            "s_1",
            "project.set_status",
            json!({"project": "sketchbook", "status": "gone"})
        )
        .await,
        "params.invalid"
    );
}

/// Fail points whose `store.replace` fails once, after `skip` more replaces, from when the
/// returned arm is set (`arm.store(skip)`); `-1` is disarmed.
fn fail_replace_once() -> (Arc<ww_core::store::FailPoints>, Arc<AtomicI64>) {
    let fp = ww_core::store::FailPoints::none();
    let arm = Arc::new(AtomicI64::new(-1));
    let hook = arm.clone();
    fp.on(
        "store.replace",
        Box::new(move || match hook.load(Ordering::SeqCst) {
            n if n < 0 => Ok(()),
            0 => {
                hook.store(-1, Ordering::SeqCst);
                Err(std::io::Error::other("fail point store.replace: test"))
            }
            n => {
                hook.store(n - 1, Ordering::SeqCst);
                Ok(())
            }
        }),
    );
    (fp, arm)
}

#[tokio::test] // spec: CORE-055, CORE-048
async fn a_restore_that_failed_midway_is_finished_by_the_retry() {
    let env = TestEnv::new();
    let ws = workspace(&env, |ws| {
        for (asset, bytes) in [
            (
                "env_arch",
                approved("env_arch", "approved", json!([]), json!([])),
            ),
            (
                "chr_golem",
                approved("chr_golem", "approved", input("env_arch@1.0.0"), json!([])),
            ),
            (
                "chr_troll",
                approved("chr_troll", "approved", input("env_arch@1.0.0"), json!([])),
            ),
        ] {
            write_file(&file(ws, &manifest_rel("sketchbook", asset)), &bytes);
        }
    });
    let (fp, arm) = fail_replace_once();
    let fx = fx_full(&env, &ws, None, fp);
    fx.ok(
        "s_1",
        "project.set_status",
        json!({"project": "sketchbook", "status": "archived"}),
    )
    .await;
    let mut newer = read_manifest(&ws, "sketchbook", "env_arch");
    newer["approved_version"] = json!("1.1.0");
    write_file(
        &file(&ws, &manifest_rel("sketchbook", "env_arch")),
        &serde_json::to_vec_pretty(&newer).unwrap(),
    );

    // The restore writes `status: active` (one replace), then fails on its first manifest edit.
    arm.store(1, Ordering::SeqCst);
    assert_eq!(
        fx.code(
            "s_1",
            "project.set_status",
            json!({"project": "sketchbook", "status": "active"})
        )
        .await,
        "store.io"
    );
    assert!(read_text(&file(&ws, "projects/sketchbook/project.yaml")).contains("status: active\n"));
    assert_eq!(
        read_manifest(&ws, "sketchbook", "chr_golem")["status"],
        "approved"
    );

    // The retry finds the status already active and still runs the check.
    let retried = fx
        .ok(
            "s_1",
            "project.set_status",
            json!({"project": "sketchbook", "status": "active"}),
        )
        .await;
    let ws_id = &fx.ws.id;
    assert_eq!(
        retried["stale"],
        json!([
            format!("ww://{ws_id}/sketchbook/chr_golem@1.0.0"),
            format!("ww://{ws_id}/sketchbook/chr_troll@1.0.0"),
        ])
    );
    for asset in ["chr_golem", "chr_troll"] {
        let m = read_manifest(&ws, "sketchbook", asset);
        assert_eq!(
            (m["status"].as_str(), m["status_before_stale"].as_str()),
            (Some("stale"), Some("approved")),
            "{asset}"
        );
    }

    // Archived to paused catches up too.
    fx.ok(
        "s_1",
        "project.set_status",
        json!({"project": "sketchbook", "status": "archived"}),
    )
    .await;
    write_file(
        &file(&ws, &manifest_rel("sketchbook", "chr_late")),
        &approved("chr_late", "approved", input("env_arch@1.0.0"), json!([])),
    );
    fx.ok(
        "s_1",
        "watch.notify",
        json!({"paths": [manifest_rel("sketchbook", "chr_late")]}),
    )
    .await;
    let paused = fx
        .ok(
            "s_1",
            "project.set_status",
            json!({"project": "sketchbook", "status": "paused"}),
        )
        .await;
    assert_eq!(
        paused["stale"],
        json!([format!("ww://{ws_id}/sketchbook/chr_late@1.0.0")])
    );
}

#[tokio::test] // spec: CORE-055, CORE-039
async fn a_create_that_stopped_before_progress_is_finished_by_the_retry() {
    let env = TestEnv::new();
    let ws = workspace(&env, |_| {});
    let (fp, arm) = fail_replace_once();
    let fx = fx_full(&env, &ws, None, fp);
    let create = json!({"project": "harbor", "name": "Harbor", "type": "game"});

    // project.yaml is written (one replace), then the progress.md write fails.
    arm.store(1, Ordering::SeqCst);
    assert_eq!(
        fx.code("s_1", "project.create", create.clone()).await,
        "store.io"
    );
    let yaml = read_text(&file(&ws, "projects/harbor/project.yaml"));
    assert_eq!(
        yaml,
        new_project_yaml("harbor", "Harbor", "game", "2027-01-15")
    );
    assert!(!file(&ws, "projects/harbor/progress.md").exists());

    // A create with other arguments doesn't take it over.
    assert_eq!(
        fx.code(
            "s_1",
            "project.create",
            json!({"project": "harbor", "name": "Other", "type": "game"})
        )
        .await,
        "project.exists"
    );
    // The same create finishes it.
    let done = fx.ok("s_1", "project.create", create.clone()).await;
    assert_eq!(
        done,
        json!({"project": "harbor", "path": "projects/harbor"})
    );
    assert_eq!(read_text(&file(&ws, "projects/harbor/project.yaml")), yaml);
    assert_eq!(
        read_text(&file(&ws, "projects/harbor/progress.md")),
        "# Harbor\n\n## Done\n- 2027-01-15: project created\n\n## Next\n\n## Open questions\n"
    );
    // Once finished, it is taken.
    assert_eq!(
        fx.code("s_1", "project.create", create).await,
        "project.exists"
    );
}

#[tokio::test] // spec: CORE-055, I6
async fn archived_projects_are_skipped_in_dependents() {
    let env = TestEnv::new();
    let ws = workspace(&env, |ws| {
        write_file(
            &file(ws, &manifest_rel("sketchbook", "env_arch")),
            &approved("env_arch", "approved", json!([]), json!([])),
        );
        write_file(
            &file(ws, &manifest_rel("sketchbook", "chr_golem")),
            &approved("chr_golem", "approved", input("env_arch@1.0.0"), json!([])),
        );
        write_file(
            &file(ws, "projects/harbor/project.yaml"),
            new_project_yaml("harbor", "Harbor", "game", "2026-10-01").as_bytes(),
        );
        let mut troll: Value = serde_json::from_slice(&approved(
            "chr_troll",
            "approved",
            input("proj:sketchbook/env_arch@1.0.0"),
            json!([]),
        ))
        .unwrap();
        troll["project"] = json!("harbor");
        write_file(
            &file(ws, &manifest_rel("harbor", "chr_troll")),
            &serde_json::to_vec_pretty(&troll).unwrap(),
        );
    });
    let fx = fx_with(&env, &ws, None);
    let open = fx.hub.open(&fx.ws).await.unwrap();
    open.index.ready().await;
    let pairs = |v: &[(&str, &str)]| -> Vec<(String, String)> {
        v.iter()
            .map(|(p, a)| (p.to_string(), a.to_string()))
            .collect()
    };

    let all = dependents(&open.store, &open.index, "sketchbook", "env_arch")
        .await
        .unwrap();
    assert_eq!(
        all,
        pairs(&[("harbor", "chr_troll"), ("sketchbook", "chr_golem")])
    );

    fx.ok(
        "s_1",
        "project.set_status",
        json!({"project": "harbor", "status": "archived"}),
    )
    .await;
    let live = dependents(&open.store, &open.index, "sketchbook", "env_arch")
        .await
        .unwrap();
    assert_eq!(live, pairs(&[("sketchbook", "chr_golem")]));
    // The index itself still knows the edge; only the scan skips it.
    assert_eq!(
        open.index
            .dependents("sketchbook", "env_arch")
            .await
            .unwrap()
            .len(),
        2
    );

    // Paused projects are not skipped.
    fx.ok(
        "s_1",
        "project.set_status",
        json!({"project": "harbor", "status": "paused"}),
    )
    .await;
    let again = dependents(&open.store, &open.index, "sketchbook", "env_arch")
        .await
        .unwrap();
    assert_eq!(again, all);
}

/// A tool of module `module` that does nothing.
fn module_tool(name: &'static str, module: &'static str) -> Arc<dyn ww_core::tools::Tool> {
    #[derive(serde::Deserialize, schemars::JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct NoArgs {}
    #[derive(serde::Serialize, schemars::JsonSchema)]
    struct Nothing {}
    typed_tool(
        name,
        "A tool of a module.",
        EffectClass::Read,
        module,
        |_ctx, _args: NoArgs| Box::pin(async move { Ok((Nothing {}, String::new())) }),
    )
}

#[tokio::test] // spec: CORE-058
async fn project_context_suggests_tools_of_enabled_modules() {
    let env = TestEnv::new();
    let ws = workspace(&env, |ws| {
        let path = file(ws, "projects/sketchbook/project.yaml");
        let mut yaml = read_text(&path);
        yaml.push_str("modules: [concept, 2d]\nlinks_note: kept\n");
        yaml = yaml.replace("links: []", "links: [lib:pal_ember, proj:harbor]");
        write_file(&path, yaml.as_bytes());
    });
    let make: &MakeRegistry = &|hub, env, probe| {
        let mut reg = Registry::new();
        register_workspace_tools(&mut reg, hub.clone(), env.env.clone(), probe.clone());
        reg.register(module_tool("x.concept_sketch", "concept"));
        reg.register(module_tool("x.sprite_slice", "2d"));
        reg.register(module_tool("x.voice_line", "voice"));
        reg.register(module_tool("x.probe", "test"));
        let tools = Arc::new(OnceLock::new());
        register_project_tools(&mut reg, tools.clone());
        tools.set(reg.descriptors()).unwrap();
        reg
    };
    let fx = fx_with(&env, &ws, Some(make));

    let context = fx
        .ok("s_1", "project.context", json!({"project": "sketchbook"}))
        .await;
    assert_eq!(context["project"], "sketchbook");
    assert_eq!(context["links"], json!(["lib:pal_ember", "proj:harbor"]));
    assert_eq!(
        context["settings"]["values"]["modules"],
        json!(["concept", "2d"])
    );
    assert_eq!(context["settings"]["sources"]["modules"], "project");
    let suggested: Vec<&str> = context["suggested_tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for want in [
        "project.context",
        "project.create",
        "project.set_status",
        "project.status",
        "workspace.create",
        "x.concept_sketch",
        "x.sprite_slice",
    ] {
        assert!(
            suggested.contains(&want),
            "{want} is not suggested: {suggested:?}"
        );
    }
    for not in ["x.voice_line", "x.probe"] {
        assert!(
            !suggested.contains(&not),
            "{not} is suggested: {suggested:?}"
        );
    }

    // Without `modules`, only the workspace module's tools are suggested.
    let path = file(&ws, "projects/sketchbook/project.yaml");
    let yaml = read_text(&path).replace("modules: [concept, 2d]\n", "");
    write_file(&path, yaml.as_bytes());
    let context = fx.ok("s_1", "project.context", json!({})).await;
    let suggested: Vec<&str> = context["suggested_tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        suggested.iter().all(|t| !t.starts_with("x.")),
        "{suggested:?}"
    );
    assert!(suggested.contains(&"project.status"));

    assert_eq!(
        fx.code("s_1", "project.context", json!({"project": "nowhere"}))
            .await,
        "project.unknown"
    );
}

#[tokio::test] // spec: CORE-038, CORE-055, ARCH-079
async fn project_status_lists_orphans_needs_repair_and_missing() {
    const SEALED: &[u8] = b"sealed stone";
    let env = TestEnv::new();
    let ws = workspace(&env, |ws| {
        let mut stone: Value =
            serde_json::from_slice(&approved("env_stone", "approved", json!([]), json!([])))
                .unwrap();
        stone["hashes"] = json!({"v1.0.0/stone.png": format!("sha256:{}", sha256_bytes(SEALED))});
        write_file(
            &file(ws, &manifest_rel("sketchbook", "env_stone")),
            &serde_json::to_vec_pretty(&stone).unwrap(),
        );
        write_file(
            &file(ws, "projects/sketchbook/assets/env_stone/v1.0.0/stone.png"),
            SEALED,
        );
        // A run folder the manifest has no run for: an orphan.
        write_file(
            &file(
                ws,
                "projects/sketchbook/assets/env_stone/candidates/r_20261001_1000/a.png",
            ),
            b"left behind",
        );
        // A manifest that doesn't parse.
        write_file(
            &file(ws, &manifest_rel("sketchbook", "chr_broken")),
            b"{not json",
        );
        write_file(
            &file(ws, &manifest_rel("sketchbook", "chr_golem")),
            &approved("chr_golem", "in_progress", json!([]), json!([])),
        );
        // Another project's orphan is not this project's.
        write_file(
            &file(ws, "projects/harbor/project.yaml"),
            new_project_yaml("harbor", "Harbor", "game", "2026-10-01").as_bytes(),
        );
        write_file(
            &file(
                ws,
                "projects/harbor/assets/chr_troll/candidates/r_20261001_1000/b.png",
            ),
            b"elsewhere",
        );
        // Conflict copies: the other project's and one outside every project are not
        // sketchbook's.
        write_file(
            &file(ws, "projects/harbor/notes.txt.ww-conflict-20261001T100000Z"),
            b"harbor's",
        );
        write_file(
            &file(ws, "notes/a.txt.ww-conflict-20261001T100000Z"),
            b"root",
        );
    });
    let fx = fx_with(&env, &ws, None);

    let status = fx
        .ok("s_1", "project.status", json!({"project": "sketchbook"}))
        .await;
    assert_eq!(status["project"], "sketchbook");
    assert_eq!(status["status"], "active");
    assert_eq!(status["assets_by_status"]["approved"], 1, "{status}");
    assert_eq!(status["assets_by_status"]["in_progress"], 1, "{status}");
    assert_eq!(
        status["orphans"],
        json!(["projects/sketchbook/assets/env_stone/candidates/r_20261001_1000"]),
        "{status}"
    );
    assert_eq!(
        status["needs_repair"],
        json!([manifest_rel("sketchbook", "chr_broken")]),
        "{status}"
    );
    assert_eq!(status["missing"], json!([]), "{status}");

    // A sealed file removed outside the suite is `missing` (and stays listed).
    let sealed = "projects/sketchbook/assets/env_stone/v1.0.0/stone.png";
    outside_remove(&file(&ws, sealed));
    fx.ok("s_1", "watch.notify", json!({"paths": [sealed]}))
        .await;
    let status = fx.ok("s_1", "project.status", json!({})).await;
    assert_eq!(status["missing"], json!([sealed]), "{status}");
    assert!(
        file(
            &ws,
            "projects/sketchbook/assets/env_stone/candidates/r_20261001_1000/a.png"
        )
        .exists(),
        "an orphan is never deleted"
    );

    // A conflict copy that appears after the workspace was opened is found at the next call,
    // anywhere under the project's folder.
    let late = "projects/sketchbook/assets/env_stone/work/scene.blend.ww-conflict-20270115T080000Z";
    write_file(&file(&ws, late), b"outside version");
    let status = fx.ok("s_1", "project.status", json!({})).await;
    assert_eq!(
        status["orphans"],
        json!([
            "projects/sketchbook/assets/env_stone/candidates/r_20261001_1000",
            late,
        ]),
        "{status}"
    );

    let harbor = fx
        .ok("s_1", "project.status", json!({"project": "harbor"}))
        .await;
    assert_eq!(
        harbor["orphans"],
        json!([
            "projects/harbor/assets/chr_troll/candidates/r_20261001_1000",
            "projects/harbor/notes.txt.ww-conflict-20261001T100000Z",
        ])
    );
    assert_eq!(harbor["assets_by_status"], json!({}));
    assert_eq!(
        fx.code("s_1", "project.status", json!({"project": "nowhere"}))
            .await,
        "project.unknown"
    );
    assert_eq!(
        fx.code("s_1", "project.status", json!({"project": "Not A Slug"}))
            .await,
        "params.invalid"
    );
}

#[test] // spec: CORE-048, I6
fn graph_reports_cycles() {
    let n = |p: &str, a: &str| (p.to_string(), a.to_string());
    let e = |from: (String, String), to: (String, String)| (from, to);
    // A chain and a diamond: no cycle.
    let acyclic = vec![
        e(n("p", "b"), n("p", "a")),
        e(n("p", "c"), n("p", "a")),
        e(n("p", "d"), n("p", "b")),
        e(n("p", "d"), n("p", "c")),
    ];
    assert!(cycles(&acyclic).is_empty());

    let mut edges = acyclic;
    // a → d closes a cycle through b and c; x ↔ y across projects; z on itself.
    edges.push(e(n("p", "a"), n("p", "d")));
    edges.push(e(n("q", "x"), n("r", "y")));
    edges.push(e(n("r", "y"), n("q", "x")));
    edges.push(e(n("s", "z"), n("s", "z")));
    edges.push(e(n("s", "w"), n("p", "a")));
    assert_eq!(
        cycles(&edges),
        vec![
            vec![n("p", "a"), n("p", "b"), n("p", "c"), n("p", "d")],
            vec![n("q", "x"), n("r", "y")],
            vec![n("s", "z")],
        ]
    );
}
