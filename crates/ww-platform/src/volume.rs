//! Volume lookup behind a trait so tests can inject a fake (`GetVolumePathNameW` in production).

use std::ffi::OsString;
use std::fmt::Debug;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use windows::Win32::Storage::FileSystem::GetVolumePathNameW;
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
        let missing = tmp.join("ww-platform-no-such-dir").join("child");
        assert_eq!(volume_root(&missing).unwrap(), root);
    }
}
