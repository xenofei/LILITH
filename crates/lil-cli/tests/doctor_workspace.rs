//! `lil doctor` and the default workspace, against the real `lil` and `lil-core` (CORE-004,
//! CORE-038; owner decision 5; plan-2b Minor 7, the rename's RN10 carry, owner question 1):
//! - the workspace's `state.db` in its local folder: damaged, or `-wal`/`-shm`/`-journal` files
//!   left without it, is set aside only after `y`, never with `--non-interactive`;
//! - a plugin-era workspace (`worldwright.yaml`, `.worldwright\state.json`, `ww://` refs) is
//!   registered as it is, reported, and renamed only after `y`.
//!
//! The prompts are answered through stdin. Every test runs on its own `TestEnv` channel with
//! isolated git (QA-038); any core `lil` starts is drained when the env drops (QA-041).

use std::path::{Path, PathBuf};

use lil_testkit::{LilOutput, TestEnv, sha256_file};

/// The non-empty stdout lines.
fn printed(out: &LilOutput) -> Vec<&str> {
    out.stdout
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty())
        .collect()
}

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

/// The default workspace and its `state.db` in the workspace's local folder (created empty).
fn workspace_db(env: &TestEnv) -> (PathBuf, PathBuf) {
    let ws = env.make_workspace("Ünïcödé ws");
    env.register_workspace(&ws, true);
    let db = env
        .env
        .workspace_local_dir(&ws)
        .expect("the workspace's local folder")
        .join("state.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    (ws, db)
}

/// `YYYYMMDDTHHMMSSZ`.
fn is_compact_utc(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() == 16
        && b[8] == b'T'
        && b[15] == b'Z'
        && b[..8].iter().chain(&b[9..15]).all(u8::is_ascii_digit)
}

#[test] // spec: CORE-004
fn doctor_sets_a_damaged_workspace_state_db_aside_after_yes() {
    let env = TestEnv::with_label("Ünïcödé space");
    let (_ws, db) = workspace_db(&env);
    std::fs::write(&db, vec![0xAB_u8; 4096]).unwrap();
    let wal = beside(&db, "-wal");
    let shm = beside(&db, "-shm");
    std::fs::write(&wal, b"an old write-ahead log").unwrap();
    std::fs::write(&shm, b"its index").unwrap();
    let before = sha256_file(&db);
    let shown = db.display().to_string();

    // `--non-interactive` never asks and never moves it; a waiting `y` is never read.
    let quiet = env.lil_with_stdin(&["--non-interactive", "doctor"], "y\n");
    assert_eq!(quiet.code, 1, "{quiet:?}");
    assert!(
        !quiet.stderr.contains("[y/N]"),
        "nothing is asked: {quiet:?}"
    );
    assert!(
        quiet.stdout.contains("run lil doctor interactively"),
        "{quiet:?}"
    );
    assert_eq!(sha256_file(&db), before);
    assert!(named(&db, "state.db.damaged-").is_empty(), "{quiet:?}");

    // `n`: reported with its full path, nothing changes, and the workspace checks are skipped.
    let no = env.lil_with_stdin(&["doctor"], "n\n");
    assert_eq!(no.code, 1, "{no:?}");
    let lines = printed(&no);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("problem: the workspace state database")
                && l.contains(&shown)
                && l.contains("damaged")),
        "{no:?}"
    );
    assert!(
        lines.contains(&"problem: workspace checks skipped — the core can't open the workspace"),
        "{no:?}"
    );
    assert!(no.stderr.contains("[y/N]"), "the question is asked: {no:?}");
    assert_eq!(sha256_file(&db), before, "answering n changed the database");
    assert_eq!(std::fs::read(&wal).unwrap(), b"an old write-ahead log");
    assert!(named(&db, "state.db.damaged-").is_empty(), "{no:?}");

    // `y`: the database and its side files are set aside under one UTC stamp, and the core
    // then opens the workspace with a fresh database.
    let yes = env.lil_with_stdin(&["doctor"], "y\n");
    assert_eq!(yes.code, 0, "{yes:?}");
    let lines = printed(&yes);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("fixed: the workspace state database")
                && l.contains("set aside as state.db.damaged-")),
        "{yes:?}"
    );
    assert!(
        lines.contains(&"ok: no orphans"),
        "the workspace checks ran: {yes:?}"
    );
    let aside = named(&db, "state.db.damaged-");
    assert_eq!(aside.len(), 1, "{aside:?}");
    assert_eq!(sha256_file(&aside[0]), before, "the damaged bytes are kept");
    let name = aside[0].file_name().unwrap().to_string_lossy().into_owned();
    let stamp = name.strip_prefix("state.db.damaged-").unwrap().to_string();
    assert!(is_compact_utc(&stamp), "{name}");
    // The log keeps its bytes; the `-shm` index is SQLite's own and may have been rebuilt by
    // the read-only look at the log, so only its move is checked.
    let wal_aside = db.with_file_name(format!("state.db-wal.damaged-{stamp}"));
    assert_eq!(
        std::fs::read(&wal_aside).unwrap(),
        b"an old write-ahead log"
    );
    assert!(
        db.with_file_name(format!("state.db-shm.damaged-{stamp}"))
            .is_file()
    );
    assert!(db.exists(), "the core made a fresh database");
    assert_ne!(sha256_file(&db), before);
}

#[test] // spec: CORE-004
fn doctor_offers_to_set_aside_orphan_sidecars_in_the_local_folder() {
    let env = TestEnv::with_label("Ünïcödé space");
    let (_ws, db) = workspace_db(&env);
    let wal = beside(&db, "-wal");
    std::fs::write(&wal, b"an old write-ahead log").unwrap();

    let no = env.lil_with_stdin(&["doctor"], "n\n");
    assert_eq!(no.code, 1, "{no:?}");
    let lines = printed(&no);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("problem: the workspace state database")
                && l.contains(&wal.display().to_string())),
        "the problem names the full path: {no:?}"
    );
    assert!(
        no.stderr
            .contains("Set them aside as state.db-wal.damaged-"),
        "{no:?}"
    );
    assert_eq!(std::fs::read(&wal).unwrap(), b"an old write-ahead log");
    assert!(!db.exists(), "no database was created beside the old log");

    let yes = env.lil_with_stdin(&["doctor"], "y\n");
    assert_eq!(yes.code, 0, "{yes:?}");
    // The core's own new log may be there now, never the old one.
    assert_ne!(
        std::fs::read(&wal).ok().as_deref(),
        Some(&b"an old write-ahead log"[..]),
        "{yes:?}"
    );
    let aside = named(&db, "state.db-wal.damaged-");
    assert_eq!(aside.len(), 1, "{aside:?}");
    assert_eq!(std::fs::read(&aside[0]).unwrap(), b"an old write-ahead log");
}

/// Every file under `dir` with its bytes, by path.
fn files_under(dir: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    let mut found = std::collections::BTreeMap::new();
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                folders.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                found.insert(path, bytes);
            }
        }
    }
    found
}

fn set_read_only(path: &Path) {
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(path, permissions).unwrap();
}

#[test] // spec: ARCH-002, CORE-004, CORE-026 (owner question 1)
fn legacy_named_workspace_is_reported_not_reinitialised() {
    use lil_platform::names::{LEGACY_WORKSPACE_DIR, WORKSPACE_FILE};
    let env = TestEnv::with_label("Ünïcödé space");
    // A plugin-era workspace: the legacy workspace file, the legacy hint naming `kites`, a
    // manifest whose `inputs` hold a legacy ref, and a sealed snapshot holding one too.
    let ws = env.root().join("plugin era");
    let file = ws.join("worldwright.yaml");
    let hint = ws.join(".worldwright\\state.json");
    let asset = ws
        .join("projects")
        .join("kites")
        .join("assets")
        .join("kite_red");
    let manifest = asset.join("kite_red.manifest.json");
    let snapshot = asset.join("v1.0.0").join("manifest.snapshot.json");
    std::fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
    std::fs::create_dir_all(hint.parent().unwrap()).unwrap();
    std::fs::write(&file, "name: plugin era\n").unwrap();
    std::fs::write(&hint, r#"{"active_project":"kites"}"#).unwrap();
    std::fs::write(
        ws.join("projects").join("kites").join("project.yaml"),
        "name: Kites\nslug: kites\ntype: media\nstatus: active\nlinks: []\ncreated: 2026-09-01\n",
    )
    .unwrap();
    let id = env.workspace_id(&ws);
    let text = format!(
        "{{\n  \"asset_id\": \"kite_red\",\n  \"project\": \"kites\",\n  \"type\": \"prop_2d\",\n  \
         \"version\": \"1.1.0\",\n  \"approved_version\": \"1.0.0\",\n  \"inputs\": [\n    \
         {{\"ref\": \"ww://{id}/kites/kite_blue@1.0.0\", \"role\": \"reference\"}}\n  ]\n}}\n"
    );
    std::fs::write(&manifest, &text).unwrap();
    let sealed = format!("{{\"inputs\": [{{\"ref\": \"ww://{id}/kites/kite_blue@1.0.0\"}}]}}\n");
    std::fs::write(&snapshot, &sealed).unwrap();
    set_read_only(&snapshot);
    let before = files_under(&ws);
    let new_file = ws.join(WORKSPACE_FILE);
    let new_hint = ws.join(".lilith").join("state.json");

    // Registered as it is: no new workspace file, and every byte that was there is unchanged.
    let init = env.lil(&["workspace", "init", ws.to_str().unwrap()]);
    assert_eq!(init.code, 0, "{init:?}");
    assert!(init.stdout.contains("workspace registered"), "{init:?}");
    assert!(!new_file.exists(), "{init:?}");
    let after = files_under(&ws);
    for (path, bytes) in &before {
        assert_eq!(after.get(path), Some(bytes), "{} changed", path.display());
    }

    // The core serves it: the legacy hint's project is the active one.
    let status = env.lil_in(&ws, &["call", "project.status"]);
    assert_eq!(status.code, 0, "{status:?}");
    assert!(
        status.stdout.contains("\"project\": \"kites\""),
        "{status:?}"
    );

    let legacy_untouched = |out: &LilOutput| {
        assert_eq!(std::fs::read(&file).unwrap(), before[&file], "{out:?}");
        assert_eq!(
            std::fs::read(&manifest).unwrap(),
            before[&manifest],
            "{out:?}"
        );
        assert!(!new_file.exists() && !new_hint.exists(), "{out:?}");
    };

    // Reported; `--non-interactive` never asks and never renames.
    let quiet = env.lil_with_stdin(&["--non-interactive", "doctor"], "y\n");
    assert_eq!(quiet.code, 1, "{quiet:?}");
    assert!(
        printed(&quiet)
            .iter()
            .any(|l| l.starts_with("problem: legacy names")),
        "{quiet:?}"
    );
    assert!(!quiet.stderr.contains("[y/N]"), "{quiet:?}");
    legacy_untouched(&quiet);

    // `n`: nothing changes.
    let no = env.lil_with_stdin(&["doctor"], "n\n");
    assert_eq!(no.code, 1, "{no:?}");
    assert!(no.stderr.contains("Rename them now? [y/N]"), "{no:?}");
    legacy_untouched(&no);

    // `y`: renamed through the core's store; nothing deleted, the sealed snapshot skipped.
    let yes = env.lil_with_stdin(&["doctor"], "y\n");
    assert_eq!(yes.code, 0, "{yes:?}");
    let lines = printed(&yes);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("fixed: legacy names renamed")),
        "{yes:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("skipped:") && l.contains("v1.0.0/manifest.snapshot.json")),
        "{yes:?}"
    );
    assert_eq!(std::fs::read(&new_file).unwrap(), before[&file]);
    assert!(!file.exists());
    assert_eq!(std::fs::read(&new_hint).unwrap(), before[&hint]);
    assert_eq!(
        std::fs::read_to_string(&manifest).unwrap(),
        text.replace("\"ww://", "\"lil://"),
        "only the ref changed"
    );
    assert_eq!(std::fs::read(&snapshot).unwrap(), before[&snapshot]);
    assert!(
        std::fs::metadata(&snapshot)
            .unwrap()
            .permissions()
            .readonly()
    );
    assert!(ws.join(LEGACY_WORKSPACE_DIR).is_dir());
    assert_eq!(std::fs::read(&hint).unwrap(), before[&hint]);

    // Later: the snapshot's legacy ref is information, never a problem.
    let later = env.lil_with_stdin(&["doctor"], "");
    assert_eq!(later.code, 0, "{later:?}");
    let lines = printed(&later);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("info:") && l.contains("v1.0.0/manifest.snapshot.json")),
        "{later:?}"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("problem:")),
        "{later:?}"
    );
    assert!(!later.stderr.contains("[y/N]"), "{later:?}");
}
