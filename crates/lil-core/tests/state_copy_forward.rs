//! The workspace `state.db` copy-forward (plan 2026-10-17, decision D3, rule R4): a read-write
//! open whose new workspace `state.db` is missing copies the legacy one, crash-safely, and
//! never writes, moves or deletes the legacy files. Read-only opens read the legacy file
//! read-only. And the `suite.toml` writer never writes the legacy file.
//!
//! Every file is under a `TestEnv` root, except the other-volume layout: the harness guard
//! refuses every per-volume local folder by design, so that one test builds a test-channel
//! `Env` over a temporary folder with a fake volume resolver and never arms the guard.
//! Durations are failure deadlines only (QA-031).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lil_core::state::{StateDb, StateRegistry, StateScope};
use lil_core::store::{FailPoints, fsops, path_key};
use lil_core::workspace::{Access, WorkspaceRef};
use lil_platform::names::{LEGACY_APP_NAME, LEGACY_LOCAL_DIR, LOCAL_DIR};
use lil_platform::{Channel, Env, Roots, VolumeResolver};
use lil_testkit::files::write_file;
use lil_testkit::{CoreFixture, RawPipe, TestEnv, failpoints, sha256_file};
use rusqlite::{Connection, OpenFlags};
use serde_json::json;

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);
/// The exit code of the fail point action `abort`.
const ABORT_EXIT: u32 = 0xDEAD;
/// The marker of the copy's temp file (map row 22).
const TEMP_MARKER: &str = ".state.db.lil-tmp-";

fn no_fatal() -> Arc<dyn Fn(&str) + Send + Sync> {
    Arc::new(|what: &str| panic!("unexpected fatal report: {what}"))
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime")
}

/// `ws` as a resolved workspace: its canonical path without `\\?\`, and its id.
fn workspace_ref(ws: &Path) -> WorkspaceRef {
    let canonical = std::fs::canonicalize(ws).expect("canonicalize");
    let text = canonical.to_string_lossy().into_owned();
    WorkspaceRef {
        id: Env::workspace_hash(&canonical),
        path: PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text)),
    }
}

fn beside(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{suffix}", path.display()))
}

/// The names in `dir`, sorted; none when it doesn't exist.
fn names_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// A legacy workspace `state.db` at `<dir>\state.db`, migrated, holding one `generated` row
/// per name in `rows` (path = name, sha256 = `sha-<name>`). Closed cleanly, so it has no
/// `-wal` left.
fn legacy_db(dir: &Path, rows: &[&str]) -> PathBuf {
    let path = dir.join("state.db");
    let db = StateDb::open(&path, StateScope::Workspace, no_fatal()).expect("the legacy db");
    runtime().block_on(async {
        for name in rows {
            db.set_generated(name, &format!("sha-{name}"), 1)
                .await
                .unwrap();
        }
    });
    drop(db);
    assert!(
        !beside(&path, "-wal").exists(),
        "a clean close leaves no WAL"
    );
    path
}

/// A legacy workspace `state.db` and `-wal` as a pre-rename core leaves them while it runs:
/// `committed` rows only in the WAL (nothing checkpointed), no `-shm`.
fn legacy_db_with_wal(env: &TestEnv, dir: &Path, base: &[&str], committed: &[&str]) -> PathBuf {
    let live = env.root().join("live-source").join("state.db");
    let db = StateDb::open(&live, StateScope::Workspace, no_fatal()).expect("the live db");
    runtime().block_on(async {
        for name in base {
            db.set_generated(name, &format!("sha-{name}"), 1)
                .await
                .unwrap();
        }
    });
    drop(db);
    let conn = Connection::open(&live).unwrap();
    conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    for name in committed {
        conn.execute(
            "INSERT INTO generated (key, path, sha256, written_utc) VALUES (?1, ?2, ?3, 2)",
            rusqlite::params![path_key(name), name, format!("sha-{name}")],
        )
        .unwrap();
    }
    let path = dir.join("state.db");
    for suffix in ["", "-wal"] {
        let bytes = std::fs::read(beside(&live, suffix)).unwrap();
        if suffix == "-wal" {
            assert!(!bytes.is_empty(), "the WAL holds the committed rows");
        }
        write_file(&beside(&path, suffix), &bytes);
    }
    drop(conn);
    // The committed rows really are only in the WAL: the main file alone lacks them.
    let alone = env.root().join("main-only").join("state.db");
    write_file(&alone, &std::fs::read(&path).unwrap());
    assert_eq!(
        rows_of(&alone),
        base.iter().map(|s| (*s).to_string()).collect::<Vec<_>>()
    );
    path
}

/// The `generated` paths of the database at `path`, read through a plain read-only
/// connection on a copy (so the file itself is never opened by the test).
fn rows_of(path: &Path) -> Vec<String> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap_or_else(|e| panic!("cannot open {}: {e}", path.display()));
    let mut stmt = conn
        .prepare("SELECT path FROM generated ORDER BY path")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// The SHA-256 of the legacy `state.db` and, when it holds frames, its `-wal`. An empty `-wal`
/// counts as none: a read-only connection on a WAL database creates an empty one (and the
/// `-shm`) when there is none, which changes no row (D3's honest limit). A `-wal` that held
/// frames and is then emptied, changed or removed is still a change.
fn legacy_hashes(path: &Path) -> Vec<(String, Option<String>)> {
    ["", "-wal"]
        .iter()
        .map(|suffix| {
            let file = beside(path, suffix);
            let held = file.exists() && (suffix.is_empty() || file.metadata().unwrap().len() > 0);
            (suffix.to_string(), held.then(|| sha256_file(&file)))
        })
        .collect()
}

/// After a copy: the legacy folder holds only the database, its WAL and at most the `-shm` a
/// read-only connection may create (D3's honest limit), with the database and WAL unchanged.
fn assert_legacy_untouched(path: &Path, before: &[(String, Option<String>)]) {
    assert_eq!(
        legacy_hashes(path),
        before,
        "the legacy state.db or -wal changed"
    );
    for name in names_in(path.parent().unwrap()) {
        assert!(
            ["state.db", "state.db-wal", "state.db-shm"].contains(&name.as_str()),
            "the legacy folder gained {name}"
        );
    }
}

/// The registry's read-write open of `ws` copies the legacy database at `legacy` to `new`
/// once; the legacy files never change; the copy has every row, no `-shm` from the legacy
/// folder, and passes the open's own checks (it is migrated and served).
fn copy_once_and_check(env: &Env, ws: &WorkspaceRef, legacy: &Path, new: &Path, rows: &[&str]) {
    let before = legacy_hashes(legacy);
    assert!(!new.exists());
    let reg = StateRegistry::open(env, no_fatal()).expect("the registry");
    let rt = runtime();
    let db = rt
        .block_on(reg.for_workspace(ws, Access::ReadWrite))
        .expect("the copied database opens");
    assert_eq!(db.path(), new);
    for name in rows {
        assert_eq!(
            rt.block_on(db.generated(name)).unwrap(),
            Some(format!("sha-{name}")),
            "{name}"
        );
    }
    // A write lands in the copy, never in the legacy file.
    rt.block_on(db.set_generated("after-copy", "sha-after", 3))
        .unwrap();
    drop(db);
    reg.close();
    assert_legacy_untouched(legacy, &before);
    assert_eq!(
        names_in(new.parent().unwrap()),
        ["state.db"],
        "the copy left a temp file, or a -wal or -shm stayed after the close"
    );
    let mut want: Vec<String> = rows.iter().map(|s| (*s).to_string()).collect();
    want.push("after-copy".into());
    want.sort();
    assert_eq!(rows_of(new), want);

    // Once: the next read-write open uses the copy, and the legacy file stays as it is.
    let copy_before = sha256_file(new);
    let reg = StateRegistry::open(env, no_fatal()).expect("the registry again");
    let db = rt
        .block_on(reg.for_workspace(ws, Access::ReadWrite))
        .expect("the copy opens again");
    assert_eq!(
        rt.block_on(db.generated("after-copy")).unwrap().as_deref(),
        Some("sha-after")
    );
    drop(db);
    reg.close();
    assert_eq!(sha256_file(new), copy_before, "the copy was made again");
    assert_legacy_untouched(legacy, &before);
}

#[test] // spec: CORE-005, ARCH-097
fn local_db_is_copied_forward_once_and_the_legacy_is_untouched() {
    // A quote in the path: the copy's target name is bound, never pasted into the SQL text.
    let env = TestEnv::with_label("Ünïcödé space o'clock");
    assert!(env.root().to_string_lossy().contains('\''));
    let ws_dir = env.make_workspace("same volume");
    let ws = workspace_ref(&ws_dir);
    let legacy_dir = env.legacy_workspace_local_dir(&ws_dir);
    let new_dir = env.workspace_local_dir(&ws_dir);
    assert!(
        legacy_dir.starts_with(
            env.root()
                .join("local")
                .join(format!("{LEGACY_APP_NAME}-{}", env.env.channel()))
        ),
        "{}",
        legacy_dir.display()
    );
    let legacy = legacy_db(&legacy_dir, &["a.png", "b/c.txt"]);
    copy_once_and_check(
        &env.env,
        &ws,
        &legacy,
        &new_dir.join("state.db"),
        &["a.png", "b/c.txt"],
    );
}

/// Every path under `<tmp>\vol-d` is on one volume, everything else on `<tmp>\vol-c`.
#[derive(Debug)]
struct TwoVolumes {
    c: PathBuf,
    d: PathBuf,
}

impl VolumeResolver for TwoVolumes {
    fn volume_root(&self, path: &Path) -> std::io::Result<PathBuf> {
        let p = path.to_string_lossy().to_ascii_lowercase();
        let d = self.d.to_string_lossy().to_ascii_lowercase();
        Ok(if p.starts_with(&d) {
            self.d.clone()
        } else {
            self.c.clone()
        })
    }
}

#[test] // spec: CORE-005, ARCH-097
fn local_db_is_copied_forward_once_on_another_volume() {
    // See the file comment: the guard refuses every per-volume local folder, so this layout
    // runs over a temporary folder with a fake volume resolver and a test channel.
    let tmp = tempfile::Builder::new()
        .prefix("lil-copy-forward-")
        .tempdir()
        .unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let base = PathBuf::from(
        base.to_string_lossy()
            .strip_prefix(r"\\?\")
            .unwrap_or(&base.to_string_lossy())
            .to_string(),
    );
    let (c, d) = (base.join("vol-c"), base.join("vol-d"));
    let roots = Roots {
        roaming: c.join("roaming"),
        local: c.join("local"),
        user_profile: c.join("profile"),
    };
    for dir in [&roots.roaming, &roots.local, &roots.user_profile, &d] {
        fsops::ensure_dir(dir).unwrap();
    }
    let env = Env::with_roots(Channel::Test("cf0vol".into()), roots, "S-1".into())
        .with_volume_resolver(Arc::new(TwoVolumes { c, d: d.clone() }));
    let ws_dir = d.join("ws");
    fsops::ensure_dir(&ws_dir).unwrap();
    let ws = workspace_ref(&ws_dir);
    let legacy_dir = env
        .legacy_workspace_local_dir_for_canonical(&ws.path)
        .unwrap();
    let new_dir = env.workspace_local_dir_for_canonical(&ws.path).unwrap();
    assert_eq!(legacy_dir, d.join(LEGACY_LOCAL_DIR).join(&ws.id));
    assert_eq!(new_dir, d.join(LOCAL_DIR).join(&ws.id));
    let legacy = legacy_db(&legacy_dir, &["x.png"]);
    copy_once_and_check(&env, &ws, &legacy, &new_dir.join("state.db"), &["x.png"]);
}

#[test] // spec: CORE-004, CORE-005
fn copy_forward_keeps_committed_wal_rows() {
    let env = TestEnv::new();
    let ws_dir = env.make_workspace("wal");
    let ws = workspace_ref(&ws_dir);
    let legacy_dir = env.legacy_workspace_local_dir(&ws_dir);
    let legacy = legacy_db_with_wal(&env, &legacy_dir, &["base.png"], &["only-in-wal.png"]);
    copy_once_and_check(
        &env.env,
        &ws,
        &legacy,
        &env.workspace_local_dir(&ws_dir).join("state.db"),
        &["base.png", "only-in-wal.png"],
    );
}

/// Review Focus 3: a core killed after the copy's temp file is written, before its fsync and
/// rename, leaves no new `state.db` and the legacy files as they were; the next core removes
/// the temp file and copies again.
#[test] // spec: CORE-005, QA-055
fn copy_forward_survives_a_kill_mid_copy() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws_dir = env.copy_fixture_workspace("kill mid copy");
    env.register_workspace(&ws_dir, true);
    let legacy_dir = env.legacy_workspace_local_dir(&ws_dir);
    let new_dir = env.workspace_local_dir(&ws_dir);
    let legacy = legacy_db_with_wal(&env, &legacy_dir, &["base.png"], &["only-in-wal.png"]);
    let before = legacy_hashes(&legacy);

    // The default workspace opens at start: the copy runs and the core dies at the point.
    let spec = failpoints::spec(&[("state.copy_forward", "abort")]);
    let core = CoreFixture::spawn_raw_with_env(&env, &[(failpoints::ENV, &spec)]);
    assert_eq!(
        core.wait_exit(DEADLINE),
        Some(ABORT_EXIT),
        "the core didn't end at state.copy_forward"
    );
    drop(core);
    let left = names_in(&new_dir);
    assert!(!new_dir.join("state.db").exists(), "{left:?}");
    assert_eq!(left.len(), 1, "one temp file is left: {left:?}");
    assert!(left[0].starts_with(TEMP_MARKER), "{left:?}");
    assert_legacy_untouched(&legacy, &before);

    // The next core removes the temp file and copies again.
    {
        let _core = CoreFixture::start(&env);
        let mut pipe = RawPipe::connect(&env);
        let hello = pipe.hello(json!({"workspace": env.workspace_id(&ws_dir)}));
        assert_eq!(hello["result"]["status"], "ok", "{hello}");
        pipe.tool_ok("project.status", json!({"project": "sketchbook"}), None);
    }
    let left = names_in(&new_dir);
    assert!(
        !left.iter().any(|n| n.starts_with(TEMP_MARKER)),
        "the temp file stayed: {left:?}"
    );
    assert!(new_dir.join("state.db").is_file(), "{left:?}");
    let rows = rows_of(&new_dir.join("state.db"));
    for want in ["base.png", "only-in-wal.png"] {
        assert!(
            rows.iter().any(|r| r == want),
            "{want} not copied: {rows:?}"
        );
    }
    assert_legacy_untouched(&legacy, &before);
}

#[test] // spec: CORE-005, ARCH-028
fn copy_forward_runs_only_read_write() {
    let env = TestEnv::new();
    let ws_dir = env.make_workspace("read only");
    let ws = workspace_ref(&ws_dir);
    let legacy = legacy_db(&env.legacy_workspace_local_dir(&ws_dir), &["a.png"]);
    let before = legacy_hashes(&legacy);
    let new_dir = env.workspace_local_dir(&ws_dir);
    let reg = StateRegistry::open(&env.env, no_fatal()).unwrap();
    let rt = runtime();
    let db = rt
        .block_on(reg.for_workspace(&ws, Access::ReadOnly))
        .expect("a read-only open");
    assert_eq!(
        db.path(),
        legacy,
        "the read-only open reads the legacy file"
    );
    drop(db);
    reg.close();
    assert!(
        !new_dir.exists(),
        "a read-only open created {}",
        new_dir.display()
    );
    assert_legacy_untouched(&legacy, &before);
}

#[test] // spec: CORE-005
fn existing_new_local_db_wins_over_the_legacy() {
    let env = TestEnv::new();
    let ws_dir = env.make_workspace("both");
    let ws = workspace_ref(&ws_dir);
    let legacy = legacy_db(&env.legacy_workspace_local_dir(&ws_dir), &["legacy.png"]);
    let new = legacy_db(&env.workspace_local_dir(&ws_dir), &["new.png"]);
    let before = legacy_hashes(&legacy);
    let reg = StateRegistry::open(&env.env, no_fatal()).unwrap();
    let rt = runtime();
    for access in [Access::ReadWrite, Access::ReadOnly] {
        let db = rt.block_on(reg.for_workspace(&ws, access)).unwrap();
        assert_eq!(db.path(), new, "{access:?}");
        assert_eq!(
            rt.block_on(db.generated("new.png")).unwrap().as_deref(),
            Some("sha-new.png"),
            "{access:?}"
        );
        assert_eq!(
            rt.block_on(db.generated("legacy.png")).unwrap(),
            None,
            "{access:?}"
        );
    }
    reg.close();
    assert_legacy_untouched(&legacy, &before);
    assert_eq!(rows_of(&new), ["new.png"]);
}

#[test] // spec: CORE-005, ARCH-028
fn read_only_open_reads_the_legacy_db_without_writing_its_db_or_wal() {
    let env = TestEnv::new();
    let ws_dir = env.make_workspace("legacy read");
    let ws = workspace_ref(&ws_dir);
    let legacy = legacy_db_with_wal(
        &env,
        &env.legacy_workspace_local_dir(&ws_dir),
        &["base.png"],
        &["only-in-wal.png"],
    );
    let before = legacy_hashes(&legacy);
    assert!(before[1].1.is_some(), "the legacy WAL exists");
    let reg = StateRegistry::open(&env.env, no_fatal()).unwrap();
    let rt = runtime();
    let db = rt
        .block_on(reg.for_workspace(&ws, Access::ReadOnly))
        .unwrap();
    assert_eq!(db.path(), legacy);
    assert!(rt.block_on(db.diagnostics()).unwrap().read_only);
    for name in ["base.png", "only-in-wal.png"] {
        assert_eq!(
            rt.block_on(db.generated(name)).unwrap(),
            Some(format!("sha-{name}")),
            "{name}"
        );
    }
    // A write is refused by the read-only connection.
    assert!(rt.block_on(db.set_generated("x", "y", 1)).is_err());
    drop(db);
    reg.close();
    assert_legacy_untouched(&legacy, &before);
    assert!(!env.workspace_local_dir(&ws_dir).exists());
}

/// The core's `suite.toml` writer (CORE-007) with only the legacy file present: the new file
/// gets the legacy entries plus the new one, and the legacy bytes never change (R4).
#[test] // spec: CORE-007, ARCH-090
fn suite_toml_registration_never_writes_the_legacy_file() {
    let env = TestEnv::with_label("Ünïcödé space");
    let old_ws = env.make_workspace("old");
    let new_ws = env.make_workspace("new");
    let legacy = env.env.legacy_data_dir().join("suite.toml");
    let text = format!(
        "# kept\ndefault_workspace = '{}'\n\n[[workspaces]]\npath = '{}'\n",
        old_ws.display(),
        old_ws.display()
    );
    write_file(&legacy, text.as_bytes());
    let before = sha256_file(&legacy);
    assert_eq!(env.env.suite_toml_to_read(), legacy);

    let written = runtime()
        .block_on(lil_core::store::register_workspace(
            &env.env,
            &new_ws,
            false,
            &FailPoints::none(),
        ))
        .expect("the registration");
    assert!(written);
    assert_eq!(
        sha256_file(&legacy),
        before,
        "the legacy suite.toml changed"
    );
    assert_eq!(
        names_in(legacy.parent().unwrap()),
        ["suite.toml"],
        "the writer left something in the legacy folder"
    );
    assert_eq!(env.env.suite_toml_to_read(), env.env.suite_toml());
    let (default, listed) = env.registered();
    assert_eq!(default.as_deref(), Some(old_ws.as_path()));
    assert_eq!(listed, [old_ws.clone(), new_ws.clone()]);
    let new_text = std::fs::read_to_string(env.env.suite_toml()).unwrap();
    assert!(new_text.starts_with("# kept"), "{new_text}");

    // Registering again changes nothing anywhere.
    let again = runtime()
        .block_on(lil_core::store::register_workspace(
            &env.env,
            &new_ws,
            false,
            &FailPoints::none(),
        ))
        .unwrap();
    assert!(!again);
    assert_eq!(sha256_file(&legacy), before);
}
