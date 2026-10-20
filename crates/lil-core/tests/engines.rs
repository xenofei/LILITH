//! The `engines` module (ARCH-021, ARCH-051, ARCH-059, ARCH-060, ARCH-063, ARCH-064, ARCH-068,
//! ARCH-071, ARCH-119, CORE-069, CORE-108, CORE-131): owned and attached ComfyUI, the loopback
//! launch and check, the scrubbed environment, the engine processes, output capture, the idle
//! free, the shutdown stop, the engine tools and node installs.
//!
//! Owned engines are the `lil-fake-comfyui` binary (child mode: a `fake-child` file in its
//! folder), attached ones the in-process `FakeComfy` or that binary started by the test, node
//! installs `lil-fake-comfy-cli`; nothing real is started (QA-038). In-process tests drive an
//! `Engines` over a manual telemetry and the fake clock; the end-to-end ones use an in-process
//! core or the test build of `lil-core`. Wall-clock durations here are failure deadlines and
//! poll pauses only, never measurements (QA-031).

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use lil_core::config::ScriptedSockets;
use lil_core::engines::comfyui::install::{self, ExactVersion, INSTALL_TOOL, NodeId};
use lil_core::engines::comfyui::{ComfyClient, ComfyError};
use lil_core::engines::tools::{EnginesSlot, register_engine_tools};
use lil_core::engines::{ComfyConfig, EngineClass, EngineState, Engines, Listen, scrubbed_env};
use lil_core::events::EventBus;
use lil_core::jobs::{
    EngineKind, JobCaller, JobId, JobStatus, JobSummary, Jobs, Priority, SubmittedSlot,
};
use lil_core::meter::{UsageCtx, UsageDraft, UsageSink, UsageStatus};
use lil_core::state::StateRegistry;
use lil_core::store::{FailPoints, fsops};
use lil_core::telemetry::{HwSources, PowerSettings, Telemetry, TickMode};
use lil_core::tools::{BillingBasis, Registry};
use lil_core::workspace::WorkspaceHub;
use lil_core::{CoreConfig, start_in_thread};
use lil_platform::fs::SystemProbe;
use lil_platform::process::{ProcessInspector, SystemInspector};
use lil_platform::sockets::{OsSockets, SocketTable};
use lil_platform::{Clock, IdSource};
use lil_protocol::{CallerKind, EffectClass};
use lil_testkit::{
    CoreFixture, FakeClock, FakeComfy, RawPipe, SeqIds, TestApp, TestEnv, ensure_built,
};
use serde_json::{Value, json};

/// How long a test waits for something before it fails (never a pass condition).
const DEADLINE: Duration = Duration::from_secs(30);
/// Pause between polls of a state that changes on its own.
const POLL: Duration = Duration::from_millis(25);

// ---------------------------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------------------------

/// `path` as UTF-8 for a TOML literal string.
fn path_str(path: &Path) -> String {
    path.to_str().expect("a UTF-8 test path").to_string()
}

/// A new ComfyUI folder `name` under the test root, holding an empty `main.py` (and
/// `fake-child`, or `fake-child` and `fake-child-any`, in child mode).
fn comfy_root(env: &TestEnv, name: &str, child: bool, any: bool) -> PathBuf {
    let root = env.root().join(name);
    lil_testkit::files::write_file(&root.join("main.py"), b"");
    if child {
        lil_testkit::files::write_file(&root.join("fake-child"), b"");
    }
    if any {
        lil_testkit::files::write_file(&root.join("fake-child-any"), b"");
    }
    root
}

/// `[engines.comfyui]` for an owned fake in `root` on a free port, plus `extra` lines.
fn own(env: &TestEnv, root: &Path, extra: &str) -> u16 {
    let port = lil_testkit::free_loopback_port();
    env.suite_toml_set(&format!(
        "[engines.comfyui]\nroot = '{}'\npython = '{}'\nport = {port}\nautostart = false\n{extra}",
        path_str(root),
        path_str(&ensure_built().fake_comfyui),
    ));
    port
}

/// `[engines.comfyui]` for an attached engine on `port`, with `root`, comfy-cli and a Python.
fn attach(env: &TestEnv, port: u16, root: &Path) {
    env.suite_toml_set(&format!(
        "[engines.comfyui]\nport = {port}\nroot = '{}'\nautostart = false\ncomfy_cli = '{}'\npython = '{}'\n",
        path_str(root),
        path_str(&ensure_built().fake_comfy_cli),
        path_str(&ensure_built().fake_comfyui),
    ));
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Polls `f` (off any runtime) until it returns `Some`.
fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "{what}: not within {DEADLINE:?}");
        std::thread::sleep(POLL);
    }
}

/// What `lil-fake-comfyui` recorded in `file` once it holds `key`.
fn recorded(file: &Path, key: &str) -> Value {
    poll(&format!("{} with {key}", file.display()), || {
        read_json(file).filter(|v| v.get(key).is_some())
    })
}

fn pid_of(v: &Value) -> u32 {
    u32::try_from(v["pid"].as_u64().expect("a pid")).unwrap()
}

/// Whether process `pid` still runs.
fn alive(pid: u32) -> bool {
    lil_platform::process::open(pid).is_ok_and(|p| matches!(p.exit_code(), Ok(None)))
}

fn gone(pid: u32) {
    poll(&format!("process {pid} gone"), || {
        (!alive(pid)).then_some(())
    });
}

fn is_conhost(pid: u32) -> bool {
    SystemInspector.inspect(pid).is_ok_and(|info| {
        info.image
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("conhost.exe"))
    })
}

/// The requests the fake binary in `root` kept, as `(method, path, body)`.
fn kept_requests(root: &Path) -> Vec<(String, String, Value)> {
    read_json(&root.join("fake-comfyui-requests.json"))
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .map(|r| {
            (
                r["method"].as_str().unwrap_or_default().to_string(),
                r["path"].as_str().unwrap_or_default().to_string(),
                r["body"].clone(),
            )
        })
        .collect()
}

/// Sends `GET <marker>` to the fake on `port` and returns the requests the fake in `root` kept
/// up to it: everything asked before the marker is in that list, so a request missing from it
/// was never sent.
fn requests_up_to_marker(root: &Path, port: u16, marker: &str) -> Vec<(String, String, Value)> {
    use std::io::{Read as _, Write as _};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "GET {marker} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut answer = Vec::new();
    let _ = stream.read_to_end(&mut answer);
    poll("the marker request kept", || {
        let kept = kept_requests(root);
        let at = kept.iter().position(|(_, path, _)| path == marker)?;
        Some(kept[..at].to_vec())
    })
}

/// Reads the real socket table and remembers which processes were asked about.
struct Recorded {
    real: Arc<dyn SocketTable>,
    asked: Mutex<Vec<u32>>,
    /// Every socket is reported on `0.0.0.0` instead of its own address.
    wildcard: AtomicBool,
}

impl Recorded {
    fn new(real: Arc<dyn SocketTable>, wildcard: bool) -> Arc<Recorded> {
        Arc::new(Recorded {
            real,
            asked: Mutex::default(),
            wildcard: AtomicBool::new(wildcard),
        })
    }

    fn asked(&self) -> Vec<u32> {
        self.asked.lock().unwrap().clone()
    }
}

impl SocketTable for Recorded {
    fn listening(&self, pid: u32) -> std::io::Result<Vec<SocketAddr>> {
        self.asked.lock().unwrap().push(pid);
        let addrs = self.real.listening(pid)?;
        Ok(if self.wildcard.load(Ordering::SeqCst) {
            addrs
                .into_iter()
                .map(|a| SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), a.port()))
                .collect()
        } else {
            addrs
        })
    }
}

/// Keeps every usage draft.
#[derive(Default)]
struct Usage(Mutex<Vec<(UsageDraft, UsageCtx)>>);

impl UsageSink for Usage {
    fn record<'a>(&'a self, u: UsageDraft, ctx: UsageCtx) -> BoxFuture<'a, ()> {
        self.0.lock().unwrap().push((u, ctx));
        Box::pin(std::future::ready(()))
    }
}

/// The jobs, a manual telemetry and the fake clock; [`Rig::engines`] makes the engines.
struct Rig {
    jobs: Arc<Jobs>,
    telemetry: Arc<Telemetry>,
    events: Arc<EventBus>,
    clock: Arc<FakeClock>,
    usage: Arc<Usage>,
    _states: Arc<StateRegistry>,
    /// Last: the folder goes only once nothing holds a file in it.
    env: TestEnv,
}

impl Rig {
    fn new(env: TestEnv) -> Rig {
        let states = Arc::new(
            StateRegistry::open(&env.env, Arc::new(|what: &str| panic!("fatal: {what}"))).unwrap(),
        );
        let clock = FakeClock::new();
        let dyn_clock: Arc<dyn Clock> = clock.clone();
        let events = Arc::new(EventBus::new(dyn_clock.clone()));
        let ids: Arc<dyn IdSource> = Arc::new(SeqIds::new());
        let hub = WorkspaceHub::new(
            env.env.clone(),
            states.clone(),
            events.clone(),
            dyn_clock.clone(),
            ids.clone(),
            FailPoints::none(),
            Arc::new(SystemProbe),
            lil_core::watch::WatchConfig::default(),
        );
        let jobs = Jobs::new(
            states.clone(),
            hub,
            events.clone(),
            dyn_clock.clone(),
            ids,
            FailPoints::none(),
        );
        let telemetry = Telemetry::start(
            HwSources::none(),
            env.env.local_dir().join("telemetry.db"),
            dyn_clock,
            PowerSettings::default(),
            events.clone(),
            TickMode::Manual,
        );
        Rig {
            jobs,
            telemetry,
            events,
            clock,
            usage: Arc::default(),
            _states: states,
            env,
        }
    }

    /// Engines over the `suite.toml` written so far, reading sockets from `sockets`.
    fn engines(&self, sockets: Arc<dyn SocketTable>) -> Arc<Engines> {
        Engines::new(
            self.env.env.clone(),
            self.jobs.clone(),
            self.telemetry.clone(),
            self.events.clone(),
            self.clock.clone(),
            FailPoints::none(),
            self.usage.clone(),
            sockets,
        )
    }

    fn caller(&self) -> JobCaller {
        JobCaller {
            caller: CallerKind::Cli,
            idem_scope: "cli|none".into(),
            workspace: None,
            trace_id: "t_engines".into(),
            idem_key: None,
            call_id: "c_0000000000000001".into(),
            reservation: None,
            submitted: Arc::new(SubmittedSlot::new()),
            origin: None,
            retry_of: None,
        }
    }

    /// Submits an install of `id` at `version` and waits until it is done.
    async fn install(&self, id: &str, version: &str) -> JobSummary {
        let spec = install::job_spec(
            &NodeId(id.into()),
            &ExactVersion(version.into()),
            "cli",
            Priority::Agent,
            None,
        );
        let caller = self.caller();
        let job = self.jobs.submit(&caller, spec).await.unwrap();
        let (job, done) = self
            .jobs
            .wait(&caller, &JobId(job.job_id), DEADLINE)
            .await
            .unwrap();
        assert!(done, "the install did not end: {job:?}");
        job
    }

    fn tracked(&self) -> BTreeSet<u32> {
        self.telemetry
            .tracked()
            .into_iter()
            .filter(|(_, role)| *role == "comfyui")
            .map(|(pid, _)| pid)
            .collect()
    }
}

/// Polls (on the runtime) until `f` holds.
async fn until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !f() {
        assert!(Instant::now() < deadline, "{what}: not within {DEADLINE:?}");
        tokio::time::sleep(POLL).await;
    }
}

async fn gone_async(pid: u32) {
    until(&format!("process {pid} gone"), || !alive(pid)).await;
}

/// A `cli` connection to `env`'s core with no workspace.
fn session(env: &TestEnv) -> RawPipe {
    let mut pipe = RawPipe::connect(env);
    let hello = pipe.hello(json!({"workspace": "none"}));
    assert_eq!(hello["result"]["status"], "ok", "{hello}");
    pipe
}

fn key() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    format!(
        "engines-{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// The ComfyUI entry of `engines.status` once `pred` holds for it.
fn until_engine(pipe: &mut RawPipe, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
    poll(what, || {
        let out = pipe.tool("engines.status", json!({}), None);
        let engine = out["structured"]["engines"][0].clone();
        pred(&engine).then_some(engine)
    })
}

/// An in-process core on `env` with the fake clock and ids, and `sockets`.
fn core_with(env: &TestEnv, sockets: Option<Arc<dyn SocketTable>>) -> lil_core::CoreThread {
    let base = CoreConfig::for_env(env.env.clone()).expect("core config");
    start_in_thread(CoreConfig {
        clock: FakeClock::new(),
        ids: Arc::new(SeqIds::new()),
        test_parent_pid: None,
        sockets: sockets.unwrap_or_else(|| base.sockets.clone()),
        ..base
    })
}

// ---------------------------------------------------------------------------------------------
// Launch, loopback and environment (ARCH-063, ARCH-068).
// ---------------------------------------------------------------------------------------------

#[test] // spec: ARCH-068
fn launch_args_pin_loopback_and_refuse_listen_in_extra_args() {
    let text = |extra: &str| {
        format!("[engines.comfyui]\nroot = 'C:\\comfy'\nport = 8190\nextra_args = {extra}\n")
    };
    let config = ComfyConfig::from_suite_toml(&text(r#"["--lowvram"]"#))
        .unwrap()
        .unwrap();
    let args: Vec<String> = config
        .launch_args()
        .unwrap()
        .into_iter()
        .map(|a| a.into_string().unwrap())
        .collect();
    assert_eq!(
        args,
        [
            "main.py",
            "--listen",
            "127.0.0.1",
            "--port",
            "8190",
            "--lowvram"
        ]
    );
    for extra in [
        r#"["--listen", "0.0.0.0"]"#,
        r#"["--listen=0.0.0.0"]"#,
        r#"["--lowvram", "--listen"]"#,
        r#"["--lis", "0.0.0.0"]"#,
    ] {
        let config = ComfyConfig::from_suite_toml(&text(extra)).unwrap().unwrap();
        let refused = config.launch_args().unwrap_err();
        assert_eq!(refused.code, "comfyui.not_loopback", "{extra}");
    }
    // The port is the suite's too: another one in extra_args is refused before any start.
    for extra in [
        r#"["--port", "8191"]"#,
        r#"["--port=8191"]"#,
        r#"["--po", "8191"]"#,
    ] {
        let config = ComfyConfig::from_suite_toml(&text(extra)).unwrap().unwrap();
        let refused = config.launch_args().unwrap_err();
        assert_eq!(refused.code, "engine.not_configured", "{extra}");
    }
}

#[test] // spec: ARCH-063, QA-032
fn scrubbed_env_removes_every_key_pattern_and_keeps_path_and_hf_home() {
    let dropped = [
        "OPENAI_API_KEY",
        "my_apikey",
        "GITHUB_TOKEN",
        "AWS_SECRET_ACCESS_KEY",
        "DB_PASSWORD",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "ANTHROPIC_BASE_URL",
        "OPENROUTER_X",
        "NANOGPT_X",
        "GEMINI_X",
        "FAL_X",
        "CIVITAI_X",
        "COMFY_API_KEY_2",
        "COMFY_APIX",
        "DEEPSEEK_X",
        "MOONSHOT_X",
        "DASHSCOPE_X",
        "ZAI_X",
        "ZHIPUAI",
        "MINIMAX_X",
        "XAI_X",
        "MISTRAL_X",
        "REPLICATE_X",
        "LIL_TEST_ROOT",
        "claude_code_x",
        "HF_TOKEN",
        "HUGGING_FACE_HUB_TOKEN",
        // Ruling P3-R19: any name holding KEY, TOKEN, SECRET, PASSWORD, PASSWD, CREDENTIAL or
        // AUTH.
        "AZURE_OPENAI_KEY",
        "KAGGLE_KEY",
        "NGROK_AUTHTOKEN",
        "SMTP_PASSWD",
        "TOKENIZERS_PARALLELISM",
    ];
    let kept = [
        "PATH",
        "HF_HOME",
        "SystemRoot",
        "TEMP",
        "COMFY_HOME",
        "CUDA_VISIBLE_DEVICES",
    ];
    let vars = dropped
        .iter()
        .chain(kept.iter())
        .map(|name| (name.into(), "v".into()));
    let names: Vec<String> = scrubbed_env(vars)
        .into_iter()
        .map(|(name, _)| name.into_string().unwrap())
        .collect();
    assert_eq!(names, kept);
}

#[test] // spec: ARCH-068
fn client_refuses_non_loopback_urls() {
    for url in [
        "http://127.0.0.1:8188",
        "http://[::1]:8188",
        "http://localhost:8188",
    ] {
        let client = ComfyClient::new(url).unwrap_or_else(|e| panic!("{url}: {e:?}"));
        assert_eq!(client.url(), url);
    }
    // No request can be sent: there is no client.
    for url in [
        "http://192.168.1.5:8188",
        "http://example.com:8188",
        "http://evil.com@127.0.0.1:8188",
        "http://user:pw@127.0.0.1:8188",
    ] {
        let refused = ComfyClient::new(url).unwrap_err();
        assert_eq!(refused.code, "comfyui.not_loopback", "{url}");
    }
}

/// A "ComfyUI" on 127.0.0.1 that answers every request with a 307 to a listener on
/// 127.0.0.2: the client must not follow it (I1), so nothing reaches the second listener.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-068
async fn client_never_follows_a_redirect() {
    use std::io::{Read as _, Write as _};
    let elsewhere = std::net::TcpListener::bind("127.0.0.2:0").unwrap();
    elsewhere.set_nonblocking(true).unwrap();
    let target = elsewhere.local_addr().unwrap();
    let redirector = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = redirector.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for conn in redirector.incoming() {
            let Ok(mut conn) = conn else { break };
            let mut buf = [0_u8; 4096];
            let _ = conn.read(&mut buf);
            let answer = format!(
                "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{target}/system_stats\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = conn.write_all(answer.as_bytes());
        }
    });
    let client = ComfyClient::new(&format!("http://127.0.0.1:{port}")).unwrap();
    for answer in [
        client.system_stats().await.map(drop),
        client.free(true, true).await,
    ] {
        assert!(
            matches!(answer, Err(ComfyError::Protocol(_))),
            "a redirect is a protocol error: {answer:?}"
        );
    }
    let reached = elsewhere.accept();
    assert!(
        matches!(&reached, Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "the redirect was followed: {reached:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-068
async fn owned_start_verifies_every_listening_socket_is_loopback() {
    let env = TestEnv::new();
    let root = comfy_root(&env, "comfy-wildcard", false, false);
    own(&env, &root, "");
    let rig = Rig::new(env);
    // The fake listens on 127.0.0.1; the socket table reports it on 0.0.0.0.
    let sockets = Recorded::new(Arc::new(OsSockets), true);
    let engines = rig.engines(sockets.clone());

    let refused = engines.start(EngineKind::ComfyUi).await.unwrap_err();
    assert_eq!(refused.code, "comfyui.not_loopback", "{refused:?}");
    let launcher = *sockets.asked().first().expect("the sockets were checked");
    gone_async(launcher).await;
    let status = engines.status().await.remove(0);
    assert_eq!(
        (status.class, status.state),
        (EngineClass::None, EngineState::Stopped)
    );
    assert!(engines.engine_pids(EngineKind::ComfyUi).is_empty());
    assert!(rig.tracked().is_empty(), "{:?}", rig.tracked());
}

/// An owned engine that starts listening off loopback after its start (a custom node opening a
/// server) is found by the supervisor's re-check and stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-068
async fn owned_engine_listening_off_loopback_later_is_stopped() {
    let env = TestEnv::new();
    let root = comfy_root(&env, "comfy-later", false, false);
    own(&env, &root, "");
    let rig = Rig::new(env);
    let sockets = Recorded::new(Arc::new(OsSockets), false);
    let engines = rig.engines(sockets.clone());
    let started = engines.start(EngineKind::ComfyUi).await.unwrap();
    assert_eq!(started.listen, Listen::Loopback);
    let launcher = started.pid.unwrap();

    sockets.wildcard.store(true, Ordering::SeqCst);
    // The re-check runs at least every SOCKET_CHECK_EVERY (5) passes.
    for _ in 0..6 {
        engines.tick().await;
        if engines.status().await[0].state == EngineState::Stopped {
            break;
        }
    }
    let status = engines.status().await.remove(0);
    assert_eq!(
        (status.class, status.state),
        (EngineClass::None, EngineState::Stopped),
        "the exposed engine still runs"
    );
    gone_async(launcher).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-064, ARCH-068, ARCH-071
async fn owned_launcher_child_is_tracked_checked_and_killed() {
    let env = TestEnv::new();
    let root = comfy_root(&env, "comfy-child", true, false);
    own(&env, &root, "");
    let rig = Rig::new(env);
    let sockets = Recorded::new(Arc::new(OsSockets), false);
    let engines = rig.engines(sockets.clone());

    let status = engines.start(EngineKind::ComfyUi).await.unwrap();
    assert_eq!(
        (status.class, status.state, status.listen),
        (EngineClass::Owned, EngineState::Ready, Listen::Loopback)
    );
    let launcher = pid_of(&recorded(&root.join("fake-comfyui.json"), "pid"));
    let child = pid_of(&recorded(&root.join("fake-comfyui-child.json"), "listen"));
    assert_eq!(
        status.pid,
        Some(launcher),
        "the launcher is the engine's pid"
    );
    let pids = engines.engine_pids(EngineKind::ComfyUi);
    let engine: BTreeSet<u32> = pids.iter().copied().filter(|p| !is_conhost(*p)).collect();
    assert_eq!(engine, BTreeSet::from([launcher, child]), "{pids:?}");
    assert!(
        sockets.asked().contains(&child),
        "the loopback check covered the child that listens: {:?}",
        sockets.asked()
    );
    let tracked = rig.tracked();
    assert!(
        tracked.contains(&launcher) && tracked.contains(&child),
        "{tracked:?}"
    );

    let stopped = engines.stop(EngineKind::ComfyUi).await.unwrap();
    assert_eq!(stopped.state, EngineState::Stopped);
    gone_async(launcher).await;
    gone_async(child).await;
    assert!(rig.tracked().is_empty(), "{:?}", rig.tracked());

    // A child that listens on 0.0.0.0 whatever --listen says: refused, both gone.
    let any = comfy_root(&rig.env, "comfy-any", true, true);
    own(&rig.env, &any, "");
    let scripted = Recorded::new(
        Arc::new(ScriptedSockets {
            real: Arc::new(OsSockets),
            rows: any.join("fake-sockets.json"),
        }),
        false,
    );
    let exposed = rig.engines(scripted.clone());
    let refused = exposed.start(EngineKind::ComfyUi).await.unwrap_err();
    assert_eq!(refused.code, "comfyui.not_loopback", "{refused:?}");
    let launcher = pid_of(&recorded(&any.join("fake-comfyui.json"), "pid"));
    let rows = std::fs::read_to_string(any.join("fake-sockets.json")).unwrap();
    let child = pid_of(&serde_json::from_str(rows.lines().next().unwrap()).unwrap());
    gone_async(launcher).await;
    gone_async(child).await;
    drop(exposed);

    // The core's Job Object handle going (as when the core dies) ends both.
    own(&rig.env, &root, "");
    fsops::remove_file(&root.join("fake-comfyui.json")).unwrap();
    fsops::remove_file(&root.join("fake-comfyui-child.json")).unwrap();
    let again = rig.engines(Arc::new(OsSockets));
    again.start(EngineKind::ComfyUi).await.unwrap();
    let launcher = pid_of(&recorded(&root.join("fake-comfyui.json"), "pid"));
    let child = pid_of(&recorded(&root.join("fake-comfyui-child.json"), "listen"));
    drop(again);
    drop(engines);
    gone_async(launcher).await;
    gone_async(child).await;
}

// ---------------------------------------------------------------------------------------------
// Attached engines (ARCH-064).
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-064
async fn attached_when_something_already_answers() {
    let fake = FakeComfy::start();
    let env = TestEnv::new();
    attach(&env, fake.port(), &fake.root());
    let rig = Rig::new(env);
    let engines = rig.engines(Arc::new(OsSockets));
    engines.tick().await;

    let status = engines.status().await.remove(0);
    assert_eq!(
        (status.class, status.state, status.listen),
        (EngineClass::Attached, EngineState::Ready, Listen::Loopback)
    );
    assert_eq!(status.pid, Some(std::process::id()), "the listener");
    assert_eq!(status.version.as_deref(), Some("0.41.0"));
    // A start finds it and starts nothing; the suite never stops or restarts it.
    let started = engines.start(EngineKind::ComfyUi).await.unwrap();
    assert_eq!(started.class, EngineClass::Attached);
    let endpoint = engines.ensure_ready(EngineKind::ComfyUi).await.unwrap();
    assert_eq!(
        (endpoint.class, endpoint.url.as_str()),
        (EngineClass::Attached, fake.url().as_str())
    );
    for refused in [
        engines.stop(EngineKind::ComfyUi).await,
        engines.restart(EngineKind::ComfyUi).await,
    ] {
        assert_eq!(refused.unwrap_err().code, "engine.attached");
    }
    let posts: Vec<_> = fake
        .requests()
        .into_iter()
        .filter(|r| r.method != "GET")
        .collect();
    assert!(posts.is_empty(), "{posts:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-064, ARCH-071
async fn attached_engine_pids_are_the_listener_and_its_descendants() {
    let fake = FakeComfy::start();
    let env = TestEnv::new();
    attach(&env, fake.port(), &fake.root());
    // A process of this one (the listener), as ComfyUI's helpers are its own.
    let folder = env.root().join("descendant");
    lil_testkit::files::write_file(&folder.join("main.py"), b"");
    let mut descendant = std::process::Command::new(&ensure_built().fake_comfyui)
        .args([
            "main.py",
            "--port",
            &lil_testkit::free_loopback_port().to_string(),
        ])
        .current_dir(&folder)
        .spawn()
        .unwrap();
    let rig = Rig::new(env);
    let engines = rig.engines(Arc::new(OsSockets));

    let me = std::process::id();
    until("the listener and its descendant", || {
        let pids = engines.engine_pids(EngineKind::ComfyUi);
        pids.first() == Some(&me) && pids.contains(&descendant.id())
    })
    .await;
    let tracked = rig.tracked();
    assert!(tracked.contains(&me) && tracked.contains(&descendant.id()));
    let parent = SystemInspector.inspect(me).unwrap().parent_pid;
    if let Some(parent) = parent {
        assert!(
            !engines.engine_pids(EngineKind::ComfyUi).contains(&parent),
            "an ancestor is not an engine process"
        );
    }
    descendant.kill().unwrap();
    descendant.wait().unwrap();
}

#[test] // spec: ARCH-064, ARCH-021
fn attached_engine_survives_core_shutdown_untouched() {
    let env = TestEnv::new();
    // An attached ComfyUI the test starts (the core didn't).
    let root = comfy_root(&env, "comfy-outside", false, false);
    let port = lil_testkit::free_loopback_port();
    let mut outside = std::process::Command::new(&ensure_built().fake_comfyui)
        .args(["main.py", "--port", &port.to_string()])
        .current_dir(&root)
        .spawn()
        .unwrap();
    recorded(&root.join("fake-comfyui.json"), "listen");
    attach(&env, port, &root);
    let core = core_with(&env, None);
    {
        let mut pipe = session(&env);
        let engine = until_engine(&mut pipe, "attached", |e| e["class"] == "attached");
        assert_eq!(engine["pid"], outside.id(), "{engine}");
        // An agent's stop is destructive (confirmation); either way nothing is stopped.
        let stopped = pipe.tool("engines.stop", json!({"engine": "comfyui"}), Some(&key()));
        assert_eq!(stopped["status"], "error", "{stopped}");
    }
    core.core().request_drain();
    let report = core.wait_stopped(DEADLINE).expect("the core stopped");
    assert!(report.steps.contains(&"engines.stop"), "{:?}", report.steps);

    assert!(alive(outside.id()), "the attached ComfyUI was stopped");
    let before = requests_up_to_marker(&root, port, "/lil-test-marker");
    let touched: Vec<_> = before
        .iter()
        .filter(|(method, _, _)| method != "GET")
        .collect();
    assert!(
        touched.is_empty(),
        "the attached engine was posted to: {touched:?}"
    );
    outside.kill().unwrap();
    outside.wait().unwrap();
}

// ---------------------------------------------------------------------------------------------
// Output, idle and shutdown (ARCH-119, ARCH-021, ARCH-064).
// ---------------------------------------------------------------------------------------------

/// Every event's target and message.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<(String, String)>>>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Captured {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        let mut message = Message(String::new());
        event.record(&mut message);
        self.0
            .lock()
            .unwrap()
            .push((event.metadata().target().to_string(), message.0));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-119
async fn engine_output_lines_reach_the_core_log_with_the_engine_target() {
    use tracing_subscriber::layer::SubscriberExt as _;
    let captured = Captured::default();
    tracing::subscriber::set_global_default(tracing_subscriber::registry().with(captured.clone()))
        .expect("the only global subscriber of this test");
    let env = TestEnv::new();
    let root = comfy_root(&env, "comfy-log", false, false);
    let port = own(&env, &root, "");
    let rig = Rig::new(env);
    let engines = rig.engines(Arc::new(OsSockets));
    engines.start(EngineKind::ComfyUi).await.unwrap();

    let line = format!("To see the GUI go to: http://127.0.0.1:{port}");
    until("the engine's line in the log", || {
        captured
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|(target, message)| target == "engine.comfyui" && message.contains(&line))
    })
    .await;
    engines.stop(EngineKind::ComfyUi).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-064
async fn idle_owned_engine_gets_free_after_the_configured_time() {
    let env = TestEnv::new();
    let root = comfy_root(&env, "comfy-idle", false, false);
    let port = own(&env, &root, "idle_free_after_s = 300\n");
    let rig = Rig::new(env);
    let engines = rig.engines(Arc::new(OsSockets));
    engines.start(EngineKind::ComfyUi).await.unwrap();
    let frees = |requests: &[(String, String, Value)]| -> Vec<Value> {
        requests
            .iter()
            .filter(|(method, path, _)| method == "POST" && path.ends_with("/free"))
            .map(|(_, _, body)| body.clone())
            .collect()
    };

    rig.clock.advance(Duration::from_secs(299));
    engines.tick().await;
    let marker = |n: u32| format!("/lil-test-marker-{n}");
    let (root2, m1) = (root.clone(), marker(1));
    let early = tokio::task::spawn_blocking(move || requests_up_to_marker(&root2, port, &m1))
        .await
        .unwrap();
    assert!(frees(&early).is_empty(), "freed before 300 s: {early:?}");

    rig.clock.advance(Duration::from_secs(1));
    engines.tick().await;
    let (root2, m2) = (root.clone(), marker(2));
    let later = tokio::task::spawn_blocking(move || requests_up_to_marker(&root2, port, &m2))
        .await
        .unwrap();
    assert_eq!(
        frees(&later),
        [json!({"unload_models": true, "free_memory": true})],
        "{later:?}"
    );
    // Once only, until a job uses it again.
    rig.clock.advance(Duration::from_secs(600));
    engines.tick().await;
    let (root2, m3) = (root.clone(), marker(3));
    let after = tokio::task::spawn_blocking(move || requests_up_to_marker(&root2, port, &m3))
        .await
        .unwrap();
    assert_eq!(frees(&after).len(), 1, "{after:?}");
    engines.stop(EngineKind::ComfyUi).await.unwrap();
}

#[test] // spec: ARCH-021, ARCH-064
fn owned_engine_is_stopped_in_the_stop_engines_phase() {
    let env = TestEnv::new();
    let root = comfy_root(&env, "comfy-shutdown", true, false);
    own(&env, &root, "");
    let core = core_with(&env, None);
    let launcher = {
        let mut pipe = session(&env);
        let started = pipe.tool("engines.start", json!({"engine": "comfyui"}), Some(&key()));
        assert_eq!(started["status"], "ok", "{started}");
        assert_eq!(
            started["structured"]["engine"]["class"], "owned",
            "{started}"
        );
        u32::try_from(started["structured"]["engine"]["pid"].as_u64().unwrap()).unwrap()
    };
    let child = pid_of(&recorded(&root.join("fake-comfyui-child.json"), "listen"));
    core.core().request_drain();
    let report = core.wait_stopped(DEADLINE).expect("the core stopped");
    let at = |name: &str| report.steps.iter().position(|s| *s == name);
    assert!(
        matches!((at("jobs.persist_interrupted"), at("engines.stop")), (Some(a), Some(b)) if a < b),
        "{:?}",
        report.steps
    );
    // The step waited for them: they are gone once the shutdown is done.
    assert!(
        !alive(launcher) && !alive(child),
        "the owned engine outlived the shutdown"
    );
}

#[test] // spec: ARCH-064
fn owned_engine_dies_with_the_core() {
    let env = TestEnv::new();
    let root = comfy_root(&env, "comfy-crash", true, false);
    own(&env, &root, "");
    let core = CoreFixture::spawn_raw(&env);
    let (launcher, child) = {
        let mut pipe = session(&env);
        let started = pipe.tool("engines.start", json!({"engine": "comfyui"}), Some(&key()));
        assert_eq!(started["status"], "ok", "{started}");
        (
            pid_of(&recorded(&root.join("fake-comfyui.json"), "pid")),
            pid_of(&recorded(&root.join("fake-comfyui-child.json"), "listen")),
        )
    };
    // Killed as a crash would: no shutdown step runs; the Job Object's handle closes.
    drop(core);
    gone(launcher);
    gone(child);
}

// ---------------------------------------------------------------------------------------------
// Engine tools and node installs (CORE-108, CORE-131, ARCH-059, ARCH-060, CORE-069).
// ---------------------------------------------------------------------------------------------

#[test] // spec: ARCH-051, CORE-108
fn engine_tools_have_the_declared_effects() {
    let mut reg = Registry::new();
    register_engine_tools(&mut reg, &EnginesSlot::default());
    let effects: Vec<(String, EffectClass, String)> = reg
        .descriptors()
        .into_iter()
        .map(|d| (d.name, d.effect, d.module))
        .collect();
    let want = [
        ("engines.comfyui.install_node", EffectClass::Exec),
        ("engines.restart", EffectClass::Destructive),
        ("engines.start", EffectClass::Write),
        ("engines.status", EffectClass::Read),
        ("engines.stop", EffectClass::Destructive),
    ];
    assert_eq!(
        effects,
        want.map(|(name, effect)| (name.to_string(), effect, "engines".to_string()))
    );
    // Only an exact version and a registry id are accepted.
    let install = reg.get(INSTALL_TOOL).unwrap();
    assert!(
        install
            .validate(&json!({"id": "a-node", "version": "1.2.3"}))
            .is_ok()
    );
    for bad in [
        json!({"id": "a-node", "version": "latest"}),
        json!({"id": "a-node", "version": "1.2"}),
        json!({"id": "--help", "version": "1.2.3"}),
        json!({"id": "a-node", "version": "1.2.3", "extra": true}),
    ] {
        assert!(install.validate(&bad).is_err(), "{bad}");
    }
    // `EngineKind::Test` is never reachable from input.
    assert_eq!(EngineKind::parse("test"), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-059, CORE-131, CORE-069
async fn install_requires_an_exact_version_and_records_usage_and_restart_needed() {
    let fake = FakeComfy::start();
    let env = TestEnv::new();
    attach(&env, fake.port(), &fake.root());
    let rig = Rig::new(env);
    let engines = rig.engines(Arc::new(OsSockets));

    let job = rig.install("comfyui-test-node", "1.2.3").await;
    assert_eq!(job.status, JobStatus::Succeeded, "{job:?}");
    assert_eq!(job.kind, "comfy.install_node");
    let log = std::fs::read_to_string(fake.root().join("fake-comfy-cli.log")).unwrap();
    let argv: Vec<String> = serde_json::from_str(log.lines().last().unwrap()).unwrap();
    assert_eq!(
        argv,
        [
            "--skip-prompt",
            "--workspace",
            &path_str(&fake.root()),
            "node",
            "registry-install",
            "comfyui-test-node",
            "--version",
            "1.2.3"
        ]
    );
    let status = engines.status().await.remove(0);
    assert!(status.restart_needed, "{status:?}");

    let drafts = rig.usage.0.lock().unwrap().clone();
    assert_eq!(drafts.len(), 1, "{drafts:?}");
    let (draft, ctx) = &drafts[0];
    assert_eq!(
        (
            draft.tool.as_str(),
            draft.provider.as_str(),
            draft.basis,
            draft.units.get("requests")
        ),
        (
            "engines.comfyui.install_node",
            "comfy-registry",
            BillingBasis::FreeTier,
            Some(&json!(1))
        )
    );
    assert_eq!(ctx.job_id.as_deref(), Some(job.job_id.as_str()));
    assert_eq!(ctx.call_id, job.job_id);
    assert_eq!(ctx.caller_kind, "cli");
    // How it ended and how long it took (CORE-069).
    assert_eq!(draft.status, UsageStatus::Succeeded);
    assert!(draft.latency_ms.is_some_and(|ms| ms > 0), "{draft:?}");

    // ComfyUI's Python goes first on PATH, and the environment is the scrubbed one.
    let started = read_json(&fake.root().join("fake-comfy-cli.env.json")).unwrap();
    let python_dir = ensure_built().fake_comfyui.parent().unwrap().to_path_buf();
    assert_eq!(
        started["path_first"].as_str().map(PathBuf::from),
        Some(python_dir),
        "{started}"
    );
    let names: Vec<String> = started["names"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap().to_string())
        .collect();
    let kept: Vec<String> = scrubbed_env(names.iter().map(|n| (n.into(), "v".into())))
        .into_iter()
        .map(|(n, _)| n.into_string().unwrap())
        .collect();
    assert_eq!(kept, names, "a secret reached comfy-cli");

    // The attached ComfyUI goes away: one failed request clears nothing; 30 s without an
    // answer (the core's clock) means its owner stopped or restarted it.
    engines.tick().await;
    assert_eq!(engines.status().await[0].class, EngineClass::Attached);
    drop(fake);
    engines.tick().await;
    assert!(
        engines.status().await[0].restart_needed,
        "cleared by one failed request"
    );
    rig.clock.advance(Duration::from_secs(29));
    engines.tick().await;
    assert!(
        engines.status().await[0].restart_needed,
        "cleared before 30 s"
    );
    rig.clock.advance(Duration::from_secs(1));
    engines.tick().await;
    assert!(!engines.status().await[0].restart_needed);
}

/// A cancelled install is a `cancelled` usage record, timed in wall time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: CORE-069, CORE-131
async fn a_cancelled_install_records_cancelled_usage() {
    let fake = FakeComfy::start();
    let env = TestEnv::new();
    attach(&env, fake.port(), &fake.root());
    lil_testkit::files::write_file(&fake.root().join("HANG"), b"");
    let rig = Rig::new(env);
    let _engines = rig.engines(Arc::new(OsSockets));
    let spec = install::job_spec(
        &NodeId("comfyui-test-node".into()),
        &ExactVersion("1.2.3".into()),
        "cli",
        Priority::Agent,
        None,
    );
    let caller = rig.caller();
    let job = rig.jobs.submit(&caller, spec).await.unwrap();
    let log = fake.root().join("fake-comfy-cli.env.json");
    until("comfy-cli runs", || log.exists()).await;
    rig.jobs
        .cancel(&caller, &JobId(job.job_id.clone()))
        .await
        .unwrap();
    let (job, done) = rig
        .jobs
        .wait(&caller, &JobId(job.job_id), DEADLINE)
        .await
        .unwrap();
    assert!(done && job.status == JobStatus::Cancelled, "{job:?}");
    let drafts = rig.usage.0.lock().unwrap().clone();
    assert_eq!(drafts.len(), 1, "{drafts:?}");
    assert_eq!(drafts[0].0.status, UsageStatus::Cancelled);
    assert!(
        drafts[0].0.latency_ms.is_some_and(|ms| ms > 0),
        "{drafts:?}"
    );
}

/// An attached ComfyUI that a new process serves was restarted: `restart_needed` clears. A
/// failed request on the way clears nothing (ruling on N2 of the Task 11 re-review).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: CORE-131, ARCH-064
async fn restart_needed_clears_when_another_process_serves_the_attached_engine() {
    let env = TestEnv::new();
    let root = comfy_root(&env, "comfy-restarted", false, false);
    let port = lil_testkit::free_loopback_port();
    let serve = |root: &Path| {
        let child = std::process::Command::new(&ensure_built().fake_comfyui)
            .args(["main.py", "--port", &port.to_string()])
            .current_dir(root)
            .spawn()
            .unwrap();
        poll("the outside fake listens", || {
            read_json(&root.join("fake-comfyui.json"))
                .filter(|v| v["pid"] == child.id() && v.get("listen").is_some())
        });
        child
    };
    let mut first = serve(&root);
    attach(&env, port, &root);
    let rig = Rig::new(env);
    let engines = rig.engines(Arc::new(OsSockets));
    engines.tick().await;
    assert_eq!(engines.status().await[0].pid, Some(first.id()));
    engines.set_restart_needed(EngineKind::ComfyUi);

    first.kill().unwrap();
    first.wait().unwrap();
    engines.tick().await;
    assert!(
        engines.status().await[0].restart_needed,
        "cleared by one failed request"
    );

    let mut second = serve(&root);
    until("the new listener is seen", || {
        futures_util::FutureExt::now_or_never(engines.status())
            .is_some_and(|s| s[0].pid == Some(second.id()))
    })
    .await;
    assert!(!engines.status().await[0].restart_needed);
    second.kill().unwrap();
    second.wait().unwrap();
}

/// The jobs hold the GPU scheduler, which holds the engines; the engines hold the jobs only
/// weakly, so dropping the core's handles ends an owned engine (review M4).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-064
async fn engines_and_jobs_form_no_cycle() {
    let env = TestEnv::new();
    let root = comfy_root(&env, "comfy-cycle", false, false);
    own(&env, &root, "");
    let rig = Rig::new(env);
    let engines = rig.engines(Arc::new(OsSockets));
    rig.jobs
        .set_admission(lil_core::jobs::scheduler::GpuScheduler::new(
            rig.telemetry.clone(),
            engines.clone(),
        ));
    let launcher = engines
        .start(EngineKind::ComfyUi)
        .await
        .unwrap()
        .pid
        .unwrap();
    let jobs = Arc::downgrade(&rig.jobs);
    drop(engines);
    // Everything but the folder goes.
    let Rig {
        jobs: strong,
        telemetry,
        events,
        clock,
        usage,
        _states,
        env: _env,
    } = rig;
    drop((strong, telemetry, events, clock, usage, _states));
    until("the jobs are gone", || jobs.upgrade().is_none()).await;
    gone_async(launcher).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-060, CORE-131
async fn install_failure_is_comfyui_node_install_failed_with_untrusted_detail() {
    let fake = FakeComfy::start();
    let env = TestEnv::new();
    attach(&env, fake.port(), &fake.root());
    lil_testkit::files::write_file(&fake.root().join("FAIL"), b"");
    let rig = Rig::new(env);
    let engines = rig.engines(Arc::new(OsSockets));

    let job = rig.install("comfyui-test-node", "1.2.3").await;
    assert_eq!(job.status, JobStatus::Failed, "{job:?}");
    let error = job.error.expect("an error");
    assert_eq!(error.code, "comfyui.node_install_failed");
    let detail = error.detail.expect("comfy-cli's output");
    assert!(detail.untrusted);
    assert!(
        detail.text.contains("failing because FAIL exists"),
        "{detail:?}"
    );
    assert!(!engines.status().await[0].restart_needed);
    let drafts = rig.usage.0.lock().unwrap().clone();
    assert_eq!(drafts.len(), 1, "the registry was asked");
    assert_eq!(drafts[0].0.status, UsageStatus::Failed);
}

#[test] // spec: CORE-131, ARCH-052, ARCH-059
fn install_tool_runs_as_a_job_and_agents_need_a_confirmation() {
    let fake = FakeComfy::start();
    let env = TestEnv::new();
    attach(&env, fake.port(), &fake.root());
    // A secret in the core's environment never reaches comfy-cli (ARCH-063).
    let canary = ("MY_SERVICE_TOKEN", "engines-canary-token");
    let _core = CoreFixture::start_with_env(&env, &[canary]);
    let args = json!({"id": "comfyui-test-node", "version": "1.2.3"});

    let mut pipe = session(&env);
    let agent = pipe.tool(INSTALL_TOOL, args.clone(), Some(&key()));
    assert_eq!(agent["error"]["code"], "confirm.unavailable", "{agent}");
    let unknown = pipe.tool("engines.start", json!({"engine": "test"}), Some(&key()));
    assert_eq!(unknown["error"]["code"], "engine.unknown", "{unknown}");

    let mut app = TestApp::start(&env, None, &[]);
    let latest = app.tool(
        INSTALL_TOOL,
        json!({"id": "comfyui-test-node", "version": "latest"}),
        Some(&key()),
    );
    assert_eq!(latest["error"]["code"], "params.invalid", "{latest}");
    let submitted = app.tool(INSTALL_TOOL, args, Some(&key()));
    assert_eq!(submitted["status"], "ok", "{submitted}");
    let id = submitted["structured"]["job"]["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let waited = app.tool("job.wait", json!({"job_id": id, "timeout_s": 30}), None);
    assert_eq!(
        waited["structured"]["job"]["status"], "succeeded",
        "{waited}"
    );
    until_engine(&mut pipe, "restart_needed", |e| e["restart_needed"] == true);
    let started = read_json(&fake.root().join("fake-comfy-cli.env.json")).unwrap();
    let python_dir = ensure_built().fake_comfyui.parent().unwrap().to_path_buf();
    assert_eq!(
        started["path_first"].as_str().map(PathBuf::from),
        Some(python_dir),
        "{started}"
    );
    let names = started["names"].as_array().unwrap();
    assert!(!names.is_empty(), "{started}");
    assert!(
        !names
            .iter()
            .any(|n| n.as_str().is_some_and(|n| n.eq_ignore_ascii_case(canary.0))),
        "{} reached comfy-cli",
        canary.0
    );
}

#[test] // spec: CORE-130, ARCH-112
fn engine_file_cleanup_stays_in_output_and_input() {
    let env = TestEnv::new();
    let root = env.root().join("comfy");
    for name in [
        r"output\lil_a_00001_.png",
        r"input\lil_b.png",
        r"output\sub\c.png",
        "main.py",
    ] {
        lil_testkit::files::write_file(&root.join(name), b"x");
    }
    let outside = env.root().join("outside.png");
    lil_testkit::files::write_file(&outside, b"x");

    fsops::remove_engine_file(&root, &root.join(r"output\lil_a_00001_.png")).unwrap();
    fsops::remove_engine_file(&root, &root.join(r"INPUT\lil_b.png")).unwrap();
    fsops::remove_engine_file(&root, &root.join(r"output\sub\c.png")).unwrap();
    assert!(!root.join(r"output\lil_a_00001_.png").exists());
    assert!(!root.join(r"input\lil_b.png").exists());
    for refused in [
        root.join("main.py"),
        root.join("output"),
        root.join(r"output\..\main.py"),
        root.join(r"output\..\..\outside.png"),
        outside.clone(),
    ] {
        let e = fsops::remove_engine_file(&root, &refused).unwrap_err();
        assert_eq!(
            e.kind(),
            std::io::ErrorKind::PermissionDenied,
            "{}",
            refused.display()
        );
    }
    assert!(root.join("main.py").exists() && outside.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-071
async fn an_unload_that_never_answers_ends_as_failed() {
    use lil_core::jobs::scheduler::EngineControl;
    use std::io::Read as _;
    // A ComfyUI that takes every request and never answers.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = silent.local_addr().unwrap().port();
    let (seen_tx, seen_rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for conn in silent.incoming() {
            let Ok(mut conn) = conn else { break };
            let mut buf = [0_u8; 1024];
            let n = conn.read(&mut buf).unwrap_or(0);
            let _ = seen_tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
            held.push(conn);
        }
    });
    let env = TestEnv::new();
    let root = env.root().join("comfy-silent");
    attach(&env, port, &root);
    let rig = Rig::new(env);
    let engines = rig.engines(Arc::new(OsSockets));

    let confirmed = tokio::time::timeout(DEADLINE, engines.unload(EngineKind::ComfyUi))
        .await
        .expect("the unload never ended");
    assert!(!confirmed, "an unanswered /free is a failed unload");
    // The scheduler's unload ends the same way.
    tokio::time::timeout(
        DEADLINE,
        EngineControl::unload(engines.as_ref(), EngineKind::ComfyUi),
    )
    .await
    .expect("the scheduler's unload never ended");
    let deadline = Instant::now() + DEADLINE;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let request = seen_rx.recv_timeout(left).expect("no /free was sent");
        if request.starts_with("POST /free") {
            break;
        }
    }
}

/// The Task 9 carry: ComfyUI's models folder is measured with the workspace, and both sizes
/// appear together.
#[test] // spec: CORE-059, CORE-061
fn models_folder_size_reaches_the_snapshot() {
    let env = TestEnv::new();
    let root = env.root().join("comfy-models");
    lil_testkit::files::write_file(
        &root.join("models").join("m.safetensors"),
        &vec![1_u8; 2 << 20],
    );
    attach(&env, lil_testkit::free_loopback_port(), &root);
    let _core = core_with(&env, None);
    let mut pipe = session(&env);
    let sizes = poll("models_mib in the snapshot", || {
        let tick = pipe.tool("test.hw_tick", json!({"samples": 1}), Some(&key()));
        let sizes = tick["structured"]["sizes"].clone();
        sizes.get("models_mib").is_some().then_some(sizes)
    });
    assert_eq!(sizes["models_mib"], 2, "{sizes}");

    let ws = env.root().join("ws");
    lil_testkit::files::write_file(&ws.join("a.bin"), &vec![1_u8; 1 << 20]);
    let mut probe = lil_core::telemetry::sizes_probe(Some(ws), Some(root.join("models")));
    let both = poll("both sizes", || {
        let s = probe();
        (s != lil_core::telemetry::Sizes::default()).then_some(s)
    });
    assert_eq!((both.workspace_mib, both.models_mib), (Some(1), Some(2)));
}

/// An owned start that hangs (the fake with a `fake-silent` file never listens, as a ComfyUI
/// stuck while it starts), under way: the rig, the engines, the root and the launcher's pid,
/// with the start's own result to come on the handle.
type Start =
    tokio::task::JoinHandle<Result<lil_core::engines::EngineStatus, lil_protocol::ErrorBody>>;
async fn a_hanging_start(name: &str) -> (Rig, Arc<Engines>, PathBuf, u32, Start) {
    let env = TestEnv::new();
    let root = comfy_root(&env, name, false, false);
    lil_testkit::files::write_file(&root.join("fake-silent"), b"");
    own(&env, &root, "");
    let rig = Rig::new(env);
    let engines = rig.engines(Arc::new(OsSockets));
    let start = {
        let engines = engines.clone();
        tokio::spawn(async move { engines.start(EngineKind::ComfyUi).await })
    };
    let begun = Instant::now();
    let pid = loop {
        let status = engines.status().await.remove(0);
        // The fake has read its `fake-silent` switch once it wrote its record.
        if let (EngineState::Starting, Some(pid), true) = (
            status.state,
            status.pid,
            root.join("fake-comfyui.json").is_file(),
        ) {
            break pid;
        }
        assert!(
            begun.elapsed() < DEADLINE,
            "the start never began: {status:?}"
        );
        tokio::time::sleep(POLL).await;
    };
    (rig, engines, root, pid, start)
}

/// QA1 M12: `engines.stop` during an owned start that hangs cancels the start instead of
/// waiting up to its 600 s timeout. The start fails with `comfyui.start_failed`, its process
/// is gone, and the engine is stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-064
async fn stop_cancels_a_start_in_progress() {
    let (_rig, engines, _root, pid, start) = a_hanging_start("comfy-hang-stop").await;
    let stopped = tokio::time::timeout(DEADLINE, engines.stop(EngineKind::ComfyUi))
        .await
        .expect("the stop waited behind the start")
        .unwrap();
    assert_eq!(
        (stopped.class, stopped.state),
        (EngineClass::None, EngineState::Stopped)
    );
    let started = tokio::time::timeout(DEADLINE, start)
        .await
        .expect("the start did not end")
        .unwrap();
    let refused = started.expect_err("a stopped start fails");
    assert_eq!(refused.code, "comfyui.start_failed", "{refused:?}");
    gone_async(pid).await;
    assert!(engines.engine_pids(EngineKind::ComfyUi).is_empty());
}

/// QA1 M12 for `engines.restart`: it cancels a start that hangs, then starts the engine again
/// (here one that answers), without waiting for the first start's timeout.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)] // spec: ARCH-064
async fn restart_cancels_a_start_in_progress() {
    let (_rig, engines, root, pid, start) = a_hanging_start("comfy-hang-restart").await;
    fsops::remove_file(&root.join("fake-silent")).unwrap();
    let restarted = tokio::time::timeout(DEADLINE, engines.restart(EngineKind::ComfyUi))
        .await
        .expect("the restart waited behind the start")
        .unwrap();
    assert_eq!(
        (restarted.class, restarted.state),
        (EngineClass::Owned, EngineState::Ready)
    );
    assert_ne!(restarted.pid, Some(pid));
    let first = tokio::time::timeout(DEADLINE, start)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.unwrap_err().code, "comfyui.start_failed");
    gone_async(pid).await;
    engines.stop(EngineKind::ComfyUi).await.unwrap();
}
