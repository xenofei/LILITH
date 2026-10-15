//! Process inspection, owned process handles and the detached, non-inheriting spawn
//! (ARCH-019, ARCH-027).
//!
//! **Why not `std::process::Command`.** On Windows it always calls `CreateProcessW` with
//! `bInheritHandles = TRUE`. A caller that captures `ww`'s output (a script, Claude Code) hands
//! `ww` inheritable pipe handles; a core spawned through `Command` would inherit them and keep
//! the caller waiting for end-of-file until the core exits. [`SystemSpawner`] calls
//! `CreateProcessW` itself with `bInheritHandles = FALSE` and no `STARTF_USESTDHANDLES` (the
//! core is a Windows-subsystem exe and needs no stdio).

use std::ffi::{OsStr, OsString, c_void};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, HANDLE, INVALID_HANDLE_VALUE,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Environment::{FreeEnvironmentStringsW, GetEnvironmentStringsW};
use windows::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_UNICODE_ENVIRONMENT,
    CreateProcessW, DETACHED_PROCESS, GetCurrentProcess, GetExitCodeProcess, INFINITE, OpenProcess,
    PROCESS_ACCESS_RIGHTS, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    QueryFullProcessImageNameW, RegisterWaitForSingleObject, STARTUPINFOW, TerminateProcess,
    UnregisterWaitEx, WT_EXECUTEONLYONCE, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

use crate::sid;

/// A Windows error as an `io::Error` whose `raw_os_error()` is the Win32 code (e.g. 5 for
/// access denied), not the HRESULT the `windows` crate carries.
pub(crate) fn io_error(e: &windows::core::Error) -> io::Error {
    let hr = e.code().0 as u32;
    if hr & 0xFFFF_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((hr & 0xFFFF) as i32)
    } else {
        io::Error::from_raw_os_error(hr as i32)
    }
}

fn invalid_input(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, what.to_string())
}

// ---------------------------------------------------------------------------------------------
// Inspection (ARCH-027).
// ---------------------------------------------------------------------------------------------

/// What a client needs to know about a pipe peer before trusting it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    /// The full Win32 path of the process image.
    pub image: PathBuf,
    /// The SID string of the process token's user.
    pub user_sid: String,
    /// From a Toolhelp snapshot; `None` when the process is not in it or has no parent.
    pub parent_pid: Option<u32>,
}

/// Reads [`ProcessInfo`]; a trait so tests can stand in for the system.
pub trait ProcessInspector: Send + Sync + std::fmt::Debug {
    fn inspect(&self, pid: u32) -> io::Result<ProcessInfo>;
}

/// The real inspector.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemInspector;

impl ProcessInspector for SystemInspector {
    fn inspect(&self, pid: u32) -> io::Result<ProcessInfo> {
        // Open first, before anything else, so the PID can't be reused under us (ARCH-027).
        let process = open_raw(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
        let raw = HANDLE(process.as_raw_handle());
        let image = image_path(raw)?;
        let user_sid = sid::token_user_sid(raw).map_err(|(_, e)| io_error(&e))?;
        let parent_pid = parent_pid(pid)?;
        Ok(ProcessInfo {
            pid,
            image,
            user_sid,
            parent_pid,
        })
    }
}

/// `OpenProcess`, with a vanished process (`ERROR_INVALID_PARAMETER`) as `NotFound`.
fn open_raw(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> io::Result<OwnedHandle> {
    // SAFETY: plain call; the returned handle is owned by the `OwnedHandle` built from it.
    match unsafe { OpenProcess(access, false, pid) } {
        // SAFETY: `OpenProcess` returned a fresh handle that nothing else owns.
        Ok(h) => Ok(unsafe { OwnedHandle::from_raw_handle(h.0) }),
        Err(e) if e.code() == ERROR_INVALID_PARAMETER.to_hresult() => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("process {pid} is not running"),
        )),
        Err(e) => Err(io_error(&e)),
    }
}

fn image_path(process: HANDLE) -> io::Result<PathBuf> {
    let mut buf = vec![0u16; 32_768];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` holds `len` UTF-16 units and outlives the call; `len` is updated in place.
    unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    }
    .map_err(|e| io_error(&e))?;
    buf.truncate(len as usize);
    Ok(PathBuf::from(OsString::from_wide(&buf)))
}

fn parent_pid(pid: u32) -> io::Result<Option<u32>> {
    // SAFETY: plain call; the snapshot handle is owned by the `OwnedHandle` built from it.
    let snapshot =
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.map_err(|e| io_error(&e))?;
    // SAFETY: the snapshot handle is fresh and owned by nothing else.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot.0) };
    let raw = HANDLE(snapshot.as_raw_handle());
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: `entry` is a valid PROCESSENTRY32W with `dwSize` set, as both calls require.
    let mut more = unsafe { Process32FirstW(raw, &mut entry) }.is_ok();
    while more {
        if entry.th32ProcessID == pid {
            return Ok(Some(entry.th32ParentProcessID).filter(|&p| p != 0));
        }
        // SAFETY: as above.
        more = unsafe { Process32NextW(raw, &mut entry) }.is_ok();
    }
    Ok(None)
}

/// The SID string of the user running this process.
pub fn current_user_sid() -> io::Result<String> {
    // SAFETY: the pseudo-handle from `GetCurrentProcess` is always valid and needs no closing.
    sid::token_user_sid(unsafe { GetCurrentProcess() }).map_err(|(_, e)| io_error(&e))
}

/// The folder holding this process's executable.
pub fn current_exe_dir() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    exe.parent().map(Path::to_path_buf).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "the current executable has no parent folder",
        )
    })
}

// ---------------------------------------------------------------------------------------------
// Owned process handles.
// ---------------------------------------------------------------------------------------------

/// An owned process handle with `SYNCHRONIZE` and query access (and `PROCESS_TERMINATE` when
/// it was granted). Closed on drop; dropping never kills the process.
#[derive(Debug)]
pub struct ProcessHandle {
    handle: OwnedHandle,
    pid: u32,
}

impl ProcessHandle {
    fn raw(&self) -> HANDLE {
        HANDLE(self.handle.as_raw_handle())
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// `Some(code)` once the process has exited, `None` while it runs.
    pub fn exit_code(&self) -> io::Result<Option<u32>> {
        // SAFETY: the handle is open for as long as `self` lives.
        match unsafe { WaitForSingleObject(self.raw(), 0) } {
            WAIT_OBJECT_0 => {
                let mut code = 0u32;
                // SAFETY: as above; `code` is a valid out-pointer.
                unsafe { GetExitCodeProcess(self.raw(), &mut code) }.map_err(|e| io_error(&e))?;
                Ok(Some(code))
            }
            WAIT_TIMEOUT => Ok(None),
            _ => Err(io::Error::last_os_error()),
        }
    }

    /// Waits for the process to exit and returns its exit code. The wait runs on the Windows
    /// thread pool, so it never blocks a runtime thread, and dropping the future cancels it.
    pub async fn wait(&self) -> io::Result<u32> {
        if let Some(code) = self.exit_code()? {
            return Ok(code);
        }
        let (tx, rx) = oneshot::channel();
        let registration = ExitWait::register(self.raw(), tx)?;
        let _ = rx.await;
        drop(registration);
        self.exit_code()?
            .ok_or_else(|| io::Error::other("the process wait ended before the process exited"))
    }

    /// Terminates the process with exit code 1. A process that has already exited is not an
    /// error.
    pub fn kill(&self) -> io::Result<()> {
        // SAFETY: the handle is open for as long as `self` lives.
        match unsafe { TerminateProcess(self.raw(), 1) } {
            Ok(()) => Ok(()),
            Err(e) => match self.exit_code()? {
                Some(_) => Ok(()),
                None => Err(io_error(&e)),
            },
        }
    }
}

/// Opens a running process. `Err` with kind `NotFound` when no process has this PID. A process
/// that has exited but whose object still exists opens fine; its `exit_code` is `Some`.
pub fn open(pid: u32) -> io::Result<ProcessHandle> {
    let base = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
    let handle = match open_raw(pid, base | PROCESS_TERMINATE) {
        Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED.0 as i32) => open_raw(pid, base)?,
        other => other?,
    };
    Ok(ProcessHandle { handle, pid })
}

type ExitSlot = Mutex<Option<oneshot::Sender<()>>>;

/// A thread-pool wait on a process handle that signals a oneshot when the process exits.
struct ExitWait {
    /// The wait object's handle value.
    wait: isize,
    /// The reference handed to Windows as the callback context; reclaimed on drop.
    context: *const ExitSlot,
}

// SAFETY: `wait` is a plain handle value and `context` points into an `Arc<ExitSlot>`, whose
// contents are `Send + Sync`; both may be used and released from any thread.
unsafe impl Send for ExitWait {}

impl ExitWait {
    fn register(process: HANDLE, tx: oneshot::Sender<()>) -> io::Result<ExitWait> {
        let context = Arc::into_raw(Arc::new(Mutex::new(Some(tx))));
        let mut wait = HANDLE::default();
        // SAFETY: `context` stays valid until `Drop` has unregistered the wait (waiting for any
        // running callback) and only then releases it. The caller keeps `process` open for as
        // long as this `ExitWait` lives.
        let registered = unsafe {
            RegisterWaitForSingleObject(
                &mut wait,
                process,
                Some(on_exit),
                Some(context.cast::<c_void>()),
                INFINITE,
                WT_EXECUTEONLYONCE,
            )
        };
        if let Err(e) = registered {
            // SAFETY: the registration failed, so Windows holds no copy of `context`.
            drop(unsafe { Arc::from_raw(context) });
            return Err(io_error(&e));
        }
        Ok(ExitWait {
            wait: wait.0 as isize,
            context,
        })
    }
}

impl Drop for ExitWait {
    fn drop(&mut self) {
        // SAFETY: `wait` came from `RegisterWaitForSingleObject` and is unregistered only here.
        // `INVALID_HANDLE_VALUE` makes the call wait for a running callback to finish, so after
        // success no callback can still use `context`.
        let unregistered = unsafe {
            UnregisterWaitEx(HANDLE(self.wait as *mut c_void), Some(INVALID_HANDLE_VALUE))
        };
        if unregistered.is_ok() {
            // SAFETY: the reference made by `Arc::into_raw` in `register`, released once.
            drop(unsafe { Arc::from_raw(self.context) });
        }
        // On failure the context is leaked: a callback might still run and use it.
    }
}

unsafe extern "system" fn on_exit(context: *mut c_void, _timed_out: bool) {
    // SAFETY: `context` is the `Arc<ExitSlot>` reference that `ExitWait` keeps alive until the
    // wait is unregistered.
    let slot = unsafe { &*context.cast::<ExitSlot>() };
    if let Ok(mut slot) = slot.lock()
        && let Some(tx) = slot.take()
    {
        let _ = tx.send(());
    }
}

// ---------------------------------------------------------------------------------------------
// Detached spawn (ARCH-019).
// ---------------------------------------------------------------------------------------------

/// What to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnRequest {
    pub exe: PathBuf,
    pub args: Vec<OsString>,
    /// Added to the current environment; a key here replaces any current variable with the
    /// same name, compared case-insensitively.
    pub extra_env: Vec<(OsString, OsString)>,
}

/// `CreateProcessW` behind a trait, so the breakaway retry can be tested.
pub trait RawSpawner {
    fn spawn(&self, req: &SpawnRequest, flags: u32) -> io::Result<ProcessHandle>;
}

/// `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_UNICODE_ENVIRONMENT`.
pub const DETACHED_FLAGS: u32 =
    DETACHED_PROCESS.0 | CREATE_NEW_PROCESS_GROUP.0 | CREATE_UNICODE_ENVIRONMENT.0;
/// `CREATE_BREAKAWAY_FROM_JOB`.
pub const BREAKAWAY: u32 = CREATE_BREAKAWAY_FROM_JOB.0;

/// The real spawner: `CreateProcessW` with `bInheritHandles = FALSE`, no
/// `STARTF_USESTDHANDLES`, an explicit UTF-16 environment block, a command line quoted by the
/// Windows argument rules, and the exe's folder as the working directory.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemSpawner;

impl RawSpawner for SystemSpawner {
    fn spawn(&self, req: &SpawnRequest, flags: u32) -> io::Result<ProcessHandle> {
        let application = wide_nul(req.exe.as_os_str(), "the exe path")?;
        let mut command_line = command_line(req.exe.as_os_str(), &req.args)?;
        let environment = environment_block(current_environment()?, &req.extra_env)?;
        let directory = match req.exe.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => {
                Some(wide_nul(dir.as_os_str(), "the exe folder")?)
            }
            _ => None,
        };
        let startup = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut info = PROCESS_INFORMATION::default();
        // SAFETY: every buffer is NUL-terminated (the environment block double-NUL-terminated)
        // and outlives the call; `command_line` is mutable as `CreateProcessW` requires;
        // `startup` and `info` are valid for reads and writes respectively.
        unsafe {
            CreateProcessW(
                PCWSTR(application.as_ptr()),
                Some(PWSTR(command_line.as_mut_ptr())),
                None,
                None,
                false,
                // The block below is always UTF-16, whatever the caller's flags say.
                PROCESS_CREATION_FLAGS(flags | CREATE_UNICODE_ENVIRONMENT.0),
                Some(environment.as_ptr().cast()),
                directory
                    .as_ref()
                    .map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
                &startup,
                &mut info,
            )
        }
        .map_err(|e| io_error(&e))?;
        // SAFETY: `CreateProcessW` succeeded, so both handles are fresh and ours; the thread
        // handle is not needed.
        let _ = unsafe { CloseHandle(info.hThread) };
        Ok(ProcessHandle {
            // SAFETY: the process handle is fresh and owned by nothing else.
            handle: unsafe { OwnedHandle::from_raw_handle(info.hProcess.0) },
            pid: info.dwProcessId,
        })
    }
}

/// Spawns `req` detached with [`SystemSpawner`].
pub fn spawn_detached(req: &SpawnRequest) -> io::Result<ProcessHandle> {
    spawn_detached_with(&SystemSpawner, req)
}

/// Tries `DETACHED_FLAGS | BREAKAWAY` first; on `ERROR_ACCESS_DENIED` (a job that forbids
/// breakaway) retries once with `DETACHED_FLAGS`, tying the child to that job (ARCH-019).
pub fn spawn_detached_with(
    spawner: &dyn RawSpawner,
    req: &SpawnRequest,
) -> io::Result<ProcessHandle> {
    match spawner.spawn(req, DETACHED_FLAGS | BREAKAWAY) {
        Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED.0 as i32) => {
            spawner.spawn(req, DETACHED_FLAGS)
        }
        other => other,
    }
}

fn wide_nul(s: &OsStr, what: &str) -> io::Result<Vec<u16>> {
    let mut wide: Vec<u16> = s.encode_wide().collect();
    if wide.contains(&0) {
        return Err(invalid_input(&format!("{what} contains a NUL character")));
    }
    wide.push(0);
    Ok(wide)
}

const QUOTE: u16 = b'"' as u16;
const BACKSLASH: u16 = b'\\' as u16;
const SPACE: u16 = b' ' as u16;

/// The NUL-terminated command line: the exe path always quoted, then each argument quoted by
/// the rules `CommandLineToArgvW` and the C runtime parse back.
fn command_line(exe: &OsStr, args: &[OsString]) -> io::Result<Vec<u16>> {
    let exe = wide_nul(exe, "the exe path")?;
    let exe = &exe[..exe.len() - 1];
    if exe.contains(&QUOTE) {
        return Err(invalid_input("the exe path contains a quote"));
    }
    let mut line = Vec::with_capacity(exe.len() + 2);
    line.push(QUOTE);
    line.extend_from_slice(exe);
    line.push(QUOTE);
    for arg in args {
        line.push(SPACE);
        append_arg(&mut line, arg)?;
    }
    line.push(0);
    Ok(line)
}

fn append_arg(line: &mut Vec<u16>, arg: &OsStr) -> io::Result<()> {
    let arg: Vec<u16> = arg.encode_wide().collect();
    if arg.contains(&0) {
        return Err(invalid_input("an argument contains a NUL character"));
    }
    let needs_quotes = arg.is_empty()
        || arg
            .iter()
            .any(|&c| matches!(c, 0x20 | 0x09 | 0x0A | 0x0B) || c == QUOTE);
    if !needs_quotes {
        line.extend_from_slice(&arg);
        return Ok(());
    }
    line.push(QUOTE);
    let mut backslashes = 0usize;
    for &c in &arg {
        if c == BACKSLASH {
            backslashes += 1;
        } else {
            if c == QUOTE {
                // Double the run of backslashes and escape the quote itself.
                line.extend(std::iter::repeat_n(BACKSLASH, backslashes + 1));
            }
            backslashes = 0;
        }
        line.push(c);
    }
    // Backslashes before the closing quote are doubled so they stay literal.
    line.extend(std::iter::repeat_n(BACKSLASH, backslashes));
    line.push(QUOTE);
    Ok(())
}

/// This process's environment, entries in block order. Hidden `=C:`-style entries are kept.
fn current_environment() -> io::Result<Vec<(OsString, OsString)>> {
    // SAFETY: plain call; the block is freed below with `FreeEnvironmentStringsW`.
    let block = unsafe { GetEnvironmentStringsW() };
    if block.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut vars = Vec::new();
    let mut p = block.0.cast_const();
    // SAFETY: the block is a sequence of NUL-terminated strings ended by an empty string; the
    // loop reads each string up to its NUL and stops at the empty one.
    unsafe {
        loop {
            let mut len = 0usize;
            while *p.add(len) != 0 {
                len += 1;
            }
            if len == 0 {
                break;
            }
            let entry = std::slice::from_raw_parts(p, len);
            // The name may itself start with '=' (`=C:=C:\dir`); split at the next one.
            if let Some(eq) = entry.iter().skip(1).position(|&c| c == b'=' as u16) {
                let eq = eq + 1;
                vars.push((
                    OsString::from_wide(&entry[..eq]),
                    OsString::from_wide(&entry[eq + 1..]),
                ));
            }
            p = p.add(len + 1);
        }
        let _ = FreeEnvironmentStringsW(PCWSTR(block.0));
    }
    Ok(vars)
}

/// The key Windows sorts and compares environment names by.
fn env_key(name: &OsStr) -> String {
    name.to_string_lossy().to_uppercase()
}

/// `base` with `extra` applied (case-insensitive replacement, later entries win), sorted by
/// upper-cased name, as a double-NUL-terminated UTF-16 block.
fn environment_block(
    base: Vec<(OsString, OsString)>,
    extra: &[(OsString, OsString)],
) -> io::Result<Vec<u16>> {
    let mut vars: Vec<(String, OsString, OsString)> = Vec::new();
    for (name, value) in base.into_iter().chain(extra.iter().cloned()) {
        let wide: Vec<u16> = name.encode_wide().collect();
        if wide.is_empty() || wide.iter().skip(1).any(|&c| c == b'=' as u16) {
            return Err(invalid_input(
                "an environment variable name is empty or contains '='",
            ));
        }
        let key = env_key(&name);
        vars.retain(|(k, _, _)| *k != key);
        vars.push((key, name, value));
    }
    vars.sort_by(|a, b| a.0.cmp(&b.0));
    let mut block = Vec::new();
    for (_, name, value) in &vars {
        let name = wide_nul(name, "an environment variable name")?;
        let value = wide_nul(value, "an environment variable value")?;
        block.extend_from_slice(&name[..name.len() - 1]);
        block.push(b'=' as u16);
        block.extend_from_slice(&value);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(exe: &str, args: &[&str]) -> String {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        let mut wide = command_line(OsStr::new(exe), &args).unwrap();
        assert_eq!(wide.pop(), Some(0));
        String::from_utf16(&wide).unwrap()
    }

    #[test] // spec: ARCH-019
    fn command_line_quotes_by_the_windows_rules() {
        assert_eq!(
            line(r"C:\Ünïcödé space\ww-core.exe", &[]),
            r#""C:\Ünïcödé space\ww-core.exe""#
        );
        assert_eq!(line(r"C:\a.exe", &["plain", ""]), r#""C:\a.exe" plain """#);
        assert_eq!(
            line(r"C:\a.exe", &["two words", r#"say "hi""#, r"dir\", r"a\\b"]),
            r#""C:\a.exe" "two words" "say \"hi\"" dir\ a\\b"#
        );
        assert_eq!(
            line(r"C:\a.exe", &[r"trailing \", r#"back\"quote"#]),
            r#""C:\a.exe" "trailing \\" "back\\\"quote""#
        );
        assert!(command_line(OsStr::new(r#"C:\a"b.exe"#), &[]).is_err());
        assert!(command_line(OsStr::new(r"C:\a.exe"), &["nul\0".into()]).is_err());
    }

    fn block_entries(block: &[u16]) -> Vec<String> {
        assert!(block.ends_with(&[0, 0]), "double-NUL terminated");
        String::from_utf16(&block[..block.len() - 2])
            .unwrap()
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    #[test] // spec: ARCH-019
    fn environment_block_overrides_case_insensitively_and_sorts() {
        let base = vec![
            ("Path".into(), r"C:\bin".into()),
            ("=C:".into(), r"C:\work".into()),
            ("zeta".into(), "1".into()),
            ("WW_CHANNEL".into(), "dev".into()),
        ];
        let extra = vec![
            ("ww_channel".into(), "test-x1".into()),
            ("Alpha".into(), "Ünïcödé space".into()),
        ];
        let block = environment_block(base, &extra).unwrap();
        assert_eq!(
            block_entries(&block),
            [
                r"=C:=C:\work",
                "Alpha=Ünïcödé space",
                r"Path=C:\bin",
                "ww_channel=test-x1",
                "zeta=1",
            ]
        );
        assert_eq!(environment_block(Vec::new(), &[]).unwrap(), [0, 0]);
        assert!(environment_block(Vec::new(), &[("A=B".into(), "c".into())]).is_err());
    }

    #[test]
    fn current_environment_contains_path() {
        let vars = current_environment().unwrap();
        assert!(vars.iter().any(|(k, _)| env_key(k) == "PATH"));
    }

    #[test] // spec: ARCH-027
    fn open_missing_process_is_not_found() {
        // No process has a PID this large.
        let err = open(0xFFFF_FFF0).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn process_handle_and_its_wait_future_are_send() {
        fn assert_send<T: Send>(_: &T) {}
        fn assert_sync<T: Sync>() {}
        assert_sync::<ProcessHandle>();
        let me = open(std::process::id()).unwrap();
        assert_eq!(me.exit_code().unwrap(), None);
        let wait = me.wait();
        assert_send(&wait);
    }

    #[test]
    fn current_exe_dir_holds_the_current_exe() {
        let exe = std::env::current_exe().unwrap();
        assert_eq!(current_exe_dir().unwrap(), exe.parent().unwrap());
    }
}
