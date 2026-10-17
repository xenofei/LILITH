//! Detached, non-inheriting spawn (ARCH-019; Review Focus 1 and 5).
//!
//! The test binary doubles as its own child: when `WW_SPAWN_HELPER_SLEEP_MS` is set, the test
//! `helper_sleep` sleeps that long and exits 0; otherwise it returns at once. When
//! `WW_SPAWN_HELPER_EXPECT_ARG` is also set, the helper first checks its own command line and
//! working directory and exits with a distinct non-zero code if they are wrong.

use std::ffi::OsString;
use std::io::{self, Read};
use std::os::windows::io::AsRawHandle;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use windows::Win32::Foundation::{HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation};
use ww_platform::process::{
    BREAKAWAY, DETACHED_FLAGS, ProcessHandle, RawSpawner, SpawnRequest, spawn_detached,
    spawn_detached_with,
};

const SLEEP_ENV: &str = "WW_SPAWN_HELPER_SLEEP_MS";
const EXPECT_ARG_ENV: &str = "WW_SPAWN_HELPER_EXPECT_ARG";
/// One argument with a space, non-ASCII letters, embedded quotes and a trailing backslash.
const TRICKY_ARG: &str = r#"Ünïcödé space "quoted" trailing\"#;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn comspec() -> PathBuf {
    PathBuf::from(std::env::var_os("ComSpec").expect("%ComSpec% is set"))
}

/// The helper's arguments: run only `helper_sleep`.
fn helper_args() -> Vec<OsString> {
    vec!["helper_sleep".into(), "--exact".into()]
}

#[test]
fn helper_sleep() {
    let Some(ms) = std::env::var_os(SLEEP_ENV) else {
        return;
    };
    if let Some(expected) = std::env::var_os(EXPECT_ARG_ENV) {
        let args: Vec<OsString> = std::env::args_os().collect();
        if !args.iter().skip(1).any(|a| *a == expected) {
            std::process::exit(3);
        }
        if !args[0]
            .to_string_lossy()
            .ends_with(r"Ünïcödé space\helper.exe")
        {
            std::process::exit(4);
        }
        let exe_dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let cwd = std::env::current_dir().unwrap();
        if cwd.canonicalize().unwrap() != exe_dir.canonicalize().unwrap() {
            std::process::exit(5);
        }
    }
    let ms: u64 = ms.to_string_lossy().parse().unwrap();
    std::thread::sleep(Duration::from_millis(ms));
}

#[derive(Debug, Default)]
struct DeniedOnce {
    flags: Mutex<Vec<u32>>,
}

impl RawSpawner for DeniedOnce {
    fn spawn(&self, _req: &SpawnRequest, flags: u32) -> io::Result<ProcessHandle> {
        let mut seen = self.flags.lock().unwrap();
        seen.push(flags);
        if seen.len() == 1 {
            return Err(io::Error::from_raw_os_error(5)); // ERROR_ACCESS_DENIED
        }
        ww_platform::process::open(std::process::id())
    }
}

#[derive(Debug, Default)]
struct AlwaysFails {
    flags: Mutex<Vec<u32>>,
}

impl RawSpawner for AlwaysFails {
    fn spawn(&self, _req: &SpawnRequest, flags: u32) -> io::Result<ProcessHandle> {
        self.flags.lock().unwrap().push(flags);
        Err(io::Error::from_raw_os_error(2)) // ERROR_FILE_NOT_FOUND
    }
}

fn dummy_request() -> SpawnRequest {
    SpawnRequest {
        exe: PathBuf::from(r"C:\nowhere\ww-core.exe"),
        args: Vec::new(),
        extra_env: Vec::new(),
        remove_env: Vec::new(),
    }
}

#[test] // spec: ARCH-019
fn spawn_retries_without_breakaway_on_access_denied() {
    let spawner = DeniedOnce::default();
    let handle = spawn_detached_with(&spawner, &dummy_request()).unwrap();
    assert_eq!(handle.pid(), std::process::id());
    assert_eq!(
        *spawner.flags.lock().unwrap(),
        vec![DETACHED_FLAGS | BREAKAWAY, DETACHED_FLAGS]
    );
}

#[test] // spec: ARCH-019
fn spawn_does_not_retry_other_errors() {
    let spawner = AlwaysFails::default();
    let err = spawn_detached_with(&spawner, &dummy_request()).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(2));
    assert_eq!(
        *spawner.flags.lock().unwrap(),
        vec![DETACHED_FLAGS | BREAKAWAY]
    );
}

#[test] // spec: ARCH-019
fn spawn_detached_runs_child_and_reports_exit_code() {
    let child = spawn_detached(&SpawnRequest {
        exe: comspec(),
        args: vec!["/c".into(), "exit 7".into()],
        extra_env: Vec::new(),
        remove_env: Vec::new(),
    })
    .unwrap();
    assert_ne!(child.pid(), 0);
    assert_eq!(runtime().block_on(child.wait()).unwrap(), 7);
    assert_eq!(child.exit_code().unwrap(), Some(7));
}

/// Review Focus 1: a caller that captures our output must see end-of-file even while the
/// detached child keeps running.
#[test] // spec: ARCH-019
fn detached_child_does_not_inherit_parent_handles() {
    let (mut reader, writer) = std::io::pipe().unwrap();
    // SAFETY: the handle is the live write end owned by `writer`; only its flags change.
    unsafe {
        SetHandleInformation(
            HANDLE(writer.as_raw_handle()),
            HANDLE_FLAG_INHERIT.0,
            HANDLE_FLAG_INHERIT,
        )
    }
    .unwrap();

    let child = spawn_detached(&SpawnRequest {
        exe: std::env::current_exe().unwrap(),
        args: helper_args(),
        extra_env: vec![(SLEEP_ENV.into(), "5000".into())],
        remove_env: Vec::new(),
    })
    .unwrap();
    drop(writer);

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        let _ = tx.send(reader.read(&mut buf).map_err(|e| e.raw_os_error()));
    });
    let read = rx.recv_timeout(Duration::from_secs(2));
    let still_running = child.exit_code().unwrap().is_none();
    child.kill().unwrap();
    let _ = runtime().block_on(child.wait());

    assert_eq!(
        read,
        Ok(Ok(0)),
        "the read end must reach end-of-file once the parent's write end is closed"
    );
    assert!(
        still_running,
        "the child must still be running at end-of-file"
    );
}

/// Review Focus 5: an exe path and an argument with spaces and non-ASCII letters survive the
/// command line.
#[test] // spec: ARCH-019
fn spawn_quotes_paths_with_spaces_and_non_ascii() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("Ünïcödé space");
    std::fs::create_dir(&dir).unwrap();
    let helper = dir.join("helper.exe");
    std::fs::copy(std::env::current_exe().unwrap(), &helper).unwrap();

    let mut args = helper_args();
    args.push(TRICKY_ARG.into());
    let child = spawn_detached(&SpawnRequest {
        exe: helper.clone(),
        args,
        extra_env: vec![
            (SLEEP_ENV.into(), "1".into()),
            (EXPECT_ARG_ENV.into(), TRICKY_ARG.into()),
        ],
        remove_env: Vec::new(),
    })
    .unwrap();
    let code = runtime().block_on(child.wait()).unwrap();
    assert_eq!(code, 0, "helper at {} exited with {code}", helper.display());
}
