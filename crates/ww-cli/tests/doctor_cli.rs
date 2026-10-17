//! `ww doctor` against the real `ww` and `ww-core` (CORE-004, CORE-040, ARCH-085, QA-060;
//! owner decision 5): a damaged suite `state.db` set aside only after `y`, never with
//! `--non-interactive`; `core.autocrlf` set for the repository only after `y`; a stale default
//! reported without stopping the doctor; no default workspace, no core.
//!
//! The prompts are answered through stdin. Every test runs on its own `TestEnv` channel with
//! isolated git (QA-038); any core `ww` starts is drained when the env drops (QA-041).

use std::path::{Path, PathBuf};

use ww_testkit::{TestEnv, WwOutput, sha256_file};

/// The non-empty stdout lines.
fn printed(out: &WwOutput) -> Vec<&str> {
    out.stdout
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty())
        .collect()
}

/// A damaged suite `state.db`; its path and SHA-256.
fn damaged_state_db(env: &TestEnv) -> (PathBuf, String) {
    let db = env.env.suite_state_db();
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    std::fs::write(&db, vec![0xAB_u8; 4096]).unwrap();
    let sha = sha256_file(&db);
    (db, sha)
}

/// The `state.db.damaged-*` files beside `db`.
fn set_aside_files(db: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(db.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("state.db.damaged-"))
        })
        .collect();
    found.sort();
    found
}

/// A git repository workspace registered as the default, whose own config sets `autocrlf`.
fn autocrlf_workspace(env: &TestEnv) -> PathBuf {
    let ws = env.make_workspace("Ünïcödé autocrlf");
    env.register_workspace(&ws, true);
    env.git_init(&ws);
    env.git(&ws, &["config", "--local", "core.autocrlf", "true"]);
    ws
}

#[test] // spec: CORE-004
fn doctor_answers_n_keeps_the_file() {
    let env = TestEnv::with_label("Ünïcödé space");
    let (db, before) = damaged_state_db(&env);

    let out = env.ww_with_stdin(&["doctor"], "n\n");
    assert_eq!(out.code, 1, "{out:?}");
    let lines = printed(&out);
    assert!(
        lines.contains(&"problem: workspace checks skipped — the core can't start"),
        "{out:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("problem: the suite state database") && l.contains("damaged")),
        "{out:?}"
    );
    assert!(
        out.stderr.contains("[y/N]"),
        "the question is asked: {out:?}"
    );
    assert_eq!(sha256_file(&db), before, "answering n changed the database");
    assert!(set_aside_files(&db).is_empty(), "{out:?}");
}

#[test] // spec: CORE-004
fn doctor_answers_y_sets_it_aside_and_the_core_starts() {
    let env = TestEnv::with_label("Ünïcödé space");
    let (db, before) = damaged_state_db(&env);

    let out = env.ww_with_stdin(&["doctor"], "y\n");
    assert_eq!(out.code, 0, "{out:?}");
    let lines = printed(&out);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("fixed:") && l.contains("set aside as state.db.damaged-")),
        "{out:?}"
    );
    assert!(
        lines.contains(&"ok: no default workspace; workspace checks skipped"),
        "{out:?}"
    );
    let aside = set_aside_files(&db);
    assert_eq!(aside.len(), 1, "{aside:?}");
    assert_eq!(sha256_file(&aside[0]), before, "the damaged bytes are kept");

    let status = env.ww(&["status"]);
    assert_eq!(
        status.code, 0,
        "the core starts with a fresh database: {status:?}"
    );
}

#[test] // spec: CORE-004
fn non_interactive_doctor_never_moves_it() {
    let env = TestEnv::with_label("Ünïcödé space");
    let (db, before) = damaged_state_db(&env);

    // A `y` waiting on stdin is never read.
    let out = env.ww_with_stdin(&["--non-interactive", "doctor"], "y\n");
    assert_eq!(out.code, 1, "{out:?}");
    assert!(!out.stderr.contains("[y/N]"), "nothing is asked: {out:?}");
    assert!(
        out.stdout.contains("run ww doctor interactively"),
        "{out:?}"
    );
    assert_eq!(sha256_file(&db), before);
    assert!(set_aside_files(&db).is_empty(), "{out:?}");
}

#[test] // spec: CORE-040, QA-060
fn doctor_sets_autocrlf_only_after_yes() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = autocrlf_workspace(&env);

    let no = env.ww_with_stdin(&["doctor"], "n\n");
    assert_eq!(no.code, 1, "{no:?}");
    assert!(
        printed(&no)
            .iter()
            .any(|l| l.starts_with("problem: core.autocrlf is true")),
        "{no:?}"
    );
    assert!(
        no.stderr
            .contains("Set core.autocrlf=false for this repository only? [y/N]"),
        "{no:?}"
    );
    assert_eq!(
        env.git(&ws, &["config", "--local", "core.autocrlf"]).trim(),
        "true",
        "answering n changed core.autocrlf"
    );

    let yes = env.ww_with_stdin(&["doctor"], "y\n");
    assert_eq!(yes.code, 0, "{yes:?}");
    assert!(
        printed(&yes).contains(&"fixed: core.autocrlf set to false for this repository only"),
        "{yes:?}"
    );
    assert_eq!(
        env.git(&ws, &["config", "--local", "core.autocrlf"]).trim(),
        "false"
    );

    let again = env.ww(&["doctor"]);
    assert_eq!(again.code, 0, "{again:?}");
    let lines = printed(&again);
    assert!(!lines.is_empty(), "{again:?}");
    assert!(
        lines.iter().all(|l| l.starts_with("ok:")),
        "a second run prints only ok lines: {again:?}"
    );
}

#[test] // spec: CORE-040
fn non_interactive_doctor_never_sets_autocrlf() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = autocrlf_workspace(&env);
    // The settings the doctor itself fixes are in place, so only autocrlf could change it.
    env.git(&ws, &["config", "--local", "core.longpaths", "true"]);
    env.git(
        &ws,
        &["config", "--local", "lfs.setlockablereadonly", "false"],
    );
    let config = ws.join(".git").join("config");
    let before = sha256_file(&config);

    let out = env.ww_with_stdin(&["--non-interactive", "doctor"], "y\n");
    assert_eq!(out.code, 1, "{out:?}");
    assert!(!out.stderr.contains("[y/N]"), "nothing is asked: {out:?}");
    assert!(
        printed(&out)
            .iter()
            .any(|l| l.starts_with("problem: core.autocrlf is true")),
        "{out:?}"
    );
    assert_eq!(sha256_file(&config), before, ".git\\config changed");
}

#[test] // spec: ARCH-090, CORE-007
fn doctor_with_a_stale_default_reports_it_and_still_runs() {
    let env = TestEnv::with_label("Ünïcödé space");
    let gone = env.root().join("ws-Ünïcödé gone");
    env.register_workspace(&gone, true);

    let out = env.ww(&["doctor"]);
    assert_eq!(out.code, 1, "it ran and a problem remains: {out:?}");
    let lines = printed(&out);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("problem: the default workspace")
                && l.contains(&gone.display().to_string())
                && l.contains("ww workspace use \"")),
        "{out:?}"
    );
    assert!(
        lines.contains(&"ok: no suite state database yet; the core creates one when it starts"),
        "{out:?}"
    );
}

#[test] // spec: CORE-004, ARCH-018
fn doctor_without_a_default_workspace_skips_the_workspace_checks() {
    let env = TestEnv::with_label("Ünïcödé space");
    let skipped = "ok: no default workspace; workspace checks skipped";

    let out = env.ww(&["doctor"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(printed(&out).contains(&skipped), "{out:?}");
    let probe = env.ww(&["--no-start", "status"]);
    assert_eq!(probe.code, 3, "no core was started: {probe:?}");

    let ws = env.make_workspace("Ünïcödé not default");
    env.register_workspace(&ws, false);
    let out = env.ww(&["doctor"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(printed(&out).contains(&skipped), "{out:?}");
    let probe = env.ww(&["--no-start", "status"]);
    assert_eq!(probe.code, 3, "no core was started: {probe:?}");
}

/// `path` with `suffix` added to its file name.
fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// The files beside `db` whose name starts with `prefix`.
fn named(db: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(db.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(prefix))
        })
        .collect();
    found.sort();
    found
}

#[test] // spec: CORE-004 (ruling Q26, M1)
fn doctor_sets_leftover_side_files_aside_only_after_yes() {
    let env = TestEnv::with_label("Ünïcödé space");
    let db = env.env.suite_state_db();
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let wal = beside(&db, "-wal");
    std::fs::write(&wal, b"an old write-ahead log").unwrap();

    let no = env.ww_with_stdin(&["doctor"], "n\n");
    assert_eq!(no.code, 1, "{no:?}");
    let lines = printed(&no);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("problem: the suite state database")
                && l.contains("state.db-wal")),
        "{no:?}"
    );
    assert!(
        lines.contains(
            &"problem: workspace checks skipped — the leftover files must be set aside before the core starts"
        ),
        "{no:?}"
    );
    assert_eq!(std::fs::read(&wal).unwrap(), b"an old write-ahead log");
    assert!(!db.exists(), "no database was created");
    assert!(
        no.stderr
            .contains("Set them aside as state.db-wal.damaged-"),
        "the question names the real files: {no:?}"
    );
    assert!(!no.stderr.contains("<name>"), "{no:?}");
    assert!(no.stdout.contains("answer y to set them aside"), "{no:?}");

    let yes = env.ww_with_stdin(&["doctor"], "y\n");
    assert_eq!(yes.code, 0, "{yes:?}");
    assert!(!wal.exists(), "{yes:?}");
    let aside = named(&db, "state.db-wal.damaged-");
    assert_eq!(aside.len(), 1, "{aside:?}");
    assert_eq!(std::fs::read(&aside[0]).unwrap(), b"an old write-ahead log");
}

#[test] // spec: CORE-004 (ruling Q26, M3)
fn doctor_offers_nothing_while_a_core_runs() {
    let env = TestEnv::with_label("Ünïcödé space");
    let _core = ww_testkit::CoreFixture::start(&env);
    let db = env.env.suite_state_db();
    // A hot journal beside the live database: the doctor must not open it read-write, nor
    // move anything, while the core holds the files.
    let journal = beside(&db, "-journal");
    std::fs::write(
        &journal,
        [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7, 0, 0],
    )
    .unwrap();
    let (db_before, journal_before) = (sha256_file(&db), sha256_file(&journal));

    let out = env.ww_with_stdin(&["doctor"], "y\ny\n");
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stdout.contains("stop it first (ww core stop)"),
        "{out:?}"
    );
    assert!(!out.stderr.contains("[y/N]"), "nothing is offered: {out:?}");
    assert_eq!(sha256_file(&db), db_before);
    assert_eq!(sha256_file(&journal), journal_before);
    assert!(named(&db, "state.db.damaged-").is_empty());
}

#[test] // spec: CORE-004 (ruling Q26, M4)
fn hot_journal_recovery_then_set_aside_when_still_unusable() {
    let env = TestEnv::with_label("Ünïcödé space");
    let db = env.env.suite_state_db();
    // After SQLite rolls the journal back the database is still from a newer version.
    env.write_hot_journal_db(&db, 999);

    let out = env.ww_with_stdin(&["doctor"], "y\ny\n");
    assert_eq!(out.code, 0, "{out:?}");
    let recover = out
        .stderr
        .find("Let SQLite finish its recovery? [y/N]")
        .unwrap_or_else(|| panic!("{out:?}"));
    let still = out
        .stderr
        .find("still can't be used")
        .unwrap_or_else(|| panic!("{out:?}"));
    let aside = out
        .stderr
        .find("Set it aside as state.db.damaged-")
        .unwrap_or_else(|| panic!("{out:?}"));
    assert!(recover < still && still < aside, "{out:?}");
    assert!(
        printed(&out).iter().any(|l| l.starts_with("fixed:")
            && l.contains("newer version")
            && l.contains("set aside as state.db.damaged-")),
        "{out:?}"
    );
    assert!(!db.exists(), "{out:?}");
    assert_eq!(named(&db, "state.db.damaged-").len(), 1);

    let status = env.ww(&["status"]);
    assert_eq!(status.code, 0, "{status:?}");
}
