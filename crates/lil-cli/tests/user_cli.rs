//! The `lil` user commands against the real `lil` and `lil-core` (QA-056): `status`, `call`,
//! `events --follow` and `core stop`, run the way scripts and agents run them, with stdout and
//! stderr captured through pipes (spec Appendix D, "The `lil` user commands").
//!
//! Every test runs on its own `TestEnv` channel; any core `lil` starts is drained when the env
//! drops (QA-041). Durations are failure deadlines only, never measurements (QA-031).

use std::path::{Path, PathBuf};
use std::time::Duration;

use lil_protocol::{EventKind, EventParams, Outcome, ToolOutput};
use lil_testkit::{CoreFixture, LilChild, LilOutput, RawPipe, TestEnv, ensure_built, sha256_file};
use serde_json::Value;

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);

fn stdout_json(out: &LilOutput) -> Value {
    serde_json::from_str(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {out:?}"))
}

fn tool_outcome(out: &LilOutput) -> Outcome<ToolOutput> {
    serde_json::from_value(stdout_json(out))
        .unwrap_or_else(|e| panic!("stdout is not a tool Outcome ({e}): {out:?}"))
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// `path` as written, and its canonical form without the `\\?\` prefix: a process image name
/// is the long form even when the temporary folder was given in 8.3 form.
fn path_forms(path: &Path) -> Vec<String> {
    let canonical = canonical(path).display().to_string();
    let plain = canonical
        .strip_prefix(r"\\?\")
        .unwrap_or(&canonical)
        .to_string();
    vec![path.display().to_string(), plain]
}

/// A `lil` started with `TestEnv::lil_spawn`. The `LilChild` drains its stderr on a thread and
/// kills it if the test ends while it still runs; every failure here shows that stderr.
struct Spawned {
    child: LilChild,
}

impl Spawned {
    fn new(child: LilChild) -> Spawned {
        Spawned { child }
    }

    fn next_line(&mut self) -> String {
        match self.child.stdout_lines().next() {
            Some(line) => line,
            None => panic!(
                "`lil` closed its stdout before printing a line\nstderr: {}",
                self.child.stderr_so_far()
            ),
        }
    }

    /// The exit code once `lil` has exited; panics after the deadline.
    fn wait(&mut self) -> i32 {
        match self.child.wait(DEADLINE) {
            Some(code) => code,
            None => panic!(
                "`lil` still runs after {DEADLINE:?}\nstderr: {}",
                self.child.stderr_so_far()
            ),
        }
    }
}

#[test] // spec: ARCH-016, ARCH-019
fn lil_status_with_captured_output_returns_while_core_runs() {
    let env = TestEnv::new();
    // `TestEnv::lil` captures stdout and stderr through pipes, as `Command::output` does, and
    // reads them to their end: a core holding them would keep it waiting.
    let out = env.lil(&["status"]);
    assert_eq!(out.code, 0, "{out:?}");
    // A failure deadline: the core stays up for minutes, so a `lil` tied to it never gets here.
    assert!(out.elapsed < DEADLINE, "{out:?}");
    assert!(out.stdout.starts_with("core: running (pid "), "{out:?}");
    for line in ["pipe: ", "protocol: ", "workspace: none", "caller: cli"] {
        assert!(out.stdout.contains(line), "no `{line}` in {out:?}");
    }
    let raw = RawPipe::try_connect(&env, Duration::from_secs(1));
    assert!(
        raw.is_some(),
        "the core did not keep running after `lil status`"
    );
}

#[test] // spec: ARCH-016
fn call_reads_args_from_stdin_dash() {
    let env = TestEnv::new();
    let out = env.lil_with_stdin(
        &["call", "test.echo", "-"],
        "{\"text\":\"Ünïcödé space\"}\n",
    );
    assert_eq!(out.code, 0, "{out:?}");
    match tool_outcome(&out) {
        Outcome::Ok(output) => assert_eq!(output.structured["text"], "Ünïcödé space", "{out:?}"),
        other => panic!("expected ok, got {other:?}"),
    }
}

#[test] // spec: ARCH-016
fn call_rejects_non_object_args_with_exit_2() {
    let env = TestEnv::new();
    for args in [
        &["call", "test.echo", "[1, 2]"][..],
        &["call", "test.echo", "\"text\""],
        &["call", "test.echo", "{not json"],
    ] {
        let out = env.lil(args);
        assert_eq!(out.code, 2, "{args:?}: {out:?}");
        assert_eq!(out.stdout, "", "{args:?}: {out:?}");
        assert!(out.stderr.contains("JSON"), "{args:?}: {out:?}");
    }
    let piped = env.lil_with_stdin(&["call", "test.echo", "-"], "42");
    assert_eq!(piped.code, 2, "{piped:?}");
    assert!(piped.stderr.contains("JSON object"), "{piped:?}");
    // The arguments are checked before any core is started.
    assert!(
        RawPipe::try_connect(&env, Duration::from_secs(1)).is_none(),
        "a rejected call started a core"
    );
}

/// Checkpoint A walkthrough (ODD-2): an `app/*` name is sent as that method, so `lil call`
/// gets the core's `caller.not_allowed` ("… is for LILITH apps only."), never `tool.unknown`;
/// `app/confirmations.list`, which a script may call, lists.
#[test] // spec: ARCH-016, APP-043
fn call_sends_an_app_name_as_that_method() {
    let env = TestEnv::new();
    for method in ["app/queue.pause", "app/queue.resume", "app/desktop.set"] {
        let out = env.lil(&["call", method, "{}"]);
        assert_eq!(out.code, 1, "{method}: {out:?}");
        let outcome = stdout_json(&out);
        assert_eq!(outcome["status"], "error", "{method}: {out:?}");
        assert_eq!(
            outcome["error"]["code"], "caller.not_allowed",
            "{method}: {out:?}"
        );
        assert_eq!(
            outcome["error"]["message"],
            format!("{method} is for LILITH apps only."),
            "{method}: {out:?}"
        );
    }
    let listed = env.lil(&["call", "app/confirmations.list", "{\"state\":\"pending\"}"]);
    assert_eq!(listed.code, 0, "{listed:?}");
    assert_eq!(stdout_json(&listed)["status"], "ok", "{listed:?}");
}

#[test] // spec: ARCH-016, ARCH-035
fn call_passes_the_idempotency_key() {
    let env = TestEnv::new();
    let write = |key: &str| {
        let out = env.lil(&[
            "call",
            "test.write",
            "{\"label\":\"Ünïcödé space\"}",
            "--idempotency-key",
            key,
        ]);
        assert_eq!(out.code, 0, "{out:?}");
        match tool_outcome(&out) {
            Outcome::Ok(output) => output.structured["executions"].clone(),
            other => panic!("expected ok, got {other:?}"),
        }
    };
    let first = write("ik_cli_one");
    // The same key replays the first call; the tool does not run again.
    assert_eq!(write("ik_cli_one"), first);
}

#[test] // spec: ARCH-016, ARCH-032
fn events_follow_prints_ndjson() {
    let env = TestEnv::new();
    let mut events = Spawned::new(env.lil_spawn(&[
        "events", "--follow", "--topic", "test:a", "--since", "test:a=0",
    ]));

    let emit = env.lil(&[
        "call",
        "test.emit",
        "{\"topic\":\"a\",\"data\":{\"label\":\"Ünïcödé space\"}}",
    ]);
    assert_eq!(emit.code, 0, "{emit:?}");

    let line = events.next_line();
    let event: EventParams = serde_json::from_str(&line)
        .unwrap_or_else(|e| panic!("not an EventParams line ({e}): {line}"));
    assert_eq!(event.topic, "test:a", "{line}");
    assert_eq!(event.seq, 1, "{line}");
    assert_eq!(event.kind, EventKind::Event, "{line}");
    assert_eq!(event.data["label"], "Ünïcödé space", "{line}");

    // `lil events` ends, with exit 0, when the core goes away.
    let stop = env.lil(&["core", "stop"]);
    assert_eq!(stop.code, 0, "{stop:?}");
    assert_eq!(events.wait(), 0);
}

#[test] // spec: ARCH-030
fn workspace_flag_selects_registered_default() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = env.make_workspace("Ünïcödé default");
    env.register_workspace(&ws, true);
    let ws_arg = ws.to_str().expect("a UTF-8 test path");

    let reported = |out: &LilOutput| {
        assert_eq!(out.code, 0, "{out:?}");
        let status = stdout_json(out);
        assert_eq!(status["running"], true, "{out:?}");
        let path = status["hello"]["workspace"]["path"]
            .as_str()
            .unwrap_or_else(|| panic!("no workspace path: {out:?}"));
        canonical(Path::new(path))
    };
    // The flag, before or after the subcommand.
    assert_eq!(
        reported(&env.lil(&["status", "--json", "--workspace", ws_arg])),
        canonical(&ws)
    );
    assert_eq!(
        reported(&env.lil(&["--workspace", ws_arg, "status", "--json"])),
        canonical(&ws)
    );
    // Without it, the registered default.
    assert_eq!(reported(&env.lil(&["status", "--json"])), canonical(&ws));

    // A folder that is not a workspace is a plain error, not a fallback.
    let plain = env.root().join("not a workspace");
    std::fs::create_dir_all(&plain).unwrap();
    let out = env.lil(&["status", "--workspace", plain.to_str().unwrap()]);
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stderr.contains("lilith.yaml"), "{out:?}");
}

#[test] // spec: ARCH-030, ARCH-016
fn a_stale_default_workspace_names_suite_toml_and_core_stop_still_works() {
    let env = TestEnv::new();
    let gone = env.make_workspace("gone");
    env.register_workspace(&gone, true);
    std::fs::remove_dir_all(&gone).unwrap();

    let status = env.lil(&["status"]);
    assert_eq!(status.code, 2, "{status:?}");
    assert!(status.stderr.contains("suite.toml"), "{status:?}");
    assert!(status.stderr.contains("ws-gone"), "{status:?}");
    let call = env.lil(&["call", "test.echo", "{\"text\":\"x\"}"]);
    assert_eq!(call.code, 2, "{call:?}");
    assert!(call.stderr.contains("suite.toml"), "{call:?}");

    // Stopping never depends on workspace configuration.
    let core = CoreFixture::start(&env);
    let stop = env.lil(&["core", "stop"]);
    assert_eq!(stop.code, 0, "{stop:?}");
    assert_eq!(stop.stdout.trim(), "core stopped", "{stop:?}");
    drop(core);

    // With no core, `--no-start` stays silent even though the registry is stale (ARCH-018).
    let quiet = env.lil(&["--no-start", "call", "test.echo"]);
    assert_eq!(
        (quiet.code, quiet.stdout.as_str(), quiet.stderr.as_str()),
        (0, "", "")
    );
}

#[test] // spec: ARCH-018
fn without_a_core_stop_and_no_start_commands_start_nothing() {
    let env = TestEnv::new();

    let stop = env.lil(&["core", "stop"]);
    assert_eq!(stop.code, 0, "{stop:?}");
    assert_eq!(stop.stdout.trim(), "core not running", "{stop:?}");
    assert_eq!(stop.stderr, "", "{stop:?}");

    for args in [
        &["--no-start", "core", "stop"][..],
        &["events", "--follow", "--topic", "test:a", "--no-start"],
        &["--no-start", "call", "test.echo"],
    ] {
        let out = env.lil(args);
        assert_eq!(out.code, 0, "{args:?}: {out:?}");
        assert_eq!(out.stdout, "", "{args:?}: {out:?}");
        assert_eq!(out.stderr, "", "{args:?}: {out:?}");
    }

    let status = env.lil(&["--no-start", "status", "--json"]);
    assert_eq!(status.code, 3, "{status:?}");
    assert_eq!(stdout_json(&status), serde_json::json!({"running": false}));

    assert!(
        RawPipe::try_connect(&env, Duration::from_secs(1)).is_none(),
        "a command started a core"
    );
}

#[test] // spec: ARCH-027
fn untrusted_core_message_names_the_exe() {
    let env = TestEnv::with_label("Ünïcödé space");
    let other = env.root().join("other bin");
    std::fs::create_dir_all(&other).unwrap();
    let copy = other.join("lil-core.exe");
    std::fs::copy(&ensure_built().lil_core, &copy).unwrap();
    let mut fixture = CoreFixture::start_exe(&env, &copy);

    let out = env.lil(&["status"]);
    assert_eq!(out.code, 2, "{out:?}");
    assert_eq!(out.stdout, "", "{out:?}");
    assert!(
        path_forms(&copy).iter().any(|p| out.stderr.contains(p)),
        "{out:?}"
    );
    assert!(out.stderr.contains("lil core stop"), "{out:?}");

    // The hint works: `lil core stop` asks that core to finish.
    let stop = env.lil(&["core", "stop"]);
    assert_eq!(stop.code, 0, "{stop:?}");
    assert_eq!(stop.stdout.trim(), "core stopped", "{stop:?}");
    assert_eq!(fixture.wait_exit(DEADLINE), Some(0));
}

#[test] // spec: CORE-004
fn damaged_state_db_message_is_plain() {
    let env = TestEnv::with_label("Ünïcödé space");
    let db = env.env.suite_state_db();
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    std::fs::write(&db, vec![0xAB_u8; 4096]).unwrap();
    let before = sha256_file(&db);

    let out = env.lil(&["status"]);
    assert_eq!(out.code, 2, "{out:?}");
    assert_eq!(out.stdout, "", "{out:?}");
    assert_eq!(
        out.stderr.trim(),
        format!(
            "The core can't start: its state database at {} is damaged. Nothing was changed. `lil doctor` can set it aside.",
            db.display()
        ),
        "{out:?}"
    );
    assert_eq!(sha256_file(&db), before, "{} was changed", db.display());
}

#[test] // spec: ARCH-030, ARCH-016
fn an_unregistered_workspace_folder_is_named_plainly() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = env.make_workspace("Ünïcödé unregistered");
    let inside = ws.join("sub folder");
    std::fs::create_dir_all(&inside).unwrap();
    let id = lil_platform::Env::workspace_hash(&canonical(&ws));
    let names_folder = |text: &str| path_forms(&ws).iter().any(|p| text.contains(p));

    // `status` from inside it: one warning naming the folder, then the status without a
    // workspace, exit 0.
    let status = env.lil_in(&inside, &["status"]);
    assert_eq!(status.code, 0, "{status:?}");
    assert!(status.stdout.contains("\nworkspace: none\n"), "{status:?}");
    assert_eq!(status.stderr.lines().count(), 1, "{status:?}");
    assert!(names_folder(&status.stderr), "{status:?}");
    assert!(status.stderr.contains("suite.toml"), "{status:?}");

    // `call` and `events` still cannot run (exit 2), with a plain message: the folder, where
    // it came from and how to fix it; never just the workspace hash.
    let call = env.lil_in(&inside, &["call", "test.echo", "{\"text\":\"x\"}"]);
    let events = env.lil_in(&inside, &["events", "--follow", "--topic", "test:a"]);
    for out in [&call, &events] {
        assert_eq!(out.code, 2, "{out:?}");
        assert_eq!(out.stdout, "", "{out:?}");
        assert!(names_folder(&out.stderr), "{out:?}");
        assert!(out.stderr.contains("current folder"), "{out:?}");
        assert!(out.stderr.contains("suite.toml"), "{out:?}");
        assert!(out.stderr.contains("lil workspace use \""), "{out:?}");
        assert!(!out.stderr.contains(&id), "{out:?}");
    }

    // Named with --workspace, `status` does not fall back, and the folder is never told to
    // become the default: the message says which workspace the core serves.
    let ws_arg = ws.to_str().expect("a UTF-8 test path");
    let flagged = env.lil(&["status", "--workspace", ws_arg]);
    assert_eq!(flagged.code, 2, "{flagged:?}");
    assert!(names_folder(&flagged.stderr), "{flagged:?}");
    assert!(flagged.stderr.contains("--workspace"), "{flagged:?}");
    assert!(!flagged.stderr.contains("lil workspace use"), "{flagged:?}");
    assert!(
        flagged.stderr.contains("serves only the default workspace"),
        "{flagged:?}"
    );
}
