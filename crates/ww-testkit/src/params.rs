//! The owner-facing test parameters in the workspace-root `test-params.toml` (QA-019), read
//! with the kit's own `toml` so no test leans on the code it tests.
//!
//! A missing key panics, naming the key and the file: a test must never fall back to a value
//! the owner did not set.

use std::path::{Path, PathBuf};

use crate::bins::workspace_root;

/// `<workspace>\test-params.toml`.
pub fn file() -> PathBuf {
    workspace_root().join("test-params.toml")
}

/// `[proptest] cases`: how many cases each property test runs.
///
/// # Panics
/// If the file cannot be read, or the key is missing or not a non-negative integer that fits
/// a `u32`.
pub fn proptest_cases() -> u32 {
    proptest_cases_in(&file())
}

/// `[budgets] <key>`, in seconds (an integer or a float).
///
/// # Panics
/// If the file cannot be read, or the key is missing or not a number. The message names the
/// key and the file.
pub fn budget(key: &str) -> f64 {
    budget_in(&file(), key)
}

fn read(path: &Path) -> toml::Table {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    text.parse()
        .unwrap_or_else(|e| panic!("{} is not TOML: {e}", path.display()))
}

fn lookup(path: &Path, table: &str, key: &str) -> toml::Value {
    read(path)
        .get(table)
        .and_then(|t| t.get(key))
        .cloned()
        .unwrap_or_else(|| panic!("{} has no [{table}] {key}", path.display()))
}

fn proptest_cases_in(path: &Path) -> u32 {
    let value = lookup(path, "proptest", "cases");
    value
        .as_integer()
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or_else(|| {
            panic!(
                "[proptest] cases in {} is not a count: {value}",
                path.display()
            )
        })
}

fn budget_in(path: &Path, key: &str) -> f64 {
    match lookup(path, "budgets", key) {
        toml::Value::Float(f) => f,
        // Budgets are small whole seconds, exact in an f64.
        toml::Value::Integer(n) => n as f64,
        other => panic!(
            "[budgets] {key} in {} is not a number: {other}",
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panic_text(f: impl FnOnce() + std::panic::UnwindSafe) -> String {
        let payload = std::panic::catch_unwind(f).expect_err("it did not panic");
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
            .unwrap_or_default()
    }

    #[test] // spec: QA-019
    fn params_read_proptest_cases_and_panic_on_a_missing_budget() {
        assert_eq!(proptest_cases(), 256);
        let text = panic_text(|| {
            budget("no_such_key");
        });
        assert!(text.contains("no_such_key"), "{text}");
        assert!(text.contains("test-params.toml"), "{text}");
        assert!(text.contains(&file().display().to_string()), "{text}");

        // Budgets read as seconds, integer or float.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-params.toml");
        std::fs::write(
            &path,
            "[proptest]\ncases = 64\n[budgets]\ncold_rebuild_s = 2.5\nwarm_reconcile_s = 3\n",
        )
        .unwrap();
        assert_eq!(proptest_cases_in(&path), 64);
        assert!((budget_in(&path, "cold_rebuild_s") - 2.5).abs() < f64::EPSILON);
        assert!((budget_in(&path, "warm_reconcile_s") - 3.0).abs() < f64::EPSILON);
        let text = panic_text(|| {
            budget_in(&path, "other");
        });
        assert!(text.contains("[budgets] other"), "{text}");
    }
}
