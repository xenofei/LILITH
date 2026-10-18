//! Store writes: the hash check that never overwrites an outside edit, failed writes that
//! leave nothing half-written, the relative-path budget and the never-written ledger (spec
//! ARCH-073, ARCH-074, ARCH-082, CORE-040, QA-011; Appendix E "Store write algorithm" and
//! "Workspace-relative paths"). Driven through `test.store_write` on a real core and the
//! workspace files themselves (QA-018).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use lil_testkit::{CoreFixture, RawPipe, TestEnv, failpoints, sha256_bytes, sha256_file};
use serde_json::{Value, json};

/// A fresh idempotency key for every write call (ARCH-035).
fn key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "spec-sw-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// A copy of the fixture workspace, registered as this channel's default.
fn default_workspace(env: &TestEnv) -> PathBuf {
    let ws = env.copy_fixture_workspace("store");
    env.register_workspace(&ws, true);
    ws
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

/// `test.store_write {path, text, expect}` with a fresh key; returns the `Outcome`.
fn store_write(pipe: &mut RawPipe, path: &str, text: &str, expect: &str) -> Value {
    pipe.tool(
        "test.store_write",
        json!({"path": path, "text": text, "expect": expect}),
        Some(&key()),
    )
}

/// The error code of an `Outcome`, after checking that it is an error.
fn error_code(outcome: &Value) -> String {
    assert_eq!(outcome["status"], "error", "expected an error: {outcome}");
    outcome["error"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("an error without a code: {outcome}"))
        .to_string()
}

/// The file at the workspace-relative `rel` (with `/` separators).
fn file(ws: &Path, rel: &str) -> PathBuf {
    ws.join(rel.replace('/', "\\"))
}

/// Writes `bytes` to `path` as an outside writer would, creating its folders.
fn write_outside(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", parent.display()));
    }
    std::fs::write(path, bytes).unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
}

/// Every file and folder name under `dir`, recursively, dot-files included.
fn all_names(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()));
    for entry in entries {
        let path = entry
            .unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()))
            .path();
        if path.is_dir() {
            all_names(&path, out);
        }
        out.push(path);
    }
}

#[test] // spec: ARCH-074, ARCH-073
fn outside_edit_is_never_overwritten() {
    let env = TestEnv::new();
    let ws = default_workspace(&env);
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);

    let first = store_write(&mut pipe, "notes/a.txt", "core", "new");
    assert_eq!(first["status"], "ok", "{first}");
    assert_eq!(
        first["structured"]["sha256"],
        json!(sha256_bytes(b"core")),
        "{first}"
    );
    let target = file(&ws, "notes/a.txt");
    assert_eq!(std::fs::read(&target).unwrap(), b"core");

    write_outside(&target, b"mine");
    let second = store_write(&mut pipe, "notes/a.txt", "core 2", "generated");
    assert_eq!(error_code(&second), "store.hash_conflict", "{second}");
    assert_eq!(
        std::fs::read(&target).unwrap(),
        b"mine",
        "the outside edit was overwritten"
    );
}

#[test] // spec: QA-011, ARCH-074
fn failed_write_leaves_no_half_written_file() {
    let env = TestEnv::new();
    let ws = default_workspace(&env);
    let target = file(&ws, "notes/b.txt");
    write_outside(&target, b"before");
    let before = sha256_file(&target);

    let points = failpoints::spec(&[("store.write_temp", "disk_full")]);
    let _core = CoreFixture::start_with_env(&env, &[(failpoints::ENV, &points)]);
    let mut pipe = session(&env, &ws);

    let outcome = store_write(&mut pipe, "notes/b.txt", "after", &before);
    assert_eq!(error_code(&outcome), "store.disk_full", "{outcome}");
    assert_eq!(sha256_file(&target), before, "the target changed");

    let mut names = Vec::new();
    all_names(&ws, &mut names);
    let temps: Vec<_> = names
        .iter()
        .filter(|p| p.to_string_lossy().contains(".lil-tmp-"))
        .collect();
    assert!(temps.is_empty(), "temp files left behind: {temps:?}");
}

#[test] // spec: CORE-040
fn relative_path_over_budget_is_refused() {
    let env = TestEnv::new();
    let ws = default_workspace(&env);
    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);

    let over = format!("over/deep/{}.txt", "x".repeat(181 - "over/deep/.txt".len()));
    assert_eq!(over.chars().count(), 181);
    let refused = store_write(&mut pipe, &over, "too long", "new");
    assert_eq!(error_code(&refused), "store.path_invalid", "{refused}");
    assert!(
        !ws.join("over").exists(),
        "a refused path must leave nothing behind, but {} exists",
        ws.join("over").display()
    );

    let reserved = store_write(&mut pipe, "con.txt", "reserved", "new");
    assert_eq!(error_code(&reserved), "store.path_invalid", "{reserved}");

    let at_budget = format!(
        "under/deep/{}.txt",
        "y".repeat(180 - "under/deep/.txt".len())
    );
    assert_eq!(at_budget.chars().count(), 180);
    let written = store_write(&mut pipe, &at_budget, "fits", "new");
    assert_eq!(written["status"], "ok", "{written}");
    assert_eq!(std::fs::read(file(&ws, &at_budget)).unwrap(), b"fits");
}

#[test] // spec: ARCH-082
fn registry_ledger_csv_is_never_written() {
    let env = TestEnv::new();
    let ws = default_workspace(&env);
    let ledger = file(&ws, "registry/ledger.csv");
    write_outside(&ledger, b"date,amount\n2026-10-01,0\n");
    let before = std::fs::read(&ledger).unwrap();

    let _core = CoreFixture::start(&env);
    let mut pipe = session(&env, &ws);
    let outcome = store_write(
        &mut pipe,
        "registry/ledger.csv",
        "date,amount\n",
        &sha256_bytes(&before),
    );
    assert_eq!(error_code(&outcome), "store.path_invalid", "{outcome}");
    assert_eq!(
        std::fs::read(&ledger).unwrap(),
        before,
        "the ledger changed"
    );
}
