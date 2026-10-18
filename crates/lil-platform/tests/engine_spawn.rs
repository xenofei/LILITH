//! Spawning an owned engine (ARCH-064, ARCH-068): output captured line by line, exactly the
//! given environment, no handle inherited but its own two pipes; and the listening sockets the
//! loopback check reads.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::net::{SocketAddr, TcpListener};
use std::os::windows::io::AsRawHandle;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use lil_platform::engine_spawn::{
    EngineSpawn, OnLine, listening_sockets, pid_listening_on, spawn_engine,
};
use lil_platform::job_object::KillOnCloseJob;
use windows::Win32::Foundation::{HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation};

const DONE: &str = "LIL_ENGINE_SPAWN_DONE";

/// The variables `cmd.exe` sets for itself when its environment lacks them.
const CMD_OWN_VARIABLES: &[&str] = &["COMSPEC", "PATHEXT", "PROMPT"];

type Lines = Arc<Mutex<Vec<(String, String)>>>;

fn collector() -> (Lines, OnLine) {
    let lines: Lines = Arc::default();
    let sink = lines.clone();
    let on_line = Box::new(move |stream: &str, line: &str| {
        sink.lock()
            .unwrap()
            .push((stream.to_string(), line.to_string()));
    });
    (lines, on_line)
}

/// Waits (up to 20 s) until `stream` has delivered the line `DONE`.
fn wait_for_done(lines: &Lines, stream: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !lines
        .lock()
        .unwrap()
        .iter()
        .any(|(s, l)| s == stream && l == DONE)
    {
        assert!(
            Instant::now() < deadline,
            "no {DONE} on {stream}: {lines:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test] // spec: ARCH-064
fn engine_spawn_inherits_only_its_pipes_and_uses_exactly_the_given_env() {
    // An inheritable handle this process opened: a child that inherited its write end would
    // keep the read end from reaching end-of-file.
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

    let system_root = std::env::var_os("SystemRoot").expect("%SystemRoot% is set");
    let root = PathBuf::from(&system_root);
    let ping = root.join(r"System32\PING.EXE");
    // `set` prints the environment, then a marker on each stream, then `ping` keeps the child
    // alive. Winsock needs `SystemRoot`, so it is one of the two variables.
    // The words go as separate arguments, so none is quoted on the command line `cmd` reads.
    let script = format!(
        "/d /c set & echo {DONE}& echo {DONE}>&2& {} -n 30 127.0.0.1 >nul",
        ping.display()
    );
    let req = EngineSpawn {
        exe: root.join(r"System32\cmd.exe"),
        args: script.split_whitespace().map(OsString::from).collect(),
        cwd: std::env::temp_dir(),
        env: vec![
            ("SystemRoot".into(), system_root.clone()),
            ("LIL_ENGINE_PROBE".into(), "two words = fine".into()),
        ],
    };
    let job = KillOnCloseJob::new().unwrap();
    let (lines, on_line) = collector();
    let child = spawn_engine(&req, &job, on_line).unwrap();
    wait_for_done(&lines, "stdout");
    wait_for_done(&lines, "stderr");

    let printed: BTreeMap<String, String> = lines
        .lock()
        .unwrap()
        .iter()
        .filter(|(s, l)| s == "stdout" && l.as_str() != DONE)
        .map(|(_, l)| {
            let (name, value) = l.split_once('=').unwrap_or((l, ""));
            (name.to_ascii_uppercase(), value.to_string())
        })
        .collect();
    assert_eq!(
        printed.get("SYSTEMROOT").map(OsString::from),
        Some(system_root)
    );
    assert_eq!(
        printed.get("LIL_ENGINE_PROBE").map(String::as_str),
        Some("two words = fine")
    );
    let others: Vec<&String> = printed
        .keys()
        .filter(|k| !["SYSTEMROOT", "LIL_ENGINE_PROBE"].contains(&k.as_str()))
        .filter(|k| !CMD_OWN_VARIABLES.contains(&k.as_str()))
        .collect();
    assert!(others.is_empty(), "inherited variables: {others:?}");

    // The child still runs (in `ping`), yet our pipe reaches end-of-file once our write end
    // closes: the child did not inherit it.
    drop(writer);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        let _ = tx.send(reader.read(&mut buf).map_err(|e| e.raw_os_error()));
    });
    let read = rx.recv_timeout(Duration::from_secs(5));
    assert_eq!(child.exit_code().unwrap(), None, "the child still runs");
    assert_eq!(read, Ok(Ok(0)), "the read end reaches end-of-file");
}

#[test] // spec: ARCH-068
fn listening_sockets_report_loopback_and_wildcard() {
    let loopback = TcpListener::bind("127.0.0.1:0").unwrap();
    let wildcard = TcpListener::bind("0.0.0.0:0").unwrap();
    let loopback_addr = loopback.local_addr().unwrap();
    let wildcard_addr = wildcard.local_addr().unwrap();
    // An outgoing connection is not a listening socket.
    let client = std::net::TcpStream::connect(loopback_addr).unwrap();

    let me = std::process::id();
    let sockets: Vec<SocketAddr> = listening_sockets(me).unwrap();
    assert!(sockets.contains(&loopback_addr), "{sockets:?}");
    assert!(sockets.contains(&wildcard_addr), "{sockets:?}");
    assert!(
        !sockets.contains(&client.local_addr().unwrap()),
        "{sockets:?}"
    );
    assert_eq!(pid_listening_on(loopback_addr.port()).unwrap(), Some(me));
    assert_eq!(pid_listening_on(wildcard_addr.port()).unwrap(), Some(me));
    drop((loopback, wildcard, client));
}
