//! Core fixtures: start a real `ww-core`, and make sure none outlives its test (QA-056).
//!
//! Every core is started through `ww_platform::process::spawn_detached`, never
//! `std::process::Command`, which would let it inherit the test harness's capture pipes
//! (ARCH-019, Global Constraints: dependency boundaries). The returned `ProcessHandle` is kept,
//! so the fixture can wait for and kill exactly that process.
//!
//! Cleanup:
//! - [`CoreFixture`]'s drop connects, says hello and sends `core/drain`. A core that answers
//!   with a JSON-RPC error (-32601 or -32001 before the drain exists) or does not exit within
//!   5 s is killed.
//! - [`KillOnDrop`] kills its process on drop.
//! - `TestEnv`'s drop drains any core still on its channel ([`drain_channel`]), which covers
//!   cores that `ww` started.

use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use ww_platform::pipe::{ConnectError, open_client};
use ww_platform::process::{ProcessHandle, SpawnRequest, spawn_detached};

use crate::bins::ensure_built;
use crate::env::TestEnv;
use crate::gitops::LEAKY_GIT_VARS;
use crate::raw_pipe::{RawPipe, merge_hello};

/// How long [`CoreFixture::start`] waits for the core's pipe.
const PIPE_DEADLINE: Duration = Duration::from_secs(30);
/// How long a drain may take before the core is killed, and how long cleanup waits for a
/// killed process.
const DRAIN_WAIT: Duration = Duration::from_secs(5);
/// How long cleanup waits for a hello or drain response.
const CLEANUP_CALL: Duration = Duration::from_secs(5);
/// How long `CoreFixture`'s drop tries to connect to a core that still runs.
const FIXTURE_CONNECT: Duration = Duration::from_secs(2);
/// How long `TestEnv`'s drop retries a busy pipe on its channel, or a missing one after it
/// launched a `ww` or core.
const ENV_CONNECT: Duration = Duration::from_millis(200);
/// Pause between polls of a pipe.
const POLL: Duration = Duration::from_millis(20);

/// A running `ww-core` on a test channel, started by this process.
#[derive(Debug)]
pub struct CoreFixture {
    handle: ProcessHandle,
    pipe: String,
}

impl CoreFixture {
    /// Starts the test build of `ww-core` on `env`'s channel and waits up to 30 s for its
    /// pipe.
    ///
    /// # Panics
    /// If `ww-core.exe` is missing, cannot be started, exits early or opens no pipe in time.
    pub fn start(env: &TestEnv) -> CoreFixture {
        CoreFixture::start_exe(env, &ensure_built().ww_core)
    }

    /// [`CoreFixture::start`] with another core executable, for example a copy of
    /// `ww-core.exe` in another folder.
    pub fn start_exe(env: &TestEnv, exe: &Path) -> CoreFixture {
        CoreFixture::launch(env, exe, &[])
    }

    /// [`CoreFixture::start`] with extra variables; each replaces a `child_env` variable of
    /// the same name (compared case-insensitively).
    pub fn start_with_env(env: &TestEnv, extra: &[(&str, &str)]) -> CoreFixture {
        CoreFixture::launch(env, &ensure_built().ww_core, extra)
    }

    /// Starts the test build of `ww-core` on `env`'s channel without waiting for anything,
    /// for tests of how it exits.
    ///
    /// # Panics
    /// If `ww-core.exe` is missing or cannot be started.
    pub fn spawn_raw(env: &TestEnv) -> KillOnDrop {
        CoreFixture::spawn_raw_with_env(env, &[])
    }

    /// [`CoreFixture::spawn_raw`] with extra variables; each replaces a `child_env` variable
    /// of the same name (compared case-insensitively).
    pub fn spawn_raw_with_env(env: &TestEnv, extra: &[(&str, &str)]) -> KillOnDrop {
        KillOnDrop(spawn_core(env, &ensure_built().ww_core, extra))
    }

    fn launch(env: &TestEnv, exe: &Path, extra: &[(&str, &str)]) -> CoreFixture {
        let handle = spawn_core(env, exe, extra);
        let pipe = env.env.pipe_name();
        wait_for_pipe(&handle, &pipe, exe);
        CoreFixture { handle, pipe }
    }

    pub fn pid(&self) -> u32 {
        self.handle.pid()
    }

    /// The core's exit code once it has exited, or `None` if it still runs after `within`.
    /// `within` is a failure deadline, never a measurement.
    pub fn wait_exit(&mut self, within: Duration) -> Option<u32> {
        wait_handle(&self.handle, within)
    }
}

impl Drop for CoreFixture {
    fn drop(&mut self) {
        if matches!(self.handle.exit_code(), Ok(Some(_))) {
            return;
        }
        let pipe = self.pipe.clone();
        let asked = off_runtime(move || ask_drain(&pipe, FIXTURE_CONNECT));
        if asked == Some(DrainAsk::Accepted) && wait_handle(&self.handle, DRAIN_WAIT).is_some() {
            return;
        }
        let _ = self.handle.kill();
        let _ = wait_handle(&self.handle, DRAIN_WAIT);
    }
}

/// A process that is killed when this is dropped.
#[derive(Debug)]
pub struct KillOnDrop(pub ProcessHandle);

impl KillOnDrop {
    /// The exit code once the process has exited, or `None` if it still runs after `within`.
    /// `within` is a failure deadline, never a measurement.
    pub fn wait_exit(&self, within: Duration) -> Option<u32> {
        wait_handle(&self.0, within)
    }
}

impl Drop for KillOnDrop {
    /// Kills the process if it still runs, then waits up to 5 s for it to end.
    fn drop(&mut self) {
        if !matches!(self.0.exit_code(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = wait_handle(&self.0, DRAIN_WAIT);
        }
    }
}

/// `spawn_detached` of `exe` with `env.child_env()`, `extra` replacing same-named variables,
/// and [`LEAKY_GIT_VARS`] removed from what the core inherits (QA-038): a test run from inside
/// a git hook never gives the core a `GIT_DIR` that points its `git` runs at another
/// repository.
fn spawn_core(env: &TestEnv, exe: &Path, extra: &[(&str, &str)]) -> ProcessHandle {
    // Set first, so a start that fails later (no pipe in time) still gets the drop's retry.
    env.launched
        .store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(
        exe.is_file(),
        "the core executable {} does not exist; it is the `ww-core` binary of crates/ww-core, \
         built into target\\testbins by ww_testkit::bins::ensure_built",
        exe.display()
    );
    let mut vars = env.child_env();
    for (name, value) in extra {
        vars.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
        vars.push((OsString::from(name), OsString::from(value)));
    }
    spawn_detached(&SpawnRequest {
        exe: exe.to_path_buf(),
        args: Vec::new(),
        extra_env: vars,
        remove_env: LEAKY_GIT_VARS.iter().map(OsString::from).collect(),
    })
    .unwrap_or_else(|e| panic!("cannot start {}: {e}", exe.display()))
}

/// Waits until `pipe` exists. Kills the core and panics if it does not appear in time; panics
/// if the core exits first.
fn wait_for_pipe(handle: &ProcessHandle, pipe: &str, exe: &Path) {
    let rt = runtime();
    let deadline = Instant::now() + PIPE_DEADLINE;
    loop {
        // A connection that opens is closed again at once; a busy pipe exists too.
        match rt.block_on(async { open_client(pipe).map(drop) }) {
            Ok(()) | Err(ConnectError::Busy) => return,
            Err(_) => {}
        }
        if let Ok(Some(code)) = handle.exit_code() {
            panic!(
                "{} exited with code {code} before it opened {pipe}",
                exe.display()
            );
        }
        if Instant::now() >= deadline {
            let _ = handle.kill();
            panic!(
                "{} did not open {pipe} within {PIPE_DEADLINE:?}; it was killed",
                exe.display()
            );
        }
        std::thread::sleep(POLL);
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("cannot build a tokio runtime")
}

/// The exit code once `handle`'s process has exited, or `None` after `within`. Never panics.
fn wait_handle(handle: &ProcessHandle, within: Duration) -> Option<u32> {
    if let Ok(Some(code)) = handle.exit_code() {
        return Some(code);
    }
    off_runtime(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        rt.block_on(async { tokio::time::timeout(within, handle.wait()).await })
            .ok()?
            .ok()
    })
    .flatten()
}

/// Runs `f` here, or on a helper thread when this thread is inside a tokio runtime (an async
/// test dropping a fixture), where blocking on another runtime would panic. `None` if `f`
/// panicked on the helper thread.
fn off_runtime<T: Send>(f: impl FnOnce() -> T + Send) -> Option<T> {
    if tokio::runtime::Handle::try_current().is_err() {
        return Some(f());
    }
    std::thread::scope(|s| s.spawn(f).join().ok())
}

/// What a core said when asked to drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrainAsk {
    /// No pipe opened within the connect time.
    NoCore,
    /// The pipe is served by this very process (an in-process core of an async test).
    InProcess,
    /// `core/drain` returned an ok `Outcome`.
    Accepted,
    /// Anything else: a JSON-RPC error (-32601 or -32001 before the drain exists), an error
    /// `Outcome`, no answer, or a broken connection.
    Refused,
}

/// Connects to `pipe`, says hello and asks for `core/drain`. Never panics.
fn ask_drain(pipe: &str, connect_within: Duration) -> DrainAsk {
    let Some(conn) = RawPipe::connect_name(pipe, connect_within) else {
        return DrainAsk::NoCore;
    };
    drain_over(conn, None)
}

/// Says hello on `conn` and asks for `core/drain`, counting the hello in `counts`. Never
/// panics.
fn drain_over(mut conn: RawPipe, counts: Option<&DrainCounts>) -> DrainAsk {
    if conn.server_pid() == Some(std::process::id()) {
        return DrainAsk::InProcess;
    }
    if let Some(counts) = counts {
        counts.hellos.fetch_add(1, Ordering::Relaxed);
    }
    if conn
        .try_call(
            ww_protocol::method::HELLO,
            merge_hello(Value::Null),
            CLEANUP_CALL,
        )
        .is_err()
    {
        return DrainAsk::Refused;
    }
    match conn.try_call(ww_protocol::method::DRAIN, json!({}), CLEANUP_CALL) {
        Ok(response) if response["result"]["status"] == "ok" => DrainAsk::Accepted,
        _ => DrainAsk::Refused,
    }
}

/// What `TestEnv`'s drop did, for the test that checks it never waits without a core.
#[derive(Debug, Default)]
pub(crate) struct DrainCounts {
    /// `open_client` probes of the channel's pipe.
    pub(crate) probes: AtomicUsize,
    /// Short retries of a missing pipe after a launch.
    pub(crate) retries: AtomicUsize,
    /// `core/hello` attempts.
    pub(crate) hellos: AtomicUsize,
}

/// `TestEnv`'s cleanup. One `open_client` probe of `pipe`. No pipe (`NotFound`) returns at
/// once, unless the env `launched` a `ww` or core, which may still be starting: then, as for a
/// busy pipe, connecting is retried for up to 200 ms. A core that answers gets hello and
/// `core/drain`; if it accepts, this waits up to 5 s for the pipe to disappear. An in-process
/// core (served by this process) is left to its test. Never panics.
pub(crate) fn drain_channel(pipe: &str, launched: bool, counts: Arc<DrainCounts>) {
    let pipe = pipe.to_string();
    off_runtime(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        counts.probes.fetch_add(1, Ordering::Relaxed);
        let conn = match rt.block_on(async { open_client(&pipe) }) {
            Ok(client) => RawPipe::from_client(rt, client),
            Err(ConnectError::Busy) => match RawPipe::connect_name(&pipe, ENV_CONNECT) {
                Some(conn) => conn,
                None => return,
            },
            Err(ConnectError::NotFound) if launched => {
                counts.retries.fetch_add(1, Ordering::Relaxed);
                match RawPipe::connect_name(&pipe, ENV_CONNECT) {
                    Some(conn) => conn,
                    None => return,
                }
            }
            Err(_) => return,
        };
        if drain_over(conn, Some(&counts)) != DrainAsk::Accepted {
            return;
        }
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        let deadline = Instant::now() + DRAIN_WAIT;
        while Instant::now() < deadline {
            if let Err(ConnectError::NotFound) = rt.block_on(async { open_client(&pipe).map(drop) })
            {
                return;
            }
            std::thread::sleep(POLL);
        }
    });
}

/// Set in a helper child's environment: the test `helper_sleep_forever` then sleeps. Shared
/// by every test in the crate that needs a long-running child.
#[cfg(test)]
pub(crate) const HELPER_SLEEP_ENV: &str = "WW_TESTKIT_HELPER_SLEEP";
/// The libtest name of that helper, for `<test binary> <name> --exact`.
#[cfg(test)]
pub(crate) const HELPER_SLEEP_TEST: &str = "core::tests::helper_sleep_forever";

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::time::Duration;
    use ww_platform::process::{self, SpawnRequest, spawn_detached};

    use super::{HELPER_SLEEP_ENV as HELPER_ENV, HELPER_SLEEP_TEST};

    /// The test binary doubles as a long-running child (ruling P6): with `HELPER_ENV` set this
    /// test sleeps and never exits on its own within any test's lifetime; without it, it
    /// returns at once.
    #[test]
    fn helper_sleep_forever() {
        if std::env::var_os(HELPER_ENV).is_none() {
            return;
        }
        // A safety bound only, far beyond every test deadline, in case a kill is ever missed.
        for _ in 0..600 {
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    #[test] // spec: QA-056
    fn kill_on_drop_kills_a_running_child() {
        let handle = spawn_detached(&SpawnRequest {
            exe: std::env::current_exe().unwrap(),
            args: vec![OsString::from(HELPER_SLEEP_TEST), OsString::from("--exact")],
            extra_env: vec![(OsString::from(HELPER_ENV), OsString::from("1"))],
            remove_env: Vec::new(),
        })
        .expect("spawn the helper");
        // A separately held handle, so the check does not depend on the guard's own handle.
        let probe = process::open(handle.pid()).expect("open the helper");
        assert_eq!(
            probe.exit_code().unwrap(),
            None,
            "the helper exited by itself"
        );

        drop(KillOnDrop(handle));

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // Drop already waited; the deadline only bounds a failure.
        let code = rt
            .block_on(async { tokio::time::timeout(Duration::from_secs(10), probe.wait()).await })
            .expect("the helper still runs after KillOnDrop was dropped")
            .unwrap();
        assert_eq!(code, 1, "TerminateProcess exit code");
        assert!(probe.exit_code().unwrap().is_some());
    }
}
