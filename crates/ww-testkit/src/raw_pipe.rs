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
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::net::windows::named_pipe::NamedPipeClient;
use tokio::runtime::Runtime;
use ww_platform::pipe::{open_client, server_pid};

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
    /// `ww/event` params that arrived while waiting for a response.
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
        "protocol": ww_protocol::PROTOCOL.to_string(),
        "build_id": "testkit",
        "schema_hash": ww_protocol::schema_hash(),
        "capabilities": ww_protocol::CAPABILITIES,
        "caller_kind": "cli",
        "non_interactive": false,
        "workspace": ww_protocol::NO_WORKSPACE,
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
        let server_pid = server_pid(&client).ok();
        let (read, writer) = tokio::io::split(client);
        Some(RawPipe {
            reader: BufReader::new(read),
            writer,
            partial: Vec::new(),
            next_id: 1,
            events: VecDeque::new(),
            server_pid,
            rt,
        })
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

    /// Sends a request and returns the whole response object (`result` or `error`). `ww/event`
    /// notifications that arrive meanwhile are kept for [`RawPipe::next_event`].
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
                if message["method"] == ww_protocol::method::EVENT {
                    self.events.push_back(message["params"].clone());
                }
                continue;
            }
            if message.get("id") == Some(&json!(id)) {
                return Ok(message);
            }
        }
    }

    /// `core/hello` with [`merge_hello`]`(overrides)`; returns the whole response.
    pub fn hello(&mut self, overrides: Value) -> Value {
        self.call(ww_protocol::method::HELLO, merge_hello(overrides))
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
        let response = self.call(ww_protocol::method::TOOLS_CALL, params);
        match response.get("result") {
            Some(result) => result.clone(),
            None => panic!("tools/call {name} returned no result: {response}"),
        }
    }

    /// The params of the next `ww/event` notification, or `None` if none arrived within
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
            if message.get("method") == Some(&json!(ww_protocol::method::EVENT)) {
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
        assert_eq!(base["protocol"], "1.0");
        assert_eq!(base["build_id"], "testkit");
        assert_eq!(base["schema_hash"], ww_protocol::schema_hash());
        assert_eq!(base["capabilities"], json!(["events.v1", "tools.v1"]));
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
                let server = ww_platform::pipe::create_instance(&name, true).unwrap();
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
                send(json!({"jsonrpc": "2.0", "method": "ww/event", "params": {"seq": 1}})).await;
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
                send(json!({"jsonrpc": "2.0", "method": "ww/event", "params": {"seq": 2}})).await;
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
}
