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

pub use crate::classes::{
    HandlerPart, HandlerParts, TEST_MARKER, TEST_OWNER, TestRegistryRoot, handler_parts,
    leftover_registrations, mark_test_registry_owner, register_handlers, register_lilref,
    register_scheme, remove_notification_settings, remove_test_registry, test_registry_roots,
    unregister_handlers,
};
pub use crate::power::POWER_REASON;
pub use crate::shortcut::{
    ShortcutInfo, read_shortcut, remove_aumid_registration, remove_clsid_registration,
    remove_shortcut, write_shortcut,
};
pub use crate::toast::{
    WatchedToast, clear_toast_history, is_elevated, post_watched, toast_history_tags,
};

/// The fixed start of every desktop-group test channel's id: `test-desktop-<16 hex>`. A run
/// that was killed before its cleanup ran can then be swept by name (Task 30's runner, P4a-R39):
/// the shortcut `<Programs>\LILITH (Test-desktop-*).lnk`, the keys
/// `HKCU\Software\LILITH-test-desktop-*`, and under `HKCU\Software\Classes` the scheme
/// `lil-test-desktop-*` and the channel's toast CLSID. The id read from any of those names
/// gives that CLSID ([`Env::toast_activator_clsid`] of `test-<id>`), and
/// [`leftover_registrations`] lists what is still there.
pub const DESKTOP_TEST_ID_PREFIX: &str = "desktop-";

/// The desktop-group test channel id (`desktop-<16 hex>`, lowercase) that `name` belongs to,
/// when it is one of the names a desktop test writes ([`DESKTOP_TEST_ID_PREFIX`]): the scheme
/// key `lil-test-desktop-<hex>`, the registry root `LILITH-test-desktop-<hex>`, the shortcut
/// `LILITH (Test-desktop-<hex>).lnk` or the AUMID `LILITH.Suite.Test-desktop-<hex>`. Compared ASCII case-insensitively; anything else, the
/// stable and dev names among it, is `None`.
pub fn desktop_test_id_in(name: &str) -> Option<String> {
    let app = crate::names::APP_NAME;
    let forms = [
        (format!("lil-test-{DESKTOP_TEST_ID_PREFIX}"), String::new()),
        (
            format!("{app}-test-{DESKTOP_TEST_ID_PREFIX}"),
            String::new(),
        ),
        (
            format!("{app} (Test-{DESKTOP_TEST_ID_PREFIX}"),
            ").lnk".to_string(),
        ),
        // The AUMID: its registration and Windows' notification settings entry.
        (
            format!("{app}.Suite.Test-{DESKTOP_TEST_ID_PREFIX}"),
            String::new(),
        ),
    ];
    let lower = name.to_ascii_lowercase();
    forms.iter().find_map(|(head, tail)| {
        let hex = lower
            .strip_prefix(&head.to_ascii_lowercase())?
            .strip_suffix(tail.as_str())?;
        (hex.len() == 16 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| format!("{DESKTOP_TEST_ID_PREFIX}{hex}"))
    })
}

/// Whether `id` is a desktop-group test channel id: `desktop-<16 lowercase hex>`.
fn is_desktop_test_id(id: &str) -> bool {
    id.strip_prefix(DESKTOP_TEST_ID_PREFIX).is_some_and(|hex| {
        hex.len() == 16 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// The ids of the desktop-group test channels that left something on this PC (read-only):
/// by name, a scheme key under `HKCU\Software\Classes`, a registry root under `HKCU\Software`,
/// an AUMID under `HKCU\Software\Classes\AppUserModelId` or under Windows' notification
/// settings (`…\CurrentVersion\Notifications\Settings`), or a shortcut in `programs`; and by [`TEST_MARKER`], any marked key directly below
/// `HKCU\Software\Classes`, its `CLSID` or its `AppUserModelId`, so an orphaned CLSID or AUMID
/// key whose other names are gone is found too. What a killed host run can leave behind, for
/// [`sweep_desktop_tests`] (P4a-R39). Sorted, without repeats.
pub fn desktop_test_ids_left(programs: &Path) -> std::io::Result<Vec<String>> {
    use crate::classes::{marked_ids, subkey_names};
    let mut names = subkey_names(r"Software\Classes")?;
    names.extend(subkey_names("Software")?);
    names.extend(subkey_names(r"Software\Classes\AppUserModelId")?);
    names.extend(subkey_names(crate::classes::NOTIFICATION_SETTINGS)?);
    match std::fs::read_dir(programs) {
        Ok(entries) => {
            for entry in entries {
                names.push(entry?.file_name().to_string_lossy().into_owned());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut ids: Vec<String> = names.iter().filter_map(|n| desktop_test_id_in(n)).collect();
    for root in [
        r"Software\Classes",
        r"Software\Classes\CLSID",
        r"Software\Classes\AppUserModelId",
    ] {
        ids.extend(
            marked_ids(root)?
                .into_iter()
                .filter(|id| is_desktop_test_id(id)),
        );
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// The desktop test channel `id` as the host run's tests build it: the shell's classes root and
/// `programs` as its Programs folder. Its roots are never used.
fn desktop_sweep_env(id: &str, programs: &Path) -> Env {
    let scratch = std::env::temp_dir().join("lil-desktop-sweep");
    Env::with_roots(
        crate::env::Channel::Test(id.to_string()),
        crate::env::Roots {
            roaming: scratch.clone(),
            local: scratch.clone(),
            user_profile: scratch,
        },
        "S-1-0-0".to_string(),
    )
    .with_programs_dir(programs.to_path_buf())
    .with_shell_classes_root()
}

/// What `env` still has: its shortcut, its keys ([`leftover_registrations`]) and its toasts. A
/// History that can't be read (`GetHistoryWithId` answers "not found" for an AUMID Windows never
/// saw) counts as empty here; the tests' own cleanup checks it strictly.
fn leftovers_of(env: &Env) -> std::io::Result<Vec<String>> {
    let mut left = Vec::new();
    let lnk = env.start_menu_shortcut();
    if lnk.exists() {
        left.push(lnk.display().to_string());
    }
    left.extend(leftover_registrations(env)?);
    if let Ok(tags) = toast_history_tags(env) {
        let aumid = env.notification_aumid();
        left.extend(tags.into_iter().map(|t| format!("toast {t} of {aumid}")));
    }
    Ok(left)
}

/// How long [`sweep_desktop_tests`] keeps removing a settings entry the notification platform
/// writes back.
const SWEEP_SETTLE: std::time::Duration = std::time::Duration::from_secs(10);

/// What [`sweep_desktop_tests`] removed, and what is still there.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesktopSweep {
    pub removed: Vec<String>,
    pub remaining: Vec<String>,
}

/// What every desktop test channel of [`desktop_test_ids_left`] still has (read-only).
pub fn desktop_test_leftovers(programs: &Path) -> std::io::Result<Vec<String>> {
    let mut left = Vec::new();
    for id in desktop_test_ids_left(programs)? {
        left.extend(leftovers_of(&desktop_sweep_env(&id, programs))?);
    }
    Ok(left)
}

/// Removes what every desktop test channel of [`desktop_test_ids_left`] left, each by its exact
/// name through the test-only removers (which refuse anything but a test channel): the
/// shortcut, the CLSID and AUMID registrations, the scheme, the registry root and the toasts.
pub fn sweep_desktop_tests(programs: &Path) -> std::io::Result<DesktopSweep> {
    let mut sweep = DesktopSweep::default();
    for id in desktop_test_ids_left(programs)? {
        let env = desktop_sweep_env(&id, programs);
        let before = leftovers_of(&env)?;
        remove_shortcut(&env)?;
        unregister_handlers(&env)?;
        remove_test_registry(&env)?;
        // History first: the notification platform writes the settings entry back a moment
        // after a toast or a History clear, so that entry goes last and until it stays gone (the
        // same order as the desktop tests' cleanup). A History Windows never had for this AUMID
        // can't be cleared: nothing to clear.
        let _ = clear_toast_history(&env);
        let deadline = std::time::Instant::now() + SWEEP_SETTLE;
        loop {
            remove_notification_settings(&env)?;
            std::thread::sleep(std::time::Duration::from_millis(500));
            let back = leftover_registrations(&env)?
                .iter()
                .any(|k| k.contains(r"\Notifications\Settings\"));
            if !back || std::time::Instant::now() >= deadline {
                break;
            }
        }
        let after = leftovers_of(&env)?;
        sweep
            .removed
            .extend(before.into_iter().filter(|b| !after.contains(b)));
        sweep.remaining.extend(after);
    }
    Ok(sweep)
}

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
/// or `no_platform`. `NotYetShown` (plan 4a ruling 2026-10-24): this AUMID has never shown a
/// toast, so Windows has no notification settings for it yet; posting is allowed, and the
/// post's result, or the next reading once it showed, decides. It ends as
/// `Unavailable("no_platform")` when it lasts [`NOT_SHOWN_GRACE`] after a post that returned
/// `Ok` ([`toasts_state_after_post`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastsState {
    Ready,
    NotYetShown,
    Unavailable(&'static str),
}

impl ToastsState {
    /// Whether a toast may be posted: [`ToastsState::Ready`] or [`ToastsState::NotYetShown`].
    pub fn can_post(self) -> bool {
        matches!(self, ToastsState::Ready | ToastsState::NotYetShown)
    }
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

/// The channel's handlers as `notification_path.handlers` shows them: its own scheme alone,
/// which toast URLs use; who holds `.lilref` never changes it (plan 4a ruling, 2026-10-25;
/// [`HandlerParts`] has both). `OtherChannel` names the exe, or the program, the scheme runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerState {
    Ok,
    Missing,
    OtherChannel(PathBuf),
}

/// `ToastNotifier::Setting()`, as this crate names it. `NotYetShown`: it failed with
/// `ERROR_NOT_FOUND` ("Element not found"), which it does until the AUMID's first toast,
/// when Windows creates the app's notification settings (Task 30's host run).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifierSetting {
    Enabled,
    NotYetShown,
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
    /// and this process isn't elevated; [`ToastsState::NotYetShown`] before the AUMID's first
    /// toast.
    fn toasts_state(&self) -> ToastsState;
    /// Posts `t` under the channel's AUMID.
    fn post(&self, t: &Toast) -> Result<(), String>;
    /// Removes the toast `tag` of `group` from the notification centre, if it is there.
    fn remove(&self, tag: &str, group: &str);
    /// The `(tag, group)` of each of the channel's toasts in the notification centre (empty
    /// when it can't be read).
    fn history(&self) -> Vec<(String, String)>;
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
/// `disabled_for_user` and `DisabledByGroupPolicy` `disabled_by_policy`; `NotYetShown` is
/// [`ToastsState::NotYetShown`] (plan 4a ruling 2026-10-24).
pub fn toasts_state_for(setting: Option<NotifierSetting>, elevated: bool) -> ToastsState {
    let Some(setting) = setting else {
        return ToastsState::Unavailable("no_platform");
    };
    if elevated {
        return ToastsState::Unavailable("elevated");
    }
    match setting {
        NotifierSetting::Enabled => ToastsState::Ready,
        NotifierSetting::NotYetShown => ToastsState::NotYetShown,
        NotifierSetting::DisabledForApplication | NotifierSetting::DisabledByManifest => {
            ToastsState::Unavailable("disabled_for_app")
        }
        NotifierSetting::DisabledForUser => ToastsState::Unavailable("disabled_for_user"),
        NotifierSetting::DisabledByGroupPolicy => ToastsState::Unavailable("disabled_by_policy"),
    }
}

/// How long `Setting()` may still answer `ERROR_NOT_FOUND` after a post that returned `Ok`
/// (plan 4a ruling 2026-10-24; Task 30's host run saw `Enabled` about 7 ms after the post).
pub const NOT_SHOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// The end of [`ToastsState::NotYetShown`] (Appendix G "Ready"): `state` as read, unless it is
/// still `NotYetShown` at least [`NOT_SHOWN_GRACE`] after this process's first post that
/// returned `Ok` (`since_first_ok_post`). Then the toast didn't show and Windows never created
/// the AUMID's settings, so toasts can't reach you: `Unavailable("no_platform")`.
pub fn toasts_state_after_post(
    state: ToastsState,
    since_first_ok_post: Option<std::time::Duration>,
) -> ToastsState {
    match (state, since_first_ok_post) {
        (ToastsState::NotYetShown, Some(since)) if since >= NOT_SHOWN_GRACE => {
            ToastsState::Unavailable("no_platform")
        }
        _ => state,
    }
}

/// The end of [`ToastsState::NotYetShown`], kept per desktop (plan 4a ruling 2026-10-24): the
/// first post that returned `Ok` since the last reading that wasn't `NotYetShown` starts the
/// [`NOT_SHOWN_GRACE`]; any other reading clears it, so a later `NotYetShown` (a reset platform)
/// starts afresh instead of reading `no_platform` at once. Times are from any fixed origin.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct NotShownEnd {
    first_ok_post: Option<std::time::Duration>,
    /// The last reading, so only a post made while `NotYetShown` (or before any reading)
    /// starts the grace.
    last: Option<ToastsState>,
}

impl NotShownEnd {
    /// A post returned `Ok` at `now`.
    pub fn posted_ok(&mut self, now: std::time::Duration) {
        if matches!(self.last, None | Some(ToastsState::NotYetShown)) {
            self.first_ok_post.get_or_insert(now);
        }
    }

    /// A post failed at `now`: it starts the grace as an `Ok` one does (plan 4a clarification,
    /// 2026-10-25), so posts that keep failing while `NotYetShown` end as `no_platform` too.
    pub fn post_failed(&mut self, now: std::time::Duration) {
        self.posted_ok(now);
    }

    /// The state to report for the reading `read` at `now` ([`toasts_state_after_post`]).
    pub fn state(&mut self, read: ToastsState, now: std::time::Duration) -> ToastsState {
        self.last = Some(read);
        if read != ToastsState::NotYetShown {
            self.first_ok_post = None;
            return read;
        }
        toasts_state_after_post(read, self.first_ok_post.map(|t| now.saturating_sub(t)))
    }
}

/// The AUMID shortcut's state for `exe`, read from the disk and the registry: what `lil doctor`
/// checks before it offers to write it (P4a-R26), on every channel. It blocks (the registry
/// and COM): never call it on a GUI thread (QA-026).
pub fn shortcut_state(env: &Env, exe: &Path) -> ShortcutState {
    crate::shortcut::state(env, exe)
}

/// The scheme and `.lilref` handlers' one state for `exe` ([`HandlerParts::state`]), read from
/// the registry; [`handler_parts`] gives each part. It blocks: never call it on a GUI thread
/// (QA-026).
pub fn handler_state(env: &Env, exe: &Path) -> HandlerState {
    crate::classes::state(env, exe)
}

/// The real desktop for `env`'s channel: toasts through
/// `CreateToastNotifierWithId(env.notification_aumid())`.
pub fn real_desktop(env: &Env) -> Arc<dyn Desktop> {
    Arc::new(RealDesktop {
        aumid: env.notification_aumid(),
        power: PowerRequest::default(),
        not_shown: std::sync::Mutex::new(NotShownEnd::default()),
        start: std::time::Instant::now(),
    })
}

struct RealDesktop {
    aumid: String,
    power: PowerRequest,
    /// The end of `NotYetShown` ([`NotShownEnd`]), timed from `start`.
    not_shown: std::sync::Mutex<NotShownEnd>,
    start: std::time::Instant,
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
        let read = toasts_state_for(
            crate::toast::setting(&self.aumid),
            crate::toast::is_elevated(),
        );
        self.not_shown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .state(read, self.start.elapsed())
    }

    fn post(&self, t: &Toast) -> Result<(), String> {
        let posted = crate::toast::post(&self.aumid, t);
        let mut not_shown = self
            .not_shown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &posted {
            Ok(()) => not_shown.posted_ok(self.start.elapsed()),
            Err(_) => not_shown.post_failed(self.start.elapsed()),
        }
        posted
    }

    fn remove(&self, tag: &str, group: &str) {
        crate::toast::remove(&self.aumid, tag, group);
    }

    fn history(&self) -> Vec<(String, String)> {
        crate::toast::history(&self.aumid)
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
