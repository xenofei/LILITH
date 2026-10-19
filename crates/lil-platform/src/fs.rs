//! Windows file primitives behind safe wrappers (ARCH-074, ARCH-089, CORE-040, CORE-041).
//!
//! Every function here opens its paths through [`long_path`], so absolute paths of 247 UTF-16
//! code units or more reach Windows with the `\\?\` (or `\\?\UNC\`) prefix and the suite never
//! depends on the `LongPathsEnabled` opt-in.
//!
//! Facts the store relies on (Microsoft Learn, re-checked 2026-10-16):
//! - `ReplaceFileW`: `REPLACEFILE_WRITE_THROUGH` "is not supported", so it is never passed. The
//!   resulting file has the replacement's file ID. Target, replacement and backup must be on one
//!   volume. Errors 1175 and 1176 (with a backup name) leave both files under their own names;
//!   1177 leaves the old target under the backup name and the replacement under its own.
//! - `MoveFileExW(MOVEFILE_WRITE_THROUGH)` flushes only a move done as a copy and delete. No
//!   function here passes `MOVEFILE_REPLACE_EXISTING`: a rename never replaces a file.
//! - `FlushFileBuffers` needs a handle with `GENERIC_WRITE`.
//! - `CfGetSyncRootInfoByPath` fails for a path that is not under a cloud-files sync root.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{
    ERROR_CLOUD_FILE_NOT_UNDER_SYNC_ROOT, ERROR_INVALID_FUNCTION, ERROR_LOCK_VIOLATION, HANDLE,
};
use windows::Win32::Storage::CloudFilters::{
    CF_SYNC_ROOT_BASIC_INFO, CF_SYNC_ROOT_INFO_BASIC, CfGetSyncRootInfoByPath,
};
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, COPY_FILE_FAIL_IF_EXISTS, COPYFILE2_EXTENDED_PARAMETERS, CopyFile2,
    DELETE, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_READONLY,
    FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, FILE_ATTRIBUTE_RECALL_ON_OPEN,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_BASIC_INFO,
    FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX,
    FILE_DISPOSITION_INFO_EX_FLAGS, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_FLAGS_AND_ATTRIBUTES, FILE_GENERIC_READ, FILE_ID_INFO, FILE_INFO_BY_HANDLE_CLASS,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO,
    FileAttributeTagInfo, FileBasicInfo, FileDispositionInfo, FileDispositionInfoEx, FileIdInfo,
    FileStandardInfo, FlushFileBuffers, GetDriveTypeW, GetFileAttributesW,
    GetFileInformationByHandle, GetFileInformationByHandleEx, GetLongPathNameW, GetShortPathNameW,
    GetVolumeInformationByHandleW, INVALID_FILE_ATTRIBUTES, LOCKFILE_EXCLUSIVE_LOCK,
    LOCKFILE_FAIL_IMMEDIATELY, LockFileEx, MOVEFILE_WRITE_THROUGH, MoveFileExW, REPLACE_FILE_FLAGS,
    REPLACEFILE_IGNORE_ACL_ERRORS, REPLACEFILE_IGNORE_MERGE_ERRORS, ReplaceFileW,
    SetFileAttributesW, SetFileInformationByHandle, UnlockFileEx,
};
use windows::Win32::System::IO::{OVERLAPPED, OVERLAPPED_0, OVERLAPPED_0_0};
use windows::core::PCWSTR;

use crate::paths::strip_verbatim;
use crate::process::io_error;
use crate::volume::volume_root;

// ---------------------------------------------------------------------------------------------
// Long paths.
// ---------------------------------------------------------------------------------------------

/// Absolute paths of this many UTF-16 code units or more get the `\\?\` prefix. It is std's
/// rule: `CreateDirectoryW`'s limit is `MAX_PATH` minus 12 (room for an 8.3 name), 248 units
/// with the terminating NUL, so 247 units of path is the first length that needs the prefix.
pub const LONG_PATH_UNITS: usize = 247;

const SEP: u16 = b'\\' as u16;
const VERBATIM: [u16; 4] = [SEP, SEP, b'?' as u16, SEP];
const DEVICE: [u16; 4] = [SEP, SEP, b'.' as u16, SEP];
const NT_PREFIX: [u16; 4] = [SEP, b'?' as u16, b'?' as u16, SEP];

/// `p` made absolute (`\` separators, no `.` or `..` components, through `GetFullPathNameW`),
/// then prefixed with `\\?\` (or `\\?\UNC\` for `\\server\share\…`) when it is
/// [`LONG_PATH_UNITS`] UTF-16 code units or more. A path that already starts with `\\?\`,
/// `\\.\` or `\??\` is returned unchanged, as is one that cannot be made absolute (empty).
pub fn long_path(p: &Path) -> PathBuf {
    let units: Vec<u16> = p.as_os_str().encode_wide().collect();
    if units.starts_with(&VERBATIM) || units.starts_with(&DEVICE) || units.starts_with(&NT_PREFIX) {
        return p.to_path_buf();
    }
    let Ok(absolute) = std::path::absolute(p) else {
        return p.to_path_buf();
    };
    let units: Vec<u16> = absolute.as_os_str().encode_wide().collect();
    if units.len() < LONG_PATH_UNITS {
        return absolute;
    }
    let mut prefixed: Vec<u16> = Vec::with_capacity(units.len() + 8);
    if units.starts_with(&[SEP, SEP]) {
        prefixed.extend(r"\\?\UNC\".encode_utf16());
        prefixed.extend_from_slice(&units[2..]);
    } else {
        prefixed.extend_from_slice(&VERBATIM);
        prefixed.extend_from_slice(&units);
    }
    PathBuf::from(std::ffi::OsString::from_wide(&prefixed))
}

/// [`long_path`] of `p` as a NUL-terminated UTF-16 string.
fn wide(p: &Path) -> io::Result<Vec<u16>> {
    let mut units: Vec<u16> = long_path(p).as_os_str().encode_wide().collect();
    if units.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a path contains a NUL character",
        ));
    }
    units.push(0);
    Ok(units)
}

fn handle(file: &File) -> HANDLE {
    HANDLE(file.as_raw_handle())
}

// ---------------------------------------------------------------------------------------------
// Writes, moves and copies.
// ---------------------------------------------------------------------------------------------

/// `FlushFileBuffers`: the file's data reaches the disk before it is swapped into place. The
/// handle must have write access (`GENERIC_WRITE`).
pub fn flush(file: &File) -> io::Result<()> {
    // SAFETY: the handle is open for as long as `file` is borrowed.
    unsafe { FlushFileBuffers(handle(file)) }.map_err(|e| io_error(&e))
}

/// The flags of every `ReplaceFileW` call: `REPLACEFILE_IGNORE_MERGE_ERRORS |
/// REPLACEFILE_IGNORE_ACL_ERRORS`. `REPLACEFILE_WRITE_THROUGH` is documented as not supported
/// and is never passed.
pub const REPLACE_FLAGS: u32 = REPLACEFILE_IGNORE_MERGE_ERRORS.0 | REPLACEFILE_IGNORE_ACL_ERRORS.0;

/// The flags of every `MoveFileExW` call: `MOVEFILE_WRITE_THROUGH` only. Never
/// `MOVEFILE_REPLACE_EXISTING` (so an existing target is error 80 or 183) and never
/// `MOVEFILE_COPY_ALLOWED` (so every move is a same-volume rename).
pub const MOVE_FLAGS: u32 = MOVEFILE_WRITE_THROUGH.0;

/// `ReplaceFileW(target, replacement, backup, REPLACE_FLAGS)`: `target` takes `replacement`'s
/// content and file ID, and the old `target` is kept under `backup`. All three names must be on
/// one volume. See [`classify`] for what each failure leaves behind.
pub fn replace_file(target: &Path, replacement: &Path, backup: &Path) -> io::Result<()> {
    let target = wide(target)?;
    let replacement = wide(replacement)?;
    let backup = wide(backup)?;
    // SAFETY: the three strings are NUL-terminated and outlive the call; the reserved
    // pointers are null, as documented.
    unsafe {
        ReplaceFileW(
            PCWSTR(target.as_ptr()),
            PCWSTR(replacement.as_ptr()),
            PCWSTR(backup.as_ptr()),
            REPLACE_FILE_FLAGS(REPLACE_FLAGS),
            None,
            None,
        )
    }
    .map_err(|e| io_error(&e))
}

/// `MoveFileExW(from, to, MOVEFILE_WRITE_THROUGH)` for files and folders. An existing `to` is
/// never replaced: the move fails with error 80 or 183 ([`RetryClass::TargetExists`]). This is
/// the only move in the suite.
pub fn move_new(from: &Path, to: &Path) -> io::Result<()> {
    let from = wide(from)?;
    let to = wide(to)?;
    // SAFETY: both strings are NUL-terminated and outlive the call.
    unsafe {
        MoveFileExW(
            PCWSTR(from.as_ptr()),
            PCWSTR(to.as_ptr()),
            windows::Win32::Storage::FileSystem::MOVE_FILE_FLAGS(MOVE_FLAGS),
        )
    }
    .map_err(|e| io_error(&e))
}

/// `CopyFile2(COPY_FILE_FAIL_IF_EXISTS)`: copies `from` to a new file `to` and returns the
/// copied file's size. An existing `to` is never overwritten (`std::fs::copy` would): the copy
/// fails with error 80 or 183. Windows block-clones the copy where the volume can (Dev Drive).
pub fn copy_new(from: &Path, to: &Path) -> io::Result<u64> {
    let source = wide(from)?;
    let target = wide(to)?;
    let params = COPYFILE2_EXTENDED_PARAMETERS {
        dwSize: size_of::<COPYFILE2_EXTENDED_PARAMETERS>() as u32,
        dwCopyFlags: COPY_FILE_FAIL_IF_EXISTS,
        pfCancel: std::ptr::null_mut(),
        pProgressRoutine: None,
        pvCallbackContext: std::ptr::null_mut(),
    };
    // SAFETY: both strings are NUL-terminated and outlive the call; `params` is a valid
    // COPYFILE2_EXTENDED_PARAMETERS with `dwSize` set, no cancel flag and no callback.
    unsafe {
        CopyFile2(
            PCWSTR(source.as_ptr()),
            PCWSTR(target.as_ptr()),
            Some(&params),
        )
    }
    .map_err(|e| io_error(&e))?;
    Ok(std::fs::metadata(long_path(to))?.len())
}

/// Sets or clears `FILE_ATTRIBUTE_READONLY`, keeping every other attribute.
pub fn set_readonly(path: &Path, on: bool) -> io::Result<()> {
    let current = attributes(path)?;
    let mut next = if on {
        current | FILE_ATTRIBUTE_READONLY.0
    } else {
        current & !FILE_ATTRIBUTE_READONLY.0
    };
    if next == current {
        return Ok(());
    }
    if next == 0 {
        next = FILE_ATTRIBUTE_NORMAL.0;
    }
    let w = wide(path)?;
    // SAFETY: `w` is NUL-terminated and outlives the call.
    unsafe { SetFileAttributesW(PCWSTR(w.as_ptr()), FILE_FLAGS_AND_ATTRIBUTES(next)) }
        .map_err(|e| io_error(&e))
}

/// `GetFileAttributesW`. It opens no handle, so a file another process holds with no sharing
/// is no obstacle (the store's `index.lock` check). A missing path is `NotFound`.
pub fn attributes(path: &Path) -> io::Result<u32> {
    let w = wide(path)?;
    // SAFETY: `w` is NUL-terminated and outlives the call.
    let attrs = unsafe { GetFileAttributesW(PCWSTR(w.as_ptr())) };
    if attrs == INVALID_FILE_ATTRIBUTES {
        Err(io::Error::last_os_error())
    } else {
        Ok(attrs)
    }
}

/// `GetLongPathNameW`: `path` with every 8.3 short component expanded to its long name. A path
/// that is gone is `NotFound`. A `\\?\` prefix that [`long_path`] added is removed again.
pub fn long_name(path: &Path) -> io::Result<PathBuf> {
    let input = wide(path)?;
    let mut buf = vec![0u16; 260];
    loop {
        // SAFETY: `input` is NUL-terminated; `buf` is a writable slice of the length passed.
        let len = unsafe { GetLongPathNameW(PCWSTR(input.as_ptr()), Some(&mut buf)) } as usize;
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        if len < buf.len() {
            buf.truncate(len);
            break;
        }
        // Too small: `len` is the size needed, terminating NUL included.
        buf = vec![0u16; len];
    }
    let long = PathBuf::from(std::ffi::OsString::from_wide(&buf));
    let caller_verbatim = path.as_os_str().encode_wide().take(4).eq(VERBATIM);
    Ok(if caller_verbatim {
        long
    } else {
        strip_verbatim(&long)
    })
}

/// `path` with every 8.3 short component of its longest existing prefix expanded by
/// `long_name` ([`long_name`], or a [`Probe`]'s), and the rest appended as given: a name that
/// doesn't exist has no short form. The walk up finds that prefix through `identity`
/// ([`identity`], or a [`Probe`]'s, so a fake can inject errors). Only a missing component
/// (`NotFound`, or `NotADirectory` below a file) moves the walk up a level; any other error is
/// returned, so a check built on this fails closed.
pub fn long_form(
    path: &Path,
    identity: &dyn Fn(&Path) -> io::Result<FileIdentity>,
    long_name: &dyn Fn(&Path) -> io::Result<PathBuf>,
) -> io::Result<PathBuf> {
    for existing in path.ancestors() {
        match identity(existing) {
            Ok(_) => {
                let long = long_name(existing)?;
                let tail = path.strip_prefix(existing).unwrap_or(Path::new(""));
                return Ok(if tail.as_os_str().is_empty() {
                    long
                } else {
                    long.join(tail)
                });
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) => {}
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no part of the path exists",
    ))
}

/// `GetShortPathNameW`: `path` with each component that has an 8.3 short name in that form. A
/// component without one (8.3 names off on the volume) keeps its long name. A `\\?\` prefix
/// that [`long_path`] added is removed again. Test support for the store's short-name checks.
pub fn short_name(path: &Path) -> io::Result<PathBuf> {
    let input = wide(path)?;
    let mut buf = vec![0u16; 260];
    loop {
        // SAFETY: `input` is NUL-terminated; `buf` is a writable slice of the length passed.
        let len = unsafe { GetShortPathNameW(PCWSTR(input.as_ptr()), Some(&mut buf)) } as usize;
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        if len < buf.len() {
            buf.truncate(len);
            break;
        }
        // Too small: `len` is the size needed, terminating NUL included.
        buf = vec![0u16; len];
    }
    Ok(strip_verbatim(&PathBuf::from(
        std::ffi::OsString::from_wide(&buf),
    )))
}

// ---------------------------------------------------------------------------------------------
// Error classes.
// ---------------------------------------------------------------------------------------------

/// What the store does about a failed file call (ARCH-074 error handling).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryClass {
    /// 32 `ERROR_SHARING_VIOLATION`, 33 `ERROR_LOCK_VIOLATION`, 1224
    /// `ERROR_USER_MAPPED_FILE`: retried with backoff.
    SharingViolation,
    /// 1175 `ERROR_UNABLE_TO_REMOVE_REPLACED`, 1176 `ERROR_UNABLE_TO_MOVE_REPLACEMENT`: with a
    /// backup name both files keep their own names, so the replace is retried.
    ReplaceRetry,
    /// 1177 `ERROR_UNABLE_TO_MOVE_REPLACEMENT_2`: the old target is under the backup name and
    /// the replacement under its own; the store moves the replacement onto the freed name.
    ReplacementNotMoved,
    /// 80 `ERROR_FILE_EXISTS`, 183 `ERROR_ALREADY_EXISTS`: a no-replace move or copy met an
    /// existing target.
    TargetExists,
    /// 1 `ERROR_INVALID_FUNCTION`, 50 `ERROR_NOT_SUPPORTED`: `ReplaceFileW` is unsupported on
    /// the volume; the store falls back to two renames.
    Unsupported,
    /// 112 `ERROR_DISK_FULL`, 39 `ERROR_HANDLE_DISK_FULL`.
    DiskFull,
    /// Anything else.
    Other,
}

/// The [`RetryClass`] of `e` by its Win32 code: 32/33/1224; 1175/1176; 1177; 80/183; 1/50;
/// 112/39; else `Other`.
pub fn classify(e: &io::Error) -> RetryClass {
    match e.raw_os_error() {
        Some(32 | 33 | 1224) => RetryClass::SharingViolation,
        Some(1175 | 1176) => RetryClass::ReplaceRetry,
        Some(1177) => RetryClass::ReplacementNotMoved,
        Some(80 | 183) => RetryClass::TargetExists,
        Some(1 | 50) => RetryClass::Unsupported,
        Some(112 | 39) => RetryClass::DiskFull,
        _ => RetryClass::Other,
    }
}

/// [`classify`] for a move of a folder (`folder`) or a file. A folder rename fails with error
/// 5 (`ERROR_ACCESS_DENIED`) while any handle is open anywhere in its subtree, whatever that
/// handle's share mode (MS-FSA 2.1.5.15.12): the index reading a file, Defender, Search or
/// Explorer can hold one. So for a folder 5 is a [`RetryClass::SharingViolation`], retried
/// under the store's budget (plan 2b ruling Q19). For a file, 5 stays `Other`: a real denial.
pub fn classify_move(e: &io::Error, folder: bool) -> RetryClass {
    match e.raw_os_error() {
        Some(5) if folder => RetryClass::SharingViolation,
        _ => classify(e),
    }
}

// ---------------------------------------------------------------------------------------------
// File identity and volume facts.
// ---------------------------------------------------------------------------------------------

/// What identifies a file's version without reading its data (CORE-041, ARCH-079).
///
/// `volume_serial` and `file_id` come from `FILE_ID_INFO` (a 64-bit serial and a 128-bit id).
/// `file_id` is `None` where `FileIdInfo` fails, and when the id is all zero or all `0xFF`
/// (MS-FSCC: such ids "MUST be ignored"); the serial then comes from
/// `BY_HANDLE_FILE_INFORMATION`. `change_time` is `None` when the file system reports 0.
/// On ReFS the id is built from the parent folder, so a move across folders changes it: ids
/// detect changes in place, never identity across folders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    pub volume_serial: u64,
    pub file_id: Option<[u8; 16]>,
    /// End of file in bytes (`FILE_STANDARD_INFO`).
    pub size: u64,
    /// `FILE_BASIC_INFO.LastWriteTime`, in 100 ns intervals since 1601 (UTC).
    pub last_write: i64,
    /// `FILE_BASIC_INFO.ChangeTime`, same unit.
    pub change_time: Option<i64>,
    /// `FILE_BASIC_INFO.FileAttributes`.
    pub attributes: u32,
}

/// The attributes that mark a cloud placeholder whose data is not on this machine.
pub const PLACEHOLDER_ATTRIBUTES: u32 = FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS.0
    | FILE_ATTRIBUTE_RECALL_ON_OPEN.0
    | FILE_ATTRIBUTE_OFFLINE.0;

impl FileIdentity {
    /// True for a cloud-only placeholder (`RECALL_ON_DATA_ACCESS`, `RECALL_ON_OPEN` or
    /// `OFFLINE`): reading its data would download it, so it is never read.
    pub fn is_cloud_placeholder(&self) -> bool {
        self.attributes & PLACEHOLDER_ATTRIBUTES != 0
    }
}

/// A 128-bit file id, or `None` when it is all zero or all `0xFF` (MS-FSCC: "MUST be ignored").
pub fn file_id_from(bytes: [u8; 16]) -> Option<[u8; 16]> {
    if bytes.iter().all(|&b| b == 0) || bytes.iter().all(|&b| b == 0xFF) {
        None
    } else {
        Some(bytes)
    }
}

/// `GetFileInformationByHandleEx` into a `T` of the class's structure type.
///
/// # Safety
/// `T` must be the structure `class` returns (a plain C struct with no invariants).
unsafe fn info<T: Default>(file: &File, class: FILE_INFO_BY_HANDLE_CLASS) -> io::Result<T> {
    let mut out = T::default();
    // SAFETY: `out` is a writable `T` of the size passed, and the caller guarantees `T` is
    // the structure `class` fills; the handle is open while `file` is borrowed.
    unsafe {
        GetFileInformationByHandleEx(
            handle(file),
            class,
            (&raw mut out).cast(),
            size_of::<T>() as u32,
        )
    }
    .map_err(|e| io_error(&e))?;
    Ok(out)
}

/// An attribute-only handle: `FILE_READ_ATTRIBUTES`, sharing read, write and delete,
/// `FILE_FLAG_BACKUP_SEMANTICS` (folders open too) and `FILE_FLAG_OPEN_REPARSE_POINT` (a link
/// is not followed). It never reads data, so a cloud placeholder is not hydrated, and share
/// modes don't apply to attribute access, so a file held without sharing still opens.
fn open_attributes(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES.0)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
        .custom_flags((FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).0)
        .open(long_path(path))
}

/// The [`FileIdentity`] of `path` through an attribute-only open (see `open_attributes`):
/// `GetFileInformationByHandleEx` with `FileIdInfo`, `FileBasicInfo` and `FileStandardInfo`.
pub fn identity(path: &Path) -> io::Result<FileIdentity> {
    let file = open_attributes(path)?;
    // SAFETY: each class is read into its documented structure.
    let basic: FILE_BASIC_INFO = unsafe { info(&file, FileBasicInfo) }?;
    // SAFETY: as above.
    let standard: FILE_STANDARD_INFO = unsafe { info(&file, FileStandardInfo) }?;
    // SAFETY: as above.
    let (volume_serial, file_id) = match unsafe { info::<FILE_ID_INFO>(&file, FileIdInfo) } {
        Ok(id) => (id.VolumeSerialNumber, file_id_from(id.FileId.Identifier)),
        Err(_) => {
            let mut by_handle = BY_HANDLE_FILE_INFORMATION::default();
            // SAFETY: `by_handle` is a valid out-pointer; the handle is open.
            unsafe { GetFileInformationByHandle(handle(&file), &mut by_handle) }
                .map_err(|e| io_error(&e))?;
            (u64::from(by_handle.dwVolumeSerialNumber), None)
        }
    };
    Ok(FileIdentity {
        volume_serial,
        file_id,
        size: u64::try_from(standard.EndOfFile).unwrap_or(0),
        last_write: basic.LastWriteTime,
        change_time: (basic.ChangeTime != 0).then_some(basic.ChangeTime),
        attributes: basic.FileAttributes,
    })
}

/// The facts of a volume that decide whether file ids can be trusted (CORE-041, ARCH-089).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    /// The file system name, as `GetVolumeInformationByHandleW` reports it (`NTFS`, `ReFS`,
    /// `FAT32`, `exFAT`, …).
    pub fs_name: String,
    /// `GetDriveTypeW` is `DRIVE_REMOTE` (a network share or a mapped drive).
    pub remote: bool,
    /// The path is under a cloud-files sync root (OneDrive and other cloud-files providers).
    pub cloud_sync_root: bool,
}

impl VolumeInfo {
    /// FileId and ChangeTime are trusted only on local NTFS or ReFS outside a cloud-files sync
    /// root. A OneDrive folder is itself NTFS, so NTFS alone is not enough.
    pub fn stable_ids(&self) -> bool {
        !self.remote
            && !self.cloud_sync_root
            && (self.fs_name.eq_ignore_ascii_case("NTFS")
                || self.fs_name.eq_ignore_ascii_case("ReFS"))
    }
}

/// `GetDriveTypeW`'s `DRIVE_REMOTE` (winbase.h; the `windows` crate has it under
/// `System_WindowsProgramming`, a feature `lil-platform` does not otherwise need).
const DRIVE_REMOTE: u32 = 4;

/// The [`VolumeInfo`] of the volume holding `path`: `volume::volume_root`, then
/// `GetVolumeInformationByHandleW` (file system name) and `GetDriveTypeW` on the root, and
/// `CfGetSyncRootInfoByPath(CF_SYNC_ROOT_INFO_BASIC)` on `path`'s nearest existing folder,
/// mapped through [`sync_root_from`]. There is no volume serial here: this one would be 32-bit,
/// and `FILE_ID_INFO`'s is 64-bit.
pub fn volume_info(path: &Path) -> io::Result<VolumeInfo> {
    let path = long_path(path);
    let existing = path
        .ancestors()
        .find(|p| !p.as_os_str().is_empty() && attributes(p).is_ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no part of the path exists"))?
        .to_path_buf();
    let root = volume_root(&existing)?;

    let root_file = open_attributes(&root)?;
    let mut fs_name = vec![0u16; 261];
    // SAFETY: the root handle is open; `fs_name` is a writable slice of the length passed;
    // the other outputs are not requested.
    unsafe {
        GetVolumeInformationByHandleW(
            handle(&root_file),
            None,
            None,
            None,
            None,
            Some(&mut fs_name),
        )
    }
    .map_err(|e| io_error(&e))?;
    let len = fs_name
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(fs_name.len());
    let fs_name = String::from_utf16_lossy(&fs_name[..len]);

    let plain_root = wide(&strip_verbatim(&root))?;
    // SAFETY: `plain_root` is NUL-terminated and outlives the call.
    let remote = unsafe { GetDriveTypeW(PCWSTR(plain_root.as_ptr())) } == DRIVE_REMOTE;

    let target = wide(&existing)?;
    let mut basic = CF_SYNC_ROOT_BASIC_INFO::default();
    // SAFETY: `target` is NUL-terminated; `basic` is a writable buffer of the size passed for
    // the BASIC class; the returned length is not requested.
    let cf = unsafe {
        CfGetSyncRootInfoByPath(
            PCWSTR(target.as_ptr()),
            CF_SYNC_ROOT_INFO_BASIC,
            (&raw mut basic).cast(),
            size_of::<CF_SYNC_ROOT_BASIC_INFO>() as u32,
            None,
        )
    }
    .map_err(|e| e.code().0);
    let cloud_sync_root = sync_root_from(cf, &fs_name, remote)?;
    Ok(VolumeInfo {
        fs_name,
        remote,
        cloud_sync_root,
    })
}

/// `HRESULT_FROM_WIN32(ERROR_CLOUD_FILE_NOT_UNDER_SYNC_ROOT)` (390).
pub const HRESULT_NOT_UNDER_SYNC_ROOT: i32 = ERROR_CLOUD_FILE_NOT_UNDER_SYNC_ROOT.to_hresult().0;

/// `HRESULT_FROM_WIN32(ERROR_INVALID_FUNCTION)` (1). On a local NTFS volume without the cloud
/// files filter attached (no sync root registered on it), `CfGetSyncRootInfoByPath` fails with
/// this instead of 390: observed for every path of the system volume on the plan-2b test
/// machine (2026-10-16). Without the filter no path can be under a sync root.
pub const HRESULT_NO_CLOUD_FILTER: i32 = ERROR_INVALID_FUNCTION.to_hresult().0;

/// The pure mapping of `CfGetSyncRootInfoByPath`'s result to "under a sync root":
/// - success is `true`;
/// - `HRESULT_FROM_WIN32(ERROR_CLOUD_FILE_NOT_UNDER_SYNC_ROOT)` (390) is `false`, and so is
///   `HRESULT_FROM_WIN32(ERROR_INVALID_FUNCTION)` (1: no cloud filter on the volume);
/// - any other failure is `false` on a remote volume or one that is neither NTFS nor ReFS (FAT
///   and exFAT sticks and SMB shares have no cloud filter), and an error on local NTFS or ReFS,
///   the volumes where stable ids could otherwise be trusted (ruling Q4: unreadable volume
///   facts mean no stable ids).
pub fn sync_root_from(cf: Result<(), i32>, fs_name: &str, remote: bool) -> io::Result<bool> {
    match cf {
        Ok(()) => Ok(true),
        // 390 is the documented "not under a sync root". 1 is what local NTFS returns when no
        // cloud filter is attached: observed for every system-volume path on the plan-2b test
        // machine (2026-10-16). Plan 2b ruling Q3 maps both to `false`; any other failure on
        // local NTFS (or ReFS, ruling Q4) stays an error.
        Err(hr) if hr == HRESULT_NOT_UNDER_SYNC_ROOT || hr == HRESULT_NO_CLOUD_FILTER => Ok(false),
        Err(_)
            if remote
                || !(fs_name.eq_ignore_ascii_case("NTFS")
                    || fs_name.eq_ignore_ascii_case("ReFS")) =>
        {
            Ok(false)
        }
        Err(hr) => Err(io_error(&windows::core::Error::from_hresult(
            windows::core::HRESULT(hr),
        ))),
    }
}

// ---------------------------------------------------------------------------------------------
// Locks.
// ---------------------------------------------------------------------------------------------

/// An `OVERLAPPED` naming the byte offset of a lock range on a synchronous handle.
fn at(offset: u64) -> OVERLAPPED {
    OVERLAPPED {
        Anonymous: OVERLAPPED_0 {
            Anonymous: OVERLAPPED_0_0 {
                Offset: offset as u32,
                OffsetHigh: (offset >> 32) as u32,
            },
        },
        ..Default::default()
    }
}

/// `LockFileEx(EXCLUSIVE | FAIL_IMMEDIATELY)` on one byte at `offset`: `false` at once when
/// another handle holds it.
fn lock_byte(file: &File, offset: u64) -> io::Result<bool> {
    let mut overlapped = at(offset);
    // SAFETY: the handle is open while `file` is borrowed and is synchronous, so the call
    // completes before it returns and `overlapped` is not used afterwards.
    let locked = unsafe {
        LockFileEx(
            handle(file),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            None,
            1,
            0,
            &mut overlapped,
        )
    };
    match locked {
        Ok(()) => Ok(true),
        Err(e) if e.code() == ERROR_LOCK_VIOLATION.to_hresult() => Ok(false),
        Err(e) => Err(io_error(&e)),
    }
}

fn unlock_byte(file: &File, offset: u64) {
    let mut overlapped = at(offset);
    // SAFETY: as in `lock_byte`. A failure leaves nothing to do: closing the handle releases
    // its locks anyway.
    let _ = unsafe { UnlockFileEx(handle(file), None, 1, 0, &mut overlapped) };
}

/// A held lock on byte 0 of a lock file (`core.lock`, ARCH-028). Drop unlocks and closes.
#[derive(Debug)]
pub struct LockedFile {
    file: File,
}

impl Drop for LockedFile {
    fn drop(&mut self) {
        unlock_byte(&self.file, 0);
    }
}

/// Opens or creates the lock file at `path` and takes `LockFileEx(EXCLUSIVE |
/// FAIL_IMMEDIATELY)` on `[0, 1)`. `None` at once when another handle holds it.
///
/// The handle shares read and write but not delete, so while it is open the file can't be
/// deleted or renamed (error 32): a second core always opens this same file and finds the
/// lock held, instead of creating a fresh one beside an orphaned handle. That second open
/// still succeeds, since neither handle asks for delete access. An open that fails because
/// another program holds the file with delete access (or with no sharing) is that error,
/// 32: the caller decides what it means (the core serves read-only, `take_core_locks`).
pub fn try_lock_exclusive(path: &Path) -> io::Result<Option<LockedFile>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE).0)
        .open(long_path(path))?;
    Ok(lock_byte(&file, 0)?.then_some(LockedFile { file }))
}

/// The append lock's byte: offset 2⁶⁴−2, length 1, past any real data (ARCH-081).
pub const APPEND_LOCK_OFFSET: u64 = u64::MAX - 1;

/// A held append lock on a log file (ARCH-081, CORE-009). Drop unlocks.
#[derive(Debug)]
pub struct AppendLock<'a> {
    file: &'a File,
}

impl Drop for AppendLock<'_> {
    fn drop(&mut self) {
        unlock_byte(self.file, APPEND_LOCK_OFFSET);
    }
}

/// `LockFileEx(EXCLUSIVE | FAIL_IMMEDIATELY)` on `[2⁶⁴−2, +1)` of `file`, which must have
/// been opened with read or write access (`LockFileEx`'s requirement; an append-only handle
/// also needs `read(true)`). `None` at once when another handle holds it: this never waits,
/// and the caller (the store) polls under its own retry budget.
pub fn try_lock_append(file: &File) -> io::Result<Option<AppendLock<'_>>> {
    Ok(lock_byte(file, APPEND_LOCK_OFFSET)?.then_some(AppendLock { file }))
}

// ---------------------------------------------------------------------------------------------
// Shared opens and the probe.
// ---------------------------------------------------------------------------------------------

/// The sharing an [`open_shared`] handle allows others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Share {
    /// No sharing: every other open for read, write or delete fails while it is held.
    None,
    /// `FILE_SHARE_READ`: others may read, never write, delete or rename.
    Read,
    /// `FILE_SHARE_READ | FILE_SHARE_WRITE`: others may read and write, never delete or rename.
    ReadWrite,
}

/// Opens an existing file for reading with exactly `share`. It is the handle-holding helper's
/// primitive, standing in for Defender, a preview pane or an image viewer.
pub fn open_shared(path: &Path, share: Share) -> io::Result<File> {
    let mode = match share {
        Share::None => 0,
        Share::Read => FILE_SHARE_READ.0,
        Share::ReadWrite => (FILE_SHARE_READ | FILE_SHARE_WRITE).0,
    };
    OpenOptions::new()
        .read(true)
        .share_mode(mode)
        .open(long_path(path))
}

/// The reparse tag of `path` when it is a reparse point (`FileAttributeTagInfo` through an
/// attribute-only open that doesn't follow links), `None` otherwise.
pub fn reparse_tag(path: &Path) -> io::Result<Option<u32>> {
    let file = open_attributes(path)?;
    // SAFETY: the class is read into its documented structure.
    let info: FILE_ATTRIBUTE_TAG_INFO = unsafe { info(&file, FileAttributeTagInfo) }?;
    Ok((info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0).then_some(info.ReparseTag))
}

/// `FSCTL_SET_REPARSE_POINT` (winioctl.h; the `windows` crate has it under
/// `Win32_System_Ioctl`, a feature `lil-platform` does not otherwise need).
const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
/// `IO_REPARSE_TAG_MOUNT_POINT`, the tag of a junction.
pub const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;

/// Creates the folder `link` as a junction (a mount-point reparse point) to the existing folder
/// `target`. Unlike a symbolic link it needs no privilege. Test support for the store's link
/// checks; the suite itself never creates links.
pub fn create_junction(link: &Path, target: &Path) -> io::Result<()> {
    let target = strip_verbatim(&std::path::absolute(target)?);
    let print: Vec<u16> = target.as_os_str().encode_wide().collect();
    let substitute: Vec<u16> = r"\??\"
        .encode_utf16()
        .chain(print.iter().copied())
        .collect();
    let bytes = |units: &[u16]| {
        u16::try_from(units.len() * 2)
            .map_err(|_| io::Error::other("the junction target is too long"))
    };
    let (sub_len, print_len) = (bytes(&substitute)?, bytes(&print)?);
    // MountPointReparseBuffer: four u16 offsets/lengths, then both names, each NUL-terminated.
    let path_bytes = usize::from(sub_len) + 2 + usize::from(print_len) + 2;
    let data_len = u16::try_from(8 + path_bytes)
        .map_err(|_| io::Error::other("the junction target is too long"))?;
    let mut buf: Vec<u8> = Vec::with_capacity(8 + usize::from(data_len));
    buf.extend_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
    buf.extend_from_slice(&data_len.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&sub_len.to_le_bytes());
    buf.extend_from_slice(&(sub_len + 2).to_le_bytes());
    buf.extend_from_slice(&print_len.to_le_bytes());
    for unit in substitute.iter().chain(&[0]).chain(&print).chain(&[0]) {
        buf.extend_from_slice(&unit.to_le_bytes());
    }
    std::fs::create_dir(long_path(link))?;
    let made = (|| {
        let dir = OpenOptions::new()
            .write(true)
            .custom_flags((FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).0)
            .open(long_path(link))?;
        let mut returned = 0u32;
        // SAFETY: `buf` is a valid REPARSE_DATA_BUFFER of the length passed; the handle is open
        // while `dir` lives and the call is synchronous.
        unsafe {
            windows::Win32::System::IO::DeviceIoControl(
                handle(&dir),
                FSCTL_SET_REPARSE_POINT,
                Some(buf.as_ptr().cast()),
                u32::try_from(buf.len()).unwrap_or(u32::MAX),
                None,
                0,
                Some(&mut returned),
                None,
            )
        }
        .map_err(|e| io_error(&e))
    })();
    if made.is_err() {
        let _ = std::fs::remove_dir(long_path(link));
    }
    made
}

/// `IsReparseTagNameSurrogate`: the tag names another file or folder (a symbolic link, a
/// junction or mount point), as opposed to one that only changes how the data is stored (a
/// cloud file, a deduplicated file).
pub fn is_name_surrogate(tag: u32) -> bool {
    tag & 0x2000_0000 != 0
}

/// A file opened for reading and deleting with no sharing: while it is open nobody else can
/// open it to read, write, rename or delete, so what was read is what [`ExclusiveFile::delete`]
/// deletes. An existing handle (even one sharing delete) makes the open fail with error 32.
#[derive(Debug)]
pub struct ExclusiveFile {
    file: File,
}

/// Opens the existing file `path` with `GENERIC_READ | DELETE` and no sharing. A link is
/// opened itself, never followed (`FILE_FLAG_OPEN_REPARSE_POINT`), so a delete through the
/// handle removes the link, never the file it points to.
pub fn open_exclusive(path: &Path) -> io::Result<ExclusiveFile> {
    let file = OpenOptions::new()
        .access_mode(FILE_GENERIC_READ.0 | DELETE.0)
        .share_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(long_path(path))?;
    Ok(ExclusiveFile { file })
}

/// Sets a disposition class on `file`.
///
/// # Safety
/// `T` must be the structure `class` takes.
unsafe fn set_info<T>(file: &File, class: FILE_INFO_BY_HANDLE_CLASS, value: &T) -> io::Result<()> {
    // SAFETY: `value` is a valid `T` of the size passed, and the caller guarantees it is the
    // structure `class` takes; the handle is open while `file` is borrowed.
    unsafe {
        SetFileInformationByHandle(
            handle(file),
            class,
            std::ptr::from_ref(value).cast(),
            size_of::<T>() as u32,
        )
    }
    .map_err(|e| io_error(&e))
}

impl ExclusiveFile {
    /// The open file, to read its data.
    pub fn file(&self) -> &File {
        &self.file
    }

    /// Deletes the file through this handle, then closes it: no other open can come between
    /// the read and the delete. `FileDispositionInfoEx` with `FILE_DISPOSITION_FLAG_DELETE |
    /// FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS`,
    /// so a read-only file is deleted too and the name goes at once. A file system that
    /// refuses POSIX semantics (error 87, 50 or 1) is tried without them, and one without the
    /// `Ex` class with the plain `FileDispositionInfo` (which a read-only file refuses with 5).
    pub fn delete(self) -> io::Result<()> {
        let base =
            FILE_DISPOSITION_FLAG_DELETE.0 | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE.0;
        let unsupported = |e: &io::Error| matches!(e.raw_os_error(), Some(87 | 50 | 1));
        for flags in [base | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS.0, base] {
            let info = FILE_DISPOSITION_INFO_EX {
                Flags: FILE_DISPOSITION_INFO_EX_FLAGS(flags),
            };
            // SAFETY: FileDispositionInfoEx takes FILE_DISPOSITION_INFO_EX; the handle has
            // DELETE access.
            match unsafe { set_info(&self.file, FileDispositionInfoEx, &info) } {
                Ok(()) => return Ok(()),
                Err(e) if unsupported(&e) => {}
                Err(e) => return Err(e),
            }
        }
        let info = FILE_DISPOSITION_INFO { DeleteFile: true };
        // SAFETY: FileDispositionInfo takes FILE_DISPOSITION_INFO; the handle has DELETE access.
        unsafe { set_info(&self.file, FileDispositionInfo, &info) }
    }
}

/// The file facts the store, index and watcher read, behind a trait so tests fake volumes and
/// placeholders.
pub trait Probe: Send + Sync + std::fmt::Debug {
    /// [`identity`]: attribute-only, never reads data.
    fn identity(&self, path: &Path) -> io::Result<FileIdentity>;
    /// [`volume_info`].
    fn volume_info(&self, path: &Path) -> io::Result<VolumeInfo>;
    /// Opens a file to read its data. It is the only way the store, index and watcher read
    /// file data.
    fn open_read(&self, path: &Path) -> io::Result<File>;
    /// [`long_name`]: every 8.3 short component expanded (attribute-only, never reads data).
    fn long_name(&self, path: &Path) -> io::Result<PathBuf> {
        long_name(path)
    }
    /// [`long_form`] with [`Probe::identity`] and [`Probe::long_name`]: the long form of a
    /// path that may not exist yet, failing closed on any error but a missing component.
    fn long_form(&self, path: &Path) -> io::Result<PathBuf> {
        long_form(path, &|p| self.identity(p), &|p| self.long_name(p))
    }
    /// [`reparse_tag`]: the reparse tag of a reparse point, read without following it.
    fn reparse_tag(&self, path: &Path) -> io::Result<Option<u32>> {
        reparse_tag(path)
    }
    /// The entries of the folder `dir` (`std::fs::read_dir` of its [`long_path`]). The store's
    /// and the index's walks of the workspace list their folders through it.
    fn read_dir(&self, dir: &Path) -> io::Result<std::fs::ReadDir> {
        std::fs::read_dir(long_path(dir))
    }
}

/// The real [`Probe`]. `open_read` shares read, write and delete, so a reader never blocks a
/// writer's replace.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProbe;

impl Probe for SystemProbe {
    fn identity(&self, path: &Path) -> io::Result<FileIdentity> {
        identity(path)
    }

    fn volume_info(&self, path: &Path) -> io::Result<VolumeInfo> {
        volume_info(path)
    }

    fn open_read(&self, path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
            .open(long_path(path))
    }
}

#[cfg(test)]
mod tests {
    use windows::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, REPLACEFILE_WRITE_THROUGH,
    };

    use super::*;

    fn volume(fs_name: &str, remote: bool, cloud_sync_root: bool) -> VolumeInfo {
        VolumeInfo {
            fs_name: fs_name.to_string(),
            remote,
            cloud_sync_root,
        }
    }

    #[test] // spec: CORE-041, ARCH-089
    fn stable_ids_only_on_local_ntfs_or_refs_outside_sync_roots() {
        for fs_name in ["NTFS", "ReFS", "FAT32", "exFAT"] {
            for remote in [false, true] {
                for cloud in [false, true] {
                    let want = matches!(fs_name, "NTFS" | "ReFS") && !remote && !cloud;
                    assert_eq!(
                        volume(fs_name, remote, cloud).stable_ids(),
                        want,
                        "{fs_name} remote={remote} cloud={cloud}"
                    );
                }
            }
        }
    }

    #[test] // spec: CORE-041, ARCH-089
    fn sync_root_failure_is_false_off_local_ntfs() {
        let other = windows::Win32::Foundation::E_FAIL.0;
        let cases = [
            ("FAT32", false),
            ("exFAT", false),
            ("NTFS", true),
            ("NTFS", false),
            ("ReFS", true),
            ("ReFS", false),
        ];
        for (fs_name, remote) in cases {
            let label = format!("{fs_name} remote={remote}");
            assert!(sync_root_from(Ok(()), fs_name, remote).unwrap(), "{label}");
            assert!(
                !sync_root_from(Err(HRESULT_NOT_UNDER_SYNC_ROOT), fs_name, remote).unwrap(),
                "{label}"
            );
            assert!(
                !sync_root_from(Err(HRESULT_NO_CLOUD_FILTER), fs_name, remote).unwrap(),
                "{label}"
            );
            let got = sync_root_from(Err(other), fs_name, remote);
            // Ruling Q4: ReFS is treated like NTFS.
            if (fs_name == "NTFS" || fs_name == "ReFS") && !remote {
                assert!(got.is_err(), "{label}: {got:?}");
            } else {
                assert!(!got.unwrap(), "{label}");
            }
        }
        assert_eq!(HRESULT_NOT_UNDER_SYNC_ROOT as u32, 0x8007_0186);
        assert_eq!(HRESULT_NO_CLOUD_FILTER as u32, 0x8007_0001);
    }

    /// Ruling Q3 (plan 2b): exactly two failure codes mean "not under a sync root" on local
    /// NTFS, pinned by their raw HRESULT values so neither mapping can be dropped silently.
    #[test] // spec: CORE-041, ARCH-089
    fn sync_root_not_under_codes_390_and_1_are_false_on_local_ntfs() {
        let not_under = 0x8007_0186_u32 as i32; // HRESULT_FROM_WIN32(390)
        let no_filter = 0x8007_0001_u32 as i32; // HRESULT_FROM_WIN32(ERROR_INVALID_FUNCTION)
        assert!(!sync_root_from(Err(not_under), "NTFS", false).unwrap());
        assert!(!sync_root_from(Err(no_filter), "NTFS", false).unwrap());
        // Neighbouring codes are not mapped: on local NTFS (and ReFS, ruling Q4) they stay
        // errors.
        for other in [0x8007_0002_u32, 0x8007_0032, 0x8007_0185, 0x8000_4005] {
            for fs_name in ["NTFS", "ReFS"] {
                assert!(
                    sync_root_from(Err(other as i32), fs_name, false).is_err(),
                    "{fs_name} {other:#x}"
                );
            }
        }
    }

    fn units(p: &Path) -> usize {
        p.as_os_str().encode_wide().count()
    }

    /// The walk up to the longest existing prefix goes through the `identity` it is given, so a
    /// probe can inject errors: a missing part (or one below a file) moves the walk up, any
    /// other error ends it with that error (fail closed) before `long_name` runs.
    #[test] // spec: CORE-040
    fn long_form_fails_closed_on_an_error_mid_walk() {
        let folder = FileIdentity {
            volume_serial: 1,
            file_id: None,
            size: 0,
            last_write: 0,
            change_time: None,
            attributes: 0x10,
        };
        let base = Path::new(r"C:\ws");
        let target = base.join(r"NOTES~1\new\x.txt");
        let long_name = |p: &Path| -> io::Result<PathBuf> {
            Ok(PathBuf::from(
                p.to_string_lossy().replace("NOTES~1", "notes.long"),
            ))
        };
        let with = |missing: &[&str], below_file: &[&str], denied: &[&str]| {
            let (missing, below_file, denied): (Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>) = (
                missing.iter().map(|m| base.join(m)).collect(),
                below_file.iter().map(|m| base.join(m)).collect(),
                denied.iter().map(|m| base.join(m)).collect(),
            );
            long_form(
                &target,
                &|p: &Path| {
                    if missing.iter().any(|m| m == p) {
                        Err(io::Error::from(io::ErrorKind::NotFound))
                    } else if below_file.iter().any(|m| m == p) {
                        Err(io::Error::from(io::ErrorKind::NotADirectory))
                    } else if denied.iter().any(|m| m == p) {
                        Err(io::Error::from_raw_os_error(5))
                    } else {
                        Ok(folder)
                    }
                },
                &long_name,
            )
        };
        let expanded = base.join(r"notes.long\new\x.txt");
        assert_eq!(with(&[r"NOTES~1\new\x.txt"], &[], &[]).unwrap(), expanded);
        assert_eq!(
            with(&[r"NOTES~1\new\x.txt"], &[r"NOTES~1\new"], &[]).unwrap(),
            expanded
        );
        let denied = with(&[r"NOTES~1\new\x.txt"], &[], &[r"NOTES~1\new"]).unwrap_err();
        assert_eq!(denied.raw_os_error(), Some(5), "{denied}");
        let denied = with(&[r"NOTES~1\new\x.txt", r"NOTES~1\new"], &[], &["NOTES~1"]).unwrap_err();
        assert_eq!(denied.raw_os_error(), Some(5), "{denied}");
    }

    #[test] // spec: CORE-040
    fn long_path_prefixes_from_247_units_and_unc() {
        let at = |n: usize| PathBuf::from(format!(r"C:\{}", "a".repeat(n - 3)));
        let short = at(246);
        assert_eq!(units(&short), 246);
        assert_eq!(long_path(&short), short);
        let long = at(247);
        assert_eq!(
            long_path(&long),
            PathBuf::from(format!(r"\\?\{}", long.display()))
        );

        // Counted in UTF-16 code units: 122 non-BMP characters are 244 units but 122 chars.
        let wide_chars = PathBuf::from(format!(r"C:\{}", "😀".repeat(122)));
        assert_eq!(units(&wide_chars), 247);
        assert!(wide_chars.to_string_lossy().chars().count() < 247);
        assert!(
            long_path(&wide_chars)
                .to_string_lossy()
                .starts_with(r"\\?\C:\")
        );

        // UNC paths get `\\?\UNC\`.
        let unc = PathBuf::from(format!(r"\\srv\share\{}", "u".repeat(240)));
        let got = long_path(&unc);
        assert_eq!(
            got,
            PathBuf::from(format!(r"\\?\UNC\srv\share\{}", "u".repeat(240)))
        );
        assert_eq!(
            long_path(&PathBuf::from(r"\\srv\share\x")),
            PathBuf::from(r"\\srv\share\x")
        );

        // `..`, `.` and `/` are resolved before the length is counted.
        let dotted = PathBuf::from(format!(r"C:\x\..\.\{}", "b".repeat(250)));
        assert_eq!(
            long_path(&dotted),
            PathBuf::from(format!(r"\\?\C:\{}", "b".repeat(250)))
        );
        assert_eq!(
            long_path(Path::new("C:/a/../b/./c")),
            PathBuf::from(r"C:\b\c")
        );
        let near = PathBuf::from(format!(r"C:\x\..\{}", "c".repeat(243)));
        assert_eq!(units(&near), 251);
        assert_eq!(
            long_path(&near),
            PathBuf::from(format!(r"C:\{}", "c".repeat(243)))
        );

        // Already verbatim: unchanged. Relative: made absolute.
        let verbatim = PathBuf::from(r"\\?\C:\x\..\y");
        assert_eq!(long_path(&verbatim), verbatim);
        assert!(long_path(Path::new("rel")).is_absolute());
    }

    #[test] // spec: CORE-041
    fn file_id_all_zero_or_all_ff_is_none() {
        assert_eq!(file_id_from([0; 16]), None);
        assert_eq!(file_id_from([0xFF; 16]), None);
        let mut id = [0u8; 16];
        id[0] = 7;
        assert_eq!(file_id_from(id), Some(id));
        let mut id = [0xFF; 16];
        id[15] = 0xFE;
        assert_eq!(file_id_from(id), Some(id));
    }

    #[test] // spec: ARCH-074
    fn classify_maps_documented_error_codes() {
        let class = |code| classify(&io::Error::from_raw_os_error(code));
        for code in [32, 33, 1224] {
            assert_eq!(class(code), RetryClass::SharingViolation, "{code}");
        }
        for code in [1175, 1176] {
            assert_eq!(class(code), RetryClass::ReplaceRetry, "{code}");
        }
        assert_eq!(class(1177), RetryClass::ReplacementNotMoved);
        for code in [80, 183] {
            assert_eq!(class(code), RetryClass::TargetExists, "{code}");
        }
        for code in [1, 50] {
            assert_eq!(class(code), RetryClass::Unsupported, "{code}");
        }
        for code in [112, 39] {
            assert_eq!(class(code), RetryClass::DiskFull, "{code}");
        }
        for code in [0, 2, 5, 87, 1178] {
            assert_eq!(class(code), RetryClass::Other, "{code}");
        }
        // Error 5 is retried for a folder move only (ruling Q19); a file operation never
        // retries it, and every other code classifies as `classify` says.
        let moving = |code, folder| classify_move(&io::Error::from_raw_os_error(code), folder);
        assert_eq!(moving(5, true), RetryClass::SharingViolation);
        assert_eq!(moving(5, false), RetryClass::Other);
        for code in [0, 2, 32, 33, 80, 87, 112, 183, 1224] {
            for folder in [false, true] {
                assert_eq!(moving(code, folder), class(code), "{code} folder={folder}");
            }
        }
        assert_eq!(classify(&io::Error::other("no code")), RetryClass::Other);
        // Errors that came through an HRESULT carry the plain Win32 code.
        let hr = windows::core::Error::from_hresult(
            windows::Win32::Foundation::ERROR_SHARING_VIOLATION.to_hresult(),
        );
        assert_eq!(classify(&io_error(&hr)), RetryClass::SharingViolation);
    }

    #[test] // spec: ARCH-074
    fn replace_flags_exclude_write_through() {
        assert_eq!(
            REPLACE_FLAGS,
            REPLACEFILE_IGNORE_MERGE_ERRORS.0 | REPLACEFILE_IGNORE_ACL_ERRORS.0
        );
        assert_eq!(REPLACE_FLAGS, 0x6);
        assert_eq!(REPLACE_FLAGS & REPLACEFILE_WRITE_THROUGH.0, 0);
        // The one move never replaces an existing file.
        assert_eq!(MOVE_FLAGS, MOVEFILE_WRITE_THROUGH.0);
        assert_eq!(MOVE_FLAGS & MOVEFILE_REPLACE_EXISTING.0, 0);
    }

    #[test] // spec: ARCH-074
    fn exclusive_open_shuts_out_others_and_deletes_through_its_handle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".x.lil-bak-1-1");
        std::fs::write(&path, b"old").unwrap();
        let held = open_exclusive(&path).unwrap();
        let mut text = String::new();
        io::Read::read_to_string(&mut held.file(), &mut text).unwrap();
        assert_eq!(text, "old");
        let other = OpenOptions::new().write(true).open(&path).unwrap_err();
        assert_eq!(other.raw_os_error(), Some(32), "{other:?}");
        held.delete().unwrap();
        assert!(!path.exists());

        // A handle that is already open (sharing everything) keeps it out.
        std::fs::write(&path, b"again").unwrap();
        let reader = SystemProbe.open_read(&path).unwrap();
        assert_eq!(open_exclusive(&path).unwrap_err().raw_os_error(), Some(32));
        drop(reader);
        open_exclusive(&path).unwrap();
        assert!(path.exists(), "dropping without delete keeps the file");
    }

    #[test] // spec: ARCH-074
    fn exclusive_delete_removes_a_read_only_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".ro.lil-bak-1-1");
        std::fs::write(&path, b"old").unwrap();
        set_readonly(&path, true).unwrap();
        open_exclusive(&path)
            .unwrap()
            .delete()
            .expect("a read-only file is deleted too");
        assert!(!path.exists());
    }

    #[test] // spec: ARCH-074
    fn a_junction_is_a_name_surrogate_reparse_point() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("inside.txt"), b"x").unwrap();
        let link = dir.path().join("link");
        create_junction(&link, &target).unwrap();
        let tag = reparse_tag(&link).unwrap();
        assert_eq!(tag, Some(IO_REPARSE_TAG_MOUNT_POINT));
        assert!(is_name_surrogate(tag.unwrap()));
        assert_eq!(
            std::fs::read(link.join("inside.txt")).unwrap(),
            b"x",
            "it resolves"
        );
        std::fs::remove_dir(&link).unwrap();
        assert!(
            target.join("inside.txt").exists(),
            "removing the junction keeps the target"
        );
    }

    #[test] // spec: ARCH-074
    fn reparse_tags_tell_links_from_stored_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.txt");
        std::fs::write(&path, b"x").unwrap();
        assert_eq!(reparse_tag(&path).unwrap(), None);
        assert_eq!(SystemProbe.reparse_tag(&path).unwrap(), None);
        assert!(is_name_surrogate(0xA000_000C), "IO_REPARSE_TAG_SYMLINK");
        assert!(is_name_surrogate(0xA000_0003), "IO_REPARSE_TAG_MOUNT_POINT");
        assert!(!is_name_surrogate(0x9000_101A), "IO_REPARSE_TAG_CLOUD_1");
        assert!(!is_name_surrogate(0x8000_0013), "IO_REPARSE_TAG_DEDUP");
    }

    #[test]
    fn placeholder_attributes_are_the_three_recall_bits() {
        assert_eq!(PLACEHOLDER_ATTRIBUTES, 0x0040_0000 | 0x0004_0000 | 0x1000);
    }
}
