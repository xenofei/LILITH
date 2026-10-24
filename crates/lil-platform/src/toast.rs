//! Classic WinRT toasts under the channel's AUMID (APP-022; Appendix G "Notifications"), with
//! no Windows App SDK: `ToastNotificationManager::CreateToastNotifierWithId`, as Task 1's S1.2
//! posted them from a windowless, detached process. Every suite toast activates by protocol;
//! no COM activator runs (P4a-R37).

use windows::Data::Xml::Dom::XmlDocument;
use windows::UI::Notifications::{
    NotificationSetting, ToastNotification, ToastNotificationManager, ToastNotifier,
};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::HSTRING;

use crate::desktop::{NotifierSetting, Toast};
use crate::env::Env;

fn notifier(aumid: &str) -> windows::core::Result<ToastNotifier> {
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(aumid))
}

/// The notifier's `Setting()` for `aumid`; `None` when there is no notifier (no platform).
pub(crate) fn setting(aumid: &str) -> Option<NotifierSetting> {
    let setting = notifier(aumid).and_then(|n| n.Setting()).ok()?;
    Some(match setting {
        NotificationSetting::Enabled => NotifierSetting::Enabled,
        NotificationSetting::DisabledForUser => NotifierSetting::DisabledForUser,
        NotificationSetting::DisabledByGroupPolicy => NotifierSetting::DisabledByGroupPolicy,
        NotificationSetting::DisabledByManifest => NotifierSetting::DisabledByManifest,
        // `DisabledForApplication`, and any value a later Windows adds, is off for this app.
        _ => NotifierSetting::DisabledForApplication,
    })
}

/// Whether this process's token is elevated. A token that can't be read counts as not
/// elevated: the notifier's own setting still decides.
pub(crate) fn is_elevated() -> bool {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo-handle of this process is always valid; `token` is a valid
    // out-pointer, and the handle it receives is closed below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return false;
    }
    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0u32;
    // SAFETY: `elevation` is a TOKEN_ELEVATION and its size is passed; `token` is open.
    let read = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&raw mut elevation).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
    };
    // SAFETY: `token` was opened above and is closed once.
    let _ = unsafe { CloseHandle(token) };
    read.is_ok() && elevation.TokenIsElevated != 0
}

/// Posts `t` under `aumid`: its XML, `Tag`, `Group` and `SuppressPopup`.
pub(crate) fn post(aumid: &str, t: &Toast) -> Result<(), String> {
    let show = || -> windows::core::Result<()> {
        let doc = XmlDocument::new()?;
        doc.LoadXml(&HSTRING::from(t.xml.as_str()))?;
        let toast = ToastNotification::CreateToastNotification(&doc)?;
        toast.SetTag(&HSTRING::from(t.tag.as_str()))?;
        toast.SetGroup(&HSTRING::from(t.group.as_str()))?;
        toast.SetSuppressPopup(t.silent)?;
        notifier(aumid)?.Show(&toast)
    };
    show().map_err(|e| format!("the toast could not be posted: {e}"))
}

/// Removes the toast `tag` of `group` from the notification centre; one that isn't there is
/// no error.
pub(crate) fn remove(aumid: &str, tag: &str, group: &str) {
    if let Ok(history) = ToastNotificationManager::History() {
        let _ = history.RemoveGroupedTagWithId(
            &HSTRING::from(tag),
            &HSTRING::from(group),
            &HSTRING::from(aumid),
        );
    }
}

/// The Tags of the channel's toasts in the notification centre, from
/// `ToastNotificationManager::History` (the desktop group's check, P4a-R39: History and the
/// toast's own events, never UI Automation).
pub fn toast_history_tags(env: &Env) -> Result<Vec<String>, String> {
    let read = || -> windows::core::Result<Vec<String>> {
        let list = ToastNotificationManager::History()?
            .GetHistoryWithId(&HSTRING::from(env.notification_aumid()))?;
        let mut tags = Vec::new();
        for i in 0..list.Size()? {
            tags.push(list.GetAt(i)?.Tag()?.to_string());
        }
        Ok(tags)
    };
    read().map_err(|e| format!("the notification history could not be read: {e}"))
}

/// Removes every toast of the channel from the notification centre (the desktop group's
/// cleanup).
pub fn clear_toast_history(env: &Env) {
    if let Ok(history) = ToastNotificationManager::History() {
        let _ = history.ClearWithId(&HSTRING::from(env.notification_aumid()));
    }
}
