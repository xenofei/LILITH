//! `RawPipe`: a line-level client of a core's pipe, for E2E tests (QA-056).
//!
//! It speaks the wire format directly (one JSON-RPC object per line) and checks nothing about
//! the server: no peer verification, no protocol negotiation. Tests use it to send exactly the
//! bytes they mean, including malformed ones.
//!
//! Each `RawPipe` owns a current-thread tokio runtime, so it is used from synchronous tests;
//! calling it from inside another runtime panics.

use std::collections::VecDeque;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use lil_platform::names;
use lil_platform::pipe::{open_client, server_pid};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::net::windows::named_pipe::NamedPipeClient;
use tokio::runtime::Runtime;

use crate::env::TestEnv;

/// How long [`RawPipe::connect`] and [`RawPipe::call`] wait before they fail the test.
const DEADLINE: Duration = Duration::from_secs(30);
/// Pause between connection attempts.
const RETRY: Duration = Duration::from_millis(20);

/// A raw connection to a core's pipe.
pub struct RawPipe {
    reader: BufReader<ReadHalf<NamedPipeClient>>,
    writer: WriteHalf<NamedPipeClient>,
    /// Bytes of a line not yet complete (a read that timed out keeps them here).
    partial: Vec<u8>,
    next_id: u64,
    /// `lil/event` params that arrived while waiting for a response.
    events: VecDeque<Value>,
    server_pid: Option<u32>,
    /// Declared last so the pipe halves are dropped while it is still alive.
    rt: Runtime,
}

/// The default `core/hello` params of a test client, with `overrides` merged in: each key of
/// `overrides` replaces the default, and a `null` value removes the key. The defaults are
/// `protocol` (this build's), `build_id: "testkit"`, `schema_hash`, `capabilities` (this
/// build's), `caller_kind: "cli"`, `non_interactive: false` and `workspace: "none"`.
///
/// # Panics
/// If `overrides` is neither an object nor `null`.
pub fn merge_hello(overrides: Value) -> Value {
    let mut hello = json!({
        "protocol": lil_protocol::PROTOCOL.to_string(),
        "build_id": "testkit",
        "schema_hash": lil_protocol::schema_hash(),
        "capabilities": lil_protocol::CAPABILITIES,
        "caller_kind": "cli",
        "non_interactive": false,
        "workspace": lil_protocol::NO_WORKSPACE,
    });
    let fields = hello
        .as_object_mut()
        .expect("the default hello is an object");
    match overrides {
        Value::Null => {}
        Value::Object(map) => {
            for (key, value) in map {
                if value.is_null() {
                    fields.remove(&key);
                } else {
                    fields.insert(key, value);
                }
            }
        }
        other => panic!("hello overrides must be a JSON object, got {other}"),
    }
    hello
}

impl RawPipe {
    /// Connects to `env`'s core pipe, retrying for up to 30 s.
    ///
    /// # Panics
    /// If no pipe answers within 30 s.
    pub fn connect(env: &TestEnv) -> RawPipe {
        let pipe = env.env.pipe_name();
        RawPipe::connect_name(&pipe, DEADLINE)
            .unwrap_or_else(|| panic!("nothing answered on {pipe} within {DEADLINE:?}"))
    }

    /// Connects to `env`'s core pipe, retrying for up to `within`; `None` if it never opened.
    pub fn try_connect(env: &TestEnv, within: Duration) -> Option<RawPipe> {
        RawPipe::connect_name(&env.env.pipe_name(), within)
    }

    /// Connects to the pipe called `name`, retrying while it does not exist or is busy.
    pub(crate) fn connect_name(name: &str, within: Duration) -> Option<RawPipe> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        let deadline = Instant::now() + within;
        let client = loop {
            match rt.block_on(async { open_client(name) }) {
                Ok(client) => break client,
                Err(_) if Instant::now() < deadline => std::thread::sleep(RETRY),
                Err(_) => return None,
            }
        };
        Some(RawPipe::from_client(rt, client))
    }

    /// A `RawPipe` over a client already opened inside `rt` (`TestEnv`'s one-probe drop).
    pub(crate) fn from_client(rt: Runtime, client: NamedPipeClient) -> RawPipe {
        let server_pid = server_pid(&client).ok();
        let (read, writer) = tokio::io::split(client);
        RawPipe {
            reader: BufReader::new(read),
            writer,
            partial: Vec::new(),
            next_id: 1,
            events: VecDeque::new(),
            server_pid,
            rt,
        }
    }

    /// The PID of the process serving the pipe, if Windows reported it.
    pub fn server_pid(&self) -> Option<u32> {
        self.server_pid
    }

    /// Writes `line` and a `\n`.
    ///
    /// # Panics
    /// If the write fails (the core closed the connection).
    pub fn send_line(&mut self, line: &str) {
        self.try_send_line(line)
            .unwrap_or_else(|e| panic!("cannot write to the core pipe: {e}"));
    }

    pub(crate) fn try_send_line(&mut self, line: &str) -> io::Result<()> {
        let RawPipe { rt, writer, .. } = self;
        rt.block_on(async {
            writer.write_all(line.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await
        })
    }

    /// The next line without its `\n` (and `\r`), or `None` if none arrived within `within`
    /// or the core closed the connection.
    pub fn read_line(&mut self, within: Duration) -> Option<String> {
        self.try_read_line(within).ok().flatten()
    }

    /// True once the core has closed the connection (lines it still sent are skipped); false
    /// if it is still open after `within`, a failure deadline.
    pub fn wait_closed(&mut self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.try_read_line(left) {
                Ok(Some(_)) => {}
                Ok(None) => return false,
                Err(_) => return true,
            }
        }
    }

    /// `Ok(None)` on timeout (the bytes read so far are kept for the next call), `Err` with
    /// `UnexpectedEof` once the connection is closed and nothing is left.
    fn try_read_line(&mut self, within: Duration) -> io::Result<Option<String>> {
        let RawPipe {
            rt,
            reader,
            partial,
            ..
        } = self;
        // `read_until` appends whatever it read before a timeout cancels it, so nothing is lost.
        let read = rt.block_on(async {
            tokio::time::timeout(within, reader.read_until(b'\n', partial)).await
        });
        match read {
            Err(_elapsed) => Ok(None),
            Ok(Err(e)) => Err(e),
            Ok(Ok(0)) if partial.is_empty() => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the core closed the connection",
            )),
            Ok(Ok(_)) => {
                let mut line = std::mem::take(partial);
                while matches!(line.last(), Some(b'\n' | b'\r')) {
                    line.pop();
                }
                Ok(Some(String::from_utf8_lossy(&line).into_owned()))
            }
        }
    }

    /// Sends a request and returns the whole response object (`result` or `error`). `lil/event`
    /// notifications that arrive meanwhile are kept for [`RawPipe::next_event`]. An error
    /// response whose `id` is `null` (the core could not read a request, -32700) is returned at
    /// once too: no response with this call's id will follow it.
    ///
    /// # Panics
    /// If no response arrives within 30 s or the connection fails.
    pub fn call(&mut self, method: &str, params: Value) -> Value {
        self.try_call(method, params, DEADLINE)
            .unwrap_or_else(|e| panic!("{method}: {e}"))
    }

    pub(crate) fn try_call(
        &mut self,
        method: &str,
        params: Value,
        within: Duration,
    ) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.try_send_line(&request.to_string())
            .map_err(|e| format!("cannot send the request: {e}"))?;
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = match self.try_read_line(left) {
                Ok(Some(line)) => line,
                Ok(None) => return Err(format!("no response within {within:?}")),
                Err(e) => return Err(format!("no response: {e}")),
            };
            let message: Value = serde_json::from_str(&line)
                .map_err(|e| format!("the core sent a line that is not JSON ({e}): {line}"))?;
            if message.get("method").is_some() {
                if message["method"] == lil_protocol::method::EVENT {
                    self.events.push_back(message["params"].clone());
                }
                continue;
            }
            let answered = match message.get("id") {
                Some(Value::Null) => message.get("error").is_some(),
                Some(other) => *other == json!(id),
                None => false,
            };
            if answered {
                return Ok(message);
            }
        }
    }

    /// `core/hello` with [`merge_hello`]`(overrides)`; returns the whole response.
    pub fn hello(&mut self, overrides: Value) -> Value {
        self.call(lil_protocol::method::HELLO, merge_hello(overrides))
    }

    /// `tools/call` of `name` with `args` and, if given, the idempotency key; returns the
    /// response's `result` (an `Outcome`).
    ///
    /// # Panics
    /// If the response has no `result` (a JSON-RPC error).
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

    /// [`RawPipe::tool`], checked: panics unless the `Outcome`'s `status` is `ok`, and returns
    /// its `structured` value (`null` when it has none).
    ///
    /// # Panics
    /// On a JSON-RPC error or any other status; the message carries the whole result.
    pub fn tool_ok(&mut self, name: &str, args: Value, key: Option<&str>) -> Value {
        let result = self.tool(name, args, key);
        assert!(
            result["status"] == "ok",
            "tools/call {name} did not succeed: {result}"
        );
        result.get("structured").cloned().unwrap_or(Value::Null)
    }

    /// The watcher fence (Global Constraints, Tests): writes the new file
    /// `<ws>\.lilith\fence\<n>` (`n` unique in this process), then calls
    /// `test.watch_fence {name: n}`, which returns once the workspace's first reconcile has
    /// finished and the watcher has handled that file and everything before it. Tests wait on
    /// the watcher with this, never with a sleep.
    ///
    /// # Panics
    /// If the file cannot be written or the tool does not succeed.
    pub fn fence(&mut self, ws: &Path) {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let name = format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let file = ws.join(names::WORKSPACE_DIR).join("fence").join(&name);
        crate::files::write_file(&file, name.as_bytes());
        self.tool_ok("test.watch_fence", json!({ "name": name }), None);
    }

    /// The params of the next `lil/event` notification, or `None` if none arrived within
    /// `within` or the connection closed. Other messages are skipped.
    pub fn next_event(&mut self, within: Duration) -> Option<Value> {
        if let Some(event) = self.events.pop_front() {
            return Some(event);
        }
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self.try_read_line(left).ok()??;
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message.get("method") == Some(&json!(lil_protocol::method::EVENT)) {
                return Some(message["params"].clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test] // spec: QA-056
    fn raw_pipe_hello_merge_removes_null_keys() {
        let base = merge_hello(json!({}));
        assert_eq!(base["protocol"], lil_protocol::PROTOCOL.to_string());
        assert_eq!(base["build_id"], "testkit");
        assert_eq!(base["schema_hash"], lil_protocol::schema_hash());
        assert_eq!(base["capabilities"], json!(lil_protocol::CAPABILITIES));
        assert_eq!(base["caller_kind"], "cli");
        assert_eq!(base["non_interactive"], false);
        assert_eq!(base["workspace"], "none");
        assert_eq!(base.as_object().unwrap().len(), 7);

        let merged = merge_hello(json!({"protocol": "2.0", "workspace": null, "extra": 1}));
        assert_eq!(merged["protocol"], "2.0");
        assert!(merged.get("workspace").is_none(), "{merged}");
        assert_eq!(merged["extra"], 1);
        assert_eq!(merged["caller_kind"], "cli");
        assert_eq!(merged.as_object().unwrap().len(), 7);
    }

    /// Against an in-process line server: responses are matched by id, a stale response is
    /// skipped, events that arrive during a call are kept, `tool` returns `result`, and a
    /// closed connection reads as `None`. The 30 s deadlines only bound a failure.
    #[test] // spec: QA-056
    fn raw_pipe_matches_ids_and_keeps_events() {
        use tokio::io::AsyncBufReadExt as _;

        let env = TestEnv::new();
        let name = env.env.pipe_name();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let server = lil_platform::pipe::create_instance(&name, true).unwrap();
                ready_tx.send(()).unwrap();
                server.connect().await.unwrap();
                let (read, mut write) = tokio::io::split(server);
                let mut lines = BufReader::new(read).lines();
                let mut send = async |v: Value| {
                    write.write_all(format!("{v}\n").as_bytes()).await.unwrap();
                };

                let hello: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap())
                    .unwrap();
                assert_eq!(hello["method"], "core/hello");
                assert_eq!(hello["params"], merge_hello(Value::Null));
                let id = hello["id"].clone();
                send(json!({"jsonrpc": "2.0", "method": "lil/event", "params": {"seq": 1}})).await;
                send(json!({"jsonrpc": "2.0", "id": 999, "result": {"status": "stale"}})).await;
                send(json!({"jsonrpc": "2.0", "id": id, "result": {"status": "ok"}})).await;

                let call: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap())
                    .unwrap();
                assert_eq!(call["method"], "tools/call");
                assert_ne!(call["id"], id);
                assert_eq!(
                    call["params"],
                    json!({"name": "test.echo", "arguments": {"text": "hi"}, "idempotency_key": "k1"})
                );
                let result = json!({"status": "ok", "structured": {"text": "hi"}});
                send(json!({"jsonrpc": "2.0", "id": call["id"], "result": result})).await;
                send(json!({"jsonrpc": "2.0", "method": "lil/event", "params": {"seq": 2}})).await;
            });
        });
        ready_rx.recv().unwrap();

        let mut pipe = RawPipe::connect(&env);
        assert_eq!(pipe.server_pid(), Some(std::process::id()));
        assert_eq!(pipe.hello(json!({}))["result"]["status"], "ok");
        let result = pipe.tool("test.echo", json!({"text": "hi"}), Some("k1"));
        assert_eq!(result["structured"]["text"], "hi");
        let wait = Duration::from_secs(30);
        assert_eq!(pipe.next_event(wait), Some(json!({"seq": 1})));
        assert_eq!(pipe.next_event(wait), Some(json!({"seq": 2})));
        server.join().expect("the fake server's checks");
        assert_eq!(pipe.read_line(wait), None);
    }

    /// Serves one connection on `env`'s pipe with `serve`, on its own thread and runtime.
    fn fake_server<F>(env: &TestEnv, serve: F) -> std::thread::JoinHandle<()>
    where
        F: AsyncFnOnce(
                tokio::io::Lines<
                    BufReader<ReadHalf<tokio::net::windows::named_pipe::NamedPipeServer>>,
                >,
                WriteHalf<tokio::net::windows::named_pipe::NamedPipeServer>,
            ) + Send
            + 'static,
    {
        let name = env.env.pipe_name();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let server = lil_platform::pipe::create_instance(&name, true).unwrap();
                ready_tx.send(()).unwrap();
                server.connect().await.unwrap();
                let (read, write) = tokio::io::split(server);
                serve(BufReader::new(read).lines(), write).await;
            });
        });
        ready_rx.recv().unwrap();
        server
    }

    /// A core that cannot read a request answers -32700 with `id: null`; `call` returns that
    /// error instead of waiting for a response with its own id. The server keeps the
    /// connection open until the client is done, so only the null-id rule can end the call
    /// (the old behaviour waited the full 30 s and then failed).
    #[test] // spec: QA-056
    fn call_returns_on_a_null_id_error() {
        let env = TestEnv::new();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let server = fake_server(&env, async move |mut lines, mut write| {
            let line = lines.next_line().await.unwrap().unwrap();
            // The fake reads every line as `{nope`: not a request it can parse.
            assert!(line.contains("\"method\":\"echo\""), "{line}");
            let error = json!({"jsonrpc": "2.0", "id": null,
                "error": {"code": -32700, "message": "parse error"}});
            write
                .write_all(format!("{error}\n").as_bytes())
                .await
                .unwrap();
            // Hold the connection open until the client has its answer.
            let _ = done_rx.recv();
        });

        let mut pipe = RawPipe::connect(&env);
        let response = pipe
            .try_call("echo", json!({"text": "{nope"}), Duration::from_secs(30))
            .expect("the null-id error ends the call");
        assert_eq!(response["error"]["code"], -32700, "{response}");
        assert_eq!(response["id"], Value::Null, "{response}");
        done_tx.send(()).unwrap();
        server.join().expect("the fake server's checks");
    }

    /// `fence` writes `<ws>\.lilith\fence\<n>` before it calls `test.watch_fence {name:
    /// n}`, and `tool_ok` returns `structured`; a failed fence panics with the outcome.
    #[test] // spec: QA-031
    fn fence_writes_its_file_then_calls_watch_fence() {
        let env = TestEnv::new();
        let ws = env.make_workspace("Ünïcödé space");
        let ws_in_server = ws.clone();
        let server = fake_server(&env, async move |mut lines, mut write| {
            for status in ["ok", "error"] {
                let call: Value =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                assert_eq!(call["method"], "tools/call");
                assert_eq!(call["params"]["name"], "test.watch_fence");
                let name = call["params"]["arguments"]["name"]
                    .as_str()
                    .unwrap()
                    .to_string();
                let file = ws_in_server
                    .join(names::WORKSPACE_DIR)
                    .join("fence")
                    .join(&name);
                assert_eq!(std::fs::read_to_string(&file).unwrap(), name);
                let result = json!({"status": status, "structured": {}});
                let reply = json!({"jsonrpc": "2.0", "id": call["id"], "result": result});
                write
                    .write_all(format!("{reply}\n").as_bytes())
                    .await
                    .unwrap();
            }
        });

        let mut pipe = RawPipe::connect(&env);
        pipe.fence(&ws);
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pipe.fence(&ws)));
        assert!(failed.is_err(), "a fence whose tool failed must panic");
        server.join().expect("the fake server's checks");
        let fences = std::fs::read_dir(ws.join(names::WORKSPACE_DIR).join("fence"))
            .unwrap()
            .count();
        assert_eq!(fences, 2, "each fence has its own file");
    }
}
