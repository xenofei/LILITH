//! Store order and areas: a crash after the outputs leaves a listed orphan, the per-machine
//! monthly append logs, staging handles inside the workspace, and the read-only workspace of a
//! second core (spec ARCH-028, ARCH-033, ARCH-075, ARCH-080, ARCH-081, CORE-003, CORE-009,
//! CORE-038, QA-055; Appendix E "Write order", "Append logs", "Workspace areas" and
//! "`core.lock`"). Driven through the test-channel tools `test.ingest_run` and
//! `test.store_write`, `project.status` and the workspace files (QA-018).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use ww_testkit::{CoreFixture, RawPipe, TestEnv, failpoints, utc};

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);
/// The fixture asset the runs are ingested into.
const ASSET: &str = "chr_paper_crane";
/// Its folder, workspace-relative.
const ASSET_DIR: &str = "projects/sketchbook/assets/chr_paper_crane";
/// The candidate runs the fixture already has.
const FIXTURE_RUNS: [&str; 2] = ["r_20261002_0915", "r_20261003_1400"];

/// A fresh idempotency key for every write call (ARCH-035).
fn key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "spec-so-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// A copy of the fixture workspace, registered as this channel's default.
fn default_workspace(env: &TestEnv) -> PathBuf {
    let ws = env.copy_fixture_workspace("order");
    env.register_workspace(&ws, true);
    ws
}

/// A connection whose hello names the workspace `ws`, checked to succeed.
fn session(env: &TestEnv, ws: &Path) -> RawPipe {
    let mut pipe = RawPipe::connect(env);
    let hello = pipe.hello(json!({"workspace": env.workspace_id(ws)}));
    assert_eq!(
        hello["result"]["status"],
        "ok",
        "hello naming {}: {hello}",
        ws.display()
    );
    pipe
}

/// The file at the workspace-relative `rel` (with `/` separators).
fn file(ws: &Path, rel: &str) -> PathBuf {
    ws.join(rel.replace('/', "\\"))
}

/// `canonicalize` without the `\\?\` prefix, lowercased for comparison.
fn canon_lower(path: &Path) -> String {
    let canonical = std::fs::canonicalize(path)
        .unwrap_or_else(|e| panic!("cannot canonicalise {}: {e}", path.display()));
    let text = canonical.to_string_lossy().into_owned();
    text.strip_prefix(r"\\?\")
        .map(str::to_string)
        .unwrap_or(text)
        .to_lowercase()
}

/// True for a run id of the Appendix E form `r_<yyyymmdd>_<hhmm>[_<n>]`.
fn is_run_id(id: &str) -> bool {
    let parts: Vec<&str> = id.split('_').collect();
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    match parts.as_slice() {
        ["r", date, time] => digits(date, 8) && digits(time, 4),
        ["r", date, time, n] => {
            digits(date, 8)
                && digits(time, 4)
                && !n.is_empty()
                && n.bytes().all(|b| b.is_ascii_digit())
        }
        _ => false,
    }
}

/// `test.ingest_run` of one or more `(name, text)` files into the fixture asset.
fn ingest(pipe: &mut RawPipe, files: &[(&str, &str)], job_line: bool) -> Value {
    let files: Vec<Value> = files
        .iter()
        .map(|(name, text)| json!({"name": name, "text": text}))
        .collect();
    pipe.tool_ok(
        "test.ingest_run",
        json!({"project": "sketchbook", "asset_id": ASSET, "files": files, "job_line": job_line}),
        Some(&key()),
    )
}

/// The `queue/jobs.*.jsonl` files of the workspace.
fn job_logs(ws: &Path) -> Vec<PathBuf> {
    let queue = ws.join("queue");
    let Ok(entries) = std::fs::read_dir(&queue) else {
        return Vec::new();
    };
    let mut logs: Vec<PathBuf> = entries
        .map(|e| {
            e.unwrap_or_else(|e| panic!("cannot list {}: {e}", queue.display()))
                .path()
        })
        .filter(|p| {
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            name.starts_with("jobs.") && name.ends_with(".jsonl")
        })
        .collect();
    logs.sort();
    logs
}

/// The one jobs log of this month: `queue/jobs.testmachine.<yyyy-mm>.jsonl`, `<yyyy-mm>` taken
/// from `months` (the UTC month before and after the calls).
fn the_job_log(ws: &Path, months: &[String]) -> PathBuf {
    let logs = job_logs(ws);
    assert_eq!(logs.len(), 1, "expected exactly one jobs log: {logs:?}");
    let name = logs[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        months
            .iter()
            .any(|m| name == format!("jobs.testmachine.{m}.jsonl")),
        "the jobs log is {name}, not jobs.testmachine.<{months:?}>.jsonl"
    );
    logs[0].clone()
}

/// Checks that `line` is a log line of `testmachine` with `seq` first two keys, and `seq`
/// equal to `want`.
fn assert_log_line(line: &str, want: u64) {
    let value: Value = serde_json::from_str(line)
        .unwrap_or_else(|e| panic!("log line is not JSON ({e}): {line:?}"));
    let object = value
        .as_object()
        .unwrap_or_else(|| panic!("log line is not an object: {line:?}"));
    let keys: Vec<&str> = object.keys().map(String::as_str).take(2).collect();
    assert_eq!(keys, ["machine", "seq"], "{line}");
    assert_eq!(object["machine"], "testmachine", "{line}");
    assert_eq!(object["seq"], json!(want), "{line}");
}

#[test] // spec: ARCH-075, CORE-038, QA-055
fn crash_after_output_lists_orphan_and_keeps_files() {
    let env = TestEnv::new();
    let ws = default_workspace(&env);

    {
        let points = failpoints::spec(&[("store.after_output", "abort")]);
        let mut core = CoreFixture::start_with_env(&env, &[(failpoints::ENV, &points)]);
        let mut pipe = session(&env, &ws);
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1_000_001,
            "method": "tools/call",
            "params": {
                "name": "test.ingest_run",
                "arguments": {
                    "project": "sketchbook",
                    "asset_id": ASSET,
                    "files": [{"name": "crash_a.png", "text": "crash a"}],
                    "job_line": false,
                },
                "idempotency_key": key(),
            },
        });
        pipe.send_line(&request.to_string());
        if let Some(line) = pipe.read_line(DEADLINE) {
            panic!("the core answered instead of stopping at store.after_output: {line}");
        }
        assert!(
            core.wait_exit(DEADLINE).is_some(),
            "the core still runs {DEADLINE:?} after store.after_output=abort"
        );
    }

    let candidates = file(&ws, &format!("{ASSET_DIR}/candidates"));
    let new_runs: Vec<String> = std::fs::read_dir(&candidates)
        .unwrap_or_else(|e| panic!("cannot list {}: {e}", candidates.display()))
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| !FIXTURE_RUNS.contains(&name.as_str()))
        .collect();
    assert_eq!(
        new_runs.len(),
        1,
        "expected one new run folder: {new_runs:?}"
    );
    let run = &new_runs[0];
    assert!(is_run_id(run), "{run} is not a run id");
    let output = candidates.join(run).join("crash_a.png");
    assert_eq!(
        std::fs::read(&output).unwrap_or_else(|e| panic!("{}: {e}", output.display())),
        b"crash a"
    );

    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);
    let status = pipe.tool_ok("project.status", json!({"project": "sketchbook"}), None);
    let folder = format!("{ASSET_DIR}/candidates/{run}");
    let orphans = status["orphans"]
        .as_array()
        .unwrap_or_else(|| panic!("project.status has no orphans list: {status}"));
    assert!(
        orphans
            .iter()
            .filter_map(Value::as_str)
            .any(|p| p.trim_end_matches('/') == folder),
        "{folder} is not listed as an orphan: {status}"
    );
    assert_eq!(
        std::fs::read(&output).unwrap_or_else(|e| panic!("{}: {e}", output.display())),
        b"crash a",
        "the orphan's file was not kept"
    );
}

#[test] // spec: ARCH-081, CORE-009
fn append_log_lines_are_per_machine_per_month_with_seq() {
    let env = TestEnv::new();
    let ws = default_workspace(&env);
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);

    let mut months = vec![utc::year_month()];
    ingest(&mut pipe, &[("log_a.png", "log a")], true);
    ingest(&mut pipe, &[("log_b.png", "log b")], true);
    months.push(utc::year_month());

    let log = the_job_log(&ws, &months);
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.ends_with('\n'), "every line ends in \\n: {text:?}");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert_log_line(lines[0], 1);
    assert_log_line(lines[1], 2);
}

#[test] // spec: ARCH-081
fn torn_last_line_is_closed_before_the_next_append() {
    let env = TestEnv::new();
    let ws = default_workspace(&env);
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);

    let mut months = vec![utc::year_month()];
    ingest(&mut pipe, &[("torn_a.png", "torn a")], true);
    ingest(&mut pipe, &[("torn_b.png", "torn b")], true);
    let log = the_job_log(&ws, &months);

    // An appender that died mid-line left a fragment without its newline.
    let mut torn = std::fs::read(&log).unwrap();
    torn.extend_from_slice(br#"{"seq":"#);
    std::fs::write(&log, &torn).unwrap();

    ingest(&mut pipe, &[("torn_c.png", "torn c")], true);
    months.push(utc::year_month());
    assert_eq!(the_job_log(&ws, &months), log);

    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.ends_with('\n'), "{text:?}");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4, "{text}");
    assert_eq!(
        lines[2], r#"{"seq":"#,
        "the fragment must stay alone on its line"
    );
    assert_log_line(lines[3], 3);
}

#[test] // spec: ARCH-080, ARCH-033, CORE-003
fn staging_handle_is_in_workspace_and_ingest_moves_files() {
    let env = TestEnv::new();
    let ws = default_workspace(&env);
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);

    let result = ingest(
        &mut pipe,
        &[("stage_a.png", "stage a"), ("stage_b.png", "stage b")],
        false,
    );
    let run_id = result["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no run_id: {result}"));
    assert!(is_run_id(run_id), "{run_id} is not a run id");

    let staging_dir = result["staging_dir"]
        .as_str()
        .unwrap_or_else(|| panic!("no staging_dir: {result}"));
    let prefix = format!(r"{}\.worldwright\staging\", canon_lower(&ws));
    let staging_lower = staging_dir.to_lowercase();
    let handle = staging_lower
        .strip_prefix(&prefix)
        .unwrap_or_else(|| panic!("staging_dir {staging_dir} is not under {prefix}"));
    let handle = handle.trim_end_matches('\\');
    let hex = handle
        .strip_prefix("sh_")
        .unwrap_or_else(|| panic!("the handle {handle} is not sh_<16 hex>"));
    assert!(
        hex.len() == 16 && hex.bytes().all(|b| b.is_ascii_hexdigit()),
        "the handle {handle} is not sh_<16 hex>"
    );
    assert!(
        !Path::new(staging_dir).exists(),
        "the staging handle {staging_dir} still exists after the ingest"
    );

    let run_dir = file(&ws, &format!("{ASSET_DIR}/candidates/{run_id}"));
    assert_eq!(
        std::fs::read(run_dir.join("stage_a.png")).unwrap(),
        b"stage a"
    );
    assert_eq!(
        std::fs::read(run_dir.join("stage_b.png")).unwrap(),
        b"stage b"
    );

    let manifest_path = file(&ws, &format!("{ASSET_DIR}/{ASSET}.manifest.json"));
    let manifest: Value = serde_json::from_slice(&std::fs::read(&manifest_path).unwrap())
        .unwrap_or_else(|e| panic!("{} is not JSON: {e}", manifest_path.display()));
    let runs = manifest["candidate_runs"]
        .as_array()
        .unwrap_or_else(|| panic!("no candidate_runs: {manifest}"));
    assert!(
        runs.iter().any(|r| r["run_id"] == run_id),
        "candidate_runs has no {run_id}: {manifest}"
    );
}

#[test] // spec: ARCH-028
fn second_channel_core_gets_read_only_workspace() {
    let first = TestEnv::new();
    let second = TestEnv::new();
    let ws = first.copy_fixture_workspace("shared");
    first.register_workspace(&ws, true);
    second.register_workspace(&ws, true);

    let _first_core = CoreFixture::start(&first);
    let mut a = session(&first, &ws);
    let served = a.tool(
        "test.store_write",
        json!({"path": "notes/first.txt", "text": "first", "expect": "new"}),
        Some(&key()),
    );
    assert_eq!(
        served["status"], "ok",
        "the first core serves the workspace: {served}"
    );

    let _second_core = CoreFixture::start(&second);
    let mut b = session(&second, &ws);
    let refused = b.tool(
        "test.store_write",
        json!({"path": "notes/second.txt", "text": "second", "expect": "new"}),
        Some(&key()),
    );
    assert_eq!(refused["status"], "error", "{refused}");
    assert_eq!(refused["error"]["code"], "workspace.read_only", "{refused}");
    assert!(!file(&ws, "notes/second.txt").exists());
}
