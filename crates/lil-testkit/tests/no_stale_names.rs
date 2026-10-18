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
//! Substrings match case-sensitively, so no row allows the old spec path
//! `2026-10-12-worldwright-suite-design.md`. Globs are case-sensitive too: `*` matches within
//! one path segment and `**` matches any number of whole segments.
//!
//! **Keep rows narrow.** One file or glob plus the exact substrings that must stay, with a
//! reason and an end date. A row that would hide a new old name is a defect.
//! [`every_allowlist_row_still_allows_something`] fails when a row, or any one glob or
//! substring of a row, no longer allows any hit, so rows can't rot.
//!
//! The rows name the `crates/lil-*` paths the files have from Task B2 on. Task B6 finished the
//! rename, so both repo-scanning tests run in every `cargo nextest run --workspace`.

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

/// The allowlist. The first rows are the plan's Task B1 table, as plan 3's Task 1 (the reduced
/// PR-D) left it: the CI, helper, credential and merge-guard rows went with the code they
/// covered, and the rows that outlive PR-D name their new end. The rest are what the PR-A
/// reviews, plan 3 and this test itself need; each says so in its `why`.
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
        until: "kept: `.worldwright\\` stays reserved and `ww://` still parses for sealed \
                snapshots (no planned end)",
    },
    Allow {
        globs: &["crates/lil-platform/src/guard.rs"],
        lines_with: &["// legacy-guard"],
        why: "literal legacy cases of the test guard",
        until: "the owner says no legacy folder is left",
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
        globs: &["crates/lil-testkit/tests/claude_settings_patterns.rs"],
        lines_with: &[
            "fn worldwright_rules_are_kept()",
            ".WorldWright",
            ".WORLDWRIGHT/",
            "notes\\worldwright.txt",
            "docs/worldwright.md",
        ],
        why: "tests of the legacy folder's deny rules: the kept-rules test and the mixed-case \
              and lookalike rows those rules are checked against (Task B5)",
        until: "the owner says the folder is gone, with the .worldwright rules",
    },
    Allow {
        globs: &["docs/superpowers/specs/2026-10-12-lilith-design.md"],
        lines_with: &["`worldwright` plugin", "plugin_worldwright_"],
        why: "R1, D5",
        until: "plugin rename",
    },
    // Rows Task B6 adds.
    Allow {
        globs: &["docs/superpowers/specs/2026-10-12-lilith-design.md"],
        lines_with: &[
            "the legacy `.worldwright\\state.json` is read only while",
            "the legacy `.worldwright\\core.lock` when `.worldwright\\` exists",
            "has `worldwright.yaml` but no `.worldwright\\`",
        ],
        why: "Appendix E: how the core serves a workspace from before the rename (R4, D4)",
        until: "the legacy workspace readers go",
    },
    Allow {
        globs: &["docs/reference/workspace-format.md"],
        lines_with: &["has `worldwright.yaml` and `.worldwright/` instead"],
        why: "the reference says a workspace from before the rename is still read (R4)",
        until: "the legacy workspace readers go",
    },
    // Rows beyond the plan's table.
    Allow {
        globs: &["crates/lil-schema/src/refs.rs"],
        lines_with: &["fn legacy_ww_ref_parses_to_the_same_ref()"],
        why: "the legacy-scheme reader test, by the name the plan's Task B4 gives it",
        until: "kept: `.worldwright\\` stays reserved and `ww://` still parses for sealed \
                snapshots (no planned end)",
    },
    // Plan 3, Global Constraints "Old names", the Task 1 `trash.rs` row and the Task 3 row.
    Allow {
        globs: &["crates/lil-core/tests/trash.rs"],
        lines_with: &["\".worldwright\""],
        why: "tests that `.worldwright\\` stays reserved, by its literal name",
        until: "kept: `.worldwright\\` stays reserved (no planned end)",
    },
    Allow {
        globs: &["crates/lil-core/tests/workspace_hub.rs"],
        lines_with: &[".worldwright\\\\core.lock"],
        why: "`core_lock_cannot_be_deleted_or_renamed_while_held`: the legacy lock keeps out an \
              old core",
        until: "kept: `.worldwright\\` stays reserved (no planned end)",
    },
    // Plan 3, Global Constraints "Old names", the Task 4 row.
    Allow {
        globs: &[
            "crates/lil-cli/tests/doctor_workspace.rs",
            "crates/lil-core/tests/workspace_tools.rs",
        ],
        lines_with: &["worldwright.yaml", ".worldwright\\\\state.json", "\"ww://"],
        why: "the legacy-name rename test builds a plugin-era workspace",
        until: "the legacy workspace readers go",
    },
    // Plan 3, Global Constraints "Old names", the Task 2 spec row: Appendix F's legacy-name
    // contract (`lil doctor`'s legacy-name rename) and its plugin contract (plugin 1.3 keeps
    // the id, R1), one row per end condition.
    Allow {
        globs: &["docs/superpowers/specs/2026-10-12-lilith-design.md"],
        lines_with: &[
            "a workspace using `worldwright.yaml`, `.worldwright\\state.json` or `ww://` refs",
            "rename `worldwright.yaml` to `lilith.yaml`",
            "rewrite `ww://` to `lil://` in asset manifests",
            "(so `ww://` parsing stays for sealed versions)",
            "`.worldwright\\` itself is left in place",
        ],
        why: "Appendix F: `lil doctor` offers the legacy-name rename of a workspace from before \
              the rename (owner question 1)",
        until: "the legacy workspace readers go",
    },
    Allow {
        globs: &["docs/superpowers/specs/2026-10-12-lilith-design.md"],
        lines_with: &["`plugin/worldwright/.claude-plugin/plugin.json` (`name: \"worldwright\"`"],
        why: "Appendix F: plugin 1.3 keeps the id `worldwright`, so its folder and name do (R1)",
        until: "plugin rename",
    },
    // Plan 3, Global Constraints "Old names", the Task 7/19/20 row: Task 7's spec-test globs.
    // `mcp_server.rs` names none of the row's substrings, so it is left out (an honest row
    // allows something through every glob).
    Allow {
        globs: &[
            "crates/lil-spec-tests/tests/plugin_hooks.rs",
            "crates/lil-spec-tests/tests/doctor_claude.rs",
        ],
        lines_with: &[
            "worldwright@lilith",
            "mcp__plugin_worldwright_",
            "plugin/worldwright/",
            "name: \"worldwright\"",
        ],
        why: "Claude Code's names for the plugin's tools and the install target (AI-110, D5)",
        until: "plugin rename",
    },
    Allow {
        globs: &["crates/lil-testkit/tests/no_stale_names.rs"],
        lines_with: &[],
        why: "this test names the old names it looks for",
        until: "never",
    },
];

/// Case-insensitive `worldwright`; `ww` in any case with no ASCII letter or digit on either
/// side; or a PascalCase identifier `Ww` + uppercase letter with no ASCII letter or digit
/// before it (`WwOutput`, `WwChild`; ruling RN8); or lowercase `ww` at the start of a word
/// followed by two or more lowercase letters, the first not `w` (`wwref`, `wwdoc`, `wwbundle`:
/// the old file extensions and names the bare-`ww` rule above misses). `www…` is not a hit.
fn is_stale(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if lower.contains("worldwright") {
        return true;
    }
    let starts_word = |b: &[u8], i: usize| i == 0 || !b[i - 1].is_ascii_alphanumeric();
    let l = lower.as_bytes();
    let bare = (0..l.len().saturating_sub(1)).any(|i| {
        l[i] == b'w'
            && l[i + 1] == b'w'
            && starts_word(l, i)
            && (i + 2 == l.len() || !l[i + 2].is_ascii_alphanumeric())
    });
    let o = text.as_bytes();
    let pascal = (0..o.len().saturating_sub(2)).any(|i| {
        o[i] == b'W' && o[i + 1] == b'w' && o[i + 2].is_ascii_uppercase() && starts_word(o, i)
    });
    let low = |c: u8| c.is_ascii_lowercase();
    let ww_word = (0..o.len().saturating_sub(3)).any(|i| {
        o[i] == b'w'
            && o[i + 1] == b'w'
            && low(o[i + 2])
            && o[i + 2] != b'w'
            && low(o[i + 3])
            && starts_word(o, i)
    });
    bare || pascal || ww_word
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
        // RN8: PascalCase identifiers.
        "WwOutput",
        "struct WwChild {",
        "Vec<WwOutput>",
        "let c: WwChild = x;",
        "WwQtRoot",
        "(WwW)",
        // The old reference-file extension.
        ".wwref",
        "a.wwref",
        "*.wwref",
        "wwref files",
        "(`lil://` refs, `.wwref` files)",
        "wwdoc",
        ".wwdoc",
        "a.wwdoc",
        "`.wwdoc` readers",
        "wwbundle",
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
        // RN8 needs `Ww`, then an uppercase letter, at the start of a word.
        "Www",
        "Wwise",
        "WwiseBank",
        "MyWwOutput",
        "XWwOutput",
        "Ww2Output",
        "WWOutput",
        // camelCase is not covered by RN8 (`_wwFlags` exists only in spike history).
        "wwOutput",
        // `wwref` inside a longer word, and the new extension.
        "awwref",
        "newwref",
        "lilref",
        ".lilref",
        "*.lilref",
        ".lildoc",
        // `www…` and words where `ww` does not start the word.
        "www",
        "wwwauth",
        "awwdoc",
        "newwdoc",
        "wwx",
        "wwdOc",
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
    // The old repo name and the App's old slug are allowed nowhere (the D9 row went with
    // PR-D).
    assert!(!allowed(
        "README.md",
        "github.com/<owner>/Worldwright-Suite"
    ));
    assert!(!allowed(
        "dev/merge-guard.toml",
        "repo = \"Worldwright-Suite\""
    ));
    assert!(!allowed(
        "crates/lil-cli/src/dev/merge.rs",
        "repo: \"Worldwright-Suite\".into(),"
    ));
    assert!(!allowed(
        "dev/merge-guard.toml",
        "reviewer_app_slug = \"worldwright-reviewer\""
    ));
    assert!(!allowed(
        "crates/lil-cli/src/dev/merge.rs",
        "const BOT: &str = \"worldwright-reviewer[bot]\";"
    ));
    assert!(!allowed(
        "dev/merge-guard.toml",
        "app = \"Worldwright-Reviewer\""
    ));
    assert!(!allowed(
        "docs/dev/merge-guard.md",
        "`worldwright-reviewer`"
    ));
    assert!(!allowed("README.md", "worldwright-reviewer"));
    assert!(!allowed(
        "crates/lil-cli/src/dev/nested/x.rs",
        "worldwright-reviewer"
    ));
    // Other substrings and globs are case-sensitive too.
    assert!(allowed("crates/lil-core/src/x.rs", "LEGACY_HOME_DIR"));
    assert!(!allowed(
        "crates/lil-core/src/x.rs",
        "legacy_home_dir = \".worldwright\""
    ));
    assert!(!allowed("Docs/superpowers/plans/x.md", "Worldwright"));
}

/// B1 follow-up: `workflows.rs`'s old tree-builder pins are reported (since PR-D, every old
/// name in `workflows.rs` is).
#[test]
fn tree_builder_pins_in_workflows_rs_are_reported() {
    const PATH: &str = "crates/lil-cli/src/dev/workflows.rs";
    // The tree-builder pins as they stand at the start of PR-B.
    for line in [
        "        \"cargo run --locked -p ww-cli -- dev spec-tests\",",
        "        \"cargo run --locked -p ww-cli -- dev spec-report\",",
        "                .any(|st| run(st).contains(\"cargo run --locked -p ww-cli -- dev spec-tests\")),",
        "/// tree itself keep `-p ww-cli` until that tree renames the crate.",
    ] {
        assert!(is_stale(line));
        assert!(!allowed(PATH, line), "{line:?} must be reported");
    }
    // And in the real tree (at either crate path), no line of `workflows.rs` that pins a
    // tree-builder run is allowed.
    for h in all_hits() {
        let at_lil = h.path.replacen("crates/ww-cli/", "crates/lil-cli/", 1);
        if at_lil == PATH
            && (h.text.contains("-p ww-cli -- dev")
                || h.text.contains("cargo run --locked -p ww-cli"))
        {
            assert!(
                !allowed(PATH, &h.text),
                "{}:{}: {} must be reported",
                h.path,
                h.line,
                h.text.trim()
            );
        }
    }
}

/// Ruling RN7: the workflows' old tree-builder lines are reported (since PR-D, every old name
/// in the workflows is).
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
