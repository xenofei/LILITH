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
    ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, FILETIME, WIN32_ERROR,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteKeyW, RegDeleteTreeW, RegEnumKeyExW,
    RegGetValueW, RegOpenKeyExW, RegQueryInfoKeyW, RegSetValueExW,
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
    apply(&handler_writes(env, exe)?)
}

/// Writes only the channel's scheme for `exe` (P4a-R26: only after your own input), leaving the
/// `.lilref` association as it is: for a scheme that is missing while another channel holds
/// `.lilref` (plan 4a ruling, 2026-10-25).
pub fn register_scheme(env: &Env, exe: &Path) -> io::Result<()> {
    apply(&scheme_writes(env, exe)?)
}

/// Writes only the channel's `.lilref` association and file type for `exe` (P4a-R26), leaving
/// the scheme as it is. A desktop-group channel on the shell's classes root writes nothing.
pub fn register_lilref(env: &Env, exe: &Path) -> io::Result<()> {
    apply(&lilref_writes(env, exe)?)
}

/// What [`register_handlers`] writes, in order: [`scheme_writes`], then [`lilref_writes`]. On a
/// test channel each key's marker comes first, so a run killed between two writes leaves a key
/// the sweep finds.
pub(crate) fn handler_writes(env: &Env, exe: &Path) -> io::Result<Vec<KeyWrite>> {
    let mut w = scheme_writes(env, exe)?;
    w.extend(lilref_writes(env, exe)?);
    Ok(w)
}

/// The scheme's writes, in order.
pub(crate) fn scheme_writes(env: &Env, exe: &Path) -> io::Result<Vec<KeyWrite>> {
    let exe = quoted(exe)?;
    let command = format!("{exe} \"%1\"");
    let k = keys(env)?;
    let mut w = Vec::new();
    marker(env, &k.scheme, &mut w);
    w.push(KeyWrite::new(
        &k.scheme,
        None,
        &format!("URL:{} ({})", names::APP_NAME, env.channel().capitalised()),
    ));
    w.push(KeyWrite::new(&k.scheme, Some("URL Protocol"), ""));
    w.push(KeyWrite::new(
        &format!(r"{}\DefaultIcon", k.scheme),
        None,
        &format!("{exe},0"),
    ));
    w.push(KeyWrite::new(
        &format!(r"{}\shell\open\command", k.scheme),
        None,
        &command,
    ));
    Ok(w)
}

/// The `.lilref` association's and file type's writes, in order; none on the shell's classes
/// root of a desktop-group channel.
pub(crate) fn lilref_writes(env: &Env, exe: &Path) -> io::Result<Vec<KeyWrite>> {
    let mut w = Vec::new();
    if env.uses_shell_classes_root() {
        return Ok(w);
    }
    let command = format!("{} \"%1\"", quoted(exe)?);
    let k = keys(env)?;
    let association = format!(r"{}\.lilref", k.root);
    marker(env, &association, &mut w);
    w.push(KeyWrite::new(&association, None, &lilref_progid(env)));
    marker(env, &k.progid, &mut w);
    w.push(KeyWrite::new(
        &format!(r"{}\shell\open\command", k.progid),
        None,
        &command,
    ));
    Ok(w)
}

/// One string value written under `HKCU\<key>` (`name` `None`: the key's default).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeyWrite {
    pub key: String,
    pub name: Option<String>,
    pub value: String,
}

impl KeyWrite {
    pub(crate) fn new(key: &str, name: Option<&str>, value: &str) -> KeyWrite {
        KeyWrite {
            key: key.to_string(),
            name: name.map(str::to_string),
            value: value.to_string(),
        }
    }
}

/// [`TEST_MARKER`] for `key` on a test channel; nothing on stable or dev.
pub(crate) fn marker(env: &Env, key: &str, w: &mut Vec<KeyWrite>) {
    if let Channel::Test(id) = env.channel() {
        w.push(KeyWrite::new(key, Some(TEST_MARKER), id));
    }
}

/// Writes `writes` in order.
pub(crate) fn apply(writes: &[KeyWrite]) -> io::Result<()> {
    for w in writes {
        set_string(&w.key, w.name.as_deref(), &w.value)?;
    }
    Ok(())
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

/// The value a `TestEnv`'s registry root carries naming the process that owns it,
/// `<pid>:<process creation FILETIME>` ([`mark_test_registry_owner`]).
pub const TEST_OWNER: &str = "LilTestOwner";

/// A test channel's registry root as [`test_registry_roots`] lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestRegistryRoot {
    /// The `<8 lowercase hex>` of `LILITH-test-<id>`.
    pub id: String,
    /// Its [`TEST_OWNER`] value, when it has one.
    pub owner: Option<String>,
    /// When the key was last written (`RegQueryInfoKeyW`'s last write time).
    pub last_written: std::time::SystemTime,
}

/// The test channels' registry roots directly below `HKCU\Software` that have `TestEnv`'s
/// shape, `LILITH-test-<8 lowercase hex>` (read-only): what a killed test can leave behind
/// (Task 3's review M9). Every other name, a desktop-group root among them, is left out. A key
/// that vanishes while it is read is left out too.
pub fn test_registry_roots() -> io::Result<Vec<TestRegistryRoot>> {
    let head = format!("{}-test-", names::APP_NAME);
    Ok(subkey_names("Software")?
        .into_iter()
        .filter_map(|name| {
            name.get(..head.len())
                .filter(|h| h.eq_ignore_ascii_case(&head))?;
            let id = &name[head.len()..];
            if !(id.len() == 8 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))) {
                return None;
            }
            let key = format!(r"Software\{name}");
            Some(TestRegistryRoot {
                id: id.to_string(),
                owner: get_string(&key, Some(TEST_OWNER)),
                last_written: last_written(&key).ok()?,
            })
        })
        .collect())
}

/// Writes `owner` as a test channel's registry root's [`TEST_OWNER`], creating the root. Refused
/// on a stable or dev channel.
pub fn mark_test_registry_owner(env: &Env, owner: &str) -> io::Result<()> {
    require_test_channel(env, "marking the registry root's owner")?;
    set_string(&hkcu_subkey(&env.registry_root())?, Some(TEST_OWNER), owner)
}

/// When `HKCU\<subkey>` was last written.
fn last_written(subkey: &str) -> io::Result<std::time::SystemTime> {
    let mut key = HKEY::default();
    // SAFETY: the name outlives the call; `key` is a valid out-pointer, closed below on success.
    check(unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(subkey),
            None,
            KEY_READ,
            &mut key,
        )
    })?;
    let mut written = FILETIME::default();
    // SAFETY: `key` is open; only the last-write-time out-pointer is asked for.
    let queried = check(unsafe {
        RegQueryInfoKeyW(
            key,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(&mut written),
        )
    });
    // SAFETY: `key` was opened above and is closed once.
    let _ = unsafe { RegCloseKey(key) };
    queried?;
    // FILETIME counts 100 ns ticks from 1601; the Unix epoch is 11,644,473,600 s later.
    let ticks = (u64::from(written.dwHighDateTime) << 32) | u64::from(written.dwLowDateTime);
    let unix = std::time::Duration::from_nanos(ticks.saturating_mul(100))
        .saturating_sub(std::time::Duration::from_secs(11_644_473_600));
    Ok(std::time::UNIX_EPOCH + unix)
}

/// `HKCU`'s subkey of Windows' own per-app notification settings for `env`'s AUMID, which
/// Windows creates on the AUMID's first toast (`Setting()` answers `ERROR_NOT_FOUND` until then).
pub(crate) const NOTIFICATION_SETTINGS: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Notifications\Settings";

fn notification_settings_key(env: &Env) -> String {
    format!(r"{NOTIFICATION_SETTINGS}\{}", env.notification_aumid())
}

/// Removes Windows' notification settings entry for a test channel's AUMID,
/// `HKCU\Software\Microsoft\Windows\CurrentVersion\Notifications\Settings\<AUMID>`, by its exact
/// name: the desktop group's toast tests make Windows create it. A missing key is no error.
/// Refused, before anything is read or written, on a stable or dev channel.
pub fn remove_notification_settings(env: &Env) -> io::Result<()> {
    require_test_channel(env, "removing the notification settings")?;
    delete_tree(&notification_settings_key(env))
}

/// What a test channel's registrations left behind (read-only; the desktop group's check that
/// its cleanup removed everything, P4a-R39): each of its scheme key, `.lilref` file type, toast
/// CLSID key, AUMID registration, Windows' notification settings entry for the AUMID and
/// registry root that still exists, and the `.lilref` association while it names
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
        notification_settings_key(env),
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

/// The app every LILITH channel registers its handlers for.
const APP_EXE: &str = "lil-image.exe";

/// One of the channel's two handlers: its scheme, or the `.lilref` association (plan 4a ruling,
/// 2026-10-25: they are checked, reported and written apart).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerPart {
    /// It opens `exe`, completely.
    Ok,
    /// Not registered, or registered for `exe` incompletely, or a dead association to another
    /// channel's file type: this channel may write it.
    Missing,
    /// Its command runs another `lil-image.exe`: another LILITH channel or build holds it.
    OtherBuild(PathBuf),
    /// Another program holds it: its command's program, or the file type `.lilref` names.
    Foreign(String),
}

/// The channel's scheme and `.lilref` association, each on its own ([`handler_parts`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerParts {
    pub scheme: HandlerPart,
    pub lilref: HandlerPart,
}

impl HandlerParts {
    /// The one state `notification_path.handlers` shows: a missing own scheme is `Missing`
    /// whoever holds `.lilref` (plan 4a ruling, 2026-10-25); a scheme held elsewhere is
    /// `OtherChannel`; with the scheme in place, `.lilref` decides.
    pub fn state(&self) -> HandlerState {
        let held = |part: &HandlerPart| match part {
            HandlerPart::OtherBuild(exe) => Some(exe.clone()),
            HandlerPart::Foreign(holder) => Some(PathBuf::from(holder)),
            HandlerPart::Ok | HandlerPart::Missing => None,
        };
        match (&self.scheme, &self.lilref) {
            (HandlerPart::Missing, _) | (HandlerPart::Ok, HandlerPart::Missing) => {
                HandlerState::Missing
            }
            (HandlerPart::Ok, HandlerPart::Ok) => HandlerState::Ok,
            (HandlerPart::Ok, lilref) => {
                HandlerState::OtherChannel(held(lilref).unwrap_or_default())
            }
            (scheme, _) => HandlerState::OtherChannel(held(scheme).unwrap_or_default()),
        }
    }
}

/// The scheme's and the `.lilref` association's states for `exe`, read from the registry
/// (read-only). A desktop-group test channel on the shell's classes root has only its scheme:
/// its `.lilref` is `Ok`.
pub fn handler_parts(env: &Env, exe: &Path) -> HandlerParts {
    parts_with(env, exe, get_string)
}

/// [`handler_parts`] over `get`, which reads a string value as [`get_string`] does.
pub(crate) fn parts_with(
    env: &Env,
    exe: &Path,
    get: impl Fn(&str, Option<&str>) -> Option<String>,
) -> HandlerParts {
    let Ok(k) = keys(env) else {
        return HandlerParts {
            scheme: HandlerPart::Missing,
            lilref: HandlerPart::Missing,
        };
    };
    // What a command runs: `exe` (complete only with the protocol value and `"%1"`), another
    // build of the app, or another program.
    let command_part = |command: Option<String>, complete: bool| -> HandlerPart {
        let Some(command) = command else {
            return HandlerPart::Missing;
        };
        let (named, rest) = split_first_argument(&command);
        let named = PathBuf::from(named);
        if same_path(&named, exe) {
            if complete && rest == "\"%1\"" {
                HandlerPart::Ok
            } else {
                HandlerPart::Missing
            }
        } else if named
            .file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(APP_EXE))
        {
            HandlerPart::OtherBuild(named)
        } else {
            HandlerPart::Foreign(named.display().to_string())
        }
    };
    let scheme = command_part(
        get(&format!(r"{}\shell\open\command", k.scheme), None),
        get(&k.scheme, Some("URL Protocol")).is_some(),
    );
    let lilref = if env.uses_shell_classes_root() {
        HandlerPart::Ok
    } else {
        let ours = lilref_progid(env);
        let family = format!("{}.lilref.", names::APP_NAME);
        match get(&format!(r"{}\.lilref", k.root), None) {
            None => HandlerPart::Missing,
            Some(progid) => {
                let command = get(&format!(r"{}\{progid}\shell\open\command", k.root), None);
                if progid.eq_ignore_ascii_case(&ours) {
                    command_part(command, true)
                } else if progid
                    .get(..family.len())
                    .is_some_and(|head| head.eq_ignore_ascii_case(&family))
                {
                    // Another channel's file type: kept while it opens something; a dead one,
                    // or one that already opens `exe`, is this channel's to write.
                    match command_part(command, true) {
                        HandlerPart::Ok => HandlerPart::Missing,
                        other => other,
                    }
                } else {
                    HandlerPart::Foreign(progid)
                }
            }
        }
    };
    HandlerParts { scheme, lilref }
}

/// The handlers' one state for `exe` ([`HandlerParts::state`]).
pub(crate) fn state(env: &Env, exe: &Path) -> HandlerState {
    handler_parts(env, exe).state()
}

/// Each key `writes` marks is marked before any other value is written under it or below it
/// (review M4): `Err` names the first write that comes before its key's marker.
#[cfg(test)]
pub(crate) fn markers_come_first(writes: &[KeyWrite]) -> Result<(), String> {
    let marked: Vec<&str> = writes
        .iter()
        .filter(|w| w.name.as_deref() == Some(TEST_MARKER))
        .map(|w| w.key.as_str())
        .collect();
    let mut seen: Vec<&str> = Vec::new();
    for w in writes {
        if w.name.as_deref() == Some(TEST_MARKER) {
            seen.push(&w.key);
            continue;
        }
        let owner = marked
            .iter()
            .filter(|k| w.key == **k || w.key.starts_with(&format!(r"{k}\")))
            .max_by_key(|k| k.len());
        match owner {
            Some(k) if !seen.contains(k) => return Err(format!("{w:?} before {k}'s marker")),
            None => return Err(format!("{w:?} is under no marked key")),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(channel: Channel) -> Env {
        Env::with_roots(
            channel,
            crate::env::Roots {
                roaming: PathBuf::from(r"C:\T\roaming"),
                local: PathBuf::from(r"C:\T\local"),
                user_profile: PathBuf::from(r"C:\T\profile"),
            },
            "S-1".into(),
        )
    }

    #[test] // spec: APP-022, QA-042
    fn every_handler_key_is_marked_before_its_values() {
        let exe = Path::new(r"C:\a\lil-image.exe");
        let own = env(Channel::Test("desktop-0123456789abcdef".into()));
        for e in [own.clone(), own.with_shell_classes_root()] {
            let writes = handler_writes(&e, exe).unwrap();
            markers_come_first(&writes).unwrap();
            assert!(
                writes
                    .iter()
                    .any(|w| w.name.as_deref() == Some(TEST_MARKER))
            );
        }
        // Stable and dev carry no marker.
        for c in [Channel::Stable, Channel::Dev] {
            let writes = handler_writes(&env(c), exe).unwrap();
            assert!(
                writes
                    .iter()
                    .all(|w| w.name.as_deref() != Some(TEST_MARKER))
            );
        }
    }

    /// A registry of `(key, value name) → string` for [`parts_with`].
    type Fake = std::collections::HashMap<(String, Option<String>), String>;

    fn parts(env: &Env, exe: &Path, fake: &Fake) -> HandlerParts {
        parts_with(env, exe, |key, name| {
            fake.get(&(key.to_string(), name.map(str::to_string)))
                .cloned()
        })
    }

    fn put(fake: &mut Fake, key: &str, name: Option<&str>, value: &str) {
        fake.insert(
            (key.to_string(), name.map(str::to_string)),
            value.to_string(),
        );
    }

    #[test] // spec: APP-020, APP-021
    fn the_scheme_and_lilref_are_read_apart() {
        let own = env(Channel::Test("0123abcd".into()));
        let exe = Path::new(r"C:\own\lil-image.exe");
        let other = r"C:\stable\lil-image.exe";
        let k = keys(&own).unwrap();
        let mut fake = Fake::new();
        assert_eq!(
            parts(&own, exe, &fake),
            HandlerParts {
                scheme: HandlerPart::Missing,
                lilref: HandlerPart::Missing
            }
        );

        // Another channel holds `.lilref`; this channel's scheme is missing: `Missing`, so the
        // scheme is still offered (plan 4a ruling, 2026-10-25).
        put(
            &mut fake,
            &format!(r"{}\.lilref", k.root),
            None,
            "LILITH.lilref.stable",
        );
        put(
            &mut fake,
            &format!(r"{}\LILITH.lilref.stable\shell\open\command", k.root),
            None,
            &format!("\"{other}\" \"%1\""),
        );
        let p = parts(&own, exe, &fake);
        assert_eq!(p.scheme, HandlerPart::Missing);
        assert_eq!(p.lilref, HandlerPart::OtherBuild(PathBuf::from(other)));
        assert_eq!(p.state(), HandlerState::Missing);

        // The scheme in place: the other channel's `.lilref` decides.
        put(&mut fake, &k.scheme, Some("URL Protocol"), "");
        let command = format!(r"{}\shell\open\command", k.scheme);
        put(&mut fake, &command, None, r#""C:\own\lil-image.exe" "%1""#);
        let p = parts(&own, exe, &fake);
        assert_eq!(p.scheme, HandlerPart::Ok);
        assert_eq!(p.state(), HandlerState::OtherChannel(PathBuf::from(other)));

        // Without its protocol value the scheme is ours, incomplete: `Missing`.
        fake.remove(&(k.scheme.clone(), Some("URL Protocol".into())));
        assert_eq!(parts(&own, exe, &fake).scheme, HandlerPart::Missing);

        // Another program's command, and another program's file type, are `Foreign`.
        put(&mut fake, &command, None, r#""C:\Tools\viewer.exe" "%1""#);
        put(&mut fake, &format!(r"{}\.lilref", k.root), None, "txtfile");
        let p = parts(&own, exe, &fake);
        assert_eq!(
            p.scheme,
            HandlerPart::Foreign(r"C:\Tools\viewer.exe".into())
        );
        assert_eq!(p.lilref, HandlerPart::Foreign("txtfile".into()));

        // Another channel's file type that opens nothing is a dead association: `Missing`.
        put(
            &mut fake,
            &format!(r"{}\.lilref", k.root),
            None,
            "LILITH.lilref.dev",
        );
        assert_eq!(parts(&own, exe, &fake).lilref, HandlerPart::Missing);

        // The shell's classes root of a desktop-group channel has its scheme only.
        let shell = env(Channel::Test("desktop-0123456789abcdef".into())).with_shell_classes_root();
        assert_eq!(parts(&shell, exe, &Fake::new()).lilref, HandlerPart::Ok);
    }

    #[test] // spec: APP-020, APP-021
    fn the_scheme_and_lilref_are_written_apart() {
        let own = env(Channel::Test("0123abcd".into()));
        let exe = Path::new(r"C:\own\lil-image.exe");
        let k = keys(&own).unwrap();
        let scheme = scheme_writes(&own, exe).unwrap();
        let lilref = lilref_writes(&own, exe).unwrap();
        assert!(
            scheme.iter().all(|w| w.key.starts_with(&k.scheme)),
            "{scheme:?}"
        );
        assert!(
            lilref.iter().all(|w| !w.key.starts_with(&k.scheme)),
            "{lilref:?}"
        );
        assert!(
            lilref.iter().any(|w| w.key.ends_with(r"\.lilref")),
            "{lilref:?}"
        );
        markers_come_first(&scheme).unwrap();
        markers_come_first(&lilref).unwrap();
        let mut both = scheme;
        both.extend(lilref);
        assert_eq!(handler_writes(&own, exe).unwrap(), both);
        let shell = env(Channel::Test("desktop-0123456789abcdef".into())).with_shell_classes_root();
        assert!(lilref_writes(&shell, exe).unwrap().is_empty());
    }

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
