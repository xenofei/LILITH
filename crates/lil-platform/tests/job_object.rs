//! Kill-on-close engine Job Objects (ARCH-064). The child is `cmd.exe` running `ping`, so the
//! job holds a launcher and its child, as a venv `python.exe` and its interpreter would.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use lil_platform::engine_spawn::{EngineSpawn, spawn_engine};
use lil_platform::job_object::{KillOnCloseJob, descendants};
use lil_platform::process::{ProcessHandle, ProcessInspector, SystemInspector};

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn system_root() -> OsString {
    std::env::var_os("SystemRoot").expect("%SystemRoot% is set")
}

/// `cmd.exe /d /c <ping> -n 30 127.0.0.1`: a launcher whose child runs for about 30 s.
/// `SystemRoot` is the only variable; Winsock needs it.
fn cmd_running_ping() -> EngineSpawn {
    let root = PathBuf::from(system_root());
    let ping = root.join(r"System32\PING.EXE");
    EngineSpawn {
        exe: root.join(r"System32\cmd.exe"),
        args: vec![
            "/d".into(),
            "/c".into(),
            ping.into_os_string(),
            "-n".into(),
            "30".into(),
            "127.0.0.1".into(),
        ],
        cwd: std::env::temp_dir(),
        env: vec![("SystemRoot".into(), system_root())],
    }
}

fn is_ping(pid: u32) -> bool {
    SystemInspector.inspect(pid).is_ok_and(|info| {
        info.image
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("ping.exe"))
    })
}

/// Waits (up to 20 s) until the `ping` child of `cmd` has started, and returns its pid.
fn wait_for_ping(cmd: &ProcessHandle) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let children = descendants(cmd.pid()).unwrap();
        if let Some(&pid) = children.iter().find(|&&pid| is_ping(pid)) {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "ping never started under cmd {}: descendants {children:?}",
            cmd.pid()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn exited_within(process: &ProcessHandle, within: Duration) -> bool {
    runtime().block_on(async { tokio::time::timeout(within, process.wait()).await.is_ok() })
}

#[test] // spec: ARCH-064
fn job_object_lists_the_launcher_and_its_child() {
    let job = KillOnCloseJob::new().unwrap();
    let cmd = spawn_engine(&cmd_running_ping(), &job, Box::new(|_, _| {})).unwrap();
    let ping = wait_for_ping(&cmd);

    let pids = job.pids().unwrap();
    // A conhost.exe may be in the job too, so the count is never asserted.
    assert!(
        pids.contains(&cmd.pid()),
        "{pids:?} lacks cmd {}",
        cmd.pid()
    );
    assert!(pids.contains(&ping), "{pids:?} lacks ping {ping}");
    assert!(descendants(cmd.pid()).unwrap().contains(&ping));
}

#[test] // spec: ARCH-064
fn job_object_kills_the_engine_when_the_handle_closes() {
    let job = KillOnCloseJob::new().unwrap();
    let cmd = spawn_engine(&cmd_running_ping(), &job, Box::new(|_, _| {})).unwrap();
    let ping = lil_platform::process::open(wait_for_ping(&cmd)).unwrap();
    assert_eq!(cmd.exit_code().unwrap(), None);

    drop(job);

    assert!(
        exited_within(&cmd, Duration::from_secs(10)),
        "cmd still runs"
    );
    assert!(
        exited_within(&ping, Duration::from_secs(10)),
        "ping still runs"
    );
}
