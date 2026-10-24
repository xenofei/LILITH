//! The per-user handlers under the channel's classes root (APP-020, APP-021, APP-022;
//! Appendix G "Activation and registration"), and the registry calls the toast CLSID's
//! registration shares with them.
//!
//! Under `<root>` = [`Env::classes_root`]:
//! - `<root>\<scheme>`: `(Default) = "URL:LILITH (<Channel>)"`, an empty `URL Protocol`,
//!   `DefaultIcon = "<exe>",0` and `shell\open\command = "<exe>" "%1"`;
//! - `<root>\.lilref`: `(Default) = "LILITH.lilref.<channel>"`, and
//!   `<root>\LILITH.lilref.<channel>\shell\open\command = "<exe>" "%1"`.
//!
//! Written only after your own input (P4a-R26). A test channel's classes root is
//! `HKCU\Software\LILITH-test-<id>\Classes`, which the shell never reads, unless it is a
//! desktop-group channel that opted into the shell's root (`Env::with_shell_classes_root`):
//! then only its `lil-test-<id>` scheme is written there, never the `.lilref` association.
//! The remove helpers act only on test channels.

use std::io;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, WIN32_ERROR,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteKeyW, RegDeleteTreeW, RegEnumKeyExW,
    RegGetValueW, RegOpenKeyExW, RegSetValueExW,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

use crate::desktop::{HandlerState, same_path, split_first_argument};
use crate::env::{Channel, Env};
use crate::names;

/// `path` (`HKCU\<subkey>`) as the subkey under `HKEY_CURRENT_USER`. The suite writes only
/// per-user keys.
pub(crate) fn hkcu_subkey(path: &str) -> io::Result<String> {
    let rest = path
        .get(..5)
        .filter(|head| head.eq_ignore_ascii_case(r"HKCU\"))
        .map(|_| &path[5..]);
    match rest {
        Some(rest) if !rest.is_empty() => Ok(rest.to_string()),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not a per-user registry key: {path}"),
        )),
    }
}

fn check(e: WIN32_ERROR) -> io::Result<()> {
    if e == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(e.0 as i32))
    }
}

/// Sets the string value `name` (`None`: the key's default) of `HKCU\<subkey>`, creating the
/// key and its parents.
pub(crate) fn set_string(subkey: &str, name: Option<&str>, value: &str) -> io::Result<()> {
    let mut key = HKEY::default();
    // SAFETY: the strings outlive the call; `key` is a valid out-pointer and is closed below.
    check(unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(subkey),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
    })?;
    let data: Vec<u8> = value
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    let name = name.map(HSTRING::from);
    // SAFETY: `key` is open; `data` is a NUL-terminated UTF-16 string as REG_SZ needs.
    let set = check(unsafe {
        RegSetValueExW(
            key,
            name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr())),
            None,
            REG_SZ,
            Some(&data),
        )
    });
    // SAFETY: `key` was opened above and is closed once.
    let _ = unsafe { RegCloseKey(key) };
    set
}

/// The string value `name` (`None`: the default) of `HKCU\<subkey>`, or `None` when the key or
/// the value is missing or isn't a string.
pub(crate) fn get_string(subkey: &str, name: Option<&str>) -> Option<String> {
    let subkey = HSTRING::from(subkey);
    let name = name.map(HSTRING::from);
    let name = name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr()));
    let mut bytes = 0u32;
    // SAFETY: a size query: no buffer, `bytes` receives the size.
    check(unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &subkey,
            name,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut bytes),
        )
    })
    .ok()?;
    let mut buf = vec![0u16; (bytes as usize).div_ceil(2) + 1];
    let mut bytes = (buf.len() * 2) as u32;
    // SAFETY: `buf` holds `bytes` bytes; the call writes at most that many.
    check(unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &subkey,
            name,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    })
    .ok()?;
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// Deletes `HKCU\<subkey>` and everything below it; a missing key is no error.
pub(crate) fn delete_tree(subkey: &str) -> io::Result<()> {
    let subkey = HSTRING::from(subkey);
    // SAFETY: plain calls with a NUL-terminated key name.
    let (tree, key) = unsafe {
        (
            RegDeleteTreeW(HKEY_CURRENT_USER, &subkey),
            RegDeleteKeyW(HKEY_CURRENT_USER, &subkey),
        )
    };
    for e in [tree, key] {
        if e != ERROR_FILE_NOT_FOUND {
            check(e)?;
        }
    }
    Ok(())
}

/// Whether `HKCU\<subkey>` exists (read-only).
pub(crate) fn key_exists(subkey: &str) -> bool {
    let mut key = HKEY::default();
    // SAFETY: the name outlives the call; `key` is a valid out-pointer, closed below on success.
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(subkey),
            None,
            KEY_READ,
            &mut key,
        )
    };
    if opened == ERROR_SUCCESS {
        // SAFETY: `key` was opened above and is closed once.
        let _ = unsafe { RegCloseKey(key) };
        true
    } else {
        false
    }
}

/// The names of the subkeys directly below `HKCU\<subkey>` (read-only); a missing key has
/// none.
pub(crate) fn subkey_names(subkey: &str) -> io::Result<Vec<String>> {
    let mut key = HKEY::default();
    // SAFETY: the name outlives the call; `key` is a valid out-pointer, closed below on success.
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(subkey),
            None,
            KEY_READ,
            &mut key,
        )
    };
    if opened == ERROR_FILE_NOT_FOUND {
        return Ok(Vec::new());
    }
    check(opened)?;
    let mut names = Vec::new();
    let mut result = Ok(());
    for index in 0u32.. {
        // A key name is at most 255 characters.
        let mut buf = [0u16; 256];
        let mut len = buf.len() as u32;
        // SAFETY: `key` is open; `buf` holds `len` UTF-16 units and outlives the call.
        let e = unsafe {
            RegEnumKeyExW(
                key,
                index,
                Some(PWSTR(buf.as_mut_ptr())),
                &mut len,
                None,
                None,
                None,
                None,
            )
        };
        if e == ERROR_NO_MORE_ITEMS {
            break;
        }
        if let Err(err) = check(e) {
            result = Err(err);
            break;
        }
        names.push(String::from_utf16_lossy(&buf[..len as usize]));
    }
    // SAFETY: `key` was opened above and is closed once.
    let _ = unsafe { RegCloseKey(key) };
    result.map(|()| names)
}

/// Refuses every channel but a test channel, before anything is read or written: the remove
/// helpers exist for the desktop group's cleanup and never touch a stable or dev registration.
pub(crate) fn require_test_channel(env: &Env, what: &str) -> io::Result<()> {
    if matches!(env.channel(), Channel::Test(_)) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{what} is done only on a test channel"),
        ))
    }
}

/// The value every registry key a test channel's registration writes carries: the test
/// channel's id (Appendix G, plan 4a ruling 2026-10-24). A killed run's keys are found by it
/// ([`marked_ids`]), even when the names that would lead to them are gone.
pub const TEST_MARKER: &str = "LilTestChannel";

/// Writes [`TEST_MARKER`] into `HKCU\<subkey>` on a test channel; nothing on stable or dev.
pub(crate) fn mark(env: &Env, subkey: &str) -> io::Result<()> {
    match env.channel() {
        Channel::Test(id) => set_string(subkey, Some(TEST_MARKER), id),
        _ => Ok(()),
    }
}

/// The [`TEST_MARKER`] values of the subkeys directly below `HKCU\<subkey>` (read-only).
pub(crate) fn marked_ids(subkey: &str) -> io::Result<Vec<String>> {
    Ok(subkey_names(subkey)?
        .into_iter()
        .filter_map(|name| get_string(&format!(r"{subkey}\{name}"), Some(TEST_MARKER)))
        .collect())
}

/// `"<exe>"`: the exe as the registry values quote it. An exe path that isn't valid Unicode
/// (it would be written lossily) or holds a `"` (it can't be quoted) is refused.
pub(crate) fn quoted(exe: &Path) -> io::Result<String> {
    let exe = exe.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the exe path isn't valid Unicode",
        )
    })?;
    if exe.contains('"') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the exe path contains a quote",
        ));
    }
    Ok(format!("\"{exe}\""))
}

/// `LILITH.lilref.<channel>`: the channel's `.lilref` file type.
fn lilref_progid(env: &Env) -> String {
    format!("{}.lilref.{}", names::APP_NAME, env.channel())
}

/// `<root>\<scheme>` and `<root>\<progid>` under `HKEY_CURRENT_USER`, and `<root>` itself.
struct Keys {
    root: String,
    scheme: String,
    progid: String,
}

fn keys(env: &Env) -> io::Result<Keys> {
    let root = hkcu_subkey(&env.classes_root())?;
    Ok(Keys {
        scheme: format!(r"{root}\{}", env.activation_scheme()),
        progid: format!(r"{root}\{}", lilref_progid(env)),
        root,
    })
}

/// Writes the channel's scheme and `.lilref` handlers for `exe` (P4a-R26: only after your own
/// input). A `.lilref` association another channel held is replaced. A desktop-group test
/// channel on the shell's classes root ([`Env::with_shell_classes_root`]) writes its scheme
/// only: never the `.lilref` association or file type, so your own association never moves.
pub fn register_handlers(env: &Env, exe: &Path) -> io::Result<()> {
    let exe = quoted(exe)?;
    let command = format!("{exe} \"%1\"");
    let k = keys(env)?;
    set_string(
        &k.scheme,
        None,
        &format!("URL:{} ({})", names::APP_NAME, env.channel().capitalised()),
    )?;
    set_string(&k.scheme, Some("URL Protocol"), "")?;
    set_string(
        &format!(r"{}\DefaultIcon", k.scheme),
        None,
        &format!("{exe},0"),
    )?;
    set_string(&format!(r"{}\shell\open\command", k.scheme), None, &command)?;
    mark(env, &k.scheme)?;
    if env.uses_shell_classes_root() {
        return Ok(());
    }
    let association = format!(r"{}\.lilref", k.root);
    set_string(&association, None, &lilref_progid(env))?;
    mark(env, &association)?;
    set_string(&format!(r"{}\shell\open\command", k.progid), None, &command)?;
    mark(env, &k.progid)
}

/// Removes a test channel's scheme and `.lilref` file type, and the `.lilref` association while
/// it still names this channel's type: exactly the names [`register_handlers`] writes. Missing
/// keys are no error. Refused, before anything is read or written, on a stable or dev channel.
pub fn unregister_handlers(env: &Env) -> io::Result<()> {
    require_test_channel(env, "removing the handlers")?;
    let k = keys(env)?;
    delete_tree(&k.scheme)?;
    delete_tree(&k.progid)?;
    let association = format!(r"{}\.lilref", k.root);
    if get_string(&association, None).as_deref() == Some(lilref_progid(env).as_str()) {
        delete_tree(&association)?;
    }
    Ok(())
}

/// Removes a test channel's whole registry root, `HKCU\Software\LILITH-test-<id>` (its classes
/// root among it): the desktop group's cleanup. Refused for any other channel.
pub fn remove_test_registry(env: &Env) -> io::Result<()> {
    require_test_channel(env, "removing the registry root")?;
    delete_tree(&hkcu_subkey(&env.registry_root())?)
}

/// What a test channel's registrations left behind (read-only; the desktop group's check that
/// its cleanup removed everything, P4a-R39): each of its scheme key, `.lilref` file type, toast
/// CLSID key, AUMID registration and registry root that still exists, and the `.lilref` association while it names
/// the channel's type, as `HKCU\…` names. Refused on a stable or dev channel.
pub fn leftover_registrations(env: &Env) -> io::Result<Vec<String>> {
    require_test_channel(env, "checking for leftover registrations")?;
    let k = keys(env)?;
    let clsid = format!(r"{}\CLSID\{}", k.root, env.toast_activator_clsid());
    let aumid = format!(r"{}\AppUserModelId\{}", k.root, env.notification_aumid());
    let association = format!(r"{}\.lilref", k.root);
    let mut left: Vec<String> = [
        k.scheme,
        k.progid,
        clsid,
        aumid,
        hkcu_subkey(&env.registry_root())?,
    ]
    .into_iter()
    .filter(|key| key_exists(key))
    .collect();
    if get_string(&association, None).as_deref() == Some(lilref_progid(env).as_str()) {
        left.push(association);
    }
    Ok(left.into_iter().map(|key| format!(r"HKCU\{key}")).collect())
}

/// The handlers' state for `exe`: `OtherChannel` when a command runs another exe, else
/// `Missing` when a key, value or the expected command is missing, else `Ok`. A desktop-group
/// test channel on the shell's classes root has only its scheme to check.
pub(crate) fn state(env: &Env, exe: &Path) -> HandlerState {
    let Ok(k) = keys(env) else {
        return HandlerState::Missing;
    };
    let scheme = get_string(&k.scheme, Some("URL Protocol"))
        .and_then(|_| get_string(&format!(r"{}\shell\open\command", k.scheme), None));
    let mut commands = vec![scheme];
    if !env.uses_shell_classes_root() {
        commands.push(
            get_string(&format!(r"{}\.lilref", k.root), None).and_then(|progid| {
                get_string(&format!(r"{}\{progid}\shell\open\command", k.root), None)
            }),
        );
    }
    let mut missing = false;
    for command in commands {
        match command {
            None => missing = true,
            Some(command) => {
                let (named, rest) = split_first_argument(&command);
                if !same_path(Path::new(&named), exe) {
                    return HandlerState::OtherChannel(PathBuf::from(named));
                }
                missing |= rest != "\"%1\"";
            }
        }
    }
    if missing {
        HandlerState::Missing
    } else {
        HandlerState::Ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: APP-022
    fn only_per_user_keys_are_written() {
        assert_eq!(
            hkcu_subkey(r"HKCU\Software\Classes").unwrap(),
            r"Software\Classes"
        );
        assert_eq!(
            hkcu_subkey(r"hkcu\Software\LILITH-test-a\Classes").unwrap(),
            r"Software\LILITH-test-a\Classes"
        );
        for bad in [
            r"HKLM\Software\Classes",
            r"HKCU\",
            "HKCU",
            r"HKEY_LOCAL_MACHINE\x",
            "",
        ] {
            assert!(hkcu_subkey(bad).is_err(), "{bad}");
        }
        assert_eq!(
            quoted(Path::new(r"C:\a b\lil-image.exe")).unwrap(),
            r#""C:\a b\lil-image.exe""#
        );
        assert!(quoted(Path::new(r#"C:\a"b.exe"#)).is_err());
        // An unpaired surrogate would be written as U+FFFD: refused instead.
        use std::os::windows::ffi::OsStringExt as _;
        let mut wide: Vec<u16> = r"C:\a".encode_utf16().collect();
        wide.push(0xD800);
        let lossy = PathBuf::from(std::ffi::OsString::from_wide(&wide));
        let err = quoted(&lossy).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
    }
}
