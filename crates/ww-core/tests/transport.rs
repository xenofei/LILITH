//! The core's transport (ARCH-024–026, ARCH-033, ARCH-006, QA-010): the pipe listener, the
//! single instance per channel, NDJSON framing and the per-connection rules, against a fake
//! router; and two E2E checks of the real `ww-core` binary.
//!
//! Wall-clock durations here are failure deadlines only, never measurements (QA-031).

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::NamedPipeClient;
use tokio::runtime::Runtime;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use ww_core::{
    ConnCtx, CoreConfig, CoreExit, Router, ServerStats, claim_pipe, serve, start_in_thread,
};
use ww_platform::TEST_PIPE_SID;
use ww_platform::pipe::open_client;
use ww_platform::process::SystemInspector;
use ww_protocol::{Id, Request, Response, RpcError, rpc_code};
use ww_testkit::{CoreFixture, FakeClock, RawPipe, SeqIds, TestEnv};

/// Failure deadline for anything the test waits on.
const DEADLINE: Duration = Duration::from_secs(30);
/// Pause between polls of a condition.
const POLL: Duration = Duration::from_millis(10);

// ---------------------------------------------------------------------------------------------
// The fake router.
// ---------------------------------------------------------------------------------------------

/// `echo` returns its params; `ack` returns `"ack"` whatever its params; `big` returns a
/// result longer than a line may be; `slow` waits until the test calls [`Fake::release`];
/// anything else is -32601. Records what happened.
struct Fake {
    handshake_done: bool,
    opened: AtomicUsize,
    closed: AtomicUsize,
    closes: Mutex<Vec<CancellationToken>>,
    release: Notify,
    slow_started: Mutex<mpsc::Sender<()>>,
    log: Mutex<Vec<String>>,
}

impl Fake {
    fn new(handshake_done: bool) -> (Arc<Fake>, mpsc::Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        let fake = Arc::new(Fake {
            handshake_done,
            opened: AtomicUsize::new(0),
            closed: AtomicUsize::new(0),
            closes: Mutex::new(Vec::new()),
            release: Notify::new(),
            slow_started: Mutex::new(tx),
            log: Mutex::new(Vec::new()),
        });
        (fake, rx)
    }

    /// Lets one pending (or the next) `slow` finish.
    fn release(&self) {
        self.release.notify_one();
    }

    fn note(&self, what: &str) {
        self.log.lock().unwrap().push(what.to_string());
    }

    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

impl Router for Fake {
    fn opened(&self, conn: &ConnCtx) {
        self.opened.fetch_add(1, Ordering::SeqCst);
        self.closes.lock().unwrap().push(conn.close.clone());
    }

    fn handshake_done(&self, _conn: &ConnCtx) -> bool {
        self.handshake_done
    }

    fn handle<'a>(
        &'a self,
        _conn: &'a ConnCtx,
        req: Request,
    ) -> Pin<Box<dyn Future<Output = Option<Response>> + Send + 'a>> {
        Box::pin(async move {
            let Some(id) = req.id.clone() else {
                // The transport never hands a notification over; record it if it does.
                self.note(&format!("notification {}", req.method));
                return None;
            };
            match req.method.as_str() {
                "echo" => {
                    self.note("echo");
                    Some(Response::Result {
                        id,
                        result: req.params.unwrap_or(Value::Null),
                    })
                }
                "ack" => {
                    self.note("ack");
                    Some(Response::Result {
                        id,
                        result: json!("ack"),
                    })
                }
                "big" => Some(Response::Result {
                    id,
                    result: json!("a".repeat(ww_protocol::MAX_LINE_BYTES)),
                }),
                "slow" => {
                    self.note("slow:start");
                    let _ = self.slow_started.lock().unwrap().send(());
                    self.release.notified().await;
                    self.note("slow:end");
                    Some(Response::Result {
                        id,
                        result: json!("slow"),
                    })
                }
                _ => Some(Response::Error {
                    id: Some(id),
                    error: RpcError::new(rpc_code::METHOD_NOT_FOUND, "method not found"),
                }),
            }
        })
    }

    fn panicked(&self, _conn: &ConnCtx, id: Id) -> Response {
        Response::Error {
            id: Some(id),
            error: RpcError::new(-32603, "the handler panicked"),
        }
    }

    fn closed(&self, _conn: &ConnCtx) {
        self.closed.fetch_add(1, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------------------------

/// `claim_pipe` + `serve` with a router, on a thread with its own runtime. Dropping it stops
/// the server and waits for `serve` to return.
struct Served {
    stop: CancellationToken,
    stats: Arc<ServerStats>,
    thread: Option<JoinHandle<io::Result<()>>>,
}

impl Served {
    fn start(pipe: &str, router: Arc<dyn Router>) -> Served {
        let stop = CancellationToken::new();
        let stats = Arc::new(ServerStats::default());
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = std::thread::spawn({
            let pipe = pipe.to_string();
            let stop = stop.clone();
            let stats = stats.clone();
            move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async move {
                    let listener = claim_pipe(&pipe).expect("claim the pipe");
                    ready_tx.send(()).unwrap();
                    serve(listener, router, stats, stop).await
                })
            }
        });
        ready_rx
            .recv()
            .expect("the server thread failed to claim the pipe");
        Served {
            stop,
            stats,
            thread: Some(thread),
        }
    }

    fn connections(&self) -> usize {
        self.stats.connections.load(Ordering::SeqCst)
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(thread) = self.thread.take() {
            let result = thread.join();
            if !std::thread::panicking() {
                result
                    .expect("the server thread panicked")
                    .expect("serve failed");
            }
        }
    }
}

/// Waits until `cond` holds; panics after the deadline.
fn wait_until(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(POLL);
    }
}

/// A bare pipe client for bytes `RawPipe` cannot send (no trailing newline, one single write)
/// and for telling end-of-stream from a timeout.
struct Bare {
    pipe: NamedPipeClient,
    rt: Runtime,
}

impl Bare {
    fn connect(name: &str) -> Bare {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let deadline = Instant::now() + DEADLINE;
        let pipe = loop {
            match rt.block_on(async { open_client(name) }) {
                Ok(pipe) => break pipe,
                Err(e) => {
                    assert!(Instant::now() < deadline, "cannot connect to {name}: {e}");
                    std::thread::sleep(POLL);
                }
            }
        };
        Bare { pipe, rt }
    }

    /// Writes `bytes` in one write.
    fn write(&mut self, bytes: &[u8]) {
        let Bare { pipe, rt } = self;
        rt.block_on(pipe.write_all(bytes))
            .expect("write to the pipe");
    }

    /// Reads until the server has sent `n` whole lines and returns them without their `\n`;
    /// panics if they have not all arrived within the deadline.
    fn read_lines(&mut self, n: usize) -> Vec<String> {
        let Bare { pipe, rt } = self;
        let mut all = Vec::new();
        let read = rt.block_on(async {
            tokio::time::timeout(DEADLINE, async {
                let mut chunk = vec![0u8; 64 * 1024];
                while all.iter().filter(|&&b| b == b'\n').count() < n {
                    match pipe.read(&mut chunk).await {
                        Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
                        Ok(got) => all.extend_from_slice(&chunk[..got]),
                        Err(e) => return Err(e),
                    }
                }
                Ok(())
            })
            .await
        });
        match read {
            Ok(Ok(())) => String::from_utf8(all)
                .expect("the server sent UTF-8")
                .lines()
                .map(str::to_string)
                .collect(),
            Ok(Err(e)) => panic!("reading the pipe failed: {e}"),
            Err(_) => panic!("{n} lines did not arrive within {DEADLINE:?}"),
        }
    }

    /// Reads until the server closes the connection and returns everything it sent; panics if
    /// the connection is still open after the deadline.
    fn read_to_eof(&mut self) -> String {
        let Bare { pipe, rt } = self;
        let mut all = Vec::new();
        let read = rt.block_on(async {
            tokio::time::timeout(DEADLINE, async {
                let mut chunk = vec![0u8; 64 * 1024];
                loop {
                    match pipe.read(&mut chunk).await {
                        Ok(0) => return Ok(()),
                        Ok(n) => all.extend_from_slice(&chunk[..n]),
                        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => return Ok(()),
                        Err(e) => return Err(e),
                    }
                }
            })
            .await
        });
        match read {
            Ok(Ok(())) => String::from_utf8(all).expect("the server sent UTF-8"),
            Ok(Err(e)) => panic!("reading the pipe failed: {e}"),
            Err(_) => panic!("the server did not close the connection within {DEADLINE:?}"),
        }
    }
}

fn parse(line: &str) -> Value {
    serde_json::from_str(line).unwrap_or_else(|e| panic!("not JSON ({e}): {line}"))
}

fn read(pipe: &mut RawPipe) -> Value {
    let line = pipe
        .read_line(DEADLINE)
        .expect("no line from the server within the deadline");
    parse(&line)
}

/// A request line.
fn request(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

/// An in-process core configuration with the fake clock and ids (QA-031).
fn config(env: &TestEnv) -> CoreConfig {
    CoreConfig {
        clock: FakeClock::new(),
        ids: Arc::new(SeqIds::new()),
        inspector: Arc::new(SystemInspector),
        test_parent_pid: None,
        ..CoreConfig::for_env(env.env.clone()).expect("core config")
    }
}

// ---------------------------------------------------------------------------------------------
// Listener and single instance.
// ---------------------------------------------------------------------------------------------

#[test] // spec: ARCH-024
fn serves_on_the_channel_pipe_name() {
    let env = TestEnv::new();
    let name = env.env.pipe_name();
    assert_eq!(
        name,
        format!(
            r"\\.\pipe\worldwright-core-{TEST_PIPE_SID}-{}",
            env.env.channel()
        )
    );

    // The full core: `start` serves on the channel's pipe name.
    {
        let core = start_in_thread(config(&env));
        assert_eq!(core.core().pipe_name(), name);
        let mut pipe = RawPipe::connect(&env);
        assert_eq!(pipe.server_pid(), Some(std::process::id()));
        let response = pipe.call("tools/list", json!({}));
        assert_eq!(response["jsonrpc"], "2.0", "{response}");
        assert_eq!(response["id"], 1, "{response}");
    }

    // `claim_pipe` + `serve` on the same name, once the core above has stopped.
    let (fake, _started) = Fake::new(true);
    let _served = Served::start(&name, fake);
    let mut pipe = RawPipe::connect(&env);
    let response = pipe.call("echo", json!({"text": "Ünïcödé space"}));
    assert_eq!(response["result"], json!({"text": "Ünïcödé space"}));
}

#[test] // spec: ARCH-026
fn second_claim_on_same_pipe_is_already_running() {
    let env = TestEnv::new();
    let name = env.env.pipe_name();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let first = claim_pipe(&name).expect("the first claim");
        let second = claim_pipe(&name).expect_err("a second claim must fail");
        assert_eq!(second, CoreExit::AlreadyRunning);
        assert_eq!(second.code(), 3);
        drop(first);
        // Once the owner is gone, the pipe can be claimed again.
        claim_pipe(&name).expect("a claim after the owner left");
    });
}

// ---------------------------------------------------------------------------------------------
// Framing.
// ---------------------------------------------------------------------------------------------

#[test] // spec: ARCH-025
fn requests_and_responses_are_one_json_object_per_line() {
    let env = TestEnv::new();
    let (fake, _started) = Fake::new(true);
    let _served = Served::start(&env.env.pipe_name(), fake);
    let mut pipe = RawPipe::connect(&env);

    // Two requests in one write, one per line; a string with an escaped newline stays on its
    // line.
    let first = request(1, "echo", json!({"text": "a\nb", "n": 1}));
    let second = request(2, "echo", json!({"text": "Ünïcödé space"}));
    pipe.send_line(&format!("{first}\n{second}"));

    let mut seen = Vec::new();
    for _ in 0..2 {
        let line = pipe.read_line(DEADLINE).expect("a response line");
        assert!(!line.contains('\n'), "{line}");
        let response = parse(&line);
        assert!(response.is_object(), "{line}");
        assert_eq!(response["jsonrpc"], "2.0", "{line}");
        seen.push((response["id"].as_u64().unwrap(), response["result"].clone()));
    }
    seen.sort_by_key(|(id, _)| *id);
    assert_eq!(
        seen,
        vec![
            (1, json!({"text": "a\nb", "n": 1})),
            (2, json!({"text": "Ünïcödé space"})),
        ]
    );
}

#[test] // spec: QA-010, ARCH-025
fn garbage_line_gets_parse_error_and_connection_survives() {
    let env = TestEnv::new();
    let (fake, _started) = Fake::new(true);
    let _served = Served::start(&env.env.pipe_name(), fake);
    let mut pipe = RawPipe::connect(&env);

    pipe.send_line("{nope");
    let response = read(&mut pipe);
    assert_eq!(
        response["error"]["code"],
        rpc_code::PARSE_ERROR,
        "{response}"
    );
    assert_eq!(response["id"], Value::Null, "{response}");

    let response = pipe.call("echo", json!({"still": "here"}));
    assert_eq!(response["result"], json!({"still": "here"}), "{response}");
}

#[test] // spec: ARCH-033, ARCH-025
fn oversized_line_closes_only_that_connection() {
    let env = TestEnv::new();
    let name = env.env.pipe_name();
    let (fake, _started) = Fake::new(true);
    let served = Served::start(&name, fake.clone());
    let mut other = RawPipe::connect(&env);
    let mut big = Bare::connect(&name);
    wait_until("both connections", || served.connections() == 2);

    // 4 MiB + 1 bytes before the newline, sent in one write.
    let mut line = vec![b'a'; ww_protocol::MAX_LINE_BYTES + 1];
    line.push(b'\n');
    big.write(&line);
    let sent = big.read_to_eof();
    let lines: Vec<&str> = sent.lines().collect();
    assert_eq!(lines.len(), 1, "{sent}");
    let response = parse(lines[0]);
    assert_eq!(
        response["error"]["code"],
        rpc_code::LINE_TOO_LONG,
        "{response}"
    );
    assert_eq!(response["id"], Value::Null, "{response}");

    wait_until("the oversized connection to close", || {
        fake.closed.load(Ordering::SeqCst) == 1 && served.connections() == 1
    });
    let response = other.call("echo", json!({"ok": true}));
    assert_eq!(response["result"], json!({"ok": true}), "{response}");
    assert_eq!(fake.closed.load(Ordering::SeqCst), 1);
}

#[test] // spec: ARCH-025, QA-010
fn oversized_response_becomes_internal_error_and_connection_stays() {
    let env = TestEnv::new();
    let name = env.env.pipe_name();
    let (fake, _started) = Fake::new(true);
    let _served = Served::start(&name, fake.clone());
    let mut pipe = RawPipe::connect(&env);

    // The core never writes a line over the cap; the caller gets -32603 for its id instead.
    let response = pipe.call("big", json!({}));
    // (Only the error is printed: a failing response is 4 MiB.)
    assert_eq!(
        response["error"]["code"],
        rpc_code::INTERNAL_ERROR,
        "error: {}",
        response["error"]
    );
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("too large")),
        "{}",
        response["error"]
    );
    // The connection is still open.
    let response = pipe.call("echo", json!({"after": "big"}));
    assert_eq!(response["result"], json!({"after": "big"}), "{response}");
    assert_eq!(fake.closed.load(Ordering::SeqCst), 0);
}

#[test] // spec: ARCH-025
fn truncated_line_at_disconnect_is_ignored() {
    let env = TestEnv::new();
    let name = env.env.pipe_name();
    let (fake, _started) = Fake::new(true);
    let served = Served::start(&name, fake.clone());

    // Half a request, then a whole request object whose `\n` never comes: both are one
    // partial line at the end of the stream, and neither reaches the router.
    let mut half = Bare::connect(&name);
    half.write(br#"{"jsonrpc":"2.0","id":1,"method":"ec"#);
    drop(half);
    let mut unterminated = Bare::connect(&name);
    unterminated.write(request(2, "echo", json!({})).as_bytes());
    drop(unterminated);

    wait_until("the truncated connections to close", || {
        fake.closed.load(Ordering::SeqCst) == 2 && served.connections() == 0
    });
    assert_eq!(fake.opened.load(Ordering::SeqCst), 2);
    assert!(fake.log().is_empty(), "{:?}", fake.log());

    let mut pipe = RawPipe::connect(&env);
    let response = pipe.call("echo", json!({"after": 1}));
    assert_eq!(response["result"], json!({"after": 1}), "{response}");
    assert_eq!(
        fake.closed.load(Ordering::SeqCst),
        2,
        "closed once per connection"
    );
}

#[test] // spec: QA-010, ARCH-025
fn non_utf8_line_is_a_parse_error() {
    let env = TestEnv::new();
    let name = env.env.pipe_name();
    let (fake, _started) = Fake::new(true);
    let served = Served::start(&name, fake.clone());
    let mut pipe = Bare::connect(&name);

    // A line that is not UTF-8 (a lone 0xFF, and half of a two-byte sequence), then a
    // request, in one write: the bad line gets -32700 with id `null`, the request its answer.
    let mut bytes =
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"echo\",\"params\":\"\xFF\xC3\"}\n".to_vec();
    bytes.extend_from_slice(request(2, "echo", json!({"after": "Ünïcödé space"})).as_bytes());
    bytes.push(b'\n');
    pipe.write(&bytes);
    let lines = pipe.read_lines(2);
    assert_eq!(lines.len(), 2, "{lines:?}");
    let error = parse(&lines[0]);
    assert_eq!(error["error"]["code"], rpc_code::PARSE_ERROR, "{error}");
    assert_eq!(error["id"], Value::Null, "{error}");
    let answer = parse(&lines[1]);
    assert_eq!(answer["id"], 2, "{answer}");
    assert_eq!(
        answer["result"],
        json!({"after": "Ünïcödé space"}),
        "{answer}"
    );

    // Only the valid request reached the router, and the connection is still open.
    assert_eq!(fake.log(), ["echo"]);
    assert_eq!(fake.closed.load(Ordering::SeqCst), 0);
    assert_eq!(served.connections(), 1);
}

#[test] // spec: ARCH-025
fn client_notifications_are_ignored() {
    // Before the handshake (handled on the reader) and after it (in their own tasks).
    for handshake_done in [false, true] {
        let env = TestEnv::new();
        let (fake, _started) = Fake::new(handshake_done);
        let _served = Served::start(&env.env.pipe_name(), fake.clone());
        let mut pipe = RawPipe::connect(&env);

        // Notifications (no `id`), of a method the router knows and of one it doesn't: no
        // answer, and the router never sees them.
        pipe.send_line(
            &json!({"jsonrpc": "2.0", "method": "echo", "params": {"n": 1}}).to_string(),
        );
        pipe.send_line(&json!({"jsonrpc": "2.0", "method": "nonsense"}).to_string());
        pipe.send_line(&request(5, "ack", json!({})));
        let response = read(&mut pipe);
        assert_eq!(response["id"], 5, "handshake {handshake_done}: {response}");
        assert_eq!(
            response["result"], "ack",
            "handshake {handshake_done}: {response}"
        );
        // The next line is the answer to the next request: nothing was queued for the
        // notifications in between.
        pipe.send_line(&request(6, "echo", json!({"after": "notifications"})));
        let response = read(&mut pipe);
        assert_eq!(response["id"], 6, "handshake {handshake_done}: {response}");
        assert_eq!(
            response["result"],
            json!({"after": "notifications"}),
            "handshake {handshake_done}: {response}"
        );
        assert_eq!(fake.log(), ["ack", "echo"], "handshake {handshake_done}");
    }
}

#[test] // spec: ARCH-025
fn line_of_exactly_max_bytes_is_accepted() {
    let env = TestEnv::new();
    let (fake, _started) = Fake::new(true);
    let _served = Served::start(&env.env.pipe_name(), fake.clone());
    let mut pipe = RawPipe::connect(&env);

    // A request whose line, without its `\n`, is exactly the cap.
    let base = request(7, "ack", json!({"pad": ""}));
    let pad = ww_protocol::MAX_LINE_BYTES - base.len();
    let line = request(7, "ack", json!({"pad": "a".repeat(pad)}));
    assert_eq!(line.len(), ww_protocol::MAX_LINE_BYTES);
    pipe.send_line(&line);
    let response = read(&mut pipe);
    assert_eq!(response["id"], 7, "{response}");
    assert_eq!(response["result"], "ack", "{response}");

    // The connection is still open.
    let response = pipe.call("echo", json!({"after": "max"}));
    assert_eq!(response["result"], json!({"after": "max"}), "{response}");
    assert_eq!(fake.closed.load(Ordering::SeqCst), 0);
}

#[test] // spec: ARCH-025, QA-010
fn batch_requests_are_rejected() {
    let env = TestEnv::new();
    let (fake, _started) = Fake::new(true);
    let _served = Served::start(&env.env.pipe_name(), fake.clone());
    let mut pipe = RawPipe::connect(&env);

    pipe.send_line(&format!("[{}]", request(1, "echo", json!({}))));
    let response = read(&mut pipe);
    assert_eq!(
        response["error"]["code"],
        rpc_code::INVALID_REQUEST,
        "{response}"
    );
    assert_eq!(response["id"], Value::Null, "{response}");
    assert!(fake.log().is_empty(), "{:?}", fake.log());
}

// ---------------------------------------------------------------------------------------------
// Per-connection scheduling.
// ---------------------------------------------------------------------------------------------

#[test] // spec: ARCH-029
fn requests_before_handshake_are_handled_in_order() {
    let env = TestEnv::new();
    let (fake, started) = Fake::new(false);
    let _served = Served::start(&env.env.pipe_name(), fake.clone());
    let mut pipe = RawPipe::connect(&env);

    pipe.send_line(&format!(
        "{}\n{}",
        request(1, "slow", json!({})),
        request(2, "echo", json!({"n": 2}))
    ));
    started.recv_timeout(DEADLINE).expect("slow did not start");
    fake.release();

    let first = read(&mut pipe);
    let second = read(&mut pipe);
    assert_eq!(
        (first["id"].clone(), second["id"].clone()),
        (json!(1), json!(2))
    );
    assert_eq!(first["result"], "slow");
    assert_eq!(second["result"], json!({"n": 2}));
    assert_eq!(fake.log(), ["slow:start", "slow:end", "echo"]);
}

#[test] // spec: ARCH-025
fn slow_request_does_not_block_others_on_the_connection() {
    let env = TestEnv::new();
    let (fake, started) = Fake::new(true);
    let _served = Served::start(&env.env.pipe_name(), fake.clone());
    let mut pipe = RawPipe::connect(&env);

    pipe.send_line(&request(100, "slow", json!({})));
    started.recv_timeout(DEADLINE).expect("slow did not start");
    // `slow` is still waiting; `echo` on the same connection is answered anyway.
    let response = pipe.call("echo", json!({"n": 1}));
    assert_eq!(response["result"], json!({"n": 1}), "{response}");
    assert_eq!(fake.log(), ["slow:start", "echo"]);

    fake.release();
    let slow = read(&mut pipe);
    assert_eq!(slow["id"], 100, "{slow}");
    assert_eq!(slow["result"], "slow", "{slow}");
}

#[test] // spec: ARCH-025
fn client_vanishing_mid_request_frees_the_connection() {
    let env = TestEnv::new();
    let (fake, started) = Fake::new(true);
    let served = Served::start(&env.env.pipe_name(), fake.clone());
    let mut pipe = RawPipe::connect(&env);
    pipe.send_line(&request(1, "slow", json!({})));
    started.recv_timeout(DEADLINE).expect("slow did not start");
    assert_eq!(served.connections(), 1);

    drop(pipe);
    wait_until("the connection count to return to 0", || {
        served.connections() == 0
    });
    assert_eq!(fake.closed.load(Ordering::SeqCst), 1);
    fake.release();
}

#[test] // spec: ARCH-029
fn cancelling_close_ends_the_connection() {
    let env = TestEnv::new();
    let name = env.env.pipe_name();
    let (fake, _started) = Fake::new(true);
    let served = Served::start(&name, fake.clone());
    let mut bare = Bare::connect(&name);
    wait_until("the connection to open", || {
        fake.opened.load(Ordering::SeqCst) == 1
    });

    for close in fake.closes.lock().unwrap().iter() {
        close.cancel();
    }
    assert_eq!(bare.read_to_eof(), "");
    wait_until("the connection to close", || {
        fake.closed.load(Ordering::SeqCst) == 1 && served.connections() == 0
    });

    // Only that connection ended; the server still serves.
    let mut pipe = RawPipe::connect(&env);
    let response = pipe.call("echo", json!({"n": 3}));
    assert_eq!(response["result"], json!({"n": 3}), "{response}");
    assert_eq!(fake.closed.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------------------------
// E2E: the real binary.
// ---------------------------------------------------------------------------------------------

#[test] // spec: ARCH-006
fn core_opens_no_network_socket() {
    let env = TestEnv::new();
    let core = CoreFixture::start(&env);
    // A request first, so the core is fully serving when its sockets are read.
    let mut pipe = RawPipe::connect(&env);
    let response = pipe.call("tools/list", json!({}));
    assert_eq!(response["id"], 1, "{response}");

    let sockets = ww_platform::sockets::sockets_of(core.pid()).expect("read the socket tables");
    assert!(sockets.is_empty(), "the core owns sockets: {sockets:?}");
}

#[test] // spec: QA-056
fn test_core_exits_when_parent_is_gone() {
    let env = TestEnv::new();
    // A parent that has already exited. The `Child` keeps its process handle open, so the
    // PID cannot be reused by another process while the core looks at it.
    let comspec = std::env::var_os("ComSpec").expect("%ComSpec% is set");
    let mut parent = std::process::Command::new(comspec)
        .args(["/c", "exit 0"])
        .spawn()
        .expect("start %ComSpec%");
    assert!(parent.wait().expect("wait for %ComSpec%").success());
    let pid = parent.id().to_string();

    let core = CoreFixture::spawn_raw_with_env(&env, &[(ww_platform::ENV_TEST_PARENT_PID, &pid)]);
    assert_eq!(
        core.wait_exit(DEADLINE),
        Some(0),
        "a test core whose parent is gone must exit 0"
    );
    drop(parent);
}
