//! The hello timeout (ARCH-029): a connection that has not said hello is closed after 30 s.
//!
//! Paused tokio time (QA-031): the core runs on the test's own runtime and the clients are
//! tokio pipe clients in the same runtime, so `tokio::time::advance` drives the timeout. A
//! paused runtime also advances by itself whenever it would sleep, so a check that something
//! has *not* happened yet is bounded by a 1 ms timeout: with nothing ready, the runtime jumps
//! to that timeout before any later timer.

use std::sync::Arc;
use std::time::Duration;

use lil_core::{CoreConfig, start};
use lil_platform::pipe::{ConnectError, ProofEvent, open_client};
use lil_testkit::{FakeClock, SeqIds, TestEnv, merge_hello};
use serde_json::{Value, json};
use tokio::io::{
    AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines, ReadHalf, WriteHalf,
};
use tokio::net::windows::named_pipe::NamedPipeClient;

/// A line client of the core's pipe.
struct Client {
    lines: Lines<BufReader<ReadHalf<NamedPipeClient>>>,
    write: WriteHalf<NamedPipeClient>,
    next_id: u64,
}

impl Client {
    async fn connect(pipe: &str) -> Client {
        let (read, write) = tokio::io::split(connect(pipe).await);
        Client {
            lines: BufReader::new(read).lines(),
            write,
            next_id: 1,
        }
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.write
            .write_all(format!("{request}\n").as_bytes())
            .await
            .expect("write");
        let line = self.lines.next_line().await.expect("read").expect("a line");
        let response: Value = serde_json::from_str(&line).expect("JSON");
        assert_eq!(response["id"], id, "{response}");
        response
    }

    /// True once the core has closed the connection; false if it is still open after 1 ms
    /// of paused time.
    async fn closed(&mut self) -> bool {
        match tokio::time::timeout(Duration::from_millis(1), self.lines.next_line()).await {
            Err(_elapsed) => false,
            Ok(Ok(Some(line))) => panic!("unexpected line: {line}"),
            Ok(Ok(None) | Err(_)) => true,
        }
    }
}

/// Opens the pipe, yielding (without moving the paused clock) while the core has not yet
/// created its next free instance.
async fn connect(pipe: &str) -> NamedPipeClient {
    for _ in 0..100_000 {
        match open_client(pipe) {
            Ok(client) => return client,
            Err(ConnectError::Busy) => tokio::task::yield_now().await,
            Err(e) => panic!("cannot connect to {pipe}: {e}"),
        }
    }
    panic!("no free instance of {pipe}");
}

#[tokio::test(start_paused = true)] // spec: ARCH-029
async fn silent_connection_is_closed_after_hello_timeout() {
    let env = TestEnv::new();
    let cfg = CoreConfig {
        clock: FakeClock::new(),
        ids: Arc::new(SeqIds::new()),
        test_parent_pid: None,
        ..CoreConfig::for_env(env.env.clone()).expect("core config")
    };
    assert_eq!(cfg.hello_timeout, Duration::from_secs(30));
    let core = start(cfg).await.expect("the core starts");
    let pipe = env.env.pipe_name();

    // One client says hello; one sends nothing at all; one sends only other requests, so the
    // test knows when its connection was opened (its first reply comes after that).
    let mut talker = Client::connect(&pipe).await;
    let hello = talker.call("core/hello", merge_hello(json!({}))).await;
    assert_eq!(hello["result"]["status"], "ok", "{hello}");
    let mut silent = connect(&pipe).await;
    let mut no_hello = Client::connect(&pipe).await;
    let early = no_hello.call("tools/list", json!({})).await;
    assert_eq!(early["error"]["code"], -32001, "{early}");
    // A proof starts nothing, so a client that only proves is no different.
    let mut prover = Client::connect(&pipe).await;
    let proof = ProofEvent::new().unwrap();
    let proved = prover
        .call("core/prove", json!({"name": proof.name()}))
        .await;
    assert_eq!(proved["result"]["status"], "ok", "{proved}");
    assert!(proof.is_set());

    // Just before the timeout: still open.
    tokio::time::advance(Duration::from_secs(29)).await;
    let late = no_hello.call("tools/list", json!({})).await;
    assert_eq!(late["error"]["code"], -32001, "{late}");
    assert!(!no_hello.closed().await, "closed before the hello timeout");
    assert!(!prover.closed().await, "closed before the hello timeout");

    // Past it: every connection without a hello is closed.
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(
        no_hello.closed().await,
        "still open after the hello timeout"
    );
    assert!(prover.closed().await, "still open after the hello timeout");
    let mut rest = Vec::new();
    let n = silent.read_to_end(&mut rest).await.unwrap_or(0);
    assert_eq!(n, 0, "{}", String::from_utf8_lossy(&rest));

    // The one that said hello still answers.
    let list = talker.call("tools/list", json!({})).await;
    assert_ne!(list["error"]["code"], -32001, "{list}");

    core.request_drain();
    let report = core.wait().await;
    assert_eq!(report.reason, lil_core::DrainReason::Requested);
}
