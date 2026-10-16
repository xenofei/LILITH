//! `TestEnv`: an `Env` whose every root lives under a fresh temporary directory (QA-038,
//! QA-040).
//!
//! Creating a `TestEnv` arms the process-global harness guard in `ww-platform`. From then on,
//! any `Env` method in the process that would return the owner's real Worldwright location
//! (roaming or local `Worldwright*` folders, `.worldwright-local`, the real registry root or
//! credential prefix) panics with `test resolved a real Worldwright location: ...`, and
//! building a stable or dev `Env` panics too.
//!
//! A `TestEnv` also runs the real `ww` with the child environment of its test channel
//! ([`TestEnv::ww`]), lays out workspaces and state databases, and on drop drains any core
//! still serving its channel (QA-041), so no core outlives its test and no temporary folder
//! stays locked.

use std::ffi::OsString;
use std::hash::{BuildHasher, RandomState};
use std::io::{BufRead as _, Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, mpsc};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use ww_platform::{
    Channel, ENV_CHANNEL, ENV_TEST_PARENT_PID, ENV_TEST_ROOT, Env, RealLocations, Roots,
    TEST_PIPE_SID, arm_test_guard,
};

use crate::bins::ensure_built;
use crate::core::DrainCounts;
use crate::gitops::LEAKY_GIT_VARS;

/// An isolated `Env`: channel `Test(<8 lowercase hex>)`, roots under a fresh temporary
/// directory that is deleted on drop.
pub struct TestEnv {
    pub env: Env,
    /// What the drop's cleanup did (probes, retries and hellos), shared with the test that
    /// checks it.
    drain_counts: Arc<DrainCounts>,
    /// Set once this env started a `ww` that may outlive the call ([`TestEnv::ww_spawn`]) or a
    /// core ([`crate::CoreFixture`]): the drop then gives a pipe that is not there yet a short
    /// retry, since such a process may still be starting.
    pub(crate) launched: AtomicBool,
    _dir: TempDir,
}

/// What a finished `ww` run printed and returned.
#[derive(Debug, Clone)]
pub struct WwOutput {
    /// The exit code; -1 if `ww` ended without one.
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    /// Wall time from start to exit. For reports only, never for assertions (QA-031).
    pub elapsed: Duration,
}

impl TestEnv {
    /// Arms the guard with the real locations, then builds the isolated `Env`.
    ///
    /// # Panics
    /// If the known folders cannot be resolved or the temporary directory cannot be created.
    pub fn new() -> TestEnv {
        TestEnv::make("ww-testenv-".to_string())
    }

    /// [`TestEnv::new`], with `label` in the temporary directory's name. Spaces and non-ASCII
    /// letters are kept, so paths with them get exercised.
    pub fn with_label(label: &str) -> TestEnv {
        TestEnv::make(format!("ww-testenv-{label}-"))
    }

    fn make(prefix: String) -> TestEnv {
        static REAL: OnceLock<RealLocations> = OnceLock::new();
        let real = REAL.get_or_init(|| {
            RealLocations::from_system().expect("cannot resolve the real known folders")
        });
        arm_test_guard(real.clone());

        let dir = tempfile::Builder::new()
            .prefix(&prefix)
            .tempdir()
            .expect("cannot create the test directory");
        let roots = Roots {
            roaming: dir.path().join("roaming"),
            local: dir.path().join("local"),
            user_profile: dir.path().join("profile"),
        };
        for r in [&roots.roaming, &roots.local, &roots.user_profile] {
            std::fs::create_dir_all(r).expect("cannot create a test root");
        }
        let env = Env::with_roots(Channel::Test(unique_id()), roots, TEST_PIPE_SID.to_string());
        TestEnv {
            env,
            drain_counts: Arc::default(),
            launched: AtomicBool::new(false),
            _dir: dir,
        }
    }

    /// The temporary directory that holds every root.
    pub fn root(&self) -> &Path {
        self._dir.path()
    }

    /// The variables that put a child process on this test channel: `WW_CHANNEL=test-<id>`,
    /// `WW_TEST_ROOT=<root>` and `WW_TEST_PARENT_PID=<this process>`, then
    /// [`TestEnv::git_env`], so every `ww` and core a test starts, and every `git` they run,
    /// is isolated from the system and user git config (QA-038). The same type as
    /// `SpawnRequest::extra_env`.
    pub fn child_env(&self) -> Vec<(OsString, OsString)> {
        let mut vars = vec![
            (
                OsString::from(ENV_CHANNEL),
                OsString::from(self.env.channel().to_string()),
            ),
            (
                OsString::from(ENV_TEST_ROOT),
                self.root().as_os_str().to_owned(),
            ),
            (
                OsString::from(ENV_TEST_PARENT_PID),
                OsString::from(std::process::id().to_string()),
            ),
        ];
        vars.extend(self.git_env());
        vars
    }

    /// Runs the test build of `ww` with `args`, in [`TestEnv::root`], with
    /// [`TestEnv::child_env`] and an empty stdin, and waits for it. `ww` itself runs through
    /// `std::process::Command` because its output must be captured; any core it starts goes
    /// through `ww-platform`'s detached spawn and inherits none of these pipes.
    ///
    /// # Panics
    /// If `ww` cannot be started; if it runs longer than 120 s (it is killed); or if its
    /// stdout or stderr stays open after it exited (a process it started inherited them). The
    /// message carries what `ww` printed so far.
    pub fn ww(&self, args: &[&str]) -> WwOutput {
        self.run_ww(self.root(), args, None)
    }

    /// [`TestEnv::ww`] run in the folder `dir` instead of [`TestEnv::root`] (the `cli`
    /// workspace chain looks at the current folder, ARCH-030).
    pub fn ww_in(&self, dir: &Path, args: &[&str]) -> WwOutput {
        self.run_ww(dir, args, None)
    }

    /// [`TestEnv::ww`] with `stdin` written to `ww`'s standard input, which is then closed.
    pub fn ww_with_stdin(&self, args: &[&str], stdin: &str) -> WwOutput {
        self.run_ww(self.root(), args, Some(stdin))
    }

    /// Starts `ww` like [`TestEnv::ww`] but does not wait: for long-running commands such as
    /// `ww events --follow`. Stdin is empty; stdout is read line by line through
    /// [`WwChild::stdout_lines`], and stderr is drained on a thread of its own
    /// ([`WwChild::stderr_so_far`]). Dropping the [`WwChild`] kills `ww` if it still runs.
    ///
    /// # Panics
    /// If `ww` cannot be started.
    pub fn ww_spawn(&self, args: &[&str]) -> WwChild {
        self.launched.store(true, Ordering::Relaxed);
        let mut cmd = self.ww_command(self.root(), args);
        WwChild::spawn(&mut cmd, format!("ww {}", args.join(" ")))
    }

    fn ww_command(&self, dir: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new(&ensure_built().ww);
        cmd.args(args).current_dir(dir);
        self.apply_child_env(&mut cmd);
        cmd
    }

    /// Gives `cmd` this channel's environment: removes [`LEAKY_GIT_VARS`] (inherited or set
    /// on `cmd`), then adds [`TestEnv::child_env`].
    pub(crate) fn apply_child_env(&self, cmd: &mut Command) {
        for var in LEAKY_GIT_VARS {
            cmd.env_remove(var);
        }
        cmd.envs(self.child_env());
    }

    fn run_ww(&self, dir: &Path, args: &[&str], stdin: Option<&str>) -> WwOutput {
        let ww = &ensure_built().ww;
        let start = Instant::now();
        let deadline = start + WW_DEADLINE;
        let mut child = self
            .ww_command(dir, args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("cannot start {}: {e}", ww.display()));
        // Written from another thread, so a `ww` that prints before it reads cannot deadlock.
        let writer = match (stdin, child.stdin.take()) {
            (Some(text), Some(mut pipe)) => {
                let text = text.to_string();
                Some(std::thread::spawn(move || {
                    let _ = pipe.write_all(text.as_bytes());
                }))
            }
            _ => None,
        };
        let stdout = Captured::read(child.stdout.take());
        let stderr = Captured::read(child.stderr.take());
        let report = |what: &str, stdout: &Captured, stderr: &Captured| -> String {
            format!(
                "`ww {}` {what}\nstdout so far: {}\nstderr so far: {}",
                args.join(" "),
                stdout.text(),
                stderr.text()
            )
        };

        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(e) => panic!("waiting for {} failed: {e}", ww.display()),
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                stdout.wait_closed(PIPE_GRACE);
                stderr.wait_closed(PIPE_GRACE);
                panic!(
                    "{}",
                    report(
                        &format!("did not exit within {WW_DEADLINE:?}; it was killed"),
                        &stdout,
                        &stderr
                    )
                );
            }
            std::thread::sleep(POLL);
        };
        // `ww` has exited, so its pipes close unless a process it started inherited them
        // (spec Appendix D: a core never holds the caller's output).
        if !(stdout.wait_closed(PIPE_GRACE) && stderr.wait_closed(PIPE_GRACE)) {
            panic!(
                "{}",
                report(
                    &format!(
                        "exited ({status}), but its stdout or stderr stayed open: a process it started inherited them"
                    ),
                    &stdout,
                    &stderr
                )
            );
        }
        if let Some(writer) = writer {
            let _ = writer.join();
        }
        WwOutput {
            code: status.code().unwrap_or(-1),
            stdout: stdout.text(),
            stderr: stderr.text(),
            elapsed: start.elapsed(),
        }
    }

    /// Creates `<root>\ws-<name>\worldwright.yaml` containing `name: <name>` and returns the
    /// workspace folder.
    pub fn make_workspace(&self, name: &str) -> PathBuf {
        let ws = self.root().join(format!("ws-{name}"));
        std::fs::create_dir_all(&ws)
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", ws.display()));
        let manifest = ws.join("worldwright.yaml");
        std::fs::write(&manifest, format!("name: {name}\n"))
            .unwrap_or_else(|e| panic!("cannot write {}: {e}", manifest.display()));
        ws
    }

    /// Registers `ws` in this channel's `data_dir\suite.toml`: adds a `[[workspaces]]` entry
    /// unless one has this path, and makes it `default_workspace` when `default` is true.
    /// Other keys in the file are kept. The path is written as given.
    ///
    /// # Panics
    /// If the file cannot be read or written, or `ws` is not valid UTF-8.
    pub fn register_workspace(&self, ws: &Path, default: bool) {
        let file = self.env.suite_toml();
        let path = ws
            .to_str()
            .unwrap_or_else(|| panic!("{} is not valid UTF-8", ws.display()))
            .to_string();
        let mut table: toml::Table = match std::fs::read_to_string(&file) {
            Ok(text) => text
                .parse()
                .unwrap_or_else(|e| panic!("{} is not TOML: {e}", file.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
            Err(e) => panic!("cannot read {}: {e}", file.display()),
        };
        let entries = table
            .entry("workspaces")
            .or_insert_with(|| toml::Value::Array(Vec::new()))
            .as_array_mut()
            .unwrap_or_else(|| panic!("`workspaces` in {} is not an array", file.display()));
        let listed = entries
            .iter()
            .any(|e| e.get("path").and_then(toml::Value::as_str) == Some(path.as_str()));
        if !listed {
            let mut entry = toml::Table::new();
            entry.insert("path".into(), toml::Value::String(path.clone()));
            entries.push(toml::Value::Table(entry));
        }
        if default {
            table.insert("default_workspace".into(), toml::Value::String(path));
        }
        let text = toml::to_string(&table).expect("a TOML table serialises");
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|e| panic!("cannot create {}: {e}", parent.display()));
        }
        std::fs::write(&file, text)
            .unwrap_or_else(|e| panic!("cannot write {}: {e}", file.display()));
    }

    /// Creates or opens the SQLite database at `path` (creating its parent folders, ruling
    /// P13) and sets its `user_version` and its journal mode: `wal` when `wal` is true, else
    /// `delete`. No tables are created. Used to fake a newer or an older state
    /// database.
    ///
    /// # Panics
    /// If the folder or the database cannot be written.
    pub fn write_state_db_version(&self, path: &Path, user_version: i64, wal: bool) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|e| panic!("cannot create {}: {e}", parent.display()));
        }
        let db = rusqlite::Connection::open(path)
            .unwrap_or_else(|e| panic!("cannot open {}: {e}", path.display()));
        let wanted = if wal { "wal" } else { "delete" };
        let mode: String = db
            .query_row(&format!("PRAGMA journal_mode = {wanted}"), [], |r| r.get(0))
            .unwrap_or_else(|e| panic!("cannot set the journal mode of {}: {e}", path.display()));
        assert_eq!(mode.to_ascii_lowercase(), wanted, "{}", path.display());
        db.pragma_update(None, "user_version", user_version)
            .unwrap_or_else(|e| panic!("cannot set user_version of {}: {e}", path.display()));
        db.close()
            .unwrap_or_else(|(_, e)| panic!("cannot close {}: {e}", path.display()));
    }
}

impl Default for TestEnv {
    fn default() -> Self {
        TestEnv::new()
    }
}

impl Drop for TestEnv {
    /// Drains any core still serving this channel, for example one that `ww` started and this
    /// process holds no handle for, and waits up to 5 s for it to go (QA-041). Runs before the
    /// temporary directory is removed. Never panics.
    fn drop(&mut self) {
        crate::core::drain_channel(
            &self.env.pipe_name(),
            self.launched.load(Ordering::Relaxed),
            Arc::clone(&self.drain_counts),
        );
    }
}

/// 8 lowercase hex characters, distinct per call within a process and random across processes.
/// Lowercase only: Windows paths, registry keys and pipe names are case-insensitive, so mixed
/// case ids could collide.
fn unique_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let h = RandomState::new().hash_one((std::process::id(), n));
    format!("{:08x}", h as u32)
}

/// How long one [`TestEnv::ww`] run may take before `ww` is killed and the test fails. A hang
/// guard, never a measurement (QA-031). It is a constant rather than test-params.toml's `e2e_s`
/// (300 s): that value is nextest's own stop for the whole test, so a per-call guard must fire
/// well before it. 120 s covers the client's longest wait (a 60 s start cap, `ww core stop`'s
/// default 60 s).
const WW_DEADLINE: Duration = Duration::from_secs(120);
/// How long to wait for `ww`'s pipes to close once it has exited or been killed. They close
/// at once unless another process holds them.
const PIPE_GRACE: Duration = Duration::from_secs(10);
/// Pause between checks of whether `ww` has exited.
const POLL: Duration = Duration::from_millis(10);

/// A child's output pipe, read to its end on a thread of its own. What arrived so far stays
/// readable even while the pipe is still open.
struct Captured {
    bytes: Arc<Mutex<Vec<u8>>>,
    closed: mpsc::Receiver<()>,
}

impl Captured {
    fn read(pipe: Option<impl Read + Send + 'static>) -> Captured {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let (done, closed) = mpsc::channel();
        let sink = Arc::clone(&bytes);
        std::thread::spawn(move || {
            if let Some(mut pipe) = pipe {
                let mut chunk = [0_u8; 8192];
                while let Ok(n) = pipe.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    sink.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .extend_from_slice(&chunk[..n]);
                }
            }
            let _ = done.send(());
        });
        Captured { bytes, closed }
    }

    /// True once the pipe has closed (now or within `within`).
    fn wait_closed(&self, within: Duration) -> bool {
        match self.closed.recv_timeout(within) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => true,
            Err(mpsc::RecvTimeoutError::Timeout) => false,
        }
    }

    fn text(&self) -> String {
        text_of(&self.bytes)
    }
}

fn text_of(bytes: &Mutex<Vec<u8>>) -> String {
    String::from_utf8_lossy(&bytes.lock().unwrap_or_else(PoisonError::into_inner)).into_owned()
}

/// A `ww` (or other child) started by [`TestEnv::ww_spawn`]. Its stdout arrives line by line
/// through [`WwChild::stdout_lines`]; its stderr is drained on a thread, so the child never
/// blocks on a full pipe, and every panic here carries what it printed there. Dropping it kills
/// the child if it still runs and waits for it.
pub struct WwChild {
    child: Child,
    what: String,
    lines: StdoutLines,
    stderr: Captured,
}

/// [`WwChild`]'s stdout lines: `None` once stdout has closed.
struct StdoutLines {
    rx: mpsc::Receiver<String>,
    what: String,
    stderr: Arc<Mutex<Vec<u8>>>,
}

impl Iterator for StdoutLines {
    type Item = String;

    /// The next line, without its line ending.
    ///
    /// # Panics
    /// If none arrives within 30 s (a failure deadline, never a measurement); the message
    /// carries the child's stderr so far.
    fn next(&mut self) -> Option<String> {
        match self.rx.recv_timeout(SPAWN_LINE_DEADLINE) {
            Ok(line) => Some(line),
            Err(mpsc::RecvTimeoutError::Disconnected) => None,
            Err(mpsc::RecvTimeoutError::Timeout) => panic!(
                "no stdout line from `{}` within {SPAWN_LINE_DEADLINE:?}\nstderr so far: {}",
                self.what,
                text_of(&self.stderr)
            ),
        }
    }
}

impl WwChild {
    /// Starts `cmd` with stdin empty and stdout and stderr piped.
    pub(crate) fn spawn(cmd: &mut Command, what: String) -> WwChild {
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("cannot start `{what}`: {e}"));
        let stderr = Captured::read(child.stderr.take());
        let stdout = child.stdout.take();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let Some(stdout) = stdout else { return };
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let lines = StdoutLines {
            rx,
            what: what.clone(),
            stderr: Arc::clone(&stderr.bytes),
        };
        WwChild {
            child,
            what,
            lines,
            stderr,
        }
    }

    /// The child's stdout, line by line; it ends when stdout closes. Each `next` panics if no
    /// line arrives within 30 s, with the child's stderr in the message.
    pub fn stdout_lines(&mut self) -> &mut dyn Iterator<Item = String> {
        &mut self.lines
    }

    /// What the child wrote to stderr so far.
    pub fn stderr_so_far(&self) -> String {
        self.stderr.text()
    }

    /// The child's process id.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// The exit code once the child has exited (-1 if it ended without one), or `None` if it
    /// still runs after `within`, a failure deadline.
    ///
    /// # Panics
    /// If waiting fails; the message carries the child's stderr.
    pub fn wait(&mut self, within: Duration) -> Option<i32> {
        let deadline = Instant::now() + within;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return Some(status.code().unwrap_or(-1)),
                Ok(None) => {}
                Err(e) => panic!(
                    "waiting for `{}` failed: {e}\nstderr so far: {}",
                    self.what,
                    self.stderr_so_far()
                ),
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(POLL);
        }
    }
}

impl Drop for WwChild {
    /// Kills the child if it still runs, then waits for it. Never panics.
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// How long [`WwChild::stdout_lines`] waits for a line before it fails the test.
const SPAWN_LINE_DEADLINE: Duration = Duration::from_secs(30);

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use ww_platform::{Channel, Env, RealLocations, Roots, parse_registered_workspaces};

    #[test] // spec: QA-040
    fn testenv_paths_are_under_tempdir() {
        let t = TestEnv::new();
        let base = t.root().to_path_buf();
        for p in [
            t.env.data_dir(),
            t.env.local_dir(),
            t.env.logs_dir(),
            t.env.claude_profiles_dir(),
            t.env.claude_default_profile_dir(),
        ] {
            assert!(
                p.starts_with(&base),
                "{} not under {}",
                p.display(),
                base.display()
            );
        }
        assert!(matches!(t.env.channel(), Channel::Test(_)));
    }

    #[test] // spec: QA-040
    fn testenv_ids_are_lowercase_alphanumeric_and_distinct() {
        let a = TestEnv::new();
        let b = TestEnv::new();
        let id = |t: &TestEnv| match t.env.channel() {
            Channel::Test(id) => id.clone(),
            other => panic!("not a test channel: {other}"),
        };
        for t in [&a, &b] {
            let id = id(t);
            assert_eq!(id.len(), 8);
            assert!(
                id.bytes()
                    .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase()),
                "{id}"
            );
        }
        assert_ne!(id(&a), id(&b));
        assert_ne!(a.env.data_dir(), b.env.data_dir());
    }

    #[test] // spec: QA-040
    #[should_panic(expected = "real Worldwright location")]
    fn guard_panics_on_real_appdata() {
        let _t = TestEnv::new(); // arms the guard
        let real = RealLocations::from_system().expect("known folders");
        let roots = Roots {
            roaming: real.roaming.clone(),
            local: real.local.clone(),
            user_profile: real.local.clone(),
        };
        let env = Env::with_roots(Channel::Test("abcd1234".into()), roots, "S-1".into());
        let _ = env.data_dir();
    }

    #[test] // spec: QA-040
    #[should_panic(expected = "real Worldwright location")]
    fn guard_panics_on_stable_channel_in_tests() {
        let t = TestEnv::new();
        let roots = Roots {
            roaming: t.root().join("r"),
            local: t.root().join("l"),
            user_profile: t.root().join("u"),
        };
        let _ = Env::with_roots(Channel::Stable, roots, "S-1".into());
    }

    #[test] // spec: QA-040
    fn registry_and_credentials_are_test_scoped() {
        let t = TestEnv::new();
        assert!(
            t.env
                .registry_root()
                .starts_with(r"HKCU\Software\Worldwright-test-")
        );
        assert!(t.env.credential_prefix().starts_with("Worldwright-test-"));
        assert!(t.env.pipe_name().contains("-test-"));
        assert!(
            t.env
                .notification_aumid()
                .starts_with("Worldwright.Suite.Test-")
        );
    }

    #[test] // spec: QA-040
    fn child_env_names_test_channel_root_and_parent() {
        let t = TestEnv::new();
        let vars = t.child_env();
        let get = |name: &str| {
            let found: Vec<&OsString> = vars
                .iter()
                .filter(|(k, _)| k == name)
                .map(|(_, v)| v)
                .collect();
            assert_eq!(found.len(), 1, "{name} in {vars:?}");
            found[0].clone()
        };
        assert_eq!(
            get("WW_CHANNEL"),
            OsString::from(t.env.channel().to_string())
        );
        assert!(get("WW_CHANNEL").to_string_lossy().starts_with("test-"));
        assert_eq!(get("WW_TEST_ROOT"), t.root().as_os_str());
        assert_eq!(
            get("WW_TEST_PARENT_PID"),
            OsString::from(std::process::id().to_string())
        );
        // The three channel variables come first; the git isolation follows them.
        assert_eq!(vars.len(), 3 + t.git_env().len(), "{vars:?}");
        assert_eq!(&vars[3..], t.git_env().as_slice(), "{vars:?}");

        // A child given these variables resolves the same isolated folders.
        let child = Env::from_vars("dev", &|name| {
            vars.iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.to_string_lossy().into_owned())
        })
        .expect("a child Env from child_env");
        assert_eq!(child.channel(), t.env.channel());
        assert_eq!(child.data_dir(), t.env.data_dir());
        assert_eq!(child.local_dir(), t.env.local_dir());
        assert_eq!(child.pipe_name(), t.env.pipe_name());
    }

    #[test] // spec: QA-040
    fn with_label_keeps_spaces_and_non_ascii() {
        let t = TestEnv::with_label("Ünïcödé space");
        let root = t.root().to_string_lossy().into_owned();
        assert!(root.contains("Ünïcödé space"), "{root}");
        assert!(t.root().join("roaming").is_dir());
        assert!(t.env.data_dir().starts_with(t.root()));
    }

    #[test] // spec: QA-040
    fn make_and_register_workspace_writes_suite_toml() {
        let t = TestEnv::with_label("Ünïcödé space");
        let a = t.make_workspace("alpha");
        let b = t.make_workspace("beta");
        assert_eq!(a, t.root().join("ws-alpha"));
        assert_eq!(
            std::fs::read_to_string(a.join("worldwright.yaml")).unwrap(),
            "name: alpha
"
        );
        t.register_workspace(&a, false);
        t.register_workspace(&b, true);
        t.register_workspace(&a, false); // registering twice keeps one entry
        let text = std::fs::read_to_string(t.env.suite_toml()).unwrap();
        // Default first, then the listed paths in order, without repeats.
        assert_eq!(
            parse_registered_workspaces(&text),
            vec![b.clone(), a.clone()]
        );
        let table: toml::Table = text.parse().unwrap();
        assert_eq!(
            table["default_workspace"].as_str(),
            Some(b.to_str().unwrap())
        );
        assert_eq!(table["workspaces"].as_array().unwrap().len(), 2, "{text}");
    }

    #[test] // spec: QA-040
    fn write_state_db_version_sets_user_version_and_journal_mode() {
        let t = TestEnv::new();
        let read = |path: &Path| -> (i64, String) {
            let db = rusqlite::Connection::open(path).unwrap();
            let v: i64 = db
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            let mode: String = db
                .query_row("PRAGMA journal_mode", [], |r| r.get(0))
                .unwrap();
            (v, mode)
        };
        // The parent folder does not exist yet (ruling P13).
        let wal = t.env.suite_state_db();
        assert!(!wal.parent().unwrap().exists());
        t.write_state_db_version(&wal, 7, true);
        assert_eq!(read(&wal), (7, "wal".to_string()));

        let plain = t.root().join("nested").join("deeper").join("state.db");
        t.write_state_db_version(&plain, -3, false);
        assert_eq!(read(&plain), (-3, "delete".to_string()));
    }

    #[test] // spec: QA-040
    fn second_testenv_does_not_trip_the_guard() {
        let _a = TestEnv::new();
        let b = TestEnv::new();
        let _ = b
            .env
            .workspace_local_dir(b.root())
            .expect("same-volume workspace");
    }

    /// A fresh `TestEnv` with no core on its channel: the drop probes the pipe once and makes
    /// no hello attempt (checked through the drop's own counters, not a timer).
    #[test] // spec: QA-041
    fn drop_without_a_core_returns_without_waiting() {
        let t = TestEnv::new();
        let counts = Arc::clone(&t.drain_counts);
        drop(t);
        assert_eq!(counts.probes.load(Ordering::Relaxed), 1);
        assert_eq!(counts.retries.load(Ordering::Relaxed), 0);
        assert_eq!(counts.hellos.load(Ordering::Relaxed), 0);
    }

    /// After this env launched a `ww` or a core, a pipe that is not there yet gets the short
    /// retry (it may still be starting); with none there, still no hello.
    #[test] // spec: QA-041
    fn drop_after_a_launch_retries_a_missing_pipe() {
        let t = TestEnv::new();
        t.launched.store(true, Ordering::Relaxed);
        let counts = Arc::clone(&t.drain_counts);
        drop(t);
        assert_eq!(counts.probes.load(Ordering::Relaxed), 1);
        assert_eq!(counts.retries.load(Ordering::Relaxed), 1);
        assert_eq!(counts.hellos.load(Ordering::Relaxed), 0);
    }

    /// Set in the helper child's environment: `helper_print_env` prints the variables below.
    const PRINT_ENV_ENV: &str = "WW_TESTKIT_HELPER_PRINT_ENV";
    /// The variables `helper_print_env` reports.
    const PRINTED_VARS: &[&str] = &["GIT_DIR", "GIT_CONFIG_PARAMETERS", "WW_CHANNEL", "HOME"];

    /// With `PRINT_ENV_ENV` set, prints `<name>=<value>` or `<name> unset` for each of
    /// `PRINTED_VARS`; otherwise does nothing.
    #[test]
    fn helper_print_env() {
        if std::env::var_os(PRINT_ENV_ENV).is_none() {
            return;
        }
        for name in PRINTED_VARS {
            match std::env::var_os(name) {
                Some(value) => println!("{name}={}", value.to_string_lossy()),
                None => println!("{name} unset"),
            }
        }
    }

    /// A child given this channel's environment never sees a `GIT_DIR` or
    /// `GIT_CONFIG_PARAMETERS` from the test's own environment (here set on the `Command`,
    /// which stands in for an inherited one), so a `ww`, the core it starts and their `git`
    /// runs never act on another repository.
    #[test] // spec: QA-038
    fn ww_children_do_not_inherit_git_dir() {
        let t = TestEnv::new();
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["env::tests::helper_print_env", "--exact", "--nocapture"])
            .env("GIT_DIR", t.root().join("elsewhere").join(".git"))
            .env("GIT_CONFIG_PARAMETERS", "'core.autocrlf'='true'");
        t.apply_child_env(&mut cmd);
        cmd.env(PRINT_ENV_ENV, "1");
        let mut child = WwChild::spawn(&mut cmd, "helper".to_string());
        let lines: Vec<String> = child.stdout_lines().collect();
        assert_eq!(child.wait(Duration::from_secs(30)), Some(0));
        for want in [
            "GIT_DIR unset".to_string(),
            "GIT_CONFIG_PARAMETERS unset".to_string(),
            format!("WW_CHANNEL={}", t.env.channel()),
            format!("HOME={}", t.root().join("profile").display()),
        ] {
            assert!(lines.contains(&want), "no `{want}` in {lines:?}");
        }
        for var in LEAKY_GIT_VARS {
            assert!(
                !t.child_env().iter().any(|(k, _)| k == var),
                "{var} must be removed, never set"
            );
        }
    }

    #[test] // spec: QA-041
    fn ww_child_is_killed_on_drop() {
        // The test binary is the long-running child (ruling P6), started as `ww_spawn` starts
        // `ww`, with output piped.
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([crate::core::HELPER_SLEEP_TEST, "--exact", "--nocapture"])
            .env(crate::core::HELPER_SLEEP_ENV, "1");
        let mut child = WwChild::spawn(&mut cmd, "helper".to_string());
        let probe = ww_platform::process::open(child.pid()).expect("open the helper");
        assert_eq!(
            probe.exit_code().unwrap(),
            None,
            "the helper exited by itself"
        );
        assert_eq!(child.wait(Duration::ZERO), None);

        drop(child);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // Drop already waited; the deadline only bounds a failure.
        let code = rt
            .block_on(async { tokio::time::timeout(Duration::from_secs(10), probe.wait()).await })
            .expect("the helper still runs after its WwChild was dropped")
            .unwrap();
        assert_eq!(code, 1, "TerminateProcess exit code");
    }

    /// Set in the helper child's environment: `helper_print_lines` prints and returns.
    const PRINT_ENV: &str = "WW_TESTKIT_HELPER_PRINT";

    /// With `PRINT_ENV` set, prints one line to stdout and one to stderr; otherwise does
    /// nothing.
    #[test]
    fn helper_print_lines() {
        if std::env::var_os(PRINT_ENV).is_some() {
            println!("out-line Ünïcödé space");
            eprintln!("err-line Ünïcödé space");
        }
    }

    #[test] // spec: QA-056
    fn ww_child_reads_stdout_lines_and_drains_stderr() {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["env::tests::helper_print_lines", "--exact", "--nocapture"])
            .env(PRINT_ENV, "1");
        let mut child = WwChild::spawn(&mut cmd, "helper".to_string());
        let lines: Vec<String> = child.stdout_lines().collect();
        assert!(
            lines.iter().any(|l| l == "out-line Ünïcödé space"),
            "{lines:?}"
        );
        assert_eq!(child.wait(Duration::from_secs(30)), Some(0));
        // The stderr thread may finish just after the exit; wait for the pipe to close.
        assert!(child.stderr.wait_closed(Duration::from_secs(30)));
        let stderr = child.stderr_so_far();
        assert!(stderr.contains("err-line Ünïcödé space"), "{stderr}");
        assert!(!stderr.contains("out-line"), "{stderr}");
    }
}
