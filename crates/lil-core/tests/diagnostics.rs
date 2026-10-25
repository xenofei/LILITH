//! Diagnostics (ARCH-117–122, QA-082–085; P4a-R19, P4a-R20; Appendix G "Diagnostics"): the
//! bundle zip and what it holds, the redaction of every entry (scrub-word keys, Claude Code
//! profile labels, e-mail addresses, Credential Manager targets, Deny URL tokens, the profile
//! folder and the computer name), recent errors by code, the `diagnostics` topic, and a bundle
//! run that opens no socket. The redactor is driven directly with hostile fixtures; the bundle
//! through a real test-channel core.

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
        profile_dir: None,
        machine: None,
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
    let (kept, none) = r.text("TESTBOX_7QX and XTESTBOX_7Q");
    assert_eq!((kept.as_str(), none), ("TESTBOX_7QX and XTESTBOX_7Q", 0));
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
        "[bundle_redaction]\nSESSION_TOKEN = \"{SECRET}\"\nowner = \"{EMAIL}\"\n\n\
         [[claude_code.profiles]]\nid = \"p_diag\"\nlabel = \"{LABEL}\"\nconfig_dir = '{}'\n",
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
