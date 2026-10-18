//! A `lil` works only on the checkout it was built from (ruling R38).
//!
//! The built binary is run from a temporary repository, where every command that reads a
//! repository must refuse in plain language instead of silently working on the checkout it was
//! built from, and from its own checkout, where it works. Git's global and system config are
//! switched off for every run.
//!
//! Ruling R39: git's repository variables (`GIT_DIR`, `GIT_WORK_TREE`, ...) pointing at another
//! repository are refused too, so they can't make the check pass while lil's git calls read the
//! other repository.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const REFUSAL: &str = "this lil was built for";

/// Git's repository variables (ruling R39). Every run starts with none of them set.
const GIT_REPO_VARS: [&str; 7] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_INDEX_FILE",
    "GIT_NAMESPACE",
];

fn lil(args: &[&str], cwd: &Path, global: &Path) -> Output {
    lil_env(args, cwd, global, &[])
}

/// [`lil`] with the variables in `env` set (after all of [`GIT_REPO_VARS`] are removed).
fn lil_env(args: &[&str], cwd: &Path, global: &Path, env: &[(&str, &Path)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lil"));
    for var in GIT_REPO_VARS {
        cmd.env_remove(var);
    }
    cmd.envs(env.iter().copied())
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", global)
        .env(
            lil_platform::names::DENYLIST_ENV,
            "not-a-real-denylist-entry",
        )
        .output()
        .expect("the built lil must start")
}

fn git(dir: &Path, global: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", global)
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

/// What `git rev-parse <args>` prints in `dir`, trimmed.
fn rev_parse(dir: &Path, global: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("rev-parse")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", global)
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git rev-parse {args:?}: {}",
        text(&out)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A temporary repository with one commit, and an empty global git config file.
fn other_repo() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("other");
    std::fs::create_dir(&dir).unwrap();
    let global = tmp.path().join("gitconfig");
    std::fs::write(&global, "").unwrap();
    git(
        &dir,
        &global,
        &["-c", "init.defaultBranch=main", "init", "-q"],
    );
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(&dir, &global, &["add", "a.txt"]);
    git(
        &dir,
        &global,
        &[
            "-c",
            "user.name=Root Test",
            "-c",
            "user.email=root@test.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "one",
        ],
    );
    (tmp, dir, global)
}

fn text(out: &Output) -> String {
    format!(
        "status {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn run_from_another_repository_every_repo_command_refuses() {
    let (_tmp, dir, global) = other_repo();
    for args in [
        &["dev", "privacy-scan", "--tree", "HEAD"][..],
        &["dev", "privacy-scan", "--commits", "HEAD"],
        &["dev", "guard", "--base", "HEAD", "--head", "HEAD"],
        &["dev", "spec-report"],
        &["dev", "merge", "1"],
        &["dev", "install-hooks"],
    ] {
        let out = lil(args, &dir, &global);
        assert!(!out.status.success(), "{args:?}: {}", text(&out));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(REFUSAL),
            "{args:?}: {}",
            text(&out)
        );
    }
    // install-hooks refused before writing anything.
    let config = std::fs::read_to_string(dir.join(".git/config")).unwrap();
    assert!(!config.contains("hooksPath"), "{config}");
}

#[test]
fn run_from_its_own_checkout_it_works() {
    let (_tmp, _dir, global) = other_repo();
    let own = Path::new(env!("CARGO_MANIFEST_DIR"));
    // An empty range: nothing to scan, so this is fast and clean.
    let out = lil(
        &["dev", "privacy-scan", "--commits", "HEAD..HEAD"],
        own,
        &global,
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(!String::from_utf8_lossy(&out.stderr).contains(REFUSAL));
}

/// The checkout this test binary was built from.
fn own_checkout() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

#[test]
fn git_dir_pointing_at_another_repository_is_refused() {
    // Run from the built checkout's own root, where `git rev-parse --show-toplevel` with
    // GIT_DIR set answers the current directory, so the R38 check alone would pass while every
    // git call read the other repository.
    let (_tmp, dir, global) = other_repo();
    let other_git = dir.join(".git");
    for args in [
        &["dev", "privacy-scan", "--commits", "HEAD"][..],
        &["dev", "privacy-scan", "--tree", "HEAD"],
        &["dev", "guard", "--base", "HEAD", "--head", "HEAD"],
        &["dev", "merge", "1"],
    ] {
        let out = lil_env(args, &own_checkout(), &global, &[("GIT_DIR", &other_git)]);
        assert!(!out.status.success(), "{args:?}: {}", text(&out));
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("lil refuses to run with GIT_DIR set to another repository; unset it"),
            "{args:?}: {}",
            text(&out)
        );
    }
}

#[test]
fn git_work_tree_pointing_elsewhere_is_refused() {
    let (_tmp, dir, global) = other_repo();
    for args in [
        &["dev", "privacy-scan", "--commits", "HEAD"][..],
        &["dev", "guard", "--base", "HEAD", "--head", "HEAD"],
    ] {
        let out = lil_env(args, &own_checkout(), &global, &[("GIT_WORK_TREE", &dir)]);
        assert!(!out.status.success(), "{args:?}: {}", text(&out));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(
                "lil refuses to run with GIT_WORK_TREE set to another repository; unset it"
            ),
            "{args:?}: {}",
            text(&out)
        );
    }
}

#[test]
fn the_other_repository_variables_are_refused_whenever_they_are_set() {
    let (_tmp, dir, global) = other_repo();
    let objects = dir.join(".git").join("objects");
    for var in [
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
    ] {
        let out = lil_env(
            &["dev", "privacy-scan", "--commits", "HEAD..HEAD"],
            &own_checkout(),
            &global,
            &[(var, &objects)],
        );
        assert_eq!(out.status.code(), Some(2), "{var}: {}", text(&out));
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .contains(&format!("lil refuses to run with {var} set; unset it")),
            "{var}: {}",
            text(&out)
        );
    }
}

#[test]
fn the_variables_git_sets_for_our_own_hooks_are_accepted() {
    // What git exports when it runs pre-push or pre-merge-commit in this checkout: nothing, or
    // GIT_INDEX_FILE=.git/index (relative to the top level); `git --git-dir=.git
    // --work-tree=. push` exports GIT_DIR=.git and GIT_WORK_TREE=. as given. In a linked
    // worktree `.git` is a file, and git names `<main>/.git/worktrees/<name>` and the index in
    // it instead, which is what `git rev-parse --git-dir` and `--git-path index` print.
    let (_tmp, _dir, global) = other_repo();
    let own = own_checkout();
    let git_dir = PathBuf::from(rev_parse(&own, &global, &["--git-dir"]));
    let index = PathBuf::from(rev_parse(&own, &global, &["--git-path", "index"]));
    let abs_git = PathBuf::from(rev_parse(&own, &global, &["--absolute-git-dir"]));
    let abs_index = abs_git.join("index");
    for env in [
        &[("GIT_INDEX_FILE", index.as_path())][..],
        &[
            ("GIT_DIR", git_dir.as_path()),
            ("GIT_WORK_TREE", Path::new(".")),
        ],
        &[("GIT_DIR", &abs_git), ("GIT_INDEX_FILE", &abs_index)],
        &[("GIT_DIR", Path::new(""))],
    ] {
        let out = lil_env(
            &["dev", "privacy-scan", "--commits", "HEAD..HEAD"],
            &own,
            &global,
            env,
        );
        assert_eq!(out.status.code(), Some(0), "{env:?}: {}", text(&out));
    }
}
