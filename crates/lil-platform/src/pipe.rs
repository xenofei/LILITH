//! User-only named pipe instances and pipe peers (ARCH-024, ARCH-026, ARCH-027, ARCH-045).
//!
//! Every instance carries the security descriptor [`pipe_sddl`] for the current user's real
//! SID (never the fixed test SID that test channels put in the pipe *name*), so only that user
//! and SYSTEM can open it. Remote clients are rejected. The core's first instance uses
//! `FILE_FLAG_FIRST_PIPE_INSTANCE` as its singleton check.

use std::ffi::c_void;
use std::fmt;
use std::io;
use std::os::windows::io::AsRawHandle;

use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, HANDLE, HLOCAL, LocalFree,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_KERNEL_OBJECT,
};
use windows::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
    DACL_SECURITY_INFORMATION, GetAce, GetAclInformation, PSECURITY_DESCRIPTOR, PSID,
    SECURITY_ATTRIBUTES,
};
use windows::Win32::System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};
use windows::Win32::System::SystemServices::{ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE};
use windows::core::{HSTRING, PWSTR};

use crate::process::{current_user_sid, io_error};

/// `D:P(A;;GA;;;<user_sid>)(A;;GA;;;SY)`: a protected DACL granting all access to the user and
/// to SYSTEM, and to nobody else (ARCH-026).
pub fn pipe_sddl(user_sid: &str) -> String {
    format!("D:P(A;;GA;;;{user_sid})(A;;GA;;;SY)")
}

/// Why [`create_instance`] failed.
#[derive(Debug)]
pub enum CreateError {
    /// A first instance was asked for, but the pipe already exists (another core owns it).
    AlreadyOwned,
    Io(io::Error),
}

impl fmt::Display for CreateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CreateError::AlreadyOwned => f.write_str("the pipe is already owned by another server"),
            CreateError::Io(e) => write!(f, "the pipe instance could not be created: {e}"),
        }
    }
}

impl std::error::Error for CreateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CreateError::AlreadyOwned => None,
            CreateError::Io(e) => Some(e),
        }
    }
}

/// A security descriptor allocated by `ConvertStringSecurityDescriptorToSecurityDescriptorW`,
/// freed on drop.
struct LocalDescriptor(PSECURITY_DESCRIPTOR);

impl LocalDescriptor {
    fn from_sddl(sddl: &str) -> io::Result<LocalDescriptor> {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: the SDDL string is NUL-terminated (`HSTRING`) and `sd` is a valid out-pointer.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &HSTRING::from(sddl),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
        }
        .map_err(|e| io_error(&e))?;
        Ok(LocalDescriptor(sd))
    }
}

impl Drop for LocalDescriptor {
    fn drop(&mut self) {
        // SAFETY: the descriptor was allocated with `LocalAlloc` by Windows and is freed once.
        let _ = unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
    }
}

/// Creates one server instance of the pipe `name` (a full `\\.\pipe\...` path) with the
/// user-only descriptor, rejecting remote clients. With `first`, the pipe must not exist yet:
/// `ERROR_ACCESS_DENIED` then means another server owns it ([`CreateError::AlreadyOwned`]).
/// Must be called inside a tokio runtime.
pub fn create_instance(name: &str, first: bool) -> Result<NamedPipeServer, CreateError> {
    let sid = current_user_sid().map_err(CreateError::Io)?;
    let descriptor = LocalDescriptor::from_sddl(&pipe_sddl(&sid)).map_err(CreateError::Io)?;
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0.0,
        bInheritHandle: false.into(),
    };
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true);
    // SAFETY: `attributes` is a valid SECURITY_ATTRIBUTES whose descriptor (`descriptor`)
    // outlives the call.
    let created = unsafe {
        options.create_with_security_attributes_raw(name, (&raw mut attributes).cast::<c_void>())
    };
    match created {
        Ok(server) => Ok(server),
        Err(e) if first && e.raw_os_error() == Some(ERROR_ACCESS_DENIED.0 as i32) => {
            Err(CreateError::AlreadyOwned)
        }
        Err(e) => Err(CreateError::Io(e)),
    }
}

/// Why [`open_client`] failed.
#[derive(Debug)]
pub enum ConnectError {
    /// No pipe of this name exists.
    NotFound,
    /// The pipe exists but no instance is free right now.
    Busy,
    Io(io::Error),
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectError::NotFound => f.write_str("the pipe does not exist"),
            ConnectError::Busy => f.write_str("every instance of the pipe is busy"),
            ConnectError::Io(e) => write!(f, "the pipe could not be opened: {e}"),
        }
    }
}

impl std::error::Error for ConnectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConnectError::Io(e) => Some(e),
            _ => None,
        }
    }
}

/// Opens the pipe `name` once, without waiting for a free instance. Must be called inside a
/// tokio runtime.
pub fn open_client(name: &str) -> Result<NamedPipeClient, ConnectError> {
    match ClientOptions::new().open(name) {
        Ok(client) => Ok(client),
        Err(e) if e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND.0 as i32) => {
            Err(ConnectError::NotFound)
        }
        Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32) => Err(ConnectError::Busy),
        Err(e) => Err(ConnectError::Io(e)),
    }
}

/// The PID of the connected client (`GetNamedPipeClientProcessId`, ARCH-045).
pub fn client_pid(server: &NamedPipeServer) -> io::Result<u32> {
    let mut pid = 0u32;
    // SAFETY: the handle is the server's open pipe handle; `pid` is a valid out-pointer.
    unsafe { GetNamedPipeClientProcessId(HANDLE(server.as_raw_handle()), &mut pid) }
        .map_err(|e| io_error(&e))?;
    Ok(pid)
}

/// The PID of the server process (`GetNamedPipeServerProcessId`, ARCH-027).
pub fn server_pid(client: &NamedPipeClient) -> io::Result<u32> {
    let mut pid = 0u32;
    // SAFETY: the handle is the client's open pipe handle; `pid` is a valid out-pointer.
    unsafe { GetNamedPipeServerProcessId(HANDLE(client.as_raw_handle()), &mut pid) }
        .map_err(|e| io_error(&e))?;
    Ok(pid)
}

/// The SID strings of the access-allowed and access-denied entries in the instance's DACL, in
/// order. A missing (NULL) DACL, which would allow everyone, is an error, as is any other kind
/// of entry.
pub fn instance_dacl_sids(server: &NamedPipeServer) -> io::Result<Vec<String>> {
    Ok(instance_dacl(server)?.into_iter().map(|e| e.sid).collect())
}

/// One entry of a pipe instance's DACL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaclEntry {
    /// The SID string the entry applies to.
    pub sid: String,
    /// `true` for an access-allowed entry, `false` for access-denied.
    pub allowed: bool,
    /// The entry's access mask, as stored.
    pub mask: u32,
}

/// The access-allowed and access-denied entries of the instance's DACL, in order, with their
/// access masks. Errors as [`instance_dacl_sids`].
pub fn instance_dacl(server: &NamedPipeServer) -> io::Result<Vec<DaclEntry>> {
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: the handle is the server's open pipe handle (opened with READ_CONTROL as part of
    // its read access); the out-pointers are valid. `sd` owns the memory `dacl` points into.
    unsafe {
        GetSecurityInfo(
            HANDLE(server.as_raw_handle()),
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut dacl),
            None,
            Some(&mut sd),
        )
    }
    .ok()
    .map_err(|e| io_error(&e))?;
    let _descriptor = LocalDescriptor(sd);
    if dacl.is_null() {
        return Err(io::Error::other(
            "the pipe has no DACL, which grants everyone access",
        ));
    }

    let mut size = ACL_SIZE_INFORMATION::default();
    // SAFETY: `dacl` points into the live descriptor; `size` is a valid buffer of the size
    // passed.
    unsafe {
        GetAclInformation(
            dacl,
            (&raw mut size).cast::<c_void>(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    }
    .map_err(|e| io_error(&e))?;

    let mut sids = Vec::with_capacity(size.AceCount as usize);
    for index in 0..size.AceCount {
        let mut ace: *mut c_void = std::ptr::null_mut();
        // SAFETY: `index` is below the ACE count; `ace` receives a pointer into the DACL.
        unsafe { GetAce(dacl, index, &mut ace) }.map_err(|e| io_error(&e))?;
        // SAFETY: every ACE starts with an ACE_HEADER.
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        let kind = u32::from(header.AceType);
        if kind != ACCESS_ALLOWED_ACE_TYPE && kind != ACCESS_DENIED_ACE_TYPE {
            return Err(io::Error::other(format!(
                "the pipe's DACL has an unexpected entry type {kind}"
            )));
        }
        // SAFETY: allowed and denied ACEs share the ACCESS_ALLOWED_ACE layout, whose `Mask`
        // follows the header and whose SID starts at `SidStart`.
        let (mask, sid) = unsafe {
            let entry = ace.cast::<ACCESS_ALLOWED_ACE>();
            ((*entry).Mask, (&raw mut (*entry).SidStart).cast())
        };
        sids.push(DaclEntry {
            sid: sid_string(PSID(sid))?,
            allowed: kind == ACCESS_ALLOWED_ACE_TYPE,
            mask,
        });
    }
    Ok(sids)
}

fn sid_string(sid: PSID) -> io::Result<String> {
    let mut text = PWSTR::null();
    // SAFETY: `sid` points at a valid SID inside a live DACL; the returned string is copied and
    // then freed once with `LocalFree`.
    unsafe {
        ConvertSidToStringSidW(sid, &mut text).map_err(|e| io_error(&e))?;
        let s = text.to_string();
        let _ = LocalFree(Some(HLOCAL(text.as_ptr().cast())));
        s.map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
}
