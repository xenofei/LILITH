//! Classic WinRT toasts under the channel's AUMID (APP-022; Appendix G "Notifications"), with
//! no Windows App SDK: `ToastNotificationManager::CreateToastNotifierWithId`, as Task 1's S1.2
//! posted them from a windowless, detached process. Every suite toast activates by protocol;
//! no COM activator runs (P4a-R37).

use std::sync::{Arc, Mutex, PoisonError};

use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::{
    NotificationSetting, ToastDismissalReason, ToastDismissedEventArgs, ToastFailedEventArgs,
    ToastNotification, ToastNotificationManager, ToastNotifier,
};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{HSTRING, IInspectable, Ref};

use crate::classes::require_test_channel;
use crate::desktop::{NotifierSetting, Toast};
use crate::env::Env;

fn notifier(aumid: &str) -> windows::core::Result<ToastNotifier> {
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(aumid))
}

/// `HRESULT_FROM_WIN32(ERROR_NOT_FOUND)`: what `Setting()` answers before the AUMID's first
/// toast.
const NOT_FOUND: windows::core::HRESULT = windows::core::HRESULT(0x8007_0490_u32 as i32);

/// The notifier's `Setting()` for `aumid`; `None` when there is no notifier (no platform), and
/// [`NotifierSetting::NotYetShown`] when `Setting()` itself answers `ERROR_NOT_FOUND`: the
/// AUMID has never shown a toast. A notifier that can't be created is `None`, whatever the
/// error.
pub(crate) fn setting(aumid: &str) -> Option<NotifierSetting> {
    let notifier = notifier(aumid).ok()?;
    let setting = match notifier.Setting() {
        Ok(s) => s,
        Err(e) if e.code() == NOT_FOUND => return Some(NotifierSetting::NotYetShown),
        Err(_) => return None,
    };
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
/// elevated: the notifier's own setting still decides. `lil dev desktop-tests --host` refuses
/// to run elevated with it (P4a-R39).
pub fn is_elevated() -> bool {
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

/// `t` as a `ToastNotification`: its XML, `Tag`, `Group` and `SuppressPopup`.
fn build(t: &Toast) -> windows::core::Result<ToastNotification> {
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(t.xml.as_str()))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    toast.SetTag(&HSTRING::from(t.tag.as_str()))?;
    toast.SetGroup(&HSTRING::from(t.group.as_str()))?;
    toast.SetSuppressPopup(t.silent)?;
    Ok(toast)
}

/// Posts `t` under `aumid`.
pub(crate) fn post(aumid: &str, t: &Toast) -> Result<(), String> {
    build(t)
        .and_then(|toast| notifier(aumid)?.Show(&toast))
        .map_err(|e| format!("the toast could not be posted: {e}"))
}

/// A toast posted by [`post_watched`], and what Windows reported about it so far.
pub struct WatchedToast {
    /// Kept so its handlers stay registered while the test waits.
    _toast: ToastNotification,
    events: Arc<Mutex<Vec<String>>>,
}

impl WatchedToast {
    /// Each event in the order it came: `activated`, `dismissed: <reason>` (`TimedOut`,
    /// `UserCanceled` or `ApplicationHidden`) or `failed: 0x<HRESULT>`.
    pub fn events(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The `failed` event's text, if Windows dropped the toast.
    pub fn failure(&self) -> Option<String> {
        self.events().into_iter().find(|e| e.starts_with("failed"))
    }
}

/// The desktop group's post (P4a-R39: the toast tests read History and the toast's own
/// `Activated`, `Dismissed` and `Failed` events, never UI Automation): posts `t` under the
/// channel's AUMID with those three handlers registered first. Refused, before anything is
/// posted, on a stable or dev channel.
pub fn post_watched(env: &Env, t: &Toast) -> Result<WatchedToast, String> {
    require_test_channel(env, "a watched toast post").map_err(|e| e.to_string())?;
    let events: Arc<Mutex<Vec<String>>> = Arc::default();
    let push = |events: &Arc<Mutex<Vec<String>>>, event: String| {
        events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event);
    };
    let watch = || -> windows::core::Result<ToastNotification> {
        let toast = build(t)?;
        let sink = events.clone();
        toast.Dismissed(&TypedEventHandler::new(
            move |_: Ref<ToastNotification>, args: Ref<ToastDismissedEventArgs>| {
                let reason = match args.as_ref().map(|a| a.Reason()) {
                    Some(Ok(ToastDismissalReason::TimedOut)) => "TimedOut",
                    Some(Ok(ToastDismissalReason::UserCanceled)) => "UserCanceled",
                    Some(Ok(ToastDismissalReason::ApplicationHidden)) => "ApplicationHidden",
                    _ => "unknown",
                };
                push(&sink, format!("dismissed: {reason}"));
                Ok(())
            },
        ))?;
        let sink = events.clone();
        toast.Failed(&TypedEventHandler::new(
            move |_: Ref<ToastNotification>, args: Ref<ToastFailedEventArgs>| {
                let code = args.as_ref().and_then(|a| a.ErrorCode().ok()).map_or_else(
                    || "unknown".to_string(),
                    |c| format!("0x{:08x}", c.0 as u32),
                );
                push(&sink, format!("failed: {code}"));
                Ok(())
            },
        ))?;
        let sink = events.clone();
        toast.Activated(&TypedEventHandler::new(
            move |_: Ref<ToastNotification>, _: Ref<IInspectable>| {
                push(&sink, "activated".to_string());
                Ok(())
            },
        ))?;
        notifier(&env.notification_aumid())?.Show(&toast)?;
        Ok(toast)
    };
    let toast = watch().map_err(|e| format!("the toast could not be posted: {e}"))?;
    Ok(WatchedToast {
        _toast: toast,
        events,
    })
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

/// Removes every toast of a test channel from the notification centre (the desktop group's
/// cleanup). Refused, before anything is read or removed, on a stable or dev channel.
pub fn clear_toast_history(env: &Env) -> std::io::Result<()> {
    require_test_channel(env, "clearing the toast history")?;
    ToastNotificationManager::History()
        .and_then(|history| history.ClearWithId(&HSTRING::from(env.notification_aumid())))
        .map_err(|e| crate::process::io_error(&e))
}
