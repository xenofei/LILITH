//! `lil-fake-comfyui <main.py, ignored> --listen <address> --port <port> [other args, ignored]`:
//! the fake ComfyUI ([`lil_testkit::FakeComfy`]) as a process (QA-066), rooted in its working
//! folder (outputs in `output\`, uploads in `input\`), as an owned ComfyUI is started.
//!
//! It always listens on `127.0.0.1` (P3-R11: no test binds a wildcard socket). `--listen` and
//! `--port` are read as ComfyUI's argparse reads them: the last occurrence wins, and a bare
//! `--listen` means `0.0.0.0,::`. A `--listen` naming anything but loopback addresses makes
//! the fake refuse to start (exit 2).
//!
//! It writes `{argv, env, pid}` (and, once listening, `listen` and `port`) to
//! `<cwd>\fake-comfyui.json`. In child mode (a `fake-child` file in the working folder, since
//! the core scrubs the environment, or `--child` on the command line) it is a launcher, as a
//! venv's `python.exe` is: it writes that file, starts itself again as its child with the same
//! arguments and waits for it. The child does the listening and writes
//! `<cwd>\fake-comfyui-child.json`.
//!
//! With a `fake-child-any` file as well, the child (still on `127.0.0.1`) appends the row
//! `{"pid": <its pid>, "port": <port>, "as": "0.0.0.0"}` to `<cwd>\fake-sockets.json`, one JSON
//! object per line, once its socket is bound and before it serves. A test core started with
//! `LIL_TEST_FAKE_SOCKETS` naming that file then sees that listener on `0.0.0.0`
//! (`lil_core::config::ScriptedSockets`).
//!
//! Once listening, the listening process prints `To see the GUI go to: http://<ip>:<port>` on
//! stdout, as ComfyUI does, and keeps the requests it received in
//! `<cwd>\fake-comfyui-requests.json`: a JSON array of `{method, path, body}`, rewritten whole
//! (through a rename) whenever a request arrives.

use std::io::Write as _;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::path::Path;
use std::process::Command;

use serde_json::{Map, Value, json};

/// Set in the child's environment by the launcher.
const ROLE_VAR: &str = "FAKE_COMFYUI_ROLE";

fn fail(message: &str) -> ! {
    eprintln!("lil-fake-comfyui: {message}");
    std::process::exit(2);
}

/// The value of the last `flag` (`--flag value` or `--flag=value`); `bare` when the last one
/// has no value (argparse's `const`); `None` when the flag is absent.
fn last_value(args: &[String], flag: &str, bare: &str) -> Option<String> {
    let with_eq = format!("{flag}=");
    let mut found = None;
    let mut i = 0;
    while i < args.len() {
        if let Some(v) = args[i].strip_prefix(&with_eq) {
            found = Some(v.to_string());
        } else if args[i] == flag {
            match args.get(i + 1) {
                Some(next) if !next.starts_with('-') => {
                    found = Some(next.clone());
                    i += 1;
                }
                _ => found = Some(bare.to_string()),
            }
        }
        i += 1;
    }
    found
}

/// Whether every address of a `--listen` value (comma-separated) is a loopback address.
fn all_loopback(listen: &str) -> bool {
    listen.split(',').all(|a| {
        let a = a.trim();
        a.eq_ignore_ascii_case("localhost") || a.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
    })
}

fn record(file: &Path, extra: &[(&str, Value)]) {
    let env: Map<String, Value> = std::env::vars_os()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                Value::from(v.to_string_lossy().into_owned()),
            )
        })
        .collect();
    let mut doc = json!({
        "argv": std::env::args().collect::<Vec<_>>(),
        "env": env,
        "pid": std::process::id(),
    });
    for (key, value) in extra {
        doc[*key] = value.clone();
    }
    if let Err(e) = std::fs::write(file, doc.to_string()) {
        fail(&format!("cannot write {}: {e}", file.display()));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir().unwrap_or_else(|e| fail(&format!("no working folder: {e}")));
    let is_child = std::env::var(ROLE_VAR).is_ok_and(|r| r == "child");
    let child_mode = cwd.join("fake-child").exists() || args.iter().any(|a| a == "--child");

    let listen = last_value(&args, "--listen", "0.0.0.0,::").unwrap_or_else(|| "127.0.0.1".into());
    if !all_loopback(&listen) {
        fail(&format!(
            "--listen {listen}: the fake listens on loopback only"
        ));
    }
    let port: u16 = last_value(&args, "--port", "")
        .unwrap_or_else(|| "8188".into())
        .parse()
        .unwrap_or_else(|e| fail(&format!("--port: {e}")));

    if child_mode && !is_child {
        record(&cwd.join("fake-comfyui.json"), &[]);
        let exe = std::env::current_exe().unwrap_or_else(|e| fail(&format!("no exe: {e}")));
        let status = Command::new(exe)
            .args(&args)
            .env(ROLE_VAR, "child")
            .status()
            .unwrap_or_else(|e| fail(&format!("cannot start the child: {e}")));
        std::process::exit(status.code().unwrap_or(1));
    }

    let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let listener = TcpListener::bind(SocketAddr::new(ip, port))
        .unwrap_or_else(|e| fail(&format!("cannot listen on {ip}:{port}: {e}")));
    // The row is in place before anything can be asked, so a socket check made after the first
    // answer always sees it.
    if is_child && cwd.join("fake-child-any").exists() {
        let row = json!({"pid": std::process::id(), "port": port, "as": "0.0.0.0"});
        let rows = cwd.join("fake-sockets.json");
        let appended = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&rows)
            .and_then(|mut f| writeln!(f, "{row}"));
        if let Err(e) = appended {
            fail(&format!("cannot append to {}: {e}", rows.display()));
        }
    }
    let fake = lil_testkit::FakeComfy::serve(listener, &cwd);
    println!("To see the GUI go to: http://{ip}:{port}");
    let file = if is_child {
        "fake-comfyui-child.json"
    } else {
        "fake-comfyui.json"
    };
    record(
        &cwd.join(file),
        &[("listen", json!(ip.to_string())), ("port", json!(port))],
    );
    keep_requests(&fake, &cwd.join("fake-comfyui-requests.json"));
}

/// Rewrites `file` with every request `fake` received whenever one arrives; never returns.
fn keep_requests(fake: &lil_testkit::FakeComfy, file: &Path) -> ! {
    let tmp = file.with_extension("json.tmp");
    let mut written = 0;
    loop {
        let requests = fake.requests();
        if requests.len() != written {
            let rows: Vec<Value> = requests
                .iter()
                .map(|r| json!({"method": r.method, "path": r.path, "body": r.body}))
                .collect();
            let saved = std::fs::write(&tmp, Value::from(rows).to_string())
                .and_then(|()| std::fs::rename(&tmp, file));
            if let Err(e) = saved {
                fail(&format!("cannot write {}: {e}", file.display()));
            }
            written = requests.len();
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
