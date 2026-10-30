//! One instance per app, the hand-off of a second launch and the toast's Deny (APP-013, APP-014,
//! APP-020; Appendix G "Apps"; P4a-R9, P4a-R11), Qt-free, in two or more processes.
//!
//! **The probe.** Each launch is a copy of this test binary running its ignored `probe` test
//! (`LIL_QT_PROBE=1`), which does what `lil-image.exe` does before Qt starts, through the same
//! functions: `lil_qt::plan_boot` of the command line in `LIL_QT_PROBE_CMDLINE` (the raw command
//! line, exe first), then `lil_qt::single_instance::start`. A probe that would start Qt stands in
//! for the window instead: it writes `run <launch JSON>` and, when it claimed a pipe, `claimed
//! <shell | confirm>`, then serves it, writing `deliver <launch JSON>` for each forwarded launch
//! the window would get and `handled` for one it handled without the window (a deny), until a
//! `stop` file appears beside it. A probe that exits writes `exit <code> forwarded=<bool>`.
//! Each line goes to its own `LIL_QT_PROBE_OUT` file.
//!
//! **The deny's identity (ruling Q4).** A deny needs a verified `ui:*` caller: the copies are
//! named `lil-test-app.exe` beside a copy of `lil-core.exe`, whose core takes them for
//! `ui:test` on the test channel, as `e2e.rs` does.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use lil_client::ClientConfig;
use lil_platform::Env;
use lil_qt::single_instance::{ForwardError, Start, forward, route, start};
use lil_qt::{APP, plan_boot};
use lil_testkit::{CoreFixture, RawPipe, TestEnv, ensure_built};
use serde_json::{Value, json};

/// Set in a probe's environment.
const PROBE: &str = "LIL_QT_PROBE";
const PROBE_CMDLINE: &str = "LIL_QT_PROBE_CMDLINE";
const PROBE_OUT: &str = "LIL_QT_PROBE_OUT";
/// A failure deadline for each wait, never a measurement.
const DEADLINE: Duration = Duration::from_secs(30);
const CF: &str = "cf_0123456789abcdef";

// ---------------------------------------------------------------------------------------------
// The probe's side.
// ---------------------------------------------------------------------------------------------

fn write_out(line: &str) {
    let path = std::env::var_os(PROBE_OUT).expect("LIL_QT_PROBE_OUT");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("the probe's out file");
    writeln!(f, "{line}").expect("write the probe's out file");
}

fn deny_cfg(env: &Env) -> Option<ClientConfig> {
    ClientConfig::for_env(env.clone(), "ui:test".parse().ok()?).ok()
}

/// What `lil-image.exe` does before Qt starts, with a window that only writes what it gets.
#[test]
#[ignore = "probe"]
fn probe() {
    if std::env::var_os(PROBE).is_none() {
        return;
    }
    let env = Env::from_process().expect("the probe's channel");
    let _log = lil_platform::logs::init(&env, lil_qt::QT_ARGV0);
    let cmdline = std::env::var(PROBE_CMDLINE).expect("LIL_QT_PROBE_CMDLINE");
    let mut plan = plan_boot(&cmdline, &env);
    let started = start(&mut plan, &env, || deny_cfg(&env));
    let server = match started {
        Start::Exit(code) => {
            write_out(&format!("exit {code} forwarded={}", plan.forwarded));
            std::process::exit(code);
        }
        Start::Run(server) => server,
    };
    write_out(&format!(
        "run {}",
        lil_qt::args::launch_json(&plan, false).unwrap_or_default()
    ));
    let Some(server) = server else {
        std::process::exit(0);
    };
    let mode = match plan.mode {
        lil_qt::Mode::Shell => "shell",
        lil_qt::Mode::Confirm { .. } => "confirm",
    };
    write_out(&format!("claimed {mode}"));
    let stop = own_folder().join("stop");
    let until = Instant::now() + Duration::from_secs(120);
    while !stop.exists() && Instant::now() < until {
        while let Some(forwarded) = server.next() {
            let env2 = env.clone();
            match route(&forwarded, &env, move || deny_cfg(&env2)) {
                Some(p) => write_out(&format!(
                    "deliver {}",
                    lil_qt::args::launch_json(&p, true).unwrap_or_default()
                )),
                None => write_out("handled"),
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(server);
    // Give a deny's own thread time to end; its outcome is in the log.
    std::thread::sleep(Duration::from_millis(500));
    write_out("stopped");
    std::process::exit(0);
}

fn own_folder() -> PathBuf {
    std::env::current_exe()
        .expect("this exe")
        .parent()
        .expect("its folder")
        .to_path_buf()
}

// ---------------------------------------------------------------------------------------------
// The test's side.
// ---------------------------------------------------------------------------------------------

/// A folder holding a copy of this binary as `lil-test-app.exe` and a copy of `lil-core.exe`.
struct Folder {
    dir: tempfile::TempDir,
}

impl Folder {
    fn new() -> Folder {
        let dir = tempfile::Builder::new()
            .prefix("lil-qt-instance-")
            .tempdir()
            .expect("a folder for the copies");
        std::fs::copy(
            std::env::current_exe().unwrap(),
            dir.path().join("lil-test-app.exe"),
        )
        .expect("copy this test as lil-test-app.exe");
        std::fs::copy(&ensure_built().lil_core, dir.path().join("lil-core.exe"))
            .expect("copy lil-core.exe");
        Folder { dir }
    }

    fn exe(&self) -> PathBuf {
        self.dir.path().join("lil-test-app.exe")
    }

    fn stop(&self) {
        std::fs::write(self.dir.path().join("stop"), "").unwrap();
    }
}

/// One launch of the probe.
struct Launch {
    child: Child,
    out: PathBuf,
    started: Instant,
}

impl Launch {
    /// Starts the probe from `folder` with the command line `"<exe>" <rest>`.
    fn start(folder: &Folder, env: &TestEnv, rest: &str) -> Launch {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let out = folder.dir.path().join(format!("out-{n}.txt"));
        let exe = folder.exe();
        let cmdline = format!("\"{}\" {rest}", exe.display());
        let started = Instant::now();
        let child = Command::new(&exe)
            .args([
                "probe",
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .envs(env.child_env())
            .env(PROBE, "1")
            .env(PROBE_CMDLINE, cmdline)
            .env(PROBE_OUT, &out)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start the probe");
        Launch {
            child,
            out,
            started,
        }
    }

    fn lines(&self) -> Vec<String> {
        std::fs::read_to_string(&self.out)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Waits for a line starting `prefix`; returns the rest.
    fn wait_line(&self, prefix: &str) -> String {
        let until = Instant::now() + DEADLINE;
        loop {
            if let Some(line) = self.lines().iter().find(|l| l.starts_with(prefix)) {
                return line[prefix.len()..].trim().to_string();
            }
            assert!(
                Instant::now() < until,
                "no `{prefix}` line from the probe; it wrote {:?}",
                self.lines()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The `deliver` lines, as JSON.
    fn delivered(&self) -> Vec<Value> {
        self.lines()
            .iter()
            .filter_map(|l| l.strip_prefix("deliver "))
            .map(|j| serde_json::from_str(j).unwrap())
            .collect()
    }

    /// Waits for the exit: its code and how long after its start it came.
    fn wait_exit(&mut self) -> (i32, Duration) {
        let until = Instant::now() + DEADLINE;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return (status.code().unwrap_or(-1), self.started.elapsed());
            }
            assert!(
                Instant::now() < until,
                "the probe didn't exit: {:?}",
                self.lines()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Launch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn scheme(env: &TestEnv) -> String {
    env.env.activation_scheme()
}

fn task_url(env: &TestEnv, n: u8) -> String {
    format!("\"{}:task/j_00000000000000{n:02x}\"", scheme(env))
}

/// A running instance (the shell), claimed.
fn running(folder: &Folder, env: &TestEnv, rest: &str, mode: &str) -> Launch {
    let first = Launch::start(folder, env, rest);
    assert_eq!(first.wait_line("claimed "), mode, "{:?}", first.lines());
    first
}

#[test] // spec: APP-013, APP-014
fn a_second_launch_forwards_and_exits() {
    let env = TestEnv::new();
    let folder = Folder::new();
    let first = running(&folder, &env, "", "shell");
    assert_eq!(
        serde_json::from_str::<Value>(&first.wait_line("run ")).unwrap()["kind"],
        "activate"
    );
    let mut second = Launch::start(
        &folder,
        &env,
        &format!("--no-activate {}", task_url(&env, 1)),
    );
    let (code, took) = second.wait_exit();
    assert_eq!(code, 0, "{:?}", second.lines());
    assert!(
        took < Duration::from_secs(2),
        "the second launch took {took:?}"
    );
    assert_eq!(second.lines(), ["exit 0 forwarded=true"]);
    let got = first.wait_line("deliver ");
    let got: Value = serde_json::from_str(&got).unwrap();
    assert_eq!(got["kind"], "task", "{got}");
    assert_eq!(got["job"], "j_0000000000000001", "{got}");
    assert_eq!(got["focus"], false, "--no-activate came along: {got}");
    assert_eq!(got["activate"], true, "{got}");
    folder.stop();
}

#[test] // spec: APP-014
fn two_quick_second_launches_reach_one_window() {
    let env = TestEnv::new();
    let folder = Folder::new();
    let first = running(&folder, &env, "", "shell");
    let mut a = Launch::start(&folder, &env, &task_url(&env, 1));
    std::thread::sleep(Duration::from_millis(10));
    let mut b = Launch::start(&folder, &env, &task_url(&env, 2));
    assert_eq!(a.wait_exit().0, 0, "{:?}", a.lines());
    assert_eq!(b.wait_exit().0, 0, "{:?}", b.lines());
    for l in [&a, &b] {
        assert_eq!(l.lines(), ["exit 0 forwarded=true"], "no second window");
    }
    let until = Instant::now() + DEADLINE;
    while first.delivered().len() < 2 {
        assert!(Instant::now() < until, "{:?}", first.lines());
        std::thread::sleep(Duration::from_millis(10));
    }
    let jobs: Vec<Value> = first.delivered().iter().map(|d| d["job"].clone()).collect();
    assert_eq!(
        jobs,
        [json!("j_0000000000000001"), json!("j_0000000000000002")]
    );
    folder.stop();
}

#[test] // spec: APP-014, ARCH-027
fn forward_checks_the_server_image_path() {
    let env = TestEnv::new();
    let folder = Folder::new();
    let first = running(&folder, &env, "", "shell");
    // This process is another exe than the server's.
    let args = vec!["--no-activate".to_string()];
    match forward(&env.env, APP, false, &args) {
        Err(ForwardError::Untrusted) => {}
        other => panic!("expected Untrusted, got {other:?}"),
    }
    // A copy in another folder is another exe too: it starts without the pipe, never forwards.
    let other = Folder::new();
    let mut third = Launch::start(&other, &env, &task_url(&env, 3));
    assert_eq!(third.wait_exit().0, 0);
    assert!(
        third
            .lines()
            .iter()
            .all(|l| !l.starts_with("exit 0 forwarded=true"))
    );
    assert!(
        third.lines().iter().any(|l| l.starts_with("run ")),
        "{:?}",
        third.lines()
    );
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        first.delivered(),
        Vec::<Value>::new(),
        "nothing was written"
    );
    folder.stop();
}

#[test] // spec: APP-014
fn forward_gives_up_after_2_s() {
    let env = TestEnv::new();
    // A server of this same exe (so the image check passes) that never says hello.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let name = env.env.app_pipe_name(APP, false);
    let server = {
        let _in = rt.enter();
        lil_platform::pipe::create_instance(&name, true).unwrap()
    };
    rt.spawn(async move {
        let _ = server.connect().await;
        tokio::time::sleep(Duration::from_secs(10)).await;
        drop(server);
    });
    let started = Instant::now();
    let result = forward(&env.env, APP, false, &[]);
    let took = started.elapsed();
    assert!(matches!(result, Err(ForwardError::Timeout)), "{result:?}");
    assert!(
        took >= Duration::from_millis(1900) && took < Duration::from_secs(3),
        "{took:?}"
    );
    // With nothing running at all, it says so at once.
    let other = TestEnv::new();
    let started = Instant::now();
    assert!(matches!(
        forward(&other.env, APP, false, &[]),
        Err(ForwardError::NotRunning)
    ));
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[test] // spec: APP-020, P4a-R11
fn a_confirm_url_goes_to_the_running_full_app_first() {
    let env = TestEnv::new();
    let folder = Folder::new();
    let url = format!("\"{}:confirm/{CF}\"", scheme(&env));
    {
        let full = running(&folder, &env, "", "shell");
        let mut click = Launch::start(&folder, &env, &url);
        assert_eq!(click.wait_exit().0, 0);
        assert_eq!(click.lines(), ["exit 0 forwarded=true"]);
        let got: Value = serde_json::from_str(&full.wait_line("deliver ")).unwrap();
        assert_eq!(
            (got["kind"].as_str(), got["id"].as_str()),
            (Some("confirm"), Some(CF))
        );
        // No confirm window was made.
        assert!(matches!(
            forward(&env.env, APP, true, &[]),
            Err(ForwardError::NotRunning)
        ));
        folder.stop();
        drop(full);
    }
    let _ = std::fs::remove_file(folder.dir.path().join("stop"));
    // Without the full app, the click opens the confirm window.
    let window = running(&folder, &env, &url, "confirm");
    let run: Value = serde_json::from_str(&window.wait_line("run ")).unwrap();
    assert_eq!(
        (run["kind"].as_str(), run["id"].as_str()),
        (Some("confirm"), Some(CF))
    );
    folder.stop();
}

#[test] // spec: P4a-R11
fn the_confirm_window_has_its_own_pipe() {
    let env = TestEnv::new();
    let folder = Folder::new();
    assert_ne!(
        env.env.app_pipe_name(APP, true),
        env.env.app_pipe_name(APP, false)
    );
    assert!(
        env.env
            .app_pipe_name(APP, true)
            .contains(r"\lilith-app-image-confirm-")
    );
    let window = running(&folder, &env, "--confirm", "confirm");
    // A second click reuses the confirm window.
    let mut click = Launch::start(&folder, &env, &format!("\"{}:confirm/{CF}\"", scheme(&env)));
    assert_eq!(click.wait_exit().0, 0);
    let got: Value = serde_json::from_str(&window.wait_line("deliver ")).unwrap();
    assert_eq!(got["id"], CF);
    // The app itself still starts, on its own pipe.
    let full = running(&folder, &env, "", "shell");
    assert_eq!(window.delivered().len(), 1);
    drop(full);
    folder.stop();
}

// ---------------------------------------------------------------------------------------------
// The deny (P4a-R9).
// ---------------------------------------------------------------------------------------------

static KEY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn key() -> String {
    format!(
        "instance-{}-{}",
        std::process::id(),
        KEY.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

fn cli(env: &TestEnv) -> RawPipe {
    let mut pipe = RawPipe::connect(env);
    let hello = pipe.hello(json!({"workspace": "none"}));
    assert_eq!(hello["result"]["status"], "ok", "{hello}");
    pipe
}

fn write_ok(pipe: &mut RawPipe, name: &str, args: Value) -> Value {
    let out = pipe.tool(name, args.clone(), Some(&key()));
    assert_eq!(out["status"], "ok", "{name} {args}: {out}");
    out["structured"].clone()
}

/// A pending confirmation of `test.destructive` (with `k`) and its toast's Deny URL.
fn pending_with_toast(pipe: &mut RawPipe, env: &TestEnv, k: &str) -> (String, String) {
    let out = pipe.tool("test.destructive", json!({}), Some(k));
    assert_eq!(out["status"], "pending_confirmation", "{out}");
    let id = out["confirmation_id"].as_str().unwrap().to_string();
    let prefix = format!("{}:deny/{id}/", scheme(env));
    let until = Instant::now() + DEADLINE;
    loop {
        let seen = write_ok(pipe, "test.desktop", json!({}));
        let xml = seen["toasts"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["op"] == "post" && t["tag"] == id.as_str())
            .filter_map(|t| t["xml"].as_str().map(str::to_string))
            .next();
        if let Some(xml) = xml
            && let Some(at) = xml.find(&prefix)
        {
            let token = &xml[at + prefix.len()..at + prefix.len() + 32];
            return (id, format!("{prefix}{token}"));
        }
        assert!(
            Instant::now() < until,
            "no confirmation toast for {id}: {seen}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn retried_code(pipe: &mut RawPipe, k: &str) -> String {
    let out = pipe.tool("test.destructive", json!({}), Some(k));
    out["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Every line of the channel's logs.
fn log_lines(env: &TestEnv) -> Vec<String> {
    let mut lines = Vec::new();
    for entry in std::fs::read_dir(env.env.logs_dir())
        .into_iter()
        .flatten()
        .flatten()
    {
        if let Ok(text) = std::fs::read_to_string(entry.path()) {
            lines.extend(text.lines().map(str::to_string));
        }
    }
    lines
}

fn deny_lines(env: &TestEnv, id: &str) -> Vec<String> {
    log_lines(env)
        .into_iter()
        .filter(|l| l.contains("a notification's deny") && l.contains(&format!("deny/{id}/")))
        .collect()
}

fn wait_deny_lines(env: &TestEnv, id: &str) -> Vec<String> {
    let until = Instant::now() + DEADLINE;
    loop {
        let lines = deny_lines(env, id);
        if !lines.is_empty() {
            // Any second line would be there by now.
            std::thread::sleep(Duration::from_millis(300));
            return deny_lines(env, id);
        }
        assert!(Instant::now() < until, "no deny log line for {id}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test] // spec: APP-020, APP-022, P4a-R9, P4a-R11
fn deny_url_opens_no_window_and_answers_once() {
    let env = TestEnv::new();
    let folder = Folder::new();
    let core = CoreFixture::start_exe(&env, &folder.dir.path().join("lil-core.exe"));
    let mut pipe = cli(&env);
    write_ok(
        &mut pipe,
        "test.desktop",
        json!({"toasts": "ready", "shortcut": true, "handlers": true, "foreground": "other",
               "fullscreen": "no", "clear": true}),
    );

    // The app runs: the deny is forwarded to it, sent once, and its window never hears of it.
    let k = key();
    let (id, deny) = pending_with_toast(&mut pipe, &env, &k);
    let token = deny.rsplit('/').next().unwrap().to_string();
    let app = running(&folder, &env, "", "shell");
    let mut click = Launch::start(&folder, &env, &format!("\"{deny}\""));
    assert_eq!(click.wait_exit().0, 0);
    assert_eq!(click.lines(), ["exit 0 forwarded=true"]);
    app.wait_line("handled");
    assert_eq!(
        app.delivered(),
        Vec::<Value>::new(),
        "no window shown or activated"
    );
    let lines = wait_deny_lines(&env, &id);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("\"denied\""), "{}", lines[0]);
    assert_eq!(retried_code(&mut pipe, &k), "confirm.denied");
    // The same toast's Deny again: its token died; nothing shows, one more line.
    let mut again = Launch::start(&folder, &env, &format!("\"{deny}\""));
    assert_eq!(again.wait_exit().0, 0);
    let until = Instant::now() + DEADLINE;
    while deny_lines(&env, &id).len() < 2 {
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(deny_lines(&env, &id)[1].contains("confirm.unknown"));
    assert_eq!(app.delivered(), Vec::<Value>::new());
    folder.stop();
    drop(app);
    let _ = std::fs::remove_file(folder.dir.path().join("stop"));

    // No app runs: this process sends it, before any window, and exits 0.
    let k2 = key();
    let (id2, deny2) = pending_with_toast(&mut pipe, &env, &k2);
    let mut alone = Launch::start(&folder, &env, &format!("\"{deny2}\""));
    assert_eq!(alone.wait_exit().0, 0);
    assert_eq!(
        alone.lines(),
        ["exit 0 forwarded=false"],
        "no window, no pipe"
    );
    let lines = wait_deny_lines(&env, &id2);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(retried_code(&mut pipe, &k2), "confirm.denied");

    // An unknown token shows nothing and writes one line, the token cut.
    let k3 = key();
    let (id3, _) = pending_with_toast(&mut pipe, &env, &k3);
    let wrong = "0123456789abcdef0123456789abcdef";
    let mut unknown = Launch::start(
        &folder,
        &env,
        &format!("{}:deny/{id3}/{wrong}", scheme(&env)),
    );
    assert_eq!(unknown.wait_exit().0, 0);
    let lines = wait_deny_lines(&env, &id3);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("confirm.unknown"), "{}", lines[0]);
    assert!(lines[0].contains(&format!("deny/{id3}/…")), "{}", lines[0]);

    // The core stops: a stale toast's Deny starts no core.
    drop(pipe);
    drop(core);
    assert!(RawPipe::try_connect(&env, Duration::from_millis(200)).is_none());
    let mut stale = Launch::start(&folder, &env, &format!("\"{deny2}\""));
    assert_eq!(stale.wait_exit().0, 0);
    let lines = wait_deny_lines(&env, &id2);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[1].contains("no_core"), "{}", lines[1]);
    assert!(
        RawPipe::try_connect(&env, Duration::from_millis(500)).is_none(),
        "no core was started"
    );

    // No log line anywhere holds a token.
    for line in log_lines(&env) {
        assert!(!line.contains(&token) && !line.contains(wrong), "{line}");
        assert!(
            !deny2.rsplit('/').next().is_some_and(|t| line.contains(t)),
            "{line}"
        );
    }
}
