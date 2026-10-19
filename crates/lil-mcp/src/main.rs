//! `lil-mcp`: the stdio MCP server `lil` that Claude Code starts (ARCH-012). It adapts the
//! protocol and lowers results (`lil_protocol::lowering`); it holds no policy and no state,
//! because the core enforces everything. It reaches the core as `mcp` through `lil-client`,
//! starting the core when none runs (ARCH-018); the core decides whether the connection
//! really is `mcp` (ARCH-047).

mod core_link;
mod resources;
mod server;

use rmcp::ServiceExt as _;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(e) => {
            eprintln!("lil-mcp: cannot read the current folder: {e}");
            std::process::exit(2);
        }
    };
    let server = server::LilServer::new(core_link::CoreLink::new(cwd));
    let running = match server.serve(rmcp::transport::stdio()).await {
        Ok(running) => running,
        Err(e) => {
            eprintln!("lil-mcp: the MCP session did not start: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = running.waiting().await {
        eprintln!("lil-mcp: the MCP session ended with an error: {e}");
        std::process::exit(1);
    }
}
