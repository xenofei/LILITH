//! Every process logs (ARCH-117, ARCH-119; P4a-R20; Appendix G "Diagnostics"): `lil` and
//! `lil-mcp` each write `<process>.<YYYY-MM-DD>.log` as JSON lines in the channel's logs
//! folder, through the shared log setup.

use std::path::{Path, PathBuf};

use lil_testkit::{CoreFixture, TestEnv};
use serde_json::{Value, json};

/// `<process>.<YYYY-MM-DD>.log` files of `process` in `dir`.
fn logs_of(dir: &Path, process: &str) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            name.strip_prefix(&format!("{process}."))
                .and_then(|rest| rest.strip_suffix(".log"))
                .is_some_and(|date| {
                    date.len() == 10
                        && date.bytes().enumerate().all(|(i, b)| {
                            if i == 4 || i == 7 {
                                b == b'-'
                            } else {
                                b.is_ascii_digit()
                            }
                        })
                })
        })
        .collect();
    found.sort();
    found
}

/// Every line of `file`, each a JSON object; panics on a line that isn't.
fn json_lines(file: &Path) -> Vec<Value> {
    let text = std::fs::read_to_string(file)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", file.display()));
    text.lines()
        .map(|line| {
            let value: Value = serde_json::from_str(line).unwrap_or_else(|e| {
                panic!(
                    "{} has a line that is not JSON ({e}): {line}",
                    file.display()
                )
            });
            assert!(value.is_object(), "{line}");
            value
        })
        .collect()
}

#[test] // spec: ARCH-117, ARCH-119
fn lil_and_lil_mcp_write_json_logs() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let status = env.lil(&["status"]);
    assert_eq!(status.code, 0, "{status:?}");

    let mut mcp = env.mcp_under_claude();
    let response = mcp.request(
        "initialize",
        json!({"protocolVersion": "2025-11-25", "capabilities": {},
               "clientInfo": {"name": "lil-cli-logs", "version": "0.0.0"}}),
    );
    assert!(response.get("result").is_some(), "{response}");
    mcp.notify("notifications/initialized", json!({}));
    let listed = mcp.request("tools/list", json!({}));
    assert!(listed.get("result").is_some(), "{listed}");
    drop(mcp);

    let logs = env.env.logs_dir();
    for process in ["lil", "lil-mcp"] {
        // Each process writes its first line when its logging starts, before it answers.
        let files = logs_of(&logs, process);
        let newest = files
            .last()
            .unwrap_or_else(|| panic!("no {process}.<date>.log in {}", logs.display()));
        assert!(
            !json_lines(newest).is_empty(),
            "{} is empty",
            newest.display()
        );
    }
}
