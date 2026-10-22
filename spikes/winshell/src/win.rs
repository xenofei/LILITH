//! Thin Windows wrappers shared by the binaries. Nothing here runs at load time.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use windows::Data::Xml::Dom::XmlDocument;
use windows::UI::Notifications::{
    NotificationSetting, ToastNotification, ToastNotificationManager, ToastNotifier,
};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_ELEVATION,
    TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenElevation, TokenIntegrityLevel,
};
use windows::Win32::Storage::EnhancedStorage::{
    PKEY_AppUserModel_ID, PKEY_AppUserModel_ToastActivatorCLSID,
};
use windows::Win32::System::Com::StructuredStorage::{
    InitPropVariantFromCLSID, PROPVARIANT, PropVariantToGUID,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, IPersistFile, STGM_READ,
};
use windows::Win32::System::Environment::GetCommandLineW;
use windows::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CreateProcessW, DETACHED_PROCESS, GetCurrentProcess, OpenProcess,
    OpenProcessToken, PROCESS_INFORMATION, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW, STARTUPINFOW,
};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{
    FOLDERID_Programs, IShellLinkW, KF_FLAG_DEFAULT, SHGetKnownFolderPath, SHStrDupW,
    ShellExecuteW, ShellLink,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GUITHREADINFO, GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, SW_SHOWNORMAL,
};
use windows::core::{BSTR, GUID, HSTRING, Interface, PCWSTR, PWSTR, Result, w};

use crate::AUMID;

/// The spike's own folder for the activation log and flags:
/// `%LOCALAPPDATA%\LILITH-spike\winshell`, or `WINSHELL_LOG_DIR` when set. Created on use.
pub fn log_dir() -> PathBuf {
    let dir = std::env::var_os("WINSHELL_LOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let base = std::env::var_os("LOCALAPPDATA").unwrap_or_else(|| ".".into());
            PathBuf::from(base).join("LILITH-spike").join("winshell")
        });
    let _ = fs::create_dir_all(&dir);
    dir
}

pub fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Appends one JSON line `{t_ms, pid, kind, ...fields}` to `<log_dir>\events.jsonl`.
pub fn log_event(kind: &str, fields: Value) {
    let mut line = json!({ "t_ms": now_ms() as u64, "pid": std::process::id(), "kind": kind });
    if let (Some(l), Value::Object(f)) = (line.as_object_mut(), fields) {
        l.extend(f);
    }
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir().join("events.jsonl"))
    {
        let _ = f.write_all(format!("{line}\n").as_bytes());
    }
}

/// Writes `value` to `path` (pretty JSON), replacing it.
pub fn write_json(path: &Path, value: &Value) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(
        path,
        serde_json::to_string_pretty(value).unwrap_or_default(),
    );
}

/// The raw command line, as `GetCommandLineW` returns it.
pub fn raw_command_line() -> String {
    // SAFETY: GetCommandLineW returns a pointer to the process's NUL-terminated command line,
    // valid for the life of the process.
    unsafe { GetCommandLineW().to_string().unwrap_or_default() }
}

/// `RoInitialize(RO_INIT_MULTITHREADED)` for the WinRT calls; a second call is harmless.
pub fn init_winrt() {
    // SAFETY: plain initialisation call; failure (already initialised) is ignored.
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
}

pub fn notifier() -> Result<ToastNotifier> {
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))
}

pub fn setting_name(s: NotificationSetting) -> &'static str {
    match s {
        NotificationSetting::Enabled => "Enabled",
        NotificationSetting::DisabledForApplication => "DisabledForApplication",
        NotificationSetting::DisabledForUser => "DisabledForUser",
        NotificationSetting::DisabledByGroupPolicy => "DisabledByGroupPolicy",
        NotificationSetting::DisabledByManifest => "DisabledByManifest",
        _ => "Unknown",
    }
}

/// `ToastNotifier::Setting()` for the spike's AUMID, as a name, or the error.
pub fn notifier_setting() -> String {
    match notifier().and_then(|n| n.Setting()) {
        Ok(s) => setting_name(s).to_string(),
        Err(e) => format!("error: {e}"),
    }
}

/// A toast from its XML, with Tag, Group and SuppressPopup set.
pub fn make_toast(xml: &str, tag: &str, group: &str, suppress: bool) -> Result<ToastNotification> {
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(xml))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    toast.SetTag(&HSTRING::from(tag))?;
    toast.SetGroup(&HSTRING::from(group))?;
    toast.SetSuppressPopup(suppress)?;
    Ok(toast)
}

/// The Tags of the spike's toasts in the notification centre.
pub fn history_tags() -> Result<Vec<String>> {
    let list = ToastNotificationManager::History()?.GetHistoryWithId(&HSTRING::from(AUMID))?;
    let mut tags = Vec::new();
    for i in 0..list.Size()? {
        tags.push(list.GetAt(i)?.Tag()?.to_string());
    }
    Ok(tags)
}

/// Removes every spike toast from the notification centre.
pub fn history_clear() -> Result<()> {
    ToastNotificationManager::History()?.ClearWithId(&HSTRING::from(AUMID))
}

/// `FOLDERID_Programs`: the per-user Start-menu Programs folder.
pub fn programs_dir() -> Result<PathBuf> {
    // SAFETY: SHGetKnownFolderPath returns a CoTaskMem string we copy and then free.
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Programs, KF_FLAG_DEFAULT, None)?;
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        Ok(PathBuf::from(s))
    }
}

fn init_sta() {
    // SAFETY: plain initialisation call; S_FALSE / RPC_E_CHANGED_MODE are fine for a spike.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
}

/// Writes a shortcut at `lnk` to `target` whose `System.AppUserModel.ID` is `aumid`.
/// With `toast_clsid`, the shortcut also carries `System.AppUserModel.ToastActivatorCLSID`
/// (Part B run 1 analysis: whether a missed toast stays in the notification centre).
pub fn write_shortcut(
    lnk: &Path,
    target: &Path,
    aumid: &str,
    toast_clsid: Option<GUID>,
) -> Result<()> {
    init_sta();
    // SAFETY: COM calls on interfaces we own; the PROPVARIANT owns its CoTaskMem string and
    // frees it on drop (PropVariantClear).
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(target.as_os_str()))?;
        if let Some(dir) = target.parent() {
            link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
        }
        let store: IPropertyStore = link.cast()?;
        let mut value = PROPVARIANT::default();
        let text = SHStrDupW(&HSTRING::from(aumid))?;
        (*value.Anonymous.Anonymous).vt = VT_LPWSTR;
        (*value.Anonymous.Anonymous).Anonymous.pwszVal = text;
        store.SetValue(&PKEY_AppUserModel_ID, &value)?;
        if let Some(clsid) = toast_clsid {
            let v = InitPropVariantFromCLSID(&clsid)?;
            store.SetValue(&PKEY_AppUserModel_ToastActivatorCLSID, &v)?;
        }
        store.Commit()?;
        let file: IPersistFile = link.cast()?;
        file.Save(&HSTRING::from(lnk.as_os_str()), true)?;
    }
    Ok(())
}

/// Reads a shortcut back: `(target, AUMID, ToastActivatorCLSID if set)`.
pub fn read_shortcut(lnk: &Path) -> Result<(String, String, Option<String>)> {
    init_sta();
    // SAFETY: COM calls on interfaces we own; buffers are sized as the calls require.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        let file: IPersistFile = link.cast()?;
        file.Load(&HSTRING::from(lnk.as_os_str()), STGM_READ)?;
        let mut buf = [0u16; 1024];
        link.GetPath(&mut buf, std::ptr::null_mut(), 0)?;
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let target = String::from_utf16_lossy(&buf[..len]);
        let store: IPropertyStore = link.cast()?;
        let value = store.GetValue(&PKEY_AppUserModel_ID)?;
        let aumid = BSTR::try_from(&value)
            .map(|b| b.to_string())
            .unwrap_or_default();
        let clsid = store
            .GetValue(&PKEY_AppUserModel_ToastActivatorCLSID)
            .ok()
            .and_then(|v| PropVariantToGUID(&v).ok())
            .map(|g| format!("{{{g:?}}}"));
        Ok((target, aumid, clsid))
    }
}

/// Starts `exe args` with `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`, as `lil-client`
/// starts the core; returns the pid.
///
/// `CreateProcessW` is called directly with `bInheritHandles = FALSE`. `std::process::Command`
/// passes TRUE, and then the child inherits every inheritable handle of this process,
/// including a stdout pipe its caller reads to EOF: the caller would wait for the child to
/// exit (fix round 1, C1; `tests/spawn_detached.rs`).
pub fn spawn_detached(exe: &str, args: &[String]) -> std::io::Result<u32> {
    let mut line: Vec<u16> = crate::text::command_line(exe, args)
        .encode_utf16()
        .chain([0])
        .collect();
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: `line` is NUL-terminated and mutable as CreateProcessW requires; `startup` and
    // `info` are valid for the call; both returned handles are closed here.
    unsafe {
        CreateProcessW(
            PCWSTR::null(),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            false,
            DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP,
            None,
            PCWSTR::null(),
            &startup,
            &mut info,
        )
        .map_err(|e| std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF))?;
        let _ = CloseHandle(info.hThread);
        let _ = CloseHandle(info.hProcess);
    }
    Ok(info.dwProcessId)
}

/// `ShellExecuteW(NULL, "open", url, ...)`; the return value (> 32 is success).
pub fn shell_execute(url: &str) -> isize {
    // SAFETY: every string is NUL-terminated and outlives the call.
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(url),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
        .0 as isize
    }
}

/// The full image path of a process, or `None`.
pub fn process_image(pid: u32) -> Option<PathBuf> {
    // SAFETY: the handle is ours and closed; the buffer length is passed in and out.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 32768];
        let mut len = buf.len() as u32;
        let ok =
            QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        ok.ok()?;
        Some(PathBuf::from(OsString::from_wide(&buf[..len as usize])))
    }
}

/// `{elevated, integrity_rid, integrity}` of this process's token.
pub fn token_info() -> Value {
    // SAFETY: the token handle is ours and closed; each buffer is sized by the first call.
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return json!({ "error": "OpenProcessToken failed" });
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut ret = 0u32;
        let elevated = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        )
        .ok()
        .map(|_| elevation.TokenIsElevated != 0);
        let mut needed = 0u32;
        let _ = GetTokenInformation(token, TokenIntegrityLevel, None, 0, &mut needed);
        let mut buf = vec![0u8; needed as usize];
        let rid = if needed > 0
            && GetTokenInformation(
                token,
                TokenIntegrityLevel,
                Some(buf.as_mut_ptr() as *mut _),
                needed,
                &mut needed,
            )
            .is_ok()
        {
            let label = &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL);
            let count = *GetSidSubAuthorityCount(label.Label.Sid);
            Some(*GetSidSubAuthority(label.Label.Sid, u32::from(count) - 1))
        } else {
            None
        };
        let _ = CloseHandle(token);
        let name = match rid {
            Some(0x0000) => "untrusted",
            Some(0x1000) => "low",
            Some(0x2000) => "medium",
            Some(0x2100) => "medium_plus",
            Some(0x3000) => "high",
            Some(0x4000) => "system",
            Some(_) => "other",
            None => "unknown",
        };
        json!({ "elevated": elevated, "integrity_rid": rid, "integrity": name })
    }
}

/// `{fg_hwnd, fg_pid, focus_hwnd}`: the foreground window, its process and the window that
/// has keyboard focus in the foreground thread.
pub fn foreground_info() -> Value {
    // SAFETY: plain queries; GUITHREADINFO is sized before the call.
    unsafe {
        let fg = GetForegroundWindow();
        let mut pid = 0u32;
        let tid = GetWindowThreadProcessId(fg, Some(&mut pid));
        let mut gui = GUITHREADINFO {
            cbSize: size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        let focus = if tid != 0 && GetGUIThreadInfo(tid, &mut gui).is_ok() {
            gui.hwndFocus
        } else {
            HWND::default()
        };
        json!({ "fg_hwnd": fg.0 as isize, "fg_pid": pid, "focus_hwnd": focus.0 as isize })
    }
}
