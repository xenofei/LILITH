//! Protected paths and the test-diff guard verdict (QA-019, QA-021).
//!
//! Callers use one entry point, [`verdict`]. It pins both revisions to SHAs, judges the change
//! against the union of the base and head protected-path lists, and gathers every input itself.
//! [`evaluate`] is the pure core: it takes the changes, the diff hunks and the new expected-fail
//! entries and returns a [`Verdict`]. Git is only touched by [`verdict`], [`collect_changes`],
//! [`file_at`] and [`expected_fail_changes`].
//!
//! Conventions:
//! - All paths are repo-relative with `/` separators (see [`normalize_path`]).
//! - `ExpectedFail::file` is relative to the `crates/lil-spec-tests` crate (for example
//!   `tests/foo.rs`), as the entries in `expected-fail.toml` are written. `spec_files_added`
//!   holds repo-relative paths (for example `crates/lil-spec-tests/tests/foo.rs`), which is
//!   what [`spec_files_added`] and git produce.
//! - An expected-fail entry is bound to its test file: `lil-spec-tests::<bin>::...` belongs to
//!   `tests/<bin>.rs` or `tests/<bin>/main.rs`, which must be added by the same change and be
//!   the entry's `file`. An existing test's name cannot be pointed at a dummy file.
//! - Only a plain regular-file (mode 100644) modification of `expected-fail.toml` gets the
//!   expected-fail exemption; symlinks, type changes and mode changes need approval.

use crate::dev::spec_tests;
use anyhow::{Context, Result, bail};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use std::path::Path;

/// Repo-relative path of the expected-fail list.
pub const EXPECTED_FAIL_PATH: &str = "crates/lil-spec-tests/expected-fail.toml";
/// The crate directory that `ExpectedFail::file` is relative to.
const SPEC_CRATE_DIR: &str = "crates/lil-spec-tests";
/// Repo-relative path of the protected-path list.
const PROTECTED_PATHS_FILE: &str = "dev/protected-paths.txt";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    /// A new regular file.
    Added,
    /// A new symlink or submodule entry (mode 120000 or 160000).
    AddedLink,
    /// Content changed; the mode is the same on both sides (and 100644 for `expected-fail.toml`).
    Modified,
    /// Only the mode changed between regular-file modes (for example `chmod +x`), possibly
    /// together with content.
    ModeChanged,
    /// The file type changed (`T`), or a symlink or submodule entry was modified.
    TypeChanged,
    Deleted,
    Renamed {
        from: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// For a rename, the new path.
    pub path: String,
    pub kind: ChangeKind,
}

/// Converts `\` to `/` and drops leading `./`, so Windows and git paths compare equal.
pub(crate) fn normalize_path(p: &str) -> String {
    let s = p.replace('\\', "/");
    let mut s = s.as_str();
    while let Some(rest) = s.strip_prefix("./") {
        s = rest;
    }
    s.to_string()
}

/// The protected-path list (QA-019).
///
/// `all` matches every pattern; `strict` matches the patterns that are not `add-ok:` (ruling
/// R26), under which a new file needs approval too.
pub struct Protected {
    all: GlobSet,
    strict: GlobSet,
}

/// The `add-ok:` line prefix (ruling R26): new files under that pattern need no approval. Only
/// the test homes carry it; changing or deleting files there still needs approval.
pub const ADD_OK_PREFIX: &str = "add-ok:";

impl Protected {
    /// Parses gitignore-style patterns, one per line.
    ///
    /// - A line may start with `add-ok:` (R26): adding a new file under that pattern needs no
    ///   approval. Every other protected pattern needs approval for additions too. When the same
    ///   pattern appears both with and without the prefix (as in the union of the base and head
    ///   lists), the stricter one wins: it is not add-ok.
    /// - Blank lines and `#` comments are skipped; `!` negation is not supported and is an error.
    /// - A pattern without a `/` matches at any depth; one with a `/` is anchored at the repo
    ///   root (a leading `/` is allowed and ignored).
    /// - A pattern also protects everything beneath it, so `dir/` and `dir` cover `dir/x/y`.
    /// - `**` crosses directories. Matching is case-insensitive (Windows paths are).
    ///
    /// A list with no patterns is an error: an empty guard would protect nothing.
    pub fn parse(text: &str) -> Result<Protected> {
        // pattern key (lower-cased glob) -> (its globs, add-ok on every occurrence)
        let mut patterns: std::collections::BTreeMap<String, (Vec<globset::Glob>, bool)> =
            Default::default();
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (line, add_ok) = match line.strip_prefix(ADD_OK_PREFIX) {
                Some(rest) => (rest.trim(), true),
                None => (line, false),
            };
            if line.is_empty() {
                bail!("protected-paths: empty pattern `{raw}`");
            }
            if line.starts_with('!') {
                bail!("protected-paths: negated pattern `{line}` is not supported");
            }
            let line = line.replace('\\', "/");
            let anchored = line.starts_with('/') || line.trim_end_matches('/').contains('/');
            let body = line.trim_start_matches('/').trim_end_matches('/');
            if body.is_empty() {
                bail!("protected-paths: empty pattern `{raw}`");
            }
            let base = if anchored {
                body.to_string()
            } else {
                format!("**/{body}")
            };
            let mut globs = vec![base.clone()];
            if !base.ends_with("**") {
                globs.push(format!("{base}/**"));
            }
            let mut built = Vec::new();
            for g in globs {
                built.push(
                    GlobBuilder::new(&g)
                        .case_insensitive(true)
                        .literal_separator(false)
                        .build()
                        .with_context(|| format!("protected-paths: bad pattern `{raw}`"))?,
                );
            }
            let entry = patterns
                .entry(base.to_lowercase())
                .or_insert_with(|| (built, true));
            // Stricter wins: one occurrence without `add-ok:` makes the pattern strict.
            entry.1 &= add_ok;
        }
        if patterns.is_empty() {
            bail!("protected-paths: the list has no patterns");
        }
        let mut all = GlobSetBuilder::new();
        let mut strict = GlobSetBuilder::new();
        for (globs, add_ok) in patterns.into_values() {
            for g in globs {
                all.add(g.clone());
                if !add_ok {
                    strict.add(g);
                }
            }
        }
        Ok(Protected {
            all: all.build().context("protected-paths: building matcher")?,
            strict: strict
                .build()
                .context("protected-paths: building matcher")?,
        })
    }

    pub fn is_protected(&self, path: &str) -> bool {
        self.all.is_match(normalize_path(path))
    }

    /// Whether adding a new file at `path` needs approval: it is under at least one protected
    /// pattern that is not `add-ok:` (R26).
    pub fn addition_needs_approval(&self, path: &str) -> bool {
        self.strict.is_match(normalize_path(path))
    }
}

/// Added and removed lines of one file, across all its hunks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub path: String,
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocker {
    ProtectedChanged {
        path: String,
        kind: String,
    },
    ExpectedFailAdditionWithoutNewTest {
        name: String,
    },
    /// A new, renamed or retyped path has a component shaped like a Windows 8.3 short name.
    ShortNameAlias {
        path: String,
        component: String,
    },
    /// A changed path contains a control character (ESC, CR, BEL, ...) or a bidi or invisible
    /// character ([`is_dangerous_char`]), which could rewrite, reorder or hide what a terminal or
    /// a page shows around it (rulings R23 and R38).
    ControlCharsInPath {
        path: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    RemovedTest { path: String, line: String },
    IgnoredTest { path: String, line: String },
    NewSleepOrRetry { path: String, line: String },
    NewLintSuppression { path: String, line: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Verdict {
    pub needs_approval: Vec<Blocker>,
    pub warnings: Vec<Warning>,
}

impl Verdict {
    /// No blockers and no warnings.
    pub fn is_clean(&self) -> bool {
        self.needs_approval.is_empty() && self.warnings.is_empty()
    }
}

/// Judges a change set.
///
/// - `Added` (a regular file) needs approval only when it is a `build.rs` or `Cargo.toml` under a
///   protected path. Every other kind of change to a protected
///   path does: `Modified`, `ModeChanged`, `TypeChanged`, `Deleted`, an added symlink or
///   submodule (`AddedLink`), and a rename whose old or new path is protected.
/// - The one exception is a plain `Modified` `expected-fail.toml`: it is governed by the
///   expected-fail rule instead. Removing entries needs nothing. Each entry in
///   `expected_fail_added` must be bound to a new test binary: its name is
///   `lil-spec-tests::<bin>::<test>`, its `file` (relative to `crates/lil-spec-tests`) is
///   `tests/<bin>.rs` or `tests/<bin>/main.rs`, and that file is among `spec_files_added`
///   (repo-relative paths of files the same change adds). Names of any other shape need approval.
///   Deleting or renaming the file, or changing its type or mode, still needs approval.
pub(crate) fn evaluate(
    protected: &Protected,
    changes: &[FileChange],
    hunks: &[Hunk],
    expected_fail_added: &[spec_tests::ExpectedFail],
    spec_files_added: &[String],
) -> Verdict {
    let mut needs_approval = Vec::new();

    for c in changes {
        let path = normalize_path(&c.path);
        // R23, R38: a control, bidi or invisible character in any changed path (protected or
        // not) could rewrite or hide part of the summary the owner reads before approving, so it
        // always needs a look.
        let named: Vec<&str> = match &c.kind {
            ChangeKind::Renamed { from } => vec![from.as_str(), c.path.as_str()],
            _ => vec![c.path.as_str()],
        };
        if let Some(bad) = named.iter().find(|p| p.chars().any(is_dangerous_char)) {
            needs_approval.push(Blocker::ControlCharsInPath {
                path: bad.to_string(),
            });
        }
        // Fail closed on 8.3 short-name look-alikes for anything that creates or moves a path.
        let alias_paths: Vec<&str> = match &c.kind {
            ChangeKind::Added | ChangeKind::AddedLink | ChangeKind::TypeChanged => {
                vec![c.path.as_str()]
            }
            ChangeKind::Renamed { from } => vec![from.as_str(), c.path.as_str()],
            _ => vec![],
        };
        if let Some((alias_path, component)) = alias_paths
            .iter()
            .find_map(|p| short_name_component(p).map(|comp| (*p, comp)))
        {
            needs_approval.push(Blocker::ShortNameAlias {
                path: alias_path.to_string(),
                component,
            });
        }
        match &c.kind {
            ChangeKind::Added => {
                // A build script runs automatically during every build, so adding one inside a
                // protected area is code execution, not just a new file.
                let file_name = path.rsplit('/').next().unwrap_or("");
                if protected.is_protected(&path) {
                    let kind = if file_name.eq_ignore_ascii_case("build.rs") {
                        Some("added as a build script (it runs during the build)")
                    } else if file_name.eq_ignore_ascii_case("cargo.toml") {
                        // A new manifest under a protected glob (the workspace takes `crates/*`)
                        // can name any file as its build script, so it needs a look too.
                        Some(
                            "added as a Cargo manifest (it can name a build script that runs during the build)",
                        )
                    } else if protected.addition_needs_approval(&path) {
                        // R26: only the test homes (`add-ok:` patterns) take new files freely.
                        // Elsewhere a new file can be a new workflow, hook or cargo config.
                        Some(
                            "added (new files there need approval; only the test homes take new files freely)",
                        )
                    } else {
                        None
                    };
                    if let Some(kind) = kind {
                        needs_approval.push(Blocker::ProtectedChanged {
                            path: c.path.clone(),
                            kind: kind.into(),
                        });
                    }
                }
            }
            ChangeKind::Modified
            | ChangeKind::ModeChanged
            | ChangeKind::TypeChanged
            | ChangeKind::AddedLink
            | ChangeKind::Deleted => {
                // Exact case: a differently-cased sibling is a different file to git and cargo.
                let governed_elsewhere =
                    c.kind == ChangeKind::Modified && path == EXPECTED_FAIL_PATH;
                if protected.is_protected(&path) && !governed_elsewhere {
                    let kind = match c.kind {
                        ChangeKind::Modified => "modified",
                        ChangeKind::ModeChanged => "given a different file mode",
                        ChangeKind::TypeChanged => {
                            "changed to or from a symlink or other file type"
                        }
                        ChangeKind::AddedLink => "added as a symlink",
                        _ => "deleted",
                    };
                    needs_approval.push(Blocker::ProtectedChanged {
                        path: c.path.clone(),
                        kind: kind.into(),
                    });
                }
            }
            ChangeKind::Renamed { from } => {
                if protected.is_protected(&path) || protected.is_protected(from) {
                    needs_approval.push(Blocker::ProtectedChanged {
                        path: c.path.clone(),
                        kind: format!("renamed from {from}"),
                    });
                }
            }
        }
    }

    let new_specs: Vec<String> = spec_files_added.iter().map(|p| normalize_path(p)).collect();
    // Everything below is compared byte-exact (after `\` to `/`): cargo binary names, git paths
    // and the runner's name match are all case-sensitive, and a case-insensitive comparison
    // lets `tests/FOO/main.rs` stand in for the existing `tests/foo.rs`. Only `is_protected`
    // is case-insensitive, because there that fails closed.
    for e in expected_fail_added {
        let file = normalize_path(&e.file);
        let full = format!("{SPEC_CRATE_DIR}/{file}");
        let bound = binary_sources(&e.name)
            .is_some_and(|sources| sources.contains(&file) && new_specs.contains(&full));
        if !bound {
            needs_approval.push(Blocker::ExpectedFailAdditionWithoutNewTest {
                name: e.name.clone(),
            });
        }
    }

    Verdict {
        needs_approval,
        warnings: warnings_for(hunks),
    }
}

/// True when `component` matches `(?i)^[^.]{1,6}~[0-9]+(\.[^.]{0,3})?$`, the shape of a Windows
/// 8.3 short name (`PROGRA~1`, `spec_t~1.rs`). A path with such a component can resolve, on a
/// volume with 8.3 names enabled, to a different long-named file, so git may write through it
/// onto a protected file.
fn is_short_name_alias(component: &str) -> bool {
    // `[^.]` excludes dots, so at most one dot, separating the base from a 0-3 char extension.
    let (base, ext) = match component.split_once('.') {
        Some((base, ext)) => (base, Some(ext)),
        None => (component, None),
    };
    if ext.is_some_and(|e| e.contains('.') || e.chars().count() > 3) {
        return false;
    }
    // The digits are everything after the last `~`, so that is where the base splits.
    let Some((stem, digits)) = base.rsplit_once('~') else {
        return false;
    };
    (1..=6).contains(&stem.chars().count())
        && !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
}

/// The first 8.3-short-name-shaped component of `path`, if any.
fn short_name_component(path: &str) -> Option<String> {
    normalize_path(path)
        .split('/')
        .find(|c| is_short_name_alias(c))
        .map(str::to_string)
}

/// The source files (relative to the spec crate, exact case) of the integration-test binary an
/// expected-fail name belongs to: `lil-spec-tests::<bin>::<test>` is `tests/<bin>.rs` or
/// `tests/<bin>/main.rs`. `None` for any other shape.
///
/// Latent ambiguity: a lib unit test `foo::t` inside the crate would also be named
/// `lil-spec-tests::foo::t` and so look like binary `foo`. Today that cannot be abused, because
/// adding a lib test module means changing `src/lib.rs`, which is protected and needs approval.
fn binary_sources(name: &str) -> Option<[String; 2]> {
    let mut parts = name.split("::");
    if parts.next()? != "lil-spec-tests" {
        return None;
    }
    let bin = parts.next()?;
    let test = parts.next()?;
    if test.is_empty() || bin.is_empty() || bin == "." || bin == ".." || bin.contains(['/', '\\']) {
        return None;
    }
    Some([format!("tests/{bin}.rs"), format!("tests/{bin}/main.rs")])
}

/// Repo-relative paths of files the change adds under `crates/lil-spec-tests/`
/// (renames into the crate are not additions and need approval anyway).
pub(crate) fn spec_files_added(changes: &[FileChange]) -> Vec<String> {
    let prefix = format!("{SPEC_CRATE_DIR}/");
    changes
        .iter()
        .filter(|c| c.kind == ChangeKind::Added)
        .map(|c| normalize_path(&c.path))
        .filter(|p| p.starts_with(&prefix))
        .collect()
}

/// Entries added to and removed from `expected-fail.toml`, as `(added, removed)`.
///
/// An old entry is *removed* when no new entry has its name. A new entry is *added* when no
/// old entry is identical to it, so changing an existing entry's `rule` or `file` counts as an
/// addition (it must then be backed by a new spec test). An empty string is an empty list,
/// which is how a missing file reads.
pub(crate) fn expected_fail_diff(
    old_toml: &str,
    new_toml: &str,
) -> Result<(Vec<spec_tests::ExpectedFail>, Vec<spec_tests::ExpectedFail>)> {
    let parse = |label: &str, text: &str| -> Result<Vec<spec_tests::ExpectedFail>> {
        let file: spec_tests::ExpectedFile = toml::from_str(text)
            .with_context(|| format!("cannot parse {label} expected-fail.toml"))?;
        Ok(file.tests)
    };
    let old = parse("base", old_toml)?;
    let new = parse("head", new_toml)?;
    let added = new.iter().filter(|n| !old.contains(n)).cloned().collect();
    let removed = old
        .iter()
        .filter(|o| !new.iter().any(|n| n.name == o.name))
        .cloned()
        .collect();
    Ok((added, removed))
}

fn run_git(repo: &Path, args: &[&str]) -> Result<std::process::Output> {
    crate::dev::root::git_command()
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .context("could not run git")
}

fn git_ok(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = run_git(repo, args)?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

fn check_rev(rev: &str) -> Result<()> {
    if rev.is_empty() || rev.starts_with('-') {
        bail!("invalid revision `{rev}`");
    }
    Ok(())
}

/// The contents of `path` at `rev`, or `None` when the file does not exist there.
/// An unknown revision is an error. Contents are the committed blob, with no filters.
pub(crate) fn file_at(repo: &Path, rev: &str, path: &str) -> Result<Option<String>> {
    // Pin `rev` once (exactly, for a full ref name) and read from that commit.
    let commit = resolve_commit(repo, rev)?;
    let spec = format!("{commit}:{path}");
    if !run_git(repo, &["cat-file", "-e", &spec])?.status.success() {
        return Ok(None);
    }
    let bytes = git_ok(repo, &["cat-file", "blob", &spec])?;
    String::from_utf8(bytes)
        .map(Some)
        .with_context(|| format!("{spec} is not valid UTF-8"))
}

/// `(added, removed)` expected-fail entries for `base...head`: reads `expected-fail.toml` at
/// the merge-base of the two and at `head`. A file missing on either side is an empty list.
pub(crate) fn expected_fail_changes(
    repo: &Path,
    base: &str,
    head: &str,
) -> Result<(Vec<spec_tests::ExpectedFail>, Vec<spec_tests::ExpectedFail>)> {
    check_rev(base)?;
    check_rev(head)?;
    let mb = git_ok(repo, &["merge-base", base, head])?;
    let mb = String::from_utf8_lossy(&mb).trim().to_string();
    let old = file_at(repo, &mb, EXPECTED_FAIL_PATH)?.unwrap_or_default();
    let new = file_at(repo, head, EXPECTED_FAIL_PATH)?.unwrap_or_default();
    expected_fail_diff(&old, &new)
}

// ---- warnings ----

fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// True for files under a `tests` directory and for `tests.rs` / `*_test(s).rs`.
fn is_test_path(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    let mut segs: Vec<&str> = p.split('/').collect();
    let file = segs.pop().unwrap_or("");
    segs.contains(&"tests")
        || file == "tests.rs"
        || file.ends_with("_test.rs")
        || file.ends_with("_tests.rs")
}

fn warnings_for(hunks: &[Hunk]) -> Vec<Warning> {
    let is_rs = |h: &Hunk| {
        normalize_path(&h.path)
            .to_ascii_lowercase()
            .ends_with(".rs")
    };
    // The diff is -U0, so `mod tests` is only visible when a hunk touches that line.
    // Files where any hunk does are treated as test files for the sleep/retry check.
    let mod_tests_files: Vec<String> = hunks
        .iter()
        .filter(|h| is_rs(h))
        .filter(|h| {
            h.added
                .iter()
                .chain(&h.removed)
                .any(|l| squash(l).contains("modtests"))
        })
        .map(|h| normalize_path(&h.path))
        .collect();

    let mut out = Vec::new();
    for h in hunks.iter().filter(|h| is_rs(h)) {
        let path = normalize_path(&h.path);
        let test_file = is_test_path(&path) || mod_tests_files.contains(&path);
        for line in &h.removed {
            let s = squash(line);
            if s.contains("#[test]") || s.contains("#[tokio::test") || line.contains("proptest!") {
                out.push(Warning::RemovedTest {
                    path: path.clone(),
                    line: line.trim().to_string(),
                });
            }
        }
        for line in &h.added {
            let s = squash(line);
            let trimmed = line.trim().to_string();
            if s.contains("#[ignore") {
                out.push(Warning::IgnoredTest {
                    path: path.clone(),
                    line: trimmed.clone(),
                });
            }
            if test_file && (line.contains("sleep(") || line.to_ascii_lowercase().contains("retry"))
            {
                out.push(Warning::NewSleepOrRetry {
                    path: path.clone(),
                    line: trimmed.clone(),
                });
            }
            if ["#[allow(", "#![allow(", "#[expect(", "#![expect("]
                .iter()
                .any(|p| s.contains(p))
            {
                out.push(Warning::NewLintSuppression {
                    path: path.clone(),
                    line: trimmed,
                });
            }
        }
    }
    out
}

// ---- git parsing ----

fn is_link_mode(mode: &str) -> bool {
    mode == "120000" || mode == "160000"
}

/// Classifies an `M` entry from its modes. Only a same-mode, regular-file edit is `Modified`,
/// and `expected-fail.toml` additionally has to be mode 100644 on both sides to be so.
fn modified_kind(old_mode: &str, new_mode: &str, path: &str) -> ChangeKind {
    if is_link_mode(old_mode) || is_link_mode(new_mode) {
        ChangeKind::TypeChanged
    } else if old_mode != new_mode {
        ChangeKind::ModeChanged
    } else if old_mode == "100644" || path != EXPECTED_FAIL_PATH {
        ChangeKind::Modified
    } else {
        ChangeKind::ModeChanged
    }
}

/// Parses `git diff -z --raw -M` output:
/// `:100644 100644 <sha> <sha> M\0path\0`, `:100644 100644 <sha> <sha> R100\0old\0new\0`, ...
/// The modes tell regular files from symlinks and submodules.
fn parse_raw(raw: &[u8]) -> Result<Vec<FileChange>> {
    let text = String::from_utf8_lossy(raw);
    let mut toks = text.split('\0').filter(|t| !t.is_empty());
    let mut out = Vec::new();
    while let Some(meta) = toks.next() {
        let fields: Vec<&str> = meta
            .strip_prefix(':')
            .with_context(|| format!("git raw diff: unexpected token `{meta}`"))?
            .split_whitespace()
            .collect();
        let [old_mode, new_mode, _old_sha, _new_sha, status] = fields[..] else {
            bail!("git raw diff: malformed entry `{meta}`");
        };
        let mut path = || -> Result<String> {
            toks.next()
                .map(normalize_path)
                .with_context(|| format!("git raw diff: missing path after `{status}`"))
        };
        let added_kind = |mode: &str| {
            if is_link_mode(mode) {
                ChangeKind::AddedLink
            } else {
                ChangeKind::Added
            }
        };
        let change = match status.chars().next() {
            Some('A') => FileChange {
                path: path()?,
                kind: added_kind(new_mode),
            },
            Some('M') => {
                let p = path()?;
                let kind = modified_kind(old_mode, new_mode, &p);
                FileChange { path: p, kind }
            }
            // T (type change) and U (unmerged) are never plain edits.
            Some('T' | 'U') => FileChange {
                path: path()?,
                kind: ChangeKind::TypeChanged,
            },
            Some('D') => FileChange {
                path: path()?,
                kind: ChangeKind::Deleted,
            },
            Some('R') => {
                let from = path()?;
                let to = path()?;
                FileChange {
                    path: to,
                    kind: ChangeKind::Renamed { from },
                }
            }
            // Copies are not requested (-M only); a copy is a new file.
            Some('C') => {
                let _from = path()?;
                FileChange {
                    path: path()?,
                    kind: added_kind(new_mode),
                }
            }
            _ => bail!("git raw diff: unknown status `{status}`"),
        };
        out.push(change);
    }
    Ok(out)
}

/// Decodes a git C-quoted path (`"a/q\"t\303\251.rs"`), or returns an unquoted one as is.
pub(crate) fn unquote_path(s: &str) -> String {
    let s = s.trim_end_matches('\t');
    let Some(inner) = s.strip_prefix('"').and_then(|r| r.strip_suffix('"')) else {
        return s.to_string();
    };
    let b = inner.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' || i + 1 >= b.len() {
            out.push(b[i]);
            i += 1;
            continue;
        }
        i += 1;
        match b[i] {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'a' => out.push(7),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'v' => out.push(11),
            b'0'..=b'3'
                if i + 2 < b.len() && b[i + 1].is_ascii_digit() && b[i + 2].is_ascii_digit() =>
            {
                let v = (b[i] - b'0') * 64 + (b[i + 1] - b'0') * 8 + (b[i + 2] - b'0');
                out.push(v);
                i += 2;
            }
            other => out.push(other),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The path in a `--- ` / `+++ ` header, without its `a/` or `b/` prefix; `None` for /dev/null.
fn header_path(rest: &str) -> Option<String> {
    let p = unquote_path(rest);
    if p == "/dev/null" {
        return None;
    }
    let p = p
        .strip_prefix("a/")
        .or_else(|| p.strip_prefix("b/"))
        .unwrap_or(&p);
    Some(normalize_path(p))
}

/// Parses `git diff -U0 --src-prefix=a/ --dst-prefix=b/` output into one [`Hunk`] per file.
/// Header lines are only recognised before a file's first `@@`; after that, a line starting
/// with `---` or `+++` is content. `\ No newline at end of file` markers are skipped. Files
/// with no text changes (pure renames, mode changes, binaries) produce no hunk.
fn parse_diff(text: &str) -> Vec<Hunk> {
    let mut out: Vec<Hunk> = Vec::new();
    let mut in_hunk = false;
    let mut minus: Option<String> = None;
    let mut cur: Option<usize> = None;

    for line in text.lines() {
        if line.starts_with("diff --git ") {
            in_hunk = false;
            minus = None;
            cur = None;
            continue;
        }
        if !in_hunk {
            if let Some(rest) = line.strip_prefix("--- ") {
                minus = header_path(rest);
            } else if let Some(rest) = line.strip_prefix("+++ ") {
                if let Some(path) = header_path(rest).or_else(|| minus.take()) {
                    cur = Some(match out.iter().position(|h| h.path == path) {
                        Some(i) => i,
                        None => {
                            out.push(Hunk {
                                path,
                                added: Vec::new(),
                                removed: Vec::new(),
                            });
                            out.len() - 1
                        }
                    });
                }
            } else if line.starts_with("@@") {
                in_hunk = true;
            }
            continue;
        }
        let Some(i) = cur else { continue };
        if let Some(l) = line.strip_prefix('+') {
            out[i].added.push(l.to_string());
        } else if let Some(l) = line.strip_prefix('-') {
            out[i].removed.push(l.to_string());
        }
    }
    out.retain(|h| !h.added.is_empty() || !h.removed.is_empty());
    out
}

/// Files changed on `head` since its merge-base with `base` (`git diff base...head`), with
/// renames detected (`-M`) and file modes read from `--raw`, plus the added and removed text
/// lines per file (`-U0 -a`). Prefer [`verdict`], which pins the revisions first.
pub(crate) fn collect_changes(
    repo: &Path,
    base: &str,
    head: &str,
) -> Result<(Vec<FileChange>, Vec<Hunk>)> {
    check_rev(base)?;
    check_rev(head)?;
    let range = format!("{base}...{head}");
    let names = git_ok(
        repo,
        &[
            "-c",
            "core.quotepath=off",
            "diff",
            "--no-ext-diff",
            "--no-relative",
            "-z",
            "--raw",
            "-M",
            &range,
            "--",
        ],
    )?;
    let patch = git_ok(
        repo,
        &[
            "-c",
            "core.quotepath=off",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-relative",
            // `-a`: diff as text even when .gitattributes says `-diff` or the file looks binary,
            // so a repository attribute cannot hide added lines from the warning scan.
            "-a",
            "--no-color",
            "-U0",
            "-M",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            &range,
            "--",
        ],
    )?;
    Ok((
        parse_raw(&names)?,
        parse_diff(&String::from_utf8_lossy(&patch)),
    ))
}

/// Every path in the tree of `rev` (`git ls-tree -r`), normalised.
fn tree_paths(repo: &Path, rev: &str) -> Result<Vec<String>> {
    let out = git_ok(repo, &["ls-tree", "-r", "--name-only", "-z", rev, "--"])?;
    Ok(String::from_utf8_lossy(&out)
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(normalize_path)
        .collect())
}

/// Blockers for new paths that differ only in letter case from another path in the tree, where
/// either of the two is protected. On a case-insensitive checkout (NTFS) such a path can replace
/// the real file. `tree` is every path of the head tree, which includes the new path itself and
/// any colliding sibling.
fn case_collisions(protected: &Protected, changes: &[FileChange], tree: &[String]) -> Vec<Blocker> {
    let mut by_lower: std::collections::HashMap<String, Vec<&str>> = Default::default();
    for p in tree {
        by_lower
            .entry(p.to_lowercase())
            .or_default()
            .push(p.as_str());
    }
    let mut out = Vec::new();
    for c in changes {
        let new_path = match c.kind {
            ChangeKind::Added | ChangeKind::AddedLink | ChangeKind::Renamed { .. } => {
                normalize_path(&c.path)
            }
            _ => continue,
        };
        let Some(siblings) = by_lower.get(&new_path.to_lowercase()) else {
            continue;
        };
        for other in siblings.iter().filter(|o| **o != new_path) {
            if protected.is_protected(&new_path) || protected.is_protected(other) {
                out.push(Blocker::ProtectedChanged {
                    path: c.path.clone(),
                    kind: format!("given a name that differs only in letter case from `{other}`"),
                });
            }
        }
    }
    out
}

/// Resolves `rev` to a full commit SHA.
///
/// A full ref name (`refs/...`) resolves to exactly that ref, through `git show-ref --verify`:
/// git's short-name lookup would also try `refs/refs/...` and friends, so a local ref could stand
/// in for it. Anything else (a SHA, a branch name) goes through `git rev-parse`.
pub(crate) fn resolve_commit(repo: &Path, rev: &str) -> Result<String> {
    check_rev(rev)?;
    let target = if rev.starts_with("refs/") {
        let out = git_ok(repo, &["show-ref", "--verify", "--hash", rev])
            .with_context(|| format!("unknown ref `{rev}`"))?;
        String::from_utf8_lossy(&out).trim().to_string()
    } else {
        rev.to_string()
    };
    let out = git_ok(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{target}^{{commit}}"),
        ],
    )
    .with_context(|| format!("unknown revision `{rev}`"))?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// The union of the protected-path lists committed at `base` and at `head`, so a change cannot
/// shrink the list it is judged by. A side without the file (a first-run change that introduces
/// it) contributes nothing; if neither side has it, that is an error.
fn protected_union(repo: &Path, base: &str, head: &str) -> Result<Protected> {
    let texts: Vec<String> = [base, head]
        .iter()
        .map(|rev| file_at(repo, rev, PROTECTED_PATHS_FILE))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();
    if texts.is_empty() {
        bail!("{PROTECTED_PATHS_FILE} exists at neither {base} nor {head}");
    }
    Protected::parse(&texts.join("\n"))
}

/// The guard verdict for `head` against `base`: the one call `lil dev guard`, `lil dev merge`,
/// the git hooks and CI make.
///
/// It resolves both revisions to SHAs once and uses only those afterwards, so a branch that
/// moves mid-check cannot change what is judged. The protected patterns are the union of the
/// base and head lists. Changes come from `git diff base...head` (merge-base), and expected-fail
/// additions are read from the merge-base and the head.
pub fn verdict(repo: &Path, base: &str, head: &str) -> Result<Verdict> {
    let base = resolve_commit(repo, base)?;
    let head = resolve_commit(repo, head)?;
    let protected = protected_union(repo, &base, &head)?;
    let (changes, hunks) = collect_changes(repo, &base, &head)?;
    let (expected_fail_added, _removed) = expected_fail_changes(repo, &base, &head)?;
    let spec_files = spec_files_added(&changes);
    let mut verdict = evaluate(
        &protected,
        &changes,
        &hunks,
        &expected_fail_added,
        &spec_files,
    );
    let tree = tree_paths(repo, &head)?;
    verdict
        .needs_approval
        .extend(case_collisions(&protected, &changes, &tree));
    Ok(verdict)
}

/// Characters that can change what a terminal or a rendered page shows around them (rulings R23
/// and R38): every control character (ESC, CR, LF, BEL, ...), the bidirectional embeddings,
/// overrides and isolates (U+202A-U+202E, U+2066-U+2069), the zero-width characters and
/// direction marks (U+200B-U+200F), the line and paragraph separators (U+2028, U+2029), the
/// byte-order mark (U+FEFF), the soft hyphen (U+00AD) and the Arabic letter mark (U+061C).
/// The one list every escape and every path check uses.
pub(crate) fn is_dangerous_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{202A}'..='\u{202E}'
                | '\u{2066}'..='\u{2069}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{FEFF}'
                | '\u{00AD}'
                | '\u{061C}'
        )
}

/// `s` with every [dangerous character](is_dangerous_char) written as a Rust escape (`\u{1b}`,
/// `\r`, `\u{202e}`, ...), so a path or line can't move the cursor, erase what a terminal
/// showed around it, or reorder or hide text (rulings R23 and R38). Everything else, including
/// non-ASCII letters, is kept.
pub(crate) fn escape_controls(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_control() {
            out.extend(c.escape_debug());
        } else if is_dangerous_char(c) {
            out.push_str(&format!("\\u{{{:x}}}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

/// Plain-language lines for reports: one line per blocker, then one per warning. Control
/// characters in paths and lines are escaped ([`escape_controls`]).
pub fn render_plain(v: &Verdict) -> String {
    if v.is_clean() {
        return "Nothing needs approval and nothing looks suspicious.".to_string();
    }
    let mut lines = Vec::new();
    for b in &v.needs_approval {
        lines.push(escape_controls(&match b {
            Blocker::ProtectedChanged { path, kind } => format!(
                "Needs your approval: the protected file `{path}` was {kind}."
            ),
            Blocker::ShortNameAlias { path, component } => format!(
                "Needs your approval: `{path}` has a part, `{component}`, that looks like a Windows 8.3 short name. On Windows it can write through to a different, real file."
            ),
            Blocker::ControlCharsInPath { path } => format!(
                "Needs your approval: `{path}`: path contains control or invisible characters."
            ),
            Blocker::ExpectedFailAdditionWithoutNewTest { name } => format!(
                "Needs your approval: `{name}` was added to the expected-failure list without a new spec test file in the same change."
            ),
        }));
    }
    for w in &v.warnings {
        lines.push(escape_controls(&match w {
            Warning::RemovedTest { path, line } => {
                format!("Warning: a test was removed in `{path}`: {line}")
            }
            Warning::IgnoredTest { path, line } => {
                format!("Warning: a test was switched off in `{path}`: {line}")
            }
            Warning::NewSleepOrRetry { path, line } => {
                format!("Warning: a sleep or retry was added to tests in `{path}`: {line}")
            }
            Warning::NewLintSuppression { path, line } => {
                format!("Warning: a lint was suppressed in `{path}`: {line}")
            }
        }));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// The real protected-path list, so the tests cover the file as shipped.
    fn protected() -> Protected {
        Protected::parse(include_str!("../../../../dev/protected-paths.txt")).unwrap()
    }

    fn change(path: &str, kind: ChangeKind) -> FileChange {
        FileChange {
            path: path.into(),
            kind,
        }
    }

    fn renamed(from: &str, to: &str) -> FileChange {
        change(to, ChangeKind::Renamed { from: from.into() })
    }

    fn ef(name: &str, file: &str) -> spec_tests::ExpectedFail {
        spec_tests::ExpectedFail {
            name: name.into(),
            rule: "QA-017".into(),
            file: file.into(),
        }
    }

    fn hunk(path: &str, added: &[&str], removed: &[&str]) -> Hunk {
        Hunk {
            path: path.into(),
            added: added.iter().map(|s| s.to_string()).collect(),
            removed: removed.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn eval(changes: &[FileChange]) -> Verdict {
        evaluate(&protected(), changes, &[], &[], &[])
    }

    fn pc(path: &str, kind: &str) -> Blocker {
        Blocker::ProtectedChanged {
            path: path.into(),
            kind: kind.into(),
        }
    }

    fn names(v: &[spec_tests::ExpectedFail]) -> Vec<String> {
        v.iter().map(|e| e.name.clone()).collect()
    }

    const SPEC_TEST: &str = "crates/lil-spec-tests/tests/foo.rs";

    #[test]
    fn added_protected_file_is_allowed() {
        let v = eval(&[change(SPEC_TEST, ChangeKind::Added)]);
        assert!(v.needs_approval.is_empty(), "{v:?}");
        assert!(v.is_clean());
    }

    #[test]
    fn modified_protected_file_needs_approval() {
        let v = eval(&[change(SPEC_TEST, ChangeKind::Modified)]);
        assert_eq!(v.needs_approval, vec![pc(SPEC_TEST, "modified")]);
    }

    #[test]
    fn deleted_protected_file_needs_approval() {
        let v = eval(&[change(SPEC_TEST, ChangeKind::Deleted)]);
        assert_eq!(v.needs_approval, vec![pc(SPEC_TEST, "deleted")]);
    }

    #[test]
    fn rename_out_of_protected_needs_approval() {
        let v = eval(&[renamed(SPEC_TEST, "crates/lil-core/tests/foo.rs")]);
        assert_eq!(
            v.needs_approval,
            vec![pc(
                "crates/lil-core/tests/foo.rs",
                &format!("renamed from {SPEC_TEST}")
            )]
        );
    }

    #[test]
    fn rename_within_protected_needs_approval() {
        let to = "crates/lil-spec-tests/tests/sub/foo.rs";
        let v = eval(&[renamed(SPEC_TEST, to)]);
        assert_eq!(v.needs_approval.len(), 1, "{v:?}");
    }

    #[test]
    fn rename_into_protected_needs_approval() {
        let v = eval(&[renamed("crates/lil-core/tests/foo.rs", SPEC_TEST)]);
        assert_eq!(v.needs_approval.len(), 1, "{v:?}");
    }

    #[test]
    fn unprotected_change_is_clean() {
        let v = eval(&[
            change("crates/lil-core/src/lib.rs", ChangeKind::Modified),
            change("README.md", ChangeKind::Deleted),
            renamed("docs/a.md", "docs/b.md"),
            change("crates/lil-core/src/new.rs", ChangeKind::Added),
        ]);
        assert!(v.is_clean(), "{v:?}");
    }

    #[test]
    fn guard_files_are_protected() {
        let p = protected();
        for path in [
            "tools/git-hooks/pre-push",
            "crates/lil-cli/src/dev/guard.rs",
            "dev/protected-paths.txt",
            "dev/merge-guard.toml",
            ".github/workflows/ci-guard.yml",
            ".github/workflows/post-merge.yml",
            // R21: every workflow, so a PR can't add one that posts a same-named check run.
            ".github/workflows/ci-push.yml",
            ".github/workflows/forged-check.yml",
            ".config/nextest.toml",
            "test-params.toml",
            "crates/lil-dev-guard/src/lib.rs",
            // R18: what controls how the hooks' `cargo run` builds and runs.
            ".cargo/config.toml",
            "Cargo.toml",
            "crates/lil-cli/Cargo.toml",
            "crates/lil-cli/build.rs",
            "crates/lil-cli/src/main.rs",
            "rust-toolchain.toml",
            ".gitattributes",
        ] {
            assert!(p.is_protected(path), "{path} must be protected");
        }
        for path in [
            "Cargo.lock",
            "crates/lil-core/Cargo.toml",
            "crates/lil-cli/src/other.rs",
            "dev/other.txt",
            ".github/ISSUE_TEMPLATE/bug.md",
            "crates/lil-spec-tests-extra/x.rs",
        ] {
            assert!(!p.is_protected(path), "{path} must not be protected");
        }
    }

    #[test]
    fn protection_matches_at_any_depth_and_normalises_separators() {
        let p = protected();
        assert!(p.is_protected("crates/lil-spec-tests/a/b/c/d.txt"));
        assert!(p.is_protected("crates/lil-spec-tests/expected-fail.toml"));
        assert!(p.is_protected("crates/lil-spec-tests\\tests\\foo.rs"));
        assert!(p.is_protected("./crates/lil-spec-tests/tests/foo.rs"));
        assert!(p.is_protected("crates/lil-core/proptest-regressions/x.txt"));
        assert!(p.is_protected("proptest-regressions/x.txt"));
        // Windows file names are case-insensitive, so a case change must not slip through.
        assert!(p.is_protected("Crates/LIL-Spec-Tests/tests/foo.rs"));
    }

    #[test]
    fn pattern_parsing_rules() {
        let p =
            Protected::parse("# comment\n\n  /rooted/*.txt  \nname.toml\nsome/dir/\r\n").unwrap();
        assert!(p.is_protected("rooted/a.txt"));
        assert!(!p.is_protected("sub/rooted/a.txt"));
        assert!(p.is_protected("name.toml"));
        assert!(p.is_protected("deep/er/name.toml"));
        assert!(p.is_protected("some/dir/x/y.rs"));
        assert!(!p.is_protected("some/other.rs"));
        // Fail closed on lists we cannot honour.
        assert!(Protected::parse("# nothing here\n").is_err());
        assert!(Protected::parse("!negated\nfoo\n").is_err());
    }

    #[test]
    fn expected_fail_addition_needs_matching_new_test() {
        let toml_change = change(EXPECTED_FAIL_PATH, ChangeKind::Modified);
        let added = [ef("lil-spec-tests::foo::bar", "tests/foo.rs")];
        let spec_files = [SPEC_TEST.to_string()];

        // Matching new test in the same change: allowed, and the toml edit is not a blocker.
        let ok = evaluate(
            &protected(),
            &[toml_change.clone(), change(SPEC_TEST, ChangeKind::Added)],
            &[],
            &added,
            &spec_files,
        );
        assert!(ok.needs_approval.is_empty(), "{ok:?}");

        // No new spec file at all.
        let none = evaluate(
            &protected(),
            std::slice::from_ref(&toml_change),
            &[],
            &added,
            &[],
        );
        assert_eq!(
            none.needs_approval,
            vec![Blocker::ExpectedFailAdditionWithoutNewTest {
                name: "lil-spec-tests::foo::bar".into()
            }]
        );

        // A different new spec file does not cover the entry.
        let other = evaluate(
            &protected(),
            std::slice::from_ref(&toml_change),
            &[],
            &added,
            &["crates/lil-spec-tests/tests/other.rs".to_string()],
        );
        assert_eq!(other.needs_approval.len(), 1, "{other:?}");

        // `file` may not escape the crate with `..` to reach an added file elsewhere.
        let sneaky = evaluate(
            &protected(),
            &[toml_change],
            &[],
            &[ef("n", "../lil-core/tests/x.rs")],
            &["crates/lil-core/tests/x.rs".to_string()],
        );
        assert_eq!(sneaky.needs_approval.len(), 1, "{sneaky:?}");
    }

    #[test]
    fn expected_fail_file_is_relative_to_the_spec_crate_and_normalised() {
        let added = [ef("lil-spec-tests::foo::a", "./tests\\foo.rs")];
        let v = evaluate(&protected(), &[], &[], &added, &[SPEC_TEST.to_string()]);
        assert!(v.needs_approval.is_empty(), "{v:?}");
    }

    #[test]
    fn expected_fail_removal_is_free() {
        let (added, removed) = expected_fail_diff(
            r#"tests = [ { name = "a::b::c", rule = "QA-017", file = "tests/x.rs" } ]"#,
            "tests = []",
        )
        .unwrap();
        assert!(added.is_empty());
        assert_eq!(removed.len(), 1);
        let v = evaluate(
            &protected(),
            &[change(EXPECTED_FAIL_PATH, ChangeKind::Modified)],
            &[],
            &added,
            &[],
        );
        assert!(v.is_clean(), "{v:?}");
    }

    #[test]
    fn expected_fail_file_itself_is_still_protected_against_delete_and_rename() {
        let del = eval(&[change(EXPECTED_FAIL_PATH, ChangeKind::Deleted)]);
        assert_eq!(del.needs_approval.len(), 1);
        let ren = eval(&[renamed(EXPECTED_FAIL_PATH, "docs/expected-fail.toml")]);
        assert_eq!(ren.needs_approval.len(), 1);
    }

    #[test]
    fn expected_fail_diff_compares_by_name() {
        let old = r#"tests = [
            { name = "a", rule = "QA-1", file = "tests/a.rs" },
            { name = "b", rule = "QA-2", file = "tests/b.rs" },
            { name = "c", rule = "QA-3", file = "tests/c.rs" },
        ]"#;
        let new = r#"tests = [
            { name = "a", rule = "QA-1", file = "tests/a.rs" },
            { name = "c", rule = "QA-3", file = "tests/elsewhere.rs" },
            { name = "d", rule = "QA-4", file = "tests/d.rs" },
        ]"#;
        let (added, removed) = expected_fail_diff(old, new).unwrap();
        // `c` kept its name but now points at a different file: that counts as an addition.
        assert_eq!(names(&added), vec!["c", "d"]);
        assert_eq!(names(&removed), vec!["b"]);
    }

    #[test]
    fn expected_fail_diff_handles_empty_and_bad_input() {
        let (a, r) = expected_fail_diff("", "").unwrap();
        assert!(a.is_empty() && r.is_empty());
        let (a, r) =
            expected_fail_diff("", r#"tests = [{ name = "x", rule = "r", file = "f" }]"#).unwrap();
        assert_eq!((a.len(), r.len()), (1, 0));
        assert!(expected_fail_diff("tests = [", "").is_err());
        assert!(expected_fail_diff("", "bogus = 1").is_err());
    }

    #[test]
    fn warns_on_removed_ignored_sleep_and_allow() {
        let path = "crates/lil-core/tests/store.rs";
        let hunks = [
            hunk(
                path,
                &[
                    "    #[ignore = \"flaky\"]",
                    "    std::thread::sleep(Duration::from_millis(50));",
                    "    retry_until_ready();",
                    "#![allow(dead_code)]",
                    "#[allow(clippy::too_many_arguments)]",
                    "#[expect(unused)]",
                    "let fine = 1;",
                ],
                &[
                    "    #[test]",
                    "    #[tokio::test(flavor = \"multi_thread\")]",
                    "    proptest! {",
                    "    let untouched = 2;",
                ],
            ),
            // Non-Rust files are not scanned.
            hunk("docs/notes.md", &["#[ignore]", "sleep("], &["#[test]"]),
        ];
        let v = evaluate(&protected(), &[], &hunks, &[], &[]);
        assert!(v.needs_approval.is_empty());
        let p = || path.to_string();
        assert_eq!(
            v.warnings,
            vec![
                Warning::RemovedTest {
                    path: p(),
                    line: "#[test]".into()
                },
                Warning::RemovedTest {
                    path: p(),
                    line: "#[tokio::test(flavor = \"multi_thread\")]".into()
                },
                Warning::RemovedTest {
                    path: p(),
                    line: "proptest! {".into()
                },
                Warning::IgnoredTest {
                    path: p(),
                    line: "#[ignore = \"flaky\"]".into()
                },
                Warning::NewSleepOrRetry {
                    path: p(),
                    line: "std::thread::sleep(Duration::from_millis(50));".into()
                },
                Warning::NewSleepOrRetry {
                    path: p(),
                    line: "retry_until_ready();".into()
                },
                Warning::NewLintSuppression {
                    path: p(),
                    line: "#![allow(dead_code)]".into()
                },
                Warning::NewLintSuppression {
                    path: p(),
                    line: "#[allow(clippy::too_many_arguments)]".into()
                },
                Warning::NewLintSuppression {
                    path: p(),
                    line: "#[expect(unused)]".into()
                },
            ]
        );
    }

    #[test]
    fn sleep_and_retry_only_warn_in_test_files() {
        let src = hunk(
            "crates/lil-core/src/lib.rs",
            &["sleep(d);", "retry();"],
            &[],
        );
        assert!(
            evaluate(&protected(), &[], &[src], &[], &[])
                .warnings
                .is_empty()
        );

        // A file that declares `mod tests` in the diff counts as a test file.
        let inline = hunk(
            "crates/lil-core/src/lib.rs",
            &["mod tests {", "sleep(d);"],
            &[],
        );
        let w = evaluate(&protected(), &[], &[inline], &[], &[]).warnings;
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(matches!(w[0], Warning::NewSleepOrRetry { .. }));

        // `mod tests` in a separate hunk of the same file still counts.
        let split = [
            hunk("crates/lil-core/src/lib.rs", &["mod tests {"], &[]),
            hunk("crates/lil-core/src/lib.rs", &["sleep(d);"], &[]),
        ];
        assert_eq!(
            evaluate(&protected(), &[], &split, &[], &[]).warnings.len(),
            1
        );
    }

    #[test]
    fn attribute_matching_ignores_whitespace() {
        let h = hunk(
            "crates/lil-core/src/lib.rs",
            &["#[ allow ( dead_code ) ]", "#[ignore]"],
            &["#[ test ]"],
        );
        let v = evaluate(&protected(), &[], &[h], &[], &[]);
        assert_eq!(v.warnings.len(), 3, "{v:?}");
    }

    #[test]
    fn render_plain_is_readable() {
        assert!(render_plain(&Verdict::default()).contains("Nothing needs approval"));
        let v = Verdict {
            needs_approval: vec![
                pc(SPEC_TEST, "modified"),
                Blocker::ExpectedFailAdditionWithoutNewTest {
                    name: "t::n".into(),
                },
            ],
            warnings: vec![Warning::IgnoredTest {
                path: "a.rs".into(),
                line: "#[ignore]".into(),
            }],
        };
        let text = render_plain(&v);
        assert!(
            text.contains(SPEC_TEST) && text.contains("modified"),
            "{text}"
        );
        assert!(text.contains("t::n"), "{text}");
        assert!(
            text.contains("a.rs") && text.contains("#[ignore]"),
            "{text}"
        );
        assert_eq!(text.lines().count(), 3, "{text}");
    }

    fn raw(entries: &[(&str, &str, &str, &[&str])]) -> Vec<u8> {
        let mut out = String::new();
        for (old_mode, new_mode, status, paths) in entries {
            out.push_str(&format!(
                ":{old_mode} {new_mode} 1111111 2222222 {status}\0"
            ));
            for p in *paths {
                out.push_str(p);
                out.push('\0');
            }
        }
        out.into_bytes()
    }

    #[test]
    fn raw_diff_parsing() {
        let r = raw(&[
            ("000000", "100644", "A", &["new file.rs"]),
            ("100644", "100644", "M", &["crates/a.rs"]),
            ("100755", "100755", "M", &["tools/hook"]),
            ("100644", "100755", "M", &["mode.sh"]),
            ("100644", "120000", "T", &["became-link"]),
            ("120000", "120000", "M", &["link-edited"]),
            ("000000", "120000", "A", &["new-link"]),
            ("000000", "160000", "A", &["new-submodule"]),
            ("100644", "000000", "D", &["gone.rs"]),
            ("100644", "100644", "R100", &["old.rs", "new.rs"]),
            ("100644", "100644", "R087", &["a b.rs", "c d.rs"]),
            ("100644", "100755", "R090", &["x", "y"]),
        ]);
        assert_eq!(
            parse_raw(&r).unwrap(),
            vec![
                change("new file.rs", ChangeKind::Added),
                change("crates/a.rs", ChangeKind::Modified),
                change("tools/hook", ChangeKind::Modified),
                change("mode.sh", ChangeKind::ModeChanged),
                change("became-link", ChangeKind::TypeChanged),
                change("link-edited", ChangeKind::TypeChanged),
                change("new-link", ChangeKind::AddedLink),
                change("new-submodule", ChangeKind::AddedLink),
                change("gone.rs", ChangeKind::Deleted),
                renamed("old.rs", "new.rs"),
                renamed("a b.rs", "c d.rs"),
                renamed("x", "y"),
            ]
        );
        // expected-fail.toml is `Modified` only as a plain 100644 file.
        let plain = raw(&[("100644", "100644", "M", &[EXPECTED_FAIL_PATH])]);
        assert_eq!(parse_raw(&plain).unwrap()[0].kind, ChangeKind::Modified);
        let exec = raw(&[("100755", "100755", "M", &[EXPECTED_FAIL_PATH])]);
        assert_eq!(parse_raw(&exec).unwrap()[0].kind, ChangeKind::ModeChanged);
        assert!(parse_raw(b"").unwrap().is_empty());
        assert!(parse_raw(&raw(&[("100644", "100644", "Q", &["x"])])).is_err());
        assert!(parse_raw(&raw(&[("100644", "100644", "R100", &["only-one"])])).is_err());
        assert!(parse_raw(b"M\0path\0").is_err());
        assert!(parse_raw(b":100644 100644 M\0path\0").is_err());
    }

    #[test]
    fn diff_parsing_handles_edge_cases() {
        // Content lines that look like file headers (`--- ...`, `+++ ...`) must stay content.
        let diff = "diff --git a/src/a.rs b/src/a.rs\n\
index 111..222 100644\n\
--- a/src/a.rs\n\
+++ b/src/a.rs\n\
@@ -3 +3,2 @@ fn x() {\n\
-    #[test]\n\
+    #[ignore]\n\
+    let y = 1;\n\
@@ -10 +11 @@\n\
--- a SQL-style comment\n\
+++ also looks like a header\n\
\\ No newline at end of file\n\
diff --git a/old.rs b/new.rs\n\
similarity index 100%\n\
rename from old.rs\n\
rename to new.rs\n\
diff --git a/gone.rs b/gone.rs\n\
deleted file mode 100644\n\
index 333..000\n\
--- a/gone.rs\n\
+++ /dev/null\n\
@@ -1,2 +0,0 @@\n\
-#[test]\n\
-fn f() {}\n\
diff --git a/with space.rs b/with space.rs\n\
--- a/with space.rs\t\n\
+++ b/with space.rs\t\n\
@@ -1 +1 @@\n\
-old\n\
+new\n\
diff --git a/bin.png b/bin.png\n\
Binary files a/bin.png and b/bin.png differ\n";
        assert_eq!(
            parse_diff(diff),
            vec![
                Hunk {
                    path: "src/a.rs".into(),
                    added: vec![
                        "    #[ignore]".into(),
                        "    let y = 1;".into(),
                        "++ also looks like a header".into()
                    ],
                    removed: vec!["    #[test]".into(), "-- a SQL-style comment".into()],
                },
                Hunk {
                    path: "gone.rs".into(),
                    added: vec![],
                    removed: vec!["#[test]".into(), "fn f() {}".into()],
                },
                Hunk {
                    path: "with space.rs".into(),
                    added: vec!["new".into()],
                    removed: vec!["old".into()],
                },
            ]
        );
    }

    #[test]
    fn diff_parsing_unquotes_c_style_paths() {
        let diff = "diff --git \"a/q\\\"t\\303\\251.rs\" \"b/q\\\"t\\303\\251.rs\"\n\
--- \"a/q\\\"t\\303\\251.rs\"\n\
+++ \"b/q\\\"t\\303\\251.rs\"\n\
@@ -1 +1 @@\n\
-a\n\
+b\n";
        let got = parse_diff(diff);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, "q\"t\u{e9}.rs");
    }

    // ---- integration: real git, temp repo, no network ----

    fn git_in(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .output()
            .expect("git must be installed");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn write(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn temp_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        git_in(d, &["-c", "init.defaultBranch=main", "init", "-q"]);
        git_in(d, &["config", "user.name", "Guard Test"]);
        git_in(d, &["config", "user.email", "guard@test.invalid"]);
        git_in(d, &["config", "commit.gpgsign", "false"]);
        git_in(d, &["config", "core.autocrlf", "false"]);
        dir
    }

    fn commit_all(d: &Path, msg: &str) {
        git_in(d, &["add", "-A"]);
        git_in(d, &["commit", "-q", "-m", msg]);
    }

    #[test]
    fn collect_changes_in_temp_repo() {
        let dir = temp_repo();
        let d = dir.path();
        write(
            d,
            "dev/protected-paths.txt",
            "add-ok:crates/lil-spec-tests/**\n",
        );
        write(
            d,
            "crates/lil-spec-tests/tests/foo.rs",
            "#[test]\nfn a() {}\n",
        );
        write(d, "crates/lil-core/src/lib.rs", "pub fn f() {}\n");
        commit_all(d, "base");

        git_in(d, &["checkout", "-q", "-b", "feature"]);
        write(d, "crates/lil-spec-tests/tests/foo.rs", "fn a() {}\n");
        commit_all(d, "weaken the test");

        let (changes, hunks) = collect_changes(d, "main", "feature").unwrap();
        assert_eq!(
            changes,
            vec![change(
                "crates/lil-spec-tests/tests/foo.rs",
                ChangeKind::Modified
            )]
        );
        assert_eq!(
            hunks,
            vec![Hunk {
                path: "crates/lil-spec-tests/tests/foo.rs".into(),
                added: vec![],
                removed: vec!["#[test]".into()],
            }]
        );

        let protected =
            Protected::parse(&std::fs::read_to_string(d.join(PROTECTED_PATHS_FILE)).unwrap())
                .unwrap();
        let v = evaluate(&protected, &changes, &hunks, &[], &[]);
        assert_eq!(v.needs_approval.len(), 1, "{v:?}");
        assert_eq!(v.warnings.len(), 1, "{v:?}");
    }

    #[test]
    fn collect_changes_uses_merge_base_and_detects_renames_adds_and_deletes() {
        let dir = temp_repo();
        let d = dir.path();
        write(
            d,
            "crates/lil-spec-tests/tests/foo.rs",
            "line one\nline two\nline three\nline four\n",
        );
        write(d, "crates/lil-spec-tests/tests/gone.rs", "x\n");
        commit_all(d, "base");

        // main moves on after the branch point; that must not show up in the diff.
        git_in(d, &["checkout", "-q", "-b", "feature"]);
        git_in(d, &["checkout", "-q", "main"]);
        write(d, "main-only.txt", "m\n");
        commit_all(d, "main moves on");

        git_in(d, &["checkout", "-q", "feature"]);
        std::fs::create_dir_all(d.join("crates/lil-core/tests")).unwrap();
        git_in(
            d,
            &[
                "mv",
                "crates/lil-spec-tests/tests/foo.rs",
                "crates/lil-core/tests/foo.rs",
            ],
        );
        git_in(d, &["rm", "-q", "crates/lil-spec-tests/tests/gone.rs"]);
        write(d, "new file.rs", "fn n() { sleep(d); }\n");
        commit_all(d, "rename, delete, add");

        let (mut changes, hunks) = collect_changes(d, "main", "feature").unwrap();
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(
            changes,
            vec![
                renamed(
                    "crates/lil-spec-tests/tests/foo.rs",
                    "crates/lil-core/tests/foo.rs"
                ),
                change("crates/lil-spec-tests/tests/gone.rs", ChangeKind::Deleted),
                change("new file.rs", ChangeKind::Added),
            ]
        );
        assert!(
            hunks
                .iter()
                .any(|h| h.path == "new file.rs" && h.added == ["fn n() { sleep(d); }"])
        );
        assert!(hunks.iter().all(|h| h.path != "main-only.txt"));

        let p = Protected::parse("crates/lil-spec-tests/**\n").unwrap();
        let v = evaluate(&p, &changes, &hunks, &[], &[]);
        assert_eq!(v.needs_approval.len(), 2, "{v:?}");
    }

    #[test]
    fn collect_changes_reports_mode_only_change_as_mode_changed() {
        let dir = temp_repo();
        let d = dir.path();
        write(d, "tools/git-hooks/pre-push", "#!/bin/sh\n");
        git_in(d, &["add", "-A"]);
        git_in(
            d,
            &["update-index", "--chmod=-x", "tools/git-hooks/pre-push"],
        );
        git_in(d, &["commit", "-q", "-m", "base"]);
        git_in(d, &["checkout", "-q", "-b", "feature"]);
        git_in(
            d,
            &["update-index", "--chmod=+x", "tools/git-hooks/pre-push"],
        );
        git_in(d, &["commit", "-q", "-m", "mode"]);
        let (changes, hunks) = collect_changes(d, "main", "feature").unwrap();
        assert_eq!(
            changes,
            vec![change("tools/git-hooks/pre-push", ChangeKind::ModeChanged)]
        );
        assert!(hunks.is_empty());
    }

    #[test]
    fn collect_changes_rejects_bad_revisions() {
        let dir = temp_repo();
        let d = dir.path();
        write(d, "a.txt", "a\n");
        commit_all(d, "base");
        assert!(collect_changes(d, "main", "nope").is_err());
        assert!(collect_changes(d, "--output=x", "main").is_err());
    }

    #[test]
    fn expected_fail_changes_reads_base_and_head_from_git() {
        let dir = temp_repo();
        let d = dir.path();
        write(
            d,
            EXPECTED_FAIL_PATH,
            r#"tests = [ { name = "old::one", rule = "QA-1", file = "tests/a.rs" } ]"#,
        );
        commit_all(d, "base");
        git_in(d, &["checkout", "-q", "-b", "feature"]);
        write(
            d,
            EXPECTED_FAIL_PATH,
            r#"tests = [ { name = "new::two", rule = "QA-2", file = "tests/b.rs" } ]"#,
        );
        commit_all(d, "swap");
        let (added, removed) = expected_fail_changes(d, "main", "feature").unwrap();
        assert_eq!(
            added,
            vec![spec_tests::ExpectedFail {
                name: "new::two".into(),
                rule: "QA-2".into(),
                file: "tests/b.rs".into()
            }]
        );
        assert_eq!(names(&removed), vec!["old::one"]);

        // The file missing on either side reads as an empty list.
        git_in(d, &["checkout", "-q", "main"]);
        git_in(d, &["checkout", "-q", "-b", "other"]);
        git_in(d, &["rm", "-q", EXPECTED_FAIL_PATH]);
        git_in(d, &["commit", "-q", "-m", "drop"]);
        let (a, r) = expected_fail_changes(d, "main", "other").unwrap();
        assert!(a.is_empty());
        assert_eq!(r.len(), 1);

        assert_eq!(file_at(d, "main", "nope.txt").unwrap(), None);
        assert!(file_at(d, "no-such-rev", EXPECTED_FAIL_PATH).is_err());
    }

    #[test]
    fn spec_files_added_lists_only_added_spec_crate_files() {
        let changes = [
            change(SPEC_TEST, ChangeKind::Added),
            change("crates/lil-spec-tests/tests/mod.rs", ChangeKind::Modified),
            change("crates/lil-core/tests/x.rs", ChangeKind::Added),
            renamed("a.rs", "crates/lil-spec-tests/tests/r.rs"),
        ];
        assert_eq!(spec_files_added(&changes), vec![SPEC_TEST.to_string()]);
    }

    // ---- fix round 1: binding, symlinks, attributes, verdict ----

    fn spec_added(rel: &str) -> String {
        format!("crates/lil-spec-tests/{rel}")
    }

    #[test]
    fn existing_test_name_with_dummy_file_blocks() {
        // An existing test's name, pointed at an empty dummy file that the change adds.
        let added = [ef(
            "lil-spec-tests::smoke::spec_harness_runs",
            "tests/dummy.rs",
        )];
        let v = evaluate(
            &protected(),
            &[change(EXPECTED_FAIL_PATH, ChangeKind::Modified)],
            &[],
            &added,
            &[spec_added("tests/dummy.rs")],
        );
        assert_eq!(
            v.needs_approval,
            vec![Blocker::ExpectedFailAdditionWithoutNewTest {
                name: "lil-spec-tests::smoke::spec_harness_runs".into()
            }]
        );
        // Naming the real file does not help: smoke.rs already exists, so it is not added.
        let real = [ef(
            "lil-spec-tests::smoke::spec_harness_runs",
            "tests/smoke.rs",
        )];
        let v = evaluate(
            &protected(),
            &[],
            &[],
            &real,
            &[spec_added("tests/dummy.rs")],
        );
        assert_eq!(v.needs_approval.len(), 1, "{v:?}");
    }

    #[test]
    fn entry_for_new_test_binary_is_allowed() {
        let one = [ef(
            "lil-spec-tests::newbin::deep::module::t",
            "tests/newbin.rs",
        )];
        let v = evaluate(
            &protected(),
            &[],
            &[],
            &one,
            &[spec_added("tests/newbin.rs")],
        );
        assert!(v.needs_approval.is_empty(), "{v:?}");
        let dir = [ef("lil-spec-tests::newbin::t", "tests/newbin/main.rs")];
        let v = evaluate(
            &protected(),
            &[],
            &[],
            &dir,
            &[spec_added("tests/newbin/main.rs")],
        );
        assert!(v.needs_approval.is_empty(), "{v:?}");
    }

    #[test]
    fn entry_whose_file_mismatches_its_binary_blocks() {
        let both_new = [spec_added("tests/newbin.rs"), spec_added("tests/other.rs")];
        // The file is new, but it is not the binary the name belongs to.
        let wrong_file = [ef("lil-spec-tests::newbin::t", "tests/other.rs")];
        let v = evaluate(&protected(), &[], &[], &wrong_file, &both_new);
        assert_eq!(v.needs_approval.len(), 1, "{v:?}");
        // Names that do not have the `lil-spec-tests::<bin>::<test>` shape need approval.
        for name in [
            "bare",
            "lil-spec-tests::newbin",
            "lil-spec-tests::newbin::",
            "lil-spec-tests::::t",
            "other-crate::newbin::t",
            "lil-spec-tests::../newbin::t",
            "lil-spec-tests::a/b::t",
        ] {
            let e = [ef(name, "tests/newbin.rs")];
            let v = evaluate(&protected(), &[], &[], &e, &both_new);
            assert_eq!(v.needs_approval.len(), 1, "{name}: {v:?}");
        }
    }

    #[test]
    fn every_non_plain_change_kind_of_a_protected_file_needs_approval() {
        for kind in [
            ChangeKind::ModeChanged,
            ChangeKind::TypeChanged,
            ChangeKind::AddedLink,
        ] {
            for path in [SPEC_TEST, EXPECTED_FAIL_PATH] {
                let v = eval(&[change(path, kind.clone())]);
                assert_eq!(v.needs_approval.len(), 1, "{kind:?} {path}: {v:?}");
            }
            assert!(eval(&[change("README.md", kind.clone())]).is_clean());
        }
    }

    /// Commits `target` as a symlink (mode 120000) at `path`, without needing OS symlinks.
    fn commit_symlink(d: &Path, path: &str, target: &str) {
        write(d, "scratch-blob.tmp", target);
        let sha = git_in(d, &["hash-object", "-w", "scratch-blob.tmp"]);
        std::fs::remove_file(d.join("scratch-blob.tmp")).unwrap();
        git_in(
            d,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("120000,{},{path}", sha.trim()),
            ],
        );
        git_in(d, &["commit", "-q", "-m", "link"]);
    }

    #[test]
    fn symlink_typechange_of_expected_fail_blocks() {
        let dir = temp_repo();
        let d = dir.path();
        write(
            d,
            "dev/protected-paths.txt",
            "add-ok:crates/lil-spec-tests/**\n",
        );
        write(d, EXPECTED_FAIL_PATH, "tests = []\n");
        commit_all(d, "base");
        git_in(d, &["checkout", "-q", "-b", "feature"]);
        // The link target text is itself a valid, empty expected-fail list.
        commit_symlink(d, EXPECTED_FAIL_PATH, "tests = []\n");

        let (changes, _) = collect_changes(d, "main", "feature").unwrap();
        assert_eq!(
            changes,
            vec![change(EXPECTED_FAIL_PATH, ChangeKind::TypeChanged)]
        );
        let v = verdict(d, "main", "feature").unwrap();
        assert_eq!(
            v.needs_approval,
            vec![pc(
                EXPECTED_FAIL_PATH,
                "changed to or from a symlink or other file type"
            )]
        );
    }

    #[test]
    fn added_symlink_under_protected_path_blocks() {
        let dir = temp_repo();
        let d = dir.path();
        write(
            d,
            "dev/protected-paths.txt",
            "add-ok:crates/lil-spec-tests/**\n",
        );
        commit_all(d, "base");
        git_in(d, &["checkout", "-q", "-b", "feature"]);
        commit_symlink(
            d,
            "crates/lil-spec-tests/tests/link.rs",
            "../../lil-core/x.rs",
        );
        let (changes, _) = collect_changes(d, "main", "feature").unwrap();
        assert_eq!(
            changes,
            vec![change(
                "crates/lil-spec-tests/tests/link.rs",
                ChangeKind::AddedLink
            )]
        );
        let v = verdict(d, "main", "feature").unwrap();
        assert_eq!(v.needs_approval.len(), 1, "{v:?}");
        // A symlink is not a new spec test, so it cannot back an expected-fail entry either.
        assert!(spec_files_added(&changes).is_empty());
    }

    #[test]
    fn gitattributes_minus_diff_does_not_hide_warnings() {
        let dir = temp_repo();
        let d = dir.path();
        write(d, ".gitattributes", "*.rs -diff\n");
        write(d, "crates/lil-core/tests/t.rs", "fn a() {}\n");
        commit_all(d, "base");
        git_in(d, &["checkout", "-q", "-b", "feature"]);
        write(
            d,
            "crates/lil-core/tests/t.rs",
            "#[ignore]\n#[allow(dead_code)]\nfn a() {}\n",
        );
        commit_all(d, "ignore the test");
        let (_, hunks) = collect_changes(d, "main", "feature").unwrap();
        assert_eq!(
            hunks,
            vec![Hunk {
                path: "crates/lil-core/tests/t.rs".into(),
                added: vec!["#[ignore]".into(), "#[allow(dead_code)]".into()],
                removed: vec![],
            }]
        );
        let v = evaluate(
            &Protected::parse("nothing/**\n").unwrap(),
            &[],
            &hunks,
            &[],
            &[],
        );
        assert_eq!(v.warnings.len(), 2, "{v:?}");
    }

    #[test]
    fn verdict_uses_union_of_base_and_head_patterns() {
        let dir = temp_repo();
        let d = dir.path();
        write(d, "dev/protected-paths.txt", "a/**\nb/**\n");
        write(d, "a/x.txt", "x\n");
        write(d, "b/y.txt", "y\n");
        commit_all(d, "base");
        git_in(d, &["checkout", "-q", "-b", "feature"]);
        // The head drops `b/**` from the list and edits a file under it.
        write(d, "dev/protected-paths.txt", "a/**\n");
        write(d, "b/y.txt", "changed\n");
        commit_all(d, "shrink the list");
        let v = verdict(d, "main", "feature").unwrap();
        assert_eq!(v.needs_approval, vec![pc("b/y.txt", "modified")]);

        // The reverse: the head adds a pattern and edits under it.
        git_in(d, &["checkout", "-q", "main"]);
        git_in(d, &["checkout", "-q", "-b", "grow"]);
        write(d, "dev/protected-paths.txt", "a/**\nb/**\nc/**\n");
        write(d, "c/z.txt", "z\n");
        commit_all(d, "grow");
        git_in(d, &["checkout", "-q", "-b", "grow2"]);
        write(d, "c/z.txt", "z2\n");
        commit_all(d, "edit c");
        let v = verdict(d, "grow", "grow2").unwrap();
        assert_eq!(v.needs_approval, vec![pc("c/z.txt", "modified")]);
    }

    #[test]
    fn verdict_first_run_without_base_list() {
        let dir = temp_repo();
        let d = dir.path();
        write(
            d,
            "crates/lil-spec-tests/tests/x.rs",
            "#[test]\nfn a() {}\n",
        );
        commit_all(d, "base has no list");
        git_in(d, &["checkout", "-q", "-b", "feature"]);
        write(
            d,
            "dev/protected-paths.txt",
            "add-ok:crates/lil-spec-tests/**\n",
        );
        write(d, "crates/lil-spec-tests/tests/x.rs", "fn a() {}\n");
        commit_all(d, "introduce the list and weaken the test");
        let v = verdict(d, "main", "feature").unwrap();
        assert_eq!(
            v.needs_approval,
            vec![pc("crates/lil-spec-tests/tests/x.rs", "modified")]
        );
        assert_eq!(v.warnings.len(), 1, "{v:?}");
    }

    #[test]
    fn verdict_errors_without_any_list_or_with_unknown_revisions() {
        let dir = temp_repo();
        let d = dir.path();
        write(d, "a.txt", "a\n");
        commit_all(d, "base");
        git_in(d, &["checkout", "-q", "-b", "feature"]);
        write(d, "a.txt", "b\n");
        commit_all(d, "edit");
        assert!(verdict(d, "main", "feature").is_err(), "no list anywhere");
        assert!(verdict(d, "main", "nope").is_err());
        assert!(verdict(d, "--output=x", "feature").is_err());
    }

    #[test]
    fn verdict_judges_the_pinned_commit_and_applies_expected_fail_binding() {
        let dir = temp_repo();
        let d = dir.path();
        write(
            d,
            "dev/protected-paths.txt",
            "add-ok:crates/lil-spec-tests/**\n",
        );
        write(d, EXPECTED_FAIL_PATH, "tests = []\n");
        write(
            d,
            "crates/lil-spec-tests/tests/smoke.rs",
            "#[test]\nfn spec_harness_runs() {}\n",
        );
        commit_all(d, "base");
        git_in(d, &["checkout", "-q", "-b", "feature"]);
        write(d, "crates/lil-spec-tests/tests/dummy.rs", "\n");
        write(
            d,
            EXPECTED_FAIL_PATH,
            r#"tests = [ { name = "lil-spec-tests::smoke::spec_harness_runs", rule = "QA-017", file = "tests/dummy.rs" } ]"#,
        );
        commit_all(d, "mask a real test with a dummy file");
        let head_sha = git_in(d, &["rev-parse", "HEAD"]).trim().to_string();
        let v = verdict(d, "main", &head_sha).unwrap();
        assert_eq!(
            v.needs_approval,
            vec![Blocker::ExpectedFailAdditionWithoutNewTest {
                name: "lil-spec-tests::smoke::spec_harness_runs".into()
            }]
        );

        // A properly bound entry for a genuinely new test binary passes.
        git_in(d, &["checkout", "-q", "main"]);
        git_in(d, &["checkout", "-q", "-b", "good"]);
        write(
            d,
            "crates/lil-spec-tests/tests/newbin.rs",
            "#[test]\nfn t() { panic!() }\n",
        );
        write(
            d,
            EXPECTED_FAIL_PATH,
            r#"tests = [ { name = "lil-spec-tests::newbin::t", rule = "QA-017", file = "tests/newbin.rs" } ]"#,
        );
        commit_all(d, "new failing spec test, expected to fail");
        let v = verdict(d, "main", "good").unwrap();
        assert!(v.needs_approval.is_empty(), "{v:?}");
    }

    // ---- fix round 2: exact-case binding, case collisions, build scripts ----

    #[test]
    fn case_variant_binary_does_not_bind_existing_name() {
        // `tests/foo.rs` already exists with a failing test `t`. The change adds an empty
        // `tests/FOO/main.rs` and an entry that names the existing test but points at it.
        let sneaky = [ef("lil-spec-tests::foo::t", "tests/FOO/main.rs")];
        let v = evaluate(
            &protected(),
            &[],
            &[],
            &sneaky,
            &[spec_added("tests/FOO/main.rs")],
        );
        assert_eq!(v.needs_approval.len(), 1, "{v:?}");

        // The mirror image: a case-variant name against a lowercase new file.
        let mirrored = [ef("lil-spec-tests::FOO::t", "tests/foo.rs")];
        let v = evaluate(
            &protected(),
            &[],
            &[],
            &mirrored,
            &[spec_added("tests/foo.rs")],
        );
        assert_eq!(v.needs_approval.len(), 1, "{v:?}");

        // The added path's case must match the entry's `file` exactly, too.
        let wrong_added_case = [ef("lil-spec-tests::foo::t", "tests/foo.rs")];
        let v = evaluate(
            &protected(),
            &[],
            &[],
            &wrong_added_case,
            &[spec_added("tests/Foo.rs")],
        );
        assert_eq!(v.needs_approval.len(), 1, "{v:?}");

        // Exactly matching case is still fine.
        let ok = [ef("lil-spec-tests::FOO::t", "tests/FOO/main.rs")];
        let v = evaluate(
            &protected(),
            &[],
            &[],
            &ok,
            &[spec_added("tests/FOO/main.rs")],
        );
        assert!(v.needs_approval.is_empty(), "{v:?}");

        // A case-variant spelling of the toml path is a different file, with no exemption.
        let v = eval(&[change(
            "crates/lil-spec-tests/EXPECTED-FAIL.toml",
            ChangeKind::Modified,
        )]);
        assert_eq!(v.needs_approval.len(), 1, "{v:?}");
    }

    /// Commits a regular file straight into the index, so two paths that differ only in case
    /// can coexist on a case-insensitive filesystem.
    fn commit_blob(d: &Path, path: &str, content: &str) {
        write(d, "scratch-blob.tmp", content);
        let sha = git_in(d, &["hash-object", "-w", "scratch-blob.tmp"]);
        std::fs::remove_file(d.join("scratch-blob.tmp")).unwrap();
        git_in(
            d,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{},{path}", sha.trim()),
            ],
        );
        git_in(d, &["commit", "-q", "-m", "blob"]);
    }

    #[test]
    fn added_case_collision_with_protected_file_needs_approval() {
        let dir = temp_repo();
        let d = dir.path();
        write(
            d,
            "dev/protected-paths.txt",
            "add-ok:crates/lil-spec-tests/**\ncrates/lil-cli/src/dev/**\n",
        );
        write(d, EXPECTED_FAIL_PATH, "tests = []\n");
        write(d, "crates/lil-cli/src/dev/guard.rs", "// guard\n");
        write(d, "crates/lil-core/src/lib.rs", "// lib\n");
        commit_all(d, "base");
        git_in(d, &["checkout", "-q", "-b", "feature"]);
        commit_blob(
            d,
            "crates/lil-spec-tests/EXPECTED-FAIL.toml",
            "tests = []\n",
        );
        commit_blob(d, "crates/lil-cli/src/dev/GUARD.rs", "// not the guard\n");
        // Unprotected on both sides: not our business.
        commit_blob(d, "crates/lil-core/src/LIB.rs", "// other\n");

        let v = verdict(d, "main", "feature").unwrap();
        let mut blocked: Vec<&str> = v
            .needs_approval
            .iter()
            .filter_map(|b| match b {
                Blocker::ProtectedChanged { path, .. } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        blocked.sort();
        // GUARD.rs is blocked twice: as a new file under a strict pattern (R26) and as a case
        // collision.
        blocked.dedup();
        assert_eq!(
            blocked,
            vec![
                "crates/lil-cli/src/dev/GUARD.rs",
                "crates/lil-spec-tests/EXPECTED-FAIL.toml"
            ],
            "{v:?}"
        );
        assert!(v.needs_approval.iter().any(|b| matches!(
            b,
            Blocker::ProtectedChanged { kind, .. }
                if kind.contains("`crates/lil-spec-tests/expected-fail.toml`")
        )));
    }

    #[test]
    fn added_build_rs_in_protected_crate_needs_approval() {
        let v = eval(&[change("crates/lil-spec-tests/build.rs", ChangeKind::Added)]);
        assert_eq!(v.needs_approval.len(), 1, "{v:?}");
        // Anywhere under a protected glob, and in any letter case.
        let v = eval(&[change(
            "crates/lil-spec-tests/tests/sub/Build.rs",
            ChangeKind::Added,
        )]);
        assert_eq!(v.needs_approval.len(), 1, "{v:?}");
        // Outside protected paths, or another file name, stays free.
        assert!(eval(&[change("crates/lil-core/build.rs", ChangeKind::Added)]).is_clean());
        assert!(
            eval(&[change(
                "crates/lil-spec-tests/tests/builder.rs",
                ChangeKind::Added
            )])
            .is_clean()
        );
    }

    // ---- fix round 3: 8.3 aliases, new manifests, Unicode case folding ----

    fn alias_component(v: &Verdict) -> Vec<String> {
        v.needs_approval
            .iter()
            .filter_map(|b| match b {
                Blocker::ShortNameAlias { component, .. } => Some(component.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn short_name_alias_component_needs_approval() {
        // A directory alias, a file alias and a nested alias, all of which would be written
        // through to real protected files on a volume with 8.3 names.
        for (path, component) in [
            ("crates/lil-sp~1/expected-fail.toml", "lil-sp~1"),
            ("crates/lil-cli/src/dev/spec_t~1.rs", "spec_t~1.rs"),
            ("crates/lil-spec-tests/tests/regres~1/main.rs", "regres~1"),
            ("crates/lil-cli/src/dev/SPEC_T~1.RS", "SPEC_T~1.RS"),
            ("docs/PROGRA~1/notes.md", "PROGRA~1"),
        ] {
            for kind in [
                ChangeKind::Added,
                ChangeKind::AddedLink,
                ChangeKind::TypeChanged,
            ] {
                let v = eval(&[change(path, kind.clone())]);
                assert_eq!(
                    alias_component(&v),
                    vec![component],
                    "{kind:?} {path}: {v:?}"
                );
            }
            // Either side of a rename.
            let to = eval(&[renamed("crates/lil-core/src/a.rs", path)]);
            assert_eq!(alias_component(&to), vec![component], "rename to {path}");
            let from = eval(&[renamed(path, "crates/lil-core/src/a.rs")]);
            assert_eq!(
                alias_component(&from),
                vec![component],
                "rename from {path}"
            );
        }
        // The rule covers new, renamed and retyped paths only.
        assert!(eval(&[change("docs/a~1.md", ChangeKind::Modified)]).is_clean());
        assert!(eval(&[change("docs/a~1.md", ChangeKind::Deleted)]).is_clean());
        // It renders in plain language.
        let text = render_plain(&eval(&[change(
            "crates/lil-sp~1/x.toml",
            ChangeKind::Added,
        )]));
        assert!(text.contains("8.3") && text.contains("lil-sp~1"), "{text}");
    }

    #[test]
    fn normal_tilde_names_unaffected() {
        for path in [
            "docs/notes~draft.md",
            "docs/toolong1~1.rs", // seven characters before the tilde
            "docs/a~1.long",      // four-character extension
            "docs/a.b~1",         // the dot comes before the tilde
            "docs/x~1.tar.gz",    // two dots
            "docs/~1",            // nothing before the tilde
            "docs/a~",            // no digits
            "docs/backup~.txt",
            "docs/~$report.docx",
            "docs/a~1b.rs",
        ] {
            let v = eval(&[change(path, ChangeKind::Added)]);
            assert!(v.is_clean(), "{path}: {v:?}");
        }
        for alias in ["PROGRA~1", "a~1", "abcdef~12.rs", "a~1.", "A~B~1", "x~7.c"] {
            assert!(is_short_name_alias(alias), "{alias}");
        }
        for plain in ["", "~", "notes.md", "a~1.abcd", "abcdefg~1"] {
            assert!(!is_short_name_alias(plain), "{plain}");
        }
    }

    #[test]
    fn added_cargo_toml_under_protected_glob_needs_approval() {
        // The workspace takes `crates/*`, and a new manifest can name any file as its build script.
        for path in [
            "crates/lil-dev-guard/Cargo.toml",
            "crates/lil-spec-tests/sub/CARGO.TOML",
            "crates/lil-cli/src/dev/cargo.toml",
        ] {
            let v = eval(&[change(path, ChangeKind::Added)]);
            assert_eq!(v.needs_approval.len(), 1, "{path}: {v:?}");
            assert!(matches!(
                &v.needs_approval[0],
                Blocker::ProtectedChanged { kind, .. } if kind.contains("Cargo manifest")
            ));
        }
        // In a test home (add-ok, R26) the script file itself is a plain addition; under any
        // other protected pattern every new file needs approval.
        assert!(eval(&[change("crates/lil-spec-tests/gen.rs", ChangeKind::Added)]).is_clean());
        assert!(!eval(&[change("crates/lil-dev-guard/gen.rs", ChangeKind::Added)]).is_clean());
        // New crates elsewhere are not under the guard's globs.
        assert!(eval(&[change("crates/lil-core/Cargo.toml", ChangeKind::Added)]).is_clean());
    }

    #[test]
    fn non_ascii_case_collision_detected() {
        let tree = vec![
            "crates/lil-spec-tests/tests/caf\u{e9}.rs".to_string(),
            "crates/lil-spec-tests/tests/CAF\u{c9}.RS".to_string(),
            "crates/lil-core/src/\u{e9}t\u{e9}.rs".to_string(),
            "crates/lil-core/src/\u{c9}T\u{c9}.rs".to_string(),
        ];
        let added = change(
            "crates/lil-spec-tests/tests/CAF\u{c9}.RS",
            ChangeKind::Added,
        );
        let got = case_collisions(&protected(), &[added], &tree);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(matches!(
            &got[0],
            Blocker::ProtectedChanged { kind, .. } if kind.contains("caf\u{e9}.rs")
        ));
        // Unprotected on both sides: not reported.
        let other = change("crates/lil-core/src/\u{c9}T\u{c9}.rs", ChangeKind::Added);
        assert!(case_collisions(&protected(), &[other], &tree).is_empty());
    }

    // ---- R23: control characters in paths -------------------------------------------------

    /// A terminal escape that moves the cursor up a line and erases it.
    const ERASE: &str = "\u{1b}[1A\u{1b}[2K";

    #[test]
    fn control_char_paths_need_approval_protected_or_not() {
        let evil = format!("docs/{ERASE}notes.md");
        let v = eval(&[change(&evil, ChangeKind::Added)]);
        assert_eq!(
            v.needs_approval,
            vec![Blocker::ControlCharsInPath { path: evil.clone() }]
        );
        // Either side of a rename counts; so does a bell or a carriage return.
        let v = eval(&[renamed("a\u{7}b.rs", "ab.rs")]);
        assert!(
            matches!(&v.needs_approval[..], [Blocker::ControlCharsInPath { path }] if path == "a\u{7}b.rs"),
            "{v:?}"
        );
        let v = eval(&[change("x\rprotected.rs", ChangeKind::Modified)]);
        assert_eq!(v.needs_approval.len(), 1, "{v:?}");
        // Ordinary non-ASCII names are fine.
        assert!(eval(&[change("docs/caf\u{e9}.md", ChangeKind::Added)]).is_clean());
    }

    #[test]
    fn rendered_lines_escape_control_characters() {
        let v = Verdict {
            needs_approval: vec![
                Blocker::ControlCharsInPath {
                    path: format!("x{ERASE}.rs"),
                },
                pc(
                    &format!("crates/lil-spec-tests/tests/{ERASE}a.rs"),
                    "modified",
                ),
                Blocker::ShortNameAlias {
                    path: format!("{ERASE}PROGRA~1"),
                    component: format!("{ERASE}PROGRA~1"),
                },
                Blocker::ExpectedFailAdditionWithoutNewTest {
                    name: format!("lil-spec-tests::a::{ERASE}"),
                },
            ],
            warnings: vec![Warning::RemovedTest {
                path: format!("t{ERASE}.rs"),
                line: format!("fn {ERASE}t() {{}}"),
            }],
        };
        let text = render_plain(&v);
        assert!(
            !text.chars().any(|c| c.is_control() && c != '\n'),
            "{text:?}"
        );
        assert!(
            text.contains("path contains control or invisible characters"),
            "{text}"
        );
        assert!(text.contains(r"\u{1b}[1A"), "{text}");
        assert_eq!(text.lines().count(), 5, "{text}");
        assert_eq!(escape_controls("caf\u{e9} ok"), "caf\u{e9} ok");
        assert_eq!(escape_controls("a\u{1b}b\nc"), r"a\u{1b}b\nc");
    }

    /// Adds `content` at `path` (any bytes, control characters included) on top of commit `base`
    /// using `git mktree`, without a work tree or index. Returns the new commit's SHA.
    fn commit_raw_path(d: &Path, base: &str, path: &str, content: &str) -> String {
        fn git_stdin(d: &Path, args: &[&str], input: &[u8]) -> String {
            use std::io::Write as _;
            let mut child = Command::new("git")
                .arg("-C")
                .arg(d)
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env(
                    "GIT_CONFIG_GLOBAL",
                    if cfg!(windows) { "NUL" } else { "/dev/null" },
                )
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(input).unwrap();
            let out = child.wait_with_output().unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        /// `tree` with `blob` placed at `comps`, as a new tree SHA.
        fn put(d: &Path, tree: Option<&str>, comps: &[&str], blob: &str) -> String {
            let listing = tree
                .map(|t| git_in(d, &["ls-tree", "-z", t]))
                .unwrap_or_default();
            let mut entries: Vec<String> = Vec::new();
            let mut sub = None;
            for e in listing.split('\0').filter(|e| !e.is_empty()) {
                let (meta, name) = e.split_once('\t').unwrap();
                if name == comps[0] {
                    sub = Some(meta.split(' ').nth(2).unwrap().to_string());
                } else {
                    entries.push(e.to_string());
                }
            }
            if comps.len() == 1 {
                entries.push(format!("100644 blob {blob}\t{}", comps[0]));
            } else {
                let t = put(d, sub.as_deref(), &comps[1..], blob);
                entries.push(format!("040000 tree {t}\t{}", comps[0]));
            }
            let mut input = entries.join("\0").into_bytes();
            input.push(0);
            git_stdin(d, &["mktree", "-z"], &input)
        }
        let blob = git_stdin(d, &["hash-object", "-w", "--stdin"], content.as_bytes());
        let base_tree = git_in(d, &["rev-parse", &format!("{base}^{{tree}}")]);
        let comps: Vec<&str> = path.split('/').collect();
        let tree = put(d, Some(base_tree.trim()), &comps, &blob);
        git_in(d, &["commit-tree", &tree, "-p", base, "-m", "raw path"])
            .trim()
            .to_string()
    }

    #[test]
    fn escape_sequence_path_from_git_is_blocked_and_never_printed_raw() {
        let dir = temp_repo();
        let d = dir.path();
        write(
            d,
            PROTECTED_PATHS_FILE,
            "crates/lil-spec-tests/**\ndev/protected-paths.txt\n",
        );
        write(
            d,
            "crates/lil-spec-tests/tests/smoke.rs",
            "#[test]\nfn a() {}\n",
        );
        commit_all(d, "base");
        let base = git_in(d, &["rev-parse", "HEAD"]).trim().to_string();
        let evil_protected = format!("crates/lil-spec-tests/tests/{ERASE}smoke.rs");
        let evil_plain = format!("{ERASE}README.md");
        let mid = commit_raw_path(d, &base, &evil_protected, "#[test]\nfn b() {}\n");
        let head = commit_raw_path(d, &mid, &evil_plain, "hi\n");

        let v = verdict(d, &base, &head).unwrap();
        for evil in [&evil_protected, &evil_plain] {
            assert!(
                v.needs_approval
                    .contains(&Blocker::ControlCharsInPath { path: evil.clone() }),
                "{evil:?} not blocked: {v:?}"
            );
        }

        let raw = |s: &str| s.contains('\u{1b}');
        let plain = render_plain(&v);
        assert!(!raw(&plain), "{plain:?}");
        let lines = crate::dev::merge::blocker_lines(&v);
        assert!(lines.iter().all(|l| !raw(l)), "{lines:?}");
        let refusal =
            crate::dev::merge::explain(&[crate::dev::merge::Refusal::ProtectedNeedsApproval(
                lines.clone(),
            )]);
        assert!(!raw(&refusal), "{refusal:?}");
        let summary = lines.join("\n");
        let shown = crate::dev::reviewer::summary_display(&head, &summary);
        assert!(!raw(&shown), "{shown:?}");
        assert!(
            shown.contains("path contains control or invisible characters"),
            "{shown}"
        );
        let body = crate::dev::reviewer::review_body(&head, &summary);
        assert!(!raw(&body), "{body:?}");
    }

    // ---- R38: bidirectional and invisible characters -------------------------------------

    /// Every character R38 adds to the control characters: the bidi embeddings, overrides and
    /// isolates, the zero-width and direction marks, the line and paragraph separators, the BOM,
    /// the soft hyphen and the Arabic letter mark.
    const INVISIBLE: [char; 19] = [
        '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}',
        '\u{2068}', '\u{2069}', '\u{200B}', '\u{200C}', '\u{200D}', '\u{200E}', '\u{200F}',
        '\u{2028}', '\u{2029}', '\u{FEFF}', '\u{00AD}', '\u{061C}',
    ];

    #[test]
    fn dangerous_chars_are_controls_plus_bidi_and_invisible_ones() {
        for c in INVISIBLE {
            assert!(is_dangerous_char(c), "{:04X}", c as u32);
        }
        for c in ['\u{1b}', '\r', '\n', '\u{7}', '\u{85}'] {
            assert!(is_dangerous_char(c), "{:04X}", c as u32);
        }
        for c in [
            'a', '\u{e9}', '\u{c9}', ' ', '\u{4e2d}', '\u{2010}', '\u{2070}', '\u{2027}',
        ] {
            assert!(!is_dangerous_char(c), "{:04X}", c as u32);
        }
    }

    #[test]
    fn right_to_left_override_in_a_path_is_blocked_and_printed_escaped() {
        // Shows as `docs/evilsr.txt` in a terminal that honours the override.
        let evil = "docs/evil\u{202E}txt.rs";
        let v = eval(&[change(evil, ChangeKind::Added)]);
        assert_eq!(
            v.needs_approval,
            vec![Blocker::ControlCharsInPath { path: evil.into() }]
        );
        let text = render_plain(&v);
        assert!(!text.contains('\u{202E}'), "{text:?}");
        assert!(text.contains(r"docs/evil\u{202e}txt.rs"), "{text}");
        // Every listed character, on either side of a rename, is blocked and escaped.
        for c in INVISIBLE {
            let path = format!("a{c}b.rs");
            let v = eval(&[renamed(&path, "ab.rs")]);
            assert_eq!(
                v.needs_approval,
                vec![Blocker::ControlCharsInPath { path: path.clone() }],
                "{:04X}",
                c as u32
            );
            let shown = escape_controls(&path);
            assert!(!shown.chars().any(is_dangerous_char), "{shown:?}");
            assert_eq!(shown, format!("a\\u{{{:x}}}b.rs", c as u32));
        }
    }

    #[test]
    fn right_to_left_override_path_from_git_is_blocked_and_never_printed_raw() {
        let dir = temp_repo();
        let d = dir.path();
        write(d, PROTECTED_PATHS_FILE, "dev/protected-paths.txt\n");
        write(d, "README.md", "hi\n");
        commit_all(d, "base");
        let base = git_in(d, &["rev-parse", "HEAD"]).trim().to_string();
        let evil = "notes\u{202E}dm.rs";
        let head = commit_raw_path(d, &base, evil, "x\n");
        let v = verdict(d, &base, &head).unwrap();
        assert!(
            v.needs_approval
                .contains(&Blocker::ControlCharsInPath { path: evil.into() }),
            "{v:?}"
        );
        let plain = render_plain(&v);
        assert!(!plain.contains('\u{202E}'), "{plain:?}");
        let lines = crate::dev::merge::blocker_lines(&v);
        assert!(lines.iter().all(|l| !l.contains('\u{202E}')), "{lines:?}");
    }

    // ---- ruling R26: additions under protected paths -------------------------------------

    #[test]
    fn add_ok_prefix_parses_and_stricter_wins() {
        let p = Protected::parse("add-ok:tests-home/**\nstrict/**\n").unwrap();
        assert!(p.is_protected("tests-home/a.rs"));
        assert!(!p.addition_needs_approval("tests-home/a.rs"));
        assert!(p.is_protected("strict/a.rs"));
        assert!(p.addition_needs_approval("strict/a.rs"));
        assert!(!p.is_protected("elsewhere/a.rs"));
        assert!(!p.addition_needs_approval("elsewhere/a.rs"));
        // The same pattern with and without the marker (a union of base and head): strict,
        // in either order and whatever the case.
        for text in [
            "add-ok:home/**\nhome/**\n",
            "home/**\nadd-ok:home/**\n",
            "add-ok:home/**\nHOME/**\n",
        ] {
            let p = Protected::parse(text).unwrap();
            assert!(p.addition_needs_approval("home/x.rs"), "{text:?}");
        }
        // A path under an add-ok pattern and a strict one: strict wins.
        let p = Protected::parse("add-ok:a/**\na/b/**\n").unwrap();
        assert!(!p.addition_needs_approval("a/x.rs"));
        assert!(p.addition_needs_approval("a/b/x.rs"));
        // The marker alone is no pattern.
        assert!(Protected::parse("add-ok:\n").is_err());
        assert!(Protected::parse("add-ok:!x\nfoo\n").is_err());
    }

    #[test]
    fn committed_list_marks_only_the_test_homes_add_ok() {
        let text = include_str!("../../../../dev/protected-paths.txt");
        let mut add_ok: Vec<&str> = text
            .lines()
            .filter_map(|l| l.trim().strip_prefix(ADD_OK_PREFIX))
            .collect();
        add_ok.sort();
        assert_eq!(
            add_ok,
            ["**/proptest-regressions/**", "crates/lil-spec-tests/**"]
        );
    }

    /// A repo whose `main` commits the real protected-path list; returns (dir, main sha).
    fn real_list_repo() -> (tempfile::TempDir, String) {
        let dir = temp_repo();
        let d = dir.path();
        write(
            d,
            PROTECTED_PATHS_FILE,
            include_str!("../../../../dev/protected-paths.txt"),
        );
        write(d, EXPECTED_FAIL_PATH, "tests = []\n");
        write(d, "README.md", "hello\n");
        commit_all(d, "base");
        let base = git_in(d, &["rev-parse", "HEAD"]).trim().to_string();
        (dir, base)
    }

    #[test]
    fn added_files_under_protected_paths_need_approval() {
        let (dir, base) = real_list_repo();
        let d = dir.path();
        for (i, path) in [
            ".github/workflows/forged.yml",
            ".cargo/config.toml",
            "tools/git-hooks/post-merge",
            "crates/lil-cli/src/dev/extra.rs",
            ".claude/settings.local.json",
        ]
        .iter()
        .enumerate()
        {
            git_in(d, &["checkout", "-q", "-B", &format!("add{i}"), &base]);
            write(d, path, "new\n");
            commit_all(d, "add a file");
            let v = verdict(d, &base, "HEAD").unwrap();
            assert!(
                v.needs_approval.iter().any(|b| matches!(b,
                    Blocker::ProtectedChanged { path: p, kind } if p == path && kind.starts_with("added"))),
                "{path}: {v:?}"
            );
            assert!(render_plain(&v).contains(path), "{path}");
        }
    }

    #[test]
    fn added_spec_test_and_proptest_seed_need_no_approval() {
        let (dir, base) = real_list_repo();
        let d = dir.path();
        git_in(d, &["checkout", "-q", "-b", "spec"]);
        write(
            d,
            "crates/lil-spec-tests/tests/new_spec.rs",
            "#[test]\nfn new_spec() {}\n",
        );
        write(
            d,
            "crates/lil-core/proptest-regressions/model.txt",
            "cc 0123\n",
        );
        commit_all(d, "a new spec test and a seed");
        let v = verdict(d, &base, "HEAD").unwrap();
        assert!(v.needs_approval.is_empty(), "{v:?}");
        // Changing that spec test afterwards still needs approval.
        let added = git_in(d, &["rev-parse", "HEAD"]).trim().to_string();
        write(
            d,
            "crates/lil-spec-tests/tests/new_spec.rs",
            "#[test]\nfn new_spec() { assert!(true) }\n",
        );
        commit_all(d, "edit it");
        let v = verdict(d, &added, "HEAD").unwrap();
        assert!(!v.needs_approval.is_empty(), "{v:?}");
    }

    #[test]
    fn a_head_list_cannot_make_its_own_additions_add_ok() {
        let (dir, base) = real_list_repo();
        let d = dir.path();
        git_in(d, &["checkout", "-q", "-b", "sneaky"]);
        // The head marks the workflows add-ok (and adds a workflow); the base's strict line wins.
        let list = include_str!("../../../../dev/protected-paths.txt")
            .replace(".github/workflows/**", "add-ok:.github/workflows/**")
            .replace(".github/workflows/ci-guard.yml\n", "")
            .replace(".github/workflows/post-merge.yml\n", "");
        write(d, PROTECTED_PATHS_FILE, &list);
        write(d, ".github/workflows/forged.yml", "on: push\n");
        commit_all(d, "mark workflows add-ok");
        let v = verdict(d, &base, "HEAD").unwrap();
        assert!(
            v.needs_approval.iter().any(|b| matches!(b,
                Blocker::ProtectedChanged { path, .. } if path == ".github/workflows/forged.yml")),
            "{v:?}"
        );
    }
}
