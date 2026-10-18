//! Repo-wide "no stale names" test for the LILITH rename (plan 2026-10-17, Task B1).
//!
//! Every tracked path (`git ls-files -z` at the repo root) and every line of every tracked
//! text file is checked for an old name: case-insensitive `worldwright`, or `ww` in any case
//! with no ASCII letter or digit on either side ([`is_stale`]). A file counts as text when it
//! is valid UTF-8 and has no NUL byte in its first 8 KiB. A hit fails the test unless a row
//! of [`ALLOW`] allows it.
//!
//! **Matching.** A row allows a line when the file's path matches one of the row's globs and
//! the line contains one of the row's `lines_with` substrings. A row with no substrings allows
//! the whole file. A path hit is checked the same way, with the path itself as the line.
//! Substrings match case-sensitively, so the D9 row's `Worldwright-Suite` never allows the
//! old spec path `2026-10-12-worldwright-suite-design.md`. Globs are case-sensitive too: `*`
//! matches within one path segment and `**` matches any number of whole segments.
//!
//! **Keep rows narrow.** One file or glob plus the exact substrings that must stay, with a
//! reason and an end date. A row that would hide a new old name is a defect.
//! [`every_allowlist_row_still_allows_something`] fails when a row, or any one glob or
//! substring of a row, no longer allows any hit, so rows can't rot.
//!
//! The rows name the `crates/lil-*` paths the files have from Task B2 on. Until the rename is
//! finished (Task B6) this test is red, so the two repo-scanning tests are `#[ignore]`d and
//! [`ignores_come_off_once_no_stale_name_is_left`] fails as soon as the tree is clean, so the
//! ignores can't outlive the rename. Run them with:
//! `cargo nextest run -p ww-testkit -E 'binary(no_stale_names)' --run-ignored all`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// One allowlist row.
struct Allow {
    /// Repo-relative paths, `/`-separated, that the row covers.
    globs: &'static [&'static str],
    /// A covered line is allowed when it contains one of these (case-sensitive). Empty means
    /// every line of a covered file, and its path, are allowed.
    lines_with: &'static [&'static str],
    /// Why the old name stays.
    why: &'static str,
    /// When the row goes.
    until: &'static str,
}

/// The allowlist. The first nine rows are the plan's Task B1 table, verbatim except that row 5
/// is split so the workflows get only the pick step's exact text (ruling RN7). The rest are
/// what the PR-A reviews and this test itself need; each says so in its `why`.
const ALLOW: &[Allow] = &[
    Allow {
        globs: &["docs/superpowers/plans/**"],
        lines_with: &[],
        why: "executed plans are history (R3)",
        until: "never",
    },
    Allow {
        globs: &["docs/spikes/**", "spikes/**"],
        lines_with: &[],
        why: "M0 spike records are history",
        until: "never",
    },
    Allow {
        globs: &["**/*.rs"],
        lines_with: &["LEGACY_"],
        why: "legacy readers (R4) and their tests",
        until: "PR-D",
    },
    Allow {
        globs: &["crates/lil-platform/src/guard.rs"],
        lines_with: &["// legacy-guard"],
        why: "literal legacy cases of the test guard",
        until: "the owner says no legacy folder is left",
    },
    // The plan's row 5, minus `.github/workflows/*.yml`: in the workflows a bare `ww-cli`
    // would also allow the tree-builder `-p ww-cli` lines Task B2 must change (ruling RN7).
    // The workflows get the narrow row right after this one.
    Allow {
        globs: &[
            "tools/git-hooks/credential-helper",
            "crates/lil-cli/src/dev/workflows.rs",
            "crates/lil-cli/tests/credential_helper.rs",
        ],
        lines_with: &["ww-cli", "ww.exe", "--bin ww", "WW_PRIVACY_DENYLIST"],
        why: "CI and helper transition (B1, B2, B5)",
        until: "PR-D",
    },
    Allow {
        globs: &[".github/workflows/*.yml"],
        lines_with: &[
            // The pick step's transition branch, by its exact text.
            "elif [ -f crates/ww-cli/Cargo.toml ]; then",
            "elif git cat-file -e \"${BEFORE}:crates/ww-cli/Cargo.toml\"",
            "pkg=ww-cli",
            "nor crates/ww-cli",
            // The comments that describe that branch.
            "lil-cli once main has it, else ww-cli.",
            "# ww-cli. Never the pushed commit",
            "else crates/ww-cli)",
            "# crates/ww-cli), so the check keeps working",
            // The legacy secret (RN5).
            "WW_PRIVACY_DENYLIST",
        ],
        why: "CI transition in the workflows: the pick step's ww-cli branch and the legacy \
              secret only, never a tree-builder `-p ww-cli` line (B1 row 5 narrowed, RN7)",
        until: "PR-D",
    },
    Allow {
        globs: &[
            ".claude/settings.json",
            "crates/lil-testkit/tests/claude_settings_patterns.rs",
        ],
        lines_with: &[".worldwright"],
        why: "deny rules stay while that folder exists",
        until: "the owner says the folder is gone",
    },
    Allow {
        globs: &["docs/superpowers/specs/2026-10-12-lilith-design.md"],
        lines_with: &["`worldwright` plugin", "plugin_worldwright_"],
        why: "R1, D5",
        until: "plugin rename",
    },
    Allow {
        globs: &["**"],
        lines_with: &["Worldwright-Suite", "worldwright-reviewer"],
        why: "repo and App names (D9)",
        until: "PR-C, which reviews and narrows it",
    },
    // Rows beyond the plan's table.
    Allow {
        globs: &[".github/workflows/*.yml"],
        lines_with: &["bin=ww"],
        why: "the pick step's ww-cli transition branch sets bin=ww (D7, PR-A review)",
        until: "PR-D, with the ww-cli branch",
    },
    Allow {
        globs: &["crates/lil-cli/src/dev/workflows.rs"],
        lines_with: &["\"$WW\""],
        why: "negative check that no step runs the old CI variable (PR-A review)",
        until: "PR-D",
    },
    Allow {
        globs: &[
            "crates/lil-platform/src/credentials.rs",
            "crates/lil-platform/src/env.rs",
            "crates/lil-platform/src/guard.rs",
            "crates/lil-platform/tests/guard.rs",
            "crates/lil-platform/tests/credential_manager_roundtrip.rs",
            "crates/lil-testkit/src/env.rs",
        ],
        lines_with: &["Worldwright-test-"],
        why: "the test credential prefix and its pins until Task B3 renames them (A1 review)",
        until: "Task B3, which removes this row",
    },
    Allow {
        globs: &["crates/lil-testkit/tests/no_stale_names.rs"],
        lines_with: &[],
        why: "this test names the old names it looks for",
        until: "never",
    },
];

/// Case-insensitive `worldwright`, or `ww` in any case with no ASCII letter or digit on
/// either side.
fn is_stale(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if lower.contains("worldwright") {
        return true;
    }
    let b = lower.as_bytes();
    (0..b.len().saturating_sub(1)).any(|i| {
        b[i] == b'w'
            && b[i + 1] == b'w'
            && (i == 0 || !b[i - 1].is_ascii_alphanumeric())
            && (i + 2 == b.len() || !b[i + 2].is_ascii_alphanumeric())
    })
}

/// Whether `pattern` (`*` within a segment, `**` across whole segments) matches `path`.
fn glob_matches(pattern: &str, path: &str) -> bool {
    let p: Vec<&str> = pattern.split('/').collect();
    let s: Vec<&str> = path.split('/').collect();
    segments_match(&p, &s)
}

fn segments_match(p: &[&str], s: &[&str]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((&"**", rest)) => (0..=s.len()).any(|i| segments_match(rest, &s[i..])),
        Some((first, rest)) => match s.split_first() {
            Some((seg, srest)) => {
                segment_matches(first.as_bytes(), seg.as_bytes()) && segments_match(rest, srest)
            }
            None => false,
        },
    }
}

fn segment_matches(p: &[u8], s: &[u8]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((b'*', rest)) => (0..=s.len()).any(|i| segment_matches(rest, &s[i..])),
        Some((c, rest)) => s.first() == Some(c) && segment_matches(rest, &s[1..]),
    }
}

fn row_allows(row: &Allow, path: &str, line: &str) -> bool {
    row.globs.iter().any(|g| glob_matches(g, path))
        && (row.lines_with.is_empty() || row.lines_with.iter().any(|w| line.contains(w)))
}

/// Whether some row allows `line` (or the path itself, passed as `line`) in `path`.
fn allowed(path: &str, line: &str) -> bool {
    ALLOW.iter().any(|row| row_allows(row, path, line))
}

/// One stale name: line 0 is the path itself.
struct Hit {
    path: String,
    line: usize,
    text: String,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Every stale path and line in the tracked tree, allowed or not.
fn all_hits() -> Vec<Hit> {
    let root = repo_root();
    let out = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(&root)
        .output()
        .expect("failed to run `git ls-files`");
    assert!(
        out.status.success(),
        "`git ls-files` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut hits = Vec::new();
    for raw in out.stdout.split(|&b| b == 0).filter(|p| !p.is_empty()) {
        let path = String::from_utf8(raw.to_vec()).expect("tracked path is not UTF-8");
        if is_stale(&path) {
            hits.push(Hit {
                path: path.clone(),
                line: 0,
                text: path.clone(),
            });
        }
        let bytes = match std::fs::read(root.join(&path)) {
            Ok(b) => b,
            // Tracked but deleted in the working tree: only its path is checked.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => panic!("cannot read {path}: {e}"),
        };
        if bytes[..bytes.len().min(8192)].contains(&0) {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if is_stale(line) {
                hits.push(Hit {
                    path: path.clone(),
                    line: i + 1,
                    text: line.to_string(),
                });
            }
        }
    }
    hits
}

fn disallowed(hits: &[Hit]) -> Vec<&Hit> {
    hits.iter().filter(|h| !allowed(&h.path, &h.text)).collect()
}

#[test]
#[ignore = "red until Task B6 finishes the rename; Task B6 removes this ignore"]
fn no_stale_names_outside_the_allowlist() {
    let hits = all_hits();
    let bad = disallowed(&hits);
    let listing: String = bad
        .iter()
        .map(|h| format!("{}:{}: {}\n", h.path, h.line, h.text.trim()))
        .collect();
    assert!(
        bad.is_empty(),
        "{} old name(s) outside the allowlist (line 0 is the path itself):\n{listing}",
        bad.len()
    );
}

#[test]
#[ignore = "red until Task B6 finishes the rename; Task B6 removes this ignore"]
fn every_allowlist_row_still_allows_something() {
    let hits = all_hits();
    let mut rotten = Vec::new();
    for row in ALLOW {
        let label = format!("row {:?} ({}; until {})", row.globs, row.why, row.until);
        let lines_ok = |line: &str| {
            row.lines_with.is_empty() || row.lines_with.iter().any(|w| line.contains(w))
        };
        for glob in row.globs {
            if !hits
                .iter()
                .any(|h| glob_matches(glob, &h.path) && lines_ok(&h.text))
            {
                rotten.push(format!("{label}: glob {glob:?} allows nothing"));
            }
        }
        for w in row.lines_with {
            if !hits
                .iter()
                .any(|h| h.text.contains(w) && row.globs.iter().any(|g| glob_matches(g, &h.path)))
            {
                rotten.push(format!("{label}: substring {w:?} allows nothing"));
            }
        }
    }
    assert!(
        rotten.is_empty(),
        "allowlist rows that allow nothing (remove or narrow them):\n{}",
        rotten.join("\n")
    );
}

/// Keeps the two `#[ignore]`s above from outliving the rename: once no stale name is left
/// outside the allowlist, this fails until Task B6 removes both ignores and this test.
#[test]
fn ignores_come_off_once_no_stale_name_is_left() {
    let hits = all_hits();
    assert!(
        !disallowed(&hits).is_empty(),
        "no stale name is left outside the allowlist: remove the #[ignore]s from \
         no_stale_names_outside_the_allowlist and every_allowlist_row_still_allows_something, \
         and delete this test"
    );
}

#[test]
fn is_stale_finds_old_names_and_ignores_lookalikes() {
    for hit in [
        "ww dev",
        "ww_core",
        "WW_X",
        ".ww-tmp-",
        "ww://",
        "ww/event",
        "Worldwright",
        "WORLDWRIGHT",
        "a.worldwright-local",
        "ww",
        "(Ww)",
    ] {
        assert!(is_stale(hit), "{hit:?} should be a hit");
    }
    for miss in [
        "www.example",
        "wwwauth[]",
        "ShowWindow",
        "NewWindow",
        "lil",
        "lilith",
        "LILITH",
        "ww2",
        "aww",
        "",
        "w",
    ] {
        assert!(!is_stale(miss), "{miss:?} should not be a hit");
    }
}

#[test]
fn allowlist_is_case_sensitive_so_the_old_spec_path_is_not_allowed() {
    let old_spec = "docs/superpowers/specs/2026-10-12-worldwright-suite-design.md";
    // As a path hit, and as a link in any file.
    assert!(is_stale(old_spec));
    assert!(!allowed(old_spec, old_spec));
    assert!(!allowed(
        "README.md",
        &format!("see [the spec]({old_spec})")
    ));
    assert!(!allowed("README.md", "WORLDWRIGHT-SUITE"));
    // The D9 row allows the repo and App names only as written.
    assert!(allowed("README.md", "github.com/<owner>/Worldwright-Suite"));
    assert!(allowed(
        "dev/merge-guard.toml",
        "app = \"worldwright-reviewer\""
    ));
    assert!(!allowed(
        "dev/merge-guard.toml",
        "app = \"Worldwright-Reviewer\""
    ));
    // Other substrings and globs are case-sensitive too.
    assert!(allowed("crates/lil-core/src/x.rs", "LEGACY_HOME_DIR"));
    assert!(!allowed(
        "crates/lil-core/src/x.rs",
        "legacy_home_dir = \".worldwright\""
    ));
    assert!(!allowed("Docs/superpowers/plans/x.md", "Worldwright"));
}

/// Ruling RN7: in the workflows only the pick step's transition branch may say `ww-cli`. The
/// tree-builder steps run the pushed tree's CLI and must be reported until Task B2 changes
/// them to `-p lil-cli`.
#[test]
fn tree_builder_ww_cli_lines_in_the_workflows_are_reported() {
    // The tree-builder lines as they stand at the start of PR-B.
    for (path, line) in [
        (
            ".github/workflows/ci-pr-ready.yml",
            "        run: cargo run --locked -p ww-cli -- dev spec-tests",
        ),
        (
            ".github/workflows/ci-push.yml",
            "        run: cargo run --locked -p ww-cli -- dev spec-tests",
        ),
        (
            ".github/workflows/ci-push.yml",
            "        run: cargo run --locked -p ww-cli -- dev spec-report",
        ),
        (
            ".github/workflows/post-merge.yml",
            "        run: cargo run --locked -p ww-cli -- dev spec-tests",
        ),
        (
            ".github/workflows/ci-push.yml",
            "        run: cargo build --locked -p ww-cli --bin ww",
        ),
    ] {
        assert!(is_stale(line));
        assert!(!allowed(path, line), "{path}: {line:?} must be reported");
    }
    // The pick step's transition branch stays allowed.
    for line in [
        "          elif [ -f crates/ww-cli/Cargo.toml ]; then",
        "            pkg=ww-cli",
        "            bin=ww",
        "            echo \"::error::main has neither crates/lil-cli nor crates/ww-cli, so there is no CLI to build.\"",
        "        # In main's working tree (the checkout above): lil-cli once main has it, else ww-cli.",
        "          WW_PRIVACY_DENYLIST: ${{ secrets.WW_PRIVACY_DENYLIST }}",
    ] {
        assert!(allowed(".github/workflows/ci-push.yml", line), "{line:?}");
    }
    // And in the real tree, no tracked workflow line that runs `-p ww-cli` is allowed.
    for h in all_hits() {
        if glob_matches(".github/workflows/*.yml", &h.path) && h.text.contains("-p ww-cli") {
            assert!(
                !allowed(&h.path, &h.text),
                "{}:{}: {} must be reported",
                h.path,
                h.line,
                h.text.trim()
            );
        }
    }
}

#[test]
fn globs_match_whole_segments() {
    assert!(glob_matches(
        "docs/superpowers/plans/**",
        "docs/superpowers/plans/a.md"
    ));
    assert!(glob_matches(
        "docs/superpowers/plans/**",
        "docs/superpowers/plans/x/a.md"
    ));
    assert!(!glob_matches(
        "docs/superpowers/plans/**",
        "docs/superpowers/plansx/a.md"
    ));
    assert!(glob_matches("**/*.rs", "build.rs"));
    assert!(glob_matches("**/*.rs", "crates/a/src/b.rs"));
    assert!(!glob_matches("**/*.rs", "crates/a/src/b.rs.txt"));
    assert!(glob_matches(
        ".github/workflows/*.yml",
        ".github/workflows/ci-push.yml"
    ));
    assert!(!glob_matches(
        ".github/workflows/*.yml",
        ".github/workflows/x/ci.yml"
    ));
    assert!(!glob_matches("spikes/**", "docs/spikes/a.md"));
    assert!(glob_matches("**", "README.md"));
    assert!(!glob_matches(
        "crates/lil-cli/src/dev/workflows.rs",
        "crates/ww-cli/src/dev/workflows.rs"
    ));
}
