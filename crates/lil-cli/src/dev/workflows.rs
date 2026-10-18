//! Tests for the CI workflow files (QA-035, QA-069–QA-073, QA-079, QA-086; rulings R21, R22,
//! R24, R32-R34).
//!
//! They parse every file in `.github/workflows` with a YAML 1.2 parser (`yaml-rust2`, ruling R2)
//! and check the properties the merge guard depends on: the exact check names `lil dev merge`
//! reads, least-privilege tokens, SHA-pinned actions, and that the guard job never builds or
//! runs the PR head's code. This file lives under `crates/lil-cli/src/dev/**`, so it is protected
//! like the workflows themselves (QA-019).

use super::spec_tests::workspace_root;
use std::collections::BTreeMap;
use std::path::PathBuf;
use yaml_rust2::{Yaml, YamlLoader};

/// The check names `lil dev merge` requires, and the one workflow file each must come from.
const CHECKS: [(&str, &str); 4] = [
    ("every-push", "ci-push.yml"),
    ("pr-ready", "ci-pr-ready.yml"),
    ("protected-test-check", "ci-guard.yml"),
    ("privacy-check", "ci-push.yml"),
];

const FILES: [&str; 5] = [
    "ci-push.yml",
    "ci-pr-ready.yml",
    "ci-guard.yml",
    "nightly.yml",
    "post-merge.yml",
];

/// The jobs that build the CLI from main (or, in post-merge, from `before`) to judge or scan
/// commits they never build (R21, R33, R38): `(file, job id)`.
const JUDGES: [(&str, &str); 4] = [
    ("ci-guard.yml", "protected-test-check"),
    ("ci-push.yml", "privacy-check"),
    ("ci-pr-ready.yml", "privacy-check-pr"),
    ("post-merge.yml", "post-merge-check"),
];

/// The jobs that build the PR's or the pushed tree itself. That tree has the renamed crate, so
/// they name `-p lil-cli` and no other package.
const TREE_BUILDERS: [(&str, &str); 3] = [
    ("ci-push.yml", "every-push"),
    ("ci-pr-ready.yml", "pr-ready"),
    ("post-merge.yml", "post-merge-tests"),
];

/// The name of the step that picks the CLI crate a judge builds (D7).
const PICK_STEP: &str = "Pick the CLI crate";

/// The only steps that get the privacy denylist: `(file, job id, step name)`.
const SCAN_STEPS: [(&str, &str, &str); 3] = [
    (
        "ci-push.yml",
        "privacy-check",
        "Privacy scan of the branch's commits that main lacks, with the CLI built from main",
    ),
    (
        "ci-pr-ready.yml",
        "privacy-check-pr",
        "Privacy scan of the PR's commits, with the CLI built from main",
    ),
    (
        "post-merge.yml",
        "post-merge-check",
        "Check the pushed commits' identities",
    ),
];

/// The denylist variables a scan step sets, each from its own repository secret (ruling RN5):
/// GitHub expands a secret that doesn't exist to "", and the reader skips a blank variable
/// (RN4), so the scan works whichever of the two secrets exists.
const DENYLIST_SECRETS: [(&str, &str); 2] = [
    (
        "LIL_PRIVACY_DENYLIST",
        "${{ secrets.LIL_PRIVACY_DENYLIST }}",
    ),
    ("WW_PRIVACY_DENYLIST", "${{ secrets.WW_PRIVACY_DENYLIST }}"),
];

/// A scan step's first line: it fails only when both denylist variables are empty (RN5).
const DENYLIST_GUARD: &str =
    "if [ -z \"${LIL_PRIVACY_DENYLIST:-}\" ] && [ -z \"${WW_PRIVACY_DENYLIST:-}\" ]; then";

fn dir() -> PathBuf {
    workspace_root().join(".github").join("workflows")
}

/// Every workflow file in the directory: name -> (raw text, parsed document).
fn all() -> BTreeMap<String, (String, Yaml)> {
    let mut out = BTreeMap::new();
    let entries = std::fs::read_dir(dir()).expect(".github/workflows must exist");
    for e in entries {
        let path = e.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !(name.ends_with(".yml") || name.ends_with(".yaml")) {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let mut docs = YamlLoader::load_from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(docs.len(), 1, "{name}: exactly one YAML document");
        out.insert(name, (text, docs.remove(0)));
    }
    out
}

fn get(name: &str) -> (String, Yaml) {
    all()
        .remove(name)
        .unwrap_or_else(|| panic!("missing workflow {name}"))
}

fn s(y: &Yaml) -> &str {
    y.as_str().unwrap_or("")
}

/// `(job id, job)` pairs.
fn jobs(doc: &Yaml) -> Vec<(String, Yaml)> {
    doc["jobs"]
        .as_hash()
        .expect("`jobs` must be a mapping")
        .iter()
        .map(|(k, v)| (s(k).to_string(), v.clone()))
        .collect()
}

fn steps(job: &Yaml) -> Vec<Yaml> {
    job["steps"].as_vec().cloned().unwrap_or_default()
}

/// A `permissions:` block as key -> value strings. `read-all` / `write-all` map to `*`.
fn perms(y: &Yaml) -> BTreeMap<String, String> {
    match y {
        Yaml::Hash(h) => h
            .iter()
            .map(|(k, v)| (s(k).to_string(), s(v).to_string()))
            .collect(),
        Yaml::String(all) => BTreeMap::from([("*".to_string(), all.clone())]),
        _ => BTreeMap::new(),
    }
}

fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn writes(p: &BTreeMap<String, String>) -> bool {
    p.values().any(|v| v.contains("write"))
}

/// The trigger names under `on:` (a string, a list or a mapping).
fn triggers(doc: &Yaml) -> Vec<String> {
    match &doc["on"] {
        Yaml::String(one) => vec![one.clone()],
        Yaml::Array(a) => a.iter().map(|t| s(t).to_string()).collect(),
        Yaml::Hash(h) => h.keys().map(|k| s(k).to_string()).collect(),
        other => panic!("unexpected `on`: {other:?}"),
    }
}

fn str_list(y: &Yaml) -> Vec<String> {
    y.as_vec()
        .map(|v| v.iter().map(|x| s(x).to_string()).collect())
        .unwrap_or_default()
}

fn uses(step: &Yaml) -> &str {
    s(&step["uses"])
}

fn run(step: &Yaml) -> &str {
    s(&step["run"])
}

/// The index of the one step of `steps` named `name`.
fn step_named(steps: &[Yaml], name: &str) -> usize {
    let found: Vec<usize> = steps
        .iter()
        .enumerate()
        .filter(|(_, st)| s(&st["name"]) == name)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(found.len(), 1, "exactly one step {name:?}");
    found[0]
}

/// The text without whole-line `#` comments, for checks that comments may legitimately mention.
fn code_only(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn workflow_files_parse_and_name_jobs() {
    let all = all();
    for f in FILES {
        assert!(all.contains_key(f), "missing .github/workflows/{f}");
    }
    let expected_jobs: [(&str, &[&str]); 5] = [
        ("ci-push.yml", &["every-push", "privacy-check"]),
        ("ci-pr-ready.yml", &["pr-ready", "privacy-check-pr"]),
        ("ci-guard.yml", &["protected-test-check"]),
        ("nightly.yml", &["nightly"]),
        ("post-merge.yml", &["post-merge-tests", "post-merge-check"]),
    ];
    for (file, ids) in expected_jobs {
        let got: Vec<String> = jobs(&all[file].1).into_iter().map(|(id, _)| id).collect();
        assert_eq!(got, ids, "{file}: job ids");
    }

    let pinned = regex_like_pinned;
    for (file, (text, doc)) in &all {
        // Every workflow's default token is read-only.
        assert_eq!(
            perms(&doc["permissions"]),
            map(&[("contents", "read")]),
            "{file}: top-level permissions must be exactly contents: read"
        );
        for (id, job) in jobs(doc) {
            // Write access only for post-merge-check.
            let p = perms(&job["permissions"]);
            if writes(&p) {
                assert_eq!(
                    (file.as_str(), id.as_str()),
                    ("post-merge.yml", "post-merge-check"),
                    "only post-merge-check may write"
                );
            }
            // Every job is time-boxed (R22).
            let t = job["timeout-minutes"].as_i64();
            assert!(
                t.is_some_and(|t| (1..=120).contains(&t)),
                "{file}/{id}: timeout-minutes {t:?}"
            );
            assert_eq!(s(&job["runs-on"]), "windows-2025", "{file}/{id}: runner");
            // A reusable-workflow call at job level must be pinned too.
            assert!(
                job["uses"].is_badvalue(),
                "{file}/{id}: no reusable workflows"
            );
        }
        // Every `uses:` is a third-party action pinned to a full commit SHA with its version in
        // a trailing comment. The comment is checked on the raw lines, since YAML drops
        // comments; every parsed `uses` value anywhere in the document is checked too, and the
        // two counts must agree, so a flow-style `{uses: x@v1}` can't slip past the line check.
        let raw_lines: Vec<&str> = text
            .lines()
            .filter(|l| {
                let t = l.trim_start().trim_start_matches("- ");
                t.starts_with("uses:")
            })
            .collect();
        for line in &raw_lines {
            assert!(pinned(line), "{file}: unpinned action: {line}");
        }
        let mut parsed = Vec::new();
        collect_uses(doc, &mut parsed);
        for value in &parsed {
            assert!(
                pinned_value(value),
                "{file}: unpinned action (parsed): {value}"
            );
        }
        assert_eq!(
            parsed.len(),
            raw_lines.len(),
            "{file}: every `uses:` must be on its own block-style line"
        );
    }
}

/// Every value of a `uses` key anywhere in `y`, as text (a non-string value is kept as its
/// debug form, which never passes [`pinned_value`]).
fn collect_uses(y: &Yaml, out: &mut Vec<String>) {
    match y {
        Yaml::Hash(h) => {
            for (k, v) in h {
                if s(k) == "uses" {
                    out.push(v.as_str().map_or_else(|| format!("{v:?}"), str::to_string));
                }
                collect_uses(v, out);
            }
        }
        Yaml::Array(a) => a.iter().for_each(|v| collect_uses(v, out)),
        _ => {}
    }
}

/// `owner/repo[/path]@<40 lowercase hex>`.
fn pinned_value(value: &str) -> bool {
    let Some((name, sha)) = value.trim().split_once('@') else {
        return false;
    };
    name.split('/').count() >= 2
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
        && sha.len() == 40
        && sha.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
}

#[test]
fn workflow_pin_check_catches_flow_style_uses() {
    let text = "jobs:\n  j:\n    steps:\n      - {uses: actions/checkout@v7}\n      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1\n";
    let doc = YamlLoader::load_from_str(text).unwrap().remove(0);
    let mut parsed = Vec::new();
    collect_uses(&doc, &mut parsed);
    assert_eq!(parsed.len(), 2);
    assert!(!pinned_value(&parsed[0]));
    assert!(pinned_value(&parsed[1]));
    let raw = text
        .lines()
        .filter(|l| l.trim_start().trim_start_matches("- ").starts_with("uses:"))
        .count();
    assert_eq!(
        raw, 1,
        "the flow-style entry is invisible to the line check"
    );
}

/// `uses: owner/repo[/path]@<40 hex> # vX.Y.Z`.
fn regex_like_pinned(line: &str) -> bool {
    let t = line.trim_start().trim_start_matches("- ").trim_start();
    let Some(rest) = t.strip_prefix("uses:") else {
        return false;
    };
    let rest = rest.trim();
    let Some((action, comment)) = rest.split_once('#') else {
        return false;
    };
    let action = action.trim();
    let Some((name, sha)) = action.split_once('@') else {
        return false;
    };
    let name_ok = name.split('/').count() >= 2
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'));
    let sha_ok = sha.len() == 40 && sha.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'));
    let v = comment.trim();
    let version_ok = v.strip_prefix('v').is_some_and(|n| {
        let parts: Vec<&str> = n.split('.').collect();
        parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    });
    name_ok && sha_ok && version_ok
}

#[test]
fn workflow_pin_check_rejects_tags_and_short_shas() {
    let good = "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1";
    assert!(regex_like_pinned(good));
    for bad in [
        "      - uses: actions/checkout@v7",
        "      - uses: actions/checkout@v7.0.1 # v7.0.1",
        "      - uses: actions/checkout@3d3c42e # v7.0.1",
        "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1",
        "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7",
        "      - uses: ./local-action",
        "      - uses: actions/checkout@3D3C42E5AAC5BA805825DA76410C181273BA90B1 # v7.0.1",
    ] {
        assert!(!regex_like_pinned(bad), "{bad}");
    }
}

/// Task 9 Minor 4: `check()` takes the latest run by name, so each required check name must
/// come from exactly one job in exactly one workflow, or another job's result could stand in.
#[test]
fn workflow_check_names_are_unique() {
    let all = all();
    for (check, home) in CHECKS {
        let mut found = Vec::new();
        for (file, (_, doc)) in &all {
            for (id, job) in jobs(doc) {
                let name = s(&job["name"]).to_string();
                if id == check || name == check {
                    found.push((file.clone(), id.clone()));
                }
                // A job may not rename itself to (or away from) a required check name.
                if CHECKS.iter().any(|(c, _)| *c == id) {
                    assert!(
                        name.is_empty() || name == id,
                        "{file}/{id}: a required check's job must not set a different name"
                    );
                }
            }
        }
        assert_eq!(
            found,
            vec![(home.to_string(), check.to_string())],
            "check {check} must come from exactly one job"
        );
    }
}

/// The required checks in the committed merge-guard settings are exactly the checks these
/// workflows produce.
#[test] // spec: QA-086
fn workflow_checks_match_the_merge_guard_settings() {
    let committed = std::fs::read_to_string(workspace_root().join("dev/merge-guard.toml")).unwrap();
    let cfg = super::merge::MergeGuardConfig::parse(&committed).unwrap();
    let mut required = cfg.required_checks.clone();
    required.sort();
    let mut ours: Vec<String> = CHECKS.iter().map(|(c, _)| c.to_string()).collect();
    ours.sort();
    assert_eq!(required, ours);
}

/// Job ids (and so check names) are unique across every workflow (R32).
#[test] // spec: QA-086
fn workflow_job_ids_are_unique_across_workflows() {
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for (file, (_, doc)) in all() {
        for (id, job) in jobs(&doc) {
            for key in [id.clone(), s(&job["name"]).to_string()] {
                if key.is_empty() {
                    continue;
                }
                if let Some(other) = seen.insert(key.clone(), format!("{file}/{id}")) {
                    assert_eq!(other, format!("{file}/{id}"), "`{key}` is used twice");
                }
            }
        }
    }
}

/// QA-086 / R32-R34: privacy-check scans every commit of a pushed branch that main doesn't
/// have; privacy-check-pr scans a PR's commits. Both read the denylist from the Actions secret
/// and fail when both denylist variables are empty. Like ci-guard (R21), neither ever builds or
/// runs the scanned code: one checkout, of main; the commits to scan are fetched as git data; the
/// CLI is built from main's working tree in a step without the secret (so not even main's build
/// scripts see it), and the built binary runs the scan in the only step that has the secret; no
/// sccache (the Actions cache is written by jobs that run unreviewed code). Only `git fetch`,
/// `git rev-parse` and `git cat-file` run.
#[test] // spec: QA-086
fn workflow_privacy_checks_shape() {
    let build_line = "cargo build --locked -p \"$pkg\" --bin \"$bin\"";
    let cases = [
        (
            "ci-push.yml",
            "privacy-check",
            "\"$CLI_BIN\" dev privacy-scan --ref-name \"$GITHUB_REF\" --range \"refs/remotes/origin/main..$HEAD_SHA\"",
            vec![("HEAD_SHA", "${{ github.sha }}")],
        ),
        (
            "ci-pr-ready.yml",
            "privacy-check-pr",
            "\"$CLI_BIN\" dev privacy-scan --ref-name \"refs/heads/$HEAD_REF\" --range \"refs/remotes/origin/$BASE_REF..$HEAD_SHA\"",
            vec![
                ("BASE_REF", "${{ github.event.pull_request.base.ref }}"),
                ("HEAD_SHA", "${{ github.event.pull_request.head.sha }}"),
                ("HEAD_REF", "${{ github.head_ref }}"),
            ],
        ),
    ];
    for (file, id, scan_line, env) in cases {
        let (_, doc) = get(file);
        let job = &doc["jobs"][id];
        assert_eq!(s(&job["name"]), id, "{file}/{id}: name");
        // Runs for every push and every PR, drafts included.
        assert!(job["if"].is_badvalue(), "{file}/{id}: job condition");
        assert!(!writes(&perms(&job["permissions"])), "{file}/{id}");
        assert_eq!(
            job["concurrency"]["cancel-in-progress"].as_bool(),
            Some(true),
            "{file}/{id}"
        );
        // No sccache: neither the action nor its wrapper.
        for key in [
            "RUSTC_WRAPPER",
            "SCCACHE_GHA_ENABLED",
            "LIL_PRIVACY_DENYLIST",
            "WW_PRIVACY_DENYLIST",
        ] {
            assert!(job["env"][key].is_badvalue(), "{file}/{id}: job env {key}");
        }
        let steps = steps(job);
        let checkouts: Vec<&Yaml> = steps
            .iter()
            .filter(|st| uses(st).starts_with("actions/checkout@"))
            .collect();
        assert_eq!(
            checkouts.len(),
            1,
            "{file}/{id}: exactly one checkout, of main"
        );
        let checkout = checkouts[0];
        assert_eq!(s(&checkout["with"]["ref"]), "main", "{file}/{id}");
        assert!(checkout["with"]["path"].is_badvalue(), "{file}/{id}");
        assert_eq!(
            checkout["with"]["fetch-depth"].as_i64(),
            Some(0),
            "{file}/{id}"
        );
        assert_eq!(
            checkout["with"]["persist-credentials"].as_bool(),
            Some(false)
        );
        for st in &steps {
            let u = uses(st);
            assert!(
                u.is_empty() || u.starts_with("actions/checkout@"),
                "{file}/{id}: unexpected action {u}"
            );
            assert!(st["if"].is_badvalue(), "{file}/{id}: conditional step");
            assert!(
                st["continue-on-error"].is_badvalue(),
                "{file}/{id}: continue-on-error"
            );
        }

        // The head is fetched as git data, and checked to be there.
        let fetch: Vec<&Yaml> = steps
            .iter()
            .filter(|st| run(st).contains("git fetch"))
            .collect();
        assert_eq!(fetch.len(), 1, "{file}/{id}: one fetch step");
        assert!(run(fetch[0]).contains("\"$HEAD_SHA\""), "{file}/{id}");
        assert!(
            run(fetch[0]).contains("git cat-file -e \"${HEAD_SHA}^{commit}\""),
            "{file}/{id}"
        );

        let (mut builds, mut scans) = (Vec::new(), Vec::new());
        for (i, st) in steps.iter().enumerate() {
            let script = run(st);
            // Event data reaches scripts only through env, never `${{ }}` in the script.
            assert!(!script.contains("${{"), "{file}/{id}: template expansion");
            // Only these git subcommands run; the working tree stays main.
            for (pos, _) in script.match_indices("git ") {
                let before = script[..pos].chars().next_back();
                if before.is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_') {
                    continue;
                }
                let sub = script[pos + 4..].split_whitespace().next().unwrap_or("");
                assert!(
                    ["fetch", "rev-parse", "cat-file"].contains(&sub),
                    "{file}/{id}: runs `git {sub}`"
                );
            }
            for forbidden in [
                "spec-tests",
                "cargo test",
                "cargo nextest",
                "cargo run",
                "--manifest-path",
            ] {
                assert!(
                    !script.contains(forbidden),
                    "{file}/{id} runs `{forbidden}`"
                );
            }
            for line in script.lines() {
                let l = line.trim();
                if l.contains("cargo ") {
                    assert!(
                        l == "cargo --version" || l == build_line,
                        "{file}/{id}: unexpected cargo call: {l}"
                    );
                }
                if l == build_line {
                    builds.push(i);
                }
                if l.contains("dev privacy-scan") {
                    assert_eq!(l, scan_line, "{file}/{id}");
                    scans.push(i);
                }
            }
        }
        // Two steps: the build (no secret) and then the scan with the built binary.
        assert_eq!(builds.len(), 1, "{file}/{id}: one build");
        assert_eq!(scans.len(), 1, "{file}/{id}: one scan");
        assert!(builds[0] < scans[0], "{file}/{id}: build before scan");
        let (build, scan) = (&steps[builds[0]], &steps[scans[0]]);
        assert!(
            !run(scan).contains("cargo"),
            "{file}/{id}: the scan step builds nothing"
        );
        // The build step takes the crate from the pick step (see
        // judges_pick_the_cli_crate_from_the_tree_they_build), which refuses when main has none.
        let pick = step_named(&steps, PICK_STEP);
        assert!(pick < builds[0], "{file}/{id}: pick before build");
        for out in ["pkg", "bin"] {
            assert_eq!(
                s(&build["env"][out]),
                format!("${{{{ steps.pick.outputs.{out} }}}}"),
                "{file}/{id}: build env {out}"
            );
        }
        assert_eq!(
            s(&scan["env"]["CLI_BIN"]),
            "./target/debug/${{ steps.pick.outputs.bin }}.exe",
            "{file}/{id}: the binary built in the workspace (main)"
        );
        for (k, v) in DENYLIST_SECRETS {
            assert_eq!(s(&scan["env"][k]), v, "{file}/{id}: env {k}");
        }
        for (k, v) in env {
            assert_eq!(s(&scan["env"][k]), v, "{file}/{id}: env {k}");
        }
        // Fails closed when both secrets are missing or empty (fork PRs get no secrets).
        assert_eq!(
            run(scan).lines().next().map(str::trim),
            Some(DENYLIST_GUARD),
            "{file}/{id}"
        );
        assert_eq!(run(scan).matches("exit 1").count(), 1, "{file}/{id}");
        // The secret reaches only the scan step.
        for (i, st) in steps.iter().enumerate() {
            if i != scans[0] {
                for (k, _) in DENYLIST_SECRETS {
                    assert!(st["env"][k].is_badvalue(), "{file}/{id}: step {i} has {k}");
                }
            }
        }
    }
}

#[test]
fn workflow_never_uses_pull_request_target() {
    for (file, (text, doc)) in all() {
        assert!(
            !triggers(&doc).iter().any(|t| t == "pull_request_target"),
            "{file}"
        );
        assert!(
            !code_only(&text).contains("pull_request_target"),
            "{file}: pull_request_target outside a comment"
        );
    }
}

/// R21: protected-test-check builds `lil` only from main and never builds, tests or runs the PR
/// head's code. The head is fetched as git data and judged against the full ref
/// refs/remotes/origin/main.
#[test]
fn workflow_ci_guard_never_runs_head_code() {
    let (text, doc) = get("ci-guard.yml");
    let mut t = triggers(&doc);
    t.sort();
    // R40: every workflow run is its own check suite, and the ruleset refuses a merge while any
    // suite on the head commit holds a cancelled or failed protected-test-check. So: only
    // `pull_request`, no `pull_request_review` (it added a run per review), no
    // `ready_for_review` (a draft-to-ready switch added a run for the same commit; the verdict
    // doesn't depend on draft state), and no run ever cancels another run of the same commit.
    // `lil dev approve` re-runs the commit's failed runs after posting the approval instead.
    assert_eq!(t, ["pull_request"]);
    assert!(doc["on"]["pull_request_review"].is_badvalue());
    assert_eq!(
        str_list(&doc["on"]["pull_request"]["types"]),
        ["opened", "synchronize", "reopened"]
    );
    // Per-head concurrency, never cancelling: a new push gets a new group, and a second run
    // for the same commit (a close and reopen) waits instead of cancelling the first.
    let concurrency = &doc["jobs"]["protected-test-check"]["concurrency"];
    assert_eq!(
        s(&concurrency["group"]),
        "guard-${{ github.event.pull_request.number }}-${{ github.event.pull_request.head.sha }}"
    );
    assert_eq!(concurrency["cancel-in-progress"].as_bool(), Some(false));
    assert!(doc["concurrency"].is_badvalue(), "no workflow-level group");

    let job = &doc["jobs"]["protected-test-check"];
    assert_eq!(
        perms(&job["permissions"]),
        map(&[("contents", "read"), ("pull-requests", "read")])
    );
    let steps = steps(job);
    let checkouts: Vec<&Yaml> = steps
        .iter()
        .filter(|st| uses(st).starts_with("actions/checkout@"))
        .collect();
    assert_eq!(checkouts.len(), 1, "exactly one checkout, of main");
    assert_eq!(s(&checkouts[0]["with"]["ref"]), "main");
    assert!(checkouts[0]["with"]["path"].is_badvalue());

    let mut guard_runs = 0;
    for st in &steps {
        let script = run(st);
        // The working tree stays main: nothing switches, resets or merges it.
        for forbidden in [
            "git checkout",
            "git switch",
            "git reset",
            "git merge",
            "git worktree",
            "git pull",
            "spec-tests",
            "cargo test",
            "cargo nextest",
            "--manifest-path",
        ] {
            assert!(!script.contains(forbidden), "ci-guard runs `{forbidden}`");
        }
        for line in script.lines().filter(|l| l.contains("cargo ")) {
            // The only cargo calls: a version print, and building and running the CLI crate
            // picked from main's checkout (the working tree) on the head commit as data.
            let l = line.trim();
            let version = l == "cargo --version";
            let run_guard = l
                == "cargo run --locked -p \"$pkg\" --bin \"$bin\" -- dev guard --base refs/remotes/origin/main --head \"$HEAD_SHA\" --pr \"$PR_NUMBER\"";
            assert!(version || run_guard, "unexpected cargo call: {l}");
            guard_runs += usize::from(run_guard);
        }
        // Event data reaches scripts only through env, never `${{ }}` in the script.
        assert!(
            !script.contains("${{"),
            "template expansion in a run script"
        );
    }
    assert_eq!(guard_runs, 1, "exactly one guard run");
    assert!(code_only(&text).contains("refs/remotes/origin/main"));
    assert!(text.contains("github.event.pull_request.head.sha"));
}

/// R24 and QA-079: only post-merge-check can write, and it never runs the pushed code's tests
/// (those run in the read-only post-merge-tests job); it judges with a CLI built from `before`.
#[test]
fn workflow_post_merge_check_is_the_only_writer() {
    let mut writers = Vec::new();
    for (file, (_, doc)) in all() {
        assert!(!writes(&perms(&doc["permissions"])), "{file}: top level");
        for (id, job) in jobs(&doc) {
            if writes(&perms(&job["permissions"])) {
                writers.push(format!("{file}/{id}"));
            }
        }
    }
    assert_eq!(writers, ["post-merge.yml/post-merge-check"]);

    let (_, doc) = get("post-merge.yml");
    assert_eq!(triggers(&doc), ["push"]);
    assert_eq!(str_list(&doc["on"]["push"]["branches"]), ["main"]);
    assert!(
        doc["concurrency"].is_badvalue(),
        "post-merge is never cancelled"
    );
    let check = &doc["jobs"]["post-merge-check"];
    assert!(check["concurrency"].is_badvalue());
    assert_eq!(
        perms(&check["permissions"]),
        map(&[
            ("contents", "write"),
            ("pull-requests", "write"),
            ("issues", "write")
        ])
    );
    assert_eq!(s(&check["needs"]), "post-merge-tests");
    let tests = &doc["jobs"]["post-merge-tests"];
    assert_eq!(perms(&tests["permissions"]), map(&[("contents", "read")]));

    let scripts: Vec<String> = steps(check).iter().map(|st| run(st).to_string()).collect();
    let all_scripts = scripts.join("\n");
    for forbidden in ["cargo nextest", "cargo test", "spec-tests", "cargo clippy"] {
        assert!(
            !all_scripts.contains(forbidden),
            "post-merge-check must not run `{forbidden}`"
        );
    }
    assert!(all_scripts.contains("dev guard --post-merge --before \"$BEFORE\" --after \"$AFTER\""));
    assert!(all_scripts.contains("dev revert-pr --before \"$BEFORE\" --after \"$AFTER\""));
    assert!(all_scripts.contains("git worktree add --detach"));

    // I4: the judge runs even when the tests failed or were cancelled.
    assert_eq!(s(&check["if"]), "${{ always() }}");
    let by_name = |name: &str| {
        steps(check)
            .into_iter()
            .find(|st| s(&st["name"]) == name)
            .unwrap_or_else(|| panic!("no step {name:?}"))
    };
    let revert = by_name("Open a revert PR and an issue");
    assert!(s(&revert["if"]).contains("always()"), "{:?}", revert["if"]);
    assert!(!s(&revert["if"]).contains("cancelled()"));
    // R27: repository creation runs the tests but skips the judge and the revert.
    let src = by_name("Choose the tree the CLI is built from");
    assert!(run(&src).contains("repository creation push; nothing to compare"));
    assert!(run(&src).contains("judge=false"));
    // R38: no fallback to the pushed commit. When `before` has no CLI crate, the pick step
    // refuses (and the revert step opens the plain issue); the CLI is only ever built from the
    // worktree of `before`.
    let pick = by_name(PICK_STEP);
    for st in [&src, &pick] {
        assert!(
            !run(st).contains("$GITHUB_WORKSPACE"),
            "the CLI must never be built from the pushed commit"
        );
        assert!(!run(st).contains("::warning::"), "{}", run(st));
    }
    for crate_path in ["crates/lil-cli/Cargo.toml", "crates/ww-cli/Cargo.toml"] {
        assert!(
            run(&pick).contains(&format!("git cat-file -e \"${{BEFORE}}:{crate_path}\"")),
            "{}",
            run(&pick)
        );
    }
    assert!(
        run(&pick).contains("else\n  echo \"::error::"),
        "no crate in `before` refuses: {}",
        run(&pick)
    );
    assert_eq!(run(&pick).matches("exit 1").count(), 1, "{}", run(&pick));
    assert_eq!(
        run(&src).matches("dir=\"").count(),
        2,
        "dir is empty or before's worktree"
    );
    assert!(run(&src).contains("dir=\"$RUNNER_TEMP/cli-before\""));
    assert!(
        run(&revert).contains("gh issue create"),
        "the plain issue stays"
    );
    for name in [PICK_STEP, "Build the CLI from main", "Judge the push"] {
        assert_eq!(
            s(&by_name(name)["if"]),
            "steps.src.outputs.judge == 'true'",
            "{name}"
        );
    }
    assert!(s(&revert["if"]).contains("steps.src.outputs.judge != 'false'"));
    // R38: the CLI refuses to run outside the checkout it was built from, so the judge and the
    // revert run in the worktree of `before` that it was built in (the revert falls back to the
    // workspace only when that step never chose a tree, to open the plain issue).
    let built_in = "${{ steps.src.outputs.dir }}";
    assert_eq!(s(&pick["working-directory"]), built_in);
    assert_eq!(
        s(&by_name("Build the CLI from main")["working-directory"]),
        built_in
    );
    assert_eq!(s(&by_name("Judge the push")["working-directory"]), built_in);
    assert_eq!(
        s(&revert["working-directory"]),
        "${{ steps.src.outputs.dir || github.workspace }}"
    );
    for st in steps(tests) {
        assert!(
            st["if"].is_badvalue(),
            "post-merge-tests never skips a step"
        );
    }
    for st in steps(check) {
        assert!(
            !run(&st).contains("${{"),
            "template expansion in a run script"
        );
    }
}

/// QA-086: GitHub creates a PR's merge commit itself, with the e-mail the account's web settings
/// give it, so no local hook or pre-push scan sees it. post-merge-check therefore runs
/// `dev privacy-scan --commits before..after` with the same CLI the judge uses (built from
/// `before`, in its worktree), in the only step of the job that gets the denylist secrets. That
/// step builds nothing, runs no cargo and holds no token. A failure fails the job and opens an
/// issue (no revert PR: history keeps the commit), whose body never repeats the CLI's output.
#[test] // spec: QA-086
fn workflow_post_merge_identity_check_shape() {
    let (_, doc) = get("post-merge.yml");
    for (id, job) in jobs(&doc) {
        for (k, _) in DENYLIST_SECRETS {
            assert!(job["env"][k].is_badvalue(), "{id}: job env {k}");
        }
    }
    for st in steps(&doc["jobs"]["post-merge-tests"]) {
        for (k, _) in DENYLIST_SECRETS {
            assert!(st["env"][k].is_badvalue(), "post-merge-tests: {k}");
        }
        assert!(!run(&st).contains("privacy-scan"));
    }
    let check = &doc["jobs"]["post-merge-check"];
    let steps = steps(check);
    let pos = |name: &str| {
        steps
            .iter()
            .position(|st| s(&st["name"]) == name)
            .unwrap_or_else(|| panic!("no step {name:?}"))
    };
    let (build, judge, identity, issue, revert) = (
        pos("Build the CLI from main"),
        pos("Judge the push"),
        pos("Check the pushed commits' identities"),
        pos("Open an issue about a commit's identity"),
        pos("Open a revert PR and an issue"),
    );
    assert!(
        build < judge && judge < identity && identity < issue,
        "step order"
    );

    // The identity step: the before-built binary, in the worktree it was built in, after the
    // judge whatever the judge said, and never for repository creation (R27).
    let st = &steps[identity];
    assert_eq!(s(&st["id"]), "identity");
    assert_eq!(s(&steps[build]["id"]), "build");
    assert_eq!(
        s(&st["if"]),
        "${{ always() && steps.src.outputs.judge == 'true' && steps.build.outcome == 'success' }}"
    );
    assert_eq!(
        s(&st["working-directory"]),
        "${{ steps.src.outputs.dir }}",
        "the worktree of `before` (R38)"
    );
    assert!(
        st["continue-on-error"].is_badvalue(),
        "a finding fails the job"
    );
    let env: Vec<&str> = st["env"]
        .as_hash()
        .expect("identity step env")
        .keys()
        .map(s)
        .collect();
    assert_eq!(
        env,
        ["CLI_BIN", "LIL_PRIVACY_DENYLIST", "WW_PRIVACY_DENYLIST"],
        "no token next to the secret"
    );
    assert_eq!(s(&st["env"]["CLI_BIN"]), "${{ steps.pick.outputs.cli }}");
    for (k, v) in DENYLIST_SECRETS {
        assert_eq!(s(&st["env"][k]), v, "identity step env {k}");
    }
    let script = run(st);
    assert!(!script.contains("${{"), "template expansion");
    assert!(
        !script.contains("cargo"),
        "the identity step builds and runs no cargo"
    );
    assert!(!script.contains("--manifest-path"));
    assert!(
        !script.contains("$GITHUB_WORKSPACE"),
        "never the pushed commit's tree"
    );
    let scans: Vec<&str> = script
        .lines()
        .map(str::trim)
        .filter(|l| l.contains("privacy-scan"))
        .collect();
    assert_eq!(
        scans,
        ["\"$CLI_BIN\" dev privacy-scan --commits \"$BEFORE..$AFTER\" || status=$?"]
    );
    assert_eq!(
        script.lines().next().map(str::trim),
        Some(DENYLIST_GUARD),
        "fails only when both denylist variables are empty"
    );
    assert!(script.contains("exit \"$status\""));
    // BEFORE and AFTER reach it through the job's env.
    assert_eq!(s(&check["env"]["BEFORE"]), "${{ github.event.before }}");
    assert_eq!(s(&check["env"]["AFTER"]), "${{ github.sha }}");

    // The secret is on that step only, within post-merge-check.
    for (i, other) in steps.iter().enumerate() {
        if i != identity {
            for (k, _) in DENYLIST_SECRETS {
                assert!(other["env"][k].is_badvalue(), "step {i} has {k}");
            }
            assert!(!run(other).contains("privacy-scan"), "step {i} scans");
        }
    }

    // The issue: opened on a failure of the identity step, assigned to the owner, without the
    // CLI's output and without a revert.
    let st = &steps[issue];
    assert_eq!(
        s(&st["if"]),
        "${{ always() && steps.identity.outcome == 'failure' }}"
    );
    assert_eq!(s(&st["env"]["GH_TOKEN"]), "${{ secrets.GITHUB_TOKEN }}");
    assert!(st["working-directory"].is_badvalue());
    let script = run(st);
    assert!(script.contains("gh issue create --repo \"$GITHUB_REPOSITORY\" --assignee \"$OWNER\""));
    assert!(script.contains("--title \"A commit on main carries a non-no-reply e-mail\""));
    assert!(script.contains("Keep my email addresses private"));
    for forbidden in [
        "$CLI_BIN",
        "$WW",
        "privacy-scan",
        "revert-pr",
        "gh pr",
        "git push",
        "cargo",
    ] {
        assert!(
            !script.contains(forbidden),
            "the issue step runs `{forbidden}`"
        );
    }
    // A revert PR can't help (history keeps the commit), so the revert never depends on it.
    assert!(!s(&steps[revert]["if"]).contains("identity"));
}

#[test]
fn workflow_checkouts_and_scripts_are_safe() {
    for (file, (_, doc)) in all() {
        for (id, job) in jobs(&doc) {
            for st in steps(&job) {
                if uses(&st).starts_with("actions/checkout@") {
                    // Task 7 carry-forward: never recurse into submodules.
                    assert!(
                        st["with"]["submodules"].is_badvalue()
                            || st["with"]["submodules"].as_bool() == Some(false),
                        "{file}/{id}: submodules"
                    );
                    // persist-credentials is always explicit and always false: the repository
                    // is public, so even the guard job fetches the PR head without credentials.
                    let persist = st["with"]["persist-credentials"].as_bool();
                    assert_eq!(persist, Some(false), "{file}/{id}: persist-credentials");
                }
                assert!(
                    !run(&st).contains("${{"),
                    "{file}/{id}: template expansion in a run script"
                );
            }
        }
    }
}

#[test]
fn workflow_every_push_and_pr_ready_shape() {
    let (_, push) = get("ci-push.yml");
    assert_eq!(triggers(&push), ["push"]);
    assert_eq!(str_list(&push["on"]["push"]["branches-ignore"]), ["main"]);
    let job = &push["jobs"]["every-push"];
    assert_eq!(s(&job["concurrency"]["group"]), "push-${{ github.ref }}");
    assert_eq!(
        job["concurrency"]["cancel-in-progress"].as_bool(),
        Some(true)
    );
    let scripts: String = steps(job)
        .iter()
        .map(|st| run(st).to_string())
        .collect::<Vec<_>>()
        .join("\n");
    for needed in [
        "cargo fmt --check",
        "cargo clippy --workspace --all-targets --locked -- -D warnings",
        "cargo deny check",
        "cargo nextest run --workspace --locked",
        "cargo run --locked -p lil-cli -- dev spec-tests",
        "cargo run --locked -p lil-cli -- dev spec-report",
    ] {
        assert!(scripts.contains(needed), "every-push lacks `{needed}`");
    }
    // R25: no skips. Every step of every push runs.
    for st in steps(job) {
        assert!(
            st["if"].is_badvalue(),
            "every-push step is conditional: {st:?}"
        );
    }

    let (_, ready) = get("ci-pr-ready.yml");
    assert_eq!(triggers(&ready), ["pull_request"]);
    assert_eq!(
        str_list(&ready["on"]["pull_request"]["types"]),
        ["opened", "synchronize", "reopened", "ready_for_review"]
    );
    let job = &ready["jobs"]["pr-ready"];
    // R38: a skipped job counts as passing for GitHub's required checks, so a draft must fail
    // pr-ready, not skip it. The job always runs, and its first step fails on a draft.
    assert!(
        job["if"].is_badvalue(),
        "pr-ready must not be skipped: {:?}",
        job["if"]
    );
    let first = &steps(job)[0];
    assert!(
        uses(first).is_empty(),
        "the draft check comes before the checkout"
    );
    assert_eq!(
        s(&first["env"]["DRAFT"]),
        "${{ github.event.pull_request.draft }}"
    );
    assert!(first["if"].is_badvalue() && first["continue-on-error"].is_badvalue());
    assert!(
        run(first).contains("if [ \"$DRAFT\" != \"false\" ]; then"),
        "{}",
        run(first)
    );
    assert_eq!(run(first).matches("exit 1").count(), 1, "{}", run(first));
    let scripts: String = steps(job)
        .iter()
        .map(|st| run(st).to_string())
        .collect::<Vec<_>>()
        .join("\n");
    for needed in [
        "cargo fmt --check",
        "cargo clippy --workspace --all-targets --locked -- -D warnings",
        "cargo deny check",
        "cargo nextest run --workspace --locked",
        "cargo run --locked -p lil-cli -- dev spec-tests",
        "cargo audit",
        "cargo build --workspace --release --locked",
    ] {
        assert!(scripts.contains(needed), "pr-ready lacks `{needed}`");
    }

    let (_, nightly) = get("nightly.yml");
    let mut t = triggers(&nightly);
    t.sort();
    assert_eq!(t, ["schedule", "workflow_dispatch"]);
    let job = &nightly["jobs"]["nightly"];
    assert_eq!(s(&job["if"]), "github.ref == 'refs/heads/main'");
    // R25: no "main unchanged" skip, so no need to read earlier runs.
    assert_eq!(perms(&job["permissions"]), map(&[("contents", "read")]));
    for st in steps(job) {
        assert!(
            st["if"].is_badvalue(),
            "nightly step is conditional: {st:?}"
        );
        assert!(!run(&st).contains("gh run list"));
    }
    assert_eq!(s(&nightly["concurrency"]["group"]), "nightly");
    assert_eq!(
        nightly["concurrency"]["cancel-in-progress"].as_bool(),
        Some(true)
    );
}

/// R38: the CLI refuses to work on a repository other than the checkout it was built from, so
/// every step that runs a built CLI binary runs in the directory the build step built it in.
/// (`cargo run` builds and runs in one step, so it is always in its own checkout.)
#[test]
fn workflow_runs_the_cli_where_it_was_built() {
    let dir_of = |st: &Yaml| -> String {
        s(&st["working-directory"])
            .trim()
            .replace(" || github.workspace }}", " }}")
    };
    let mut checked = 0;
    for (file, (_, doc)) in all() {
        for (id, job) in jobs(&doc) {
            let steps = steps(&job);
            let builds: Vec<String> = steps
                .iter()
                .filter(|st| {
                    let script = run(st);
                    script.contains("cargo build")
                        && ["-p \"$pkg\"", "-p ww-cli", "-p lil-cli"]
                            .iter()
                            .any(|p| script.contains(p))
                })
                .map(dir_of)
                .collect();
            for st in &steps {
                let script = run(st);
                let runs_binary = ["\"$CLI_BIN\" ", "ww.exe ", "lil.exe ", "\"$WW\" "]
                    .iter()
                    .any(|r| script.contains(r));
                if !runs_binary {
                    continue;
                }
                assert_eq!(builds.len(), 1, "{file}/{id}: one CLI build per job");
                assert_eq!(
                    dir_of(st),
                    builds[0],
                    "{file}/{id}: step {:?} runs the CLI outside the directory it was built in",
                    s(&st["name"])
                );
                checked += 1;
            }
        }
    }
    // privacy-check, privacy-check-pr, the post-merge judge, identity check and revert.
    assert_eq!(checked, 5);
}

/// R38: the public repository starts from one commit with `lil` on main, so no workflow keeps
/// wording about a first merge made by hand or a bootstrap without `lil`.
#[test]
fn workflow_text_has_no_first_merge_bootstrap() {
    for (file, (text, _)) in all() {
        let lower = text.to_lowercase();
        for stale in ["first merge", "bootstrap", "merges that pr by hand"] {
            assert!(!lower.contains(stale), "{file}: mentions {stale:?}");
        }
    }
}

/// Review Focus 4, R21, R33, R38: CI has to keep working while the CLI crate is renamed. Every
/// judge (the jobs that build the CLI from main, or from `before` in post-merge, and never from
/// the commits they judge) first runs the step "Pick the CLI crate" in the tree it builds. That
/// step looks for `crates/lil-cli` first and then `crates/ww-cli`, refuses when neither is there,
/// and hands `pkg` and `bin` to the one build step. Nothing else in the job names a crate, so
/// the judge builds whichever crate that tree has. The jobs that build the PR's or the pushed
/// tree itself name `-p lil-cli`, the crate that tree has, and no other package.
#[test] // spec: QA-086
fn judges_pick_the_cli_crate_from_the_tree_they_build() {
    for (file, id) in JUDGES {
        let (_, doc) = get(file);
        let job = &doc["jobs"][id];
        let steps = steps(job);
        let p = step_named(&steps, PICK_STEP);
        let pick = &steps[p];
        assert_eq!(s(&pick["id"]), "pick", "{file}/{id}");
        let script = run(pick);
        let from_before = file == "post-merge.yml";

        // Both branches, the new crate first, and a refusal when neither is in the tree.
        let (lil_test, legacy_test) = if from_before {
            (
                "if git cat-file -e \"${BEFORE}:crates/lil-cli/Cargo.toml\" 2>/dev/null; then",
                "elif git cat-file -e \"${BEFORE}:crates/ww-cli/Cargo.toml\" 2>/dev/null; then",
            )
        } else {
            (
                "if [ -f crates/lil-cli/Cargo.toml ]; then",
                "elif [ -f crates/ww-cli/Cargo.toml ]; then",
            )
        };
        let lines: Vec<&str> = script.lines().map(str::trim).collect();
        assert!(lines.len() > 10, "{file}/{id}: {script}");
        assert_eq!(
            lines[..7],
            [
                lil_test,
                "pkg=lil-cli",
                "bin=lil",
                legacy_test,
                "pkg=ww-cli",
                "bin=ww",
                "else"
            ],
            "{file}/{id}: {script}"
        );
        assert!(
            lines[7].starts_with("echo \"::error::"),
            "{file}/{id}: {script}"
        );
        assert_eq!(lines[8..10], ["exit 1", "fi"], "{file}/{id}: {script}");
        assert_eq!(script.matches("exit ").count(), 1, "{file}/{id}: {script}");
        for out in [
            "echo \"pkg=$pkg\"",
            "echo \"bin=$bin\"",
            ">> \"$GITHUB_OUTPUT\"",
        ] {
            assert!(script.contains(out), "{file}/{id}: no `{out}`: {script}");
        }
        // R38: the pick reads the tree the judge builds, never the pushed commit's.
        assert!(!script.contains("$GITHUB_WORKSPACE"), "{file}/{id}");
        assert!(!script.contains("${{"), "{file}/{id}: template expansion");
        assert!(
            !script.contains("cargo"),
            "{file}/{id}: the pick builds nothing"
        );

        // The tree: main's working tree (the job's one checkout, of main), or the worktree of
        // `before` that the step before it made.
        if from_before {
            let src = step_named(&steps, "Choose the tree the CLI is built from");
            assert!(src < p, "{file}/{id}: the tree is chosen first");
            assert_eq!(s(&steps[src]["id"]), "src");
            assert!(
                run(&steps[src])
                    .contains("git worktree add --detach \"$RUNNER_TEMP/cli-before\" \"$BEFORE\""),
                "{}",
                run(&steps[src])
            );
            assert_eq!(s(&job["env"]["BEFORE"]), "${{ github.event.before }}");
            assert_eq!(
                s(&pick["working-directory"]),
                "${{ steps.src.outputs.dir }}"
            );
            assert_eq!(s(&pick["if"]), "steps.src.outputs.judge == 'true'");
            assert!(
                script.contains("echo \"cli=$RUNNER_TEMP/cli-target/debug/$bin.exe\""),
                "{script}"
            );
        } else {
            assert!(pick["working-directory"].is_badvalue(), "{file}/{id}");
            let checkouts: Vec<usize> = steps
                .iter()
                .enumerate()
                .filter(|(_, st)| uses(st).starts_with("actions/checkout@"))
                .map(|(i, _)| i)
                .collect();
            assert_eq!(checkouts.len(), 1, "{file}/{id}: one checkout");
            let checkout = &steps[checkouts[0]];
            assert_eq!(s(&checkout["with"]["ref"]), "main", "{file}/{id}");
            assert!(checkout["with"]["path"].is_badvalue(), "{file}/{id}");
            assert!(checkouts[0] < p, "{file}/{id}: main is checked out first");
        }

        // One build (or build-and-run) step, after the pick, in the same tree, with its outputs.
        let builds: Vec<usize> = steps
            .iter()
            .enumerate()
            .filter(|(_, st)| run(st).contains("cargo build") || run(st).contains("cargo run"))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(builds.len(), 1, "{file}/{id}: one build step");
        let b = builds[0];
        let build = &steps[b];
        assert!(p < b, "{file}/{id}: pick before build");
        assert_eq!(
            s(&build["env"]["pkg"]),
            "${{ steps.pick.outputs.pkg }}",
            "{file}/{id}"
        );
        assert_eq!(
            s(&build["env"]["bin"]),
            "${{ steps.pick.outputs.bin }}",
            "{file}/{id}"
        );
        assert!(
            run(build).contains("--locked -p \"$pkg\" --bin \"$bin\""),
            "{file}/{id}: {}",
            run(build)
        );
        assert_eq!(
            s(&build["working-directory"]),
            s(&pick["working-directory"]),
            "{file}/{id}: built in the tree the crate was picked from"
        );
        if from_before {
            assert_eq!(
                s(&build["env"]["CARGO_TARGET_DIR"]),
                "${{ runner.temp }}/cli-target"
            );
        }

        // Only the pick names a crate; every binary run goes through CLI_BIN, from its outputs.
        let expected_bin = if from_before {
            "${{ steps.pick.outputs.cli }}"
        } else {
            "./target/debug/${{ steps.pick.outputs.bin }}.exe"
        };
        let mut runs = 0;
        for (i, st) in steps.iter().enumerate() {
            let script = run(st);
            if i != p {
                for name in ["ww-cli", "lil-cli", "ww.exe", "lil.exe", "\"$WW\""] {
                    assert!(
                        !script.contains(name),
                        "{file}/{id}: step {:?} names `{name}`",
                        s(&st["name"])
                    );
                }
            }
            if script.contains("\"$CLI_BIN\"") {
                assert!(i > b, "{file}/{id}: runs the CLI before building it");
                assert_eq!(s(&st["env"]["CLI_BIN"]), expected_bin, "{file}/{id}");
                runs += 1;
            }
        }
        let expected_runs = match id {
            "protected-test-check" => 0, // `cargo run` builds and runs in one step
            "post-merge-check" => 3,     // the judge, the identity check and the revert
            _ => 1,                      // the scan
        };
        assert_eq!(runs, expected_runs, "{file}/{id}: CLI_BIN runs");
    }

    for (file, id) in TREE_BUILDERS {
        let (_, doc) = get(file);
        let steps = steps(&doc["jobs"][id]);
        assert!(
            steps
                .iter()
                .all(|st| s(&st["name"]) != PICK_STEP && s(&st["id"]) != "pick"),
            "{file}/{id}: builds its own tree"
        );
        assert!(
            steps
                .iter()
                .any(|st| run(st).contains("cargo run --locked -p lil-cli -- dev spec-tests")),
            "{file}/{id}"
        );
        // The renamed crate only: every `-p` in these jobs names `lil-cli`.
        for st in &steps {
            let script = run(st);
            for (at, _) in script.match_indices(" -p ") {
                assert!(
                    script[at..].starts_with(" -p lil-cli "),
                    "{file}/{id}: builds a package other than lil-cli: {script}"
                );
            }
        }
    }
}

/// QA-086, rulings RN4 and RN5: each scan step sets both denylist variables, each from its own
/// repository secret, and fails only when both are empty. GitHub expands a secret that doesn't
/// exist to "", and the reader skips a blank variable, so the scan keeps working whichever secret
/// exists. The scripts never expand either variable except in that guard, so nothing prints it.
#[test] // spec: QA-086
fn scan_steps_set_both_denylist_names_each_from_its_own_secret() {
    // The names the CLI reads (privacy.rs), new first.
    assert_eq!(
        [
            lil_platform::names::DENYLIST_ENV,
            lil_platform::names::LEGACY_DENYLIST_ENV
        ],
        [DENYLIST_SECRETS[0].0, DENYLIST_SECRETS[1].0]
    );
    for (file, id, name) in SCAN_STEPS {
        let (_, doc) = get(file);
        let steps = steps(&doc["jobs"][id]);
        let st = &steps[step_named(&steps, name)];
        for (k, v) in DENYLIST_SECRETS {
            assert_eq!(s(&st["env"][k]), v, "{file}/{id}: env {k}");
            assert!(
                s(&st["env"][k]).contains(&format!("secrets.{k} ")),
                "{file}/{id}: {k} comes from the secret of the same name"
            );
        }
        let script = run(st);
        let lines: Vec<&str> = script.lines().map(str::trim).collect();
        assert_eq!(lines[0], DENYLIST_GUARD, "{file}/{id}: {script}");
        assert!(
            lines[1].starts_with("echo \"::error::"),
            "{file}/{id}: {script}"
        );
        for (k, _) in DENYLIST_SECRETS {
            assert_eq!(
                script.matches(&format!("[ -z \"${{{k}:-}}\" ]")).count(),
                1,
                "{file}/{id}: {k} is tested once, in the both-empty guard"
            );
            assert_eq!(
                script.matches(&format!("${{{k}")).count(),
                1,
                "{file}/{id}: {k} is expanded only in the guard"
            );
            assert!(
                !script.contains(&format!("${k}")),
                "{file}/{id}: {k} expanded outside the guard"
            );
        }
        assert!(
            !lines
                .iter()
                .any(|l| l.contains("PRIVACY_DENYLIST") && l.contains("||")),
            "{file}/{id}: one empty variable must not fail the scan"
        );
    }
}

/// QA-086, R33: the denylist secrets reach the three scan steps and nothing else: no workflow
/// or job env, no other step's env or action inputs, no other script, and no way of reading
/// every secret at once.
#[test] // spec: QA-086
fn no_step_outside_a_scan_sees_the_denylist_secret() {
    let mut scans_seen = 0;
    for (file, (text, doc)) in all() {
        let code = code_only(&text);
        let expected = usize::from(SCAN_STEPS.iter().any(|(f, _, _)| *f == file));
        for (_, secret) in DENYLIST_SECRETS {
            assert_eq!(code.matches(secret).count(), expected, "{file}: {secret}");
        }
        // GitHub expressions ignore case, so count every spelling, comments included.
        let lower = text.to_lowercase();
        for name in [
            "secrets.lil_privacy_denylist",
            "secrets.ww_privacy_denylist",
        ] {
            assert_eq!(
                lower.matches(name).count(),
                expected,
                "{file}: {name} in any case"
            );
        }
        assert_eq!(
            code.matches("PRIVACY_DENYLIST }}").count(),
            2 * expected,
            "{file}: a denylist secret under another name"
        );
        let lower_code = code.to_lowercase();
        for whole in ["tojson(secrets", "secrets[", "secrets.*"] {
            assert!(!lower_code.contains(whole), "{file}: `{whole}` in any case");
        }
        let mentions = |y: &Yaml| -> bool {
            y.as_hash().is_some_and(|h| {
                h.iter().any(|(k, v)| {
                    s(k).contains("PRIVACY_DENYLIST") || s(v).contains("PRIVACY_DENYLIST")
                })
            })
        };
        assert!(!mentions(&doc["env"]), "{file}: workflow env");
        for (id, job) in jobs(&doc) {
            assert!(!mentions(&job["env"]), "{file}/{id}: job env");
            for st in steps(&job) {
                let name = s(&st["name"]);
                let is_scan = SCAN_STEPS
                    .iter()
                    .any(|(f, j, n)| *f == file && *j == id && *n == name);
                if is_scan {
                    scans_seen += 1;
                    continue;
                }
                assert!(!mentions(&st["env"]), "{file}/{id}: step {name:?} env");
                assert!(!mentions(&st["with"]), "{file}/{id}: step {name:?} with");
                assert!(
                    !run(&st).contains("PRIVACY_DENYLIST"),
                    "{file}/{id}: step {name:?} script"
                );
            }
        }
    }
    assert_eq!(scans_seen, SCAN_STEPS.len(), "every scan step exists once");
}
