//! Test processes that talk JSON lines over stdio (QA-066): [`TestApp`], the `ui:test` app
//! `lil-test-app`, and [`McpStdio`], a stdio server started under the fake `claude`.
//!
//! The core verifies a `ui:*` claim from the client process's image (ARCH-045), so the
//! connection must be made by `lil-test-app.exe` itself: the app says hello as `ui:test` and
//! then relays lines between its stdin and stdout and the core's pipe. [`TestApp`] is the test's
//! side of that relay. The core accepts the claim only on a test channel and only from a
//! trusted folder (the core's own: `target\testbins\debug` for a spawned test core).

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::bins::ensure_built;
use crate::env::TestEnv;

/// The capabilities `lil-test-app` offers in its hello: protocol 1.1's set (Global
/// Constraints, Protocol 1.1). A core shares the ones it has.
pub const TEST_APP_CAPABILITIES: &[&str] = &[
    "events.v1",
    "tools.v1",
    "jobs.v1",
    "confirmations.v1",
    "caps.v1",
];

/// How long a call or the app's hello may take before the test fails.
const DEADLINE: Duration = Duration::from_secs(30);

/// A child whose stdout lines arrive on a channel and whose stderr is kept.
struct LineChild {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
    stderr: Arc<Mutex<Vec<u8>>>,
    what: String,
}

impl LineChild {
    fn spawn(cmd: &mut Command, what: String) -> LineChild {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("cannot start {what}: {e}"));
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("piped stdout");
        let mut err_pipe = child.stderr.take().expect("piped stderr");
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&stderr);
        std::thread::spawn(move || {
            let mut chunk = [0_u8; 4096];
            while let Ok(n) = err_pipe.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                sink.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .extend_from_slice(&chunk[..n]);
            }
        });
        LineChild {
            child,
            stdin,
            lines,
            stderr,
            what,
        }
    }

    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.stderr.lock().unwrap_or_else(PoisonError::into_inner))
            .into_owned()
    }

    fn send_line(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("stdin is open");
        let written = stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.write_all(b"\n"))
            .and_then(|()| stdin.flush());
        if let Err(e) = written {
            panic!(
                "cannot write to {}: {e}\nstderr so far: {}",
                self.what,
                self.stderr()
            );
        }
    }

    fn read_line(&mut self, within: Duration) -> Option<String> {
        self.lines.recv_timeout(within).ok()
    }

    fn wait(&mut self, within: Duration) -> Option<i32> {
        let deadline = Instant::now() + within;
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Some(status.code().unwrap_or(-1));
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for LineChild {
    fn drop(&mut self) {
        drop(self.stdin.take());
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// JSON-RPC over a [`LineChild`]: ids, and `lil/event` notifications kept aside.
struct Rpc {
    io: LineChild,
    next_id: u64,
    events: VecDeque<Value>,
}

impl Rpc {
    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.io.send_line(&request.to_string());
        let deadline = Instant::now() + DEADLINE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(line) = self.io.read_line(left) else {
                panic!(
                    "{method}: no response from {} within {DEADLINE:?}\nstderr so far: {}",
                    self.io.what,
                    self.io.stderr()
                );
            };
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                panic!("{} sent a line that is not JSON: {line}", self.io.what);
            };
            if message.get("method").is_some() {
                if message["method"] == lil_protocol::method::EVENT {
                    self.events.push_back(message["params"].clone());
                }
                continue;
            }
            if message.get("id") == Some(&json!(id)) {
                return message;
            }
        }
    }

    fn next_event(&mut self, within: Duration) -> Option<Value> {
        if let Some(event) = self.events.pop_front() {
            return Some(event);
        }
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self.io.read_line(left)?;
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message.get("method") == Some(&json!(lil_protocol::method::EVENT)) {
                return Some(message["params"].clone());
            }
        }
    }
}

/// `lil-test-app` connected to a channel's core as `ui:test` (module docs). Dropping it ends
/// the app, which closes its connection.
pub struct TestApp {
    rpc: Rpc,
    hello: Value,
}

impl TestApp {
    /// Starts `lil-test-app` on `env`'s channel; it connects to the running core (waiting up
    /// to 30 s for its pipe) and says hello as `ui:test` with [`TEST_APP_CAPABILITIES`] and
    /// the workspace `ws` (none when `None`), then this subscribes to `topics`.
    ///
    /// # Panics
    /// If the app cannot start, its hello or the subscription is refused, or no answer comes
    /// within 30 s.
    pub fn start(env: &TestEnv, ws: Option<&Path>, topics: &[&str]) -> TestApp {
        TestApp::start_with_capabilities(env, ws, topics, TEST_APP_CAPABILITIES)
    }

    /// [`TestApp::start`], offering `capabilities` in the hello instead of
    /// [`TEST_APP_CAPABILITIES`] (an app of an older protocol, or one that lacks a capability).
    ///
    /// # Panics
    /// As [`TestApp::start`].
    pub fn start_with_capabilities(
        env: &TestEnv,
        ws: Option<&Path>,
        topics: &[&str],
        capabilities: &[&str],
    ) -> TestApp {
        let workspace = ws.map_or_else(
            || lil_protocol::NO_WORKSPACE.to_string(),
            |ws| env.workspace_id(ws),
        );
        let mut cmd = Command::new(&ensure_built().test_app);
        cmd.args(["--workspace", &workspace])
            .args(["--capabilities", &capabilities.join(",")])
            .current_dir(env.root());
        env.apply_child_env(&mut cmd);
        let mut io = LineChild::spawn(&mut cmd, "lil-test-app".to_string());
        let Some(line) = io.read_line(DEADLINE) else {
            panic!(
                "lil-test-app said no hello within {DEADLINE:?}\nstderr so far: {}",
                io.stderr()
            );
        };
        let hello: Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("lil-test-app's hello is not JSON ({e}): {line}"));
        assert_eq!(
            hello["result"]["status"], "ok",
            "the core refused lil-test-app's hello: {hello}"
        );
        let mut app = TestApp {
            rpc: Rpc {
                io,
                next_id: 1,
                events: VecDeque::new(),
            },
            hello: hello["result"].clone(),
        };
        if !topics.is_empty() {
            let subscribed = app.call(
                lil_protocol::method::SUBSCRIBE,
                json!({"topics": topics, "since_seq": {}}),
            );
            assert_eq!(
                subscribed["result"]["status"], "ok",
                "the subscription was refused: {subscribed}"
            );
        }
        app
    }

    /// The `core/hello` result the core gave the app (its `caller_kind`, capabilities, …).
    pub fn hello(&self) -> &Value {
        &self.hello
    }

    /// The app closes its connection and connects again, as the same process, saying hello
    /// with the workspace id `workspace` (`"none"` for none); returns the new hello's result,
    /// which [`TestApp::hello`] gives from now on. Events that arrive meanwhile are kept for
    /// [`TestApp::next_event`]; the subscriptions of the old connection are not made again.
    ///
    /// # Panics
    /// If the core refuses the hello, or no answer comes within 30 s.
    pub fn reconnect(&mut self, workspace: &str) -> Value {
        let line = json!({"lil_test_app": "reconnect", "workspace": workspace});
        self.rpc.io.send_line(&line.to_string());
        let deadline = Instant::now() + DEADLINE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(line) = self.rpc.io.read_line(left) else {
                panic!(
                    "lil-test-app said no hello after reconnecting within {DEADLINE:?}\nstderr so far: {}",
                    self.rpc.io.stderr()
                );
            };
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                panic!("lil-test-app sent a line that is not JSON: {line}");
            };
            if message.get("method") == Some(&json!(lil_protocol::method::EVENT)) {
                self.rpc.events.push_back(message["params"].clone());
                continue;
            }
            if message.get("id") == Some(&json!(0)) {
                assert_eq!(
                    message["result"]["status"], "ok",
                    "the core refused lil-test-app's hello: {message}"
                );
                self.hello = message["result"].clone();
                return self.hello.clone();
            }
        }
    }

    /// Sends a request and returns the whole response object (`result` or `error`); events
    /// that arrive meanwhile are kept for [`TestApp::next_event`].
    ///
    /// # Panics
    /// If no response arrives within 30 s.
    pub fn call(&mut self, method: &str, params: Value) -> Value {
        self.rpc.call(method, params)
    }

    /// `tools/call` of `name` with `args` and, if given, the idempotency key; returns the
    /// response's `result` (an `Outcome`).
    ///
    /// # Panics
    /// If the response has no `result`.
    pub fn tool(&mut self, name: &str, args: Value, key: Option<&str>) -> Value {
        let mut params = json!({"name": name, "arguments": args});
        if let Some(key) = key {
            params["idempotency_key"] = json!(key);
        }
        let response = self.call(lil_protocol::method::TOOLS_CALL, params);
        match response.get("result") {
            Some(result) => result.clone(),
            None => panic!("tools/call {name} returned no result: {response}"),
        }
    }

    /// The params of the next `lil/event`, or `None` if none arrived within `within`.
    pub fn next_event(&mut self, within: Duration) -> Option<Value> {
        self.rpc.next_event(within)
    }

    /// The app's process id.
    pub fn pid(&self) -> u32 {
        self.rpc.io.child.id()
    }
}

/// A stdio server started by the fake `claude --spawn-stdio` (so the server's direct parent is
/// `claude.exe`): lines in through its stdin, out through its stdout. Dropping it ends `claude`
/// (the server's stdin closes with it).
pub struct McpStdio {
    rpc: Rpc,
}

impl McpStdio {
    /// Writes `line` and a `\n` to the server's stdin.
    pub fn send_line(&mut self, line: &str) {
        self.rpc.io.send_line(line);
    }

    /// The server's next stdout line, or `None` if none arrived within `within`.
    pub fn read_line(&mut self, within: Duration) -> Option<String> {
        self.rpc.io.read_line(within)
    }

    /// A JSON-RPC request; returns the whole response (lines that are not its response are
    /// skipped).
    ///
    /// # Panics
    /// If no response arrives within 30 s.
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        self.rpc.call(method, params)
    }

    /// A JSON-RPC notification (no id, no response).
    pub fn notify(&mut self, method: &str, params: Value) {
        let note = json!({"jsonrpc": "2.0", "method": method, "params": params});
        self.rpc.io.send_line(&note.to_string());
    }

    /// The fake `claude`'s process id: the server's parent.
    pub fn claude_pid(&self) -> u32 {
        self.rpc.io.child.id()
    }

    /// What `claude` and the server wrote to stderr so far.
    pub fn stderr_so_far(&self) -> String {
        self.rpc.io.stderr()
    }

    /// `claude`'s exit code once it has exited (it exits with the server's), or `None` if it
    /// still runs after `within`.
    pub fn wait(&mut self, within: Duration) -> Option<i32> {
        self.rpc.io.wait(within)
    }
}

impl TestEnv {
    /// Runs `claude --spawn-stdio <target…>` (the fake `claude` from the test binaries) with
    /// this channel's environment, `CLAUDE_CONFIG_DIR` set to the test's default Claude Code
    /// profile, and [`TestEnv::path_with_testbins`] as `PATH`.
    ///
    /// # Panics
    /// If `claude` cannot start or `target` is empty.
    pub fn spawn_stdio(&self, target: &[&OsStr]) -> McpStdio {
        assert!(!target.is_empty(), "spawn_stdio needs a program");
        // The server may start a core (lil-mcp does), so the drop gives its pipe a retry.
        self.launched
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let mut cmd = Command::new(&ensure_built().claude);
        cmd.arg("--spawn-stdio")
            .args(target)
            .current_dir(self.root());
        self.apply_child_env(&mut cmd);
        cmd.env("CLAUDE_CONFIG_DIR", self.env.claude_default_profile_dir())
            .env("PATH", self.path_with_testbins());
        let what = format!(
            "claude --spawn-stdio {}",
            target
                .iter()
                .map(|t| t.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ")
        );
        McpStdio {
            rpc: Rpc {
                io: LineChild::spawn(&mut cmd, what),
                next_id: 1,
                events: VecDeque::new(),
            },
        }
    }

    /// `lil-mcp.exe` from the test binaries, started as the direct child of the fake
    /// `claude.exe`, as Claude Code starts it.
    pub fn mcp_under_claude(&self) -> McpStdio {
        let mcp: OsString = ensure_built().lil_mcp.clone().into_os_string();
        self.spawn_stdio(&[&mcp])
    }

    /// `lil-mcp.exe` started through a shell under the fake `claude`
    /// (`claude --spawn-stdio cmd.exe /d /c <lil-mcp.exe>`), as an agent's shell tool would.
    pub fn mcp_under_shell(&self) -> McpStdio {
        let mcp: OsString = ensure_built().lil_mcp.clone().into_os_string();
        self.spawn_stdio(&[
            OsStr::new("cmd.exe"),
            OsStr::new("/d"),
            OsStr::new("/c"),
            &mcp,
        ])
    }
}
