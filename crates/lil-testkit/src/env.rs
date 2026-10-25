//! `TestEnv`: an `Env` whose every root lives under a fresh temporary directory (QA-038,
//! QA-040).
//!
//! Creating a `TestEnv` arms the process-global harness guard in `lil-platform`. From then on,
//! any `Env` method in the process that would return the owner's real `<App>` location, under
//! the new or the legacy app name (roaming or local `<App>*` folders, a per-volume local-data
//! folder, the real registry root or credential prefix), panics with
//! `test resolved a real LILITH location: ...`, and building a stable or dev `Env` panics too.
//!
//! A `TestEnv` also runs the real `lil` with the child environment of its test channel
//! ([`TestEnv::lil`]), lays out workspaces and state databases, and on drop drains any core
//! still serving its channel (QA-041), so no core outlives its test and no temporary folder
//! stays locked. It then removes its folder with [`crate::cleanup`], which clears sealed
//! read-only files, never follows a junction out of the folder, retries briefly while another
//! handle still holds a file, and reports anything it had to leave. The registry key a killed
//! test leaves is swept by a later process's first `TestEnv` ([`sweep_test_registry_roots`]).

use std::ffi::OsString;
use std::hash::{BuildHasher, RandomState};
use std::io::{BufRead as _, Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock, PoisonError, mpsc};
use std::time::{Duration, Instant};

use lil_platform::{
    Channel, ENV_CHANNEL, ENV_TEST_DOCTOR_STDIN, ENV_TEST_PARENT_PID, ENV_TEST_ROOT, Env,
    RealLocations, Roots, TEST_PIPE_SID, arm_test_guard, names,
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
    /// Set once this env started a `lil` that may outlive the call ([`TestEnv::lil_spawn`]) or a
    /// core ([`crate::CoreFixture`]): the drop then gives a pipe that is not there yet a short
    /// retry, since such a process may still be starting.
    pub(crate) launched: AtomicBool,
    /// The temporary directory. Removed by this type's drop (or [`TestEnv::close`]), not by
    /// `tempfile`, whose removal stops at the first locked file and discards the error.
    root: PathBuf,
    /// Set by [`TestEnv::close`]: the drop then has nothing left to do.
    closed: bool,
}

/// What a finished `lil` run printed and returned.
#[derive(Debug, Clone)]
pub struct LilOutput {
    /// The exit code; -1 if `lil` ended without one.
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    /// Wall time from start to exit. For reports only, never for assertions (QA-031).
    pub elapsed: Duration,
}

impl TestEnv {
    /// Arms the guard with the real locations, then builds the isolated `Env`. Its folder is
    /// `%TEMP%\lil-testenv-<id>-<random>`, `<id>` being its channel's. The first one in a
    /// process sweeps the registry keys killed tests left ([`sweep_test_registry_roots`]).
    ///
    /// # Panics
    /// If the known folders cannot be resolved or the temporary directory cannot be created.
    pub fn new() -> TestEnv {
        TestEnv::make("lil-testenv-".to_string())
    }

    /// [`TestEnv::new`], with `label` in the temporary directory's name
    /// (`lil-testenv-<label>-<id>-<random>`). Spaces and non-ASCII letters are kept, so paths
    /// with them get exercised.
    pub fn with_label(label: &str) -> TestEnv {
        TestEnv::make(format!("lil-testenv-{label}-"))
    }

    fn make(prefix: String) -> TestEnv {
        static REAL: OnceLock<RealLocations> = OnceLock::new();
        static SWEPT: Once = Once::new();
        let real = REAL.get_or_init(|| {
            RealLocations::from_system().expect("cannot resolve the real known folders")
        });
        arm_test_guard(real.clone());
        SWEPT.call_once(|| {
            if let Err(e) = sweep_test_registry_roots() {
                eprintln!("the sweep of leftover test registry keys failed: {e}");
            }
        });

        // The id in the folder's name lets the sweep match a registry key to its folder.
        let id = unique_id();
        let root = tempfile::Builder::new()
            .prefix(&format!("{prefix}{id}-"))
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
        let env = Env::with_roots(Channel::Test(id), roots, TEST_PIPE_SID.to_string());
        TestEnv {
            env,
            drain_counts: Arc::default(),
            launched: AtomicBool::new(false),
            root,
            closed: false,
        }
    }

    /// The temporary directory that holds every root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The variables that put a child process on this test channel: `LIL_CHANNEL=test-<id>`,
    /// `LIL_TEST_ROOT=<root>` and `LIL_TEST_PARENT_PID=<this process>`, then
    /// [`TestEnv::git_env`], so every `lil` and core a test starts, and every `git` they run,
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
            // Tests answer `lil doctor` through a pipe (a person's `lil doctor` takes answers
            // only from a terminal).
            (OsString::from(ENV_TEST_DOCTOR_STDIN), OsString::from("1")),
        ];
        vars.extend(self.git_env());
        vars
    }

    /// Runs the test build of `lil` with `args`, in [`TestEnv::root`], with
    /// [`TestEnv::child_env`] and an empty stdin, and waits for it. `lil` itself runs through
    /// `std::process::Command` because its output must be captured; any core it starts goes
    /// through `lil-platform`'s detached spawn and inherits none of these pipes.
    ///
    /// # Panics
    /// If `lil` cannot be started; if it runs longer than 120 s (it is killed); or if its
    /// stdout or stderr stays open after it exited (a process it started inherited them). The
    /// message carries what `lil` printed so far.
    pub fn lil(&self, args: &[&str]) -> LilOutput {
        self.run_lil(self.root(), args, None)
    }

    /// [`TestEnv::lil`] run in the folder `dir` instead of [`TestEnv::root`] (the `cli`
    /// workspace chain looks at the current folder, ARCH-030).
    pub fn lil_in(&self, dir: &Path, args: &[&str]) -> LilOutput {
        self.run_lil(dir, args, None)
    }

    /// [`TestEnv::lil`] with `stdin` written to `lil`'s standard input, which is then closed.
    pub fn lil_with_stdin(&self, args: &[&str], stdin: &str) -> LilOutput {
        self.run_lil(self.root(), args, Some(stdin))
    }

    /// [`TestEnv::lil`] run in `dir`, with the variables in `extra` added after
    /// [`TestEnv::child_env`] (each replaces one of the same name) and `stdin`, if any, written
    /// to `lil`'s standard input: for `lil hook` and `lil doctor` runs that need the fake
    /// `claude` on `PATH` or `LIL_ENGINE`.
    pub fn lil_in_with(
        &self,
        dir: &Path,
        args: &[&str],
        extra: &[(&str, OsString)],
        stdin: Option<&str>,
    ) -> LilOutput {
        self.run_lil_with(dir, args, extra, stdin)
    }

    /// Starts `lil` like [`TestEnv::lil`] but does not wait: for long-running commands such as
    /// `lil events --follow`. Stdin is empty; stdout is read line by line through
    /// [`LilChild::stdout_lines`], and stderr is drained on a thread of its own
    /// ([`LilChild::stderr_so_far`]). Dropping the [`LilChild`] kills `lil` if it still runs.
    ///
    /// # Panics
    /// If `lil` cannot be started.
    pub fn lil_spawn(&self, args: &[&str]) -> LilChild {
        self.launched.store(true, Ordering::Relaxed);
        let mut cmd = self.lil_command(self.root(), args);
        LilChild::spawn(&mut cmd, format!("lil {}", args.join(" ")))
    }

    fn lil_command(&self, dir: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new(&ensure_built().lil);
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

    fn run_lil(&self, dir: &Path, args: &[&str], stdin: Option<&str>) -> LilOutput {
        self.run_lil_with(dir, args, &[], stdin)
    }

    fn run_lil_with(
        &self,
        dir: &Path,
        args: &[&str],
        extra: &[(&str, OsString)],
        stdin: Option<&str>,
    ) -> LilOutput {
        let lil = &ensure_built().lil;
        let start = Instant::now();
        let deadline = start + LIL_DEADLINE;
        let mut command = self.lil_command(dir, args);
        for (name, value) in extra {
            command.env(name, value);
        }
        let mut child = command
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("cannot start {}: {e}", lil.display()));
        // Written from another thread, so a `lil` that prints before it reads cannot deadlock.
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
                "`lil {}` {what}\nstdout so far: {}\nstderr so far: {}",
                args.join(" "),
                stdout.text(),
                stderr.text()
            )
        };

        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(e) => panic!("waiting for {} failed: {e}", lil.display()),
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                stdout.wait_closed(PIPE_GRACE);
                stderr.wait_closed(PIPE_GRACE);
                panic!(
                    "{}",
                    report(
                        &format!("did not exit within {LIL_DEADLINE:?}; it was killed"),
                        &stdout,
                        &stderr
                    )
                );
            }
            std::thread::sleep(POLL);
        };
        // `lil` has exited, so its pipes close unless a process it started inherited them
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
        LilOutput {
            code: status.code().unwrap_or(-1),
            stdout: stdout.text(),
            stderr: stderr.text(),
            elapsed: start.elapsed(),
        }
    }

    /// Creates `<root>\ws-<name>\lilith.yaml` containing `name: <name>` and returns the
    /// workspace folder.
    pub fn make_workspace(&self, name: &str) -> PathBuf {
        self.make_workspace_with(name, names::WORKSPACE_FILE)
    }

    /// A workspace from before the rename (R4): `<root>\ws-<name>\` with only the legacy
    /// workspace file (`names::LEGACY_WORKSPACE_FILE`) containing `name: <name>`.
    pub fn make_legacy_workspace(&self, name: &str) -> PathBuf {
        self.make_workspace_with(name, names::LEGACY_WORKSPACE_FILE)
    }

    fn make_workspace_with(&self, name: &str, file: &str) -> PathBuf {
        let ws = self.root().join(format!("ws-{name}"));
        std::fs::create_dir_all(&ws)
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", ws.display()));
        let manifest = ws.join(file);
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

    /// Merges `toml_fragment` into this channel's `suite.toml` (created when missing): tables
    /// merge key by key, at any depth, and every other value in the fragment replaces the one
    /// in the file. Comments in the file are not kept.
    ///
    /// # Panics
    /// If the fragment or the file is not TOML, or the file cannot be written.
    pub fn suite_toml_set(&self, toml_fragment: &str) {
        fn merge(into: &mut toml::Table, from: toml::Table) {
            for (key, value) in from {
                match (into.get_mut(&key), value) {
                    (Some(toml::Value::Table(old)), toml::Value::Table(new)) => merge(old, new),
                    (_, value) => {
                        into.insert(key, value);
                    }
                }
            }
        }
        let fragment: toml::Table = toml_fragment
            .parse()
            .unwrap_or_else(|e| panic!("the suite.toml fragment is not TOML: {e}"));
        let file = self.env.suite_toml();
        let mut table: toml::Table = match std::fs::read_to_string(&file) {
            Ok(text) => text
                .parse()
                .unwrap_or_else(|e| panic!("{} is not TOML: {e}", file.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
            Err(e) => panic!("cannot read {}: {e}", file.display()),
        };
        merge(&mut table, fragment);
        let text = toml::to_string(&table).expect("a TOML table serialises");
        crate::files::write_file(&file, text.as_bytes());
    }

    /// This process's `PATH` with the test binaries' folder first, for a child that must find
    /// the fake `claude.exe` (or another test binary) by name. The real `PATH` is never changed.
    ///
    /// # Panics
    /// If a `PATH` entry holds a `;`.
    pub fn path_with_testbins(&self) -> OsString {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let dirs = std::iter::once(ensure_built().dir.clone()).chain(std::env::split_paths(&path));
        std::env::join_paths(dirs).expect("PATH entries without ';'")
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

    /// Lays out the SQLite database `path` as a crash mid-write leaves it: `path` with a hot
    /// rollback journal `<path>-journal` beside it. The database (journal mode `delete`,
    /// `user_version` set) holds table `t` with [`HOT_JOURNAL_ROWS`] rows whose `x` starts with
    /// `before`; a transaction then rewrote every row to start with `after`, with a cache small
    /// enough that SQLite wrote changed pages into the file before the commit, and the files
    /// were copied at that moment. A read-write open rolls the journal back, after which every
    /// `x` starts with `before` again.
    ///
    /// # Panics
    /// If a step fails or the journal comes out empty.
    pub fn write_hot_journal_db(&self, path: &Path, user_version: i64) {
        let dir = path.parent().expect("the database has a folder");
        std::fs::create_dir_all(dir)
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", dir.display()));
        let live = dir.join("hot-journal-source.db");
        let live_journal = dir.join("hot-journal-source.db-journal");
        let db = rusqlite::Connection::open(&live)
            .unwrap_or_else(|e| panic!("cannot open {}: {e}", live.display()));
        let padding = "x".repeat(1000);
        db.execute_batch(
            "PRAGMA journal_mode = DELETE; PRAGMA cache_size = 8;
             CREATE TABLE t (n INTEGER PRIMARY KEY, x TEXT NOT NULL);",
        )
        .expect("set up the source database");
        db.execute_batch("BEGIN").unwrap();
        for n in 0..HOT_JOURNAL_ROWS {
            db.execute(
                "INSERT INTO t (n, x) VALUES (?1, ?2)",
                rusqlite::params![n, format!("before {padding}")],
            )
            .unwrap();
        }
        db.execute_batch("COMMIT").unwrap();
        db.pragma_update(None, "user_version", user_version)
            .unwrap();
        db.execute_batch("BEGIN; UPDATE t SET x = 'after' || substr(x, 7);")
            .expect("the open transaction");
        let journal = std::fs::read(&live_journal)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", live_journal.display()));
        assert!(!journal.is_empty(), "the open transaction wrote no journal");
        let bytes =
            std::fs::read(&live).unwrap_or_else(|e| panic!("cannot read {}: {e}", live.display()));
        std::fs::write(path, bytes)
            .unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
        let mut journal_path = path.as_os_str().to_owned();
        journal_path.push("-journal");
        std::fs::write(&journal_path, journal)
            .unwrap_or_else(|e| panic!("cannot write the journal of {}: {e}", path.display()));
        db.execute_batch("ROLLBACK").unwrap();
        drop(db);
        for leftover in [&live, &live_journal] {
            let _ = std::fs::remove_file(leftover);
        }
    }
}

/// The rows of [`TestEnv::write_hot_journal_db`]'s table `t`.
pub const HOT_JOURNAL_ROWS: i64 = 300;

impl Default for TestEnv {
    fn default() -> Self {
        TestEnv::new()
    }
}

impl TestEnv {
    /// What the drop does, now, for a fixture that has closed everything that held files in
    /// the folder (its hub and state databases) and wants to know the folder went: drains any
    /// core still on the channel, removes the folder (retrying for up to 2 s while another
    /// handle holds a file), and returns what it had to leave, each entry as `<path>: <error>`.
    /// Leftovers are also reported as the drop reports them.
    ///
    /// # Errors
    /// The entries left behind.
    pub fn close(mut self) -> Result<(), Vec<String>> {
        self.closed = true;
        self.clean_up()
    }

    /// The drop's work; `Err` lists what was left (already reported).
    fn clean_up(&self) -> Result<(), Vec<String>> {
        crate::core::drain_channel(
            &self.env.pipe_name(),
            self.launched.load(Ordering::Relaxed),
            Arc::clone(&self.drain_counts),
        );
        crate::cleanup::remove_tree(&self.root, crate::cleanup::RETRY_FOR).map_err(|left| {
            crate::cleanup::report_leftovers(&self.root, &left);
            left.iter()
                .map(|(path, e)| format!("{}: {e}", path.display()))
                .collect()
        })
    }
}

impl Drop for TestEnv {
    /// Drains any core still serving this channel, for example one that `lil` started and this
    /// process holds no handle for, and waits up to 5 s for it to go (QA-041). Then removes the
    /// temporary directory with [`crate::cleanup::remove_tree`], retrying for up to 2 s while
    /// another handle still holds a file, and reports what it had to leave on stderr and in
    /// [`crate::cleanup::leftovers_log`]. Never panics. After [`TestEnv::close`] it does nothing.
    fn drop(&mut self) {
        if !self.closed {
            let _ = self.clean_up();
        }
    }
}

/// Removes the registry roots `HKCU\Software\LILITH-test-<id>` that tests killed before their
/// cleanup left (nextest's `terminate-after`; Task 3's review M9): each one
/// `lil_platform::desktop::test_registry_ids` lists for which no `lil-testenv-*<id>-*` folder is
/// in the system temporary folder. A key whose folder is there belongs to a test still running,
/// in this process or another, and is kept. The keys are listed before the folders, so a test
/// that starts in between is seen with its folder. Nothing else is touched: not
/// `HKCU\Software\LILITH`, not `HKCU\Software\Classes`, not a key of another shape. Returns the
/// keys removed.
///
/// # Errors
/// When the keys or the temporary folder can't be listed, or a key can't be removed.
pub fn sweep_test_registry_roots() -> std::io::Result<Vec<String>> {
    let ids = lil_platform::desktop::test_registry_ids()?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let prefix = "lil-testenv-";
    let folders: Vec<String> = std::fs::read_dir(std::env::temp_dir())?
        .filter_map(|e| Some(e.ok()?.file_name().to_string_lossy().into_owned()))
        .filter_map(|n| n.strip_prefix(prefix).map(str::to_string))
        .collect();
    let scratch = std::env::temp_dir().join("lil-registry-sweep");
    let mut removed = Vec::new();
    for id in ids {
        let tail = format!("{id}-");
        if folders.iter().any(|f| f.contains(&tail)) {
            continue;
        }
        let env = Env::with_roots(
            Channel::Test(id),
            Roots {
                roaming: scratch.clone(),
                local: scratch.clone(),
                user_profile: scratch.clone(),
            },
            TEST_PIPE_SID.to_string(),
        );
        lil_platform::desktop::remove_test_registry(&env)?;
        removed.push(env.registry_root());
    }
    Ok(removed)
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

/// How long one [`TestEnv::lil`] run may take before `lil` is killed and the test fails. A hang
/// guard, never a measurement (QA-031). It is a constant rather than test-params.toml's `e2e_s`
/// (300 s): that value is nextest's own stop for the whole test, so a per-call guard must fire
/// well before it. 120 s covers the client's longest wait (a 60 s start cap, `lil core stop`'s
/// default 60 s).
const LIL_DEADLINE: Duration = Duration::from_secs(120);
/// How long to wait for `lil`'s pipes to close once it has exited or been killed. They close
/// at once unless another process holds them.
const PIPE_GRACE: Duration = Duration::from_secs(10);
/// Pause between checks of whether `lil` has exited.
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

/// A `lil` (or other child) started by [`TestEnv::lil_spawn`]. Its stdout arrives line by line
/// through [`LilChild::stdout_lines`]; its stderr is drained on a thread, so the child never
/// blocks on a full pipe, and every panic here carries what it printed there. Dropping it kills
/// the child if it still runs and waits for it.
pub struct LilChild {
    child: Child,
    what: String,
    lines: StdoutLines,
    stderr: Captured,
}

/// [`LilChild`]'s stdout lines: `None` once stdout has closed.
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

impl LilChild {
    /// Starts `cmd` with stdin empty and stdout and stderr piped.
    pub(crate) fn spawn(cmd: &mut Command, what: String) -> LilChild {
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
        LilChild {
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

impl Drop for LilChild {
    /// Kills the child if it still runs, then waits for it. Never panics.
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// How long [`LilChild::stdout_lines`] waits for a line before it fails the test.
const SPAWN_LINE_DEADLINE: Duration = Duration::from_secs(30);

#[cfg(test)]
mod tests {
    use super::*;
    use lil_platform::{Channel, Env, RealLocations, Roots, parse_registered_workspaces};
    use std::ffi::OsString;

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
    #[should_panic(expected = "real LILITH location")]
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
    #[should_panic(expected = "real LILITH location")]
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
                .starts_with(r"HKCU\Software\LILITH-test-")
        );
        assert!(t.env.credential_prefix().starts_with("LILITH-test-"));
        assert!(t.env.pipe_name().contains("-test-"));
        assert!(t.env.notification_aumid().starts_with("LILITH.Suite.Test-"));
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
            get("LIL_CHANNEL"),
            OsString::from(t.env.channel().to_string())
        );
        assert!(get("LIL_CHANNEL").to_string_lossy().starts_with("test-"));
        assert_eq!(get("LIL_TEST_ROOT"), t.root().as_os_str());
        assert_eq!(
            get("LIL_TEST_PARENT_PID"),
            OsString::from(std::process::id().to_string())
        );
        assert_eq!(get("LIL_TEST_DOCTOR_STDIN"), OsString::from("1"));
        // The three channel variables and the doctor's test override come first; the git
        // isolation follows them.
        assert_eq!(vars.len(), 4 + t.git_env().len(), "{vars:?}");
        assert_eq!(&vars[4..], t.git_env().as_slice(), "{vars:?}");

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

    /// The new `suite.toml` wins whenever it exists, even with a legacy one beside it (R4).
    #[test] // spec: CORE-007, ARCH-090
    fn suite_toml_prefers_the_new_file() {
        let t = TestEnv::new();
        let new = t.env.suite_toml();
        let legacy = t.env.legacy_data_dir().join("suite.toml");
        assert_ne!(new, legacy);
        assert!(new.starts_with(t.root()) && legacy.starts_with(t.root()));
        // Neither exists: the new name (the file a write creates).
        assert_eq!(t.env.suite_toml_to_read(), new);
        for (file, text) in [
            (&legacy, "default_workspace = 'legacy'\n"),
            (&new, "x = 1\n"),
        ] {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
        assert_eq!(t.env.suite_toml_to_read(), new);
        // Writes never name the legacy file.
        assert_eq!(t.env.suite_toml(), new);
    }

    /// With only the legacy `suite.toml` there, it is the one read (R4), from the legacy folder
    /// of the same channel.
    #[test] // spec: CORE-007, ARCH-090
    fn suite_toml_reads_the_legacy_file_when_the_new_one_is_missing() {
        let t = TestEnv::new();
        let legacy = t.env.legacy_data_dir().join("suite.toml");
        let channel = t.env.channel().to_string();
        assert_eq!(
            legacy
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy(),
            format!("{}-{channel}", lil_platform::names::LEGACY_APP_NAME)
        );
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "default_workspace = 'legacy'\n").unwrap();
        assert_eq!(t.env.suite_toml_to_read(), legacy);
        assert!(!t.env.suite_toml().exists());
        assert!(!t.env.data_dir().exists(), "reading created the new folder");
    }

    #[test] // spec: QA-040
    fn make_and_register_workspace_writes_suite_toml() {
        let t = TestEnv::with_label("Ünïcödé space");
        let a = t.make_workspace("alpha");
        let b = t.make_workspace("beta");
        assert_eq!(a, t.root().join("ws-alpha"));
        assert_eq!(
            std::fs::read_to_string(a.join("lilith.yaml")).unwrap(),
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
            .prefix("lil-testkit-junction-target-")
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

    /// After this env launched a `lil` or a core, a pipe that is not there yet gets the short
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
    const PRINT_ENV_ENV: &str = "LIL_TESTKIT_HELPER_PRINT_ENV";
    /// The variables `helper_print_env` reports.
    const PRINTED_VARS: &[&str] = &["GIT_DIR", "GIT_CONFIG_PARAMETERS", "LIL_CHANNEL", "HOME"];

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
    /// which stands in for an inherited one), so a `lil`, the core it starts and their `git`
    /// runs never act on another repository.
    #[test] // spec: QA-038
    fn lil_children_do_not_inherit_git_dir() {
        let t = TestEnv::new();
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["env::tests::helper_print_env", "--exact", "--nocapture"])
            .env("GIT_DIR", t.root().join("elsewhere").join(".git"))
            .env("GIT_CONFIG_PARAMETERS", "'core.autocrlf'='true'");
        t.apply_child_env(&mut cmd);
        cmd.env(PRINT_ENV_ENV, "1");
        let mut child = LilChild::spawn(&mut cmd, "helper".to_string());
        let lines: Vec<String> = child.stdout_lines().collect();
        assert_eq!(child.wait(Duration::from_secs(30)), Some(0));
        for want in [
            "GIT_DIR unset".to_string(),
            "GIT_CONFIG_PARAMETERS unset".to_string(),
            format!("LIL_CHANNEL={}", t.env.channel()),
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
    fn lil_child_is_killed_on_drop() {
        // The test binary is the long-running child (ruling P6), started as `lil_spawn` starts
        // `lil`, with output piped.
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([crate::core::HELPER_SLEEP_TEST, "--exact", "--nocapture"])
            .env(crate::core::HELPER_SLEEP_ENV, "1");
        let mut child = LilChild::spawn(&mut cmd, "helper".to_string());
        let probe = lil_platform::process::open(child.pid()).expect("open the helper");
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
            .expect("the helper still runs after its LilChild was dropped")
            .unwrap();
        assert_eq!(code, 1, "TerminateProcess exit code");
    }

    /// Set in the helper child's environment: `helper_print_lines` prints and returns.
    const PRINT_ENV: &str = "LIL_TESTKIT_HELPER_PRINT";

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
    fn lil_child_reads_stdout_lines_and_drains_stderr() {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["env::tests::helper_print_lines", "--exact", "--nocapture"])
            .env(PRINT_ENV, "1");
        let mut child = LilChild::spawn(&mut cmd, "helper".to_string());
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
