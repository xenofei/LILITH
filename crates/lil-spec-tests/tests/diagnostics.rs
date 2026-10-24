//! Diagnostics: the bundle that redacts secrets, Claude Code profile labels and e-mail addresses
//! and never holds a workspace file, recent errors grouped by code, and JSON-lines logs from
//! every process (spec ARCH-117, ARCH-121, QA-083, QA-084; Appendix G "Diagnostics", rulings
//! P4a-R19 and P4a-R20). Driven through the diagnostics tools, `lil`, and the zip and log files
//! on disk, unpacked with Windows' own `tar` as an outside program would (QA-018).

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

use lil_testkit::{CoreFixture, TestEnv};
use serde_json::{Value, json};
use support::*;

/// The secret, label and address `suite.toml` holds in the bundle test; none may leave.
const SECRET: &str = "spec-canary-secret-5c1d";
const LABEL: &str = "Spec Canary Label 7e2a";
const EMAIL: &str = "spec.canary@example.com";

/// The entries Appendix G allows in a bundle.
fn allowed_entry(name: &str) -> bool {
    let name = name.replace('\\', "/");
    [
        "manifest.json",
        "versions.json",
        "suite.toml",
        "errors.json",
        "hardware.json",
        "engines.json",
        "job.json",
    ]
    .contains(&name.as_str())
        || (name.starts_with("logs/") && !name[5..].contains('/'))
}

/// Unpacks `zip` into a new folder with `tar -xf`; returns the folder.
fn unpack(zip: &Path, into: PathBuf) -> PathBuf {
    std::fs::create_dir_all(&into).unwrap();
    let out = Command::new("tar")
        .arg("-xf")
        .arg(zip)
        .arg("-C")
        .arg(&into)
        .output()
        .unwrap_or_else(|e| panic!("cannot run tar: {e}"));
    assert!(
        out.status.success(),
        "tar -xf {} failed: {}",
        zip.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    into
}

/// Every file under `dir`, recursively.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else {
            found.push(path);
        }
    }
    found
}

#[test] // spec: ARCH-121, QA-084
fn diagnostics_bundle_redacts_secrets_labels_and_emails() {
    let env = TestEnv::new();
    let ws = default_workspace(&env, "diagnostics");
    env.suite_toml_set(&format!(
        "[spec_redaction]\nAPI_KEY = \"{SECRET}\"\ncontact = \"{EMAIL}\"\n\n\
         [[claude_code.profiles]]\nid = \"p_spec\"\nlabel = \"{LABEL}\"\nconfig_dir = '{}'\n",
        path_str(&env.root().join("claude-spec"))
    ));
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, Some(&ws));
    // Something in the logs.
    let status = env.lil(&["status"]);
    assert_eq!(status.code, 0, "{status:?}");

    let bundle = write_ok(&mut pipe, "diagnostics.bundle", json!({}));
    let path = PathBuf::from(
        bundle["path"]
            .as_str()
            .unwrap_or_else(|| panic!("no path: {bundle}")),
    );
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    assert!(
        name.starts_with("lilith-diagnostics-") && name.ends_with(".zip"),
        "{bundle}"
    );
    // The same folder, whatever its case or `\\?\` prefix.
    let norm = |p: &Path| {
        path_str(p)
            .replace('/', "\\")
            .trim_start_matches(r"\\?\")
            .to_ascii_lowercase()
    };
    assert_eq!(
        path.parent().map(norm),
        Some(norm(&env.env.local_dir().join("diagnostics"))),
        "{bundle}"
    );
    assert!(bundle["redactions"].as_u64().unwrap_or(0) >= 3, "{bundle}");
    let files: Vec<&str> = bundle["files"]
        .as_array()
        .unwrap_or_else(|| panic!("no files: {bundle}"))
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for f in &files {
        assert!(
            allowed_entry(f),
            "{f} is not a bundle entry (never a workspace file)"
        );
    }
    assert!(files.contains(&"suite.toml"), "{bundle}");

    let dir = unpack(&path, env.root().join("bundle"));
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("manifest.json")).unwrap_or_default(),
    )
    .unwrap_or_else(|e| panic!("manifest.json is not JSON: {e}"));
    assert_eq!(manifest["bundle"], 1, "{manifest}");
    let suite_toml = std::fs::read_to_string(dir.join("suite.toml")).unwrap_or_default();
    assert!(suite_toml.contains("<redacted>"), "{suite_toml}");
    for file in files_under(&dir) {
        let rel = file
            .strip_prefix(&dir)
            .unwrap_or(&file)
            .to_string_lossy()
            .into_owned();
        if !(rel == "suite.toml" || rel.starts_with("logs")) {
            continue;
        }
        let text = String::from_utf8_lossy(&std::fs::read(&file).unwrap_or_default()).into_owned();
        for canary in [SECRET, LABEL, EMAIL] {
            assert!(!text.contains(canary), "{rel} still holds {canary:?}");
        }
    }
}

#[test] // spec: QA-083
fn diagnostics_recent_groups_errors_by_code() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);
    let mut watcher = session(&env, None);
    subscribe_live(&mut watcher, &["diagnostics"]);

    for _ in 0..2 {
        let unknown = pipe.tool("spec.no_such_tool", json!({}), None);
        assert_eq!(error_code(&unknown), "tool.unknown", "{unknown}");
    }
    let invalid = pipe.tool(
        "job.wait",
        json!({"job_id": "j_0000000000000000", "timeout_s": 91}),
        None,
    );
    assert_eq!(error_code(&invalid), "params.invalid", "{invalid}");

    let recent = ok(&mut pipe, "diagnostics.recent", json!({}), None);
    let rows = recent["errors"]
        .as_array()
        .unwrap_or_else(|| panic!("no errors: {recent}"));
    assert_eq!(rows.len(), 2, "one row per code: {recent}");
    assert_eq!(
        rows[0]["code"], "params.invalid",
        "the newest first: {recent}"
    );
    assert_eq!(rows[0]["count"], 1, "{recent}");
    assert_eq!(rows[0]["last_tool"], "job.wait", "{recent}");
    assert_eq!(rows[1]["code"], "tool.unknown", "{recent}");
    assert_eq!(rows[1]["count"], 2, "{recent}");
    for row in rows {
        assert!(
            row["first_at"].is_string() && row["last_at"].is_string(),
            "{row}"
        );
        assert!(
            row["last_message"].as_str().is_some_and(|m| !m.is_empty()),
            "{row}"
        );
    }
    let event = wait_for_kind(&mut watcher, "diagnostics", "error");
    assert_eq!(event["code"], "tool.unknown", "{event}");
}

#[test] // spec: ARCH-117
fn every_process_logs_json_lines() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);

    let status = env.lil(&["status"]);
    assert_eq!(status.code, 0, "{status:?}");
    let logs = env.env.local_dir().join("logs");
    let files = files_named(&logs, "lil.", ".log");
    assert!(!files.is_empty(), "no lil.<date>.log in {}", logs.display());
    for file in &files {
        let name = file
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let date = &name["lil.".len()..name.len() - ".log".len()];
        assert!(
            date.len() == 10
                && date.bytes().enumerate().all(|(i, b)| if i == 4 || i == 7 {
                    b == b'-'
                } else {
                    b.is_ascii_digit()
                }),
            "the log is named by day: {name}"
        );
    }
    let lines = jsonl(&files[files.len() - 1]);
    assert!(
        lines.iter().any(Value::is_object),
        "the log holds JSON lines: {}",
        files[files.len() - 1].display()
    );
}
