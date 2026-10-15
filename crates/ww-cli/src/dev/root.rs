//! Which repository a `ww dev` command works on (ruling R38).
//!
//! `ww` is built from one checkout, and that checkout's path is compiled into it
//! ([`built_root`]). The commands that read or judge a repository work on the git repository of
//! the current directory (`git rev-parse --show-toplevel`), and refuse when that isn't the
//! checkout the binary was built from: a `ww` built here and run in another repository would
//! otherwise silently judge, scan or merge this checkout instead.
//!
//! Both paths are canonicalised before they are compared: symlinks and 8.3 short names are
//! resolved, the Windows `\\?\` prefix is stripped, and on Windows the comparison ignores case.
//!
//! `spec-tests` and `spec-report` read the source tree the binary was built from
//! ([`source_root`]); they refuse a mismatch too.
//!
//! Ruling R39: git's repository variables ([`GIT_REPO_VARS`]) would let the check above pass
//! while ww's git calls read another repository (`GIT_DIR=<other>/.git`, run from this
//! checkout's top level). So before the root is resolved, ww refuses when any of them is set
//! to anything but the built checkout's own repository ([`check_git_env`]), and every child
//! process ww starts has all of them removed ([`git_command`], [`scrub_git_env`]).
//!
//! What git itself exports to the hooks (git 2.56, checked by running real hooks): `pre-push`
//! in the main checkout gets none of them; `pre-merge-commit` gets `GIT_INDEX_FILE=.git/index`
//! (relative to the top level). In a linked worktree both get `GIT_DIR=<main>/.git/worktrees/<name>`,
//! and `pre-merge-commit` also `GIT_INDEX_FILE=<that>/index`. `git --git-dir=.git --work-tree=.`
//! exports both as given. Those name this checkout's own repository, so they are accepted.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Git's variables that choose which repository, objects, index or refs git reads (R39).
pub(crate) const GIT_REPO_VARS: [&str; 7] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_INDEX_FILE",
    "GIT_NAMESPACE",
];

/// Removes [`GIT_REPO_VARS`] from `cmd`'s environment, so the child works on the repository
/// it is pointed at (`-C <root>` or its current directory) and on nothing else.
pub(crate) fn scrub_git_env(cmd: &mut Command) -> &mut Command {
    for var in GIT_REPO_VARS {
        cmd.env_remove(var);
    }
    cmd
}

/// `git`, with [`GIT_REPO_VARS`] removed from its environment. Every git child ww starts is
/// made here.
pub(crate) fn git_command() -> Command {
    let mut cmd = Command::new("git");
    scrub_git_env(&mut cmd);
    cmd
}

/// The checkout this binary was built from (the workspace root above `crates/ww-cli`).
pub(crate) fn built_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// The repository of the current directory, when it is the checkout this `ww` was built from.
/// Otherwise a plain-language refusal.
pub(crate) fn repo_root() -> Result<PathBuf, String> {
    let cwd = std::env::current_dir()
        .map_err(|e| format!("couldn't read the current directory, so nothing was done: {e}"))?;
    let built = built_root();
    check_git_env(&|var| std::env::var_os(var), &cwd, &built)?;
    repo_root_from(&cwd, &built)
}

/// Refuses when one of [`GIT_REPO_VARS`] (read with `var`) is set to anything but the
/// repository of `built`. Empty values are ignored (child processes never see them). Relative
/// paths are read from `cwd`, as git reads them.
///
/// Accepted: `GIT_DIR` naming `built`'s git directory (`git rev-parse --absolute-git-dir`, so
/// `<built>/.git`, or `.git/worktrees/<name>` for a linked worktree), `GIT_INDEX_FILE` naming
/// the `index` in it, and `GIT_WORK_TREE` naming `built` itself. The other four are refused
/// whenever they are set.
pub(crate) fn check_git_env(
    var: &dyn Fn(&str) -> Option<OsString>,
    cwd: &Path,
    built: &Path,
) -> Result<(), String> {
    let mut git_dir: Option<PathBuf> = None;
    for name in GIT_REPO_VARS {
        let Some(value) = var(name).filter(|v| !v.is_empty()) else {
            continue;
        };
        let value = cwd.join(value);
        let own = match name {
            "GIT_DIR" | "GIT_INDEX_FILE" => {
                if git_dir.is_none() {
                    git_dir = Some(own_git_dir(built).map_err(|e| {
                        format!("ww refuses to run with {name} set, because it couldn't tell whether {name} names this checkout's repository ({e}); unset it")
                    })?);
                }
                let dir = git_dir.as_deref().unwrap_or(built);
                if name == "GIT_DIR" {
                    dir.to_path_buf()
                } else {
                    dir.join("index")
                }
            }
            "GIT_WORK_TREE" => built.to_path_buf(),
            _ => return Err(format!("ww refuses to run with {name} set; unset it")),
        };
        if !same_file_path(&value, &own) {
            return Err(format!(
                "ww refuses to run with {name} set to another repository; unset it ({name} is {}, but this ww works only on {})",
                value.display(),
                comparable(built)
                    .unwrap_or_else(|_| built.to_path_buf())
                    .display()
            ));
        }
    }
    Ok(())
}

/// `built`'s git directory, absolute, asked of git with [`GIT_REPO_VARS`] removed.
fn own_git_dir(built: &Path) -> Result<PathBuf, String> {
    let out = git_command()
        .arg("-C")
        .arg(built)
        .args(["rev-parse", "--absolute-git-dir"])
        .output()
        .map_err(|e| format!("couldn't run git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

/// `a` and `b` name the same file or directory. A path that doesn't exist (an index not
/// written yet) is compared through its canonical parent directory.
fn same_file_path(a: &Path, b: &Path) -> bool {
    let resolve = |p: &Path| -> Option<PathBuf> {
        comparable(p).ok().or_else(|| {
            let parent = comparable(p.parent()?).ok()?;
            Some(parent.join(p.file_name()?))
        })
    };
    match (resolve(a), resolve(b)) {
        (Some(a), Some(b)) => same_path(&a, &b),
        _ => false,
    }
}

/// The source tree this `ww` was built from, after the same check as [`repo_root`].
pub(crate) fn source_root() -> Result<PathBuf, String> {
    repo_root()?;
    Ok(built_root())
}

/// [`repo_root`] for the directory `dir` and a binary built from `built`.
pub(crate) fn repo_root_from(dir: &Path, built: &Path) -> Result<PathBuf, String> {
    let built_shown = comparable(built).unwrap_or_else(|_| built.to_path_buf());
    let rebuild = format!(
        "this ww was built for {}; run it from there, or build it in this checkout (cargo run)",
        built_shown.display()
    );
    let out = git_command()
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .output()
        .map_err(|e| {
            format!("couldn't run git to find the repository, so nothing was done: {e}")
        })?;
    if !out.status.success() {
        return Err(format!(
            "{} isn't inside a git repository ({}); {rebuild}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let top = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let top = comparable(&top).map_err(|e| {
        format!(
            "couldn't resolve the repository path {} ({e}); {rebuild}",
            top.display()
        )
    })?;
    let built = comparable(built).map_err(|e| {
        format!(
            "couldn't resolve the path this ww was built for, {} ({e}); build ww in this checkout (cargo run)",
            built.display()
        )
    })?;
    if same_path(&top, &built) {
        Ok(top)
    } else {
        Err(format!(
            "this ww was built for {}, but the current directory is in {}; run it from there, or build it in this checkout (cargo run)",
            built.display(),
            top.display()
        ))
    }
}

/// `p` canonicalised, without the Windows verbatim prefix (`\\?\C:\x` is `C:\x`,
/// `\\?\UNC\host\share` is `\\host\share`).
fn comparable(p: &Path) -> std::io::Result<PathBuf> {
    let canonical = std::fs::canonicalize(p)?;
    Ok(strip_verbatim(&canonical))
}

fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(unc) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{unc}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p.to_path_buf()
    }
}

/// Equal paths; on Windows, ignoring case and the separator style.
fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        let norm = |p: &Path| {
            p.to_string_lossy()
                .replace('/', "\\")
                .trim_end_matches('\\')
                .to_lowercase()
        };
        norm(a) == norm(b)
    } else {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let ok = Command::new("git")
            .args(["-c", "init.defaultBranch=main", "init", "-q"])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success();
        assert!(ok);
        dir
    }

    #[test]
    fn another_repository_is_refused_in_plain_language() {
        let other = temp_repo();
        let err = repo_root_from(other.path(), &built_root()).unwrap_err();
        assert!(err.contains("this ww was built for"), "{err}");
        assert!(
            err.contains("build it in this checkout (cargo run)"),
            "{err}"
        );
        // Neither path is shown with the verbatim prefix.
        assert!(!err.contains(r"\\?\"), "{err}");
    }

    #[test]
    fn a_directory_outside_any_repository_is_refused() {
        let plain = tempfile::tempdir().unwrap();
        // GIT_CEILING_DIRECTORIES can't be set per call here, but a temp dir has no repository
        // above it on any machine this runs on.
        let err = repo_root_from(plain.path(), &built_root()).unwrap_err();
        assert!(err.contains("this ww was built for"), "{err}");
    }

    #[test]
    fn the_built_checkout_and_its_subdirectories_are_accepted() {
        let built = built_root();
        let top = repo_root_from(&built, &built).unwrap();
        let sub = repo_root_from(Path::new(env!("CARGO_MANIFEST_DIR")), &built).unwrap();
        assert_eq!(top, sub);
        assert!(top.join("dev/protected-paths.txt").is_file());
        assert!(!top.to_string_lossy().starts_with(r"\\?\"));
    }

    #[test]
    fn verbatim_prefixes_are_stripped() {
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\C:\repo")),
            PathBuf::from(r"C:\repo")
        );
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\UNC\host\share\repo")),
            PathBuf::from(r"\\host\share\repo")
        );
        assert_eq!(
            strip_verbatim(Path::new("/srv/repo")),
            PathBuf::from("/srv/repo")
        );
    }

    /// `check_git_env` with exactly the variables in `set`.
    fn check(set: &[(&str, &str)], cwd: &Path, built: &Path) -> Result<(), String> {
        let lookup = |name: &str| {
            set.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| OsString::from(v))
        };
        check_git_env(&lookup, cwd, built)
    }

    fn git_t(dir: &Path, args: &[&str]) {
        let ok = git_command()
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn git_dir_and_index_of_the_own_repository_are_accepted() {
        // What git exports to pre-push and pre-merge-commit run in this repository (R39).
        let repo = temp_repo();
        let root = repo.path();
        let s = |p: PathBuf| p.to_string_lossy().into_owned();
        for set in [
            vec![],
            vec![("GIT_INDEX_FILE", ".git/index".to_string())],
            vec![
                ("GIT_DIR", ".git".to_string()),
                ("GIT_WORK_TREE", ".".to_string()),
            ],
            vec![
                ("GIT_DIR", s(root.join(".git"))),
                ("GIT_INDEX_FILE", s(root.join(".git").join("index"))),
            ],
            vec![
                ("GIT_DIR", String::new()),
                ("GIT_OBJECT_DIRECTORY", String::new()),
            ],
        ] {
            let set: Vec<(&str, &str)> = set.iter().map(|(k, v)| (*k, v.as_str())).collect();
            assert_eq!(check(&set, root, root), Ok(()), "{set:?}");
        }
    }

    #[test]
    fn a_linked_worktree_accepts_its_own_git_dir_only() {
        let repo = temp_repo();
        let main = repo.path().join("main");
        std::fs::create_dir(&main).unwrap();
        git_t(&main, &["-c", "init.defaultBranch=main", "init", "-q"]);
        git_t(
            &main,
            &[
                "-c",
                "user.name=Root Test",
                "-c",
                "user.email=root@test.invalid",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "one",
            ],
        );
        let wt = repo.path().join("wt");
        git_t(&main, &["worktree", "add", "-q", "-b", "side", "../wt"]);
        let wt_git = main.join(".git").join("worktrees").join("wt");
        let wt_git_s = wt_git.to_string_lossy().into_owned();
        let index = wt_git.join("index").to_string_lossy().into_owned();
        assert_eq!(
            check(
                &[("GIT_DIR", &wt_git_s), ("GIT_INDEX_FILE", &index)],
                &wt,
                &wt
            ),
            Ok(())
        );
        // The main checkout's git directory is another HEAD and index: refused.
        let main_git = main.join(".git").to_string_lossy().into_owned();
        let err = check(&[("GIT_DIR", &main_git)], &wt, &wt).unwrap_err();
        assert!(
            err.contains("GIT_DIR set to another repository; unset it"),
            "{err}"
        );
    }

    #[test]
    fn foreign_repository_variables_are_refused_in_plain_language() {
        let own = temp_repo();
        let other = temp_repo();
        let root = own.path();
        let other_git = other.path().join(".git").to_string_lossy().into_owned();
        let other_dir = other.path().to_string_lossy().into_owned();
        let other_index = other
            .path()
            .join(".git")
            .join("index")
            .to_string_lossy()
            .into_owned();
        for (name, value) in [
            ("GIT_DIR", other_git.as_str()),
            ("GIT_WORK_TREE", other_dir.as_str()),
            ("GIT_INDEX_FILE", other_index.as_str()),
            // Relative to the current directory, as git reads it: not this repository's.
            ("GIT_INDEX_FILE", "index"),
        ] {
            let err = check(&[(name, value)], root, root).unwrap_err();
            assert!(
                err.contains(&format!(
                    "ww refuses to run with {name} set to another repository; unset it"
                )),
                "{name}={value}: {err}"
            );
        }
        for name in [
            "GIT_COMMON_DIR",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_NAMESPACE",
        ] {
            // Refused whenever set, even to this repository's own paths.
            let err = check(&[(name, ".git")], root, root).unwrap_err();
            assert_eq!(err, format!("ww refuses to run with {name} set; unset it"));
        }
        // A foreign value is refused even next to an accepted one.
        let err = check(
            &[("GIT_INDEX_FILE", ".git/index"), ("GIT_DIR", &other_git)],
            root,
            root,
        )
        .unwrap_err();
        assert!(err.contains("GIT_DIR"), "{err}");
    }

    #[test]
    fn git_children_have_every_repository_variable_removed() {
        let cmd = git_command();
        let removed: Vec<_> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        for var in GIT_REPO_VARS {
            assert!(removed.iter().any(|k| k == var), "{var}: {removed:?}");
        }
        assert_eq!(cmd.get_program(), "git");
    }

    #[cfg(windows)]
    #[test]
    fn windows_comparison_ignores_case_and_separators() {
        assert!(same_path(Path::new(r"C:\Repo\X"), Path::new("c:/repo/x/")));
        assert!(!same_path(Path::new(r"C:\Repo\X"), Path::new(r"C:\Repo\Y")));
    }
}
