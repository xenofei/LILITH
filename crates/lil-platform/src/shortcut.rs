//! The AUMID shortcut (APP-022; Appendix G "Activation and registration"; P4a-R37):
//! [`Env::start_menu_shortcut`], targeting the channel's `lil-image.exe`, with
//! `System.AppUserModel.ID` = [`Env::notification_aumid`] and
//! `System.AppUserModel.ToastActivatorCLSID` = [`Env::toast_activator_clsid`], whose per-user
//! registration `<classes root>\CLSID\{<CLSID>}\LocalServer32 = "<exe>"` is written, checked and
//! removed with the shortcut. The CLSID only keeps a missed toast in the notification centre:
//! every toast activates by protocol, and no COM activator runs.
//!
//! With them the AUMID itself is registered per user, `<classes root>\AppUserModelId\<AUMID>`
//! with `DisplayName` and `CustomActivator` = the CLSID (Appendix G, plan 4a ruling
//! 2026-10-24): a toast posts with only this key (Task 1's S1.2), so a new install's first
//! toast works before the Start-menu index has the shortcut. On a test channel each key carries
//! [`crate::classes::TEST_MARKER`].
//!
//! The shortcut goes through `IShellLinkW` and its `IPropertyStore`, as Task 1's spike wrote
//! and read it back (S1.2p). The COM calls run on a short thread of their own with a
//! single-threaded apartment, so the caller's thread keeps whatever apartment it has.

use std::io;
use std::path::{Path, PathBuf};

use windows::Win32::Storage::EnhancedStorage::{
    PKEY_AppUserModel_ID, PKEY_AppUserModel_ToastActivatorCLSID,
};
use windows::Win32::System::Com::StructuredStorage::{
    InitPropVariantFromCLSID, PROPVARIANT, PropVariantToGUID,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize, IPersistFile, STGM_READ,
};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{IShellLinkW, SHStrDupW, ShellLink};
use windows::core::{BSTR, GUID, HSTRING, Interface};

use crate::classes::{
    delete_tree, get_string, hkcu_subkey, mark, quoted, require_test_channel, set_string,
};
use crate::desktop::{ShortcutState, same_path};
use crate::env::{Channel, Env};
use crate::names;
use crate::process::io_error;

/// What a shortcut says, read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutInfo {
    pub target: PathBuf,
    pub aumid: String,
    /// `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`, upper-case, when the shortcut carries one.
    pub toast_activator_clsid: Option<String>,
}

/// Runs `f` on a new thread inside a single-threaded COM apartment, and returns what it returns.
fn with_com<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                // SAFETY: initialises COM on this new thread, which ends after `f`.
                let init = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
                let out = f();
                if init.is_ok() {
                    // SAFETY: balances the successful initialisation above; `f`'s COM objects
                    // are all dropped by now.
                    unsafe { CoUninitialize() };
                }
                out
            })
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

/// The CLSID string's `GUID`.
fn clsid_guid(clsid: &str) -> io::Result<GUID> {
    let hex: String = clsid.chars().filter(char::is_ascii_hexdigit).collect();
    u128::from_str_radix(&hex, 16)
        .ok()
        .filter(|_| hex.len() == 32)
        .map(GUID::from_u128)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("bad CLSID {clsid}")))
}

/// A `GUID` in the registry form, upper-case.
fn clsid_string(guid: GUID) -> String {
    let v = guid.to_u128();
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:04X}-{:012X}}}",
        v >> 96,
        (v >> 80) & 0xFFFF,
        (v >> 64) & 0xFFFF,
        (v >> 48) & 0xFFFF,
        v & 0xFFFF_FFFF_FFFF
    )
}

/// `HKCU`'s subkey of `<classes root>\CLSID\{<CLSID>}`.
fn clsid_key(env: &Env) -> io::Result<String> {
    Ok(format!(
        r"{}\CLSID\{}",
        hkcu_subkey(&env.classes_root())?,
        env.toast_activator_clsid()
    ))
}

/// `HKCU`'s subkey of `<classes root>\AppUserModelId\<AUMID>`.
fn aumid_key(env: &Env) -> io::Result<String> {
    Ok(format!(
        r"{}\AppUserModelId\{}",
        hkcu_subkey(&env.classes_root())?,
        env.notification_aumid()
    ))
}

/// The AUMID's `DisplayName`: `LILITH` for stable, `LILITH (<Channel>)` otherwise.
fn display_name(env: &Env) -> String {
    match env.channel() {
        Channel::Stable => names::APP_NAME.to_string(),
        other => format!("{} ({})", names::APP_NAME, other.capitalised()),
    }
}

/// Writes the channel's AUMID shortcut to `exe`, with its AUMID and CLSID, registers the
/// CLSID's `LocalServer32` and the AUMID (`DisplayName`, `CustomActivator`) under the classes
/// root (P4a-R26: only after your own input). An existing shortcut is replaced. No channel has
/// an icon file yet, so no `IconUri` is written.
pub fn write_shortcut(env: &Env, exe: &Path) -> io::Result<()> {
    let lnk = env.start_menu_shortcut();
    let aumid = env.notification_aumid();
    let clsid = clsid_guid(&env.toast_activator_clsid())?;
    let server = quoted(exe)?;
    if let Some(dir) = lnk.parent() {
        std::fs::create_dir_all(dir)?;
    }
    with_com(|| save(&lnk, exe, &aumid, clsid)).map_err(|e| io_error(&e))?;
    let clsid_key = clsid_key(env)?;
    set_string(&format!(r"{clsid_key}\LocalServer32"), None, &server)?;
    mark(env, &clsid_key)?;
    let aumid_key = aumid_key(env)?;
    set_string(&aumid_key, Some("DisplayName"), &display_name(env))?;
    set_string(
        &aumid_key,
        Some("CustomActivator"),
        &env.toast_activator_clsid(),
    )?;
    mark(env, &aumid_key)
}

fn save(lnk: &Path, exe: &Path, aumid: &str, clsid: GUID) -> windows::core::Result<()> {
    // SAFETY: COM calls on interfaces this function owns, on a thread inside an apartment
    // (`with_com`). The AUMID's PROPVARIANT owns its CoTaskMem string and frees it on drop.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(exe.as_os_str()))?;
        if let Some(dir) = exe.parent() {
            link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
        }
        let store: IPropertyStore = link.cast()?;
        let mut value = PROPVARIANT::default();
        let text = SHStrDupW(&HSTRING::from(aumid))?;
        (*value.Anonymous.Anonymous).vt = VT_LPWSTR;
        (*value.Anonymous.Anonymous).Anonymous.pwszVal = text;
        store.SetValue(&PKEY_AppUserModel_ID, &value)?;
        let value = InitPropVariantFromCLSID(&clsid)?;
        store.SetValue(&PKEY_AppUserModel_ToastActivatorCLSID, &value)?;
        store.Commit()?;
        let file: IPersistFile = link.cast()?;
        file.Save(&HSTRING::from(lnk.as_os_str()), true)
    }
}

/// Reads a shortcut back: its target, AUMID and toast activator CLSID.
pub fn read_shortcut(lnk: &Path) -> io::Result<ShortcutInfo> {
    with_com(|| load(lnk)).map_err(|e| io_error(&e))
}

fn load(lnk: &Path) -> windows::core::Result<ShortcutInfo> {
    // SAFETY: COM calls on interfaces this function owns, on a thread inside an apartment
    // (`with_com`); the path buffer's length is passed with it.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        let file: IPersistFile = link.cast()?;
        file.Load(&HSTRING::from(lnk.as_os_str()), STGM_READ)?;
        let mut buf = vec![0u16; 32_768];
        link.GetPath(&mut buf, std::ptr::null_mut(), 0)?;
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let target = PathBuf::from(String::from_utf16_lossy(&buf[..len]));
        let store: IPropertyStore = link.cast()?;
        let aumid = store
            .GetValue(&PKEY_AppUserModel_ID)
            .ok()
            .and_then(|v| BSTR::try_from(&v).ok())
            .map(|b| b.to_string())
            .unwrap_or_default();
        let toast_activator_clsid = store
            .GetValue(&PKEY_AppUserModel_ToastActivatorCLSID)
            .ok()
            .and_then(|v| PropVariantToGUID(&v).ok())
            .filter(|g| g.to_u128() != 0)
            .map(clsid_string);
        Ok(ShortcutInfo {
            target,
            aumid,
            toast_activator_clsid,
        })
    }
}

/// Removes a test channel's shortcut, its CLSID registration and its AUMID registration;
/// missing ones are no error. Refused, before anything is read or written, on a stable or dev
/// channel.
pub fn remove_shortcut(env: &Env) -> io::Result<()> {
    require_test_channel(env, "removing the shortcut")?;
    match std::fs::remove_file(env.start_menu_shortcut()) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    remove_clsid_registration(env)?;
    remove_aumid_registration(env)
}

/// Removes only a test channel's AUMID registration, `<classes root>\AppUserModelId\<AUMID>`,
/// by its exact name. Refused, before anything is read or written, on a stable or dev channel.
pub fn remove_aumid_registration(env: &Env) -> io::Result<()> {
    require_test_channel(env, "removing the AUMID registration")?;
    delete_tree(&aumid_key(env)?)
}

/// Removes only a test channel's CLSID registration, `<classes root>\CLSID\{<CLSID>}`.
/// Refused, before anything is read or written, on a stable or dev channel.
pub fn remove_clsid_registration(env: &Env) -> io::Result<()> {
    require_test_channel(env, "removing the CLSID registration")?;
    delete_tree(&clsid_key(env)?)
}

/// The shortcut's state for `exe`, checked in this order: there, its target, its AUMID, then
/// its CLSID, that CLSID's `LocalServer32` (P4a-R37) and the AUMID's registration, read back
/// (`DisplayName`, and `CustomActivator` naming the CLSID; plan 4a ruling 2026-10-24). A
/// missing or different registration is `WrongClsid`.
pub(crate) fn state(env: &Env, exe: &Path) -> ShortcutState {
    let lnk = env.start_menu_shortcut();
    if matches!(lnk.try_exists(), Ok(false)) {
        return ShortcutState::Missing;
    }
    let Ok(info) = read_shortcut(&lnk) else {
        return ShortcutState::WrongTarget;
    };
    if !same_path(&info.target, exe) {
        return ShortcutState::WrongTarget;
    }
    if info.aumid != env.notification_aumid() {
        return ShortcutState::WrongAumid;
    }
    let clsid = env.toast_activator_clsid();
    let registered = clsid_key(env)
        .ok()
        .and_then(|key| get_string(&format!(r"{key}\LocalServer32"), None));
    let clsid_ok = info
        .toast_activator_clsid
        .is_some_and(|c| c.eq_ignore_ascii_case(&clsid))
        && registered.is_some_and(|server| same_path(Path::new(&server), exe));
    let aumid_ok = aumid_key(env).ok().is_some_and(|key| {
        get_string(&key, Some("DisplayName")).as_deref() == Some(display_name(env).as_str())
            && get_string(&key, Some("CustomActivator"))
                .is_some_and(|c| c.eq_ignore_ascii_case(&clsid))
    });
    if clsid_ok && aumid_ok {
        ShortcutState::Ok
    } else {
        ShortcutState::WrongClsid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: APP-022
    fn clsid_strings_round_trip_through_guid() {
        let s = "{6F1A0C52-3B7E-4E0D-9A11-5C2B8D7E4F31}";
        let g = clsid_guid(s).unwrap();
        assert_eq!(clsid_string(g), s);
        assert_eq!(
            clsid_string(clsid_guid(&s.to_ascii_lowercase()).unwrap()),
            s
        );
        assert!(clsid_guid("{6F1A0C52}").is_err());
    }
}
