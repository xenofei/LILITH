//! `ww doctor`'s client side (CORE-004; owner decision 5): the suite `state.db` inspected
//! without leaving a trace, set aside without deleting anything, a hot journal rolled back only
//! on request, the start-failure messages, and the states of `suite.toml`.
//!
//! Every database and `suite.toml` here is the test channel's own, under its `TestEnv` root
//! (QA-038, QA-040).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{Connection, OpenFlags};
use ww_client::doctor::{
    StateFileState, SuiteTomlState, finish_recovery, inspect_suite_state, inspect_suite_toml,
    set_aside, set_aside_suffix,
};
use ww_client::{ClientError, StartFailure};
use ww_platform::fs::Share;
use ww_testkit::{HOT_JOURNAL_ROWS, HandleHolder, TestEnv, sha256_file};

/// The schema this build knows (ww-core's suite migrations).
const KNOWN: i64 = ww_schema::SUITE_STATE_SCHEMA;

/// `path` with `suffix` added to its file name.
fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// The names of the files in `dir`, sorted.
fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()))
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn assert_no_side_files(db: &Path, what: &str) {
    for suffix in ["-wal", "-shm", "-journal"] {
        let side = beside(db, suffix);
        assert!(!side.exists(), "{what}: {} appeared", side.display());
    }
}

#[test] // spec: CORE-004, QA-062
fn inspect_never_creates_side_files() {
    let env = TestEnv::with_label("Ünïcödé space");
    let db = env.env.suite_state_db();
    assert_eq!(inspect_suite_state(&env.env), StateFileState::Missing);
    assert!(!db.exists(), "inspecting a missing database created it");

    // A healthy WAL database (its `-wal` and `-shm` go away when the last connection closes).
    env.write_state_db_version(&db, KNOWN, true);
    assert_no_side_files(&db, "the fixture");
    let before = sha256_file(&db);
    assert_eq!(inspect_suite_state(&env.env), StateFileState::Healthy);
    assert_no_side_files(&db, "a healthy WAL database");
    assert_eq!(sha256_file(&db), before);

    // A newer one, which a plain read-only open would give fresh `-wal` and `-shm` files.
    env.write_state_db_version(&db, 999, true);
    let before = sha256_file(&db);
    assert_eq!(
        inspect_suite_state(&env.env),
        StateFileState::TooNew {
            found: 999,
            known: KNOWN
        }
    );
    assert_no_side_files(&db, "a newer WAL database");
    assert_eq!(sha256_file(&db), before);

    // A damaged one.
    std::fs::write(&db, vec![0xAB_u8; 4096]).unwrap();
    let before = sha256_file(&db);
    assert!(
        matches!(inspect_suite_state(&env.env), StateFileState::Damaged(_)),
        "{:?}",
        inspect_suite_state(&env.env)
    );
    assert_no_side_files(&db, "a damaged database");
    assert_eq!(sha256_file(&db), before);
}

#[test] // spec: CORE-004
fn set_aside_moves_db_wal_and_shm_and_deletes_nothing() {
    let env = TestEnv::with_label("Ünïcödé space");
    let db = env.env.suite_state_db();
    let dir = db.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&dir).unwrap();
    let files = [
        (db.clone(), vec![0xAB_u8; 4096]),
        (beside(&db, "-wal"), b"wal bytes".to_vec()),
        (beside(&db, "-shm"), b"shm bytes".to_vec()),
    ];
    for (path, bytes) in &files {
        std::fs::write(path, bytes).unwrap();
    }
    let other = dir.join("other.txt");
    std::fs::write(&other, "Ünïcödé space").unwrap();
    let now = 1_792_200_000; // 2026-10-17T…Z
    let suffix = set_aside_suffix(now);
    assert!(suffix.starts_with(".damaged-2026"), "{suffix}");

    // Held open by another program: nothing moves, and the error says why.
    let held = HandleHolder::hold(&db, Share::Read);
    let err = set_aside(&env.env, now).expect_err("a held database can't be moved");
    let text = err.to_string();
    assert!(text.contains("holds it open"), "{text}");
    assert!(text.contains("Nothing was moved"), "{text}");
    for (path, bytes) in &files {
        assert_eq!(&std::fs::read(path).unwrap(), bytes, "{}", path.display());
    }
    held.release();

    let moved = set_aside(&env.env, now).expect("set aside");
    let expected: Vec<PathBuf> = files
        .iter()
        .map(|(path, _)| beside(path, &suffix))
        .collect();
    assert_eq!(moved, expected);
    for ((path, bytes), to) in files.iter().zip(&expected) {
        assert!(!path.exists(), "{} is still there", path.display());
        assert_eq!(&std::fs::read(to).unwrap(), bytes, "{}", to.display());
    }
    assert_eq!(std::fs::read_to_string(&other).unwrap(), "Ünïcödé space");
    let mut want: Vec<String> = expected
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    want.push("other.txt".to_string());
    want.sort();
    assert_eq!(names_in(&dir), want, "nothing else changed in the folder");

    // A second set-aside in the same second never replaces the first one's files.
    std::fs::write(&db, b"again").unwrap();
    let err = set_aside(&env.env, now).expect_err("the set-aside name is taken");
    assert!(err.to_string().contains("Nothing was moved"), "{err}");
    assert_eq!(std::fs::read(&db).unwrap(), b"again");
    assert_eq!(std::fs::read(&expected[0]).unwrap(), vec![0xAB_u8; 4096]);
}

#[test] // spec: CORE-004
fn finish_recovery_rolls_back_a_hot_journal() {
    let env = TestEnv::with_label("Ünïcödé space");
    let db = env.env.suite_state_db();
    env.write_hot_journal_db(&db, KNOWN);
    let journal = beside(&db, "-journal");
    let (db_before, journal_before) = (sha256_file(&db), sha256_file(&journal));

    // The file itself holds part of the interrupted write (read past the journal).
    let raw = Connection::open_with_flags(
        format!(
            "file:///{}?immutable=1",
            db.to_str().unwrap().replace('\\', "/").replace(' ', "%20")
        ),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap();
    let after: i64 = raw
        .query_row("SELECT count(*) FROM t WHERE x LIKE 'after%'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(
        after > 0,
        "the fixture's file holds none of the interrupted write"
    );
    drop(raw);
    assert_eq!(sha256_file(&db), db_before);

    // Inspecting changes nothing: the journal is never rolled back by a look.
    assert_eq!(inspect_suite_state(&env.env), StateFileState::HotJournal);
    assert_eq!(sha256_file(&db), db_before);
    assert_eq!(sha256_file(&journal), journal_before);

    assert_eq!(
        finish_recovery(&env.env).expect("the recovery runs"),
        StateFileState::Healthy
    );
    assert_eq!(inspect_suite_state(&env.env), StateFileState::Healthy);
    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let before: i64 = conn
        .query_row("SELECT count(*) FROM t WHERE x LIKE 'before%'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        before, HOT_JOURNAL_ROWS,
        "the interrupted write was rolled back"
    );
    drop(conn);
}

#[test] // spec: CORE-004, QA-005
fn start_failure_messages_name_doctor_without_plan_2b() {
    let failed = |code: u32| ClientError::StartFailed {
        exit_code: Some(code),
        kind: StartFailure::from_exit_code(Some(code)),
        state_db: PathBuf::from(r"C:\t\Ünïcödé space\state.db"),
        logs_dir: PathBuf::from(r"C:\t\logs"),
    };
    assert_eq!(
        failed(5).to_string(),
        r"The core can't start: its state database at C:\t\Ünïcödé space\state.db is damaged. Nothing was changed. `ww doctor` can set it aside."
    );
    assert_eq!(
        failed(7).to_string(),
        r"The core can't start: its state database at C:\t\Ünïcödé space\state.db was left mid-write by a crash. Nothing was changed. `ww doctor` can finish the recovery."
    );
    for code in [5, 6, 7] {
        let text = failed(code).to_string();
        assert!(!text.contains("plan 2b"), "{text}");
    }
}

#[test] // spec: ARCH-090, CORE-007
fn suite_toml_states_cover_missing_unreadable_and_stale_default() {
    let env = Arc::new(TestEnv::with_label("Ünïcödé space"));
    assert_eq!(inspect_suite_toml(&env.env), SuiteTomlState::Missing);

    let file = env.env.suite_toml();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "default_workspace = [not toml").unwrap();
    assert!(
        matches!(inspect_suite_toml(&env.env), SuiteTomlState::Unreadable(why) if why.contains("suite.toml")),
        "{:?}",
        inspect_suite_toml(&env.env)
    );

    std::fs::write(&file, "").unwrap();
    let ws = env.make_workspace("Ünïcödé registered");
    env.register_workspace(&ws, false);
    assert_eq!(inspect_suite_toml(&env.env), SuiteTomlState::NoDefault);

    env.register_workspace(&ws, true);
    assert_eq!(
        inspect_suite_toml(&env.env),
        SuiteTomlState::Ok {
            default: ws.clone(),
            id: env.workspace_id(&ws),
        }
    );

    let gone = env.root().join("ws-gone");
    env.register_workspace(&gone, true);
    assert_eq!(
        inspect_suite_toml(&env.env),
        SuiteTomlState::DefaultMissing(gone)
    );
}

#[test] // spec: CORE-004 (ruling Q26, M1)
fn leftover_side_files_are_reported_and_set_aside_not_deleted() {
    let env = TestEnv::with_label("Ünïcödé space");
    let db = env.env.suite_state_db();
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let (wal, shm) = (beside(&db, "-wal"), beside(&db, "-shm"));
    std::fs::write(&wal, b"old wal").unwrap();
    std::fs::write(&shm, b"old shm").unwrap();

    assert_eq!(
        inspect_suite_state(&env.env),
        StateFileState::Leftovers(vec![wal.clone(), shm.clone()])
    );
    let now = 1_792_200_000;
    let suffix = set_aside_suffix(now);
    let moved = set_aside(&env.env, now).expect("set aside");
    assert_eq!(moved, [beside(&wal, &suffix), beside(&shm, &suffix)]);
    assert_eq!(std::fs::read(&moved[0]).unwrap(), b"old wal");
    assert_eq!(std::fs::read(&moved[1]).unwrap(), b"old shm");
    assert!(!db.exists(), "set_aside created a database");
    assert_eq!(inspect_suite_state(&env.env), StateFileState::Missing);
    // With nothing left, there is nothing to set aside.
    let err = set_aside(&env.env, now + 1).expect_err("nothing to move");
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "{err}");
}
