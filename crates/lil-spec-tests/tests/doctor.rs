//! `lil doctor`: the repository's git config and the workspace's `.gitignore` and
//! `.gitattributes`, a second run that changes nothing, and a damaged suite `state.db` that is
//! set aside only after the person types `y` (spec CORE-004, CORE-040, ARCH-085, QA-060;
//! Global Constraints "The `lil` commands this plan adds", owner decision 5). Driven through the
//! real `lil` (`TestEnv::lil`), isolated git (`TestEnv::git`) and the files on disk (QA-018).

use std::path::{Path, PathBuf};

use lil_testkit::{LilOutput, TestEnv, sha256_file, utc};

/// The five `.gitignore` entries `lil doctor` adds when missing (Global Constraints, CORE-040).
const GITIGNORE_ENTRIES: [&str; 5] = [
    ".lilith/",
    ".trash/",
    "inbox/*",
    "projects/*/assets/*/candidates/",
    "projects/*/assets/*/work/",
];

/// The lines of a text file, without line endings.
fn lines_of(path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    text.lines()
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect()
}

/// The non-empty lines `lil` printed on stdout.
fn printed(out: &LilOutput) -> Vec<&str> {
    out.stdout
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty())
        .collect()
}

/// A damaged suite `state.db` (4 KiB that are not SQLite); returns its path and SHA-256.
fn damaged_state_db(env: &TestEnv) -> (PathBuf, String) {
    let db = env.env.suite_state_db();
    let parent = db.parent().expect("state.db has a folder");
    std::fs::create_dir_all(parent)
        .unwrap_or_else(|e| panic!("cannot create {}: {e}", parent.display()));
    std::fs::write(&db, vec![0xAB_u8; 4096])
        .unwrap_or_else(|e| panic!("cannot write {}: {e}", db.display()));
    let sha = sha256_file(&db);
    (db, sha)
}

/// The `state.db.damaged-*` files beside `db`.
fn set_aside_files(db: &Path) -> Vec<PathBuf> {
    let parent = db.parent().expect("state.db has a folder");
    let mut found: Vec<PathBuf> = std::fs::read_dir(parent)
        .unwrap_or_else(|e| panic!("cannot list {}: {e}", parent.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("state.db.damaged-"))
        })
        .collect();
    found.sort();
    found
}

#[test] // spec: CORE-040, ARCH-085, QA-060
fn doctor_fixes_git_config_and_gitignore_then_changes_nothing() {
    let env = TestEnv::new();
    let ws = env.copy_fixture_workspace("doctor");
    env.register_workspace(&ws, true);
    env.git_init(&ws);
    let gitignore = ws.join(".gitignore");
    let gitattributes = ws.join(".gitattributes");
    std::fs::write(&gitignore, b"").unwrap();
    if gitattributes.exists() {
        std::fs::remove_file(&gitattributes).unwrap();
    }

    // `y` sets up the notification shortcut and handlers (plan 4a, APP-022), the only questions.
    let first = env.lil_with_stdin(&["doctor"], "y\ny\n");
    assert_eq!(first.code, 0, "{first:?}");
    let lines = printed(&first);
    assert!(
        lines.iter().any(|l| l.starts_with("fixed:")),
        "the first run fixes things and says so: {first:?}"
    );

    assert_eq!(env.git(&ws, &["config", "core.longpaths"]).trim(), "true");
    assert_eq!(
        env.git(&ws, &["config", "lfs.setlockablereadonly"]).trim(),
        "false"
    );
    let config = env.git(&ws, &["config", "--list"]);
    assert!(
        !config
            .lines()
            .any(|l| l.to_ascii_lowercase().starts_with("core.autocrlf=")),
        "core.autocrlf must stay unset; doctor neither asks for nor sets it:\n{config}"
    );
    let attributes = lines_of(&gitattributes);
    assert!(
        attributes.iter().any(|l| l == "projects/** -text"),
        ".gitattributes lacks `projects/** -text`: {attributes:?}"
    );
    let ignored = lines_of(&gitignore);
    for entry in GITIGNORE_ENTRIES {
        assert!(
            ignored.iter().any(|l| l == entry),
            ".gitignore lacks {entry:?}: {ignored:?}"
        );
    }

    let watched = [
        gitignore.clone(),
        gitattributes.clone(),
        ws.join(".git").join("config"),
    ];
    let hashes: Vec<String> = watched.iter().map(|p| sha256_file(p)).collect();

    let second = env.lil(&["doctor"]);
    assert_eq!(second.code, 0, "{second:?}");
    let lines = printed(&second);
    assert!(!lines.is_empty(), "{second:?}");
    assert!(
        lines.iter().all(|l| l.starts_with("ok:")),
        "a second run prints only `ok:` lines: {second:?}"
    );
    for (path, before) in watched.iter().zip(&hashes) {
        assert_eq!(
            &sha256_file(path),
            before,
            "the second run changed {}",
            path.display()
        );
    }
}

#[test] // spec: CORE-004
fn doctor_sets_damaged_state_db_aside_only_after_yes() {
    let env = TestEnv::new();
    let (db, before) = damaged_state_db(&env);

    let status = env.lil(&["status"]);
    assert!(
        status.stderr.contains("`lil doctor` can set it aside."),
        "{status:?}"
    );
    assert!(!status.stderr.contains("(plan 2b)"), "{status:?}");

    let no = env.lil_with_stdin(&["doctor"], "n\n");
    assert_eq!(no.code, 1, "{no:?}");
    assert_eq!(sha256_file(&db), before, "answering n changed the database");
    assert!(set_aside_files(&db).is_empty(), "{no:?}");

    // Then `y` to the notification setup's two questions (plan 4a, APP-022).
    let yes = env.lil_with_stdin(&["doctor"], "y\ny\ny\n");
    assert_eq!(yes.code, 0, "{yes:?}");
    assert!(
        printed(&yes).contains(&"ok: no default workspace; workspace checks skipped"),
        "{yes:?}"
    );
    let aside = set_aside_files(&db);
    assert_eq!(aside.len(), 1, "one set-aside file: {aside:?}");
    let name = aside[0].file_name().unwrap().to_string_lossy().into_owned();
    let suffix = name.strip_prefix("state.db.damaged-").unwrap();
    assert!(
        utc::compact_now_pattern()(suffix),
        "{suffix} is not a YYYYMMDDTHHMMSSZ suffix of now"
    );
    assert_eq!(
        sha256_file(&aside[0]),
        before,
        "the set-aside file must hold the damaged bytes"
    );

    let after = env.lil(&["status"]);
    assert_eq!(after.code, 0, "{after:?}");
}

#[test] // spec: CORE-004
fn non_interactive_doctor_never_moves_state_db() {
    let env = TestEnv::new();
    let (db, before) = damaged_state_db(&env);

    let out = env.lil(&["--non-interactive", "doctor"]);
    assert_eq!(out.code, 1, "{out:?}");
    assert_eq!(sha256_file(&db), before, "the database changed");
    assert!(set_aside_files(&db).is_empty(), "{out:?}");
    assert!(
        out.stdout.contains("lil doctor interactively"),
        "stdout must say to run lil doctor interactively: {out:?}"
    );
}
