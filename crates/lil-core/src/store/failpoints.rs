//! Fail points (QA-066, QA-055): named places in the store (and, for `jobs.*`, in a job's run;
//! for `confirm.after_answer`, in a confirmation's answer) where a test makes the next file call
//! fail, or ends the process as a crash would.
//!
//! `LIL_TEST_FAILPOINTS=<name>=<action>[,…]` arms them, read only on test channels
//! ([`armed`]); a build whose `LIL_BUILD_CHANNEL` is `stable` compiles every check to a constant
//! `Ok` ([`FailPoints::hit`] tests a compile-time constant first).
//!
//! In-process tests can also run code at a point ([`FailPoints::on`]). At a point, the hooks
//! run first, then the armed action. An `Err` from either is returned by `hit`, and the store
//! treats it as the OS error of the file call that follows the point (so
//! `io::Error::from_raw_os_error(1177)` simulates a 1177).

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use lil_platform::{BUILD_CHANNEL, Channel, Env};

/// The variable that arms fail points on a test channel.
pub const ENV_FAILPOINTS: &str = "LIL_TEST_FAILPOINTS";

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
    "jobs.after_submitting",
    "jobs.after_post",
    "jobs.after_outputs",
    // After `Confirm::answer` commits the answer, before the `answered` event (Task 14).
    "confirm.after_answer",
];

/// The exit code of the `abort` action.
pub const ABORT_EXIT_CODE: u32 = 0xDEAD;

/// `a == b`, usable in a `const`.
const fn str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// False in a `stable` build: every [`FailPoints::hit`] is then a constant `Ok(())`.
const COMPILED_IN: bool = !str_eq(BUILD_CHANNEL, "stable");

/// What an armed fail point does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailAction {
    /// The process ends at once (`TerminateProcess`), as a crash would.
    Abort,
    /// Error 112, `ERROR_DISK_FULL`.
    DiskFull,
    /// Error 32, `ERROR_SHARING_VIOLATION` (retried by the store, then `store.busy`).
    Busy,
    /// Error 50, `ERROR_NOT_SUPPORTED` (`ReplaceFileW` unsupported: the two-rename fallback).
    Unsupported,
    /// An I/O error with no Windows code (`store.io`).
    Io,
}

impl FailAction {
    /// `abort`, `disk_full`, `busy`, `unsupported` or `io`.
    pub fn parse(s: &str) -> Option<FailAction> {
        Some(match s {
            "abort" => FailAction::Abort,
            "disk_full" => FailAction::DiskFull,
            "busy" => FailAction::Busy,
            "unsupported" => FailAction::Unsupported,
            "io" => FailAction::Io,
            _ => return None,
        })
    }

    /// The error the action stands for. `Abort` never returns.
    fn fire(self, name: &str) -> io::Error {
        match self {
            FailAction::Abort => {
                tracing::warn!(point = name, "fail point: abort");
                lil_platform::process::terminate_self(ABORT_EXIT_CODE)
            }
            FailAction::DiskFull => io::Error::from_raw_os_error(112),
            FailAction::Busy => io::Error::from_raw_os_error(32),
            FailAction::Unsupported => io::Error::from_raw_os_error(50),
            FailAction::Io => io::Error::other(format!("fail point {name}: io")),
        }
    }
}

type Hook = Arc<dyn Fn() -> io::Result<()> + Send + Sync>;

/// The armed fail points of one core, plus the hooks of in-process tests.
pub struct FailPoints {
    actions: HashMap<String, FailAction>,
    hooks: Mutex<HashMap<String, Vec<Hook>>>,
}

impl fmt::Debug for FailPoints {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hooked: Vec<String> = self
            .hooks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .cloned()
            .collect();
        f.debug_struct("FailPoints")
            .field("actions", &self.actions)
            .field("hooks", &hooked)
            .finish()
    }
}

impl FailPoints {
    fn empty() -> FailPoints {
        FailPoints {
            actions: HashMap::new(),
            hooks: Mutex::new(HashMap::new()),
        }
    }

    /// No point armed.
    pub fn none() -> Arc<FailPoints> {
        Arc::new(FailPoints::empty())
    }

    /// Parses `name=action[,…]`. An empty spec arms nothing. An unknown name or action is an
    /// error, so a typo never silently tests nothing.
    pub fn parse(spec: &str) -> Result<FailPoints, String> {
        let mut points = FailPoints::empty();
        for pair in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (name, action) = pair
                .split_once('=')
                .ok_or_else(|| format!("`{pair}` is not name=action"))?;
            let (name, action) = (name.trim(), action.trim());
            if !NAMES.contains(&name) {
                return Err(format!(
                    "`{name}` is not a fail point; the names are {NAMES:?}"
                ));
            }
            let action = FailAction::parse(action)
                .ok_or_else(|| format!("`{action}` is not a fail point action"))?;
            points.actions.insert(name.to_string(), action);
        }
        Ok(points)
    }

    /// The points `LIL_TEST_FAILPOINTS` arms, read through `var`, when [`armed`] holds for the
    /// env's channel and this build; otherwise (and for a spec that doesn't parse, which is
    /// logged) none.
    pub fn from_env(env: &Env, var: &dyn Fn(&str) -> Option<String>) -> Arc<FailPoints> {
        if !armed(env.channel(), BUILD_CHANNEL) {
            return FailPoints::none();
        }
        let Some(spec) = var(ENV_FAILPOINTS) else {
            return FailPoints::none();
        };
        match FailPoints::parse(&spec) {
            Ok(points) => {
                if !points.actions.is_empty() {
                    tracing::warn!(spec = %spec, "fail points armed");
                }
                Arc::new(points)
            }
            Err(why) => {
                tracing::error!(spec = %spec, %why, "LIL_TEST_FAILPOINTS ignored");
                FailPoints::none()
            }
        }
    }

    /// Runs the point `name`: its hooks in the order they were added, then its armed action.
    /// The first `Err` is returned; `Abort` ends the process.
    pub fn hit(&self, name: &str) -> io::Result<()> {
        if !COMPILED_IN {
            return Ok(());
        }
        let hooks: Vec<Hook> = self
            .hooks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .cloned()
            .unwrap_or_default();
        for hook in hooks {
            hook()?;
        }
        match self.actions.get(name) {
            Some(action) => Err(action.fire(name)),
            None => Ok(()),
        }
    }

    /// Adds a hook that runs at the point `name` (in-process tests).
    ///
    /// # Panics
    /// If `name` is not one of [`NAMES`].
    pub fn on(&self, name: &str, hook: Box<dyn Fn() -> io::Result<()> + Send + Sync>) {
        assert!(
            NAMES.contains(&name),
            "`{name}` is not a fail point; the names are {NAMES:?}"
        );
        self.hooks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(name.to_string())
            .or_default()
            .push(Arc::from(hook));
    }
}

/// Fail points are read only on a `test-*` channel of a build whose channel isn't `stable`.
pub fn armed(channel: &Channel, build_channel: &str) -> bool {
    matches!(channel, Channel::Test(_)) && build_channel != "stable"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// QA1 M1: the test kit's list (what the spec tests arm) and the core's (what `parse`
    /// accepts) agree: every core name is the test kit's, and the test kit's extra names are
    /// only the plan-3b points whose tasks add them here (Task 14 `confirm.*`, Task 15
    /// `meter.*`, Task 16 `caps.*`; once Task 16 lands, the lists are equal). A spec that names
    /// a point the core lacks arms nothing at all, so a drift would silently test nothing.
    #[test] // spec: QA-066
    fn the_core_and_test_kit_fail_point_lists_agree() {
        const LATER: &[&str] = &[
            "confirm.after_answer",
            "meter.after_pending",
            "meter.after_usage_line",
            "caps.after_reserve",
        ];
        let kit = lil_testkit::failpoints::NAMES;
        for name in NAMES {
            assert!(kit.contains(name), "{name} is not in the test kit's list");
        }
        for name in kit {
            assert!(
                NAMES.contains(name) || LATER.contains(name),
                "{name} is in the test kit's list but no task adds it to the core's"
            );
        }
    }

    #[test] // spec: QA-066
    fn parse_accepts_the_closed_lists_only() {
        let points = FailPoints::parse("store.replace=io, store.write_temp=disk_full").unwrap();
        assert_eq!(points.actions["store.replace"], FailAction::Io);
        assert_eq!(points.actions["store.write_temp"], FailAction::DiskFull);
        assert!(FailPoints::parse("").unwrap().actions.is_empty());
        for bad in [
            "store.replcae=io",
            "store.replace=crash",
            "store.replace",
            "=io",
        ] {
            assert!(FailPoints::parse(bad).is_err(), "{bad}");
        }
    }

    #[test] // spec: QA-066
    fn hooks_run_before_the_action_and_errors_carry_codes() {
        let points = FailPoints::parse("store.replace=unsupported").unwrap();
        let ran = Arc::new(Mutex::new(0));
        let seen = ran.clone();
        points.on(
            "store.replace",
            Box::new(move || {
                *seen.lock().unwrap() += 1;
                Ok(())
            }),
        );
        let err = points.hit("store.replace").unwrap_err();
        assert_eq!(err.raw_os_error(), Some(50));
        assert_eq!(*ran.lock().unwrap(), 1);
        assert!(points.hit("store.after_replace").is_ok());
        for (action, code) in [
            (FailAction::DiskFull, Some(112)),
            (FailAction::Busy, Some(32)),
            (FailAction::Unsupported, Some(50)),
            (FailAction::Io, None),
        ] {
            assert_eq!(action.fire("x").raw_os_error(), code, "{action:?}");
        }
        points.on(
            "store.put_back",
            Box::new(|| Err(io::Error::from_raw_os_error(1177))),
        );
        assert_eq!(
            points.hit("store.put_back").unwrap_err().raw_os_error(),
            Some(1177)
        );
    }

    #[test]
    fn str_eq_is_exact() {
        assert!(str_eq("stable", "stable"));
        assert!(!str_eq("stable", "stabl"));
        assert!(!str_eq("dev", "stable"));
    }
}
