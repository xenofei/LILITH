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
//! `stop` file appears beside it (a `closing` file first closes it, as the app's teardown does,
//! and it writes `closing`). A probe that exits writes `exit <code> forwarded=<bool>`.
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
use lil_qt::single_instance::{ForwardError, InstanceServer, Routed, Start, forward, route, start};
use lil_qt::{APP, plan_boot};
use lil_testkit::{CoreFixture, RawPipe, TestEnv, ensure_built};
use serde_json::{Value, json};

/// Set in a probe's environment.
const PROBE: &str = "LIL_QT_PROBE";
const PROBE_CMDLINE: &str = "LIL_QT_PROBE_CMDLINE";
const PROBE_OUT: &str = "LIL_QT_PROBE_OUT";
/// With a role, a probe isn't a launch: see `raw_pipe` and what follows it.
const PROBE_ROLE: &str = "LIL_QT_PROBE_ROLE";
const PROBE_SAYS: &str = "LIL_QT_PROBE_SAYS";
const PROBE_HOLDER: &str = "LIL_QT_PROBE_HOLDER";
const PROBE_HANDED: &str = "LIL_QT_PROBE_HANDED";
const PROBE_KEEP: &str = "LIL_QT_PROBE_KEEP";
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
    match std::env::var(PROBE_ROLE).as_deref() {
        Ok("creator") => pipe_creator(&env),
        Ok("holder") => pipe_holder(),
        Ok("says") => {
            let pipe = raw_pipe(&env.app_pipe_name(APP, false));
            write_out("ready");
            hold_pipe(pipe, std::env::var(PROBE_SAYS).ok());
        }
        _ => {}
    }
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
    let closing = own_folder().join("closing");
    let mut closed = false;
    let mut denies = Vec::new();
    let until = Instant::now() + Duration::from_secs(120);
    while !stop.exists() && Instant::now() < until {
        if !closed && closing.exists() {
            // As the app does when it starts to close: no launch is taken from now on.
            server.close();
            closed = true;
            write_out("closing");
        }
        while let Some(forwarded) = server.next() {
            let env2 = env.clone();
            match route(&forwarded, &env, move || deny_cfg(&env2)) {
                Routed::Show(p) => write_out(&format!(
                    "deliver {}",
                    lil_qt::args::launch_json(&p, true).unwrap_or_default()
                )),
                Routed::Deny(handle) => {
                    denies.push(handle);
                    write_out("handled");
                }
                Routed::Nothing => write_out("nothing"),
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(server);
    // A deny in flight is joined before the process ends (its outcome is in the log).
    for deny in denies {
        let _ = deny.join();
    }
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

// A pipe that isn't an app's own: `creator` makes it and hands its handle to a `holder`, and
// `says` does both and speaks as told (review finding 6: `GetNamedPipeServerProcessId` names the
// creator whoever holds the handle now). All blocking calls; a test's end kills them.

/// A first instance of `name`, as the app's, its handle not yet tied to any completion port.
fn raw_pipe(name: &str) -> windows::Win32::Foundation::HANDLE {
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
    use windows::Win32::System::Pipes::{
        CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    use windows::core::HSTRING;
    let user = lil_platform::process::current_user_sid().expect("this user's SID");
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: a NUL-terminated SDDL and a valid out-pointer; the descriptor is left to the
    // process's end (a test), and `attributes` outlives the call that reads it.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &HSTRING::from(lil_platform::pipe::pipe_sddl(&user)),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
        .expect("the pipe's descriptor");
        let attributes = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap(),
            lpSecurityDescriptor: sd.0,
            bInheritHandle: false.into(),
        };
        let pipe = CreateNamedPipeW(
            &HSTRING::from(name),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            65536,
            65536,
            0,
            Some(&raw const attributes),
        );
        assert!(!pipe.is_invalid(), "the pipe can't be made");
        pipe
    }
}

/// `creator`: makes the app's pipe, copies its handle into the holder and closes its own. It then
/// ends, or with `PROBE_KEEP` stays on, a copy of the app's exe that passes every check of a pid
/// without holding the pipe, as a copy that took a dead creator's pid does.
fn pipe_creator(env: &Env) {
    use windows::Win32::Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE};
    let holder: u32 = std::env::var(PROBE_HOLDER).unwrap().parse().unwrap();
    let pipe = raw_pipe(&env.app_pipe_name(APP, false));
    let mut copy = HANDLE::default();
    // SAFETY: every handle is this process's own and closed once, and `copy` is a valid
    // out-pointer; the copy belongs to the holder from here on.
    unsafe {
        let target = OpenProcess(PROCESS_DUP_HANDLE, false, holder).expect("the holder");
        DuplicateHandle(
            GetCurrentProcess(),
            pipe,
            target,
            &raw mut copy,
            0,
            false,
            DUPLICATE_SAME_ACCESS,
        )
        .expect("a copy of the pipe's handle");
        let _ = CloseHandle(target);
        let _ = CloseHandle(pipe);
    }
    std::fs::write(
        std::env::var_os(PROBE_HANDED).unwrap(),
        format!("{} {}", copy.0 as usize, std::process::id()),
    )
    .expect("the handed file");
    write_out("handed");
    while std::env::var_os(PROBE_KEEP).is_some() && !own_folder().join("stop").exists() {
        std::thread::sleep(Duration::from_millis(10));
    }
    std::process::exit(0);
}

/// `holder`: waits for the creator's handle, then holds the pipe.
fn pipe_holder() -> ! {
    use windows::Win32::Foundation::HANDLE;
    write_out("ready");
    let path = PathBuf::from(std::env::var_os(PROBE_HANDED).unwrap());
    let until = Instant::now() + DEADLINE;
    let handed = loop {
        if let Ok(text) = std::fs::read_to_string(&path)
            && !text.is_empty()
        {
            break text;
        }
        assert!(Instant::now() < until, "no handle was handed over");
        std::thread::sleep(Duration::from_millis(5));
    };
    let (handle, creator) = handed.split_once(' ').unwrap();
    let handle = HANDLE(handle.parse::<usize>().unwrap() as *mut _);
    let hello = format!("{{\"app\":\"{APP}\",\"pid\":{creator}}}");
    hold_pipe(handle, Some(hello));
}

/// Serves `pipe` as a hostile holder would, one client after another, writing what it gets to its
/// out file: `hello` when a client connects (none: says nothing), and to a proof it tries every
/// way it has to set the event (`proof-open <ok | code>`, and `proof-impersonated <ok | code>`
/// under the client's token), then says it did, whatever came of it. It takes arguments as the
/// app would, so what it got shows.
fn hold_pipe(pipe: windows::Win32::Foundation::HANDLE, hello: Option<String>) -> ! {
    use std::io::{BufRead as _, BufReader};
    use std::mem::ManuallyDrop;
    use std::os::windows::io::FromRawHandle as _;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Security::RevertToSelf;
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, DisconnectNamedPipe, ImpersonateNamedPipeClient,
    };
    use windows::Win32::System::Threading::{EVENT_MODIFY_STATE, OpenEventW, SetEvent};
    use windows::core::HSTRING;
    let code = |e: windows::core::Error| format!("{}", e.code().0 & 0xFFFF);
    let set = |name: &str| -> Result<(), windows::core::Error> {
        // SAFETY: the name outlives the call; the event is closed once.
        unsafe {
            let event = OpenEventW(EVENT_MODIFY_STATE, false, &HSTRING::from(name))?;
            let done = SetEvent(event);
            let _ = CloseHandle(event);
            done
        }
    };
    // SAFETY: the handle is this process's, and it outlives the file, which never closes it.
    let file = ManuallyDrop::new(unsafe { std::fs::File::from_raw_handle(pipe.0.cast::<_>()) });
    loop {
        // SAFETY: a synchronous call on an open pipe handle; "already connected" is no matter.
        let _ = unsafe { ConnectNamedPipe(pipe, None) };
        write_out("connected");
        if let Some(hello) = &hello {
            let mut writer: &std::fs::File = &file;
            let _ = writeln!(writer, "{hello}");
            let _ = writer.flush();
        }
        let mut reader = BufReader::new(&*file);
        let mut line = String::new();
        while matches!(reader.read_line(&mut line), Ok(n) if n > 0) {
            write_out(&format!("got {}", line.trim_end()));
            let message: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
            let mut writer: &std::fs::File = &file;
            if let Some(name) = message["prove"].as_str() {
                let direct = set(name);
                write_out(&format!(
                    "proof-open {}",
                    direct.map_or_else(code, |()| "ok".to_string())
                ));
                // SAFETY: the pipe is connected; this thread reverts before anything else.
                let impersonated = unsafe { ImpersonateNamedPipeClient(pipe) }.and_then(|()| {
                    let set = set(name);
                    // SAFETY: as above.
                    unsafe { RevertToSelf() }?;
                    set
                });
                write_out(&format!(
                    "proof-impersonated {}",
                    impersonated.map_or_else(code, |()| "ok".to_string())
                ));
                let _ = writeln!(writer, "{{\"proved\":true}}");
            } else if message.get("args").is_some() {
                let _ = writeln!(writer, "{{\"ok\":true}}");
            }
            let _ = writer.flush();
            line.clear();
        }
        write_out("eof");
        // SAFETY: an open pipe handle.
        let _ = unsafe { DisconnectNamedPipe(pipe) };
    }
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
        Self::start_with(folder, env, rest, &[])
    }

    /// [`Launch::start`] with more variables: a role's.
    fn start_with(
        folder: &Folder,
        env: &TestEnv,
        rest: &str,
        extra: &[(&str, std::ffi::OsString)],
    ) -> Launch {
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
            .envs(extra.iter().map(|(k, v)| (*k, v)))
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
    // Which of the two connects first is up to the scheduler; the order of connections is
    // `the_instance_keeps_the_order_of_its_launches`'s.
    let mut jobs: Vec<String> = first
        .delivered()
        .iter()
        .map(|d| d["job"].as_str().unwrap_or_default().to_string())
        .collect();
    jobs.sort();
    assert_eq!(jobs, ["j_0000000000000001", "j_0000000000000002"]);
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
fn with_nothing_running_a_forward_says_so_at_once() {
    let env = TestEnv::new();
    let started = Instant::now();
    assert!(matches!(
        forward(&env.env, APP, false, &[]),
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

// ---------------------------------------------------------------------------------------------
// Order, teardown, threads and a held pipe (review M-5, I-3, I-2).
// ---------------------------------------------------------------------------------------------

#[test] // spec: APP-014
fn the_instance_keeps_the_order_of_its_launches() {
    // Each launch connects only after the one before was answered.
    let env = TestEnv::new();
    let folder = Folder::new();
    let first = running(&folder, &env, "", "shell");
    for n in 1..=3 {
        let mut launch = Launch::start(&folder, &env, &task_url(&env, n));
        assert_eq!(launch.wait_exit().0, 0, "{:?}", launch.lines());
    }
    let until = Instant::now() + DEADLINE;
    while first.delivered().len() < 3 {
        assert!(Instant::now() < until, "{:?}", first.lines());
        std::thread::sleep(Duration::from_millis(10));
    }
    let got: Vec<String> = first
        .delivered()
        .iter()
        .map(|d| d["job"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        got,
        [
            "j_0000000000000001",
            "j_0000000000000002",
            "j_0000000000000003"
        ]
    );
    folder.stop();
}

#[test] // spec: APP-013, APP-014
fn a_launch_while_the_app_closes_becomes_its_own_instance() {
    let env = TestEnv::new();
    let folder = Folder::new();
    let first = running(&folder, &env, "", "shell");
    std::fs::write(folder.dir.path().join("closing"), "").unwrap();
    first.wait_line("closing");
    // The launch is told "closing" and waits for the first instance to go.
    let mut second = Launch::start(&folder, &env, &task_url(&env, 7));
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        second.child.try_wait().unwrap().is_none(),
        "{:?}",
        second.lines()
    );
    assert!(second.lines().is_empty(), "{:?}", second.lines());
    folder.stop();
    first.wait_line("stopped");
    // Then it is the instance, with its own launch: nothing was swallowed.
    assert_eq!(second.wait_line("claimed "), "shell");
    let run: Value = serde_json::from_str(&second.wait_line("run ")).unwrap();
    assert_eq!(run["job"], "j_0000000000000007", "{run}");
    assert_eq!(first.delivered(), Vec::<Value>::new());
    assert_eq!(second.wait_exit().0, 0);
}

/// This process's threads.
fn thread_count() -> usize {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    let pid = std::process::id();
    // SAFETY: a snapshot walked with a correctly sized entry, then closed.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0).expect("a thread snapshot");
        let mut entry = THREADENTRY32 {
            dwSize: u32::try_from(size_of::<THREADENTRY32>()).unwrap(),
            ..Default::default()
        };
        let mut n = 0;
        let mut more = Thread32First(snap, &mut entry).is_ok();
        while more {
            if entry.th32OwnerProcessID == pid {
                n += 1;
            }
            more = Thread32Next(snap, &mut entry).is_ok();
        }
        let _ = CloseHandle(snap);
        n
    }
}

#[test] // spec: APP-013
fn no_thread_outlives_the_instance_or_a_forward() {
    let env = TestEnv::new();
    let before = thread_count();
    let server = InstanceServer::claim(&env.env, APP, false)
        .unwrap()
        .expect("the pipe is free");
    assert_eq!(
        thread_count(),
        before + 1,
        "the instance serves on one thread"
    );
    // This process finds itself as the pipe's server, and no launch takes itself for it.
    assert!(matches!(
        forward(&env.env, APP, false, &["--no-activate".to_string()]),
        Err(ForwardError::Untrusted)
    ));
    assert_eq!(server.next(), None, "nothing was handed over");
    assert_eq!(thread_count(), before + 1, "a forward leaves no thread");
    // A claim that finds the pipe owned leaves none either.
    assert!(
        InstanceServer::claim(&env.env, APP, false)
            .unwrap()
            .is_none()
    );
    assert_eq!(thread_count(), before + 1);
    drop(server);
    assert_eq!(
        thread_count(),
        before,
        "dropping the instance joins its thread"
    );
    assert!(matches!(
        forward(&env.env, APP, false, &[]),
        Err(ForwardError::NotRunning)
    ));
}

/// A pipe of `name` made first by this process with the security descriptor `sddl`, listening.
fn squat(
    rt: &tokio::runtime::Runtime,
    name: &str,
    sddl: &str,
) -> tokio::net::windows::named_pipe::NamedPipeServer {
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows::core::HSTRING;
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: a NUL-terminated SDDL and a valid out-pointer; the descriptor is left to the
    // process's end (a test).
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &HSTRING::from(sddl),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
    }
    .expect("the squatter's descriptor");
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap(),
        lpSecurityDescriptor: sd.0,
        bInheritHandle: false.into(),
    };
    let _in = rt.enter();
    // SAFETY: `attributes` and its descriptor outlive the call.
    unsafe {
        tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(true)
            .create_with_security_attributes_raw(name, (&raw mut attributes).cast())
    }
    .expect("the squatter's pipe")
}

#[test] // spec: APP-013, ARCH-026, ARCH-027
fn a_pipe_held_by_another_program_starts_its_own_instance() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    // A DACL that leaves this user out (SYSTEM only), and one that lets everyone in but whose
    // server isn't the app's exe.
    for (what, sddl) in [("deny-all", "D:P(A;;GA;;;SY)"), ("open", "D:P(A;;GA;;;WD)")] {
        let env = TestEnv::new();
        let folder = Folder::new();
        let _squatter = squat(&rt, &env.env.app_pipe_name(APP, false), sddl);
        for rest in [String::new(), task_url(&env, 9)] {
            let mut launch = Launch::start(&folder, &env, &rest);
            let (code, _) = launch.wait_exit();
            assert_eq!(code, 0, "{what} {rest}: {:?}", launch.lines());
            let run: Value = serde_json::from_str(&launch.wait_line("run ")).unwrap();
            // The first launch finds the pipe held; this squatter serves no one, so a later
            // one finds every instance busy, which can't be told from a hung LILITH (M-14).
            let notice = run["pipe_notice"].as_str().unwrap_or_default();
            assert!(
                [
                    lil_qt::single_instance::NOTICE_HELD,
                    lil_qt::single_instance::NOTICE_BUSY
                ]
                .contains(&notice),
                "{what} {rest}: {run}"
            );
            if rest.is_empty() {
                assert_eq!(
                    notice,
                    lil_qt::single_instance::NOTICE_HELD,
                    "{what}: {run}"
                );
            }
            assert!(
                launch.lines().iter().all(|l| !l.starts_with("claimed")),
                "{what}: {:?}",
                launch.lines()
            );
            if !rest.is_empty() {
                assert_eq!(run["job"], "j_0000000000000009", "its own launch: {run}");
            }
        }
        assert!(
            log_lines(&env)
                .iter()
                .any(|l| l.contains("another program holds the app's pipe")),
            "{what}: a warning is logged"
        );
    }
}

#[test] // spec: P4a-R9
fn a_deny_that_fails_exits_non_zero() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let env = TestEnv::new();
    let folder = Folder::new();
    // The core's pipe is held by this process, which isn't a core the client trusts.
    let _squatter = squat(&rt, &env.env.pipe_name(), "D:P(A;;GA;;;WD)");
    let deny = format!(
        "\"{}:deny/{CF}/0123456789abcdef0123456789abcdef\"",
        scheme(&env)
    );
    let mut launch = Launch::start(&folder, &env, &deny);
    let (code, took) = launch.wait_exit();
    assert_eq!(code, 1, "{:?}", launch.lines());
    assert!(took < Duration::from_secs(15), "bounded: {took:?}");
    assert_eq!(launch.lines(), ["exit 1 forwarded=false"]);
    let lines = deny_lines(&env, CF);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("\"failed\""), "{}", lines[0]);
}

// ---------------------------------------------------------------------------------------------
// Integrity levels (review I-6, M-16).
// ---------------------------------------------------------------------------------------------

/// Labels `dir` and what it holds Low-integrity writable (`icacls /setintegritylevel`), as a
/// sandboxed helper's folder is: a Low process can then write its out file there. Never the
/// folder of an exe the test starts at Medium: Windows starts an exe labelled Low at Low.
fn label_low(dir: &std::path::Path) {
    let done = Command::new("icacls")
        .arg(dir)
        .args(["/setintegritylevel", "(OI)(CI)low"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("icacls");
    assert!(done.success(), "icacls {}", dir.display());
}

/// A probe launch started at Low integrity, as a sandboxed helper of this user could start the
/// app: this process's token, duplicated with the Low label (S-1-16-4096), through
/// `CreateProcessAsUserW` (review I-6's recipe).
struct LowLaunch {
    process: windows::Win32::Foundation::HANDLE,
    out: PathBuf,
}

impl LowLaunch {
    fn start(folder: &Folder, out_dir: &std::path::Path, env: &TestEnv, rest: &str) -> LowLaunch {
        Self::start_as(false, folder, out_dir, env, rest, &[])
    }

    /// With `restricted`, a write-restricted Medium token instead (restricting SIDs: this user
    /// and Everyone), as some sandboxes run a helper (review M-20). `extra`: more variables, a
    /// role's.
    fn start_as(
        restricted: bool,
        folder: &Folder,
        out_dir: &std::path::Path,
        env: &TestEnv,
        rest: &str,
        extra: &[(&str, std::ffi::OsString)],
    ) -> LowLaunch {
        use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
        use windows::Win32::Security::Authorization::ConvertStringSidToSidW;
        use windows::Win32::Security::{
            CreateRestrictedToken, DuplicateTokenEx, PSID, SID_AND_ATTRIBUTES,
            SecurityImpersonation, SetTokenInformation, TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY,
            TOKEN_DUPLICATE, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenIntegrityLevel, TokenPrimary,
            WRITE_RESTRICTED,
        };
        use windows::Win32::System::Threading::{
            CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, GetCurrentProcess,
            OpenProcessToken, PROCESS_INFORMATION, STARTUPINFOW,
        };
        use windows::core::{HSTRING, PWSTR};

        let out = out_dir.join("low.txt");
        let exe = folder.exe();
        let mut vars: Vec<(std::ffi::OsString, std::ffi::OsString)> = std::env::vars_os().collect();
        let mut set = |k: &str, v: std::ffi::OsString| {
            vars.retain(|(name, _)| !name.eq_ignore_ascii_case(k));
            vars.push((k.into(), v));
        };
        for (k, v) in env.child_env() {
            set(&k.to_string_lossy(), v);
        }
        set(PROBE, "1".into());
        set(
            PROBE_CMDLINE,
            format!("\"{}\" {rest}", exe.display()).into(),
        );
        set(PROBE_OUT, out.clone().into_os_string());
        for (k, v) in extra {
            set(k, v.clone());
        }
        let mut block: Vec<u16> = Vec::new();
        for (k, v) in &vars {
            use std::os::windows::ffi::OsStrExt as _;
            block.extend(k.encode_wide());
            block.push(u16::from(b'='));
            block.extend(v.encode_wide());
            block.push(0);
        }
        block.push(0);
        let mut command: Vec<u16> = format!(
            "\"{}\" probe --exact --ignored --nocapture --test-threads=1",
            exe.display()
        )
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
        // SAFETY: every handle is this process's own and closed once; every buffer outlives
        // the call that reads it; the SID from ConvertStringSidToSidW is freed once.
        unsafe {
            let mut token = HANDLE::default();
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ADJUST_DEFAULT | TOKEN_ASSIGN_PRIMARY,
                &mut token,
            )
            .expect("this process's token");
            let mut low = HANDLE::default();
            DuplicateTokenEx(
                token,
                windows::Win32::Security::TOKEN_ACCESS_MASK(0x0200_0000),
                None,
                SecurityImpersonation,
                TokenPrimary,
                &mut low,
            )
            .expect("a copy of the token");
            let mut sid = PSID::default();
            ConvertStringSidToSidW(&HSTRING::from("S-1-16-4096"), &mut sid).expect("the Low SID");
            if restricted {
                let user = lil_platform::process::current_user_sid().expect("this user's SID");
                let mut sids = Vec::new();
                for text in [user.as_str(), "S-1-1-0"] {
                    let mut one = PSID::default();
                    ConvertStringSidToSidW(&HSTRING::from(text), &mut one).expect("a SID");
                    sids.push(SID_AND_ATTRIBUTES {
                        Sid: one,
                        Attributes: 0,
                    });
                }
                // The logon SID, without which the restricted process can't open its window
                // station or desktop (STATUS_DLL_INIT_FAILED).
                let mut needed = 0u32;
                let _ = windows::Win32::Security::GetTokenInformation(
                    token,
                    windows::Win32::Security::TokenGroups,
                    None,
                    0,
                    &mut needed,
                );
                let mut groups_buf = vec![0u64; (needed as usize).div_ceil(8)];
                windows::Win32::Security::GetTokenInformation(
                    token,
                    windows::Win32::Security::TokenGroups,
                    Some(groups_buf.as_mut_ptr().cast()),
                    needed,
                    &mut needed,
                )
                .expect("the token's groups");
                let groups = &*groups_buf
                    .as_ptr()
                    .cast::<windows::Win32::Security::TOKEN_GROUPS>();
                let all = std::slice::from_raw_parts(
                    groups.Groups.as_ptr(),
                    usize::try_from(groups.GroupCount).unwrap(),
                );
                let mut extra: Vec<SID_AND_ATTRIBUTES> = all
                    .iter()
                    // SE_GROUP_LOGON_ID.
                    .filter(|g| g.Attributes & 0xC000_0000 == 0xC000_0000)
                    .map(|g| SID_AND_ATTRIBUTES {
                        Sid: g.Sid,
                        Attributes: 0,
                    })
                    .collect();
                let owned = sids.len();
                sids.append(&mut extra);
                let mut write_restricted = HANDLE::default();
                CreateRestrictedToken(
                    token,
                    WRITE_RESTRICTED,
                    None,
                    None,
                    Some(&sids),
                    &mut write_restricted,
                )
                .expect("a write-restricted token");
                let _ = CloseHandle(low);
                low = write_restricted;
                for one in sids.into_iter().take(owned) {
                    let _ = LocalFree(Some(HLOCAL(one.Sid.0)));
                }
            } else {
                let label = TOKEN_MANDATORY_LABEL {
                    Label: SID_AND_ATTRIBUTES {
                        Sid: sid,
                        // SE_GROUP_INTEGRITY.
                        Attributes: 0x20,
                    },
                };
                SetTokenInformation(
                    low,
                    TokenIntegrityLevel,
                    (&raw const label).cast(),
                    u32::try_from(size_of::<TOKEN_MANDATORY_LABEL>()).unwrap() + 12,
                )
                .expect("the Low label");
            }
            let startup = STARTUPINFOW {
                cb: u32::try_from(size_of::<STARTUPINFOW>()).unwrap(),
                ..Default::default()
            };
            let mut info = PROCESS_INFORMATION::default();
            CreateProcessAsUserW(
                Some(low),
                &HSTRING::from(exe.as_os_str()),
                Some(PWSTR(command.as_mut_ptr())),
                None,
                None,
                false,
                CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
                Some(block.as_ptr().cast()),
                None,
                &startup,
                &mut info,
            )
            .expect("the Low process");
            let _ = CloseHandle(info.hThread);
            let _ = CloseHandle(low);
            let _ = CloseHandle(token);
            let _ = LocalFree(Some(HLOCAL(sid.0)));
            LowLaunch {
                process: info.hProcess,
                out,
            }
        }
    }

    fn pid(&self) -> u32 {
        // SAFETY: the process handle is this struct's own and open.
        unsafe { windows::Win32::System::Threading::GetProcessId(self.process) }
    }

    fn lines(&self) -> Vec<String> {
        std::fs::read_to_string(&self.out)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn wait_line(&self, prefix: &str) -> String {
        let until = Instant::now() + DEADLINE;
        loop {
            if let Some(line) = self.lines().iter().find(|l| l.starts_with(prefix)) {
                return line[prefix.len()..].trim().to_string();
            }
            assert!(
                Instant::now() < until,
                "no `{prefix}` line from the Low probe; it wrote {:?}",
                self.lines()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for LowLaunch {
    fn drop(&mut self) {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{TerminateProcess, WaitForSingleObject};
        // SAFETY: the process handle is this struct's own, closed once.
        unsafe {
            let _ = TerminateProcess(self.process, 1);
            let _ = WaitForSingleObject(self.process, 5000);
            let _ = CloseHandle(self.process);
        }
    }
}

#[test] // spec: ARCH-027, APP-013
fn a_lower_integrity_instance_is_never_trusted() {
    let env = TestEnv::new();
    let folder = Folder::new();
    let low_dir = tempfile::tempdir().unwrap();
    label_low(low_dir.path());
    label_low(env.root());
    // A Low copy of the app's exe claims the pipe first.
    let low = LowLaunch::start(&folder, low_dir.path(), &env, "");
    assert_eq!(low.wait_line("claimed "), "shell", "{:?}", low.lines());
    // A normal launch hands it nothing, not even a deny, and runs as an instance of its own.
    for rest in [
        task_url(&env, 5),
        format!(
            "\"{}:deny/{CF}/00112233445566778899aabbccddeeff\"",
            scheme(&env)
        ),
    ] {
        let mut launch = Launch::start(&folder, &env, &rest);
        let (code, _) = launch.wait_exit();
        assert_eq!(code, 0, "{rest}: {:?}", launch.lines());
        assert!(
            !launch.lines().iter().any(|l| l.contains("forwarded=true")),
            "{rest}: {:?}",
            launch.lines()
        );
        if rest.contains("task/") {
            let run: Value = serde_json::from_str(&launch.wait_line("run ")).unwrap();
            assert_eq!(
                run["pipe_notice"],
                lil_qt::single_instance::NOTICE_HELD,
                "{run}"
            );
            assert_eq!(run["job"], "j_0000000000000005", "its own launch: {run}");
        }
    }
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        low.lines()
            .iter()
            .all(|l| !l.starts_with("deliver") && !l.starts_with("handled")),
        "the Low instance got nothing: {:?}",
        low.lines()
    );
}

#[test] // spec: ARCH-027
fn a_lower_integrity_launch_hands_the_instance_nothing() {
    let env = TestEnv::new();
    let folder = Folder::new();
    let low_dir = tempfile::tempdir().unwrap();
    label_low(low_dir.path());
    label_low(env.root());
    let app = running(&folder, &env, "", "shell");
    let low = LowLaunch::start(&folder, low_dir.path(), &env, &task_url(&env, 6));
    // It can't hand its launch over: it runs as an instance of its own, and the app gets
    // nothing.
    let run: Value = serde_json::from_str(&low.wait_line("run ")).unwrap();
    assert!(run["pipe_notice"].is_string(), "{run}");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(app.delivered(), Vec::<Value>::new());
    folder.stop();
}

#[test] // spec: ARCH-027
fn a_restricted_token_copy_is_never_trusted() {
    // A write-restricted Medium copy of the app's exe holds the pipe: a launch hands it
    // nothing (review M-20).
    let env = TestEnv::new();
    let folder = Folder::new();
    let out_dir = tempfile::tempdir().unwrap();
    let held = LowLaunch::start_as(true, &folder, out_dir.path(), &env, "", &[]);
    assert_eq!(held.wait_line("claimed "), "shell", "{:?}", held.lines());
    let mut launch = Launch::start(&folder, &env, &task_url(&env, 3));
    assert_eq!(launch.wait_exit().0, 0, "{:?}", launch.lines());
    let run: Value = serde_json::from_str(&launch.wait_line("run ")).unwrap();
    assert_eq!(
        run["pipe_notice"],
        lil_qt::single_instance::NOTICE_HELD,
        "{run}"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        held.lines().iter().all(|l| !l.starts_with("deliver")),
        "{:?}",
        held.lines()
    );
    drop(held);

    // And a restricted launch hands a normal instance nothing: LILITH's own instance doesn't
    // accept it, and it says so.
    let app = running(&folder, &env, "", "shell");
    let out_dir2 = tempfile::tempdir().unwrap();
    let restricted = LowLaunch::start_as(
        true,
        &folder,
        out_dir2.path(),
        &env,
        &task_url(&env, 4),
        &[],
    );
    let run: Value = serde_json::from_str(&restricted.wait_line("run ")).unwrap();
    assert_eq!(
        run["pipe_notice"],
        lil_qt::single_instance::NOTICE_NOT_ACCEPTED,
        "{run}"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(app.delivered(), Vec::<Value>::new());
    folder.stop();
}

// ---------------------------------------------------------------------------------------------
// A pipe whose reported server isn't who holds it (the PR #65 audit, finding 6).
// ---------------------------------------------------------------------------------------------

/// Whoever holds the pipe, and how to read what it got.
trait Holder {
    fn holder_pid(&self) -> u32;
    fn holder_lines(&self) -> Vec<String>;
}

impl Holder for Launch {
    fn holder_pid(&self) -> u32 {
        self.child.id()
    }
    fn holder_lines(&self) -> Vec<String> {
        self.lines()
    }
}

impl Holder for LowLaunch {
    fn holder_pid(&self) -> u32 {
        self.pid()
    }
    fn holder_lines(&self) -> Vec<String> {
        self.lines()
    }
}

/// A `creator` copy of the app's exe, whose pipe `holder` now holds.
fn hand_pipe(
    folder: &Folder,
    env: &TestEnv,
    handed: &std::path::Path,
    holder: &impl Holder,
    keep: bool,
) -> Launch {
    let mut extra = vec![
        (PROBE_ROLE, "creator".into()),
        (PROBE_HOLDER, holder.holder_pid().to_string().into()),
        (PROBE_HANDED, handed.as_os_str().to_owned()),
    ];
    if keep {
        extra.push((PROBE_KEEP, "1".into()));
    }
    let creator = Launch::start_with(folder, env, "", &extra);
    creator.wait_line("handed");
    creator
}

/// Waits until `holder` has seen `clients` clients come and go.
fn wait_clients(holder: &impl Holder, clients: usize) -> Vec<String> {
    let until = Instant::now() + DEADLINE;
    loop {
        let lines = holder.holder_lines();
        if lines.iter().filter(|l| *l == "eof").count() >= clients {
            return lines;
        }
        assert!(Instant::now() < until, "{lines:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test] // spec: ARCH-027, APP-013
fn a_low_holder_of_the_pipe_fails_the_proof_and_gets_no_arguments() {
    let env = TestEnv::new();
    let folder = Folder::new();
    let low_dir = tempfile::tempdir().unwrap();
    label_low(low_dir.path());
    label_low(env.root());
    let handed = low_dir.path().join("handed");
    // A Low process holds the pipe, and the pid the pipe reports is a live Medium copy of this
    // same exe, which passes every check of a pid: as when a copy took a dead creator's pid.
    let holder = LowLaunch::start_as(
        false,
        &folder,
        low_dir.path(),
        &env,
        "",
        &[
            (PROBE_ROLE, "holder".into()),
            (PROBE_HANDED, handed.as_os_str().to_owned()),
        ],
    );
    holder.wait_line("ready");
    let _creator = hand_pipe(&folder, &env, &handed, &holder, true);

    let token = "00112233445566778899aabbccddeeff";
    let deny = format!("\"{}:deny/{CF}/{token}\"", scheme(&env));
    let mut task = Launch::start(&folder, &env, &task_url(&env, 5));
    assert_eq!(task.wait_exit().0, 0, "{:?}", task.lines());
    let run: Value = serde_json::from_str(&task.wait_line("run ")).unwrap();
    assert_eq!(
        run["pipe_notice"],
        lil_qt::single_instance::NOTICE_HELD,
        "{run}"
    );
    assert_eq!(run["job"], "j_0000000000000005", "its own launch: {run}");
    let mut denied = Launch::start(&folder, &env, &deny);
    assert_eq!(denied.wait_exit().0, 0, "{:?}", denied.lines());
    assert_eq!(denied.lines(), ["exit 0 forwarded=false"]);

    let lines = wait_clients(&holder, 2);
    let got: Vec<&String> = lines.iter().filter(|l| l.starts_with("got ")).collect();
    assert_eq!(
        got.len(),
        2,
        "a proof from each launch and nothing else: {lines:?}"
    );
    assert!(
        got.iter().all(|l| l.contains("\"prove\"")),
        "0 bytes of arguments: {lines:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|l| l.contains(token) || l.contains("task/")),
        "{lines:?}"
    );
    // It couldn't set the event, by either way it has: no write up (5: access denied), and the
    // client's token is lent to it for identifying only.
    let tries = |prefix: &str| -> Vec<String> {
        lines
            .iter()
            .filter_map(|l| l.strip_prefix(prefix).map(str::to_string))
            .collect()
    };
    assert_eq!(tries("proof-open "), ["5", "5"], "{lines:?}");
    let impersonated = tries("proof-impersonated ");
    assert_eq!(impersonated.len(), 2, "{lines:?}");
    assert!(!impersonated.iter().any(|t| t == "ok"), "{lines:?}");
    folder.stop();
}

#[test] // spec: ARCH-027
fn a_holder_at_the_launchs_own_level_passes_the_proof_outside_the_threat_model() {
    // The same run with the holder at Medium: it can set the event, so it gets the arguments.
    // ARCH-027 leaves a same-user process at this level out of the model; this also shows the
    // Low run above would see the arguments if the proof didn't stop them.
    let env = TestEnv::new();
    let folder = Folder::new();
    let dir = tempfile::tempdir().unwrap();
    let handed = dir.path().join("handed");
    let holder = Launch::start_with(
        &folder,
        &env,
        "",
        &[
            (PROBE_ROLE, "holder".into()),
            (PROBE_HANDED, handed.as_os_str().to_owned()),
        ],
    );
    holder.wait_line("ready");
    let _creator = hand_pipe(&folder, &env, &handed, &holder, true);
    let mut task = Launch::start(&folder, &env, &task_url(&env, 5));
    assert_eq!(task.wait_exit().0, 0, "{:?}", task.lines());
    assert_eq!(task.lines(), ["exit 0 forwarded=true"]);
    let lines = wait_clients(&holder, 1);
    assert!(lines.iter().any(|l| l == "proof-open ok"), "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("got ") && l.contains("task/j_0000000000000005")),
        "{lines:?}"
    );
    folder.stop();
}

#[test] // spec: ARCH-027, APP-013
fn a_pipe_whose_creator_exited_gets_nothing_from_a_launch() {
    // A helper holds the pipe while its creator exits: the pipe still reports the dead pid. The
    // helper is at the launch's own level, so a proof alone wouldn't stop it.
    let env = TestEnv::new();
    let folder = Folder::new();
    let dir = tempfile::tempdir().unwrap();
    let handed = dir.path().join("handed");
    let holder = Launch::start_with(
        &folder,
        &env,
        "",
        &[
            (PROBE_ROLE, "holder".into()),
            (PROBE_HANDED, handed.as_os_str().to_owned()),
        ],
    );
    holder.wait_line("ready");
    let mut creator = hand_pipe(&folder, &env, &handed, &holder, false);
    assert_eq!(creator.wait_exit().0, 0);
    // No handle of this test keeps the dead process's pid alive.
    drop(creator);
    let mut task = Launch::start(&folder, &env, &task_url(&env, 5));
    assert_eq!(task.wait_exit().0, 0, "{:?}", task.lines());
    let run: Value = serde_json::from_str(&task.wait_line("run ")).unwrap();
    assert_eq!(
        run["pipe_notice"],
        lil_qt::single_instance::NOTICE_HELD,
        "{run}"
    );
    assert_eq!(run["job"], "j_0000000000000005", "its own launch: {run}");
    let lines = wait_clients(&holder, 1);
    assert!(
        !lines.iter().any(|l| l.starts_with("got ")),
        "0 bytes from the launch: {lines:?}"
    );
}
