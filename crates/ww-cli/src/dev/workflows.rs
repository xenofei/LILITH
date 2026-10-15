//! Tests for the CI workflow files (QA-035, QA-069–QA-073, QA-079, QA-086; rulings R21, R22,
//! R24, R32-R34).
//!
//! They parse every file in `.github/workflows` with a YAML 1.2 parser (`yaml-rust2`, ruling R2)
//! and check the properties the merge guard depends on: the exact check names `ww dev merge`
//! reads, least-privilege tokens, SHA-pinned actions, and that the guard job never builds or
//! runs the PR head's code. This file lives under `crates/ww-cli/src/dev/**`, so it is protected
//! like the workflows themselves (QA-019).

use super::spec_tests::workspace_root;
use std::collections::BTreeMap;
use std::path::PathBuf;
use yaml_rust2::{Yaml, YamlLoader};

/// The check names `ww dev merge` requires, and the one workflow file each must come from.
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
/// and fail when it is empty. Like ci-guard (R21), neither ever builds or runs the scanned code:
/// one checkout, of main; the commits to scan are fetched as git data; `ww` is built from main's
/// working tree in a step without the secret (so not even main's build scripts see it), and the
/// built binary runs the scan in the only step that has the secret; no sccache (the Actions
/// cache is written by jobs that run unreviewed code). Only `git fetch`, `git rev-parse` and
/// `git cat-file` run.
#[test] // spec: QA-086
fn workflow_privacy_checks_shape() {
    let secret = "${{ secrets.WW_PRIVACY_DENYLIST }}";
    let build_line = "cargo build --locked -p ww-cli --bin ww";
    let cases = [
        (
            "ci-push.yml",
            "privacy-check",
            "./target/debug/ww.exe dev privacy-scan --ref-name \"$GITHUB_REF\" --range \"refs/remotes/origin/main..$HEAD_SHA\"",
            vec![("HEAD_SHA", "${{ github.sha }}")],
        ),
        (
            "ci-pr-ready.yml",
            "privacy-check-pr",
            "./target/debug/ww.exe dev privacy-scan --ref-name \"refs/heads/$HEAD_REF\" --range \"refs/remotes/origin/$BASE_REF..$HEAD_SHA\"",
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
        assert!(
            run(build).contains("if [ ! -f crates/ww-cli/Cargo.toml ]; then"),
            "{file}/{id}: no ww on main fails the build step"
        );
        assert_eq!(
            s(&scan["env"]["WW_PRIVACY_DENYLIST"]),
            secret,
            "{file}/{id}"
        );
        for (k, v) in env {
            assert_eq!(s(&scan["env"][k]), v, "{file}/{id}: env {k}");
        }
        // Fails closed when the secret is missing or empty (fork PRs get no secrets).
        assert!(
            run(scan).contains("if [ -z \"${WW_PRIVACY_DENYLIST:-}\" ]; then"),
            "{file}/{id}"
        );
        assert_eq!(run(scan).matches("exit 1").count(), 1, "{file}/{id}");
        // The secret reaches only the scan step.
        for (i, st) in steps.iter().enumerate() {
            if i != scans[0] {
                assert!(
                    st["env"]["WW_PRIVACY_DENYLIST"].is_badvalue(),
                    "{file}/{id}: step {i} has the secret"
                );
            }
        }
    }
    // The denylist secret reaches these two jobs' scan steps and post-merge-check's identity
    // step (checked in workflow_post_merge_identity_check_shape), and nothing else.
    for (file, (text, doc)) in all() {
        let expected = match file.as_str() {
            "ci-push.yml" | "ci-pr-ready.yml" | "post-merge.yml" => 1,
            _ => 0,
        };
        assert_eq!(
            code_only(&text)
                .matches("secrets.WW_PRIVACY_DENYLIST")
                .count(),
            expected,
            "{file}"
        );
        for (id, job) in jobs(&doc) {
            assert!(
                job["env"]["WW_PRIVACY_DENYLIST"].is_badvalue(),
                "{file}/{id}"
            );
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

/// R21: protected-test-check builds `ww` only from main and never builds, tests or runs the PR
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
    // `ww dev approve` re-runs the commit's failed runs after posting the approval instead.
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
            // The only cargo calls: a version print, and building and running ww from main's
            // checkout (the working tree) on the head commit as data.
            let l = line.trim();
            let version = l == "cargo --version";
            let run_guard = l
                == "cargo run --locked -p ww-cli -- dev guard --base refs/remotes/origin/main --head \"$HEAD_SHA\" --pr \"$PR_NUMBER\"";
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
/// (those run in the read-only post-merge-tests job); it judges with a ww built from `before`.
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
    let src = by_name("Choose the source ww is built from");
    assert!(run(&src).contains("repository creation push; nothing to compare"));
    assert!(run(&src).contains("judge=false"));
    // R38: no fallback to the pushed commit. When `before` has no ww, the step refuses (and the
    // revert step opens the plain issue); ww is only ever built from the worktree of `before`.
    assert!(
        !run(&src).contains("$GITHUB_WORKSPACE"),
        "ww must never be built from the pushed commit"
    );
    assert!(
        run(&src).contains("git cat-file -e \"${BEFORE}:crates/ww-cli/Cargo.toml\""),
        "{}",
        run(&src)
    );
    assert_eq!(
        run(&src).matches("dir=\"").count(),
        2,
        "dir is empty or before's worktree"
    );
    assert!(run(&src).contains("dir=\"$RUNNER_TEMP/ww-before\""));
    assert!(!run(&src).contains("::warning::"), "{}", run(&src));
    assert!(
        run(&revert).contains("gh issue create"),
        "the plain issue stays"
    );
    for name in ["Build ww", "Judge the push"] {
        assert_eq!(
            s(&by_name(name)["if"]),
            "steps.src.outputs.judge == 'true'",
            "{name}"
        );
    }
    assert!(s(&revert["if"]).contains("steps.src.outputs.judge != 'false'"));
    // R38: ww refuses to run outside the checkout it was built from, so the judge and the revert
    // run in the worktree of `before` that ww was built in (the revert falls back to the
    // workspace only when that step never chose a source, to open the plain issue).
    let built_in = "${{ steps.src.outputs.dir }}";
    assert_eq!(s(&by_name("Build ww")["working-directory"]), built_in);
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
/// `ww dev privacy-scan --commits before..after` with the same `ww` the judge uses (built from
/// `before`, in its worktree), in the only step of the job that gets the denylist secret. That
/// step builds nothing, runs no cargo and holds no token. A failure fails the job and opens an
/// issue (no revert PR: history keeps the commit), whose body never repeats ww's output.
#[test] // spec: QA-086
fn workflow_post_merge_identity_check_shape() {
    let secret = "${{ secrets.WW_PRIVACY_DENYLIST }}";
    let (_, doc) = get("post-merge.yml");
    for (id, job) in jobs(&doc) {
        assert!(job["env"]["WW_PRIVACY_DENYLIST"].is_badvalue(), "{id}");
    }
    for st in steps(&doc["jobs"]["post-merge-tests"]) {
        assert!(st["env"]["WW_PRIVACY_DENYLIST"].is_badvalue());
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
        pos("Build ww"),
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
        ["WW", "WW_PRIVACY_DENYLIST"],
        "no token next to the secret"
    );
    assert_eq!(s(&st["env"]["WW"]), "${{ steps.src.outputs.ww }}");
    assert_eq!(s(&st["env"]["WW_PRIVACY_DENYLIST"]), secret);
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
        ["\"$WW\" dev privacy-scan --commits \"$BEFORE..$AFTER\" || status=$?"]
    );
    assert!(script.contains("if [ -z \"${WW_PRIVACY_DENYLIST:-}\" ]; then"));
    assert!(script.contains("exit \"$status\""));
    // BEFORE and AFTER reach it through the job's env.
    assert_eq!(s(&check["env"]["BEFORE"]), "${{ github.event.before }}");
    assert_eq!(s(&check["env"]["AFTER"]), "${{ github.sha }}");

    // The secret is on that step only, within post-merge-check.
    for (i, other) in steps.iter().enumerate() {
        if i != identity {
            assert!(
                other["env"]["WW_PRIVACY_DENYLIST"].is_badvalue(),
                "step {i} has the secret"
            );
            assert!(!run(other).contains("privacy-scan"), "step {i} scans");
        }
    }

    // The issue: opened on a failure of the identity step, assigned to the owner, without ww's
    // output and without a revert.
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
        "cargo run --locked -p ww-cli -- dev spec-tests",
        "cargo run --locked -p ww-cli -- dev spec-report",
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
        "cargo run --locked -p ww-cli -- dev spec-tests",
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

/// R38: `ww` refuses to work on a repository other than the checkout it was built from, so every
/// step that runs a built `ww` binary runs in the directory the build step built it in.
/// (`cargo run` builds and runs in one step, so it is always in its own checkout.)
#[test]
fn workflow_runs_ww_where_it_was_built() {
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
                .filter(|st| run(st).contains("cargo build") && run(st).contains("-p ww-cli"))
                .map(dir_of)
                .collect();
            for st in &steps {
                let script = run(st);
                let runs_binary = script.contains("ww.exe ") || script.contains("\"$WW\" ");
                if !runs_binary {
                    continue;
                }
                assert_eq!(builds.len(), 1, "{file}/{id}: one ww build per job");
                assert_eq!(
                    dir_of(st),
                    builds[0],
                    "{file}/{id}: step {:?} runs ww outside the directory it was built in",
                    s(&st["name"])
                );
                checked += 1;
            }
        }
    }
    // privacy-check, privacy-check-pr, the post-merge judge, identity check and revert.
    assert_eq!(checked, 5);
}

/// R38: the public repository starts from one commit with `ww` on main, so no workflow keeps
/// wording about a first merge made by hand or a bootstrap without `ww`.
#[test]
fn workflow_text_has_no_first_merge_bootstrap() {
    for (file, (text, _)) in all() {
        let lower = text.to_lowercase();
        for stale in ["first merge", "bootstrap", "merges that pr by hand"] {
            assert!(!lower.contains(stale), "{file}: mentions {stale:?}");
        }
    }
}
