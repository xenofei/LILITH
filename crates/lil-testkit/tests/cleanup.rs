//! The in-process fixtures of `lil-core`'s tests shut down before their `TestEnv` drops, so no
//! test folder is left in `%TEMP%` (the rename's carry; plan-2b final review).
//!
//! Each fixture's `shutdown()` closes the hub and joins the state actors, then fails its test if
//! `TestEnv::close` leaves anything behind, so every run of those tests checks it. This test
//! runs five of them in one `cargo test` process, the way the leaks showed up (many tests, one
//! process), and checks that [`lil_testkit::cleanup::leftovers_log`] gained no entry for them.
//! It runs cargo, so it is ignored by default:
//! `cargo nextest run -p lil-testkit --run-ignored only -E 'test(leave_no_test_root)'`.
//!
//! It also checks the test registry roots: a `TestEnv` names its owner in its root and its drop
//! removes it, and the sweep of the roots killed tests leave behind
//! ([`lil_testkit::env::sweep_test_registry_roots`]) follows its rule, checked over a fake
//! registry ([`lil_testkit::env::sweep_with`]) so no other run's key can be hit.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

use lil_platform::desktop::{TestRegistryRoot, test_registry_roots};

/// The five tests, as `(test binary, test name)`.
const TESTS: [(&str, &str); 5] = [
    (
        "assets",
        "import_copy_creates_asset_with_uuid_and_candidate_run",
    ),
    (
        "record",
        "record_runs_after_meter_and_applies_the_plan_in_write_order",
    ),
    (
        "projects",
        "resolve_explicit_then_session_then_hint_then_sketchbook",
    ),
    ("settings", "cascade_is_a_plain_override_chain"),
    (
        "versions",
        "seal_moves_files_and_writes_a_read_only_snapshot",
    ),
];

/// The leftovers log's text from byte `from` on (empty when there is no log).
fn log_since(from: u64) -> String {
    let bytes = std::fs::read(lil_testkit::cleanup::leftovers_log()).unwrap_or_default();
    let from = usize::try_from(from).unwrap_or(usize::MAX).min(bytes.len());
    String::from_utf8_lossy(&bytes[from..]).into_owned()
}

#[test] // spec: QA-040
#[ignore = "runs cargo test for five lil-core test binaries"]
fn in_process_fixtures_leave_no_test_root() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("lil-testkit lives at <workspace>/crates/lil-testkit");
    let before = std::fs::metadata(lil_testkit::cleanup::leftovers_log()).map_or(0, |m| m.len());

    let mut cargo = Command::new(env!("CARGO"));
    cargo.args(["test", "--locked", "--offline", "-p", "lil-core"]);
    cargo.arg("--manifest-path").arg(root.join("Cargo.toml"));
    for (binary, _) in TESTS {
        cargo.args(["--test", binary]);
    }
    cargo.args(["--", "--exact"]);
    cargo.args(TESTS.iter().map(|(_, name)| *name));
    let out = cargo.current_dir(root).output().expect("cargo runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "the five tests failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for (_, name) in TESTS {
        assert!(
            stdout.contains(&format!("test {name} ... ok")),
            "{name} didn't run:\n{stdout}"
        );
    }

    let added = log_since(before);
    let leaked: Vec<&str> = TESTS
        .iter()
        .map(|(_, name)| *name)
        .filter(|name| added.contains(&format!("test `{name}`")))
        .collect();
    assert!(
        leaked.is_empty(),
        "these tests left their folder behind: {leaked:?}\n{added}"
    );
}

/// `reg <args>`; whether it succeeded.
fn reg(args: &[&str]) -> bool {
    Command::new("reg")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap_or_else(|e| panic!("cannot run reg: {e}"))
        .success()
}

/// A root as the sweep sees it, last written `age` before `now`.
fn root(id: &str, owner: Option<&str>, now: SystemTime, age: Duration) -> TestRegistryRoot {
    TestRegistryRoot {
        id: id.to_string(),
        owner: owner.map(str::to_string),
        last_written: now - age,
    }
}

/// A killed test leaves its `HKCU\Software\LILITH-test-<id>` key (Task 3's review M9). The
/// sweep's rule, over a fake registry so no real key of another worktree can be hit: a key is
/// removed only when at least an hour old, and either its owner is dead (its folder may be
/// left: a killed test leaves both) or, naming no owner, no `lil-testenv-*<id>-*` folder is
/// left; a failed removal doesn't stop the rest.
#[test] // spec: QA-038
fn leftover_test_registry_keys_are_swept() {
    let now = SystemTime::now();
    let hours = Duration::from_secs(2 * 60 * 60);
    let roots = [
        root("0000000a", None, now, hours), // an old orphan of an old test kit
        root("0000000b", Some("dead"), now, hours), // an old orphan whose owner ended
        root("0000000c", None, now, Duration::from_secs(5 * 60)), // fresh: a test still running
        root("0000000d", Some("live"), now, hours), // its owner still runs
        root("0000000e", None, now, hours), // its folder is there
        root("0000000f", None, now, hours), // its labelled folder is there
        root("00000010", None, now, hours), // can't be removed
        root("00000011", None, now, hours), // removed after the failure
        root("00000013", Some("dead"), now, hours), // a killed test: dead owner, folder left
        root("00000014", Some("live"), now, hours), // a live owner with its folder: kept
        TestRegistryRoot {
            // Written after `now` (a clock change): kept.
            last_written: now + hours,
            ..root("00000012", None, now, hours)
        },
    ];
    let folders = [
        "lil-testenv-0000000e-x1y2z3".to_string(),
        "lil-testenv-ÃœnÃ¯cÃ¶dÃ© space-0000000f-a1b2c3".to_string(),
        // Not a TestEnv folder, and the id doesn't end a segment: neither counts.
        "lil-other-0000000a-a1b2c3".to_string(),
        "lil-testenv-0000000b0-a1b2c3".to_string(),
        "lil-testenv-00000013-k1l2m3".to_string(),
        "lil-testenv-00000014-n1o2p3".to_string(),
    ];
    let alive = |owner: &str| owner == "live";
    let mut asked = Vec::new();
    let mut remove = |id: &str| {
        asked.push(id.to_string());
        if id == "00000010" {
            Err(std::io::Error::other("access denied"))
        } else {
            Ok(())
        }
    };
    let sweep = lil_testkit::env::sweep_with(&roots, &folders, now, &alive, &mut remove);

    assert_eq!(
        asked,
        ["0000000a", "0000000b", "00000010", "00000011", "00000013"]
    );
    assert_eq!(
        sweep.removed,
        [
            r"HKCU\Software\LILITH-test-0000000a",
            r"HKCU\Software\LILITH-test-0000000b",
            r"HKCU\Software\LILITH-test-00000011",
            r"HKCU\Software\LILITH-test-00000013",
        ]
    );
    assert_eq!(sweep.failed.len(), 1, "{sweep:?}");
    assert_eq!(sweep.failed[0].0, r"HKCU\Software\LILITH-test-00000010");
}

/// The owner test: this process is alive; the same pid with another start time, or a value that
/// doesn't parse, isn't.
#[test] // spec: QA-038
fn a_root_owner_is_alive_only_as_this_very_process() {
    let owner = lil_testkit::env::process_owner().expect("this process's start time");
    assert!(lil_testkit::env::owner_alive(&owner), "{owner}");
    let (pid, started) = owner.split_once(':').unwrap();
    let reused = format!("{pid}:{}", started.parse::<u64>().unwrap() + 1);
    assert!(!lil_testkit::env::owner_alive(&reused));
    for bad in ["", "x", "1:", ":1", "4294967296:1"] {
        assert!(!lil_testkit::env::owner_alive(bad), "{bad}");
    }
}

/// The keys `HKCU\Software\<name>` this test made, removed by exact name when it ends.
struct Made<'a>(&'a [String]);

impl Drop for Made<'_> {
    fn drop(&mut self) {
        for name in self.0 {
            let _ = reg(&["delete", &format!(r"HKCU\Software\{name}"), "/f"]);
        }
    }
}

/// A `TestEnv`'s registry root names its owner from the start and is listed with it (read-only:
/// nothing but this test's own keys is written or removed); keys of another shape are never
/// listed; and the drop removes the root with everything a doctor `y` would write below it.
#[test] // spec: QA-038
fn a_test_env_owns_its_registry_root_and_its_drop_removes_it() {
    let env = lil_testkit::TestEnv::new();
    let id = env.env.channel().to_string();
    let id = id.strip_prefix("test-").unwrap().to_string();
    let listed = test_registry_roots().unwrap();
    let ours = listed
        .iter()
        .find(|r| r.id == id)
        .unwrap_or_else(|| panic!("{id} not in {listed:?}"));
    assert_eq!(ours.owner, lil_testkit::env::process_owner());
    let age = SystemTime::now()
        .duration_since(ours.last_written)
        .unwrap_or_default();
    assert!(age < Duration::from_secs(600), "{ours:?}");

    // Other shapes, made from this test's own id so no other run's key is involved.
    let other_shapes = [
        format!("LILITH-test-{id}0"),
        format!("LILITH-test-{}", &id[..7]),
        format!("LILITH-testx-{id}"),
    ];
    // No sweep ever removes these shapes, so they go by exact name even if the test panics.
    let _made = Made(&other_shapes);
    for name in &other_shapes {
        assert!(reg(&[
            "add",
            &format!(r"HKCU\Software\{name}"),
            "/v",
            "x",
            "/d",
            "x",
            "/f"
        ]));
    }
    let listed = test_registry_roots().unwrap();
    for name in &other_shapes {
        let tail = name.split_once("-test-").map_or("", |(_, t)| t);
        assert!(!listed.iter().any(|r| r.id == tail), "{name} listed");
    }

    let key = env.env.registry_root();
    lil_platform::desktop::register_handlers(&env.env, &env.root().join("lil-image.exe")).unwrap();
    assert!(reg(&["query", &format!(r"{key}\Classes")]));
    drop(env);
    assert!(!reg(&["query", &key]), "{key} was left after the drop");
}

/// A folder entry as the sweep sees it, last written `age` before `now`.
fn entry(name: &str, is_dir: bool, now: SystemTime, age: Duration) -> lil_testkit::env::SweepEntry {
    lil_testkit::env::SweepEntry {
        path: std::path::PathBuf::from(r"C:\scratch").join(name),
        is_dir,
        modified: now - age,
    }
}

/// SC-06, SC-05: the age-gated sweep of leftover test folders chooses a `lil-testenv-<id>-*`
/// folder in `%TEMP%` or a `LILITH-test-<8 hex>` folder in the real local folder only when it is
/// a directory, at least an hour old, and no registry root with its id has a live owner.
#[test] // spec: QA-038
fn old_test_folders_without_a_live_owner_are_chosen() {
    let now = SystemTime::now();
    let old = Duration::from_secs(2 * 60 * 60);
    let new = Duration::from_secs(10 * 60);
    let entries = [
        entry("lil-testenv-0000000a-x1y2z3", true, now, old), // chosen: no root
        entry("lil-testenv-0000000b-x1y2z3", true, now, new), // too new
        entry("lil-testenv-0000000c-x1y2z3", true, now, old), // its root's owner is alive
        entry("lil-testenv-0000000d-x1y2z3", true, now, old), // its root's owner is dead: chosen
        entry("lil-testenv-Ünïcödé space-0000000e-a1b2c3", true, now, old), // labelled: chosen
        entry("lil-testenv-0000000f-x1y2z3", false, now, old), // a file
        entry("lil-testenv-leftovers.log", false, now, old),
        entry("lil-testenv-a1b2c3", true, now, old), // the old no-id form
        entry("lil-testenv-0000000A-x1y2z3", true, now, old), // not lowercase hex
        entry("lil-testenv-00000010-x1y2z3", true, now + old, old), // modified after now
        entry("LILITH-test-00000011", true, now, old), // a Qt cache folder: chosen
        entry("LILITH-test-00000012", true, now, new),
        entry("LILITH-test-topbarcheck", true, now, old), // a hand-made folder
        entry("LILITH-test-t18uia", true, now, old),
        entry("LILITH-test-00000013-extra", true, now, old),
        entry("LILITH", true, now, old),
        entry("other-0000000a-x1y2z3", true, now, old),
    ];
    let roots = [
        root("0000000c", Some("live"), now, old),
        root("0000000d", Some("dead"), now, old),
    ];
    let alive = |owner: &str| owner == "live";
    let chosen: Vec<String> = lil_testkit::env::folders_to_sweep(&entries, &roots, now, &alive)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        chosen,
        [
            "lil-testenv-0000000a-x1y2z3",
            "lil-testenv-0000000d-x1y2z3",
            "lil-testenv-Ünïcödé space-0000000e-a1b2c3",
            "LILITH-test-00000011",
        ]
    );
}

/// The sweep over real folders (a scratch one, so no other run's folder can be hit): what the
/// rule chooses goes, at most `cap` per pass, and nothing else is touched. Time is read as two
/// hours on, so the fresh folders are old.
#[test] // spec: QA-038
fn the_sweep_removes_what_the_rule_chooses_up_to_its_cap() {
    let scratch = tempfile::Builder::new()
        .prefix("lil-testkit-sweep-")
        .tempdir()
        .unwrap();
    let make = |name: &str| {
        let dir = scratch.path().join(name);
        std::fs::create_dir_all(dir.join("local")).unwrap();
        std::fs::write(dir.join("local").join("f.txt"), name).unwrap();
        dir
    };
    let doomed: Vec<_> = (0..3)
        .map(|i| make(&format!("lil-testenv-0000001{i}-x1y2z3")))
        .collect();
    let alive_dir = make("lil-testenv-0000001a-x1y2z3");
    let by_hand = make("LILITH-test-topbarcheck");
    let file = scratch.path().join("lil-testenv-0000001b-x1y2z3");
    std::fs::write(&file, "a file").unwrap();
    let log = scratch.path().join("lil-testenv-leftovers.log");
    std::fs::write(&log, "a log").unwrap();

    let now = SystemTime::now() + Duration::from_secs(2 * 60 * 60);
    let roots = [root("0000001a", Some("live"), now, Duration::from_secs(60))];
    let alive = |owner: &str| owner == "live";
    let sweep =
        |cap| lil_testkit::env::sweep_folders_in(&[scratch.path()], &roots, now, &alive, cap);
    assert_eq!(sweep(2), 2, "the cap holds");
    assert_eq!(doomed.iter().filter(|d| d.exists()).count(), 1);
    assert_eq!(sweep(2), 1);
    assert!(doomed.iter().all(|d| !d.exists()));
    assert_eq!(sweep(2), 0);
    assert!(alive_dir.exists() && by_hand.exists() && file.exists() && log.exists());
}

/// The leftovers log is cut to its tail once it is over its limit, from a line start, and left
/// alone at or below it.
#[test] // spec: QA-038
fn the_leftovers_log_is_trimmed_to_its_tail_over_the_limit() {
    let dir = tempfile::Builder::new()
        .prefix("lil-testkit-trim-")
        .tempdir()
        .unwrap();
    let log = dir.path().join("lil-testenv-leftovers.log");
    let lines: Vec<String> = (0..100)
        .map(|i| format!("line {i:03} {}", "x".repeat(40)))
        .collect();
    std::fs::write(&log, lines.join("\n") + "\n").unwrap();
    let size = std::fs::metadata(&log).unwrap().len();

    assert!(
        !lil_testkit::env::trim_log(&log, size, 1000).unwrap(),
        "at the limit: kept"
    );
    assert_eq!(std::fs::metadata(&log).unwrap().len(), size);

    assert!(lil_testkit::env::trim_log(&log, size - 1, 1000).unwrap());
    let kept = std::fs::read_to_string(&log).unwrap();
    assert!(kept.len() <= 1000 && kept.len() > 900, "{}", kept.len());
    assert!(kept.starts_with("line "), "from a line start: {kept:?}");
    assert!(
        kept.ends_with(&format!("{}\n", lines[99])),
        "the newest line is kept"
    );
    assert!(!kept.contains("line 000"));
}

/// SC-05: a `TestEnv` removes its channel's Qt cache folder in the real local app-data folder,
/// `LILITH-test-<own id>`, when it closes; a folder of another id there stays.
#[test] // spec: QA-038
fn closing_a_test_env_removes_its_qt_cache_folder() {
    let env = lil_testkit::TestEnv::new();
    let id = env.env.channel().to_string();
    let id = id.strip_prefix("test-").unwrap().to_string();
    let real = lil_platform::RealLocations::from_system().unwrap();
    let ours = real.local.join(format!("LILITH-test-{id}"));
    std::fs::create_dir_all(ours.join("image").join("cache")).unwrap();
    // Exactly this id: one more character is another channel's folder, never touched.
    let other = real.local.join(format!("LILITH-test-{id}0"));
    std::fs::create_dir_all(&other).unwrap();

    let closed = env.close();
    let ours_left = ours.exists();
    let other_left = other.exists();
    // Exact names, made by this test.
    let _ = std::fs::remove_dir_all(&ours);
    let _ = std::fs::remove_dir_all(&other);
    closed.unwrap();
    assert!(!ours_left, "{} was left", ours.display());
    assert!(other_left, "another id's folder was removed");
}
