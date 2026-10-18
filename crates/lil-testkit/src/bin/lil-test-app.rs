//! `lil-test-app [--workspace <id>]`: the `ui:test` app of the test channels (QA-066).
//!
//! It connects to its channel's core (from the `LIL_*` variables, as `lil` does; it waits up
//! to 30 s for the pipe), says `core/hello` as `ui:test` with protocol 1.1's capabilities and
//! the workspace (`none` by default), prints the hello's response as its first stdout line, and
//! then relays lines both ways: each stdin line goes to the core, each line from the core goes
//! to stdout. It exits when either side closes (1 when it cannot connect).

use std::time::{Duration, Instant};

use lil_platform::Env;
use lil_platform::pipe::open_client;
use lil_testkit::TEST_APP_CAPABILITIES;
use serde_json::json;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

fn fail(message: &str) -> ! {
    eprintln!("lil-test-app: {message}");
    std::process::exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let workspace = match args.as_slice() {
        [] => lil_protocol::NO_WORKSPACE.to_string(),
        [flag, id] if flag == "--workspace" => id.clone(),
        _ => fail("usage: lil-test-app [--workspace <id>]"),
    };
    let env = Env::from_process().unwrap_or_else(|e| fail(&e.to_string()));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| fail(&e.to_string()));
    rt.block_on(relay(&env.pipe_name(), &workspace));
}

async fn relay(pipe: &str, workspace: &str) {
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
    let mut from_core = BufReader::new(read);
    let hello = json!({"jsonrpc": "2.0", "id": 0, "method": lil_protocol::method::HELLO,
    "params": {
        "protocol": lil_protocol::PROTOCOL.to_string(),
        "build_id": "lil-test-app",
        "schema_hash": lil_protocol::schema_hash(),
        "capabilities": TEST_APP_CAPABILITIES,
        "caller_kind": "ui:test",
        "non_interactive": false,
        "workspace": workspace,
    }});
    if let Err(e) = send(&mut to_core, &hello.to_string()).await {
        fail(&format!("cannot send the hello: {e}"));
    }

    let mut stdout = tokio::io::stdout();
    let core_to_stdout = async {
        let mut line = String::new();
        loop {
            line.clear();
            match from_core.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if send(&mut stdout, line.trim_end_matches(['\r', '\n']))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    };
    let stdin_to_core = async {
        let mut stdin = BufReader::new(tokio::io::stdin());
        let mut line = String::new();
        loop {
            line.clear();
            match stdin.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if send(&mut to_core, line.trim_end_matches(['\r', '\n']))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    };
    futures_util::future::select(
        std::pin::pin!(core_to_stdout),
        std::pin::pin!(stdin_to_core),
    )
    .await;
}

/// Writes `line` and a `\n`, then flushes.
async fn send(to: &mut (impl tokio::io::AsyncWrite + Unpin), line: &str) -> std::io::Result<()> {
    to.write_all(line.as_bytes()).await?;
    to.write_all(b"\n").await?;
    to.flush().await
}
