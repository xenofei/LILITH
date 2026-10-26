//! `lil-test-app [--workspace <id>] [--capabilities <a,b,…>]`: the `ui:test` app of the test
//! channels (QA-066).
//!
//! It connects to its channel's core (from the `LIL_*` variables, as `lil` does; it waits up
//! to 30 s for the pipe), says `core/hello` as `ui:test` with protocol 1.1's capabilities (or
//! the comma-separated `--capabilities`; empty for none) and the workspace (`none` by
//! default), prints the hello's response as its first stdout line, and
//! then relays lines both ways: each stdin line goes to the core, each line from the core goes
//! to stdout. It exits when either side closes (1 when it cannot connect).
//!
//! The stdin line `{"lil_test_app": "reconnect", "workspace": "<id>"}` is not relayed: the
//! app closes its connection, connects again and says hello naming that workspace (the same
//! one when `workspace` is left out), prints that hello's response, and relays on the new
//! connection. The process stays the same, as an app's does when its session reconnects.

use std::time::{Duration, Instant};

use futures_util::future::{Either, select};
use lil_platform::Env;
use lil_platform::pipe::open_client;
use lil_testkit::TEST_APP_CAPABILITIES;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

fn fail(message: &str) -> ! {
    eprintln!("lil-test-app: {message}");
    std::process::exit(1);
}

fn usage() -> ! {
    fail("usage: lil-test-app [--workspace <id>] [--capabilities <a,b,…>]")
}

fn main() {
    let mut workspace = lil_protocol::NO_WORKSPACE.to_string();
    let mut capabilities: Vec<String> = TEST_APP_CAPABILITIES
        .iter()
        .map(|c| (*c).to_string())
        .collect();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--workspace" => workspace = value,
            "--capabilities" => {
                capabilities = value
                    .split(',')
                    .filter(|c| !c.is_empty())
                    .map(str::to_string)
                    .collect();
            }
            _ => usage(),
        }
    }
    let env = Env::from_process().unwrap_or_else(|e| fail(&e.to_string()));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| fail(&e.to_string()));
    rt.block_on(relay(&env.pipe_name(), workspace, &capabilities));
}

/// The workspace a reconnect line names (`Some(None)`: the same one), or `None` for any other
/// line.
fn reconnect_request(line: &str) -> Option<Option<String>> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("lil_test_app")? != "reconnect" {
        return None;
    }
    Some(
        value
            .get("workspace")
            .and_then(Value::as_str)
            .map(str::to_string),
    )
}

async fn relay(pipe: &str, mut workspace: String, capabilities: &[String]) {
    let mut stdin = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    loop {
        let deadline = Instant::now() + Duration::from_secs(30);
        let client = loop {
            match open_client(pipe) {
                Ok(client) => break client,
                Err(_) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => fail(&format!("cannot connect to {pipe}: {e}")),
            }
        };
        let (read, mut to_core) = tokio::io::split(client);
        let mut from_core = BufReader::new(read).lines();
        let hello = json!({"jsonrpc": "2.0", "id": 0, "method": lil_protocol::method::HELLO,
        "params": {
            "protocol": lil_protocol::PROTOCOL.to_string(),
            "build_id": "lil-test-app",
            "schema_hash": lil_protocol::schema_hash(),
            "capabilities": capabilities,
            "caller_kind": "ui:test",
            "non_interactive": false,
            "workspace": workspace,
        }});
        if let Err(e) = send(&mut to_core, &hello.to_string()).await {
            fail(&format!("cannot send the hello: {e}"));
        }
        // Both `next_line`s are cancel safe: the one that loses the race loses no line.
        let next = loop {
            let from = std::pin::pin!(from_core.next_line());
            let input = std::pin::pin!(stdin.next_line());
            match select(from, input).await {
                Either::Left((Ok(Some(line)), _)) => {
                    if send(&mut stdout, &line).await.is_err() {
                        return;
                    }
                }
                Either::Right((Ok(Some(line)), _)) => match reconnect_request(&line) {
                    Some(next) => break next,
                    None => {
                        if send(&mut to_core, &line).await.is_err() {
                            return;
                        }
                    }
                },
                // Either side closed (or failed).
                _ => return,
            }
        };
        if let Some(next) = next {
            workspace = next;
        }
        // Dropping both halves closes this connection before the next one is made.
        drop((from_core, to_core));
    }
}

/// Writes `line` and a `\n`, then flushes.
async fn send(to: &mut (impl tokio::io::AsyncWrite + Unpin), line: &str) -> std::io::Result<()> {
    to.write_all(line.as_bytes()).await?;
    to.write_all(b"\n").await?;
    to.flush().await
}
