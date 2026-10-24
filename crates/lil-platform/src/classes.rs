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
//! `HKCU\Software\LILITH-test-<id>\Classes`, which the shell never reads.

use std::io;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyW, RegDeleteTreeW, RegGetValueW, RegSetValueExW,
};
use windows::core::{HSTRING, PCWSTR};

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

/// `"<exe>"`: the exe as the registry values quote it. An exe path with a `"` in it can't be
/// quoted and is refused.
pub(crate) fn quoted(exe: &Path) -> io::Result<String> {
    let exe = exe.to_string_lossy();
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
/// input). A `.lilref` association another channel held is replaced.
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
    set_string(&format!(r"{}\.lilref", k.root), None, &lilref_progid(env))?;
    set_string(&format!(r"{}\shell\open\command", k.progid), None, &command)
}

/// Removes the channel's scheme and `.lilref` file type, and the `.lilref` association while it
/// still names this channel's type. Missing keys are no error.
pub fn unregister_handlers(env: &Env) -> io::Result<()> {
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
    if !matches!(env.channel(), Channel::Test(_)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "only a test channel's registry root is removed",
        ));
    }
    delete_tree(&hkcu_subkey(&env.registry_root())?)
}

/// The handlers' state for `exe`: `OtherChannel` when a command runs another exe, else
/// `Missing` when a key, value or the expected command is missing, else `Ok`.
pub(crate) fn state(env: &Env, exe: &Path) -> HandlerState {
    let Ok(k) = keys(env) else {
        return HandlerState::Missing;
    };
    let scheme = get_string(&k.scheme, Some("URL Protocol"))
        .and_then(|_| get_string(&format!(r"{}\shell\open\command", k.scheme), None));
    let lilref = get_string(&format!(r"{}\.lilref", k.root), None)
        .and_then(|progid| get_string(&format!(r"{}\{progid}\shell\open\command", k.root), None));
    let mut missing = false;
    for command in [scheme, lilref] {
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
    }
}
