//! Fail points (QA-066, QA-055): the variable a test sets on a test-channel core, and its
//! value.
//!
//! `WW_TEST_FAILPOINTS=<name>=<action>[,…]`. A core reads it only on test channels, and builds
//! whose `WW_BUILD_CHANNEL` is `stable` compile the checks out. Actions: `abort`, `disk_full`,
//! `busy`, `unsupported` and `io` (Global Constraints, "Fail points").

/// The variable that arms fail points in a test-channel core.
pub const ENV: &str = "WW_TEST_FAILPOINTS";

/// The value of [`ENV`] for `points`: `name=action` pairs joined by `,`, in the given order.
///
/// # Panics
/// If a name or action is empty or contains `=`, `,` or whitespace, which the format cannot
/// carry.
pub fn spec(points: &[(&str, &str)]) -> String {
    let check = |what: &str, s: &str| {
        assert!(
            !s.is_empty() && !s.contains(['=', ',']) && !s.contains(char::is_whitespace),
            "fail point {what} {s:?} cannot be written in {ENV}"
        );
    };
    points
        .iter()
        .map(|(name, action)| {
            check("name", name);
            check("action", action);
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
        for bad in [("a=b", "abort"), ("store.replace", "a,b"), ("", "io")] {
            assert!(
                std::panic::catch_unwind(|| spec(&[bad])).is_err(),
                "{bad:?} was accepted"
            );
        }
    }
}
