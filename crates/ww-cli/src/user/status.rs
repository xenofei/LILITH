//! `ww status [--json]`: whether the core runs, and what its `core/hello` said (ARCH-016,
//! ARCH-018). Without `--no-start` it starts the core, which keeps running after `ww` exits.

use serde_json::json;
use ww_protocol::HelloResult;

use super::{
    EXIT_NOT_RUNNING, EXIT_OK, Exit, GlobalArgs, UnknownCwdWorkspace, cannot_run, connect,
    print_line,
};

pub(super) fn run(global: &GlobalArgs, json: bool) -> Result<i32, Exit> {
    let Some(client) = connect(global, UnknownCwdWorkspace::WarnAndUseNone)? else {
        let text = if json {
            json!({ "running": false }).to_string()
        } else {
            "core: not running".to_string()
        };
        let _ = print_line(&text);
        return Ok(EXIT_NOT_RUNNING);
    };
    let hello = client.hello();
    let text = if json {
        serde_json::to_string_pretty(&json!({ "running": true, "hello": hello }))
            .map_err(|e| cannot_run(format!("Cannot print the core's answer: {e}")))?
    } else {
        human(hello)
    };
    let _ = print_line(&text);
    Ok(EXIT_OK)
}

/// The human lines of Appendix D.
fn human(hello: &HelloResult) -> String {
    let workspace = hello
        .workspace
        .as_ref()
        .map_or("none", |ws| ws.path.as_str());
    format!(
        "core: running (pid {}, channel {})\npipe: {}\nprotocol: {} (build {})\nworkspace: {workspace}\ncaller: {}",
        hello.core.pid,
        hello.core.channel,
        hello.core.pipe,
        hello.protocol,
        hello.build_id,
        hello.caller_kind
    )
}

#[cfg(test)]
mod tests {
    use ww_protocol::{CoreInfo, WorkspaceInfo};

    use super::*;

    #[test] // spec: ARCH-016
    fn human_status_has_the_appendix_d_lines() {
        let mut hello = HelloResult {
            session_id: "s_1".into(),
            instance_id: "i_1".into(),
            protocol: "1.0".into(),
            build_id: "0.0.0+local".into(),
            schema_hash: "0123456789abcdef".into(),
            capabilities: vec!["events.v1".into()],
            caller_kind: "cli".into(),
            workspace: None,
            core: CoreInfo {
                pid: 42,
                channel: "test-0a1b2c3d".into(),
                pipe: r"\\.\pipe\worldwright-core-S-1-test-0a1b2c3d".into(),
            },
        };
        assert_eq!(
            human(&hello),
            "core: running (pid 42, channel test-0a1b2c3d)\n\
             pipe: \\\\.\\pipe\\worldwright-core-S-1-test-0a1b2c3d\n\
             protocol: 1.0 (build 0.0.0+local)\n\
             workspace: none\n\
             caller: cli"
        );
        hello.workspace = Some(WorkspaceInfo {
            id: "0123456789abcdef".into(),
            path: r"C:\ws\Ünïcödé space".into(),
        });
        assert!(human(&hello).contains("\nworkspace: C:\\ws\\Ünïcödé space\n"));
    }
}
