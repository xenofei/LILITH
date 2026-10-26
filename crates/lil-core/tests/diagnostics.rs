//! Diagnostics (ARCH-117–122, QA-082–085; P4a-R19, P4a-R20; Appendix G "Diagnostics"): the
//! bundle zip and what it holds, the redaction of every entry (scrub-word keys, Claude Code
//! profile labels, e-mail addresses, Credential Manager targets, Deny URL tokens, the profile
//! folder and the computer name), recent errors by code, the `diagnostics` topic, and a bundle
//! run that opens no socket. The redactor is driven directly with hostile fixtures; the bundle
//! through a real test-channel core, with a canary of each kind planted in a log line, an
//! error message, a job's arguments and a panic.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use lil_core::diagnostics::{
    DIAGNOSTICS_MODULE, DIAGNOSTICS_TOPIC, Diagnostics, DiagnosticsSources, RECENT_CAPACITY,
    Redactor, register_diagnostics_tools,
};
use lil_core::events::EventBus;
use lil_core::tools::Registry;
use lil_platform::Clock;
use lil_protocol::EffectClass;
use lil_testkit::files::write_file;
use lil_testkit::{CoreFixture, FakeClock, RawPipe, TestEnv};
use serde_json::{Value, json};

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------------------------
// The redactor.
// ---------------------------------------------------------------------------------------------

fn redactor(labels: &[&str]) -> Redactor {
    Redactor {
        labels: labels.iter().map(|l| (*l).to_string()).collect(),
        ..Redactor::default()
    }
}

#[test] // spec: ARCH-121, QA-084
fn redaction_covers_scrub_words_labels_emails_and_targets() {
    let r = redactor(&["Night Shift Profile", "Quote \"Me\" \\ Back"]);
    // A target under the suite's name before the rename.
    let legacy = format!("{}-dev/old", lil_platform::names::LEGACY_APP_NAME);
    let legacy_line = format!("and lilith-test-abc/name, {legacy}");
    let secrets = [
        "sk-live-0001",
        "tok-0002",
        "hunter2-0003",
        "bearer-0004",
        "query-0005",
        "nested-0006",
        "passwd-0007",
        "cred-0008",
        "esc-0009",
        "Night Shift Profile",
        "Quote",
        "first.last+tag@mail.example.org",
        "x@y.test",
        "providers/openai",
        "github/agent-token",
        "lilith-test-abc/name",
        legacy.as_str(),
    ];
    let hostile = [
        "API_KEY=sk-live-0001 and more",
        r#"{"fields":{"message":"call","auth_token":"tok-0002","trace_id":"t_1"}}"#,
        "password: hunter2-0003",
        "Authorization: Bearer bearer-0004",
        "GET /x?apiKey=query-0005&page=2",
        r#"{"credentials":{"user":"u","pass":"nested-0006"},"kept":"yes"}"#,
        "PASSWD = 'passwd-0007'",
        "my_credential=cred-0008;",
        r#"{"fields":{"message":"saw \"secret\":\"esc-0009\" in it"}}"#,
        "the profile Night Shift Profile is busy",
        r#"{"label":"Quote \"Me\" \\ Back"}"#,
        "mail first.last+tag@mail.example.org or x@y.test now",
        r"read LILITH/providers/openai and LILITH-dev/github/agent-token",
        legacy_line.as_str(),
    ]
    .join("\n");
    let (out, n) = r.text(&hostile);
    for secret in secrets {
        assert!(!out.contains(secret), "{secret:?} is still in:\n{out}");
    }
    assert!(n >= 17, "{n} redactions:\n{out}");
    // What isn't secret stays, and the JSON lines stay JSON.
    for kept in [
        "and more",
        r#""trace_id":"t_1""#,
        "page=2",
        r#""kept":"yes""#,
        "is busy",
        "mail ",
        " now",
    ] {
        assert!(out.contains(kept), "{kept:?} is gone:\n{out}");
    }
    for line in out.lines().filter(|l| l.starts_with('{')) {
        serde_json::from_str::<Value>(line).unwrap_or_else(|e| panic!("{e}: {line}"));
    }

    // Rust paths and source locations are not keys.
    let (same, none) = r.text(r"at crates\lil-platform\src\credentials.rs:120:5 in lil_platform::credentials::check_target");
    assert_eq!(none, 0, "{same}");
    assert!(same.contains("credentials.rs:120:5"), "{same}");

    // Exactly one replacement each.
    assert_eq!(r.text("token=abc").1, 1);
    assert_eq!(r.text("token=<redacted>").1, 0, "nothing is redacted twice");
    assert_eq!(r.text("x@y.test").0, "<redacted>");

    // suite.toml: values under a scrub-word key or table, the profile labels, and anything in
    // comments and keys.
    let doc = r#"# set up by owner@example.com
default_workspace = 'C:\w'

[providers.openai]
api_key = "sk-toml-0010"
model = "m"

[auth]
user = "toml-user-0011"
deep = { inner = "toml-inner-0012" }

[[claude_code.profiles]]
id = "p_1"
label = "Toml Label 0013"
config_dir = 'C:\c'

[accounts]
"who@example.org" = 1
"#;
    let (toml_out, toml_n) = r.toml(doc);
    for secret in [
        "owner@example.com",
        "sk-toml-0010",
        "toml-user-0011",
        "toml-inner-0012",
        "Toml Label 0013",
        "who@example.org",
    ] {
        assert!(
            !toml_out.contains(secret),
            "{secret:?} is still in:\n{toml_out}"
        );
    }
    // api_key, [auth], the label, the comment's and the key's addresses.
    assert!(toml_n >= 5, "{toml_n}:\n{toml_out}");
    let parsed: toml::Table = toml_out
        .parse()
        .unwrap_or_else(|e| panic!("the redacted suite.toml is not TOML ({e}):\n{toml_out}"));
    assert_eq!(parsed["providers"]["openai"]["model"].as_str(), Some("m"));
    assert_eq!(
        parsed["claude_code"]["profiles"][0]["id"].as_str(),
        Some("p_1")
    );
    assert_eq!(parsed["default_workspace"].as_str(), Some(r"C:\w"));

    // A suite.toml that doesn't parse is still redacted, line by line.
    let broken = "[auth]\nuser = \"broken-0014\"\n[[claude_code.profiles]]\nlabel = \"Broken Label 0015\"\nthis is = not toml = at all\n";
    let (broken_out, _) = r.toml(broken);
    for secret in ["broken-0014", "Broken Label 0015"] {
        assert!(!broken_out.contains(secret), "{secret:?}:\n{broken_out}");
    }
}

#[test] // spec: ARCH-121
fn the_redactor_cuts_deny_tokens() {
    let r = redactor(&[]);
    let line = r#"{"fields":{"message":"toast posted","launch":"lil-dev:deny/cf_0123456789abcdef/0123456789abcdef0123456789abcdef"}}"#;
    let (out, n) = r.text(line);
    assert!(
        out.contains("lil-dev:deny/cf_0123456789abcdef/<redacted>"),
        "{out}"
    );
    assert!(!out.contains("0123456789abcdef0123456789abcdef"), "{out}");
    assert_eq!(n, 1, "{out}");
    serde_json::from_str::<Value>(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    // A confirm URL holds no token and stays.
    let confirm = "lil-dev:confirm/cf_0123456789abcdef";
    assert_eq!(r.text(confirm), (confirm.to_string(), 0));
}

#[test] // spec: ARCH-121
fn profile_folder_and_machine_name_are_rewritten() {
    let env = TestEnv::new();
    let long = env.root().join("Profile Folder With A Long Name");
    write_file(&long.join("marker"), b"");
    let long_text = long.to_str().unwrap().to_string();
    let short = lil_platform::fs::short_name(&long).unwrap();
    let short_text = short.to_str().unwrap().to_string();
    let r = Redactor {
        labels: Vec::new(),
        profile_dir: Some(long_text.clone()),
        machine: Some("TESTBOX_7Q".to_string()),
        ..Redactor::default()
    };
    let json_escaped = serde_json::to_string(&format!(r"{long_text}\AppData\x.log")).unwrap();
    // A `Debug` path inside a JSON line: escaped twice.
    let debug_in_json =
        serde_json::to_string(&format!("{:?}", format!(r"{long_text}\AppData\x.log"))).unwrap();
    let forms = [
        format!(r"{long_text}\AppData\x.log"),
        format!(r"{short_text}\AppData\x.log"),
        format!(r"{long_text}\AppData\x.log").replace('\\', "/"),
        format!(r"\\?\{long_text}\AppData\x.log"),
        format!(r"{}\AppData\x.log", long_text.to_uppercase()),
        format!(r"{}\AppData\x.log", long_text.to_lowercase()),
        format!(r#"{{"fields":{{"path":{json_escaped}}}}}"#),
        format!(r#"{{"fields":{{"report":{debug_in_json}}}}}"#),
        format!(r"\\?\{}\AppData\x.log", long_text.replace('\\', r"\\")),
    ];
    for form in &forms {
        let (out, n) = r.text(form);
        assert!(out.contains("%USERPROFILE%"), "{form} -> {out}");
        assert!(n >= 1, "{form} -> {out}");
        let lower = out.to_lowercase();
        assert!(!lower.contains("profile folder with a long name"), "{out}");
        assert!(!lower.contains(r"\\?\"), "{out}");
        if short_text != long_text {
            let short_leaf = short.file_name().unwrap().to_str().unwrap().to_lowercase();
            assert!(!lower.contains(&short_leaf), "{out}");
        }
        assert!(
            out.contains("AppData") && out.contains("x.log"),
            "the rest of the path stays: {out}"
        );
        if form.starts_with('{') {
            serde_json::from_str::<Value>(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        }
    }

    // The computer name, in any case and as its machine id, as a whole word.
    let (out, n) =
        r.text("built on TESTBOX_7Q, logs jobs.testbox-7q.2026-10.jsonl, host testbox_7q.");
    assert_eq!(
        out,
        "built on <machine>, logs jobs.<machine>.2026-10.jsonl, host <machine>."
    );
    assert_eq!(n, 3);
    // A name of 6 characters or more goes inside a word too; a shorter one only whole.
    let (inside, n) = r.text("TESTBOX_7QX and XTESTBOX_7Q");
    assert_eq!((inside.as_str(), n), ("<machine>X and X<machine>", 2));
    let short = Redactor {
        machine: Some("PC7Q".to_string()),
        ..Redactor::default()
    };
    assert_eq!(
        short.text("PC7QX and XPC7Q and PC7Q").0,
        "PC7QX and XPC7Q and <machine>"
    );
}

// ---------------------------------------------------------------------------------------------
// Recent errors and the topic, in-process.
// ---------------------------------------------------------------------------------------------

fn diagnostics(env: &TestEnv, events: &Arc<EventBus>) -> Arc<Diagnostics> {
    let clock: Arc<dyn Clock> = FakeClock::new();
    Diagnostics::new(
        env.env.clone(),
        clock,
        events.clone(),
        DiagnosticsSources::default(),
    )
}

/// The diagnostics tools' rows (CORE-146, P4a-R24): `diagnostics.recent` reads and
/// `diagnostics.bundle` writes, both in the always-available module `diagnostics`. Pinned
/// here, not in the `tools` unit tests, so `tools` names no `diagnostics` type.
#[test] // spec: ARCH-121, CORE-058
fn the_diagnostics_tools_are_pinned() {
    let env = TestEnv::new();
    let mut reg = Registry::new();
    register_diagnostics_tools(
        &mut reg,
        &diagnostics(&env, &Arc::new(EventBus::new(FakeClock::new()))),
    );
    let tools: Vec<(String, EffectClass, String)> = reg
        .descriptors()
        .into_iter()
        .map(|d| (d.name, d.effect, d.module))
        .collect();
    let row = |name: &str, effect| (name.to_string(), effect, DIAGNOSTICS_MODULE.to_string());
    assert_eq!(
        tools,
        [
            row("diagnostics.bundle", EffectClass::Write),
            row("diagnostics.recent", EffectClass::Read),
        ]
    );
}

#[test] // spec: ARCH-121, QA-083
fn recent_groups_by_code_newest_first_over_the_last_1000() {
    let env = TestEnv::new();
    let d = diagnostics(&env, &Arc::new(EventBus::new(FakeClock::new())));
    let at0: i64 = 1_790_000_000_000;
    // Five that the ring forgets, then 1,000 kept: a, b and c in turn, c the newest.
    for i in 0..5 {
        d.record_error("old.code", Some("old.tool"), "forgotten", at0 + i);
    }
    for i in 0..1000i64 {
        let code = ["a.code", "b.code", "c.code"][(i % 3) as usize];
        let tool = (code != "b.code").then_some("x.tool");
        d.record_error(code, tool, &format!("message {i}"), at0 + 5 + i);
    }
    assert_eq!(RECENT_CAPACITY, 1000);
    let rows = d.recent(200);
    let codes: Vec<&str> = rows.iter().map(|r| r.code.as_str()).collect();
    // The newest is i = 999 (a), then 998 (c), then 997 (b).
    assert_eq!(codes, ["a.code", "c.code", "b.code"]);
    let counts: Vec<u32> = rows.iter().map(|r| r.count).collect();
    assert_eq!(counts, [334, 333, 333]);
    let a = &rows[0];
    assert_eq!(a.last_message, "message 999");
    assert_eq!(a.last_tool.as_deref(), Some("x.tool"));
    assert_eq!(a.first_at, lil_core::telemetry::format_ts(at0 + 5));
    assert_eq!(a.last_at, lil_core::telemetry::format_ts(at0 + 5 + 999));
    assert_eq!(rows[2].last_tool, None, "b.code has no tool");
    let json = serde_json::to_value(&rows[2]).unwrap();
    assert!(json.get("last_tool").is_none(), "{json}");
    assert_eq!(d.recent(1).len(), 1);
}

/// Review M-6: a tool call's error message is kept redacted, as a job's is, so every row of
/// `diagnostics.recent` reads the same way.
#[test] // spec: ARCH-121, QA-083
fn a_tool_calls_error_message_is_kept_redacted() {
    let env = TestEnv::new();
    let d = diagnostics(&env, &Arc::new(EventBus::new(FakeClock::new())));
    let profile = env.env.user_profile().display().to_string();
    d.record_error(
        "store.io",
        Some("x.tool"),
        &format!("token: SECRET-PROBE-62 in {profile}\\notes.txt"),
        1_790_000_000_000,
    );
    let message = &d.recent(1)[0].last_message;
    assert!(!message.contains("SECRET-PROBE-62"), "{message}");
    assert!(!message.contains(&profile), "{message}");
    assert!(message.contains("%USERPROFILE%"), "{message}");
}

#[tokio::test] // spec: ARCH-121, QA-083
async fn each_error_is_published_on_the_diagnostics_topic() {
    let env = TestEnv::new();
    let events = Arc::new(EventBus::new(FakeClock::new()));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(16);
    events.attach(7, tx);
    events
        .subscribe(7, &[DIAGNOSTICS_TOPIC.to_string()], &BTreeMap::new())
        .await;
    let d = diagnostics(&env, &events);
    let at = 1_790_000_000_123;
    d.record_error("tool.unknown", Some("no.such"), "There is no tool", at);
    d.record_error("params.invalid", None, "bad", at + 1);
    let mut seen = Vec::new();
    for _ in 0..2 {
        let line = tokio::time::timeout(DEADLINE, rx.recv())
            .await
            .expect("an event in time")
            .expect("the bus is open");
        let message: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(message["params"]["topic"], DIAGNOSTICS_TOPIC, "{message}");
        seen.push(message["params"]["data"].clone());
    }
    assert_eq!(
        seen[0],
        json!({"kind": "error", "code": "tool.unknown", "tool": "no.such",
               "at": lil_core::telemetry::format_ts(at)})
    );
    assert_eq!(
        seen[1],
        json!({"kind": "error", "code": "params.invalid",
               "at": lil_core::telemetry::format_ts(at + 1)})
    );
}

// ---------------------------------------------------------------------------------------------
// The bundle, through a real core.
// ---------------------------------------------------------------------------------------------

/// A `cli` session of `env`'s core, in `ws` when given.
fn session(env: &TestEnv, ws: Option<&Path>) -> RawPipe {
    let mut pipe = RawPipe::connect(env);
    let workspace = ws.map_or_else(|| "none".to_string(), |ws| env.workspace_id(ws));
    let hello = pipe.hello(json!({ "workspace": workspace }));
    assert_eq!(hello["result"]["status"], "ok", "hello: {hello}");
    pipe
}

/// `diagnostics.bundle` with `args`, checked `ok`.
fn bundle(pipe: &mut RawPipe, args: Value, key: &str) -> Value {
    pipe.tool_ok("diagnostics.bundle", args, Some(key))
}

/// Every entry of the zip at `path`, by name, as text.
fn entries(path: &Path) -> BTreeMap<String, String> {
    let file = std::fs::File::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut zip = zip::ZipArchive::new(file).unwrap_or_else(|e| panic!("not a zip: {e}"));
    let mut found = BTreeMap::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).unwrap();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        found.insert(
            entry.name().expect("a UTF-8 entry name").into_owned(),
            String::from_utf8_lossy(&bytes).into_owned(),
        );
    }
    found
}

/// The bundle result's `path`.
fn path_of(result: &Value) -> PathBuf {
    PathBuf::from(
        result["path"]
            .as_str()
            .unwrap_or_else(|| panic!("no path: {result}")),
    )
}

/// The same folder, whatever its case or `\\?\` prefix.
fn same_folder(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        p.to_string_lossy()
            .replace('/', "\\")
            .trim_start_matches(r"\\?\")
            .trim_end_matches('\\')
            .to_ascii_lowercase()
    };
    norm(a) == norm(b)
}

const WS_CANARY: &str = "workspace-file-canary-41c7";
const SECRET: &str = "bundle-canary-secret-9d02";
const LABEL: &str = "Bundle Canary Label 3f5e";
const EMAIL: &str = "bundle.canary@example.com";

#[test] // spec: ARCH-121, QA-084
fn bundle_holds_the_listed_entries_and_no_workspace_file() {
    let env = TestEnv::new();
    let ws = env.make_workspace("diag");
    write_file(&ws.join("notes.txt"), WS_CANARY.as_bytes());
    env.register_workspace(&ws, true);
    let profile_dir = env.env.user_profile().join("claude-diag");
    env.suite_toml_set(&format!(
        "[bundle_redaction]\nSESSION_TOKEN = \"{SECRET}\"\nowner = \"{EMAIL}\"\n\
         folder = '{}'\n\n\
         [[claude_code.profiles]]\nid = \"p_diag\"\nlabel = \"{LABEL}\"\nconfig_dir = '{}'\n",
        env.env.user_profile().join("Documents").display(),
        profile_dir.display()
    ));
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, Some(&ws));
    let status = env.lil(&["status"]);
    assert_eq!(status.code, 0, "{status:?}");

    let result = bundle(&mut pipe, json!({}), "bundle-1");
    let path = path_of(&result);
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let stamp = name
        .strip_prefix("lilith-diagnostics-")
        .and_then(|s| s.strip_suffix(".zip"))
        .unwrap_or_else(|| panic!("{name}"));
    assert!(
        stamp.len() == 16 && stamp.as_bytes()[8] == b'T' && stamp.ends_with('Z'),
        "{name}"
    );
    assert!(
        same_folder(path.parent().unwrap(), &env.env.diagnostics_dir()),
        "{result}"
    );
    assert_eq!(
        result["bytes"].as_u64(),
        Some(std::fs::metadata(&path).unwrap().len()),
        "{result}"
    );
    assert!(result["redactions"].as_u64().unwrap_or(0) >= 3, "{result}");

    let found = entries(&path);
    let names: BTreeSet<String> = found.keys().cloned().collect();
    let listed: BTreeSet<String> = result["files"]
        .as_array()
        .unwrap_or_else(|| panic!("no files: {result}"))
        .iter()
        .map(|f| f.as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, listed, "`files` names the zip's entries");
    let fixed: BTreeSet<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|n| !n.starts_with("logs/"))
        .collect();
    assert_eq!(
        fixed,
        BTreeSet::from([
            "manifest.json",
            "versions.json",
            "suite.toml",
            "errors.json",
            "hardware.json",
            "engines.json",
        ]),
        "{names:?}"
    );
    let logs: Vec<&String> = names.iter().filter(|n| n.starts_with("logs/")).collect();
    for prefix in ["logs/lil-core.", "logs/lil."] {
        assert!(
            logs.iter()
                .any(|l| l.starts_with(prefix) && l.ends_with(".log")),
            "no {prefix}<date>.log in {logs:?}"
        );
    }
    for log in &logs {
        assert!(!log[5..].contains('/'), "{log}");
        for line in found[*log].lines() {
            serde_json::from_str::<Value>(line)
                .unwrap_or_else(|e| panic!("{log} has a line that is not JSON ({e}): {line}"));
        }
    }

    let manifest: Value = serde_json::from_str(&found["manifest.json"]).unwrap();
    assert_eq!(manifest["bundle"], 1, "{manifest}");
    assert_eq!(manifest["build_id"], lil_protocol::BUILD_ID, "{manifest}");
    assert_eq!(
        manifest["channel"],
        env.env.channel().to_string(),
        "{manifest}"
    );
    assert_eq!(
        manifest["protocol"],
        lil_protocol::PROTOCOL.to_string(),
        "{manifest}"
    );
    assert!(
        manifest["created"]
            .as_str()
            .is_some_and(|c| c.ends_with('Z'))
    );
    let versions: Value = serde_json::from_str(&found["versions.json"]).unwrap();
    assert_eq!(
        versions["core"]["build_id"],
        lil_protocol::BUILD_ID,
        "{versions}"
    );
    for key in [
        "apps",
        "comfyui",
        "qt",
        "qt_webengine",
        "kddockwidgets",
        "driver",
    ] {
        assert!(versions.get(key).is_some(), "{key}: {versions}");
    }
    let errors: Value = serde_json::from_str(&found["errors.json"]).unwrap();
    assert!(errors["errors"].is_array(), "{errors}");
    let hardware: Value = serde_json::from_str(&found["hardware.json"]).unwrap();
    assert!(hardware["ts"].is_string(), "{hardware}");
    let engines: Value = serde_json::from_str(&found["engines.json"]).unwrap();
    assert!(engines["status"].is_array(), "{engines}");
    assert!(engines.get("environment").is_some(), "{engines}");

    let suite_toml = &found["suite.toml"];
    assert!(suite_toml.contains("<redacted>"), "{suite_toml}");
    assert!(suite_toml.contains("%USERPROFILE%"), "{suite_toml}");
    assert!(suite_toml.contains("p_diag"), "{suite_toml}");
    let profile_text = env.env.user_profile().display().to_string().to_lowercase();
    for (entry, text) in &found {
        for canary in [WS_CANARY, SECRET, LABEL, EMAIL] {
            assert!(!text.contains(canary), "{entry} holds {canary:?}");
        }
        assert!(
            !text.to_lowercase().contains(&profile_text),
            "{entry} holds the profile folder"
        );
    }
}

/// Every file under `dir` with its length and modification time.
fn tree(dir: &Path) -> BTreeMap<PathBuf, (u64, SystemTime)> {
    let mut found = BTreeMap::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            found.extend(tree(&path));
        } else {
            found.insert(path, (meta.len(), meta.modified().unwrap()));
        }
    }
    found
}

#[test] // spec: ARCH-121, QA-084
fn bundle_writes_only_under_the_diagnostics_folder() {
    let env = TestEnv::new();
    let ws = env.make_workspace("only");
    env.register_workspace(&ws, true);
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, Some(&ws));
    // A first call, so the workspace is open before the trees are read; `limit` is 1–200.
    pipe.tool_ok("diagnostics.recent", json!({"limit": 200}), None);
    for limit in [0, 201] {
        let refused = pipe.tool("diagnostics.recent", json!({"limit": limit}), None);
        assert_eq!(refused["error"]["code"], "params.invalid", "{refused}");
    }
    let before = tree(env.root());

    let result = bundle(&mut pipe, json!({}), "only-1");
    let zip = path_of(&result);
    let after = tree(env.root());

    let logs = env.env.logs_dir();
    let state_file = |p: &Path| {
        let name = p
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_ascii_lowercase();
        name.ends_with(".db") || name.ends_with(".db-wal") || name.ends_with(".db-shm")
    };
    let changed: Vec<&PathBuf> = after
        .iter()
        .filter(|(p, meta)| before.get(*p) != Some(meta))
        .map(|(p, _)| p)
        .filter(|p| !same_folder(p.parent().unwrap(), &logs) && !state_file(p))
        .collect();
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert!(same_folder(changed[0], &zip), "{changed:?} {zip:?}");
    assert!(
        same_folder(zip.parent().unwrap(), &env.env.diagnostics_dir()),
        "{zip:?}"
    );
    // Nothing in the workspace changed, and no temp file was left.
    for (p, meta) in &before {
        if p.starts_with(&ws) {
            assert_eq!(after.get(p), Some(meta), "{} changed", p.display());
        }
    }
    let leftovers: Vec<String> = std::fs::read_dir(env.env.diagnostics_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('.'))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

/// Waits until job `id` has finished; returns its summary.
fn wait_done(pipe: &mut RawPipe, id: &str) -> Value {
    let until = Instant::now() + DEADLINE;
    loop {
        let waited = pipe.tool_ok("job.wait", json!({"job_id": id, "timeout_s": 5}), None);
        if waited["done"] == true {
            return waited["job"].clone();
        }
        assert!(Instant::now() < until, "job {id} did not finish: {waited}");
    }
}

/// Checkpoint A ruling (conformance M-4): a job's final failure is a recent error, with its
/// kind as the tool, its id and its message redacted; a cancelled job adds none.
#[test] // spec: QA-083, ARCH-121
fn a_jobs_failure_is_a_recent_error_with_its_id_and_a_redacted_message() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);
    let failed = pipe.tool_ok(
        "test.job",
        json!({"lane": "cpu", "fail": "final",
               "fail_message": "the node said api_key=SECRET-PROBE-41 and gave up"}),
        Some("job-fail-1"),
    );
    let id = failed["job"]["job_id"].as_str().unwrap().to_string();
    assert_eq!(wait_done(&mut pipe, &id)["status"], "failed");
    let cancelled = pipe.tool_ok(
        "test.job",
        json!({"lane": "cpu", "gated": true}),
        Some("job-cancel-1"),
    );
    let gated = cancelled["job"]["job_id"].as_str().unwrap().to_string();
    pipe.tool_ok("job.cancel", json!({"job_id": gated}), Some("cancel-1"));
    assert_eq!(wait_done(&mut pipe, &gated)["status"], "cancelled");

    let recent = pipe.tool_ok("diagnostics.recent", json!({}), None);
    let errors = recent["errors"].as_array().unwrap();
    let job_rows: Vec<&Value> = errors
        .iter()
        .filter(|g| g.get("last_job_id").is_some())
        .collect();
    assert_eq!(job_rows.len(), 1, "only the failure: {recent}");
    let row = job_rows[0];
    assert_eq!(row["code"], "params.invalid", "{row}");
    assert_eq!(row["last_tool"], "test.job", "{row}");
    assert_eq!(row["last_job_id"], id.as_str(), "{row}");
    let message = row["last_message"].as_str().unwrap();
    assert!(!message.contains("SECRET-PROBE-41"), "{message}");
    assert!(message.contains("<redacted>"), "{message}");
}

#[test] // spec: QA-084, ARCH-121
fn bundle_with_a_job_adds_its_summary_and_untrusted_args() {
    let env = TestEnv::new();
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);
    let submitted = pipe.tool_ok(
        "test.job",
        json!({"lane": "cpu", "fail": "final", "estimate_key": "args-canary@example.net"}),
        Some("job-1"),
    );
    let id = submitted["job"]["job_id"].as_str().unwrap().to_string();
    let job = wait_done(&mut pipe, &id);
    assert_eq!(job["status"], "failed", "{job}");

    let result = bundle(&mut pipe, json!({"job_id": id}), "bundle-job-1");
    let found = entries(&path_of(&result));
    assert!(
        result["files"]
            .as_array()
            .unwrap()
            .contains(&json!("job.json")),
        "{result}"
    );
    let entry: Value = serde_json::from_str(&found["job.json"]).unwrap();
    assert_eq!(entry["job"]["job_id"], id.as_str(), "{entry}");
    assert_eq!(entry["job"]["status"], "failed", "{entry}");
    assert_eq!(entry["tool"], "test.job", "{entry}");
    assert_eq!(entry["arguments"]["lane"], "cpu", "{entry}");
    assert_eq!(entry["untrusted"], true, "{entry}");
    assert!(
        !found["job.json"].contains("args-canary@example.net"),
        "the arguments are redacted too: {entry}"
    );
    // Its error is among the recent ones.
    let errors: Value = serde_json::from_str(&found["errors.json"]).unwrap();
    assert!(errors["errors"].is_array(), "{errors}");

    // An unknown job is job.unknown, and no bundle is written for it.
    let unknown = pipe.tool(
        "diagnostics.bundle",
        json!({"job_id": "j_00000000000000ff"}),
        Some("bundle-job-2"),
    );
    assert_eq!(unknown["error"]["code"], "job.unknown", "{unknown}");
}

/// Polls the core's sockets on a thread until stopped; every socket table it saw that wasn't
/// empty is kept (the test network guard, ARCH-122).
struct NetworkGuard {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<Vec<String>>>,
}

impl NetworkGuard {
    fn watch(pid: u32) -> NetworkGuard {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut seen = Vec::new();
            let mut polls = 0u64;
            while !flag.load(Ordering::SeqCst) || polls == 0 {
                match lil_platform::sockets::sockets_of(pid) {
                    Ok(sockets) if sockets.is_empty() => {}
                    Ok(sockets) => seen.push(format!("{sockets:?}")),
                    Err(e) => seen.push(format!("the socket tables can't be read: {e}")),
                }
                polls += 1;
            }
            seen
        });
        NetworkGuard {
            stop,
            thread: Some(thread),
        }
    }

    /// Stops polling; what it saw.
    fn finish(mut self) -> Vec<String> {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap()
    }
}

#[test] // spec: ARCH-122, QA-085
fn nothing_is_sent_anywhere() {
    let env = TestEnv::new();
    let core = CoreFixture::start(&env);
    let mut pipe = session(&env, None);
    pipe.tool_ok("diagnostics.recent", json!({}), None);
    let guard = NetworkGuard::watch(core.pid());
    let result = bundle(&mut pipe, json!({}), "net-1");
    let seen = guard.finish();
    assert!(seen.is_empty(), "the core opened sockets: {seen:?}");
    assert!(path_of(&result).exists(), "{result}");
    let after = lil_platform::sockets::sockets_of(core.pid()).unwrap();
    assert!(after.is_empty(), "{after:?}");
}

// ---------------------------------------------------------------------------------------------
// Canaries, names, retention, the log tail and the network.
// ---------------------------------------------------------------------------------------------

/// The canaries of [`every_canary_is_gone_from_every_entry`], one or more per redaction
/// stage, built at run time so no token-shaped literal is in the source.
struct Canaries {
    machine: String,
    label: String,
    config_dir: String,
    /// Every canary text, each kind once; the planted texts are built from it.
    parts: Vec<String>,
    /// What no entry, and no entry name, may hold, in any letter case.
    forbidden: Vec<String>,
    /// How many profile-folder paths [`Canaries::parts`] holds.
    paths: usize,
}

fn canaries(env: &TestEnv) -> Canaries {
    use base64::Engine as _;
    let machine = "CANARYBOX-7Q".to_string();
    let label = "Canary Label Kq7".to_string();
    let config_dir = env.root().join("canary-claude-dir").display().to_string();
    let profile = env.env.user_profile().display().to_string();
    let picture = format!(r"{profile}\Pictures\canary-pic.png");
    let sid = "S-1-5-21-4111111111-4222222222-4333333333-1042".to_string();
    let tokens = vec![
        format!("ghp_{}", "Ca1".repeat(12)),
        format!("gho_{}", "Cb2".repeat(12)),
        format!("ghu_{}", "Cc3".repeat(12)),
        format!("ghs_{}", "Cd4".repeat(12)),
        format!("ghr_{}", "Ce5".repeat(12)),
        format!("github_pat_11{}", "Cf6_".repeat(15)),
        format!("hf_{}", "Cg7".repeat(12)),
        format!("sk-ant-api03-{}", "Ch8-".repeat(12)),
        format!("sk-proj-{}", "Ci9".repeat(12)),
        format!("AGE-SECRET-KEY-1{}", "CJ0".repeat(20)),
        format!("xoxb-{}", "Cl2-".repeat(6)),
        format!("AKIA{}", "CM3".repeat(5) + "N"),
        format!(
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJjYW5hcnkifQ.{}",
            "Ck1".repeat(10)
        ),
    ];
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&picture);
    let open = format!("lil-{}:open/{payload}", env.env.channel());
    let deny_token = "c0ffee".repeat(5) + "ab";
    let deny = format!("lil-dev:deny/cf_0123456789abcdef/{deny_token}");
    let target = "LILITH-dev/canary-target-Zq9".to_string();
    let email = "canary.person@example.com".to_string();
    let pct_email = "canary.pct%40example.com".to_string();
    let secret = "canary-secret-value-5e1b".to_string();
    let mut parts = vec![
        // The profile folder, four ways: each becomes `%USERPROFILE%`.
        picture.clone(),
        picture.replace('\\', "/"),
        picture.to_uppercase(),
        picture.replace('\\', "%5C").replace(':', "%3A"),
        // The account name alone.
        "profile".to_string(),
        label.clone(),
        config_dir.clone(),
        machine.clone(),
        sid.clone(),
        open,
        deny,
        target,
        email.clone(),
        pct_email,
        format!("api_key={secret}"),
    ];
    parts.extend(tokens.iter().cloned());
    let mut forbidden = vec![
        profile.clone(),
        profile.replace('\\', "/"),
        profile.replace('\\', r"\\"),
        profile.replace('\\', "%5C"),
        label.clone(),
        config_dir.clone(),
        config_dir.replace('\\', r"\\"),
        machine.clone(),
        sid,
        payload,
        deny_token,
        "canary-target-zq9".to_string(),
        email,
        "canary.pct".to_string(),
        secret,
    ];
    forbidden.extend(tokens);
    Canaries {
        machine,
        label,
        config_dir,
        parts,
        forbidden,
        paths: 4,
    }
}

/// Whether `word` stands in `text` as a whole word (any case).
fn has_word(text: &str, word: &str) -> bool {
    let (text, word) = (text.to_lowercase(), word.to_lowercase());
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(&word).any(|(i, _)| {
        !text[..i].chars().next_back().is_some_and(is_word)
            && !text[i + word.len()..].chars().next().is_some_and(is_word)
    })
}

#[test] // spec: ARCH-121, QA-084, ARCH-119
fn every_canary_is_gone_from_every_entry() {
    let env = TestEnv::new();
    let canary = canaries(&env);
    env.suite_toml_set(&format!(
        "[[claude_code.profiles]]\nid = \"p_canary\"\nlabel = \"{}\"\nconfig_dir = '{}'\n",
        canary.label, canary.config_dir
    ));
    let _core = CoreFixture::start_with_env(&env, &[("COMPUTERNAME", &canary.machine)]);
    let mut pipe = session(&env, None);
    // Each canary after a space, after a `\n` (an escape before a value, N1), after an ANSI
    // colour code and after `%3D`.
    let spaced = canary.parts.join(" ");
    let lined = canary.parts.join("\n");
    let coloured = format!("\u{1b}[32m{}", canary.parts.join("\u{1b}[0m\u{1b}[32m"));
    let encoded = canary.parts.join("%3D");

    // A log line holding the canaries five times: plain, `Debug`-escaped, line by line,
    // coloured and percent-joined; and a log whose file name holds the account name.
    let today = lil_schema::time::date(
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
    );
    let line = json!({"fields": {"message": spaced, "report": format!("{spaced:?}"),
                                 "lines": lined, "coloured": coloured, "encoded": encoded}});
    write_file(
        &env.env.logs_dir().join(format!("plant.{today}.log")),
        format!("{line}\n").as_bytes(),
    );
    write_file(
        &env.env
            .logs_dir()
            .join(format!("profile-tools.{today}.log")),
        b"{\"fields\":{\"message\":\"a log under the account's name\"}}\n",
    );
    // An error message, a job's arguments and a panic.
    let unknown = pipe.tool(&format!("no.such {spaced}"), json!({}), None);
    assert_eq!(unknown["error"]["code"], "tool.unknown", "{unknown}");
    let submitted = pipe.tool_ok(
        "test.job",
        json!({"lane": "cpu", "fail": "final", "estimate_key": lined}),
        Some("canary-job"),
    );
    let id = submitted["job"]["job_id"].as_str().unwrap().to_string();
    wait_done(&mut pipe, &id);
    let panicked = pipe.tool("test.panic", json!({"message": lined}), None);
    assert_eq!(panicked["error"]["code"], "core.bug", "{panicked}");

    let result = bundle(&mut pipe, json!({"job_id": id}), "canary-bundle");
    let found = entries(&path_of(&result));
    let plant = found
        .get(&format!("logs/plant.{today}.log"))
        .unwrap_or_else(|| panic!("no plant log: {:?}", found.keys()));
    for entry in ["errors.json", "job.json"] {
        assert!(found.contains_key(entry), "no {entry}: {:?}", found.keys());
    }
    assert!(
        found["errors.json"].contains("core.bug"),
        "{}",
        found["errors.json"]
    );
    assert!(
        found
            .iter()
            .any(|(name, text)| name.starts_with("logs/lil-core.") && text.contains("\"panic\"")),
        "the panic is in the core's log"
    );

    // No canary in any entry or entry name.
    for (name, text) in &found {
        let (name_l, text_l) = (name.to_lowercase(), text.to_lowercase());
        for c in &canary.forbidden {
            let c = c.to_lowercase();
            assert!(!name_l.contains(&c), "the entry name {name} holds {c:?}");
            assert!(!text_l.contains(&c), "{name} holds {c:?}:\n{text}");
        }
        assert!(!name_l.contains("profile-tools"), "{name}");
        let without = text.replace("%USERPROFILE%", "");
        assert!(
            !has_word(&without, "profile"),
            "{name} holds the account name:\n{text}"
        );
    }
    assert!(
        found.contains_key(&format!("logs/redacted-1.{today}.log")),
        "the log under the account's name is renamed: {:?}",
        found.keys()
    );

    // Each stage did its part in the plant log (so skipping one stage fails here), and no
    // terminal control sequence is left.
    assert!(!plant.contains(r"\u001b"), "{plant}");
    let copies = 5;
    assert_eq!(
        plant.matches("%USERPROFILE%").count(),
        canary.paths * copies,
        "each planted profile path:\n{plant}"
    );
    for (placeholder, at_least) in [
        ("<user>", copies),
        ("<sid>", copies),
        ("<machine>", copies),
        ("open/<redacted>", copies),
        ("deny/cf_0123456789abcdef/<redacted>", copies),
    ] {
        assert!(
            plant.matches(placeholder).count() >= at_least,
            "{placeholder} at least {at_least} times:\n{plant}"
        );
    }
}

#[tokio::test] // spec: ARCH-121, P4a-R19
async fn a_bundle_in_the_same_second_never_replaces_another() {
    let env = TestEnv::new();
    let events = Arc::new(EventBus::new(FakeClock::new()));
    // The fake clock never moves: every bundle has the same second.
    let d = diagnostics(&env, &events);
    let ctx = common::ctx(&env, lil_protocol::CallerKind::Cli, &events);
    let first = d.bundle(&ctx, None).await.unwrap();
    let first_bytes = std::fs::read(&first.path).unwrap();
    d.record_error("x.changed", None, "the later bundles differ", 1);
    let mut made = vec![first.path.clone()];
    for _ in 0..14 {
        made.push(d.bundle(&ctx, None).await.unwrap().path);
    }
    // Fifteen paths, never one twice, each still there; the first one's bytes as they were.
    let distinct: BTreeSet<&String> = made.iter().collect();
    assert_eq!(distinct.len(), made.len(), "{made:?}");
    for (k, path) in made.iter().enumerate().skip(1) {
        assert_eq!(
            *path,
            first.path.replace(".zip", &format!("-{}.zip", k + 1)),
            "{made:?}"
        );
    }
    for path in &made {
        assert!(Path::new(path).exists(), "{path} was removed");
    }
    assert_eq!(
        std::fs::read(&first.path).unwrap(),
        first_bytes,
        "the first is kept"
    );
    assert!(entries(Path::new(&made[1]))["errors.json"].contains("x.changed"));

    // A file someone else put at the next name is skipped, never replaced.
    let next = first.path.replace(".zip", "-16.zip");
    write_file(Path::new(&next), b"not a bundle");
    let after = d.bundle(&ctx, None).await.unwrap();
    assert_eq!(after.path, first.path.replace(".zip", "-17.zip"));
    assert_eq!(std::fs::read(&next).unwrap(), b"not a bundle");
}

/// The folder keeps the newest 10 bundles, by time stamp and then number compared as a number:
/// a burst keeps every bundle of it while they are young, and once they are older than a
/// minute, a bundle a second later trims them back to 10, the oldest first.
#[tokio::test] // spec: ARCH-121, P4a-R19
async fn a_bundle_a_second_later_trims_the_folder_back_to_ten() {
    let env = TestEnv::new();
    let clock = FakeClock::new();
    let events = Arc::new(EventBus::new(clock.clone()));
    let d = Diagnostics::new(
        env.env.clone(),
        clock.clone(),
        events.clone(),
        DiagnosticsSources::default(),
    );
    let ctx = common::ctx(&env, lil_protocol::CallerKind::Cli, &events);
    let notes = env.env.diagnostics_dir().join("notes.txt");
    write_file(&notes, b"mine");
    let mut burst = Vec::new();
    for _ in 0..15 {
        burst.push(d.bundle(&ctx, None).await.unwrap().path);
    }
    let listed = || -> BTreeSet<String> {
        std::fs::read_dir(env.env.diagnostics_dir())
            .unwrap()
            .map(|e| e.unwrap().path().display().to_string())
            .collect()
    };
    let mut want: BTreeSet<String> = burst.iter().cloned().collect();
    want.insert(notes.display().to_string());
    assert_eq!(listed(), want, "a burst of 15 in one second keeps all 15");

    // Made 2 minutes ago, as far as the file system knows: old enough to remove.
    for path in &burst {
        created_ago(Path::new(path), Duration::from_secs(120));
    }
    clock.advance(Duration::from_secs(1));
    let later = d.bundle(&ctx, None).await.unwrap().path;
    // The oldest go: no suffix, then -2 … -6 (numbers, so -10 … -15 are newer than -2).
    let mut want: BTreeSet<String> = burst[6..].iter().cloned().collect();
    want.insert(later.clone());
    want.insert(notes.display().to_string());
    assert_eq!(listed(), want);
    assert!(!later.ends_with("-2.zip"), "{later}");
}

/// Sets the file system's creation time of `path` to `at`.
#[allow(clippy::disallowed_methods)]
fn created_at(path: &Path, at: SystemTime) {
    use std::os::windows::fs::FileTimesExt as _;
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_times(std::fs::FileTimes::new().set_created(at))
        .unwrap();
}

/// Sets the file system's creation time of `path` to `ago` before now.
fn created_ago(path: &Path, ago: Duration) {
    created_at(path, SystemTime::now() - ago);
}

/// No bundle removes the one made most recently before it (by creation time), whatever the
/// clocks say. The production form of a clock moved back: the bundles made while it ran an
/// hour ahead carry that hour in their names and in their files' creation times, so they stay
/// young; still, a bundle minutes after the last one never removes it.
#[tokio::test] // spec: ARCH-121, P4a-R19
async fn a_bundle_never_removes_the_one_made_just_before_it() {
    let env = TestEnv::new();
    let clock = FakeClock::new();
    let events = Arc::new(EventBus::new(clock.clone()));
    let d = Diagnostics::new(
        env.env.clone(),
        clock.clone(),
        events.clone(),
        DiagnosticsSources::default(),
    );
    let ctx = common::ctx(&env, lil_protocol::CallerKind::Cli, &events);
    clock.shift_utc(3600);
    for _ in 0..10 {
        let path = d.bundle(&ctx, None).await.unwrap().path;
        created_at(
            Path::new(&path),
            SystemTime::now() + Duration::from_secs(3600),
        );
        clock.advance(Duration::from_secs(1));
    }
    clock.shift_utc(-3700);
    // A, then B made "4 minutes" after it, then C "2 minutes" after B.
    let a = d.bundle(&ctx, None).await.unwrap().path;
    created_ago(Path::new(&a), Duration::from_secs(240));
    clock.advance(Duration::from_secs(240));
    let b = d.bundle(&ctx, None).await.unwrap().path;
    assert!(Path::new(&a).exists(), "B removed A");
    created_ago(Path::new(&b), Duration::from_secs(120));
    clock.advance(Duration::from_secs(120));
    let c = d.bundle(&ctx, None).await.unwrap().path;
    assert!(Path::new(&b).exists(), "C removed B");
    assert!(Path::new(&c).exists(), "{c}");
}

/// The diagnostics folder's files.
fn diagnostics_files(env: &TestEnv) -> BTreeSet<String> {
    std::fs::read_dir(env.env.diagnostics_dir())
        .unwrap()
        .map(|e| e.unwrap().path().display().to_string())
        .collect()
}

/// Retention never removes a bundle whose file was made less than 60 s ago, judged by the
/// file system's creation time, not the name's time stamp. After the clock moves back, the
/// bundles stamped later sort as the newest; still no bundle removes the one made before it,
/// and the older files go instead.
#[tokio::test] // spec: ARCH-121, P4a-R19
async fn a_clock_moved_back_never_makes_a_bundle_remove_the_one_before_it() {
    let env = TestEnv::new();
    let clock = FakeClock::new();
    let events = Arc::new(EventBus::new(clock.clone()));
    let d = Diagnostics::new(
        env.env.clone(),
        clock.clone(),
        events.clone(),
        DiagnosticsSources::default(),
    );
    let ctx = common::ctx(&env, lil_protocol::CallerKind::Cli, &events);
    // Ten bundles an hour ahead, made 2 minutes ago as far as the file system knows.
    clock.shift_utc(3600);
    let mut ahead = Vec::new();
    for _ in 0..10 {
        ahead.push(d.bundle(&ctx, None).await.unwrap().path);
        clock.advance(Duration::from_secs(1));
    }
    for path in &ahead {
        created_ago(Path::new(path), Duration::from_secs(120));
    }
    // The clock goes back past them; three bundles a second apart.
    clock.shift_utc(-3700);
    let mut made: Vec<String> = Vec::new();
    for _ in 0..3 {
        made.push(d.bundle(&ctx, None).await.unwrap().path);
        for path in &made {
            assert!(Path::new(path).exists(), "{path} was removed: {made:?}");
        }
        clock.advance(Duration::from_secs(1));
    }
    // Back to 10: the three new ones, and the newest seven by time stamp.
    let want: BTreeSet<String> = ahead[3..].iter().chain(&made).cloned().collect();
    assert_eq!(diagnostics_files(&env), want);
}

/// A message is redacted whole before the 2,000-character cut, and the cut never leaves part
/// of a word, so no fragment of a secret or a path is left that no rule knows. A label with a
/// space straddling the cut shows that the order matters: cut first, its first word would
/// stay.
#[tokio::test] // spec: ARCH-121, QA-084
async fn a_long_error_message_is_redacted_before_it_is_cut() {
    let env = TestEnv::new();
    env.suite_toml_set(
        "[[claude_code.profiles]]\nid = \"p_cut\"\nlabel = \"Straddle Qz7 Label\"\n",
    );
    let events = Arc::new(EventBus::new(FakeClock::new()));
    let d = diagnostics(&env, &events);
    let token = format!("ghp_{}", "Zz9".repeat(12));
    // The token straddles the cut.
    let message = format!("{} {token} after", "x".repeat(1990));
    d.record_error("long.message", Some("x.tool"), &message, 1);
    let shown = &d.recent(1)[0].last_message;
    assert!(shown.chars().count() <= 2000, "{}", shown.len());
    assert!(
        !shown.contains("ghp_"),
        "the tool shows no fragment: {shown}"
    );
    let ctx = common::ctx(&env, lil_protocol::CallerKind::Cli, &events);
    let result = d.bundle(&ctx, None).await.unwrap();
    let errors: Value =
        serde_json::from_str(&entries(Path::new(&result.path))["errors.json"]).unwrap();
    let last = errors["errors"][0]["last_message"].as_str().unwrap();
    assert!(!last.contains("ghp_") && !last.contains("Zz9"), "{last}");
    assert!(last.chars().count() <= 2000, "{}", last.len());

    // The label straddles the cut: the cut keeps 1,999 characters and goes back to the last
    // space, so cut first, "Straddle" (characters 1,989 to 1,996) would stay and the rest go.
    let message = format!("{} Straddle Qz7 Label after", "x".repeat(1988));
    d.record_error("long.label", None, &message, 2);
    let result = d.bundle(&ctx, None).await.unwrap();
    let text = &entries(Path::new(&result.path))["errors.json"];
    let errors: Value = serde_json::from_str(text).unwrap();
    assert_eq!(errors["errors"][0]["code"], "long.label", "{errors}");
    for part in ["Straddle", "Qz7", "Label"] {
        assert!(!text.contains(part), "{part} is in errors.json:\n{text}");
    }
}

#[test] // spec: ARCH-121
fn read_tail_keeps_the_newest_bytes_from_a_line_start() {
    use lil_core::store::fsops::read_tail;
    let env = TestEnv::new();
    let log = env.root().join("t.log");
    write_file(&log, b"aaaa\nbbbb\ncccc\n");
    let tail = |max| String::from_utf8(read_tail(&log, max).unwrap()).unwrap();
    assert_eq!(tail(100), "aaaa\nbbbb\ncccc\n", "a shorter file whole");
    assert_eq!(tail(15), "aaaa\nbbbb\ncccc\n", "exactly max");
    assert_eq!(tail(10), "bbbb\ncccc\n", "a window that starts at a line");
    assert_eq!(tail(7), "cccc\n", "cut to the next line");
    assert_eq!(tail(3), "", "no line starts in the window");
    let one = env.root().join("one.log");
    write_file(&one, b"no newline at all");
    assert_eq!(String::from_utf8(read_tail(&one, 5).unwrap()).unwrap(), "");
}

/// The diagnostics module can't send anything: it names no network API (ARCH-122).
#[test] // spec: ARCH-122, QA-085
fn the_diagnostics_module_names_no_network_api() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("diagnostics");
    let mut read = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        read += 1;
        for api in [
            "reqwest",
            "TcpStream",
            "TcpListener",
            "UdpSocket",
            "std::net",
            "tokio::net",
            "hyper",
            "tungstenite",
            "WinHttp",
            "WSAStartup",
        ] {
            assert!(!text.contains(api), "{} names {api}", path.display());
        }
    }
    assert!(read >= 4, "{read} files in {}", dir.display());
}
