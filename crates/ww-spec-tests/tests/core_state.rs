//! The suite `state.db` at core start: a newer or a damaged database stops the core with its
//! own exit code, and nothing on disk changes (spec Appendix D, "Exit codes of `ww-core`";
//! owner decision 5). The database file is written through the test kit (QA-018).

use std::path::PathBuf;
use std::time::Duration;

use ww_testkit::{CoreFixture, TestEnv, sha256_file};

/// A failure deadline only, never a measurement (QA-031).
const DEADLINE: Duration = Duration::from_secs(30);

/// The suite `state.db`: `local_dir\state.db` (CORE-005).
fn suite_state_db(env: &TestEnv) -> PathBuf {
    env.env.local_dir().join("state.db")
}

#[test] // spec: QA-062, CORE-004
fn newer_state_db_is_refused_and_kept() {
    let env = TestEnv::new();
    let db = suite_state_db(&env);
    env.write_state_db_version(&db, 999, true);
    let before = sha256_file(&db);

    let core = CoreFixture::spawn_raw(&env);
    assert_eq!(
        core.wait_exit(DEADLINE),
        Some(6),
        "a core facing a newer suite state.db must exit with code 6"
    );
    assert_eq!(sha256_file(&db), before, "{} was changed", db.display());
}

#[test] // spec: CORE-004
fn corrupt_state_db_is_refused_and_kept() {
    let env = TestEnv::new();
    let db = suite_state_db(&env);
    let parent = db.parent().expect("state.db has a folder");
    std::fs::create_dir_all(parent)
        .unwrap_or_else(|e| panic!("cannot create {}: {e}", parent.display()));
    std::fs::write(&db, vec![0xAB_u8; 4096])
        .unwrap_or_else(|e| panic!("cannot write {}: {e}", db.display()));
    let before = sha256_file(&db);

    let core = CoreFixture::spawn_raw(&env);
    assert_eq!(
        core.wait_exit(DEADLINE),
        Some(5),
        "a core facing a damaged suite state.db must exit with code 5"
    );
    assert_eq!(sha256_file(&db), before, "{} was changed", db.display());
}
