//! The desktop: toasts, the fullscreen state, the foreground window, the power request, the
//! AUMID shortcut and the per-user handlers (APP-014, APP-022, ARCH-127, CORE-160; Appendix G
//! "Notifications", "Keep-awake" and "Activation and registration").
//!
//! The core and the apps see the desktop through the [`Desktop`] trait: [`real_desktop`] on
//! the stable and dev channels, and `desktop_fake::FakeDesktop` on test channels (QA-066), so
//! no automated test posts a real toast, reads the real foreground or holds a real power
//! request (QA-038). The writes of P4a-R26 ([`write_shortcut`], [`register_handlers`]) are free
//! functions, called only after your own input (`lil doctor`'s `y` or the app's "Set up").
//!
//! What Task 1 found (`docs/spikes/11-windows-integration.md`) and this module relies on:
//! - a windowless, detached process posts toasts under a per-user shortcut's AUMID (S1.2);
//! - `SHQueryUserNotificationState` reports `QUNS_RUNNING_D3D_FULL_SCREEN`, then `QUNS_BUSY`,
//!   for a fullscreen Direct3D program, in well under 1 ms (S1.6);
//! - keep-awake is `PowerSetRequest(PowerRequestSystemRequired)` on a `PowerCreateRequest`
//!   object with a reason string (S1.7);
//! - a protocol activation's command line is `"<exe>" "<url>"`, the URL one quoted argument,
//!   unchanged (S1.3), so the handler reads the raw command line and never re-splits it
//!   (P4a-R3).
//!
//! No toast is ever updated in place, so there is no update call here (P4a-R36).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Environment::GetCommandLineW;
use windows::Win32::UI::Shell::{
    QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, GetForegroundWindow, GetWindowThreadProcessId,
};

use crate::env::Env;
use crate::power::PowerRequest;

pub use crate::classes::{register_handlers, remove_test_registry, unregister_handlers};
pub use crate::power::POWER_REASON;
pub use crate::shortcut::{
    ShortcutInfo, read_shortcut, remove_clsid_registration, remove_shortcut, write_shortcut,
};
pub use crate::toast::{clear_toast_history, toast_history_tags};

/// What `SHQueryUserNotificationState` says (CORE-160, P4a-R6): `D3d`, `Busy` and
/// `Presentation` are fullscreen; `Unknown` when the call failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fullscreen {
    No,
    D3d,
    Busy,
    Presentation,
    Unknown,
}

/// A toast to post: its XML (Appendix G "Toast XML"), `Tag` and `Group`, and whether it is
/// quiet (`SuppressPopup`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub tag: String,
    pub group: String,
    pub xml: String,
    pub silent: bool,
}

/// Whether the channel's toasts can be posted (Appendix G "Ready"). `Unavailable` carries the
/// `toasts_reason`: `disabled_for_app`, `disabled_for_user`, `disabled_by_policy`, `elevated`
/// or `no_platform`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastsState {
    Ready,
    Unavailable(&'static str),
}

/// The AUMID shortcut's state (`notification_path.shortcut`; P4a-R37). `WrongClsid`: the
/// shortcut's `ToastActivatorCLSID`, or that CLSID's `LocalServer32` registration, is missing
/// or differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutState {
    Ok,
    Missing,
    WrongTarget,
    WrongAumid,
    WrongClsid,
}

/// The channel's scheme and `.lilref` handlers (`notification_path.handlers`). `OtherChannel`
/// names the exe another build's command runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerState {
    Ok,
    Missing,
    OtherChannel(PathBuf),
}

/// `ToastNotifier::Setting()`, as this crate names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifierSetting {
    Enabled,
    DisabledForApplication,
    DisabledForUser,
    DisabledByGroupPolicy,
    DisabledByManifest,
}

/// The desktop as the core and the apps see it.
pub trait Desktop: Send + Sync {
    /// The fullscreen state, read once (the core's 2 s poll, P4a-R6).
    fn fullscreen(&self) -> Fullscreen;
    /// The process that owns the foreground window, if any (P4a-R7).
    fn foreground_pid(&self) -> Option<u32>;
    /// [`ToastsState::Ready`] only when the channel's notifier reports `Setting() == Enabled`
    /// and this process isn't elevated.
    fn toasts_state(&self) -> ToastsState;
    /// Posts `t` under the channel's AUMID.
    fn post(&self, t: &Toast) -> Result<(), String>;
    /// Removes the toast `tag` of `group` from the notification centre, if it is there.
    fn remove(&self, tag: &str, group: &str);
    /// Holds (`true`) or releases (`false`) the one system-required power request (ARCH-127).
    /// Holding twice holds it once.
    fn set_power_request(&self, held: bool) -> std::io::Result<()>;
    /// Whether this PC lets the request be made (`keep_awake_available`).
    fn power_request_available(&self) -> bool;
    /// The AUMID shortcut's state for `exe`.
    fn shortcut_state(&self, env: &Env, exe: &Path) -> ShortcutState;
    /// The scheme and `.lilref` handlers' state for `exe`.
    fn handler_state(&self, env: &Env, exe: &Path) -> HandlerState;
}

/// Appendix G's map from the notifier's setting to [`ToastsState`]: no notifier (`None`) is
/// `no_platform`; an elevated process is `elevated` whatever the setting; then
/// `DisabledForApplication` and `DisabledByManifest` are `disabled_for_app`, `DisabledForUser`
/// `disabled_for_user` and `DisabledByGroupPolicy` `disabled_by_policy`.
pub fn toasts_state_for(setting: Option<NotifierSetting>, elevated: bool) -> ToastsState {
    let Some(setting) = setting else {
        return ToastsState::Unavailable("no_platform");
    };
    if elevated {
        return ToastsState::Unavailable("elevated");
    }
    match setting {
        NotifierSetting::Enabled => ToastsState::Ready,
        NotifierSetting::DisabledForApplication | NotifierSetting::DisabledByManifest => {
            ToastsState::Unavailable("disabled_for_app")
        }
        NotifierSetting::DisabledForUser => ToastsState::Unavailable("disabled_for_user"),
        NotifierSetting::DisabledByGroupPolicy => ToastsState::Unavailable("disabled_by_policy"),
    }
}

/// The real desktop for `env`'s channel: toasts through
/// `CreateToastNotifierWithId(env.notification_aumid())`.
pub fn real_desktop(env: &Env) -> Arc<dyn Desktop> {
    Arc::new(RealDesktop {
        aumid: env.notification_aumid(),
        power: PowerRequest::default(),
    })
}

struct RealDesktop {
    aumid: String,
    power: PowerRequest,
}

impl Desktop for RealDesktop {
    fn fullscreen(&self) -> Fullscreen {
        // SAFETY: a plain query with no arguments.
        match unsafe { SHQueryUserNotificationState() } {
            Ok(QUNS_RUNNING_D3D_FULL_SCREEN) => Fullscreen::D3d,
            Ok(QUNS_BUSY) => Fullscreen::Busy,
            Ok(QUNS_PRESENTATION_MODE) => Fullscreen::Presentation,
            Ok(_) => Fullscreen::No,
            Err(_) => Fullscreen::Unknown,
        }
    }

    fn foreground_pid(&self) -> Option<u32> {
        // SAFETY: plain queries; a null or stale window gives pid 0.
        unsafe {
            let window: HWND = GetForegroundWindow();
            if window.is_invalid() {
                return None;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(window, Some(&mut pid));
            (pid != 0).then_some(pid)
        }
    }

    fn toasts_state(&self) -> ToastsState {
        toasts_state_for(
            crate::toast::setting(&self.aumid),
            crate::toast::is_elevated(),
        )
    }

    fn post(&self, t: &Toast) -> Result<(), String> {
        crate::toast::post(&self.aumid, t)
    }

    fn remove(&self, tag: &str, group: &str) {
        crate::toast::remove(&self.aumid, tag, group);
    }

    fn set_power_request(&self, held: bool) -> std::io::Result<()> {
        self.power.set(held)
    }

    fn power_request_available(&self) -> bool {
        self.power.available()
    }

    fn shortcut_state(&self, env: &Env, exe: &Path) -> ShortcutState {
        crate::shortcut::state(env, exe)
    }

    fn handler_state(&self, env: &Env, exe: &Path) -> HandlerState {
        crate::classes::state(env, exe)
    }
}

/// `AllowSetForegroundWindow(pid)`: lets the running instance take the foreground from this
/// second launch (APP-014). `false` when Windows refused.
pub fn allow_set_foreground(pid: u32) -> bool {
    // SAFETY: a plain call with a process id.
    unsafe { AllowSetForegroundWindow(pid) }.is_ok()
}

/// The raw command line, as `GetCommandLineW` returns it (P4a-R3).
pub fn raw_command_line() -> String {
    // SAFETY: `GetCommandLineW` returns this process's NUL-terminated command line, valid for
    // the process's life; it is copied out at once.
    unsafe { String::from_utf16_lossy(GetCommandLineW().as_wide()) }
}

/// Splits a raw command line into the exe, by `CommandLineToArgvW`'s rule for the first
/// argument (a quoted exe ends at the next `"`, with no escapes; an unquoted one at the first
/// space or tab), and the rest, with only the spaces and tabs after the exe taken off. The rest
/// is otherwise untouched: quotes, backslashes and percent signs stay as they came (P4a-R3).
pub fn split_first_argument(cmdline: &str) -> (String, String) {
    let (exe, rest) = match cmdline.strip_prefix('"') {
        Some(quoted) => match quoted.find('"') {
            Some(end) => (&quoted[..end], &quoted[end + 1..]),
            None => (quoted, ""),
        },
        None => match cmdline.find([' ', '\t']) {
            Some(end) => (&cmdline[..end], &cmdline[end..]),
            None => (cmdline, ""),
        },
    };
    (
        exe.to_string(),
        rest.trim_start_matches([' ', '\t']).to_string(),
    )
}

/// `a` and `b` name the same file by their spelling: no `\\?\`, `/` as `\`, ASCII case
/// ignored, no surrounding quotes.
pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let s = p.to_string_lossy();
        crate::env::normalise_for_hash(Path::new(s.trim_matches('"')))
    };
    norm(a) == norm(b)
}
