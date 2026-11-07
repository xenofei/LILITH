//! Redirects and the one resolver (ARCH-125, APP-019, SP-08; Appendix H
//! "`registry/redirects.yaml`" and "`ref.resolve`"): `registry/redirects.yaml` read and
//! appended with comments kept, every ref path resolved through `projects::Resolver` (chains,
//! cycles, the 64-hop limit, trashed and missing ends, a live folder winning, the `#sha256`
//! checked at the end), the creation warning, `uses`/`used_by` across a rename, and
//! `ref.resolve {path}`. Driven through the dispatcher of an in-process core with the fake
//! clock and ids (QA-031), on a copy of the spec fixture workspace. Folders are moved as
//! Explorer would, while no core runs, and the redirect file is written by hand.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lil_core::events::EventBus;
use lil_core::interchange::{Interchange, register_interchange_tools, write_lilref};
use lil_core::projects::{
    Redirect, Resolver, append_redirect, apply_staleness, plan_staleness, read_redirects,
};
use lil_core::session::Session;
use lil_core::state::StateRegistry;
use lil_core::tools::{Dispatcher, PipelineHooks, Registry, TestGates, core_registry};
use lil_core::workspace::{OpenWorkspace, WorkspaceHub, WorkspaceRef};
use lil_platform::fs::{Probe, SystemProbe};
use lil_platform::{Clock, IdSource};
use lil_protocol::{CallerKind, EffectClass, ErrorBody, Outcome, ToolCallParams};
use lil_schema::{AssetRef, parse_lilref};
use lil_testkit::files::write_file;
use lil_testkit::{FakeClock, SeqIds, TestEnv};
use serde_json::{Value, json};

const P: &str = "sketchbook";
const CRANE: &str = "chr_paper_crane";
const LAMP: &str = "prop_signal_lamp";
const ORIGAMI: &str = "chr_origami";

fn key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("redirects-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

fn file(ws: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(ws.to_path_buf(), |p, c| p.join(c))
}

fn asset_dir(ws: &Path, asset: &str) -> PathBuf {
    file(ws, &format!("projects/{P}/assets/{asset}"))
}

fn manifest_file(ws: &Path, asset: &str) -> PathBuf {
    asset_dir(ws, asset).join(format!("{asset}.manifest.json"))
}

fn read_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

fn edit_manifest(ws: &Path, asset: &str, f: impl FnOnce(&mut Value)) {
    let path = manifest_file(ws, asset);
    let mut m = read_json(&path);
    f(&mut m);
    write_file(&path, serde_json::to_string_pretty(&m).unwrap().as_bytes());
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

/// Removes a file, or an empty folder or junction, as an outside program would.
#[allow(clippy::disallowed_methods)]
fn outside_remove(path: &Path) {
    let removed = if path.is_file() {
        std::fs::remove_file(path)
    } else {
        std::fs::remove_dir(path)
    };
    removed.unwrap_or_else(|e| panic!("cannot remove {}: {e}", path.display()));
}

/// Renames the asset folder `from` to `to` as Explorer would, with its manifest, whose
/// `asset_id` is set as an accepted move sets it.
fn move_asset(ws: &Path, from: &str, to: &str) {
    outside_rename(&asset_dir(ws, from), &asset_dir(ws, to));
    outside_rename(
        &asset_dir(ws, to).join(format!("{from}.manifest.json")),
        &manifest_file(ws, to),
    );
    edit_manifest(ws, to, |m| m["asset_id"] = json!(to));
}

/// `registry/redirects.yaml` with `entries` (`from`, `to`, `moved_utc`), names in `P`.
fn redirects_text(entries: &[(&str, &str, &str)]) -> String {
    let mut text = String::from("redirects:\n");
    for (from, to, utc) in entries {
        let name = |n: &str| {
            if n.contains('/') {
                n.to_string()
            } else {
                format!("{P}/{n}")
            }
        };
        text.push_str(&format!(
            "  - from: {}\n    to: {}\n    moved_utc: {utc}\n    by: cli\n",
            name(from),
            name(to)
        ));
    }
    text
}

fn write_redirects(ws: &Path, entries: &[(&str, &str, &str)]) {
    write_file(
        &file(ws, "registry/redirects.yaml"),
        redirects_text(entries).as_bytes(),
    );
}

const T1: &str = "2026-11-01T10:00:00Z";
const T2: &str = "2026-11-02T10:00:00Z";

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
        Fx::with_probe(Arc::new(SystemProbe), setup)
    }

    /// [`Fx::with`], the store and index seeing the disk through `probe`.
    fn with_probe(probe: Arc<dyn Probe>, setup: impl FnOnce(&Path)) -> Fx {
        let env = TestEnv::new();
        let ws = env.copy_fixture_workspace("redirects");
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

    async fn shutdown(self) {
        let Fx {
            dispatcher,
            hub,
            states,
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

    async fn open(&self) -> Arc<OpenWorkspace> {
        self.hub
            .open(&self.ws_ref)
            .await
            .expect("the workspace opens")
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
            id: "s_redirects".into(),
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

    async fn resolve(&self, r: &str) -> Value {
        self.ok("ref.resolve", json!({ "ref": r })).await
    }

    async fn resolve_err(&self, r: &str) -> ErrorBody {
        self.err("ref.resolve", json!({ "ref": r })).await
    }

    fn ws_id(&self) -> String {
        self.ws_ref.id.clone()
    }

    /// `lil://<ws>/sketchbook/<asset>@<version>`.
    fn sealed(&self, asset: &str, version: &str) -> String {
        format!("lil://{}/{P}/{asset}@{version}", self.ws_id())
    }

    /// The content hash of `asset`'s version `version`, from `asset.get`.
    async fn hash_of(&self, asset: &str, version: &str) -> String {
        let detail = self
            .ok("asset.get", json!({"project": P, "asset_id": asset}))
            .await;
        let entry = detail["versions"]
            .as_array()
            .and_then(|vs| vs.iter().find(|v| v["version"] == version))
            .unwrap_or_else(|| panic!("no version {version}: {detail}"));
        entry["ref"]
            .as_str()
            .and_then(|r| r.split_once("#sha256="))
            .map(|(_, h)| h.to_string())
            .unwrap_or_else(|| panic!("no hash: {entry}"))
    }

    async fn status_needs_repair(&self) -> Vec<String> {
        let status = self.ok("project.status", json!({"project": P})).await;
        status["needs_repair"]
            .as_array()
            .unwrap_or_else(|| panic!("{status}"))
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    }
}

/// The crane moved to `chr_origami`, with the redirect an accepted move records. Its manifest
/// `locks` go, so its folder can be moved to the trash.
fn crane_moved(ws: &Path) {
    move_asset(ws, CRANE, ORIGAMI);
    edit_manifest(ws, ORIGAMI, |m| {
        m.as_object_mut().unwrap().shift_remove("locks");
    });
    write_redirects(ws, &[(CRANE, ORIGAMI, T1)]);
}

#[tokio::test] // spec: ARCH-125, ARCH-042
async fn a_redirect_is_followed_and_named() {
    let fx = Fx::with(crane_moved);
    let hash = fx.hash_of(ORIGAMI, "1.0.0").await;
    let r = fx
        .resolve(&format!("{}#sha256={hash}", fx.sealed(CRANE, "1.0.0")))
        .await;
    assert_eq!(r["kind"], "sealed", "{r}");
    assert_eq!(r["project"], P, "{r}");
    assert_eq!(r["asset_id"], ORIGAMI, "{r}");
    assert_eq!(r["version"], "1.0.0", "{r}");
    assert_eq!(r["read_only"], true, "{r}");
    assert_eq!(r["sha256_ok"], true, "{r}");
    assert_eq!(
        r["ref"],
        format!("{}#sha256={hash}", fx.sealed(ORIGAMI, "1.0.0")),
        "the result names the new ref: {r}"
    );
    assert_eq!(r["redirected_from"], json!([format!("{P}/{CRANE}")]), "{r}");

    // A candidate ref of the old place reaches the run at the new one.
    let candidate = format!(
        "lil://{}/{P}/{CRANE}@candidate/r_20261003_1400/crane_c.png",
        fx.ws_id()
    );
    let r = fx.resolve(&candidate).await;
    assert_eq!(r["kind"], "candidate", "{r}");
    assert_eq!(r["read_only"], false, "{r}");
    assert_eq!(
        r["file"],
        format!("projects/{P}/assets/{ORIGAMI}/candidates/r_20261003_1400/crane_c.png"),
        "{r}"
    );
    assert_eq!(r["ref"], candidate.replace(CRANE, ORIGAMI), "{r}");
    assert!(r.get("sha256_ok").is_none(), "{r}");

    // The new name itself: no redirect, and no hash given means none checked.
    let r = fx.resolve(&fx.sealed(ORIGAMI, "1.0.0")).await;
    assert_eq!(r["redirected_from"], json!([]), "{r}");
    assert!(r.get("sha256_ok").is_none(), "{r}");
    assert_eq!(
        r["ref"],
        format!("{}#sha256={hash}", fx.sealed(ORIGAMI, "1.0.0")),
        "a sealed ref carries its hash (CORE-047): {r}"
    );
    fx.shutdown().await;

    // A folder renamed as Explorer renames it keeps its manifest's file name: still live.
    let fx = Fx::with(|ws| {
        outside_rename(&asset_dir(ws, LAMP), &asset_dir(ws, "prop_lamp_two"));
        write_redirects(ws, &[(LAMP, "prop_lamp_two", T1)]);
    });
    let hash = fx.hash_of("prop_lamp_two", "1.0.0").await;
    let r = fx
        .resolve(&format!("{}#sha256={hash}", fx.sealed(LAMP, "1.0.0")))
        .await;
    assert_eq!(r["asset_id"], "prop_lamp_two", "{r}");
    assert_eq!(r["sha256_ok"], true, "{r}");
    fx.shutdown().await;
}

#[tokio::test] // spec: ARCH-125
async fn chains_are_followed_to_their_end() {
    let fx = Fx::with(|ws| {
        move_asset(ws, CRANE, "chr_crane_three");
        write_file(
            &file(ws, "registry/redirects.yaml"),
            format!(
                "# Moves, oldest first.\n{}",
                redirects_text(&[
                    // The newest entry from a name wins, whatever its place in the file.
                    (CRANE, "chr_crane_two", "2026-11-02T10:00:00.5Z"),
                    (CRANE, "chr_nowhere", T1),
                    // A time that doesn't parse sorts before any that does, and the fraction
                    // of a second counts.
                    (CRANE, "chr_garbled", "yesterday"),
                    (CRANE, "chr_earlier", T2),
                    (CRANE, "chr_earlier", "2026-11-02T10:00:00.25Z"),
                    // On a tie, the later entry.
                    ("chr_crane_two", "chr_lost", T1),
                    ("chr_crane_two", "chr_crane_three", T1),
                ])
            )
            .as_bytes(),
        );
    });
    let r = fx.resolve(&fx.sealed(CRANE, "1.0.0")).await;
    assert_eq!(r["asset_id"], "chr_crane_three", "{r}");
    assert_eq!(
        r["redirected_from"],
        json!([format!("{P}/{CRANE}"), format!("{P}/chr_crane_two")]),
        "{r}"
    );
    // From the middle of the chain too.
    let r = fx.resolve(&fx.sealed("chr_crane_two", "1.0.0")).await;
    assert_eq!(r["asset_id"], "chr_crane_three", "{r}");
    fx.shutdown().await;
}

#[tokio::test] // spec: ARCH-125
async fn a_cycle_is_redirect_broken_and_listed() {
    let fx = Fx::with(|ws| {
        write_redirects(
            ws,
            &[
                ("ghost_a", "ghost_b", T1),
                ("ghost_b", "ghost_a", T1),
                // Into the loop from outside it.
                ("ghost_in", "ghost_a", T1),
                // A redirect to itself.
                ("ghost_self", "ghost_self", T1),
                // A redirect to a project that has none of these.
                ("ghost_x", "harbor/ghost_y", T1),
                ("harbor/ghost_y", "ghost_x", T1),
            ],
        );
    });
    for (name, chain) in [
        ("ghost_a", vec!["ghost_a", "ghost_b", "ghost_a"]),
        (
            "ghost_in",
            vec!["ghost_in", "ghost_a", "ghost_b", "ghost_a"],
        ),
        ("ghost_self", vec!["ghost_self", "ghost_self"]),
    ] {
        let e = fx.resolve_err(&fx.sealed(name, "1.0.0")).await;
        assert_eq!(e.code, "ref.redirect_broken", "{e:?}");
        let data = e.data.clone().unwrap();
        assert_eq!(data["reason"], "cycle", "{e:?}");
        let want: Vec<String> = chain.iter().map(|n| format!("{P}/{n}")).collect();
        assert_eq!(data["chain"], json!(want), "{e:?}");
        assert!(data.get("to").is_none(), "{e:?}");
        assert!(
            e.message
                .starts_with(&format!("The redirect for {P}/{name} leads nowhere: ")),
            "{}",
            e.message
        );
    }
    let e = fx.resolve_err(&fx.sealed("ghost_x", "1.0.0")).await;
    assert_eq!(e.data.unwrap()["chain"][1], "harbor/ghost_y");

    // The file is listed as needing repair, once per looping name of the project.
    let needs_repair = fx.status_needs_repair().await;
    let listed: Vec<&String> = needs_repair
        .iter()
        .filter(|p| p.starts_with("registry/redirects.yaml"))
        .collect();
    assert_eq!(
        listed,
        [
            "registry/redirects.yaml#sketchbook/ghost_a",
            "registry/redirects.yaml#sketchbook/ghost_b",
            "registry/redirects.yaml#sketchbook/ghost_in",
            "registry/redirects.yaml#sketchbook/ghost_self",
            "registry/redirects.yaml#sketchbook/ghost_x",
        ],
        "{needs_repair:?}"
    );
    fx.shutdown().await;
}

#[tokio::test] // spec: ARCH-125
async fn sixty_five_hops_is_broken() {
    let hop = |i: usize| format!("hop_{i:02}");
    let fx = Fx::with(|ws| {
        // hop_00 → … → hop_64: 64 redirects, the crane's folder at the end. hop_x → hop_00 is
        // one more.
        move_asset(ws, CRANE, &hop(64));
        let names: Vec<String> = (0..=64).map(hop).collect();
        let mut entries: Vec<(&str, &str, &str)> = names
            .windows(2)
            .map(|w| (w[0].as_str(), w[1].as_str(), T1))
            .collect();
        entries.push(("hop_x", "hop_00", T1));
        write_redirects(ws, &entries);
    });
    let r = fx.resolve(&fx.sealed(&hop(0), "1.0.0")).await;
    assert_eq!(r["asset_id"], hop(64), "64 redirects resolve: {r}");
    assert_eq!(r["redirected_from"].as_array().unwrap().len(), 64);

    let e = fx.resolve_err(&fx.sealed("hop_x", "1.0.0")).await;
    assert_eq!(e.code, "ref.redirect_broken", "{e:?}");
    let data = e.data.clone().unwrap();
    assert_eq!(data["reason"], "cycle", "{e:?}");
    assert_eq!(data["chain"].as_array().unwrap().len(), 65, "{e:?}");
    assert_eq!(data["chain"][0], format!("{P}/hop_x"));
    assert!(
        e.message.contains("longer than 64 redirects"),
        "{}",
        e.message
    );
    let needs_repair = fx.status_needs_repair().await;
    assert!(
        needs_repair.contains(&"registry/redirects.yaml#sketchbook/hop_x".to_string()),
        "{needs_repair:?}"
    );
    assert!(
        !needs_repair.contains(&"registry/redirects.yaml#sketchbook/hop_00".to_string()),
        "{needs_repair:?}"
    );
    fx.shutdown().await;
}

#[tokio::test] // spec: ARCH-125, CORE-157
async fn a_trashed_end_names_its_entry() {
    let fx = Fx::with(crane_moved);
    let dir = format!("projects/{P}/assets/{ORIGAMI}");
    let first = fx.ok("test.trash", json!({ "path": dir })).await;
    let r = fx.resolve_err(&fx.sealed(CRANE, "1.0.0")).await;
    assert_eq!(r.code, "asset.trashed", "{r:?}");
    assert_eq!(
        r.data.as_ref().unwrap(),
        &json!({"entry_id": first["entry_id"]})
    );
    assert_eq!(
        r.message,
        format!(
            "This asset is in the trash (entry {}). Restore it with trash.restore.",
            first["entry_id"].as_str().unwrap()
        )
    );
    // The name itself, and a candidate ref, are trashed as well.
    let e = fx.resolve_err(&fx.sealed(ORIGAMI, "1.0.0")).await;
    assert_eq!(e.code, "asset.trashed", "{e:?}");

    // A new asset at the name, trashed in its turn: the newest entry is named.
    let src = fx.env.root().join("src").join("o.png");
    write_file(&src, b"origami two");
    fx.ok(
        "asset.import",
        json!({"project": P, "asset_id": ORIGAMI, "type": "image",
               "files": [src.to_string_lossy()], "mode": "copy"}),
    )
    .await;
    let second = fx.ok("test.trash", json!({ "path": dir })).await;
    assert_ne!(first["entry_id"], second["entry_id"]);
    let e = fx.resolve_err(&fx.sealed(CRANE, "1.0.0")).await;
    assert_eq!(e.data.unwrap()["entry_id"], second["entry_id"]);

    // Restored, it resolves again.
    fx.ok("trash.restore", json!({"entry_id": second["entry_id"]}))
        .await;
    let e = fx.resolve_err(&fx.sealed(CRANE, "1.0.0")).await;
    assert_eq!(
        e.code, "asset.unknown",
        "the restored asset has no version 1.0.0: {e:?}"
    );
    let candidate = fx
        .ok("asset.get", json!({"project": P, "asset_id": ORIGAMI}))
        .await;
    let run = candidate["candidate_runs"][0]["run_id"].as_str().unwrap();
    let r = fx
        .resolve(&format!(
            "lil://{}/{P}/{CRANE}@candidate/{run}/o.png",
            fx.ws_id()
        ))
        .await;
    assert_eq!(r["asset_id"], ORIGAMI, "{r}");
    fx.shutdown().await;
}

#[tokio::test] // spec: ARCH-125
async fn a_missing_end_is_target_missing() {
    let fx = Fx::with(|ws| {
        write_redirects(
            ws,
            &[("ghost_c", "ghost_e", T1), ("ghost_e", "ghost_d", T1)],
        );
    });
    let e = fx.resolve_err(&fx.sealed("ghost_c", "1.0.0")).await;
    assert_eq!(e.code, "ref.redirect_broken", "{e:?}");
    assert_eq!(
        e.data.clone().unwrap(),
        json!({
            "chain": ["sketchbook/ghost_c", "sketchbook/ghost_e", "sketchbook/ghost_d"],
            "reason": "target_missing",
            "to": "sketchbook/ghost_d",
        })
    );
    // A dead end isn't a loop: the file needs no repair for it.
    let needs_repair = fx.status_needs_repair().await;
    assert!(
        needs_repair.iter().all(|p| !p.starts_with("registry/")),
        "{needs_repair:?}"
    );
    // A name never redirected and never there is unknown.
    let e = fx.resolve_err(&fx.sealed("ghost_z", "1.0.0")).await;
    assert_eq!(e.code, "asset.unknown", "{e:?}");
    fx.shutdown().await;
}

#[tokio::test] // spec: ARCH-125
async fn a_live_folder_wins_over_a_redirect() {
    let fx = Fx::with(|ws| {
        move_asset(ws, CRANE, ORIGAMI);
        write_redirects(
            ws,
            &[
                // The lamp is live: its own redirect is never followed.
                (LAMP, ORIGAMI, T2),
                // A live folder in the middle of a chain ends it there.
                ("chr_old", LAMP, T1),
            ],
        );
    });
    let r = fx.resolve(&fx.sealed(LAMP, "1.0.0")).await;
    assert_eq!(r["asset_id"], LAMP, "{r}");
    assert_eq!(r["redirected_from"], json!([]), "{r}");
    let r = fx.resolve(&fx.sealed("chr_old", "1.0.0")).await;
    assert_eq!(r["asset_id"], LAMP, "{r}");
    assert_eq!(r["redirected_from"], json!(["sketchbook/chr_old"]), "{r}");
    // `used_by` follows the same rule: nothing redirects to the origami through the lamp.
    let open = fx.open().await;
    let resolver = Resolver::load(&open.index, &open.store).await.unwrap();
    assert_eq!(
        resolver.names_resolving_to(P, ORIGAMI),
        [(P.to_string(), ORIGAMI.to_string())]
    );
    assert_eq!(
        resolver.names_resolving_to(P, LAMP),
        [
            (P.to_string(), "chr_old".to_string()),
            (P.to_string(), LAMP.to_string())
        ]
    );
    drop(resolver);
    drop(open);
    fx.shutdown().await;
}

#[tokio::test] // spec: ARCH-125
async fn creating_onto_a_redirect_from_warns() {
    let fx = Fx::with(|ws| {
        write_redirects(ws, &[("prop_kite", CRANE, T1)]);
    });
    let src = fx.env.root().join("src").join("kite.png");
    write_file(&src, b"kite");
    let import = |asset: &str, mode: &str| {
        json!({"project": P, "asset_id": asset, "type": "image",
               "files": [src.to_string_lossy()], "mode": mode})
    };
    let warning = "Old refs to sketchbook/prop_kite will now reach this asset \
                   (registry/redirects.yaml).";
    let r = fx.ok("asset.import", import("prop_kite", "copy")).await;
    assert_eq!(r["warnings"], json!([warning]), "{r}");
    // Only the first import creates the asset.
    let r = fx.ok("asset.import", import("prop_kite", "copy")).await;
    assert_eq!(r["warnings"], json!([]), "{r}");
    // Link-external creates it too; another name gets no warning.
    let fx2 = Fx::with(|ws| write_redirects(ws, &[("prop_kite", CRANE, T1)]));
    let src2 = fx2.env.root().join("src").join("kite.png");
    write_file(&src2, b"kite");
    let r = fx2
        .ok(
            "asset.import",
            json!({"project": P, "asset_id": "prop_kite", "type": "image",
                   "files": [src2.to_string_lossy()], "mode": "link_external"}),
        )
        .await;
    assert_eq!(r["warnings"], json!([warning]), "{r}");
    let r = fx.ok("asset.import", import("prop_other", "copy")).await;
    assert_eq!(r["warnings"], json!([]), "{r}");
    // Now the old name reaches the new asset, silently for a ref without a hash.
    let resolved = fx
        .resolve(&format!(
            "lil://{}/{P}/prop_kite@candidate/{}/kite.png",
            fx.ws_id(),
            r["run_id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(resolved["asset_id"], "prop_kite", "{resolved}");
    fx2.shutdown().await;
    fx.shutdown().await;
}

#[tokio::test] // spec: ARCH-125, CORE-047
async fn hash_is_checked_after_a_redirect() {
    let fx = Fx::with(|ws| {
        move_asset(ws, CRANE, ORIGAMI);
        write_redirects(ws, &[(CRANE, "chr_mid", T1), ("chr_mid", ORIGAMI, T1)]);
    });
    let hash = fx.hash_of(ORIGAMI, "1.0.0").await;
    let wrong = "0".repeat(64);
    let e = fx
        .resolve_err(&format!("{}#sha256={wrong}", fx.sealed(CRANE, "1.0.0")))
        .await;
    assert_eq!(e.code, "ref.hash_mismatch", "{e:?}");
    let data = e.data.unwrap();
    assert_eq!(data["expected"], wrong.as_str());
    assert_eq!(data["found"], hash.as_str());
    assert_eq!(
        data["redirected_from"],
        json!(["sketchbook/chr_paper_crane", "sketchbook/chr_mid"])
    );
    let r = fx
        .resolve(&format!("{}#sha256={hash}", fx.sealed(CRANE, "1.0.0")))
        .await;
    assert_eq!(r["sha256_ok"], true, "{r}");
    // A version the asset doesn't have is no match either.
    let e = fx
        .resolve_err(&format!("{}#sha256={hash}", fx.sealed(CRANE, "9.9.9")))
        .await;
    assert_eq!(e.code, "asset.unknown", "{e:?}");
    fx.shutdown().await;
}

#[tokio::test] // spec: ARCH-125, APP-148
async fn used_by_includes_names_that_redirect_here() {
    let fx = Fx::with(|ws| {
        crane_moved(ws);
        // The lamp still names the old place, as an input and a pin.
        edit_manifest(ws, LAMP, |m| {
            m["inputs"] = json!([{"ref": format!("{CRANE}@1.0.0"), "role": "reference"}]);
            m["pinned_inputs"] = json!([{"ref": format!("{CRANE}@1.0.0"), "reason": "kept"}]);
        });
    });
    let origami = fx
        .ok("asset.get", json!({"project": P, "asset_id": ORIGAMI}))
        .await;
    assert_eq!(
        origami["used_by"],
        json!([{"ref": "sketchbook/prop_signal_lamp", "kind": "input"}]),
        "{origami}"
    );
    fx.shutdown().await;
}

#[tokio::test] // spec: ARCH-125, APP-148
async fn uses_carry_their_resolution() {
    let fx = Fx::with(|ws| {
        crane_moved(ws);
        let mut text = std::fs::read_to_string(file(ws, "registry/redirects.yaml")).unwrap();
        text.push_str(
            &redirects_text(&[("ghost_a", "ghost_b", T1), ("ghost_b", "ghost_a", T1)])[11..],
        );
        write_file(&file(ws, "registry/redirects.yaml"), text.as_bytes());
        edit_manifest(ws, LAMP, |m| {
            m["inputs"] = json!([
                {"ref": format!("{CRANE}@1.0.0"), "role": "reference"},
                {"ref": "lib:pal_ember@1.0.0", "role": "palette"},
                {"ref": "ghost_a@1.0.0", "role": "reference"},
                {"ref": format!("{ORIGAMI}@2.0.0"), "role": "reference"},
            ]);
            // An open run's inputs as canonical refs: of this workspace, and of another one.
            let ws_id = ws_ref(ws).id;
            m["candidate_runs"] = json!([{
                "run_id": "r_20261101_1000", "path": "candidates/r_20261101_1000/",
                "status": "open", "files": [],
                "inputs": [
                    {"ref": format!("lil://{ws_id}/{P}/{CRANE}@1.0.0"), "role": "style"},
                    {"ref": format!("lil://0123456789abcdef/{P}/{CRANE}@1.0.0"), "role": "style"},
                ],
            }]);
        });
    });
    let hash = fx.hash_of(ORIGAMI, "1.0.0").await;
    let lamp = fx
        .ok("asset.get", json!({"project": P, "asset_id": LAMP}))
        .await;
    let uses = lamp["uses"].as_array().unwrap_or_else(|| panic!("{lamp}"));
    assert_eq!(uses.len(), 6, "{lamp}");
    assert_eq!(
        uses[0]["ref"],
        format!("{CRANE}@1.0.0"),
        "as the manifest writes it"
    );
    assert_eq!(
        uses[0]["resolved"],
        json!({
            "kind": "sealed",
            "ref": format!("{}#sha256={hash}", fx.sealed(ORIGAMI, "1.0.0")),
            "project": P, "asset_id": ORIGAMI, "version": "1.0.0",
            "read_only": true, "redirected_from": [format!("{P}/{CRANE}")],
        })
    );
    // A library input isn't an asset of the workspace.
    assert!(uses[1]["resolved"].is_null(), "{}", uses[1]);
    assert!(uses[1].get("error").is_none(), "{}", uses[1]);
    // A broken one says why.
    assert!(uses[2]["resolved"].is_null(), "{}", uses[2]);
    assert_eq!(
        uses[2]["error"]["code"], "ref.redirect_broken",
        "{}",
        uses[2]
    );
    // A version not sealed yet is named, without a hash, and isn't a sealed version (as for
    // `ref.resolve {path}`): the asset, not read-only.
    assert_eq!(uses[3]["resolved"]["kind"], "asset", "{}", uses[3]);
    assert_eq!(uses[3]["resolved"]["read_only"], false, "{}", uses[3]);
    assert_eq!(uses[3]["resolved"]["version"], "2.0.0", "{}", uses[3]);
    assert_eq!(
        uses[3]["resolved"]["ref"],
        fx.sealed(ORIGAMI, "2.0.0"),
        "{}",
        uses[3]
    );
    // A canonical ref resolves as ref.resolve does; another workspace's is no ref here.
    assert_eq!(uses[4]["resolved"], uses[0]["resolved"], "{}", uses[4]);
    assert!(uses[5]["resolved"].is_null(), "{}", uses[5]);
    assert_eq!(uses[5]["error"]["code"], "ref.invalid", "{}", uses[5]);
    fx.shutdown().await;
}

#[tokio::test] // spec: ARCH-125, CORE-048, CORE-019, APP-021, APP-061
async fn every_ref_path_resolves_through_redirects() {
    let fx = Fx::with(|ws| {
        crane_moved(ws);
        // harbor/old_wall now lives in this project.
        let mut text = std::fs::read_to_string(file(ws, "registry/redirects.yaml")).unwrap();
        text.push_str("  - from: harbor/old_wall\n    to: sketchbook/chr_origami\n    moved_utc: 2026-11-01T10:00:00Z\n    by: cli\n");
        write_file(&file(ws, "registry/redirects.yaml"), text.as_bytes());
        // The lamp is approved and takes the crane by its old name.
        edit_manifest(ws, LAMP, |m| {
            m["status"] = json!("approved");
            m["approved_version"] = json!("1.0.0");
            m["inputs"] = json!([{"ref": format!("{CRANE}@1.0.0"), "role": "reference"}]);
        });
    });
    let lamp_ref = |fx: &Fx| fx.sealed(LAMP, "1.0.0");

    // Run inputs (the Record stage): signing off the origami's next version makes the lamp,
    // which names the old place, stale.
    let src = fx.env.root().join("src").join("crane_d.png");
    write_file(&src, b"crane four");
    let imported = fx
        .ok(
            "asset.import",
            json!({"project": P, "asset_id": ORIGAMI, "type": "image",
                   "files": [src.to_string_lossy()], "mode": "copy"}),
        )
        .await;
    let signed = fx
        .ok(
            "asset.signoff",
            json!({"project": P, "asset_id": ORIGAMI,
                   "run_id": imported["run_id"], "files": ["crane_d.png"]}),
        )
        .await;
    assert_eq!(signed["version"], "1.1.0", "{signed}");
    let made_stale = signed["handoff"]["made_stale"].as_array().unwrap();
    assert!(
        made_stale
            .iter()
            .any(|r| r.as_str().unwrap().starts_with(&lamp_ref(&fx))),
        "{signed}"
    );

    // Staleness on restore: the lamp, put back to approved by hand while the project is
    // archived, is caught up when it is restored.
    fx.ok(
        "project.set_status",
        json!({"project": P, "status": "archived"}),
    )
    .await;
    let lamp_file = manifest_file(&fx.ws, LAMP);
    let mut lamp = read_json(&lamp_file);
    assert_eq!(lamp["status"], "stale", "{lamp}");
    lamp["status"] = json!("approved");
    write_file(
        &lamp_file,
        serde_json::to_string_pretty(&lamp).unwrap().as_bytes(),
    );
    let restored = fx
        .ok(
            "project.set_status",
            json!({"project": P, "status": "active"}),
        )
        .await;
    assert!(
        restored["stale"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r.as_str().unwrap().starts_with(&lamp_ref(&fx))),
        "{restored}"
    );

    // `uses`: the lamp's input names its new place.
    let detail = fx
        .ok("asset.get", json!({"project": P, "asset_id": LAMP}))
        .await;
    assert_eq!(
        detail["uses"][0]["resolved"]["asset_id"], ORIGAMI,
        "{detail}"
    );

    // `asset.pin_input`: harbor/old_wall is this project's now, so no `proj:harbor` link.
    let pinned = fx
        .ok(
            "asset.pin_input",
            json!({"project": P, "asset_id": LAMP, "ref": "proj:harbor/old_wall@1.0.0",
                   "reason": "kept"}),
        )
        .await;
    assert_eq!(
        pinned["pinned_inputs"][0]["ref"],
        "proj:harbor/old_wall@1.0.0"
    );
    let project_yaml =
        std::fs::read_to_string(file(&fx.ws, &format!("projects/{P}/project.yaml"))).unwrap();
    assert!(project_yaml.contains("links: []"), "{project_yaml}");

    // A `.lilref` naming the old place.
    let hash = fx.hash_of(ORIGAMI, "1.0.0").await;
    let old: AssetRef = format!("{}#sha256={hash}", fx.sealed(CRANE, "1.0.0"))
        .parse()
        .unwrap();
    let written = write_lilref(&fx.env.env, &old, Some("crane")).unwrap();
    let lilref = parse_lilref(&std::fs::read_to_string(&written).unwrap()).unwrap();
    let r = fx.resolve(&lilref.r#ref).await;
    assert_eq!(r["asset_id"], ORIGAMI, "{r}");
    assert_eq!(r["sha256_ok"], true, "{r}");

    // `handlers.for` of the old ref finds the asset's type.
    let core_dir = fx.env.root().join("core");
    write_file(
        &core_dir.join("apps").join("test.toml"),
        b"app = \"test\"\nexe = \"test.exe\"\n[[opens]]\ntypes = [\"image\"]\nintents = [\"open\"]\n",
    );
    let interchange = Interchange::new(core_dir, fx.env.env.clone());
    let mut reg = Registry::new();
    register_interchange_tools(&mut reg, &interchange);
    let mut ctx = common::ctx(&fx.env, CallerKind::Cli, &fx.events);
    ctx.workspace = Some(fx.open().await);
    let handlers = common::call(
        &reg,
        &ctx,
        "handlers.for",
        json!({"target": fx.sealed(CRANE, "1.0.0")}),
    )
    .await
    .unwrap();
    assert_eq!(handlers["handlers"][0]["app"], "test", "{handlers}");
    drop((ctx, reg, interchange));
    fx.shutdown().await;
}

#[tokio::test] // spec: ARCH-125
async fn the_trash_is_read_only_for_a_name_with_no_live_folder() {
    let fx = Fx::with(crane_moved);
    let open = fx.open().await;
    // A live name, and a redirect to a live folder: the trash is never listed.
    let resolver = Resolver::load(&open.index, &open.store).await.unwrap();
    assert_eq!(resolver.resolve_name(P, LAMP).await.unwrap().asset_id, LAMP);
    let moved = resolver.resolve_name(P, CRANE).await.unwrap();
    assert_eq!(moved.asset_id, ORIGAMI);
    assert_eq!(moved.redirected_from, ["sketchbook/chr_paper_crane"]);
    assert!(!resolver.trash_was_listed());
    // A name with no live folder: listed, once.
    let e = resolver.resolve_name(P, "chr_gone").await.unwrap_err();
    assert_eq!(e.code, "asset.unknown");
    assert!(resolver.trash_was_listed());
    drop(resolver);
    drop(open);
    fx.shutdown().await;
}

/// A junction at `link` to the folder `target` (no privilege needed).
#[allow(clippy::disallowed_methods)]
fn junction(link: &Path, target: &Path) {
    lil_platform::fs::create_junction(link, target)
        .unwrap_or_else(|e| panic!("cannot create a junction at {}: {e}", link.display()));
}

#[tokio::test] // spec: APP-019
async fn resolve_path_maps_sealed_candidate_inbox_and_outside() {
    let fx = Fx::with(|ws| {
        write_file(&file(ws, "inbox/in_0123456789abcdef/out.png"), b"inboxed");
    });
    let hash = fx.hash_of(CRANE, "1.0.0").await;
    let path = |p: &str| json!({ "path": p });
    let resolve = |p: Value| fx.ok("ref.resolve", p);
    let sealed = asset_dir(&fx.ws, CRANE).join("v1.0.0").join("crane_b.png");

    // An absolute sealed file: read-only, with its version's ref.
    let r = resolve(path(&sealed.to_string_lossy())).await;
    assert_eq!(r["kind"], "sealed", "{r}");
    assert_eq!(r["read_only"], true, "{r}");
    assert_eq!(r["asset_id"], CRANE, "{r}");
    assert_eq!(r["version"], "1.0.0", "{r}");
    assert_eq!(
        r["ref"],
        format!("{}#sha256={hash}", fx.sealed(CRANE, "1.0.0"))
    );
    assert_eq!(
        r["file"],
        format!("projects/{P}/assets/{CRANE}/v1.0.0/crane_b.png"),
        "{r}"
    );
    assert_eq!(r["redirected_from"], json!([]));
    // Workspace-relative, either separator, another case, `.` and `..`.
    for spelling in [
        format!("projects/{P}/assets/{CRANE}/v1.0.0/crane_b.png"),
        format!(r"Projects\Sketchbook\ASSETS\{CRANE}\v1.0.0\.\crane_b.png"),
        format!("projects/{P}/assets/{LAMP}/../{CRANE}/v1.0.0/crane_b.png"),
        sealed.to_string_lossy().replace('\\', "/").to_uppercase(),
    ] {
        let r = resolve(path(&spelling)).await;
        assert_eq!(r["kind"], "sealed", "{spelling}: {r}");
        assert_eq!(r["asset_id"], CRANE, "{spelling}: {r}");
    }
    // A candidate, the asset folder and its manifest, the inbox.
    let r = resolve(path(&format!(
        "projects/{P}/assets/{CRANE}/candidates/r_20261003_1400/crane_c.png"
    )))
    .await;
    assert_eq!(r["kind"], "candidate", "{r}");
    assert_eq!(r["read_only"], false, "{r}");
    assert_eq!(
        r["ref"],
        format!(
            "lil://{}/{P}/{CRANE}@candidate/r_20261003_1400/crane_c.png",
            fx.ws_id()
        )
    );
    for asset in [
        format!("projects/{P}/assets/{CRANE}"),
        format!("projects/{P}/assets/{CRANE}/{CRANE}.manifest.json"),
    ] {
        let r = resolve(path(&asset)).await;
        assert_eq!(r["kind"], "asset", "{asset}: {r}");
        assert_eq!(r["read_only"], false, "{r}");
    }
    let r = resolve(path("inbox/in_0123456789abcdef/out.png")).await;
    assert_eq!(r["kind"], "inbox", "{r}");
    assert_eq!(r["file"], "inbox/in_0123456789abcdef/out.png", "{r}");
    assert!(r.get("project").is_none(), "{r}");

    // Outside: other workspace files, other asset files, no asset there, outside the
    // workspace, climbing out with `..`, and a junction inside the workspace to outside it.
    let elsewhere = fx.env.root().join("elsewhere");
    write_file(&elsewhere.join("v1.0.0").join("x.png"), b"outside");
    junction(
        &file(&fx.ws, &format!("projects/{P}/assets/chr_linked")),
        &elsewhere,
    );
    write_file(
        &elsewhere.join("chr_linked.manifest.json"),
        br#"{"asset_id": "chr_linked", "project": "sketchbook"}"#,
    );
    for outside in [
        "registry/redirects.yaml".to_string(),
        ".trash".to_string(),
        format!("projects/{P}/assets/{CRANE}/prompts/crane.txt"),
        format!("projects/{P}/assets/chr_nobody/v1.0.0/x.png"),
        format!("projects/{P}/progress.md"),
        elsewhere.join("x.png").to_string_lossy().into_owned(),
        r"projects\..\..\elsewhere\x.png".to_string(),
        format!("projects/{P}/assets/chr_linked/v1.0.0/x.png"),
        format!("projects/{P}/assets/chr_linked/chr_linked.manifest.json"),
    ] {
        let r = resolve(path(&outside)).await;
        assert_eq!(
            r,
            json!({"kind": "outside", "read_only": false, "redirected_from": []}),
            "{outside}"
        );
    }
    // A path that climbs out of the workspace and back in is inside it.
    let r = resolve(path(&format!(
        "../ws-redirects/projects/{P}/assets/{CRANE}/v1.0.0/crane_b.png"
    )))
    .await;
    assert_eq!(r["kind"], "sealed", "{r}");
    let r = resolve(path(&format!(
        r"projects\..\..\ws-redirects\.\projects\{P}\assets\{CRANE}"
    )))
    .await;
    assert_eq!(r["kind"], "asset", "{r}");
    // A version the asset doesn't have isn't sealed (M1).
    let r = resolve(path(&format!("projects/{P}/assets/{CRANE}/v9.9.9/x.png"))).await;
    assert_eq!(r["kind"], "outside", "{r}");
    // One of `ref` and `path`.
    for bad in [
        json!({}),
        json!({"ref": "x", "path": "y"}),
        json!({"path": ""}),
    ] {
        let e = fx.err("ref.resolve", bad.clone()).await;
        assert_eq!(e.code, "params.invalid", "{bad}");
    }
    let e = fx
        .err("ref.resolve", json!({"ref": "chr_paper_crane@1.0.0"}))
        .await;
    assert_eq!(e.code, "ref.invalid", "{e:?}");
    let e = fx
        .err(
            "ref.resolve",
            json!({"ref": "lil://0123456789abcdef/sketchbook/chr_paper_crane@1.0.0"}),
        )
        .await;
    assert_eq!(e.code, "ref.invalid", "another workspace's ref: {e:?}");
    outside_remove(&file(&fx.ws, &format!("projects/{P}/assets/chr_linked")));
    fx.shutdown().await;
}

#[tokio::test] // spec: ARCH-125
async fn a_redirect_never_blocks_trash_or_removal() {
    let fx = Fx::with(crane_moved);
    // The target of a redirect goes to the trash.
    let trashed = fx
        .ok(
            "test.trash",
            json!({"path": format!("projects/{P}/assets/{ORIGAMI}")}),
        )
        .await;
    assert!(trashed["entry_id"].is_string(), "{trashed}");
    let e = fx.resolve_err(&fx.sealed(CRANE, "1.0.0")).await;
    assert_eq!(e.data.unwrap()["entry_id"], trashed["entry_id"]);
    // And the redirect file itself.
    let gone = fx
        .ok("test.trash", json!({"path": "registry/redirects.yaml"}))
        .await;
    assert!(gone["entry_id"].is_string(), "{gone}");
    let e = fx.resolve_err(&fx.sealed(CRANE, "1.0.0")).await;
    assert_eq!(e.code, "asset.unknown", "no redirect any more: {e:?}");
    fx.shutdown().await;
}

#[tokio::test] // spec: ARCH-125, ARCH-076
async fn redirects_yaml_keeps_comments_and_order() {
    let original = "# Redirects (ARCH-125). Hand-written notes stay.\nredirects:\n  # the first move\n  - from: sketchbook/chr_a   # was the crane\n    to: sketchbook/chr_b\n    moved_utc: 2026-11-01T10:00:00Z\n    by: user\n    note: kept as written\n# trailing comment\n";
    let fx = Fx::with(|ws| write_file(&file(ws, "registry/redirects.yaml"), original.as_bytes()));
    let open = fx.open().await;
    let path = file(&fx.ws, "registry/redirects.yaml");
    let redirect = |from: &str, to: &str, actor| Redirect {
        from: (P.to_string(), from.to_string()),
        to: (P.to_string(), to.to_string()),
        moved_utc: "2026-11-05T09:30:00Z".to_string(),
        by: "claude-code".to_string(),
        actor,
    };
    append_redirect(
        &open.store,
        &redirect(
            "chr_b",
            "chr_c",
            Some(lil_schema::Actor {
                kind: lil_schema::ActorKind::ClaudeCode,
                task_id: None,
                conversation_id: None,
                session_id: Some("s_1".to_string()),
            }),
        ),
    )
    .await
    .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let (kept, added) = text.split_at(original.find("# trailing").unwrap());
    assert_eq!(
        kept,
        &original[..kept.len()],
        "everything before is byte for byte"
    );
    assert_eq!(
        added,
        "  - from: sketchbook/chr_b\n    to: sketchbook/chr_c\n    moved_utc: 2026-11-05T09:30:00Z\n    by: claude-code\n    actor:\n      kind: claude_code\n      session_id: s_1\n# trailing comment\n"
    );
    let read = read_redirects(&open.store).await.unwrap();
    assert_eq!(read.len(), 2);
    assert_eq!(read[0].to.1, "chr_b");
    assert_eq!(read[1], redirect("chr_b", "chr_c", read[1].actor.clone()));
    assert_eq!(
        read[1].actor.as_ref().unwrap().session_id.as_deref(),
        Some("s_1")
    );

    // No file: it is created.
    outside_remove(&path);
    append_redirect(&open.store, &redirect("chr_x", "chr_y", None))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "redirects:\n  - from: sketchbook/chr_x\n    to: sketchbook/chr_y\n    moved_utc: 2026-11-05T09:30:00Z\n    by: claude-code\n"
    );

    // A file that isn't redirects is never read as "none", and never edited.
    let broken = "redirects:\n  - from: Sketchbook/Chr_X\n    to: nowhere\n";
    write_file(&path, broken.as_bytes());
    let e = append_redirect(&open.store, &redirect("chr_x", "chr_y", None))
        .await
        .unwrap_err();
    assert_eq!(e.to_error("").code, "yaml.unsupported_edit", "{e}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
    // The resolver keeps the file's error aside: a live name still resolves (a live folder
    // wins without the file); a name that would follow a redirect gets the error.
    let resolver = Resolver::load(&open.index, &open.store).await.unwrap();
    let e = resolver.file_error().unwrap().clone();
    assert_eq!(e.code, "manifest.needs_repair", "{e:?}");
    assert_eq!(e.data.as_ref().unwrap()["path"], "registry/redirects.yaml");
    assert!(e.message.contains("entry 1"), "{}", e.message);
    assert_eq!(resolver.resolve_name(P, LAMP).await.unwrap().asset_id, LAMP);
    assert_eq!(resolver.resolve_name(P, "chr_x").await.unwrap_err(), e);
    assert_eq!(
        resolver.names_resolving_to(P, LAMP),
        [(P.to_string(), LAMP.to_string())]
    );
    drop(resolver);
    drop(open);
    let r = fx.resolve(&fx.sealed(LAMP, "1.0.0")).await;
    assert_eq!(r["asset_id"], LAMP, "{r}");
    let e = fx.resolve_err(&fx.sealed("chr_x", "1.0.0")).await;
    assert_eq!(e.code, "manifest.needs_repair", "{e:?}");
    let needs_repair = fx.status_needs_repair().await;
    assert!(
        needs_repair.contains(&"registry/redirects.yaml".to_string()),
        "{needs_repair:?}"
    );
    fx.shutdown().await;
}

/// A `registry/redirects.yaml` that isn't redirects (entry 1's names aren't lowercase).
const BROKEN: &str = "redirects:\n  - from: Sketchbook/Chr_X\n    to: nowhere\n";

fn broken_file(ws: &Path) {
    write_file(&file(ws, "registry/redirects.yaml"), BROKEN.as_bytes());
}

#[tokio::test] // spec: ARCH-125
async fn a_broken_redirects_file_blocks_only_what_needs_it() {
    let fx = Fx::with(|ws| {
        broken_file(ws);
        edit_manifest(ws, LAMP, |m| {
            m["inputs"] = json!([
                {"ref": format!("{CRANE}@1.0.0"), "role": "reference"},
                {"ref": "chr_gone@1.0.0", "role": "reference"},
            ]);
        });
    });
    // `ref.resolve` of a live asset, and of a path (no redirect is read for a path).
    let r = fx.resolve(&fx.sealed(LAMP, "1.0.0")).await;
    assert_eq!(r["asset_id"], LAMP, "{r}");
    let r = fx
        .ok(
            "ref.resolve",
            json!({"path": format!("projects/{P}/assets/{CRANE}/v1.0.0/crane_b.png")}),
        )
        .await;
    assert_eq!(r["kind"], "sealed", "{r}");
    // Arguments are checked before the file is read.
    let e = fx.err("ref.resolve", json!({})).await;
    assert_eq!(e.code, "params.invalid", "{e:?}");
    // `asset.get` answers: a live input resolves, one that would need the file says so, and
    // `used_by` is by the asset's own name.
    let lamp = fx
        .ok("asset.get", json!({"project": P, "asset_id": LAMP}))
        .await;
    assert_eq!(lamp["uses"][0]["resolved"]["asset_id"], CRANE, "{lamp}");
    assert_eq!(
        lamp["uses"][1]["error"]["code"], "manifest.needs_repair",
        "{lamp}"
    );
    let crane = fx
        .ok("asset.get", json!({"project": P, "asset_id": CRANE}))
        .await;
    assert_eq!(
        crane["used_by"],
        json!([{"ref": "sketchbook/prop_signal_lamp", "kind": "input"}]),
        "{crane}"
    );
    // `asset.pin_input`: the name as written.
    let pinned = fx
        .ok(
            "asset.pin_input",
            json!({"project": P, "asset_id": LAMP, "ref": format!("{CRANE}@1.0.0"),
                   "reason": "kept"}),
        )
        .await;
    assert_eq!(pinned["pinned_inputs"][0]["ref"], format!("{CRANE}@1.0.0"));
    // The file is listed as needing repair.
    let needs_repair = fx.status_needs_repair().await;
    assert!(
        needs_repair.contains(&"registry/redirects.yaml".to_string()),
        "{needs_repair:?}"
    );
    fx.shutdown().await;
}

#[tokio::test] // spec: ARCH-125, CORE-048
async fn a_broken_redirects_file_fails_a_restore_only_when_an_input_needs_it() {
    let fx = Fx::with(|ws| {
        edit_manifest(ws, LAMP, |m| {
            m["status"] = json!("approved");
            m["approved_version"] = json!("1.0.0");
            m["inputs"] = json!([{"ref": format!("{CRANE}@1.0.0"), "role": "reference"}]);
        });
    });
    let yaml = file(&fx.ws, &format!("projects/{P}/project.yaml"));
    // Every input of a checked asset is live: the restore runs.
    fx.ok(
        "project.set_status",
        json!({"project": P, "status": "archived"}),
    )
    .await;
    broken_file(&fx.ws);
    let restored = fx
        .ok(
            "project.set_status",
            json!({"project": P, "status": "active"}),
        )
        .await;
    assert_eq!(restored["status"], "active", "{restored}");
    // An input whose name has no folder needs the file: the restore fails and writes nothing.
    fx.ok(
        "project.set_status",
        json!({"project": P, "status": "archived"}),
    )
    .await;
    edit_manifest(&fx.ws, LAMP, |m| {
        m["inputs"] = json!([{"ref": "chr_gone@1.0.0", "role": "reference"}]);
    });
    let before = std::fs::read(&yaml).unwrap();
    let e = fx
        .err(
            "project.set_status",
            json!({"project": P, "status": "active"}),
        )
        .await;
    assert_eq!(e.code, "yaml.unsupported_edit", "{e:?}");
    assert!(
        e.message.contains("registry/redirects.yaml"),
        "{}",
        e.message
    );
    assert_eq!(
        std::fs::read(&yaml).unwrap(),
        before,
        "project.yaml is untouched"
    );
    // Fixing the file lets it through.
    write_redirects(&fx.ws, &[]);
    let restored = fx
        .ok(
            "project.set_status",
            json!({"project": P, "status": "active"}),
        )
        .await;
    assert_eq!(restored["status"], "active", "{restored}");
    fx.shutdown().await;
}

#[tokio::test] // spec: ARCH-125
async fn a_moved_utc_of_any_scalar_sorts_first_and_never_refuses_the_file() {
    let fx = Fx::with(|ws| {
        move_asset(ws, CRANE, ORIGAMI);
        write_file(
            &file(ws, "registry/redirects.yaml"),
            format!(
                "redirects:\n  - {{from: {P}/{CRANE}, to: {P}/{ORIGAMI}, moved_utc: 2026-11-01T10:00:00Z, by: cli}}\n  - {{from: {P}/{CRANE}, to: {P}/chr_lost, moved_utc: 20261231, by: cli}}\n  - {{from: {P}/{CRANE}, to: {P}/chr_lost, moved_utc: true, by: cli}}\n  - {{from: {P}/{CRANE}, to: {P}/chr_lost, moved_utc: ~, by: cli}}\n"
            )
            .as_bytes(),
        );
    });
    let r = fx.resolve(&fx.sealed(CRANE, "1.0.0")).await;
    assert_eq!(r["asset_id"], ORIGAMI, "{r}");
    let open = fx.open().await;
    let read = read_redirects(&open.store).await.unwrap();
    assert_eq!(read[1].moved_utc, "20261231");
    drop(open);
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-019, ARCH-125
async fn pin_input_follows_redirects_only_for_its_own_workspace() {
    let fx = Fx::with(|ws| {
        crane_moved(ws);
        let mut text = std::fs::read_to_string(file(ws, "registry/redirects.yaml")).unwrap();
        text.push_str("  - from: harbor/old_wall\n    to: sketchbook/chr_origami\n    moved_utc: 2026-11-01T10:00:00Z\n    by: cli\n");
        write_file(&file(ws, "registry/redirects.yaml"), text.as_bytes());
    });
    let yaml = file(&fx.ws, &format!("projects/{P}/project.yaml"));
    // Another workspace's harbor/old_wall isn't this one's: it keeps its project as written.
    fx.ok(
        "asset.pin_input",
        json!({"project": P, "asset_id": LAMP,
               "ref": "lil://0123456789abcdef/harbor/old_wall@1.0.0", "reason": "kept"}),
    )
    .await;
    let text = std::fs::read_to_string(&yaml).unwrap();
    assert!(text.contains("proj:harbor"), "{text}");
    fx.shutdown().await;

    // This workspace's own ref follows the redirect into this project: no link.
    let fx = Fx::with(|ws| {
        crane_moved(ws);
        let mut text = std::fs::read_to_string(file(ws, "registry/redirects.yaml")).unwrap();
        text.push_str("  - from: harbor/old_wall\n    to: sketchbook/chr_origami\n    moved_utc: 2026-11-01T10:00:00Z\n    by: cli\n");
        write_file(&file(ws, "registry/redirects.yaml"), text.as_bytes());
    });
    let yaml = file(&fx.ws, &format!("projects/{P}/project.yaml"));
    fx.ok(
        "asset.pin_input",
        json!({"project": P, "asset_id": LAMP,
               "ref": format!("lil://{}/harbor/old_wall@1.0.0", fx.ws_id()), "reason": "kept"}),
    )
    .await;
    let text = std::fs::read_to_string(&yaml).unwrap();
    assert!(text.contains("links: []"), "{text}");
    fx.shutdown().await;
}

#[tokio::test] // spec: APP-019, CORE-154
async fn a_live_folder_s_manifest_under_its_old_name_is_the_asset() {
    let fx = Fx::with(|ws| {
        outside_rename(&asset_dir(ws, LAMP), &asset_dir(ws, "prop_lamp_two"));
    });
    let r = fx
        .ok(
            "ref.resolve",
            json!({"path": format!("projects/{P}/assets/prop_lamp_two/{LAMP}.manifest.json")}),
        )
        .await;
    assert_eq!(r["kind"], "asset", "{r}");
    assert_eq!(r["asset_id"], "prop_lamp_two", "{r}");
    assert_eq!(
        r["file"],
        format!("projects/{P}/assets/prop_lamp_two/{LAMP}.manifest.json")
    );
    // Another manifest-like name there isn't.
    let r = fx
        .ok(
            "ref.resolve",
            json!({"path": format!("projects/{P}/assets/prop_lamp_two/other.manifest.json")}),
        )
        .await;
    assert_eq!(r["kind"], "outside", "{r}");
    fx.shutdown().await;
}

/// A OneDrive (cloud-files) workspace: every folder under `projects` is a reparse point whose
/// tag is a cloud tag, not a name surrogate, as Files On-Demand folders are.
#[derive(Debug)]
struct CloudProbe;

fn cloudy(path: &Path) -> bool {
    path.to_string_lossy().to_lowercase().contains(r"\projects") && path.is_dir()
}

impl Probe for CloudProbe {
    fn identity(&self, path: &Path) -> std::io::Result<lil_platform::fs::FileIdentity> {
        let mut id = SystemProbe.identity(path)?;
        if cloudy(path) {
            id.attributes |= 0x400;
        }
        Ok(id)
    }
    fn volume_info(&self, path: &Path) -> std::io::Result<lil_platform::fs::VolumeInfo> {
        SystemProbe.volume_info(path)
    }
    fn open_read(&self, path: &Path) -> std::io::Result<std::fs::File> {
        SystemProbe.open_read(path)
    }
    fn reparse_tag(&self, path: &Path) -> std::io::Result<Option<u32>> {
        if cloudy(path) {
            return Ok(Some(0x9000_601A)); // IO_REPARSE_TAG_CLOUD_6
        }
        SystemProbe.reparse_tag(path)
    }
}

#[tokio::test] // spec: ARCH-125
async fn cloud_reparse_folders_are_live() {
    let fx = Fx::with_probe(Arc::new(CloudProbe), |_| {});
    let open = fx.open().await;
    let resolver = Resolver::load(&open.index, &open.store).await.unwrap();
    let r = resolver.resolve_name(P, LAMP).await;
    assert_eq!(r.map(|r| r.asset_id), Ok(LAMP.to_string()));
    drop(resolver);
    drop(open);
    fx.shutdown().await;
}

/// `setup` that makes the lamp approved (at 1.0.0) with these inputs and pins.
fn approved_lamp_with(inputs: Value, pins: Value) -> impl FnOnce(&Path) {
    move |ws: &Path| {
        edit_manifest(ws, LAMP, |m| {
            m["status"] = json!("approved");
            m["approved_version"] = json!("1.0.0");
            m["inputs"] = inputs;
            m["pinned_inputs"] = pins;
        });
    }
}

/// `registry/redirects.yaml` seen as a OneDrive cloud-only placeholder ("Free up space"):
/// the store never reads it.
#[derive(Debug)]
struct PlaceholderProbe;

fn is_redirects_file(path: &Path) -> bool {
    path.to_string_lossy()
        .to_lowercase()
        .ends_with(r"registry\redirects.yaml")
}

impl Probe for PlaceholderProbe {
    fn identity(&self, path: &Path) -> std::io::Result<lil_platform::fs::FileIdentity> {
        let mut id = SystemProbe.identity(path)?;
        if is_redirects_file(path) {
            id.attributes |= 0x0040_0000; // FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS
        }
        Ok(id)
    }
    fn volume_info(&self, path: &Path) -> std::io::Result<lil_platform::fs::VolumeInfo> {
        SystemProbe.volume_info(path)
    }
    fn open_read(&self, path: &Path) -> std::io::Result<std::fs::File> {
        SystemProbe.open_read(path)
    }
    fn reparse_tag(&self, path: &Path) -> std::io::Result<Option<u32>> {
        SystemProbe.reparse_tag(path)
    }
}

/// With a redirects file that can't be read, only what needs it fails: live assets resolve,
/// `asset.get`, `asset.pin_input`, `project.set_status` and interchange's type lookup work, and
/// a sign-off marks dependents by the asset's own name.
async fn unreadable_file_blocks_only_what_needs_it(fx: Fx) {
    let r = fx.resolve(&fx.sealed(LAMP, "1.0.0")).await;
    assert_eq!(r["asset_id"], LAMP, "{r}");
    let e = fx.resolve_err(&fx.sealed("chr_gone", "1.0.0")).await;
    assert!(e.message.contains("registry/redirects.yaml"), "{e:?}");
    let lamp = fx
        .ok("asset.get", json!({"project": P, "asset_id": LAMP}))
        .await;
    assert_eq!(lamp["uses"][0]["resolved"]["asset_id"], CRANE, "{lamp}");
    fx.ok(
        "project.set_status",
        json!({"project": P, "status": "archived"}),
    )
    .await;
    let r = fx
        .ok(
            "project.set_status",
            json!({"project": P, "status": "active"}),
        )
        .await;
    assert_eq!(r["status"], "active", "{r}");
    let needs_repair = fx.status_needs_repair().await;
    assert!(
        needs_repair.contains(&"registry/redirects.yaml".to_string()),
        "{needs_repair:?}"
    );
    // A sign-off of the crane still marks the lamp, which names it, stale.
    let src = fx.env.root().join("src").join("crane_d.png");
    write_file(&src, b"crane four");
    let imported = fx
        .ok(
            "asset.import",
            json!({"project": P, "asset_id": CRANE, "type": "image",
                   "files": [src.to_string_lossy()], "mode": "copy"}),
        )
        .await;
    let signed = fx
        .ok(
            "asset.signoff",
            json!({"project": P, "asset_id": CRANE,
                   "run_id": imported["run_id"], "files": ["crane_d.png"]}),
        )
        .await;
    let made_stale = signed["handoff"]["made_stale"].as_array().unwrap();
    assert!(
        made_stale
            .iter()
            .any(|r| r.as_str().unwrap().starts_with(&fx.sealed(LAMP, "1.0.0"))),
        "{signed}"
    );
    // A pin of the crane (after the sign-off, which a pin would hold back) by the name as
    // written.
    fx.ok(
        "asset.pin_input",
        json!({"project": P, "asset_id": LAMP, "ref": format!("{CRANE}@1.0.0"),
               "reason": "kept"}),
    )
    .await;
    // Interchange still finds a live ref's type.
    let core_dir = fx.env.root().join("core");
    write_file(
        &core_dir.join("apps").join("test.toml"),
        b"app = \"test\"\nexe = \"test.exe\"\n[[opens]]\ntypes = [\"image\"]\nintents = [\"open\"]\n",
    );
    let interchange = Interchange::new(core_dir, fx.env.env.clone());
    let mut reg = Registry::new();
    register_interchange_tools(&mut reg, &interchange);
    let mut ctx = common::ctx(&fx.env, CallerKind::Cli, &fx.events);
    ctx.workspace = Some(fx.open().await);
    let handlers = common::call(
        &reg,
        &ctx,
        "handlers.for",
        json!({"target": fx.sealed(CRANE, "1.0.0")}),
    )
    .await
    .unwrap();
    assert_eq!(handlers["handlers"][0]["app"], "test", "{handlers}");
    drop((ctx, reg, interchange));
    fx.shutdown().await;
}

#[tokio::test] // spec: ARCH-125
async fn a_redirects_file_that_is_a_folder_blocks_only_what_needs_it() {
    let fx = Fx::with(|ws| {
        approved_lamp_with(
            json!([{"ref": format!("{CRANE}@1.0.0"), "role": "reference"}]),
            json!([]),
        )(ws);
        outside_mkdir(&file(ws, "registry/redirects.yaml"));
    });
    unreadable_file_blocks_only_what_needs_it(fx).await;
}

#[tokio::test] // spec: ARCH-125
async fn a_cloud_only_redirects_file_blocks_only_what_needs_it() {
    let fx = Fx::with_probe(Arc::new(PlaceholderProbe), |ws| {
        approved_lamp_with(
            json!([{"ref": format!("{CRANE}@1.0.0"), "role": "reference"}]),
            json!([]),
        )(ws);
        write_redirects(ws, &[("chr_gone", "chr_nowhere", T1)]);
    });
    unreadable_file_blocks_only_what_needs_it(fx).await;
}

#[tokio::test] // spec: ARCH-125, CORE-048
async fn a_restore_refusal_names_the_asset_and_the_input_that_needed_the_file() {
    for (inputs, pins, named) in [
        (
            json!([{"ref": "chr_gone@1.0.0", "role": "reference"}]),
            json!([]),
            "chr_gone@1.0.0",
        ),
        (
            json!([{"ref": format!("{CRANE}@1.0.0"), "role": "reference"}]),
            json!([{"ref": "proj:harbor/old_wall@1.0.0", "reason": "kept"}]),
            "proj:harbor/old_wall@1.0.0",
        ),
    ] {
        let fx = Fx::with(approved_lamp_with(inputs, pins));
        fx.ok(
            "project.set_status",
            json!({"project": P, "status": "archived"}),
        )
        .await;
        broken_file(&fx.ws);
        let e = fx
            .err(
                "project.set_status",
                json!({"project": P, "status": "active"}),
            )
            .await;
        assert!(
            e.message.contains(LAMP) && e.message.contains(named),
            "{named}: {}",
            e.message
        );
        assert_eq!(
            e.message.matches("registry/redirects.yaml").count(),
            1,
            "the path once: {}",
            e.message
        );
        fx.shutdown().await;
    }
    // An unreadable file: the path once too.
    let fx = Fx::with(approved_lamp_with(
        json!([{"ref": "chr_gone@1.0.0", "role": "reference"}]),
        json!([]),
    ));
    fx.ok(
        "project.set_status",
        json!({"project": P, "status": "archived"}),
    )
    .await;
    outside_mkdir(&file(&fx.ws, "registry/redirects.yaml"));
    let e = fx
        .err(
            "project.set_status",
            json!({"project": P, "status": "active"}),
        )
        .await;
    assert_eq!(e.code, "store.io", "{e:?}");
    assert_eq!(
        e.message.matches("registry/redirects.yaml").count(),
        1,
        "the path once: {}",
        e.message
    );
    assert!(e.message.contains("chr_gone@1.0.0"), "{}", e.message);
    fx.shutdown().await;
}

#[tokio::test] // spec: ARCH-125, ARCH-076
async fn append_redirect_writes_only_what_reads_back() {
    let fx = Fx::with(crane_moved);
    let open = fx.open().await;
    let path = file(&fx.ws, "registry/redirects.yaml");
    let before = std::fs::read(&path).unwrap();
    let redirect = |from: &str, by: &str| Redirect {
        from: (P.to_string(), from.to_string()),
        to: (P.to_string(), ORIGAMI.to_string()),
        moved_utc: "2026-11-05T09:30:00Z".to_string(),
        by: by.to_string(),
        actor: None,
    };
    for (bad, field) in [
        (redirect("Prop Lamp", "cli"), "from"),
        (redirect("prop_lamp", ""), "by"),
    ] {
        let e = append_redirect(&open.store, &bad).await.unwrap_err();
        let body = e.to_error("");
        assert_eq!(body.code, "params.invalid", "{e}");
        assert!(body.message.contains(field), "{}", body.message);
        assert_eq!(std::fs::read(&path).unwrap(), before, "nothing written");
    }
    // The redirects still read.
    assert_eq!(read_redirects(&open.store).await.unwrap().len(), 1);
    append_redirect(&open.store, &redirect("prop_lamp", "cli"))
        .await
        .unwrap();
    assert_eq!(read_redirects(&open.store).await.unwrap().len(), 2);
    drop(open);
    fx.shutdown().await;
}

#[tokio::test] // spec: CORE-048, ARCH-125
async fn apply_staleness_leaves_an_asset_changed_since_the_plan() {
    let fx = Fx::with(|ws| {
        approved_lamp_with(
            json!([{"ref": format!("{CRANE}@1.0.0"), "role": "reference"}]),
            json!([]),
        )(ws);
        edit_manifest(ws, CRANE, |m| m["approved_version"] = json!("1.1.0"));
    });
    let open = fx.open().await;
    let plan = plan_staleness(&open, P).await.unwrap();
    // Re-signed against the new version before the plan is applied.
    edit_manifest(&fx.ws, LAMP, |m| {
        m["inputs"] = json!([{"ref": format!("{CRANE}@1.1.0"), "role": "reference"}]);
    });
    let stale = apply_staleness(&open, plan).await.unwrap();
    assert_eq!(stale, Vec::<String>::new());
    assert_eq!(
        read_json(&manifest_file(&fx.ws, LAMP))["status"],
        "approved"
    );
    // Unchanged, the same plan marks it.
    let plan = plan_staleness(&open, P).await.unwrap();
    edit_manifest(&fx.ws, LAMP, |m| {
        m["inputs"] = json!([{"ref": format!("{CRANE}@1.0.0"), "role": "reference"}]);
    });
    let replanned = plan_staleness(&open, P).await.unwrap();
    assert_eq!(apply_staleness(&open, replanned).await.unwrap().len(), 1);
    drop(plan);
    drop(open);
    fx.shutdown().await;
}

#[tokio::test] // spec: ARCH-125, CORE-020
async fn a_signoff_with_a_broken_redirects_file_marks_by_own_name_and_warns() {
    let fx = Fx::with(approved_lamp_with(
        json!([{"ref": format!("{CRANE}@1.0.0"), "role": "reference"}]),
        json!([]),
    ));
    broken_file(&fx.ws);
    let src = fx.env.root().join("src").join("crane_d.png");
    write_file(&src, b"crane four");
    let imported = fx
        .ok(
            "asset.import",
            json!({"project": P, "asset_id": CRANE, "type": "image",
                   "files": [src.to_string_lossy()], "mode": "copy"}),
        )
        .await;
    let signed = fx
        .ok(
            "asset.signoff",
            json!({"project": P, "asset_id": CRANE,
                   "run_id": imported["run_id"], "files": ["crane_d.png"]}),
        )
        .await;
    let stale = signed["handoff"]["made_stale"].as_array().unwrap();
    assert!(
        stale.iter().any(|r| r.as_str().unwrap().contains(LAMP)),
        "{signed}"
    );
    let warnings = signed["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|w| {
            let w = w.as_str().unwrap();
            w.contains("old places weren't checked") && w.contains("registry/redirects.yaml")
        }),
        "{signed}"
    );
    fx.shutdown().await;
}

/// Cloud reparse folders everywhere, and real junctions: the probe gives a real reparse tag
/// first, so a junction stays a link.
#[derive(Debug)]
struct MixedProbe;

impl Probe for MixedProbe {
    fn identity(&self, path: &Path) -> std::io::Result<lil_platform::fs::FileIdentity> {
        CloudProbe.identity(path)
    }
    fn volume_info(&self, path: &Path) -> std::io::Result<lil_platform::fs::VolumeInfo> {
        SystemProbe.volume_info(path)
    }
    fn open_read(&self, path: &Path) -> std::io::Result<std::fs::File> {
        SystemProbe.open_read(path)
    }
    fn reparse_tag(&self, path: &Path) -> std::io::Result<Option<u32>> {
        if let Some(tag) = SystemProbe.reparse_tag(path).ok().flatten() {
            return Ok(Some(tag));
        }
        CloudProbe.reparse_tag(path)
    }
}

#[tokio::test] // spec: ARCH-125
async fn a_junction_is_never_live_beside_cloud_reparse_folders() {
    let fx = Fx::with_probe(Arc::new(MixedProbe), |ws| {
        write_redirects(
            ws,
            &[("chr_old", "chr_linked", T1), ("chr_old2", "chr_alias", T1)],
        );
    });
    let elsewhere = fx.env.root().join("elsewhere");
    write_file(
        &elsewhere.join("chr_linked.manifest.json"),
        br#"{"asset_id":"chr_linked","project":"sketchbook"}"#,
    );
    junction(&asset_dir(&fx.ws, "chr_linked"), &elsewhere);
    junction(&asset_dir(&fx.ws, "chr_alias"), &asset_dir(&fx.ws, CRANE));
    let open = fx.open().await;
    let resolver = Resolver::load(&open.index, &open.store).await.unwrap();
    for live in [LAMP, CRANE] {
        let r = resolver.resolve_name(P, live).await;
        assert_eq!(r.map(|r| r.asset_id), Ok(live.to_string()));
    }
    for (name, code) in [
        ("chr_old", "ref.redirect_broken"),
        ("chr_old2", "ref.redirect_broken"),
        ("chr_linked", "asset.unknown"),
        ("chr_alias", "asset.unknown"),
    ] {
        let e = resolver.resolve_name(P, name).await.unwrap_err();
        assert_eq!(e.code, code, "{name}: {e:?}");
    }
    drop(resolver);
    drop(open);
    let r = fx
        .ok(
            "ref.resolve",
            json!({"path": format!("projects/{P}/assets/{CRANE}/v1.0.0/crane_b.png")}),
        )
        .await;
    assert_eq!(r["kind"], "sealed", "{r}");
    outside_remove(&asset_dir(&fx.ws, "chr_linked"));
    outside_remove(&asset_dir(&fx.ws, "chr_alias"));
    fx.shutdown().await;
}
