//! Kill-on-close Job Objects for owned engines, and the process tree of an attached one
//! (ARCH-064).
//!
//! An owned engine is every process in its job (`JobObjectBasicProcessIdList`): a venv's
//! `python.exe` is a launcher that starts the interpreter as its child, and the child inherits
//! the job. The job exists for crash cleanup: when the core dies, its handle closes and Windows
//! kills every process in the job. Normal exits stop the engine explicitly. An attached engine
//! has no job, so its processes are the listener and its [`descendants`].

use std::collections::{HashMap, HashSet};
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use windows::Win32::Foundation::{ERROR_MORE_DATA, FILETIME, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicProcessIdList, JobObjectExtendedLimitInformation, QueryInformationJobObject,
    SetInformationJobObject,
};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::core::PCWSTR;

use crate::process::{ProcessHandle, io_error};

/// An unnamed Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: dropping it (or the core
/// dying) kills every process in it.
#[derive(Debug)]
pub struct KillOnCloseJob {
    handle: OwnedHandle,
}

impl KillOnCloseJob {
    pub fn new() -> io::Result<KillOnCloseJob> {
        // SAFETY: plain call; no attributes and no name.
        let raw = unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(|e| io_error(&e))?;
        // SAFETY: the handle is fresh and owned by nothing else.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw.0) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `limits` is a JOBOBJECT_EXTENDED_LIMIT_INFORMATION of the size passed, as
        // the information class requires.
        unsafe {
            SetInformationJobObject(
                HANDLE(handle.as_raw_handle()),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }
        .map_err(|e| io_error(&e))?;
        Ok(KillOnCloseJob { handle })
    }

    fn raw(&self) -> HANDLE {
        HANDLE(self.handle.as_raw_handle())
    }

    /// Puts `p` in the job. Processes it starts afterwards are in the job too.
    pub fn assign(&self, p: &ProcessHandle) -> io::Result<()> {
        // SAFETY: both handles are open for the duration of the call.
        unsafe { AssignProcessToJobObject(self.raw(), p.raw()) }.map_err(|e| io_error(&e))
    }

    /// Every process now in the job: the launcher and each of its children, a `conhost.exe`
    /// included when one was started.
    pub fn pids(&self) -> io::Result<Vec<u32>> {
        // The list is a header of two u32 counts (one usize) followed by usize ids.
        let header = size_of::<JOBOBJECT_BASIC_PROCESS_ID_LIST>() / size_of::<usize>() - 1;
        let mut room = 16usize;
        loop {
            let mut buf = vec![0usize; header + room];
            // SAFETY: `buf` is writable for the size passed and aligned for the list.
            let queried = unsafe {
                QueryInformationJobObject(
                    Some(self.raw()),
                    JobObjectBasicProcessIdList,
                    buf.as_mut_ptr().cast(),
                    (buf.len() * size_of::<usize>()) as u32,
                    None,
                )
            };
            // SAFETY: `buf` starts with a JOBOBJECT_BASIC_PROCESS_ID_LIST header.
            let list = unsafe { &*buf.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>() };
            let (assigned, listed) = (
                list.NumberOfAssignedProcesses as usize,
                list.NumberOfProcessIdsInList as usize,
            );
            match queried {
                Ok(()) => {
                    let ids = &buf[header..header + listed.min(room)];
                    return Ok(ids.iter().map(|&id| id as u32).collect());
                }
                // Processes joined since the last try: make room for them and a few more.
                Err(e) if e.code() == ERROR_MORE_DATA.to_hresult() => {
                    room = room.max(assigned) + 16;
                }
                Err(e) => return Err(io_error(&e)),
            }
        }
    }
}

/// Every process descended from `pid` (children, their children, and so on), from a Toolhelp
/// snapshot; `pid` itself is not in the list.
///
/// A parent pid in the snapshot may belong to a process that has exited and whose pid was
/// reused, so a child is followed only when it started no earlier than its parent. A child
/// whose start time can't be read (another user's process) is kept: for the loopback check a
/// stray process is safer than a missed one.
pub fn descendants(pid: u32) -> io::Result<Vec<u32>> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (child, parent) in snapshot()? {
        if child != parent {
            children.entry(parent).or_default().push(child);
        }
    }
    let mut found = Vec::new();
    let mut seen = HashSet::from([pid]);
    let mut queue = vec![(pid, start_time(pid))];
    while let Some((parent, parent_start)) = queue.pop() {
        for &child in children.get(&parent).into_iter().flatten() {
            if !seen.insert(child) {
                continue;
            }
            let child_start = start_time(child);
            if let (Some(p), Some(c)) = (parent_start, child_start)
                && c < p
            {
                continue;
            }
            found.push(child);
            queue.push((child, child_start));
        }
    }
    Ok(found)
}

/// `(pid, parent pid)` of every process.
fn snapshot() -> io::Result<Vec<(u32, u32)>> {
    // SAFETY: plain call; the snapshot handle is owned by the `OwnedHandle` built from it.
    let raw =
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.map_err(|e| io_error(&e))?;
    // SAFETY: the snapshot handle is fresh and owned by nothing else.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let raw = HANDLE(snapshot.as_raw_handle());
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut rows = Vec::new();
    // SAFETY: `entry` is a valid PROCESSENTRY32W with `dwSize` set, as both calls require.
    let mut more = unsafe { Process32FirstW(raw, &mut entry) }.is_ok();
    while more {
        rows.push((entry.th32ProcessID, entry.th32ParentProcessID));
        // SAFETY: as above.
        more = unsafe { Process32NextW(raw, &mut entry) }.is_ok();
    }
    Ok(rows)
}

/// The process's creation time in 100-ns ticks, `None` when it can't be opened.
fn start_time(pid: u32) -> Option<u64> {
    // SAFETY: plain call; the handle is owned by the `OwnedHandle` built from it.
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    // SAFETY: the handle is fresh and owned by nothing else.
    let process = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let [mut created, mut exited, mut kernel, mut user] = [FILETIME::default(); 4];
    // SAFETY: the handle is open; each pointer is to a live FILETIME.
    unsafe {
        GetProcessTimes(
            HANDLE(process.as_raw_handle()),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    }
    .ok()?;
    Some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}
