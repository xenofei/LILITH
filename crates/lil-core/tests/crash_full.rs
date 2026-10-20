//! The full crash suite (QA-055, QA-072), run nightly on `main`: every `#[ignore = "nightly"]`
//! test here is named `crash_full_*` and runs with
//! `cargo nextest run --workspace --run-ignored only -E 'test(/^crash_full_/)'`. The fast
//! subset is `crash_recovery.rs`, on every push.
//!
//! A real test-channel `lil-core` is killed (fail point action `abort`) at every plan-3 fail
//! point, for each job kind that passes it, in both `state.db` scopes (a call without a
//! workspace keeps its job in the suite `state.db`, one with a workspace in that workspace's),
//! and a fresh core must recover: nothing runs twice, no prompt is posted twice, no usage line
//! is written twice, a reservation is neither lost nor counted twice, and an answered
//! confirmation is kept. Then damaged databases: `telemetry.db` (set aside and recreated) and
//! both `state.db` scopes (the suite's refuses the core unchanged; a workspace's fails only
//! that workspace).
//!
//! Everything is under the test's `TestEnv`; ComfyUI is the in-process `FakeComfy`. Durations
//! are failure deadlines only (QA-031).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use lil_testkit::files::write_file;
use lil_testkit::sha256_file;
use lil_testkit::{CoreFixture, FakeComfy, KillOnDrop, RawPipe, TestApp, TestEnv, failpoints};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);

/// The plan-3 fail points a job passes (`store/failpoints.rs` `NAMES`).
const JOB_POINTS: [&str; 3] = [
    "jobs.after_submitting",
    "jobs.after_post",
    "jobs.after_outputs",
];

fn key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "crash-full-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// A core that stops at `point` (`abort`).
fn aborting_core(env: &TestEnv, point: &str) -> KillOnDrop {
    let spec = failpoints::spec(&[(point, "abort")]);
    CoreFixture::spawn_raw_with_env(env, &[(failpoints::ENV, &spec)])
}

/// A connection with a checked hello, naming `ws` when given.
fn session(env: &TestEnv, ws: Option<&Path>) -> RawPipe {
    let mut pipe = RawPipe::connect(env);
    let hello = match ws {
        Some(ws) => pipe.hello(json!({"workspace": env.workspace_id(ws)})),
        None => pipe.hello(json!({})),
    };
    assert_eq!(hello["result"]["status"], "ok", "{hello}");
    pipe
}

/// Sends `name args` to a core meant to abort while (or after) handling it: an answer that
/// comes first must be `ok`; then the connection closes. Returns that answer's `structured`.
fn call_into_abort(pipe: &mut RawPipe, name: &str, args: Value) -> Option<Value> {
    let request = json!({"jsonrpc": "2.0", "id": 9_100_001, "method": "tools/call",
        "params": {"name": name, "arguments": args, "idempotency_key": key()}});
    pipe.send_line(&request.to_string());
    let deadline = Instant::now() + DEADLINE;
    let mut structured = None;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(
            !left.is_zero(),
            "the core neither answered {name} nor closed"
        );
        let Some(line) = pipe.read_line(left) else {
            return structured;
        };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if message["id"] == 9_100_001 {
            assert_eq!(
                message["result"]["status"], "ok",
                "{name} failed before the fail point: {message}"
            );
            structured = message["result"].get("structured").cloned();
        }
    }
}

/// Runs `drive` on a core that aborts at `point`, checks it ended there, and returns a fresh
/// core with a connection (naming `ws` when given).
fn crash_then_restart(
    env: &TestEnv,
    ws: Option<&Path>,
    point: &str,
    drive: impl FnOnce(&mut RawPipe),
) -> (CoreFixture, RawPipe) {
    let core = aborting_core(env, point);
    {
        let mut pipe = session(env, ws);
        drive(&mut pipe);
    }
    assert_eq!(
        core.wait_exit(DEADLINE),
        Some(lil_core::store::failpoints::ABORT_EXIT_CODE),
        "the core did not end at {point}"
    );
    drop(core);
    let restarted = CoreFixture::start(env);
    let pipe = session(env, ws);
    (restarted, pipe)
}

fn all_jobs(pipe: &mut RawPipe) -> Vec<Value> {
    let list = pipe.tool_ok("jobs.list", json!({"limit": 500}), None);
    list["jobs"].as_array().cloned().unwrap_or_default()
}

/// `job.wait` until done; its `JobSummary`.
fn wait_done(pipe: &mut RawPipe, id: &str) -> Value {
    let waited = pipe.tool_ok("job.wait", json!({"job_id": id, "timeout_s": 30}), None);
    assert_eq!(waited["done"], true, "{id} is not done: {waited}");
    waited["job"].clone()
}

/// Every prompt id `fake` was posted, each must appear once.
fn assert_no_prompt_posted_twice(fake: &FakeComfy, what: &str) {
    let mut ids: Vec<String> = fake
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST" && r.path.trim_end_matches('/') == "/prompt")
        .filter_map(|r| {
            r.body
                .and_then(|b| b["prompt_id"].as_str().map(String::from))
        })
        .collect();
    let total = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), total, "{what}: a prompt id was posted twice");
}

/// `[engines.comfyui]` for an attached `fake`, with its folder as `root`.
fn attach(env: &TestEnv, fake: &FakeComfy) {
    let root = fake.output_dir();
    let root = root.parent().unwrap_or(&root);
    env.suite_toml_set(&format!(
        "[engines.comfyui]\nport = {}\nroot = '{}'\nautostart = false\n",
        fake.port(),
        root.to_str().unwrap()
    ));
}

/// A copy of the fixture workspace registered as the default.
fn default_workspace(env: &TestEnv, name: &str) -> PathBuf {
    let ws = env.copy_fixture_workspace(name);
    env.register_workspace(&ws, true);
    ws
}

/// One engine job kind.
#[derive(Clone, Copy, Debug)]
enum Kind {
    /// `test.job` on the `comfyui` engine, with or without external side effects.
    TestJob { external: bool },
    /// `comfy.prompt` (`test.comfy_submit`, the tiny graph) on the fake ComfyUI, with or
    /// without external side effects.
    Prompt { external: bool },
}

impl Kind {
    fn submit(self, pipe: &mut RawPipe) {
        match self {
            Kind::TestJob { external } => call_into_abort(
                pipe,
                "test.job",
                json!({"lane": "gpu-heavy", "engine": "comfyui",
                       "external_side_effects": external}),
            ),
            Kind::Prompt { external } => call_into_abort(
                pipe,
                "test.comfy_submit",
                json!({"graph": "tiny", "external_side_effects": external}),
            ),
        };
    }

    fn name(self) -> &'static str {
        match self {
            Kind::TestJob { .. } => "test.job",
            Kind::Prompt { .. } => "comfy.prompt",
        }
    }
}

/// Every job fail point × each engine job kind × both `state.db` scopes. The next core finds
/// the one job of the run that died and never makes a new job of it, never posts a prompt
/// twice, and ends it (ARCH-022, CORE-120, CORE-128, QA-007, ruling P3-R21):
/// - without external side effects: `interrupted`, never run again;
/// - with them, while still `submitting` (before or just after the post): `interrupted_unknown`,
///   for only the person can tell whether the engine has it;
/// - a prompt with them that ComfyUI finished (the core died while staging its outputs): taken
///   up by its prompt id and its outputs ingested, so it succeeds, in the suite's cache folder
///   (where the dead run's copy stays beside the new one) as in a workspace's staging.
#[test]
#[ignore = "nightly"] // spec: QA-055, QA-007, ARCH-022, CORE-120, CORE-128
fn crash_full_every_job_fail_point_for_each_job_kind_in_both_scopes() {
    let kinds = [
        Kind::TestJob { external: false },
        Kind::TestJob { external: true },
        Kind::Prompt { external: false },
        Kind::Prompt { external: true },
    ];
    for scope in ["suite", "workspace"] {
        for point in JOB_POINTS {
            for kind in kinds {
                let what = format!("{kind:?} at {point} ({scope} scope)");
                let env = TestEnv::new();
                let ws = (scope == "workspace").then(|| default_workspace(&env, "crash-full"));
                let fake = FakeComfy::start();
                attach(&env, &fake);
                // A test core without fake hardware has no GPU source, so no job waits for
                // VRAM.
                let (_core, mut pipe) =
                    crash_then_restart(&env, ws.as_deref(), point, |p| kind.submit(p));
                let of_kind = |pipe: &mut RawPipe| -> Vec<Value> {
                    all_jobs(pipe)
                        .into_iter()
                        .filter(|j| j["kind"] == kind.name())
                        .collect()
                };
                let jobs = of_kind(&mut pipe);
                assert_eq!(jobs.len(), 1, "{what}: one job, nothing new: {jobs:?}");
                let job = wait_done(&mut pipe, jobs[0]["job_id"].as_str().unwrap());
                let state = (job["status"].as_str(), job["substatus"].as_str());
                let outputs = point == "jobs.after_outputs";
                match kind {
                    Kind::Prompt { external: true } if outputs => {
                        assert_eq!(state, (Some("succeeded"), None), "{what}: {job}");
                        let artifacts = job["result"]["artifacts"].as_array();
                        assert_eq!(artifacts.map(Vec::len), Some(1), "{what}: {job}");
                    }
                    // A `test.job` has no engine id to look for; with no post in flight it
                    // ends plain `interrupted` or `interrupted_unknown` (reported to the
                    // owner as a P3-R21 reading).
                    Kind::TestJob { external: true } if outputs => {
                        assert!(
                            matches!(state.1, Some("interrupted" | "interrupted_unknown")),
                            "{what}: {job}"
                        );
                    }
                    Kind::TestJob { external: true } | Kind::Prompt { external: true } => {
                        assert_eq!(
                            state,
                            (Some("running"), Some("interrupted_unknown")),
                            "{what}: {job}"
                        );
                    }
                    _ => {
                        assert_eq!(
                            state,
                            (Some("failed"), Some("interrupted")),
                            "{what}: {job}"
                        );
                    }
                }
                if !matches!(kind, Kind::Prompt { external: true } if outputs) {
                    assert!(
                        job.get("result").is_none(),
                        "{what}: it never finished: {job}"
                    );
                }
                assert_eq!(job["attempt"], 1, "{what}: {job}");
                assert_eq!(of_kind(&mut pipe).len(), 1, "{what}: nothing was re-run");
                assert_no_prompt_posted_twice(&fake, &what);
            }
        }
    }
}

/// `[money]` in USD and a `monthly` cap of 10.
fn caps_10(env: &TestEnv) {
    env.suite_toml_set("[money]\ndisplay_currency = \"USD\"\n[caps]\nmonthly = \"10\"\n");
}

/// The `monthly` row of `caps.get`: `(settled, held)`.
fn monthly(pipe: &mut RawPipe) -> (String, String) {
    let view = pipe.tool_ok("caps.get", json!({}), None);
    let row = view["caps"]
        .as_array()
        .and_then(|caps| caps.iter().find(|c| c["cap"] == "monthly"))
        .cloned()
        .unwrap_or_else(|| panic!("no monthly cap: {view}"));
    (
        row["settled"].as_str().unwrap_or_default().to_string(),
        row["held"].as_str().unwrap_or_default().to_string(),
    )
}

/// The `test.meter_external` lines of `usage-loose.jsonl`.
fn external_lines(env: &TestEnv) -> Vec<Value> {
    std::fs::read_to_string(env.env.local_dir().join("usage-loose.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|l| l["tool"] == "test.meter_external")
        .collect()
}

/// Polls until `f` holds.
fn until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !f() {
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(POLL);
    }
}

/// The meter's and the caps' fail points for an external call of 0.5 USD under a monthly cap
/// of 10: after the write-ahead row (`meter.after_pending`) the next core writes one `pending`
/// line and keeps the reservation; after the usage line (`meter.after_usage_line`) it writes
/// no second line and settles the reservation once; after the reservation alone
/// (`caps.after_reserve`, nothing sent) no line is written and the repair releases the
/// reservation. Another restart changes nothing (CORE-071, CORE-088, QA-047).
#[test]
#[ignore = "nightly"] // spec: QA-055, CORE-071, CORE-088, QA-047
fn crash_full_metered_calls_at_every_meter_and_caps_fail_point() {
    for point in [
        "meter.after_pending",
        "meter.after_usage_line",
        "caps.after_reserve",
    ] {
        let env = TestEnv::new();
        caps_10(&env);
        let (core, mut pipe) = crash_then_restart(&env, None, point, |p| {
            call_into_abort(
                p,
                "test.meter_external",
                json!({"provider": "fakeprov", "amount": "0.5"}),
            );
        });
        let check = |pipe: &mut RawPipe, env: &TestEnv| match point {
            "meter.after_pending" => {
                until("the pending line", || external_lines(env).len() == 1);
                assert_eq!(external_lines(env)[0]["status"], "pending", "{point}");
                assert_eq!(monthly(pipe).1, "0.5", "{point}: the reservation is kept");
            }
            "meter.after_usage_line" => {
                until("the settle", || monthly(pipe) == ("0.5".into(), "0".into()));
                let lines = external_lines(env);
                assert_eq!(lines.len(), 1, "{point}: {lines:?}");
                assert_eq!(lines[0]["status"], "succeeded", "{point}: {lines:?}");
            }
            _ => {
                assert!(external_lines(env).is_empty(), "{point}: nothing was sent");
                let (settled, held) = monthly(pipe);
                assert_eq!(settled, "0", "{point}");
                assert_eq!(
                    held, "0",
                    "{point}: a call never sent holds nothing once repaired"
                );
            }
        };
        check(&mut pipe, &env);
        let before = (monthly(&mut pipe), external_lines(&env));
        drop(pipe);
        drop(core);
        let _again = CoreFixture::start(&env);
        let mut pipe = session(&env, None);
        check(&mut pipe, &env);
        assert_eq!(
            (monthly(&mut pipe), external_lines(&env)),
            before,
            "{point}: a second restart changes nothing"
        );
    }
}

/// `confirm.after_answer`: the app approves a destructive call and the core dies right after
/// committing the answer, before telling anyone. The next core keeps the approval: the
/// caller's retry with the same token runs the call once, and a second retry gets the same
/// outcome without running it again (ARCH-053, CORE-091, QA-016).
#[test]
#[ignore = "nightly"] // spec: QA-055, ARCH-053, CORE-091, QA-016
fn crash_full_confirmation_answer_survives_a_crash() {
    let env = TestEnv::new();
    let token = key();
    let core = aborting_core(&env, "confirm.after_answer");
    {
        let mut app = TestApp::start(&env, None, &[]);
        let mut pipe = session(&env, None);
        let pending = pipe.tool("test.destructive", json!({}), Some(&token));
        assert_eq!(pending["status"], "pending_confirmation", "{pending}");
        let id = pending["confirmation_id"].as_str().unwrap().to_string();
        // The core dies inside the answer: the app gets no response.
        let answered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            app.call(
                "app/confirmation.answer",
                json!({"confirmation_id": id, "answer": "approve"}),
            )
        }));
        if let Ok(response) = answered {
            panic!("the core answered instead of stopping: {response}");
        }
    }
    assert_eq!(
        core.wait_exit(DEADLINE),
        Some(lil_core::store::failpoints::ABORT_EXIT_CODE),
        "the core did not end at confirm.after_answer"
    );
    drop(core);

    let _core = CoreFixture::start(&env);
    let _app = TestApp::start(&env, None, &[]);
    let mut pipe = session(&env, None);
    let ran = pipe.tool("test.destructive", json!({}), Some(&token));
    assert_eq!(ran["status"], "ok", "the approval was kept: {ran}");
    assert_eq!(ran["structured"]["count"], 1, "{ran}");
    let again = pipe.tool("test.destructive", json!({}), Some(&token));
    assert_eq!(again["structured"]["count"], 1, "ran once: {again}");
}

/// `path` with `suffix` appended to its file name.
fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// A damaged `telemetry.db` is set aside as `telemetry.db.damaged-<UTC>` with its bytes
/// unchanged, never deleted, and a new one serves samples (CORE-004, CORE-062).
#[test]
#[ignore = "nightly"] // spec: QA-055, CORE-004
fn crash_full_corrupt_telemetry_db_is_set_aside_and_recreated() {
    let env = TestEnv::new();
    let db = env.env.local_dir().join("telemetry.db");
    write_file(&db, &[0x5A_u8; 8192]);
    let before = sha256_file(&db);
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);
    pipe.tool_ok("test.hw_tick", json!({"samples": 2}), Some(&key()));
    let snapshot = pipe.tool_ok("telemetry.snapshot", json!({}), None);
    assert!(snapshot["ts"].is_string(), "{snapshot}");
    let aside: Vec<PathBuf> = std::fs::read_dir(env.env.local_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("telemetry.db.damaged-"))
        })
        .filter(|p| {
            !p.to_string_lossy().ends_with("-wal") && !p.to_string_lossy().ends_with("-shm")
        })
        .collect();
    assert_eq!(aside.len(), 1, "{aside:?}");
    assert_eq!(
        sha256_file(&aside[0]),
        before,
        "the damaged file is kept as it was"
    );
    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let check: String = conn
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(check, "ok");
}

/// A damaged suite `state.db` stops the core with exit code 5 and is left exactly as it was,
/// with no side file made; a damaged workspace `state.db` fails only that workspace, and the
/// core serves the rest (CORE-004, CORE-005).
#[test]
#[ignore = "nightly"] // spec: QA-055, CORE-004, CORE-005
fn crash_full_damaged_state_db_in_each_scope() {
    // The suite scope.
    let env = TestEnv::new();
    let db = env.env.suite_state_db();
    write_file(&db, &[0xAB_u8; 4096]);
    let before = sha256_file(&db);
    let core = CoreFixture::spawn_raw(&env);
    assert_eq!(
        core.wait_exit(DEADLINE),
        Some(5),
        "a damaged suite state.db"
    );
    assert_eq!(
        sha256_file(&db),
        before,
        "the damaged suite state.db changed"
    );
    for suffix in ["-wal", "-shm", "-journal"] {
        assert!(!beside(&db, suffix).exists(), "{suffix} appeared");
    }

    // The workspace scope.
    let env = TestEnv::new();
    let ws = default_workspace(&env, "damaged-ws");
    let db = env.workspace_local_dir(&ws).join("state.db");
    write_file(&db, &[0xAB_u8; 4096]);
    let before = sha256_file(&db);
    let _core = CoreFixture::start(&env);
    let mut pipe = RawPipe::connect(&env);
    let hello = pipe.hello(json!({"workspace": env.workspace_id(&ws)}));
    let refused = if hello["result"]["status"] == "ok" {
        pipe.tool("jobs.list", json!({}), None)
    } else {
        hello["result"].clone()
    };
    assert_eq!(refused["error"]["code"], "state.unavailable", "{refused}");
    let mut other = session(&env, None);
    other.tool_ok("jobs.list", json!({}), None);
    assert_eq!(
        sha256_file(&db),
        before,
        "the damaged workspace state.db changed"
    );
}
