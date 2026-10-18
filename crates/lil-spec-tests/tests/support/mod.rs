//! Helpers shared by the plan-3 scenario spec tests (QA-017, QA-018). They drive only the
//! stable surfaces: `RawPipe`, `TestApp`, the real `lil`, `lil-mcp` over stdio, the test kit's
//! fakes and files read as an outside program reads them. Each test binary uses a part of
//! them, so unused ones are allowed here.

#![allow(
    dead_code,
    reason = "each spec-test binary uses a different part of these helpers"
)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use lil_testkit::{CoreFixture, FakeComfy, LilOutput, McpStdio, RawPipe, TestApp, TestEnv};
use serde_json::{Value, json};

/// A failure deadline only, never a measurement (QA-031).
pub const DEADLINE: Duration = Duration::from_secs(30);
/// Pause between polls of a state that changes on its own.
const POLL: Duration = Duration::from_millis(50);

/// A fresh idempotency key for every write call (ARCH-035).
pub fn key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "spec-p3-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// What a test calls tools through: a `cli` connection (`RawPipe`) or the `ui:test` app.
pub trait Caller {
    fn tool(&mut self, name: &str, args: Value, key: Option<&str>) -> Value;
    fn call(&mut self, method: &str, params: Value) -> Value;
    fn next_event(&mut self, within: Duration) -> Option<Value>;
}

impl Caller for RawPipe {
    fn tool(&mut self, name: &str, args: Value, key: Option<&str>) -> Value {
        RawPipe::tool(self, name, args, key)
    }
    fn call(&mut self, method: &str, params: Value) -> Value {
        RawPipe::call(self, method, params)
    }
    fn next_event(&mut self, within: Duration) -> Option<Value> {
        RawPipe::next_event(self, within)
    }
}

impl Caller for TestApp {
    fn tool(&mut self, name: &str, args: Value, key: Option<&str>) -> Value {
        TestApp::tool(self, name, args, key)
    }
    fn call(&mut self, method: &str, params: Value) -> Value {
        TestApp::call(self, method, params)
    }
    fn next_event(&mut self, within: Duration) -> Option<Value> {
        TestApp::next_event(self, within)
    }
}

/// A tool call that must succeed: returns its `structured` value.
pub fn ok(c: &mut impl Caller, name: &str, args: Value, key: Option<&str>) -> Value {
    let outcome = c.tool(name, args.clone(), key);
    assert_eq!(
        outcome["status"], "ok",
        "{name} {args} did not succeed: {outcome}"
    );
    outcome.get("structured").cloned().unwrap_or(Value::Null)
}

/// A write call with a fresh idempotency key that must succeed.
pub fn write_ok(c: &mut impl Caller, name: &str, args: Value) -> Value {
    ok(c, name, args, Some(&key()))
}

/// The error code of an `Outcome`, panicking unless it is an error.
pub fn error_code(outcome: &Value) -> String {
    assert_eq!(outcome["status"], "error", "expected an error: {outcome}");
    outcome["error"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("the error has no code: {outcome}"))
        .to_string()
}

/// A `cli` connection whose hello names `ws` (or no workspace), checked to succeed.
pub fn session(env: &TestEnv, ws: Option<&Path>) -> RawPipe {
    let mut pipe = RawPipe::connect(env);
    let workspace = ws.map_or_else(|| "none".to_string(), |ws| env.workspace_id(ws));
    let hello = pipe.hello(json!({ "workspace": workspace }));
    assert_eq!(hello["result"]["status"], "ok", "hello: {hello}");
    pipe
}

/// A `cli` connection that declared itself non-interactive (QA-016).
pub fn non_interactive_session(env: &TestEnv, ws: Option<&Path>) -> RawPipe {
    let mut pipe = RawPipe::connect(env);
    let workspace = ws.map_or_else(|| "none".to_string(), |ws| env.workspace_id(ws));
    let hello = pipe.hello(json!({ "workspace": workspace, "non_interactive": true }));
    assert_eq!(hello["result"]["status"], "ok", "hello: {hello}");
    pipe
}

/// A copy of the fixture workspace, registered as this channel's default.
pub fn default_workspace(env: &TestEnv, name: &str) -> PathBuf {
    let ws = env.copy_fixture_workspace(name);
    env.register_workspace(&ws, true);
    ws
}

/// `path` as UTF-8, for a command line or a JSON argument.
pub fn path_str(path: &Path) -> String {
    path.to_str()
        .unwrap_or_else(|| panic!("{} is not valid UTF-8", path.display()))
        .to_string()
}

/// `lil`'s stdout as JSON.
pub fn stdout_json(out: &LilOutput) -> Value {
    serde_json::from_str(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {out:?}"))
}

/// Polls `f` until it returns `Some`, failing after [`DEADLINE`] with `what`.
pub fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(
            Instant::now() < deadline,
            "{what}: not reached within {DEADLINE:?}"
        );
        std::thread::sleep(POLL);
    }
}

// ---------------------------------------------------------------------------------------------
// Jobs (Global Constraints "Jobs", "Product tools")

/// `^j_[0-9a-f]{16}$` (CORE-117).
pub fn is_job_id(id: &str) -> bool {
    id.len() == 18
        && id.starts_with("j_")
        && id[2..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The `job_id` of a `{job}` result.
pub fn job_id_of(structured: &Value) -> String {
    structured["job"]["job_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no job.job_id in {structured}"))
        .to_string()
}

/// Submits a job tool with a fresh key and returns its job id.
pub fn submit(c: &mut impl Caller, tool: &str, args: Value) -> String {
    job_id_of(&write_ok(c, tool, args))
}

/// `job.wait` until the job is done (at most 30 s); returns its `JobSummary`.
pub fn wait_done(c: &mut impl Caller, job_id: &str) -> Value {
    let waited = ok(
        c,
        "job.wait",
        json!({"job_id": job_id, "timeout_s": 30}),
        None,
    );
    assert_eq!(waited["done"], true, "job {job_id} is not done: {waited}");
    waited["job"].clone()
}

/// The job's current `JobSummary`, from `jobs.list`.
pub fn job(c: &mut impl Caller, job_id: &str) -> Value {
    let list = ok(c, "jobs.list", json!({"ids": [job_id]}), None);
    list["jobs"]
        .as_array()
        .and_then(|jobs| jobs.iter().find(|j| j["job_id"] == job_id))
        .cloned()
        .unwrap_or_else(|| panic!("jobs.list has no {job_id}: {list}"))
}

/// Every job `jobs.list` returns.
pub fn all_jobs(c: &mut impl Caller) -> Vec<Value> {
    let list = ok(c, "jobs.list", json!({"limit": 500}), None);
    list["jobs"]
        .as_array()
        .unwrap_or_else(|| panic!("jobs.list has no jobs list: {list}"))
        .clone()
}

/// Polls the job until `pred` holds for its summary; returns that summary.
pub fn until_job(
    c: &mut impl Caller,
    job_id: &str,
    what: &str,
    pred: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let last = job(c, job_id);
        if pred(&last) {
            return last;
        }
        assert!(
            Instant::now() < deadline,
            "job {job_id} never became {what}; last: {last}"
        );
        std::thread::sleep(POLL);
    }
}

/// True when the summary's status is `running`.
pub fn running(j: &Value) -> bool {
    j["status"] == "running"
}

/// `test.job_release` of a gated job.
pub fn release(c: &mut impl Caller, job_id: &str) {
    write_ok(c, "test.job_release", json!({ "job_id": job_id }));
}

// ---------------------------------------------------------------------------------------------
// Events (Appendix D "Events")

/// Subscribes to `topic` replaying its whole history; returns the topic's current `seq`.
pub fn subscribe_replay(c: &mut impl Caller, topic: &str) -> u64 {
    let response = c.call(
        "lil/subscribe",
        json!({"topics": [topic], "since_seq": {topic: 0}}),
    );
    assert_eq!(
        response["result"]["status"], "ok",
        "subscribe {topic}: {response}"
    );
    response["result"]["topics"]
        .as_array()
        .and_then(|topics| topics.iter().find(|t| t["topic"] == topic))
        .and_then(|t| t["seq"].as_u64())
        .unwrap_or_else(|| panic!("the subscription names no seq for {topic}: {response}"))
}

/// The `data` of every `event` on `topic` up to and including `seq`, in order, read from a
/// connection that has just called [`subscribe_replay`].
pub fn replayed(c: &mut impl Caller, topic: &str, seq: u64) -> Vec<Value> {
    let mut seen = Vec::new();
    let mut last = 0;
    while last < seq {
        let event = c
            .next_event(DEADLINE)
            .unwrap_or_else(|| panic!("no event on {topic} up to seq {seq}; got {seen:?}"));
        if event["topic"] != topic {
            continue;
        }
        last = event["seq"].as_u64().unwrap_or(last);
        if event["kind"] == "event" {
            seen.push(event["data"].clone());
        }
    }
    seen
}

/// The next `event` on `topic` whose `data.kind` is `kind`; others are skipped.
pub fn wait_for_kind(c: &mut impl Caller, topic: &str, kind: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let event = c
            .next_event(left)
            .unwrap_or_else(|| panic!("no {kind} event on {topic} within {DEADLINE:?}"));
        if event["topic"] == topic && event["data"]["kind"] == kind {
            return event["data"].clone();
        }
    }
}

/// Subscribes `c` to `topics` for live events only.
pub fn subscribe_live(c: &mut impl Caller, topics: &[&str]) {
    let response = c.call("lil/subscribe", json!({"topics": topics, "since_seq": {}}));
    assert_eq!(
        response["result"]["status"], "ok",
        "subscribe {topics:?}: {response}"
    );
}

// ---------------------------------------------------------------------------------------------
// Confirmations (Global Constraints "Confirmations", "`app/*` methods")

/// The `confirmation_id` of a `pending_confirmation` outcome, panicking on anything else.
pub fn pending_id(outcome: &Value) -> String {
    assert_eq!(
        outcome["status"], "pending_confirmation",
        "expected pending_confirmation: {outcome}"
    );
    outcome["confirmation_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no confirmation_id: {outcome}"))
        .to_string()
}

/// `app/confirmation.answer` from the app, checked to succeed.
pub fn answer(app: &mut TestApp, confirmation_id: &str, params: Value) -> Value {
    let mut p = json!({ "confirmation_id": confirmation_id });
    if let (Some(into), Some(from)) = (p.as_object_mut(), params.as_object()) {
        for (k, v) in from {
            into.insert(k.clone(), v.clone());
        }
    }
    let response = app.call("app/confirmation.answer", p);
    assert_eq!(
        response["result"]["status"], "ok",
        "app/confirmation.answer: {response}"
    );
    response["result"].clone()
}

/// `app/confirmations.list` of `state` (`pending` or `history`), checked to succeed.
pub fn confirmations(c: &mut impl Caller, state: &str) -> Vec<Value> {
    let response = c.call("app/confirmations.list", json!({ "state": state }));
    assert_eq!(
        response["result"]["status"], "ok",
        "app/confirmations.list: {response}"
    );
    response["result"]["confirmations"]
        .as_array()
        .unwrap_or_else(|| panic!("no confirmations list: {response}"))
        .clone()
}

// ---------------------------------------------------------------------------------------------
// ComfyUI (Global Constraints "ComfyUI engine")

/// `[engines.comfyui]` for an attached `FakeComfy`: its port, and its folder as `root` so the
/// suite can clean up and salvage ComfyUI-side files (the engine stays attached: the core
/// didn't start it).
pub fn attach(env: &TestEnv, fake: &FakeComfy) {
    env.suite_toml_set(&format!(
        "[engines.comfyui]\nport = {}\nroot = '{}'\nautostart = false\n",
        fake.port(),
        path_str(&fake.root())
    ));
}

/// Polls `engines.status` until the ComfyUI entry satisfies `pred`; returns it.
pub fn until_engine(c: &mut impl Caller, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let status = ok(c, "engines.status", json!({}), None);
        let last = status["engines"]
            .as_array()
            .and_then(|e| e.iter().find(|e| e["engine"] == "comfyui"))
            .cloned()
            .unwrap_or_else(|| panic!("engines.status has no comfyui: {status}"));
        if pred(&last) {
            return last;
        }
        assert!(
            Instant::now() < deadline,
            "the comfyui engine never became {what}; last: {last}"
        );
        std::thread::sleep(POLL);
    }
}

/// The `POST /prompt` bodies the fake received, in order.
pub fn posted_prompts(fake: &FakeComfy) -> Vec<Value> {
    fake.requests()
        .into_iter()
        .filter(|r| r.method == "POST" && r.path.trim_start_matches("/api") == "/prompt")
        .map(|r| r.body.unwrap_or(Value::Null))
        .collect()
}

/// A canonical lowercase hyphenated UUID (CORE-120).
pub fn is_canonical_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
        })
}

/// One `GET <path>` to `127.0.0.1:<port>`; `Some(status code)` when anything answered.
pub fn http_status(port: u16, path: &str) -> Option<u16> {
    use std::io::{Read as _, Write as _};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    let mut head = [0_u8; 12];
    stream.read_exact(&mut head).ok()?;
    std::str::from_utf8(&head[9..12]).ok()?.parse().ok()
}

// ---------------------------------------------------------------------------------------------
// Processes and cores

/// Whether a process with this id runs (from `tasklist`, as an outside program sees it).
pub fn pid_alive(pid: u32) -> bool {
    let out = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .unwrap_or_else(|e| panic!("cannot run tasklist: {e}"));
    String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
}

/// The value of `LIL_TEST_FAILPOINTS` for plan-3 fail points (Global Constraints, "Fail points
/// added"), which the test kit's closed list does not name yet.
pub fn failpoints(points: &[(&str, &str)]) -> String {
    points
        .iter()
        .map(|(name, action)| format!("{name}={action}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Sends a tool call on `pipe` to a core that is meant to abort while handling it (a fail
/// point): the call's response, if one comes before the connection closes, must be `ok`; then
/// the connection must close. Returns the `structured` value of that response, if any.
pub fn call_into_abort(pipe: &mut RawPipe, name: &str, args: Value) -> Option<Value> {
    let request = json!({"jsonrpc": "2.0", "id": 9001, "method": "tools/call",
        "params": {"name": name, "arguments": args, "idempotency_key": key()}});
    pipe.send_line(&request.to_string());
    let deadline = Instant::now() + DEADLINE;
    let mut structured = None;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            panic!("the core neither answered {name} nor closed the connection");
        }
        let Some(line) = pipe.read_line(left) else {
            return structured;
        };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if message["id"] == 9001 {
            let result = &message["result"];
            assert_eq!(
                result["status"], "ok",
                "{name} failed before the fail point: {message}"
            );
            structured = result.get("structured").cloned();
        }
    }
}

/// Starts a core that may abort: a fixture killed on drop if it is still running.
pub fn start_abortable(env: &TestEnv, extra: &[(&str, &str)]) -> lil_testkit::KillOnDrop {
    CoreFixture::spawn_raw_with_env(env, extra)
}

/// Runs the test build of `lil` with `args` in `dir`, with the channel's environment plus
/// `extra` (each replacing a variable of the same name), and `stdin` written to it. Every
/// inherited `GIT_*` variable is removed first (QA-038). Fails after 120 s.
pub fn lil_with(
    env: &TestEnv,
    dir: &Path,
    args: &[&str],
    extra: &[(&str, OsString)],
    stdin: Option<&str>,
) -> LilOutput {
    use std::io::Write as _;
    let mut cmd = Command::new(&lil_testkit::ensure_built().lil);
    cmd.args(args).current_dir(dir);
    for (name, _) in std::env::vars_os() {
        if name
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("GIT_")
        {
            cmd.env_remove(&name);
        }
    }
    cmd.envs(env.child_env());
    for (name, value) in extra {
        cmd.env(name, value);
    }
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let start = Instant::now();
    let mut child = cmd
        .spawn()
        .unwrap_or_else(|e| panic!("cannot start lil {args:?}: {e}"));
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let text = text.to_string();
        std::thread::spawn(move || {
            let _ = pipe.write_all(text.as_bytes());
        });
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let output = rx
        .recv_timeout(Duration::from_secs(120))
        .unwrap_or_else(|_| panic!("lil {args:?} did not finish within 120 s"))
        .unwrap_or_else(|e| panic!("waiting for lil {args:?} failed: {e}"));
    LilOutput {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        elapsed: start.elapsed(),
    }
}

// ---------------------------------------------------------------------------------------------
// MCP (Global Constraints "MCP lowering")

/// `initialize` (protocol `2026-07-28`) and `notifications/initialized`; returns the result.
pub fn mcp_initialize(mcp: &mut McpStdio) -> Value {
    let response = mcp.request(
        "initialize",
        json!({"protocolVersion": "2026-07-28", "capabilities": {},
               "clientInfo": {"name": "lil-spec-tests", "version": "0.0.0"}}),
    );
    let result = response
        .get("result")
        .cloned()
        .unwrap_or_else(|| panic!("initialize failed: {response}"));
    mcp.notify("notifications/initialized", json!({}));
    result
}

/// `tools/list`: the tools, all pages.
pub fn mcp_tools(mcp: &mut McpStdio) -> Vec<Value> {
    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let params = match &cursor {
            Some(c) => json!({ "cursor": c }),
            None => json!({}),
        };
        let response = mcp.request("tools/list", params);
        let result = response
            .get("result")
            .unwrap_or_else(|| panic!("tools/list failed: {response}"));
        tools.extend(
            result["tools"]
                .as_array()
                .unwrap_or_else(|| panic!("tools/list has no tools: {response}"))
                .iter()
                .cloned(),
        );
        match result["nextCursor"].as_str() {
            Some(next) => cursor = Some(next.to_string()),
            None => return tools,
        }
    }
}

/// What a `tools/call` returned: `isError`, the JSON of its first text block, and every text
/// block joined.
pub struct McpCall {
    pub is_error: bool,
    pub json: Value,
    pub text: String,
}

/// `tools/call` of `name` with `args`.
pub fn mcp_call(mcp: &mut McpStdio, name: &str, args: Value) -> McpCall {
    let response = mcp.request("tools/call", json!({"name": name, "arguments": args}));
    let result = response
        .get("result")
        .unwrap_or_else(|| panic!("tools/call {name} failed at the protocol level: {response}"));
    let texts: Vec<String> = result["content"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/call {name} has no content: {response}"))
        .iter()
        .filter(|c| c["type"] == "text")
        .filter_map(|c| c["text"].as_str().map(str::to_string))
        .collect();
    let json = texts
        .iter()
        .find_map(|t| serde_json::from_str::<Value>(t).ok())
        .unwrap_or_else(|| panic!("tools/call {name} has no JSON text block: {response}"));
    McpCall {
        is_error: result["isError"] == true,
        json,
        text: texts.join("\n"),
    }
}

// ---------------------------------------------------------------------------------------------
// Files and times

/// The lines of a JSON-lines file, parsed (none when the file is missing).
pub fn jsonl(path: &Path) -> Vec<Value> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str(l)
                    .unwrap_or_else(|e| panic!("{}: a line is not JSON ({e}): {l}", path.display()))
            })
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => panic!("cannot read {}: {e}", path.display()),
    }
}

/// Every file in `dir` whose name starts with `prefix` and ends with `suffix`, sorted.
pub fn files_named(dir: &Path, prefix: &str, suffix: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().is_some_and(|n| {
                let n = n.to_string_lossy();
                n.starts_with(prefix) && n.ends_with(suffix)
            })
        })
        .collect();
    found.sort();
    found
}

/// `<local>\usage-loose.jsonl`: the usage log of calls without a workspace (ARCH-081).
pub fn loose_usage(env: &TestEnv) -> PathBuf {
    env.env.local_dir().join("usage-loose.jsonl")
}

/// A decimal string or JSON number as a float, for comparisons with a tolerance.
pub fn number(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => s.parse().unwrap_or(f64::NAN),
        _ => f64::NAN,
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` of Unix seconds (UTC).
pub fn rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}
