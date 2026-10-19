//! `lil dev spec-report`: spec traceability (QA-028).
//!
//! Reads the spec's rule definitions, the `spec:` citations in the Rust sources under
//! `crates/` (a line comment such as `spec: A-001, B-002`) and `dev/rule-classification.toml`.
//! It prints how many rules there are per classification and how many a test cites, and it
//! fails when a citation names an ID that the spec does not define (a dangling citation).
//! Rules classified `test` that no test cites are listed for information only.

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The spec file the rule IDs come from, relative to the workspace root.
const SPEC_PATH: &str = "docs/superpowers/specs/2026-10-12-lilith-design.md";
/// The classification file, relative to the workspace root.
const CLASSIFICATION_PATH: &str = "dev/rule-classification.toml";
/// The classes a rule can have. `unclassified` is the default until a rule is classified.
const CLASSES: [&str; 5] = ["test", "lint", "manual", "n/a", "unclassified"];
/// ID prefixes of the spec's rule definitions.
const PREFIXES: [&str; 7] = ["ARCH", "CORE", "APP", "AI", "IMG", "QA", "OPS"];
/// What introduces a citation in a Rust source line. Built with `concat!` so this constant is
/// not itself a citation when this file is scanned.
const CITATION_MARKER: &str = concat!("// ", "spec:");

/// How each rule is checked: `default` for rules not listed, `rules` for the rest.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Classification {
    pub default: String,
    #[serde(default)]
    pub rules: BTreeMap<String, String>,
}

impl Classification {
    /// Parses `dev/rule-classification.toml`. Every class must be one of `CLASSES`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let c: Classification = toml::from_str(text).map_err(|e| e.to_string())?;
        let known = |v: &str| CLASSES.contains(&v);
        if !known(&c.default) {
            return Err(format!("default: unknown class `{}`", c.default));
        }
        if let Some((id, v)) = c.rules.iter().find(|(_, v)| !known(v)) {
            return Err(format!("{id}: unknown class `{v}`"));
        }
        Ok(c)
    }

    fn class_of(&self, id: &str) -> &str {
        self.rules.get(id).unwrap_or(&self.default)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecReport {
    /// `(rule id, "path:line")` for every citation of an ID the spec does not define.
    pub dangling: Vec<(String, String)>,
    /// Rules classified `test` that no test cites.
    pub test_rules_without_tests: Vec<String>,
    /// Rules per class, with a (possibly zero) entry for every class.
    pub counts: BTreeMap<String, usize>,
}

/// The tagged rule definitions in the spec: lines that start `- **<ID>** [`. Untagged repeats
/// of an ID (the appendix list) don't count. Same definition as the project's ID check.
pub fn spec_rule_ids(spec_markdown: &str) -> BTreeSet<String> {
    spec_markdown.lines().filter_map(definition_id).collect()
}

fn definition_id(line: &str) -> Option<String> {
    let rest = line.strip_prefix("- **")?;
    let (prefix, rest) = rest.split_once('-')?;
    if !PREFIXES.contains(&prefix) {
        return None;
    }
    let digits = rest.get(..3)?;
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    rest.get(3..)?
        .starts_with("** [")
        .then(|| format!("{prefix}-{digits}"))
}

/// Every citation in the sources, as `ID -> ["path:line", ...]` in source order. The IDs are
/// comma-separated and whitespace is ignored; the list ends at the first entry that doesn't
/// start with an ID, so trailing prose is fine.
pub fn cited_ids(rust_sources: &[(String, String)]) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (path, text) in rust_sources {
        for (n, line) in text.lines().enumerate() {
            let Some(at) = line.find(CITATION_MARKER) else {
                continue;
            };
            for entry in line[at + CITATION_MARKER.len()..].split(',') {
                let Some(token) = entry.split_whitespace().next() else {
                    break;
                };
                if !looks_like_id(token) {
                    break;
                }
                out.entry(token.to_string())
                    .or_default()
                    .push(format!("{path}:{}", n + 1));
            }
        }
    }
    out
}

/// Letters, a dash, digits: `QA-028`, `A-001`.
fn looks_like_id(token: &str) -> bool {
    match token.split_once('-') {
        Some((letters, digits)) => {
            !letters.is_empty()
                && letters.bytes().all(|b| b.is_ascii_alphabetic())
                && !digits.is_empty()
                && digits.bytes().all(|b| b.is_ascii_digit())
        }
        None => false,
    }
}

pub fn report(
    ids: &BTreeSet<String>,
    cited: &BTreeMap<String, Vec<String>>,
    classification: &Classification,
) -> SpecReport {
    let dangling = cited
        .iter()
        .filter(|(id, _)| !ids.contains(*id))
        .flat_map(|(id, places)| places.iter().map(|p| (id.clone(), p.clone())))
        .collect();
    let mut counts: BTreeMap<String, usize> = CLASSES.iter().map(|c| (c.to_string(), 0)).collect();
    let mut test_rules_without_tests = Vec::new();
    for id in ids {
        let class = classification.class_of(id);
        *counts.entry(class.to_string()).or_default() += 1;
        if class == "test" && !cited.contains_key(id) {
            test_rules_without_tests.push(id.clone());
        }
    }
    SpecReport {
        dangling,
        test_rules_without_tests,
        counts,
    }
}

/// Tracked `.rs` files under `crates/` (never `target/`), read from the working tree.
/// Tracked files deleted from the working tree are skipped.
fn rust_sources(root: &Path) -> Result<Vec<(String, String)>, String> {
    let out = super::root::git_command()
        .args(["ls-files", "-z", "--", "crates"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let listing = String::from_utf8_lossy(&out.stdout);
    let mut sources = Vec::new();
    for path in listing.split('\0').filter(|p| p.ends_with(".rs")) {
        if path.split('/').any(|part| part == "target") {
            continue;
        }
        match std::fs::read_to_string(root.join(path)) {
            Ok(text) => sources.push((path.to_string(), text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot read {path}: {e}")),
        }
    }
    Ok(sources)
}

/// Prints the report. Exit 1 when a citation is dangling, 2 when the report can't be made,
/// otherwise 0.
pub fn run() -> i32 {
    let root = match super::root::source_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("spec report: {e}");
            return 2;
        }
    };
    let read = |rel: &str| {
        std::fs::read_to_string(root.join(rel)).map_err(|e| format!("cannot read {rel}: {e}"))
    };
    let inputs = read(SPEC_PATH).and_then(|spec| {
        let classification = read(CLASSIFICATION_PATH).and_then(|t| {
            Classification::parse(&t).map_err(|e| format!("{CLASSIFICATION_PATH}: {e}"))
        })?;
        Ok((spec, classification, rust_sources(&root)?))
    });
    let (spec, classification, sources) = match inputs {
        Ok(v) => v,
        Err(e) => {
            eprintln!("spec report: {e}");
            return 2;
        }
    };

    let ids = spec_rule_ids(&spec);
    let cited = cited_ids(&sources);
    let r = report(&ids, &cited, &classification);

    println!("spec report: {} rules in the spec", ids.len());
    let counts: Vec<String> = r.counts.iter().map(|(c, n)| format!("{c} {n}")).collect();
    println!("classification: {}", counts.join(", "));
    let cited_rules = cited.keys().filter(|id| ids.contains(*id)).count();
    println!("cited by tests: {cited_rules} rules");
    for id in classification.rules.keys().filter(|id| !ids.contains(*id)) {
        eprintln!("warning: {CLASSIFICATION_PATH} classifies {id}, which the spec does not define");
    }
    println!(
        "test rules without a citing test: {}",
        r.test_rules_without_tests.len()
    );
    for id in &r.test_rules_without_tests {
        println!("  {id}");
    }
    println!("dangling citations: {}", r.dangling.len());
    for (id, place) in &r.dangling {
        eprintln!("dangling citation: {id} at {place} (the spec defines no such rule)");
    }
    i32::from(!r.dangling.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The number of tagged rule definitions in the spec. Update this one number when the
    /// spec gains or loses rules (the project's ID check prints the same count).
    const EXPECTED_RULE_COUNT: usize = 1014;

    /// The citation marker, built in two pieces so these fixtures are not themselves
    /// citations when `lil dev spec-report` scans this file.
    fn marker() -> String {
        format!("// {}:", "spec")
    }

    fn src(path: &str, text: &str) -> (String, String) {
        (path.to_string(), text.to_string())
    }

    fn ids(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn class(default: &str, rules: &[(&str, &str)]) -> Classification {
        Classification {
            default: default.to_string(),
            rules: rules
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test] // spec: QA-028
    fn extracts_all_ids_from_spec() {
        let path = crate::dev::spec_tests::workspace_root().join(SPEC_PATH);
        let text = std::fs::read_to_string(&path).expect("the spec file exists");
        assert_eq!(spec_rule_ids(&text).len(), EXPECTED_RULE_COUNT);
    }

    #[test] // spec: QA-028
    fn spec_rule_ids_counts_tagged_definitions_only() {
        let md = [
            "- **QA-028** [M1] (contract) defined",
            "- **ARCH-114** The appendix repeat has no tag",
            "  - **QA-029** [M1] indented, not a definition",
            "- **XYZ-001** [M1] unknown prefix",
            "- **QA-0301** [M1] four digits",
            "- **OPS-007** [M2] (lint) defined\r",
            "- **QA-028** [M1] defined twice",
            "text - **AI-001** [M1] not at line start",
        ]
        .join("\n");
        assert_eq!(spec_rule_ids(&md), ids(&["OPS-007", "QA-028"]));
    }

    #[test] // spec: QA-028
    fn parses_multi_id_citations() {
        let m = marker();
        let sources = [
            src(
                "crates/a/src/x.rs",
                &format!("fn f() {{}}\n#[test] {m} A-001, B-002\nfn g() {{}}\n"),
            ),
            src(
                "crates/b/src/y.rs",
                &format!("  {m}A-001 ,  QA-123,ARCH-009  \n{m} QA-5 because of the thing\n"),
            ),
            src("crates/c/src/z.rs", "// nothing to see\nlet x = 1;\n"),
        ];
        let cited = cited_ids(&sources);
        let at = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            cited.get("A-001"),
            Some(&at(&["crates/a/src/x.rs:2", "crates/b/src/y.rs:1"]))
        );
        assert_eq!(cited.get("B-002"), Some(&at(&["crates/a/src/x.rs:2"])));
        assert_eq!(cited.get("QA-123"), Some(&at(&["crates/b/src/y.rs:1"])));
        assert_eq!(cited.get("ARCH-009"), Some(&at(&["crates/b/src/y.rs:1"])));
        // Trailing prose after an ID ends the list; the ID before it still counts.
        assert_eq!(cited.get("QA-5"), Some(&at(&["crates/b/src/y.rs:2"])));
        assert_eq!(cited.len(), 5);
    }

    #[test] // spec: QA-028
    fn dangling_citation_reported() {
        let cited = BTreeMap::from([
            ("QA-001".to_string(), vec!["crates/a.rs:3".to_string()]),
            (
                "QA-999".to_string(),
                vec!["crates/a.rs:9".to_string(), "crates/b.rs:1".to_string()],
            ),
        ]);
        let r = report(
            &ids(&["QA-001", "QA-002"]),
            &cited,
            &class("unclassified", &[]),
        );
        assert_eq!(
            r.dangling,
            vec![
                ("QA-999".to_string(), "crates/a.rs:9".to_string()),
                ("QA-999".to_string(), "crates/b.rs:1".to_string()),
            ]
        );
    }

    #[test] // spec: QA-028
    fn test_classified_rule_without_test_listed() {
        let cited = BTreeMap::from([("QA-001".to_string(), vec!["crates/a.rs:3".to_string()])]);
        let c = class(
            "unclassified",
            &[
                ("QA-001", "test"),
                ("QA-002", "test"),
                ("QA-003", "lint"),
                ("QA-004", "n/a"),
            ],
        );
        let r = report(
            &ids(&["QA-001", "QA-002", "QA-003", "QA-004", "QA-005"]),
            &cited,
            &c,
        );
        assert_eq!(r.test_rules_without_tests, vec!["QA-002".to_string()]);
        assert!(r.dangling.is_empty());
        assert_eq!(r.counts["test"], 2);
        assert_eq!(r.counts["lint"], 1);
        assert_eq!(r.counts["n/a"], 1);
        assert_eq!(r.counts["unclassified"], 1);
        assert_eq!(r.counts["manual"], 0);
    }

    #[test] // spec: QA-028
    fn classification_file_parses_and_validates() {
        let text = std::fs::read_to_string(
            crate::dev::spec_tests::workspace_root().join(CLASSIFICATION_PATH),
        )
        .expect("the classification file exists");
        let c = Classification::parse(&text).expect("the shipped file is valid");
        assert_eq!(c.default, "unclassified");

        let ok = Classification::parse("default = \"unclassified\"\n[rules]\nQA-001 = \"n/a\"\n");
        assert_eq!(ok.unwrap().class_of("QA-001"), "n/a");
        assert!(Classification::parse("default = \"nope\"").is_err());
        assert!(Classification::parse("default = \"test\"\n[rules]\nQA-001 = \"maybe\"").is_err());
        assert!(Classification::parse("default = \"test\"\nextra = 1").is_err());
        assert!(Classification::parse("[rules]\n").is_err());
    }
}
