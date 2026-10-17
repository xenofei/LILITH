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
//! stays locked. It then removes its folder with [`crate::cleanup`], which clears sealed
//! read-only files, never follows a junction out of the folder, retries briefly while another
//! handle still holds a file, and reports anything it had to leave.

use std::ffi::OsString;
use std::hash::{BuildHasher, RandomState};
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, mpsc};
use std::time::{Duration, Instant};

use ww_platform::{
    Channel, ENV_CHANNEL, ENV_TEST_PARENT_PID, ENV_TEST_ROOT, Env, RealLocations, Roots,
    TEST_PIPE_SID, arm_test_guard,
};

use crate::bins::ensure_built;

/// An isolated `Env`: channel `Test(<8 lowercase hex>)`, roots under a fresh temporary
/// directory that is deleted on drop.
pub struct TestEnv {
    pub env: Env,
    /// The temporary directory. Removed by this type's drop, not by `tempfile`, whose removal
    /// stops at the first locked file and discards the error.
    root: PathBuf,
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

        let root = tempfile::Builder::new()
            .prefix(&prefix)
            .tempdir()
            .expect("cannot create the test directory")
            .keep();
        let roots = Roots {
            roaming: root.join("roaming"),
            local: root.join("local"),
            user_profile: root.join("profile"),
        };
        for r in [&roots.roaming, &roots.local, &roots.user_profile] {
            std::fs::create_dir_all(r).expect("cannot create a test root");
        }
        let env = Env::with_roots(Channel::Test(unique_id()), roots, TEST_PIPE_SID.to_string());
        TestEnv { env, root }
    }

    /// The temporary directory that holds every root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The variables that put a child process on this test channel: `WW_CHANNEL=test-<id>`,
    /// `WW_TEST_ROOT=<root>` and `WW_TEST_PARENT_PID=<this process>`. The same type as
    /// `SpawnRequest::extra_env`.
    pub fn child_env(&self) -> Vec<(OsString, OsString)> {
        vec![
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
        ]
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
    /// `ww events --follow`. Stdout and stderr are piped (read stdout as it comes, stderr
    /// after the child has ended); stdin is empty. The caller kills or waits for the child.
    ///
    /// # Panics
    /// If `ww` cannot be started.
    pub fn ww_spawn(&self, args: &[&str]) -> Child {
        self.ww_command(self.root(), args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("cannot start {}: {e}", ensure_built().ww.display()))
    }

    fn ww_command(&self, dir: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new(&ensure_built().ww);
        cmd.args(args).current_dir(dir).envs(self.child_env());
        cmd
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
    /// process holds no handle for, and waits up to 5 s for it to go (QA-041). Then removes the
    /// temporary directory with [`crate::cleanup::remove_tree`], retrying for up to 2 s while
    /// another handle still holds a file, and reports what it had to leave on stderr and in
    /// [`crate::cleanup::leftovers_log`]. Never panics.
    fn drop(&mut self) {
        crate::core::drain_channel(&self.env.pipe_name());
        if let Err(left) = crate::cleanup::remove_tree(&self.root, crate::cleanup::RETRY_FOR) {
            crate::cleanup::report_leftovers(&self.root, &left);
        }
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
        String::from_utf8_lossy(&self.bytes.lock().unwrap_or_else(PoisonError::into_inner))
            .into_owned()
    }
}

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
        assert_eq!(vars.len(), 3, "{vars:?}");

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

    fn is_read_only(path: &Path) -> bool {
        std::fs::metadata(path).unwrap().permissions().readonly()
    }

    fn set_read_only(path: &Path) {
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    #[test] // spec: QA-040
    fn drop_removes_sealed_read_only_files_and_junctions_without_following_them() {
        // A folder outside the TestEnv that a junction inside it points to, holding a
        // read-only file that must stay exactly as it is.
        let outside = tempfile::Builder::new()
            .prefix("ww-testkit-junction-target-")
            .tempdir()
            .unwrap();
        let kept = outside.path().join("kept.txt");
        std::fs::write(&kept, "outside").unwrap();
        set_read_only(&kept);

        let t = TestEnv::with_label("Ünïcödé space");
        let root = t.root().to_path_buf();
        // A sealed version: read-only files in a read-only folder.
        let sealed = t
            .make_workspace("sealed")
            .join("projects")
            .join("sketchbook")
            .join("assets")
            .join("crane")
            .join("v1.0.0");
        std::fs::create_dir_all(&sealed).unwrap();
        for name in ["crane.png", "manifest.snapshot.json"] {
            std::fs::write(sealed.join(name), name).unwrap();
            set_read_only(&sealed.join(name));
        }
        set_read_only(&sealed);
        // A junction from inside the root to the outside folder.
        let link = root.join("ws-sealed").join("linked");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(outside.path())
            .output()
            .unwrap();
        assert!(made.status.success(), "mklink /J: {made:?}");
        assert!(
            link.join("kept.txt").is_file(),
            "the junction does not resolve"
        );
        assert!(is_read_only(&sealed.join("crane.png")));

        drop(t);

        assert!(!root.exists(), "{} was left behind", root.display());
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "outside");
        assert!(
            is_read_only(&kept),
            "the read-only flag was cleared through the junction"
        );
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
}
