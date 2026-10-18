//! `lil dev spec-tests`: runs the spec tests under nextest and applies the expected-fail list.
//!
//! Test names are qualified (`<classname>::<name>`, e.g.
//! `lil-spec-tests::smoke::spec_harness_runs`) in the JUnit parse, in the expected-fail
//! list and in every message, so same-named tests in different binaries cannot mask each other.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JunitCase {
    pub name: String,
    pub failed: bool,
    pub skipped: bool,
    /// Passed, but only after a retry (`<flakyFailure>` / `<flakyError>` children).
    pub flaky: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedFail {
    pub name: String,
    pub rule: String,
    pub file: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecVerdict {
    Pass,
    UnexpectedFailures(Vec<String>),
    /// Listed entries that match no test case in the run.
    StaleEntries(Vec<String>),
    NowPassing(Vec<String>),
}

/// The JUnit classname prefix nextest gives a setup script's entry (ruling P18).
const SETUP_SCRIPT_CLASS: &str = "@setup-script:";

/// Reads every `<testcase>` from a JUnit document. A case is failed when it has a
/// `<failure>` or `<error>` child; flaky-retry elements mark it flaky, not failed.
/// A setup script's entry (classname `@setup-script:<name>`) is not a test and is left out,
/// unless it failed: then it is kept as a failed case, so the run names it.
/// Malformed XML or a document with no test cases is an error.
pub fn parse_junit(xml: &str) -> Result<Vec<JunitCase>, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("malformed JUnit XML: {e}"))?;
    let cases: Vec<JunitCase> = doc
        .descendants()
        .filter(|n| n.has_tag_name("testcase"))
        .map(|n| {
            let has = |tags: &[&str]| {
                n.children()
                    .any(|c| tags.iter().any(|t| c.has_tag_name(*t)))
            };
            let name = n.attribute("name").unwrap_or_default();
            let name = match n.attribute("classname") {
                Some(class) if !class.is_empty() => format!("{class}::{name}"),
                _ => name.to_string(),
            };
            JunitCase {
                name,
                failed: has(&["failure", "error"]),
                skipped: has(&["skipped"]),
                flaky: has(&["flakyFailure", "flakyError"]),
            }
        })
        .filter(|c| c.failed || !c.name.starts_with(SETUP_SCRIPT_CLASS))
        .collect();
    if cases.is_empty() {
        return Err("JUnit output contains no test cases".to_string());
    }
    Ok(cases)
}

pub fn evaluate(cases: &[JunitCase], expected: &[ExpectedFail]) -> SpecVerdict {
    let is_expected = |name: &str| expected.iter().any(|e| e.name == name);
    let unexpected: Vec<String> = cases
        .iter()
        .filter(|c| c.failed && !is_expected(&c.name))
        .map(|c| c.name.clone())
        .collect();
    if !unexpected.is_empty() {
        return SpecVerdict::UnexpectedFailures(unexpected);
    }
    let stale: Vec<String> = expected
        .iter()
        .filter(|e| !cases.iter().any(|c| c.name == e.name))
        .map(|e| e.name.clone())
        .collect();
    if !stale.is_empty() {
        return SpecVerdict::StaleEntries(stale);
    }
    let now_passing: Vec<String> = expected
        .iter()
        .filter(|e| {
            cases
                .iter()
                .any(|c| c.name == e.name && !c.failed && !c.skipped)
        })
        .map(|e| e.name.clone())
        .collect();
    if !now_passing.is_empty() {
        return SpecVerdict::NowPassing(now_passing);
    }
    SpecVerdict::Pass
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExpectedFile {
    #[serde(default)]
    pub(crate) tests: Vec<ExpectedFail>,
}

/// The checkout this binary was built from. Tests read source files through it; commands use
/// [`super::root::repo_root`] or [`super::root::source_root`], which refuse a mismatch.
#[cfg(test)]
pub(crate) fn workspace_root() -> PathBuf {
    super::root::built_root()
}

/// nextest's own default for `store.dir` (relative to the workspace root).
const DEFAULT_STORE_DIR: &str = "target/nextest";

#[derive(Deserialize)]
struct NextestConfig {
    #[serde(default)]
    store: Option<NextestStore>,
    #[serde(default)]
    profile: std::collections::BTreeMap<String, NextestProfile>,
}

#[derive(Deserialize)]
struct NextestStore {
    dir: Option<PathBuf>,
}

#[derive(Deserialize)]
struct NextestProfile {
    junit: Option<NextestJunit>,
}

#[derive(Deserialize)]
struct NextestJunit {
    path: Option<PathBuf>,
}

/// Where nextest writes the `spec` profile's JUnit file, read from the workspace's nextest
/// config (`nextest_config`, the text of `.config/nextest.toml`) the way nextest resolves it:
/// `<workspace>/<store.dir>/spec/<profile.spec.junit.path>`, with nextest's default store
/// `target/nextest`. nextest keeps its store there even when `CARGO_TARGET_DIR` points
/// elsewhere, so the target directory plays no part (R38).
pub(crate) fn junit_path(root: &Path, nextest_config: &str) -> Result<PathBuf, String> {
    let cfg: NextestConfig =
        toml::from_str(nextest_config).map_err(|e| format!("{NEXTEST_CONFIG} is unusable: {e}"))?;
    let store = cfg
        .store
        .and_then(|s| s.dir)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_STORE_DIR));
    let junit = cfg
        .profile
        .get("spec")
        .and_then(|p| p.junit.as_ref())
        .and_then(|j| j.path.clone())
        .ok_or_else(|| {
            format!("{NEXTEST_CONFIG} sets no [profile.spec.junit] path, so there is no JUnit output to read")
        })?;
    Ok(root.join(store).join("spec").join(junit))
}

/// The arguments for the nextest run. `--ignore-default-filter` makes nextest disregard any
/// `default-filter` in `.config/nextest.toml`, so a config edit cannot exclude a failing spec test.
pub(crate) const NEXTEST_CONFIG: &str = ".config/nextest.toml";

pub(crate) fn nextest_args() -> Vec<String> {
    [
        "nextest",
        "run",
        "-p",
        "lil-spec-tests",
        "--profile",
        "spec",
        "--ignore-default-filter",
    ]
    .map(String::from)
    .to_vec()
}

pub fn run() -> i32 {
    let root = match super::root::source_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("spec tests: {e}");
            return 1;
        }
    };
    let expected_path = root.join("crates/lil-spec-tests/expected-fail.toml");
    let expected: Vec<ExpectedFail> = match std::fs::read_to_string(&expected_path)
        .map_err(|e| e.to_string())
        .and_then(|s| toml::from_str::<ExpectedFile>(&s).map_err(|e| e.to_string()))
    {
        Ok(f) => f.tests,
        Err(e) => {
            eprintln!("spec tests: cannot read {}: {e}", expected_path.display());
            return 1;
        }
    };

    // Interactive output is inherited, not captured. A nonzero exit is data: the
    // JUnit file decides the verdict.
    let junit_path = match std::fs::read_to_string(root.join(NEXTEST_CONFIG))
        .map_err(|e| format!("cannot read {NEXTEST_CONFIG}: {e}"))
        .and_then(|text| junit_path(&root, &text))
    {
        Ok(p) => p,
        Err(e) => {
            eprintln!("spec tests: {e}");
            return 1;
        }
    };
    let _ = std::fs::remove_file(&junit_path);
    let status = match super::root::scrub_git_env(&mut Command::new("cargo"))
        .args(nextest_args())
        .current_dir(&root)
        .status()
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("spec tests: could not start cargo nextest: {e}");
            return 1;
        }
    };

    let xml = match std::fs::read_to_string(&junit_path) {
        Ok(x) => x,
        Err(e) => {
            eprintln!(
                "spec tests: no JUnit output at {}: {e}",
                junit_path.display()
            );
            return 1;
        }
    };
    let cases = match parse_junit(&xml) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("spec tests: {e} ({})", junit_path.display());
            return 1;
        }
    };

    for c in cases.iter().filter(|c| c.flaky && !c.failed) {
        eprintln!("flaky: {} (passed on retry)", c.name);
    }

    match evaluate(&cases, &expected) {
        SpecVerdict::Pass => {
            let failed = cases.iter().filter(|c| c.failed).count();
            let skipped = cases.iter().filter(|c| c.skipped).count();
            if !status.success() && failed == 0 {
                eprintln!("spec tests: nextest failed without reporting a test failure");
                return 1;
            }
            let passed = cases.len() - failed - skipped;
            if skipped > 0 {
                println!("spec tests: {passed} passed, {failed} expected-fail, {skipped} skipped");
            } else {
                println!("spec tests: {passed} passed, {failed} expected-fail");
            }
            0
        }
        SpecVerdict::UnexpectedFailures(names) => {
            eprintln!("spec tests: unexpected failures: {}", names.join(", "));
            1
        }
        SpecVerdict::StaleEntries(names) => {
            for n in names {
                eprintln!("stale expected-fail entry: {n}");
            }
            1
        }
        SpecVerdict::NowPassing(names) => {
            eprintln!(
                "spec tests: now passing, remove from expected-fail.toml: {}",
                names.join(", ")
            );
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(name: &str, failed: bool) -> JunitCase {
        JunitCase {
            name: name.into(),
            failed,
            skipped: false,
            flaky: false,
        }
    }

    fn exp(name: &str) -> ExpectedFail {
        ExpectedFail {
            name: name.into(),
            rule: "QA-017".into(),
            file: "tests/x.rs".into(),
        }
    }

    #[test]
    fn nextest_args_ignore_the_default_filter() {
        let args = nextest_args();
        assert!(
            args.iter().any(|a| a == "--ignore-default-filter"),
            "{args:?}"
        );
        // The run is still the spec profile of the spec crate.
        assert!(
            args.windows(2).any(|w| w == ["--profile", "spec"]),
            "{args:?}"
        );
        assert!(
            args.windows(2).any(|w| w == ["-p", "lil-spec-tests"]),
            "{args:?}"
        );
    }

    /// R38: nextest keeps its store (and the JUnit file) under `store.dir`, relative to the
    /// workspace root, whatever `CARGO_TARGET_DIR` says.
    #[test]
    fn junit_path_follows_the_nextest_store_not_the_target_dir() {
        let root = Path::new("ws");
        let spec_junit = "[profile.spec.junit]
path = \"junit.xml\"
";
        // nextest's default store is target/nextest under the workspace root.
        assert_eq!(
            junit_path(root, spec_junit).unwrap(),
            root.join("target/nextest").join("spec").join("junit.xml")
        );
        let custom = format!(
            "[store]
dir = \"elsewhere/store\"
{spec_junit}"
        );
        assert_eq!(
            junit_path(root, &custom).unwrap(),
            root.join("elsewhere/store").join("spec").join("junit.xml")
        );
        let named = "[profile.spec.junit]
path = \"out/spec.xml\"
";
        assert_eq!(
            junit_path(root, named).unwrap(),
            root.join("target/nextest")
                .join("spec")
                .join("out/spec.xml")
        );
        // No JUnit output configured for the spec profile: the runner can't work.
        assert!(
            junit_path(
                root,
                "[profile.spec]
fail-fast = false
"
            )
            .is_err()
        );
        assert!(junit_path(root, "not toml [").is_err());
    }

    /// The committed config: the JUnit file is under the repository's own target/nextest.
    #[test]
    fn committed_nextest_config_gives_the_workspace_store() {
        let root = workspace_root();
        let text = std::fs::read_to_string(root.join(NEXTEST_CONFIG)).unwrap();
        assert_eq!(
            junit_path(&root, &text).unwrap(),
            root.join("target/nextest").join("spec").join("junit.xml")
        );
    }

    /// QA-030 and R38: one retry everywhere, and the slow lil-cli tests (real git repositories,
    /// up to about a minute each) get a 5-minute limit instead of the default 2 minutes.
    #[test]
    fn nextest_config_keeps_one_retry_and_gives_lil_cli_five_minutes() {
        let text = std::fs::read_to_string(workspace_root().join(NEXTEST_CONFIG)).unwrap();
        let cfg: toml::Value = toml::from_str(&text).unwrap();
        let default = &cfg["profile"]["default"];
        assert_eq!(default["retries"].as_integer(), Some(1));
        let seconds = |v: &toml::Value| -> i64 {
            let period = v["period"].as_str().unwrap();
            let period: i64 = period.strip_suffix('s').unwrap().parse().unwrap();
            period * v["terminate-after"].as_integer().unwrap()
        };
        assert_eq!(seconds(&default["slow-timeout"]), 120);
        let overrides = default
            .get("overrides")
            .and_then(toml::Value::as_array)
            .expect("[[profile.default.overrides]]");
        let lil_cli: Vec<&toml::Value> = overrides
            .iter()
            .filter(|o| o["filter"].as_str() == Some("package(lil-cli)"))
            .collect();
        assert_eq!(lil_cli.len(), 1, "{overrides:?}");
        assert_eq!(seconds(&lil_cli[0]["slow-timeout"]), 300);
        // An override never changes the retry count.
        assert!(overrides.iter().all(|o| o.get("retries").is_none()));
    }

    #[test]
    fn unlisted_failure_is_unexpected() {
        let v = evaluate(&[case("a", false), case("b", true)], &[]);
        assert_eq!(v, SpecVerdict::UnexpectedFailures(vec!["b".into()]));
    }

    #[test]
    fn listed_failure_is_tolerated() {
        let v = evaluate(&[case("a", false), case("b", true)], &[exp("b")]);
        assert_eq!(v, SpecVerdict::Pass);
    }

    #[test]
    fn listed_but_passing_is_reported() {
        let v = evaluate(&[case("a", false), case("b", false)], &[exp("b")]);
        assert_eq!(v, SpecVerdict::NowPassing(vec!["b".into()]));
    }

    #[test]
    fn parse_junit_reads_failures() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="2" failures="1">
  <testsuite name="lil-spec-tests::smoke" tests="2" failures="1">
    <testcase name="ok_one" classname="lil-spec-tests::smoke" time="0.01"/>
    <testcase name="bad_one" classname="lil-spec-tests::smoke" time="0.01">
      <failure message="boom">details</failure>
    </testcase>
  </testsuite>
</testsuites>"#;
        assert_eq!(
            parse_junit(xml).unwrap(),
            vec![
                case("lil-spec-tests::smoke::ok_one", false),
                case("lil-spec-tests::smoke::bad_one", true)
            ]
        );
    }

    #[test]
    fn setup_scripts_are_not_counted_as_tests() {
        let script = |body: &str| {
            format!(
                r#"<testsuites>
  <testsuite name="@setup-script:build-testbins" tests="1">
    <testcase name="build-testbins" classname="@setup-script:build-testbins">{body}</testcase>
  </testsuite>
  <testsuite name="lil-spec-tests::smoke" tests="1">
    <testcase name="ok_one" classname="lil-spec-tests::smoke"/>
  </testsuite>
</testsuites>"#
            )
        };
        assert_eq!(
            parse_junit(&script("")).unwrap(),
            vec![case("lil-spec-tests::smoke::ok_one", false)]
        );
        // A failed setup script stays, as a failure the run reports.
        let failed = parse_junit(&script(r#"<failure message="build failed"/>"#)).unwrap();
        assert_eq!(
            failed,
            vec![
                case("@setup-script:build-testbins::build-testbins", true),
                case("lil-spec-tests::smoke::ok_one", false)
            ]
        );
        assert_eq!(
            evaluate(&failed, &[]),
            SpecVerdict::UnexpectedFailures(vec![
                "@setup-script:build-testbins::build-testbins".into()
            ])
        );
        // A run with nothing but a passing setup script has no test cases.
        let only = r#"<testsuites><testsuite name="@setup-script:b">
    <testcase name="b" classname="@setup-script:b"/></testsuite></testsuites>"#;
        assert!(parse_junit(only).is_err());
    }

    #[test]
    fn malformed_junit_fails() {
        assert!(parse_junit("<testsuites><testcase name=").is_err());
    }

    #[test]
    fn empty_junit_fails() {
        let xml = r#"<?xml version="1.0"?><testsuites name="nextest-run" tests="0"></testsuites>"#;
        assert!(parse_junit(xml).is_err());
    }

    #[test]
    fn flaky_cases_are_reported_not_failed() {
        let xml = r#"<testsuites><testsuite name="s">
    <testcase name="wobbly" classname="b::s">
      <flakyFailure message="first try"/>
    </testcase>
  </testsuite></testsuites>"#;
        let cases = parse_junit(xml).unwrap();
        assert_eq!(cases.len(), 1);
        assert!(cases[0].flaky);
        assert!(!cases[0].failed);
        assert_eq!(evaluate(&cases, &[]), SpecVerdict::Pass);
    }

    #[test]
    fn same_name_in_different_binaries_does_not_mask() {
        let cases = [case("bin_a::t::same", true), case("bin_b::t::same", true)];
        let v = evaluate(&cases, &[exp("bin_a::t::same")]);
        assert_eq!(
            v,
            SpecVerdict::UnexpectedFailures(vec!["bin_b::t::same".into()])
        );
    }

    #[test]
    fn stale_entry_is_reported() {
        let v = evaluate(&[case("a", false)], &[exp("gone")]);
        assert_eq!(v, SpecVerdict::StaleEntries(vec!["gone".into()]));
    }

    #[test]
    fn skipped_listed_test_is_not_now_passing() {
        let skipped = JunitCase {
            skipped: true,
            ..case("s", false)
        };
        assert_eq!(evaluate(&[skipped], &[exp("s")]), SpecVerdict::Pass);
    }
}
