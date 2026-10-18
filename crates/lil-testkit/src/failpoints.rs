//! Fail points (QA-066, QA-055): the variable a test sets on a test-channel core, and its
//! value.
//!
//! `WW_TEST_FAILPOINTS=<name>=<action>[,…]`. A core reads it only on test channels, and builds
//! whose `WW_BUILD_CHANNEL` is `stable` compile the checks out. Actions: `abort`, `disk_full`,
//! `busy`, `unsupported` and `io` (Global Constraints, "Fail points").

/// The variable that arms fail points in a test-channel core.
pub const ENV: &str = "WW_TEST_FAILPOINTS";

/// Every fail point name (Global Constraints, "Fail points"), a closed list.
pub const NAMES: &[&str] = &[
    "store.write_temp",
    "store.replace",
    "store.after_replace",
    "store.between_renames",
    "store.put_back",
    "store.suite_toml",
    "store.after_output",
    "store.after_manifest",
    "store.after_jobs_log",
    "store.after_progress",
    "store.after_index",
    "store.after_seal",
    "watch.start",
];

/// Every fail point action, a closed list.
pub const ACTIONS: &[&str] = &["abort", "disk_full", "busy", "unsupported", "io"];

/// The value of [`ENV`] for `points`: `name=action` pairs joined by `,`, in the given order.
///
/// # Panics
/// If a name is not in [`NAMES`] or an action not in [`ACTIONS`]: a core ignores an unknown
/// fail point, so a typo would otherwise make the test silently test nothing.
pub fn spec(points: &[(&str, &str)]) -> String {
    points
        .iter()
        .map(|(name, action)| {
            assert!(
                NAMES.contains(name),
                "unknown fail point name {name:?}; the names are {NAMES:?}"
            );
            assert!(
                ACTIONS.contains(action),
                "unknown fail point action {action:?} for {name}; the actions are {ACTIONS:?}"
            );
            format!("{name}={action}")
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: QA-066
    fn failpoint_spec_formats_pairs() {
        assert_eq!(ENV, "WW_TEST_FAILPOINTS");
        assert_eq!(spec(&[]), "");
        assert_eq!(spec(&[("store.replace", "abort")]), "store.replace=abort");
        assert_eq!(
            spec(&[
                ("store.between_renames", "abort"),
                ("watch.start", "unsupported"),
                ("store.write_temp", "disk_full"),
            ]),
            "store.between_renames=abort,watch.start=unsupported,store.write_temp=disk_full"
        );
        // Every name and action of the closed lists is accepted.
        assert_eq!((NAMES.len(), ACTIONS.len()), (13, 5));
        for name in NAMES {
            for action in ACTIONS {
                assert_eq!(spec(&[(name, action)]), format!("{name}={action}"));
            }
        }
        // A typo panics instead of arming nothing.
        for bad in [
            ("store.replcae", "abort"),
            ("store.replace", "crash"),
            ("Store.replace", "abort"),
            ("store.replace", "Abort"),
            ("a=b", "abort"),
            ("store.replace", "a,b"),
            ("", "io"),
        ] {
            assert!(
                std::panic::catch_unwind(|| spec(&[bad])).is_err(),
                "{bad:?} was accepted"
            );
        }
    }
}
