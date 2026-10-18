//! `lil-fake-comfyui <main.py, ignored> --listen <address> --port <port> [other args, ignored]`:
//! the fake ComfyUI ([`lil_testkit::FakeComfy`]) as a process (QA-066), rooted in its working
//! folder (outputs in `output\`, uploads in `input\`), as an owned ComfyUI is started.
//!
//! It writes `{argv, env, pid}` (and, once listening, `listen` and `port`) to
//! `<cwd>\fake-comfyui.json`. In child mode (a `fake-child` file in the working folder, since
//! the core scrubs the environment, or `--child` on the command line) it is a launcher, as a
//! venv's `python.exe` is: it writes that file, starts itself again as its child with the same
//! arguments and waits for it. The child does the listening and writes
//! `<cwd>\fake-comfyui-child.json`. A `fake-child-any` file makes the child listen on `0.0.0.0`
//! whatever `--listen` says (for the loopback check's tests only).

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

fn value_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
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

    let ip: IpAddr = if is_child && cwd.join("fake-child-any").exists() {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    } else {
        value_after(&args, "--listen")
            .unwrap_or("127.0.0.1")
            .parse()
            .unwrap_or_else(|e| fail(&format!("--listen: {e}")))
    };
    let port: u16 = value_after(&args, "--port")
        .unwrap_or("8188")
        .parse()
        .unwrap_or_else(|e| fail(&format!("--port: {e}")));
    let listener = TcpListener::bind(SocketAddr::new(ip, port))
        .unwrap_or_else(|e| fail(&format!("cannot listen on {ip}:{port}: {e}")));
    let _fake = lil_testkit::FakeComfy::serve(listener, &cwd);
    let file = if is_child {
        "fake-comfyui-child.json"
    } else {
        "fake-comfyui.json"
    };
    record(
        &cwd.join(file),
        &[("listen", json!(ip.to_string())), ("port", json!(port))],
    );
    loop {
        std::thread::park();
    }
}
