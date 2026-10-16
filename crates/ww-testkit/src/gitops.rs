//! Git for tests (QA-038, QA-066, ARCH-083): real `git` with an isolated configuration, and
//! [`GitOps`], which acts out a `git checkout` on disk.
//!
//! **Isolation.** [`TestEnv::git_env`] is `GIT_CONFIG_NOSYSTEM=1`, `HOME` and
//! `XDG_CONFIG_HOME` set to `<root>\profile`, and `GIT_CONFIG_GLOBAL` set to
//! `<root>\profile\.gitconfig`. [`TestEnv::child_env`] carries it too, so a `ww` or core a
//! test starts, and every `git` they run, never reads the system or user git config: a
//! machine whose system config sets `core.autocrlf=true` sees what every other machine sees.
//! The kit's own `git` runs also drop the variables that would point git at another
//! repository or inject configuration (`GIT_DIR`, `GIT_CONFIG_PARAMETERS`, …), so a test run
//! from inside a git hook still touches only its temporary repository. No test repository has
//! a remote, so nothing here uses the network.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::env::TestEnv;

/// The identity of every test commit; the `.invalid` domain is reserved and never routes.
const AUTHOR: &str = "sample-author";
const AUTHOR_EMAIL: &str = "sample-author@example.invalid";

/// Variables that would make a kit `git` run act on another repository or read configuration
/// from the environment. Removed from every run of [`TestEnv::git`] and [`TestEnv::git_init`].
const LEAKY_GIT_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_COMMITTER_DATE",
];

impl TestEnv {
    /// `<root>\profile`: the home folder git sees in this test.
    pub fn git_profile(&self) -> PathBuf {
        self.root().join("profile")
    }

    /// The git isolation: `GIT_CONFIG_NOSYSTEM=1`; `HOME` and `XDG_CONFIG_HOME` =
    /// `<root>\profile`; `GIT_CONFIG_GLOBAL` = `<root>\profile\.gitconfig`.
    pub fn git_env(&self) -> Vec<(OsString, OsString)> {
        let profile = self.git_profile();
        vec![
            (OsString::from("GIT_CONFIG_NOSYSTEM"), OsString::from("1")),
            (OsString::from("HOME"), profile.as_os_str().to_owned()),
            (
                OsString::from("XDG_CONFIG_HOME"),
                profile.as_os_str().to_owned(),
            ),
            (
                OsString::from("GIT_CONFIG_GLOBAL"),
                profile.join(".gitconfig").into_os_string(),
            ),
        ]
    }

    /// Makes `ws` a git repository with one commit of everything in it (an empty commit if it
    /// holds nothing), on branch `main`, with no remote. The test profile's `.gitconfig` gets
    /// the test identity (`sample-author`), so nothing of the machine's config is used.
    ///
    /// # Panics
    /// If `git` is not on `PATH` or a step fails.
    pub fn git_init(&self, ws: &Path) {
        let ws_text = ws.to_string_lossy().into_owned();
        std::fs::create_dir_all(self.git_profile()).unwrap_or_else(|e| {
            panic!("cannot create {}: {e}", self.git_profile().display());
        });
        self.git(ws, &["config", "--global", "user.name", AUTHOR]);
        self.git(ws, &["config", "--global", "user.email", AUTHOR_EMAIL]);
        self.git(ws, &["init", "--quiet", "--initial-branch=main", &ws_text]);
        self.git(ws, &["add", "--all"]);
        self.git(
            ws,
            &[
                "commit",
                "--quiet",
                "--allow-empty",
                "--no-verify",
                "-m",
                "fixture",
            ],
        );
    }

    /// Runs `git <args>` in `ws` with [`TestEnv::git_env`] and returns its stdout.
    ///
    /// # Panics
    /// If `git` is not on `PATH`, cannot start or exits with a failure; the message carries
    /// its stderr.
    pub fn git(&self, ws: &Path, args: &[&str]) -> String {
        let out = self.git_output(ws, args);
        assert!(
            out.status.success(),
            "`git {}` in {} failed ({}):\n{}{}",
            args.join(" "),
            ws.display(),
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// [`TestEnv::git`] without the success check.
    pub(crate) fn git_output(&self, ws: &Path, args: &[&str]) -> Output {
        let mut cmd = Command::new(git_exe());
        for var in LEAKY_GIT_VARS {
            cmd.env_remove(var);
        }
        cmd.args(args)
            .current_dir(ws)
            .envs(self.git_env())
            .stdin(Stdio::null())
            .output()
            .unwrap_or_else(|e| panic!("cannot run git: {e}"))
    }
}

/// `git` on `PATH`.
///
/// # Panics
/// If there is none.
pub(crate) fn git_exe() -> PathBuf {
    ww_platform::process::find_on_path("git")
        .expect("git is not on PATH; tests that need git use the real git CLI")
}

/// A `git checkout` acted out on disk (ARCH-083): [`GitOps::begin`] creates
/// `<ws>\.git\index.lock`, [`GitOps::write`] and [`GitOps::remove`] change working-tree files
/// while it is held, and [`GitOps::finish`] removes the lock and then moves `HEAD` (rewritten
/// through `HEAD.lock`, as git writes refs), the order git itself uses. The real index is never
/// touched, so the repository stays usable afterwards. Dropping an unfinished `GitOps` removes
/// the lock.
#[derive(Debug)]
pub struct GitOps {
    ws: PathBuf,
    lock: PathBuf,
    finished: bool,
}

impl GitOps {
    /// Takes `<ws>\.git\index.lock` (creating `.git` if it is missing).
    ///
    /// # Panics
    /// If the lock already exists (git would refuse too) or cannot be created.
    pub fn begin(ws: &Path) -> GitOps {
        let git_dir = ws.join(".git");
        std::fs::create_dir_all(&git_dir)
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", git_dir.display()));
        let lock = git_dir.join("index.lock");
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .unwrap_or_else(|e| panic!("cannot take {}: {e}", lock.display()));
        GitOps {
            ws: ws.to_path_buf(),
            lock,
            finished: false,
        }
    }

    /// `<ws>\.git\index.lock`.
    pub fn lock_path(&self) -> &Path {
        &self.lock
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.ws.join(rel.replace('/', "\\"))
    }

    /// Writes the working-tree file `rel` (`/`-separated) as git does: an existing file is
    /// unlinked first, then the new one is created with its parent folders.
    ///
    /// # Panics
    /// If a step fails.
    pub fn write(&self, rel: &str, bytes: &[u8]) {
        let path = self.path(rel);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => panic!("cannot unlink {}: {e}", path.display()),
        }
        crate::files::write_file(&path, bytes);
    }

    /// Removes the working-tree file `rel`, then its parent folders that became empty, up to
    /// the workspace root, as git does.
    ///
    /// # Panics
    /// If the file cannot be removed.
    pub fn remove(&self, rel: &str) {
        let path = self.path(rel);
        std::fs::remove_file(&path)
            .unwrap_or_else(|e| panic!("cannot remove {}: {e}", path.display()));
        let mut dir = path.parent();
        while let Some(d) = dir {
            if d == self.ws || std::fs::remove_dir(d).is_err() {
                break;
            }
            dir = d.parent();
        }
    }

    /// Removes the lock, then moves `HEAD`: `HEAD.lock` gets `HEAD`'s content (or
    /// `ref: refs/heads/main` when there is none) and is renamed onto `HEAD`.
    ///
    /// # Panics
    /// If a step fails.
    pub fn finish(mut self) {
        self.finished = true;
        std::fs::remove_file(&self.lock)
            .unwrap_or_else(|e| panic!("cannot remove {}: {e}", self.lock.display()));
        let head = self.ws.join(".git").join("HEAD");
        let content = std::fs::read(&head).unwrap_or_else(|_| b"ref: refs/heads/main\n".to_vec());
        let head_lock = self.ws.join(".git").join("HEAD.lock");
        std::fs::write(&head_lock, content)
            .unwrap_or_else(|e| panic!("cannot write {}: {e}", head_lock.display()));
        std::fs::rename(&head_lock, &head)
            .unwrap_or_else(|e| panic!("cannot move {}: {e}", head.display()));
    }
}

impl Drop for GitOps {
    fn drop(&mut self) {
        if !self.finished {
            let _ = std::fs::remove_file(&self.lock);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path as git prints it: C-quoted (`"C:\\a\\b"`, octal escapes for non-ASCII bytes)
    /// when it holds a backslash or another special byte, else as is.
    fn git_unquote(s: &str) -> String {
        let Some(inner) = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
            return s.to_string();
        };
        let mut out = Vec::new();
        let mut bytes = inner.bytes();
        while let Some(b) = bytes.next() {
            if b != b'\\' {
                out.push(b);
                continue;
            }
            match bytes.next() {
                Some(b't') => out.push(b'\t'),
                Some(b'n') => out.push(b'\n'),
                Some(d @ b'0'..=b'7') => {
                    let mut n = u32::from(d - b'0');
                    for _ in 0..2 {
                        let d = bytes.next().expect("three octal digits");
                        n = n * 8 + u32::from(d - b'0');
                    }
                    out.push(u8::try_from(n).expect("an octal byte"));
                }
                Some(other) => out.push(other),
                None => break,
            }
        }
        String::from_utf8(out).expect("UTF-8 path")
    }

    /// `path` canonicalised, for comparing the paths git prints with the test's own.
    fn canonical(path: &Path) -> PathBuf {
        std::fs::canonicalize(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    #[test] // spec: QA-038
    fn git_init_is_isolated_from_user_config() {
        let t = TestEnv::new();
        let ws = t.make_workspace("git");
        t.git_init(&ws);

        let profile = canonical(&t.git_profile());
        let origins = t.git(&ws, &["config", "--show-origin", "--get-all", "user.name"]);
        let lines: Vec<&str> = origins.lines().collect();
        assert_eq!(lines.len(), 1, "{origins}");
        for line in lines {
            let (origin, value) = line.split_once('\t').unwrap_or((line, ""));
            assert_eq!(value, AUTHOR, "{line}");
            let file = git_unquote(
                origin
                    .strip_prefix("file:")
                    .unwrap_or_else(|| panic!("not a file origin: {line}")),
            );
            assert!(
                canonical(Path::new(&file)).starts_with(&profile),
                "{file} is not under {}",
                profile.display()
            );
        }
        // One commit by the test identity, on `main`, and no remote.
        assert_eq!(
            t.git(&ws, &["log", "--format=%an <%ae>"]).trim(),
            format!("{AUTHOR} <{AUTHOR_EMAIL}>")
        );
        assert_eq!(t.git(&ws, &["branch", "--show-current"]).trim(), "main");
        assert_eq!(t.git(&ws, &["remote"]), "");
        assert_eq!(t.git(&ws, &["status", "--porcelain"]), "");
        assert!(ws.join(".git").is_dir());

        // A path with spaces and non-ASCII letters works the same.
        let u = TestEnv::with_label("Ünïcödé space");
        let ws = u.make_workspace("Ünïcödé space");
        u.git_init(&ws);
        assert_eq!(u.git(&ws, &["rev-list", "--count", "HEAD"]).trim(), "1");
    }

    #[test] // spec: QA-038
    fn child_env_carries_the_git_isolation() {
        let t = TestEnv::new();
        let vars = t.child_env();
        let profile = t.root().join("profile");
        let get = |name: &str| {
            let found: Vec<&OsString> = vars
                .iter()
                .filter(|(k, _)| k == name)
                .map(|(_, v)| v)
                .collect();
            assert_eq!(found.len(), 1, "{name} in {vars:?}");
            PathBuf::from(found[0])
        };
        assert_eq!(get("GIT_CONFIG_NOSYSTEM"), PathBuf::from("1"));
        assert_eq!(get("HOME"), profile);
        assert_eq!(get("XDG_CONFIG_HOME"), profile);
        assert_eq!(get("GIT_CONFIG_GLOBAL"), profile.join(".gitconfig"));
        for (_, value) in t.git_env() {
            assert!(vars.iter().any(|(_, v)| *v == value), "{value:?}");
        }

        // A `git` given child_env() (as a spawned core is) sees no system or user setting,
        // whatever this machine's system config says.
        let out = Command::new(git_exe())
            .args(["config", "--show-origin", "core.autocrlf"])
            .current_dir(t.root())
            .envs(vars.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .output()
            .expect("run git");
        assert_eq!(
            out.status.code(),
            Some(1),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    }

    #[test] // spec: QA-066, ARCH-083
    fn gitops_holds_index_lock_until_finish() {
        let t = TestEnv::new();
        let ws = t.make_workspace("gitops");
        crate::files::write_file(&ws.join("projects").join("a").join("old.txt"), b"old");
        t.git_init(&ws);
        let head = std::fs::read(ws.join(".git").join("HEAD")).unwrap();
        let lock = ws.join(".git").join("index.lock");

        let ops = GitOps::begin(&ws);
        assert_eq!(ops.lock_path(), lock);
        assert!(lock.is_file());
        ops.write("projects/b/new.txt", "Ünïcödé space".as_bytes());
        ops.write("projects/a/old.txt", b"rewritten");
        assert!(lock.is_file(), "the lock is held while files change");
        // Real git refuses to touch the index meanwhile.
        let blocked = t.git_output(&ws, &["add", "--all"]);
        assert!(
            !blocked.status.success(),
            "git add ran while the lock was held"
        );
        ops.remove("projects/a/old.txt");
        assert!(lock.is_file());
        assert!(
            !ws.join("projects").join("a").exists(),
            "the emptied folder is removed"
        );
        assert_eq!(
            std::fs::read(ws.join("projects").join("b").join("new.txt")).unwrap(),
            "Ünïcödé space".as_bytes()
        );

        ops.finish();
        assert!(!lock.exists());
        assert!(!ws.join(".git").join("HEAD.lock").exists());
        assert_eq!(std::fs::read(ws.join(".git").join("HEAD")).unwrap(), head);
        // The repository still works.
        t.git(&ws, &["add", "--all"]);
        assert_eq!(t.git(&ws, &["rev-list", "--count", "HEAD"]).trim(), "1");

        // An unfinished GitOps gives the lock back when dropped.
        drop(GitOps::begin(&ws));
        assert!(!lock.exists());
    }
}
