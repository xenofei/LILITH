//! `lil dev spec-report`: spec traceability (QA-028).
//!
//! Reads the spec's rule definitions, the `spec:` citations in the Rust sources under
//! `crates/` (a line comment such as `spec: A-001, B-002`) and `dev/rule-classification.toml`.
//! It prints how many rules there are per classification and how many a test cites, and it
//! fails when a citation names an ID that the spec does not define (a dangling citation).
//! Rules classified `test` that no test cites are listed for information only.
//!
//! It also fails when a tracked Markdown file under `docs/` or at the root cites a ruling id
//! from a private ledger (`P3-R9`, `ruling Q32`, `ledger ruling R-T`) that no such file
//! records as a bullet starting `- **<id>**` (QA-090), so a reason never lives only in a
//! private ledger.

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
/// The Markdown files whose ruling citations are checked (QA-090): `docs/` and the root.
const PUBLIC_MARKDOWN: &[&str] = &["docs", ":(glob)*.md"];
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

/// A ruling id at the start of `s`: `R-T`, or `Q`/`R` then digits and an optional lowercase
/// suffix (`Q32`, `R16b`). Returns the id's length.
fn ledger_id_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let ends_at = |len: usize| (!b.get(len).is_some_and(u8::is_ascii_alphanumeric)).then_some(len);
    if s.starts_with("R-T") {
        return ends_at(3);
    }
    if !matches!(b.first(), Some(b'Q' | b'R')) {
        return None;
    }
    let digits = b[1..].iter().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let len = 1 + digits;
    ends_at(
        len + b[len..]
            .iter()
            .take_while(|c| c.is_ascii_lowercase())
            .count(),
    )
}

/// A plan-numbered ruling id at the start of `s`: `P<digits>-R<digits>`, each number with an
/// optional lowercase suffix (`P3-R16b`). Returns the id's length.
fn plan_ruling_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let count = |from: usize, f: fn(&u8) -> bool| b[from..].iter().take_while(|c| f(c)).count();
    if b.first() != Some(&b'P') {
        return None;
    }
    let mut at = 1;
    for (i, sep) in ["", "-R"].into_iter().enumerate() {
        if !s[at..].starts_with(sep) {
            return None;
        }
        at += sep.len();
        let digits = count(at, u8::is_ascii_digit);
        if digits == 0 {
            return None;
        }
        at += digits;
        let suffix = count(at, u8::is_ascii_lowercase);
        at += if i == 0 { suffix.min(1) } else { suffix };
    }
    (!b.get(at).is_some_and(u8::is_ascii_alphanumeric)).then_some(at)
}

/// The ruling ids a line cites: every plan-numbered id (`P3-R9`), and the `Q`/`R` ids listed
/// right after the word "ruling", "rulings" or "ledger" (`rulings Q31, Q34`, `ledger ruling
/// R-T`, `rulings R15–R17`). A bare `R2` is not counted: it may be a product name.
pub fn ruling_citations(line: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let bytes = line.as_bytes();
    let word_starts = line
        .char_indices()
        .map(|(i, _)| i)
        .filter(|&i| i == 0 || !bytes[i - 1].is_ascii_alphanumeric());
    for i in word_starts {
        let at = &line[i..];
        if let Some(len) = plan_ruling_len(at) {
            out.insert(at[..len].to_string());
            continue;
        }
        let Some(mut rest) = strip_keyword(at, &["rulings ", "ruling ", "ledger "]) else {
            continue;
        };
        rest = strip_keyword(rest, &["rulings ", "ruling "]).unwrap_or(rest);
        while let Some(len) = ledger_id_len(rest) {
            out.insert(rest[..len].to_string());
            rest = &rest[len..];
            match [", and ", ", ", " and ", "–", "-"]
                .into_iter()
                .find(|sep| rest.starts_with(sep))
            {
                Some(sep) => rest = &rest[sep.len()..],
                None => break,
            }
        }
    }
    out
}

/// `text` without the first of `keywords` it starts with, compared without regard to case.
fn strip_keyword<'a>(text: &'a str, keywords: &[&str]) -> Option<&'a str> {
    keywords.iter().find_map(|k| {
        let head = text.get(..k.len())?;
        head.eq_ignore_ascii_case(k).then(|| &text[k.len()..])
    })
}

/// The ruling ids a public file records, each as a bullet that starts with the id in bold:
/// `- **P3-R9** …`, `- **R1:** …` or `- **Q32.** …`.
pub fn ruling_records(markdown: &str) -> BTreeSet<String> {
    markdown
        .lines()
        .filter_map(|line| {
            let rest = line.trim_start().strip_prefix("- **")?;
            let len = plan_ruling_len(rest).or_else(|| ledger_id_len(rest))?;
            ["**", ":**", ".**"]
                .iter()
                .any(|end| rest[len..].starts_with(end))
                .then(|| rest[..len].to_string())
        })
        .collect()
}

/// `(ruling id, "path:line")` for every ruling citation in `files` that no file records.
pub fn unrecorded_rulings(files: &[(String, String)]) -> Vec<(String, String)> {
    let recorded: BTreeSet<String> = files.iter().flat_map(|(_, t)| ruling_records(t)).collect();
    let mut out = Vec::new();
    for (path, text) in files {
        for (n, line) in text.lines().enumerate() {
            for id in ruling_citations(line) {
                if !recorded.contains(&id) {
                    out.push((id, format!("{path}:{}", n + 1)));
                }
            }
        }
    }
    out
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

/// Tracked files under `pathspecs` whose names end in `ext` (never `target/`), read from the
/// working tree. Tracked files deleted from the working tree are skipped.
fn tracked_files(
    root: &Path,
    pathspecs: &[&str],
    ext: &str,
) -> Result<Vec<(String, String)>, String> {
    let out = super::root::git_command()
        .args(["ls-files", "-z", "--"])
        .args(pathspecs)
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
    for path in listing.split('\0').filter(|p| p.ends_with(ext)) {
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

/// Prints the report. Exit 1 when a citation is dangling or a private ruling citation has no
/// public record, 2 when the report can't be made, otherwise 0.
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
        let sources = tracked_files(&root, &["crates"], ".rs")?;
        Ok((
            spec,
            classification,
            sources,
            tracked_files(&root, PUBLIC_MARKDOWN, ".md")?,
        ))
    });
    let (spec, classification, sources, markdown) = match inputs {
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
    let unrecorded = unrecorded_rulings(&markdown);
    println!("private ruling citations: {}", unrecorded.len());
    for (id, place) in &unrecorded {
        eprintln!("private ruling citation: {id} at {place} (no public `- **{id}**` record)");
    }
    i32::from(!r.dangling.is_empty() || !unrecorded.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The number of tagged rule definitions in the spec. Update this one number when the
    /// spec gains or loses rules (the project's ID check prints the same count).
    const EXPECTED_RULE_COUNT: usize = 1076;

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

    #[test] // spec: QA-090
    fn ruling_citations_finds_plan_ids_and_keyword_lists() {
        let found = |line: &str| ruling_citations(line).into_iter().collect::<Vec<_>>();
        let at = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(found("(plan-3 ruling P3-R9)."), at(&["P3-R9"]));
        assert_eq!(
            found("rulings P3-R16, P3-R16b and P3-R17"),
            at(&["P3-R16", "P3-R16b", "P3-R17"])
        );
        assert_eq!(found("(rulings Q31, Q34): once"), at(&["Q31", "Q34"]));
        assert_eq!(
            found("Ruling R22 and (ledger ruling R-T: targeted)"),
            at(&["R-T", "R22"])
        );
        assert_eq!(
            found("rulings R15–R17, and R30, then prose R2"),
            at(&["R15", "R17", "R30"])
        );
        assert_eq!(found("ledger Q34 carry"), at(&["Q34"]));
        // Not citations: a bare R2 (Cloudflare R2), a plain number after "ruling", an id inside
        // a longer word, a rule id.
        assert!(found("R2 and B2 prices; review ruling 5; XP3-R9; QR12; QA-090").is_empty());
        assert!(found("the rulings' reasons, ruling Q3X").is_empty());
    }

    #[test] // spec: QA-090
    fn ruling_records_reads_bold_bullets_only() {
        let md = [
            "- **P3-R9** A model's calls run under an agent session. Why: …",
            "  - **Q31** indented records count too",
            "- **R1:** the plugin keeps its name.",
            "- **P3-R16b.** dotted",
            "- **R38 and more** bold text that is not an id alone",
            "A line naming **P3-R10** is not a record",
            "- **CORE-109** [M1] a rule, not a ruling",
        ]
        .join("\n");
        assert_eq!(ruling_records(&md), ids(&["P3-R16b", "P3-R9", "Q31", "R1"]));
    }

    #[test] // spec: QA-090
    fn unrecorded_ruling_citation_reported() {
        let files = [
            src(
                "docs/plan.md",
                "- **P3-R9** recorded with its reason\nsee ruling P3-R9 and ruling Q32\n",
            ),
            src(
                "docs/spec.md",
                "x\n(rulings Q31, P3-R9)\n- **Q31** recorded here\n",
            ),
        ];
        assert_eq!(
            unrecorded_rulings(&files),
            vec![("Q32".to_string(), "docs/plan.md:2".to_string())]
        );
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
