//! The shared log setup (ARCH-117, ARCH-119; P4a-R20, Appendix G "Diagnostics"): JSON lines in
//! `<process>.<YYYY-MM-DD>.log`, a file a day, 14 kept per process, a day's file stopped at its
//! cap with one `log_capped` line, and a panic hook that logs the panic's location.
//!
//! `init_at` installs the process-wide subscriber, so only one test per process may call it:
//! nextest runs each test in a process of its own, and the panic test calls it in a child.

use std::ffi::OsStr;
use std::io::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lil_platform::logs::{DailyWriter, init_at};
use lil_platform::process::run_hidden;

/// The log files of `process` in `dir`, by name.
fn files_of(dir: &Path, process: &str) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(&format!("{process}.")) && n.ends_with(".log"))
        .collect();
    names.sort();
    names
}

#[test] // spec: ARCH-117
fn one_file_a_day_and_14_kept() {
    let dir = tempfile::tempdir().unwrap();
    // Another process's file and an unrelated file are never touched.
    std::fs::write(dir.path().join("other.2026-01-01.log"), b"{}\n").unwrap();
    std::fs::write(dir.path().join("core.notes.txt"), b"x").unwrap();
    let today = Arc::new(Mutex::new(String::new()));
    let date = today.clone();
    let mut writer = DailyWriter::new(dir.path(), "core", 64 * 1024 * 1024, move || {
        date.lock().unwrap().clone()
    })
    .unwrap();
    for day in 1..=20 {
        *today.lock().unwrap() = format!("2026-01-{day:02}");
        writer.write_all(b"{\"a\":1}\n").unwrap();
        writer.write_all(b"{\"a\":2}\n").unwrap();
    }
    let want: Vec<String> = (7..=20)
        .map(|d| format!("core.2026-01-{d:02}.log"))
        .collect();
    assert_eq!(files_of(dir.path(), "core"), want);
    for name in &want {
        let text = std::fs::read_to_string(dir.path().join(name)).unwrap();
        assert_eq!(text, "{\"a\":1}\n{\"a\":2}\n", "{name}");
    }
    assert!(dir.path().join("other.2026-01-01.log").exists());
    assert!(dir.path().join("core.notes.txt").exists());

    // A second writer on the same day appends to that day's file.
    drop(writer);
    let mut again = DailyWriter::new(dir.path(), "core", 64 * 1024 * 1024, || {
        "2026-01-20".to_string()
    })
    .unwrap();
    again.write_all(b"{\"a\":3}\n").unwrap();
    let text = std::fs::read_to_string(dir.path().join("core.2026-01-20.log")).unwrap();
    assert_eq!(text, "{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n");
}

/// A process that starts again on a day whose file is already capped writes nothing more and
/// no second `log_capped` line; the next day starts a new file.
#[test] // spec: ARCH-117
fn a_restart_on_a_capped_day_writes_no_second_capped_line() {
    let dir = tempfile::tempdir().unwrap();
    let line = b"{\"fields\":{\"message\":\"0123456789abcdef0123456789abcdef\"}}\n";
    let writer = |date: &'static str| {
        DailyWriter::new(dir.path(), "core", 256, move || date.to_string()).unwrap()
    };
    let mut first = writer("2026-02-01");
    for _ in 0..20 {
        first.write_all(line).unwrap();
    }
    drop(first);
    let path = dir.path().join("core.2026-02-01.log");
    let capped = std::fs::read_to_string(&path).unwrap();
    assert_eq!(capped.matches("log_capped").count(), 1, "{capped}");

    let mut again = writer("2026-02-01");
    for _ in 0..20 {
        again.write_all(line).unwrap();
    }
    drop(again);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), capped);

    let mut next = writer("2026-02-02");
    next.write_all(line).unwrap();
    let text = std::fs::read_to_string(dir.path().join("core.2026-02-02.log")).unwrap();
    assert_eq!(text.as_bytes(), line);
}

/// A process name that could leave the log folder, or isn't a plain name, is refused.
#[test] // spec: ARCH-117
fn a_log_process_name_stays_in_the_log_folder() {
    let dir = tempfile::tempdir().unwrap();
    for bad in [
        "",
        r"..\core",
        "a/b",
        r"a\b",
        "core.x",
        "a b",
        &"x".repeat(65),
    ] {
        let err = DailyWriter::new(dir.path(), bad, 1024, || "2026-01-01".to_string())
            .err()
            .unwrap_or_else(|| panic!("{bad:?} must be refused"));
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{bad:?}");
        assert!(init_at(dir.path(), bad, 1024).is_err(), "{bad:?}");
    }
    assert!(DailyWriter::new(dir.path(), "lil-core_2", 1024, || "2026-01-01".into()).is_ok());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test] // spec: ARCH-117
fn a_day_stops_at_the_cap_with_one_capped_line() {
    const CAP: u64 = 4 * 1024;
    let dir = tempfile::tempdir().unwrap();
    let _guard = init_at(dir.path(), "capped", CAP).unwrap();
    for i in 0..400 {
        tracing::info!(i, filler = "0123456789abcdef0123456789abcdef", "a line");
    }
    let names = files_of(dir.path(), "capped");
    assert_eq!(names.len(), 1, "{names:?}");
    let text = std::fs::read_to_string(dir.path().join(&names[0])).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines.len() > 2, "{text}");
    let capped: Vec<&&str> = lines.iter().filter(|l| l.contains("log_capped")).collect();
    assert_eq!(capped.len(), 1, "{text}");
    let last = lines.last().unwrap();
    assert!(last.contains("log_capped"), "{last}");
    // Every line is JSON, and what came before the capped line fits the cap.
    for line in &lines {
        serde_json::from_str::<serde_json::Value>(line).unwrap_or_else(|e| panic!("{e}: {line}"));
    }
    let before = text.len() - last.len() - 1;
    assert!(
        before as u64 <= CAP,
        "{before} bytes before the capped line"
    );
    assert!(lines.iter().any(|l| l.contains("a line")));
}

/// Set by `panic_hook_logs_the_location` for its child run only: the log folder.
const PANIC_HELPER_VAR: &str = "LIL_PLATFORM_TEST_LOG_PANIC";

/// The child half of `panic_hook_logs_the_location`. In a normal test run the variable is unset
/// and this passes without doing anything.
#[test]
fn log_panic_helper() {
    if let Some(dir) = std::env::var_os(PANIC_HELPER_VAR) {
        let _guard = init_at(Path::new(&dir), "panicker", 1024 * 1024).unwrap();
        panic!("the helper panics on purpose");
    }
}

#[test] // spec: ARCH-117, ARCH-119
fn panic_hook_logs_the_location() {
    let dir = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let args: Vec<&OsStr> = ["log_panic_helper", "--exact"]
        .iter()
        .map(OsStr::new)
        .collect();
    let out = run_hidden(
        &exe,
        &args,
        exe.parent().unwrap(),
        &[(PANIC_HELPER_VAR.into(), dir.path().as_os_str().to_owned())],
        Duration::from_secs(120),
    )
    .unwrap();
    assert_ne!(out.code, Some(0), "the helper must fail: {out:?}");
    let names = files_of(dir.path(), "panicker");
    assert_eq!(names.len(), 1, "{names:?}");
    let text = std::fs::read_to_string(dir.path().join(&names[0])).unwrap();
    let line = text
        .lines()
        .find(|l| l.contains("\"panic\""))
        .unwrap_or_else(|| panic!("no panic line: {text}"));
    let value: serde_json::Value = serde_json::from_str(line).unwrap();
    let fields = &value["fields"];
    assert_eq!(fields["panic"], "the helper panics on purpose", "{line}");
    let location = fields["location"].as_str().unwrap_or_default();
    // `file:line:column`, here this file.
    let mut parts = location.rsplitn(3, ':');
    let (column, line_no, file) = (parts.next(), parts.next(), parts.next());
    assert!(file.is_some_and(|f| f.ends_with("logs.rs")), "{location}");
    assert!(
        line_no.is_some_and(|n| n.parse::<u32>().is_ok())
            && column.is_some_and(|n| n.parse::<u32>().is_ok()),
        "{location}"
    );
}
