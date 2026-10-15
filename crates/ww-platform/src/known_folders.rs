//! Known folders via `SHGetKnownFolderPath`. Only `Env::from_system` calls these.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    FOLDERID_LocalAppData, FOLDERID_Profile, FOLDERID_RoamingAppData, KF_FLAG_DEFAULT,
    SHGetKnownFolderPath,
};
use windows::core::GUID;

use crate::env::EnvError;

/// `FOLDERID_RoamingAppData` (`%APPDATA%`).
pub fn roaming_app_data() -> Result<PathBuf, EnvError> {
    known_folder(&FOLDERID_RoamingAppData)
}

/// `FOLDERID_LocalAppData` (`%LOCALAPPDATA%`).
pub fn local_app_data() -> Result<PathBuf, EnvError> {
    known_folder(&FOLDERID_LocalAppData)
}

/// `FOLDERID_Profile` (`%USERPROFILE%`).
pub fn profile() -> Result<PathBuf, EnvError> {
    known_folder(&FOLDERID_Profile)
}

fn known_folder(id: &GUID) -> Result<PathBuf, EnvError> {
    // SAFETY: `id` is a valid GUID reference. On success the returned buffer is NUL-terminated
    // and owned by us; it is copied out and then freed with `CoTaskMemFree` exactly once.
    unsafe {
        let pwstr =
            SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).map_err(|e| EnvError::Windows {
                call: "SHGetKnownFolderPath",
                message: e.to_string(),
            })?;
        let path = PathBuf::from(OsString::from_wide(pwstr.as_wide()));
        CoTaskMemFree(Some(pwstr.as_ptr().cast_const().cast()));
        Ok(path)
    }
}
