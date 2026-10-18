//! Starting an owned engine (ARCH-064, ARCH-068): in a kill-on-close Job Object, with its
//! output captured line by line and exactly the environment the caller gives (the core builds
//! the scrubbed one).
//!
//! The child is created with `CREATE_NO_WINDOW | CREATE_SUSPENDED`, assigned to the job, then
//! resumed, so it can't start a process outside the job first. It inherits exactly two handles,
//! the write ends of its stdout and stderr pipes (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`); stdin is
//! none. Those two ends are inheritable only from just before `CreateProcessW` until our copies
//! close right after it, so a `std::process::Command` spawned at that moment on another thread
//! (which inherits every inheritable handle) could take a copy; the engine's output then
//! still arrives, but its end-of-file waits until that other process exits as well.
//!
//! The listening-socket reads for the loopback check are here too.

use std::ffi::{OsString, c_void};
use std::io::{self, BufRead, BufReader};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::sync::Arc;

use windows::Win32::Foundation::{HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION,
    ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
    UpdateProcThreadAttribute,
};
use windows::core::{PCWSTR, PWSTR};

pub use crate::sockets::{listening_sockets, pid_listening_on};

use crate::job_object::KillOnCloseJob;
use crate::process::{ProcessHandle, command_line, environment_block, io_error, wide_nul};

/// What to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineSpawn {
    pub exe: PathBuf,
    pub args: Vec<OsString>,
    /// The working directory.
    pub cwd: PathBuf,
    /// The child's whole environment: nothing of the current one is passed on.
    pub env: Vec<(OsString, OsString)>,
}

/// Called with `"stdout"` or `"stderr"` and each line, without its line ending, decoded as
/// UTF-8 with invalid sequences replaced. Called from two reader threads, which end when the
/// child and everything that inherited its pipes have closed them.
pub type OnLine = Box<dyn Fn(&str, &str) + Send + Sync>;

/// Starts `req` in `job` and returns its handle once it runs. The output readers run until the
/// pipes close.
pub fn spawn_engine(
    req: &EngineSpawn,
    job: &KillOnCloseJob,
    on_line: OnLine,
) -> io::Result<ProcessHandle> {
    let application = wide_nul(req.exe.as_os_str(), "the exe path")?;
    let mut command_line = command_line(req.exe.as_os_str(), &req.args)?;
    let environment = environment_block(Vec::new(), &req.env, &[])?;
    let directory = wide_nul(req.cwd.as_os_str(), "the working directory")?;

    let (out_read, out_write) = std::io::pipe()?;
    let (err_read, err_write) = std::io::pipe()?;
    let inherited = [
        HANDLE(out_write.as_raw_handle()),
        HANDLE(err_write.as_raw_handle()),
    ];
    for handle in inherited {
        // SAFETY: the handle is a live pipe end owned by this function; only its flag changes.
        unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT) }
            .map_err(|e| io_error(&e))?;
    }

    let mut attributes = AttributeList::with_handles(&inherited)?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdOutput = inherited[0];
    startup.StartupInfo.hStdError = inherited[1];
    startup.lpAttributeList = attributes.list();
    let mut info = PROCESS_INFORMATION::default();
    let flags = CREATE_NO_WINDOW
        | CREATE_SUSPENDED
        | CREATE_UNICODE_ENVIRONMENT
        | EXTENDED_STARTUPINFO_PRESENT;
    // SAFETY: every buffer is NUL-terminated (the environment block double-NUL-terminated) and
    // outlives the call; `command_line` is mutable as `CreateProcessW` requires; `startup` is a
    // STARTUPINFOEXW whose attribute list (holding `inherited`, both live) outlives the call.
    let created = unsafe {
        CreateProcessW(
            PCWSTR(application.as_ptr()),
            Some(PWSTR(command_line.as_mut_ptr())),
            None,
            None,
            true,
            flags,
            Some(environment.as_ptr().cast::<c_void>()),
            PCWSTR(directory.as_ptr()),
            &startup.StartupInfo,
            &mut info,
        )
    };
    // The child has its own copies now; ours close, so the pipes end when the child's do.
    drop(attributes);
    drop((out_write, err_write));
    created.map_err(|e| io_error(&e))?;

    // SAFETY: `CreateProcessW` succeeded, so both handles are fresh and ours.
    let (process, thread) = unsafe {
        (
            OwnedHandle::from_raw_handle(info.hProcess.0),
            OwnedHandle::from_raw_handle(info.hThread.0),
        )
    };
    let child = ProcessHandle::from_owned(process, info.dwProcessId);
    let started = job.assign(&child).and_then(|()| {
        // SAFETY: the thread handle is open; resuming a suspended thread is the documented use.
        match unsafe { ResumeThread(HANDLE(thread.as_raw_handle())) } {
            u32::MAX => Err(io::Error::last_os_error()),
            _ => Ok(()),
        }
    });
    drop(thread);
    if let Err(e) = started {
        // SAFETY: the process handle is open; the child never ran user code.
        let _ = unsafe { TerminateProcess(child.raw(), 1) };
        return Err(e);
    }

    let on_line: Arc<OnLine> = Arc::new(on_line);
    read_lines("stdout", out_read, on_line.clone());
    read_lines("stderr", err_read, on_line);
    Ok(child)
}

/// Reads `pipe` line by line on its own thread until it ends.
fn read_lines(stream: &'static str, pipe: std::io::PipeReader, on_line: Arc<OnLine>) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(pipe);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    while line.last().is_some_and(|&b| b == b'\n' || b == b'\r') {
                        line.pop();
                    }
                    on_line(stream, &String::from_utf8_lossy(&line));
                }
            }
        }
    });
}

/// A `PROC_THREAD_ATTRIBUTE_LIST` with one attribute, `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`. It
/// points into `handles`, which the caller keeps alive and unmoved while the list lives.
struct AttributeList {
    /// The initialised list's storage; `usize`s so it is pointer-aligned.
    buf: Vec<usize>,
}

impl AttributeList {
    fn with_handles(handles: &[HANDLE]) -> io::Result<AttributeList> {
        let mut size = 0usize;
        // SAFETY: the documented size query: no list, one attribute; it fails with
        // ERROR_INSUFFICIENT_BUFFER and sets `size`.
        let _ = unsafe { InitializeProcThreadAttributeList(None, 1, None, &mut size) };
        let mut buf = vec![0usize; size.div_ceil(size_of::<usize>())];
        let raw = LPPROC_THREAD_ATTRIBUTE_LIST(buf.as_mut_ptr().cast());
        // SAFETY: `buf` holds at least `size` bytes.
        unsafe { InitializeProcThreadAttributeList(Some(raw), 1, None, &mut size) }
            .map_err(|e| io_error(&e))?;
        // From here `Drop` deletes the list.
        let mut list = AttributeList { buf };
        // SAFETY: the list is initialised; `handles` is an array of HANDLE of the size passed,
        // which the caller keeps alive while the list is used.
        unsafe {
            UpdateProcThreadAttribute(
                list.list(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                Some(handles.as_ptr().cast()),
                size_of_val(handles),
                None,
                None,
            )
        }
        .map_err(|e| io_error(&e))?;
        Ok(list)
    }

    fn list(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        LPPROC_THREAD_ATTRIBUTE_LIST(self.buf.as_mut_ptr().cast())
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: an `AttributeList` exists only once its list is initialised; deleted once.
        unsafe { DeleteProcThreadAttributeList(self.list()) };
    }
}
