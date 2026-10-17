//! Workspace tools: `ww workspace init|use`, the default workspace in `suite.toml`, and how a
//! session that names another workspace is told to switch (spec ARCH-090, ARCH-098, CORE-007,
//! QA-003; Global Constraints "The `ww` commands this plan adds"). Driven through the real
//! `ww` (`TestEnv::ww`), `RawPipe` and the files on disk (QA-018).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use ww_testkit::{CoreFixture, RawPipe, TestEnv, WwOutput};

/// The five `.gitignore` entries `workspace.create` writes (Global Constraints, CORE-040).
const GITIGNORE_ENTRIES: [&str; 5] = [
    ".worldwright/",
    ".trash/",
    "inbox/*",
    "projects/*/assets/*/candidates/",
    "projects/*/assets/*/work/",
];

/// A fresh idempotency key for every write call (ARCH-035).
fn key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "spec-ws-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// `path` as the suite shows a workspace path: canonical, without the `\\?\` prefix.
fn canon(path: &Path) -> String {
    let canonical = std::fs::canonicalize(path)
        .unwrap_or_else(|e| panic!("cannot canonicalise {}: {e}", path.display()));
    let text = canonical.to_string_lossy().into_owned();
    text.strip_prefix(r"\\?\")
        .map(str::to_string)
        .unwrap_or(text)
}

/// True when `a` and `b` name the same existing folder.
fn same_folder(a: &Path, b: &Path) -> bool {
    canon(a).eq_ignore_ascii_case(&canon(b))
}

/// The lines of a text file, without line endings.
fn lines_of(path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    text.lines()
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect()
}

fn path_arg(path: &Path) -> &str {
    path.to_str()
        .unwrap_or_else(|| panic!("{} is not valid UTF-8", path.display()))
}

/// `ww`'s stdout as JSON.
fn stdout_json(out: &WwOutput) -> Value {
    serde_json::from_str(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {out:?}"))
}

/// A connection whose hello names the workspace `ws`, checked to succeed.
fn session(env: &TestEnv, ws: &Path) -> RawPipe {
    let mut pipe = RawPipe::connect(env);
    let hello = pipe.hello(json!({"workspace": env.workspace_id(ws)}));
    assert_eq!(
        hello["result"]["status"],
        "ok",
        "hello naming {}: {hello}",
        ws.display()
    );
    pipe
}

/// Two fixture workspaces: the first registered as the default, the second registered only.
fn two_workspaces(env: &TestEnv) -> (PathBuf, PathBuf) {
    let ws1 = env.copy_fixture_workspace("one");
    let ws2 = env.copy_fixture_workspace("two");
    env.register_workspace(&ws1, true);
    env.register_workspace(&ws2, false);
    (ws1, ws2)
}

#[test] // spec: CORE-007, ARCH-098
fn workspace_init_writes_layout_and_registers_default() {
    let env = TestEnv::new();
    let ws = env.root().join("Ünïcödé space ws");

    let out = env.ww(&["workspace", "init", path_arg(&ws)]);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(
        out.stdout.contains("workspace created at "),
        "`ww workspace init` should say it created the workspace: {out:?}"
    );
    assert!(
        out.stdout.contains("default workspace: "),
        "the first workspace becomes the default and `ww` says so: {out:?}"
    );

    for rel in [
        "worldwright.yaml",
        ".gitignore",
        ".gitattributes",
        r"projects\sketchbook\project.yaml",
        r"projects\sketchbook\progress.md",
    ] {
        assert!(ws.join(rel).is_file(), "{rel} is missing after init");
    }
    let gitignore = lines_of(&ws.join(".gitignore"));
    for entry in GITIGNORE_ENTRIES {
        assert!(
            gitignore.iter().any(|l| l == entry),
            ".gitignore lacks {entry:?}: {gitignore:?}"
        );
    }
    let gitattributes = lines_of(&ws.join(".gitattributes"));
    assert!(
        gitattributes.iter().any(|l| l == "projects/** -text"),
        ".gitattributes lacks `projects/** -text`: {gitattributes:?}"
    );

    let (default, listed) = env.registered();
    let default = default.unwrap_or_else(|| panic!("no default_workspace after init"));
    assert!(
        same_folder(&default, &ws),
        "default_workspace is {}, not {}",
        default.display(),
        ws.display()
    );
    assert!(
        listed.iter().any(|p| p.exists() && same_folder(p, &ws)),
        "{} is not in [[workspaces]]: {listed:?}",
        ws.display()
    );

    let status = env.ww(&["status", "--json"]);
    assert_eq!(status.code, 0, "{status:?}");
    let status = stdout_json(&status);
    assert_eq!(
        status["hello"]["workspace"]["path"],
        json!(canon(&ws)),
        "{status}"
    );
}

#[test] // spec: ARCH-090
fn workspace_use_switches_default_and_old_sessions_get_unsupported() {
    let env = TestEnv::new();
    let (ws1, ws2) = two_workspaces(&env);
    let _core = CoreFixture::start(&env);

    let mut a = session(&env, &ws1);
    let before = a.tool(
        "test.store_write",
        json!({"path": "notes/before.txt", "text": "one", "expect": "new"}),
        Some(&key()),
    );
    assert_eq!(
        before["status"], "ok",
        "a write in the default workspace: {before}"
    );

    let out = env.ww(&["workspace", "use", path_arg(&ws2)]);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(
        out.stdout.contains("default workspace: "),
        "`ww workspace use` names the new default: {out:?}"
    );

    let after = a.tool(
        "test.store_write",
        json!({"path": "notes/after.txt", "text": "two", "expect": "new"}),
        Some(&key()),
    );
    assert_eq!(after["status"], "error", "{after}");
    assert_eq!(after["error"]["code"], "workspace.unsupported", "{after}");
    let message = after["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("no message: {after}"))
        .to_lowercase();
    assert!(
        message.contains(&path_arg(&ws2).to_lowercase())
            || message.contains(&canon(&ws2).to_lowercase()),
        "the refusal must name the new default {}: {after}",
        ws2.display()
    );
    assert!(
        !ws1.join(r"notes\after.txt").exists(),
        "the refused write still reached the old workspace"
    );

    let mut b = RawPipe::connect(&env);
    let hello = b.hello(json!({"workspace": env.workspace_id(&ws2)}));
    assert_eq!(hello["result"]["status"], "ok", "{hello}");
}

#[test] // spec: QA-003, ARCH-090
fn unsupported_workspace_error_carries_set_default_hint() {
    let env = TestEnv::new();
    let (_ws1, ws2) = two_workspaces(&env);
    let _core = CoreFixture::start(&env);

    let mut pipe = RawPipe::connect(&env);
    let response = pipe.hello(json!({"workspace": env.workspace_id(&ws2)}));
    let error = &response["result"]["error"];
    assert_eq!(response["result"]["status"], "error", "{response}");
    assert_eq!(error["code"], "workspace.unsupported", "{response}");
    assert_eq!(
        error["fix"]["agent_hint"]["tool"], "workspace.set_default",
        "{response}"
    );
    let hinted = error["fix"]["agent_hint"]["arguments"]["path"]
        .as_str()
        .unwrap_or_else(|| panic!("agent_hint.arguments.path is not a string: {response}"));
    assert!(
        Path::new(hinted).exists() && same_folder(Path::new(hinted), &ws2),
        "agent_hint.arguments.path is {hinted:?}, not {}",
        ws2.display()
    );
    let message = error["message"].as_str().unwrap_or_default().to_lowercase();
    assert!(
        message.contains("ww workspace use \""),
        "the message quotes the path in `ww workspace use \"<path>\"`: {response}"
    );
}

#[test] // spec: CORE-007, ARCH-076
fn suite_toml_edits_keep_comments_and_unknown_keys() {
    let env = TestEnv::new();
    let suite = env.env.suite_toml();
    let parent = suite.parent().expect("suite.toml has a folder");
    std::fs::create_dir_all(parent)
        .unwrap_or_else(|e| panic!("cannot create {}: {e}", parent.display()));
    std::fs::write(&suite, "# my note\n[caps]\nmonthly_usd = 50\n")
        .unwrap_or_else(|e| panic!("cannot write {}: {e}", suite.display()));

    let ws = env.root().join("ws-new");
    let out = env.ww(&["workspace", "init", path_arg(&ws)]);
    assert_eq!(out.code, 0, "{out:?}");

    let text = std::fs::read_to_string(&suite)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", suite.display()));
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines.contains(&"# my note"),
        "the comment was lost:\n{text}"
    );
    assert!(
        lines
            .windows(2)
            .any(|w| w == ["[caps]", "monthly_usd = 50"]),
        "the unknown [caps] table changed:\n{text}"
    );

    let (default, listed) = env.registered();
    assert!(
        listed.iter().any(|p| p.exists() && same_folder(p, &ws)),
        "suite.toml does not list {}:\n{text}",
        ws.display()
    );
    assert!(
        default.is_some_and(|d| d.exists() && same_folder(&d, &ws)),
        "suite.toml has no default_workspace for {}:\n{text}",
        ws.display()
    );
}
