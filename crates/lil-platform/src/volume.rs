//! Volume lookup behind a trait so tests can inject a fake (`GetVolumePathNameW` in production),
//! and the free space of a volume (`GetDiskFreeSpaceExW`).

use std::ffi::OsString;
use std::fmt::Debug;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumePathNameW};
use windows::core::PCWSTR;

/// Finds the root of the volume a path lives on (`C:\`, `\server\share\`, or a mount point).
pub trait VolumeResolver: Send + Sync + Debug {
    fn volume_root(&self, path: &Path) -> io::Result<PathBuf>;
}

/// Production resolver: `GetVolumePathNameW`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemVolumeResolver;

impl VolumeResolver for SystemVolumeResolver {
    fn volume_root(&self, path: &Path) -> io::Result<PathBuf> {
        // The path may not exist yet (a fresh local_dir). Try the path, then each ancestor,
        // so a missing leaf still resolves to the volume that would hold it.
        let mut last_err = None;
        for candidate in path.ancestors().filter(|p| !p.as_os_str().is_empty()) {
            match query(candidate) {
                Ok(root) => return Ok(root),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| io::Error::from(io::ErrorKind::InvalidInput)))
    }
}

/// Volume root of `path` using the real Windows resolver.
pub fn volume_root(path: &Path) -> io::Result<PathBuf> {
    SystemVolumeResolver.volume_root(path)
}

/// The bytes free to the caller on the volume that holds `path` (or would hold it: a missing
/// path is resolved through its ancestors, as [`volume_root`] does). The jobs' disk preflight
/// reads it (CORE-128).
pub fn free_bytes(path: &Path) -> io::Result<u64> {
    let root = volume_root(path)?;
    let wide: Vec<u16> = root.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0u64;
    // SAFETY: `wide` is NUL-terminated and outlives the call; `free` is a valid u64 to write.
    unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&raw mut free), None, None) }
        .map_err(|e| io::Error::from_raw_os_error(win32_code(&e)))?;
    Ok(free)
}

fn query(path: &Path) -> io::Result<PathBuf> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // 32768 UTF-16 units is the maximum extended-length path size.
    let mut buf = vec![0u16; 32768];
    // SAFETY: `wide` is NUL-terminated and outlives the call; `buf` is a valid writable slice.
    unsafe { GetVolumePathNameW(PCWSTR(wide.as_ptr()), &mut buf) }
        .map_err(|e| io::Error::from_raw_os_error(win32_code(&e)))?;
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Ok(PathBuf::from(OsString::from_wide(&buf[..len])))
}

/// Win32 error code out of an HRESULT built by `HRESULT_FROM_WIN32`.
fn win32_code(e: &windows::core::Error) -> i32 {
    let hr = e.code().0 as u32;
    if hr & 0xFFFF_0000 == 0x8007_0000 {
        (hr & 0xFFFF) as i32
    } else {
        hr as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read-only: asks Windows for the volume of the temp directory (does not write anything).
    #[test] // spec: ARCH-097
    fn system_resolver_returns_drive_root_for_existing_and_missing_paths() {
        let tmp = std::env::temp_dir();
        let root = volume_root(&tmp).unwrap();
        assert!(root.to_string_lossy().ends_with('\\'), "{root:?}");
        let missing = tmp.join("lil-platform-no-such-dir").join("child");
        assert_eq!(volume_root(&missing).unwrap(), root);
    }

    /// Read-only: the temp directory's volume has some free space, and a missing child of it
    /// reports the same volume's figure.
    #[test] // spec: CORE-128
    fn free_bytes_reads_the_volume_of_a_missing_path() {
        let tmp = std::env::temp_dir();
        assert!(free_bytes(&tmp).unwrap() > 0);
        assert!(free_bytes(&tmp.join("lil-platform-no-such-dir").join("child")).unwrap() > 0);
    }
}
