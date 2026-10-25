//! Windows Hello for the reviewer key (QA-088, P4a-R23, P4a-R38; plan 4a Task 28).
//!
//! **The one route LILITH builds: the KEK.** A Windows Hello key credential
//! ([`KeyCredentialManager`]) signs a fixed 32-byte challenge, and the key-encryption key is
//! SHA-256 over [`KEK_DOMAIN`] followed by the raw signature ([`kek_from_signature`], P4a-R23).
//! Spike 11 (S1.8) found 10 of 10 signatures over one challenge byte-identical, across two
//! processes. The DPAPI route is not built (Task 28 fix round 1: a DPAPI-protected key is
//! readable without a prompt by every program running as you). The DPAPI calls and the consent
//! check below stay only because the route probe (`examples/hello-probe.rs`) still measures
//! them.
//!
//! **In front first (QA-088, Task 28 Phase B ruling).** [`kek_enroll`] and [`kek_derive`] open
//! an [`OwnerWindow`] that shows what you are confirming and try to bring it to the front. When
//! Windows keeps another window in front (on the owner's PC the foreground lock timeout is at
//! its maximum, and the probe saw the raise fail), the window's taskbar button flashes and it
//! waits, within [`HELLO_WAIT`], for you to bring it forward ([`front_or_wait`], [`ask_then`]);
//! it never falls back to the passphrase for that. The Windows Hello prompt raised after that
//! comes to the front by itself (the probe: 7 of 7 steps, within 302 ms, over fullscreen too).
//! Every prompt is cancelled after [`HELLO_WAIT`] ([`wait_bounded`]). With an accept label ("Merge",
//! "Approve"), [`kek_derive`]'s window has that button and "Deny", and Windows Hello is raised
//! only after the accept click ([`ask_then`]); "Deny", closing the window or no choice within
//! [`HELLO_WAIT`] refuses with `PermissionDenied` (Task 28 fix round 2: Windows Hello Face can
//! complete a prompt with no deliberate act). Only a real pointer click accepts
//! ([`choice_from_command`]): the accept button is subclassed and armed only by a `WM_LBUTTONUP`
//! inside it from a real mouse, touch, pen or touchpad, on its own thread ([`arms_accept`]:
//! `GetCurrentInputMessageSource` is `IMO_HARDWARE` with a pointer device, `InSendMessage` is
//! false), after a real press on it that, for a mouse or touchpad, followed a real hover of
//! [`HOVER_BEFORE_PRESS`] ([`PressTracker`]: real pointer moves that put the cursor on it, judged
//! with every other gate in [`ClickGate`], Task 28c); and only a `BN_CLICKED` from that button's own
//! window counts, so a posted or sent `WM_COMMAND`, `BM_CLICK`, UI Automation's Invoke,
//! `SendInput` and any key (Space, Enter) are ignored. The buttons work only once the window
//! has been in front for [`BUTTONS_LIVE_AFTER`] of real time ([`LiveClock`]), and stop when it
//! loses the front, or it or the accept button moves or is resized. The text and the labels
//! refuse changes from other programs ([`text_change`]). "Deny" accepts any source: refusing is
//! always safe. A click on the accept button that a gate refuses is never silent (Task 28b): a
//! status line beside the buttons says which gate ([`Refusal::message`]) and the window stays
//! open; while the buttons are grey it says what they wait for ([`StatusLine`]).
//!
//! **Placement (P4a-R38).** The owner's rule is that the prompt comes to the front of every
//! application, a fullscreen one included. Spike 11 showed that an unowned create prompt and a
//! prompt owned by the console window are not reliably in front, so every prompting call here is
//! owned by a real top-level window that this process opens and brings to the front
//! ([`OwnerWindow`]), never `GetConsoleWindow()`. The window-owned key-credential calls,
//! `KeyCredentialManager.RequestCreateForWindowAsync` and `KeyCredential.RequestSignForWindowAsync`,
//! are not in `windows` 0.62.2, so they are bound by hand below from the interfaces in this
//! Windows build's own metadata (`C:\Windows\System32\WinMetadata\Windows.Security.winmd`):
//! `IKeyCredentialManagerCreateWithWindowStatics` {30b1b9c9-61ef-43e8-88ac-cc433b38d1a6} and
//! `IKeyCredentialWithWindow` {050deb3b-b19c-4635-9df6-5650d66c62b1}, each one method after
//! `IInspectable`. The route probe (Task 28 Step 0, 2026-10-24) showed the window-owned prompt
//! comes to the front by itself, and that a create and two signs work with identical
//! signatures: the KEK route.
//!
//! **Secrets.** The signature and the KEK live in [`Zeroizing`] buffers and are never logged;
//! DPAPI's plaintext comes back in one. The only output is one stderr line per refused accept
//! click, naming the gate ([`Refusal::name`]) and nothing about the input.
//!
//! **Prompts.** [`kek_enroll`], [`kek_derive`], their `*_for_window` forms and the `start_*`
//! calls raise a Windows Hello prompt. [`supported`], [`tpm_present`], [`credential_exists`],
//! [`kek_remove`], [`window_bindings_available`] and the DPAPI calls never do.

use std::ffi::c_void;
use std::io::{self, Write as _};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use windows::Security::Credentials::UI::{UserConsentVerificationResult, UserConsentVerifier};
use windows::Security::Credentials::{
    KeyCredential, KeyCredentialCreationOption, KeyCredentialManager, KeyCredentialOperationResult,
    KeyCredentialRetrievalResult, KeyCredentialStatus,
};
use windows::Security::Cryptography::CryptographicBuffer;
use windows::Win32::Devices::BiometricFramework::{
    WINBIO_E_UNKNOWN_ID, WINBIO_IDENTITY, WINBIO_POOL_SYSTEM, WINBIO_UNIT_SCHEMA,
    WinBioCloseSession, WinBioEnumBiometricUnits, WinBioEnumEnrollments, WinBioFree,
    WinBioOpenSession,
};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    DIGCF_PRESENT, GUID_DEVCLASS_BIOMETRIC, SP_DEVINFO_DATA, SetupDiDestroyDeviceInfoList,
    SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceInstanceIdW,
};
use windows::Win32::Foundation::ERROR_NO_MORE_ITEMS;
use windows::Win32::Foundation::{HLOCAL, HWND, LPARAM, LRESULT, LocalFree, WPARAM};
use windows::Win32::Graphics::Gdi::{
    DEFAULT_GUI_FONT, GetMonitorInfoW, GetStockObject, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromWindow, ScreenToClient,
};
use windows::Win32::Security::Authorization::ConvertStringSidToSidW;
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::Win32::Security::{GetLengthSid, PSID};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::System::TpmBaseServices::{TPM_DEVICE_INFO, Tbsi_GetDeviceInfo};
use windows::Win32::System::WinRT::IUserConsentVerifierInterop;
use windows::Win32::UI::Controls::{EM_REPLACESEL, EM_SETREADONLY, WM_MOUSELEAVE};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, IsWindowEnabled, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::Input::{
    GetCurrentInputMessageSource, GetRawInputData, GetRegisteredRawInputDevices, HRAWINPUT,
    IMDT_KEYBOARD, IMDT_MOUSE, IMDT_PEN, IMDT_TOUCH, IMDT_TOUCHPAD, IMO_HARDWARE, IMO_INJECTED,
    IMO_SYSTEM, INPUT_MESSAGE_SOURCE, MOUSE_MOVE_ABSOLUTE, RAWINPUT, RAWINPUTDEVICE,
    RAWINPUTHEADER, RID_INPUT, RIDEV_INPUTSINK, RIDEV_REMOVE, RIM_TYPEMOUSE,
    RegisterRawInputDevices,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, BringWindowToTop, CW_USEDEFAULT, CallWindowProcW, CreateWindowExW, DefWindowProcW,
    DestroyWindow, DispatchMessageW, ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY, FLASHW_ALL,
    FLASHW_TIMERNOFG, FLASHWINFO, FlashWindowEx, GWLP_WNDPROC, GetClientRect, GetCursorPos,
    GetDlgItem, GetForegroundWindow, GetMessagePos, GetMessageW, GetParent, GetWindowLongPtrW,
    GetWindowRect, GetWindowThreadProcessId, HMENU, InSendMessage, KillTimer, MSG, PostMessageW,
    PostQuitMessage, RegisterClassW, SW_SHOWNORMAL, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW,
    SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow,
    TranslateMessage, WA_INACTIVE, WINDOW_STYLE, WM_ACTIVATE, WM_CANCELMODE, WM_CAPTURECHANGED,
    WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_INPUT, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_PASTE, WM_SETFONT, WM_SETTEXT, WM_TIMER,
    WM_WINDOWPOSCHANGED, WNDCLASSW, WNDPROC, WS_CAPTION, WS_CHILD, WS_DISABLED, WS_EX_APPWINDOW,
    WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};
use windows::core::{GUID, HRESULT, HSTRING, IInspectable_Vtbl, Interface, w};
use windows_future::IAsyncOperation;
use zeroize::Zeroizing;

/// The start of the KEK's hash input (P4a-R23).
pub const KEK_DOMAIN: &[u8] = b"LILITH reviewer KEK v1";

/// The owner window's class name. A probe or test can find the window by it.
pub const OWNER_WINDOW_CLASS: &str = "LILITH-hello-owner";

/// The class of the window that hosts the Windows Hello prompt (`CredentialUIBroker.exe`), as
/// spike 11 recorded it.
pub const PROMPT_WINDOW_CLASS: &str = "Credential Dialog Xaml Host";

/// The key-encryption key: SHA-256 over [`KEK_DOMAIN`] followed by the raw `signature` bytes
/// (P4a-R23). Pure; the KEK route's only cryptography.
pub fn kek_from_signature(signature: &[u8]) -> Zeroizing<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(KEK_DOMAIN);
    hasher.update(signature);
    let mut kek = Zeroizing::new([0u8; 32]);
    // Straight into the zeroized buffer: no temporary copy of the KEK is left behind.
    hasher.finalize_into((&mut *kek).into());
    kek
}

/// 32 random bytes from the operating system: an enrolment's challenge (P4a-R23).
pub fn new_challenge() -> io::Result<[u8; 32]> {
    let mut challenge = [0u8; 32];
    getrandom::fill(&mut challenge).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(challenge)
}

fn io_err(e: windows::core::Error) -> io::Error {
    io::Error::other(e.message())
}

/// Whether Windows Hello is set up for this user (`KeyCredentialManager.IsSupportedAsync`).
/// No prompt. `false` when the call fails.
pub fn supported() -> bool {
    KeyCredentialManager::IsSupportedAsync()
        .and_then(|op| op.join())
        .unwrap_or(false)
}

/// Whether a TPM 2.0 is present (`Tbsi_GetDeviceInfo`). Windows Hello keeps its keys in it when
/// there is one; without one the key is software-held, and the suite never calls it
/// hardware-bound (QA-088). No prompt.
pub fn tpm_present() -> bool {
    let mut info = TPM_DEVICE_INFO {
        structVersion: 1,
        ..Default::default()
    };
    // SAFETY: `info` is a live, correctly sized structure owned by this frame.
    let rc = unsafe {
        Tbsi_GetDeviceInfo(
            size_of::<TPM_DEVICE_INFO>() as u32,
            (&raw mut info).cast::<c_void>(),
        )
    };
    rc == 0 && info.tpmVersion == 2
}

// ---------------------------------------------------------------------------------------------
// The window-owned key-credential calls, bound by hand (P4a-R38).
// ---------------------------------------------------------------------------------------------

/// `Windows.UI.WindowId`: for a Win32 window its value is the window handle.
#[repr(C)]
#[derive(Clone, Copy)]
struct WindowId {
    value: u64,
}

impl WindowId {
    fn of(hwnd: isize) -> Self {
        Self { value: hwnd as u64 }
    }
}

#[repr(transparent)]
#[derive(Clone, PartialEq, Eq)]
struct IKeyCredentialManagerCreateWithWindowStatics(windows::core::IUnknown);

// SAFETY: the IID and the vtable below are `IKeyCredentialManagerCreateWithWindowStatics` as
// this Windows build's metadata defines it: `IInspectable`, then `RequestCreateForWindowAsync`.
unsafe impl Interface for IKeyCredentialManagerCreateWithWindowStatics {
    type Vtable = CreateWithWindowVtbl;
    const IID: GUID = GUID::from_u128(0x30b1b9c9_61ef_43e8_88ac_cc433b38d1a6);
}

#[repr(C)]
struct CreateWithWindowVtbl {
    base: IInspectable_Vtbl,
    request_create_for_window_async: unsafe extern "system" fn(
        this: *mut c_void,
        window: WindowId,
        name: *mut c_void,
        option: KeyCredentialCreationOption,
        result: *mut *mut c_void,
    ) -> HRESULT,
}

#[repr(transparent)]
#[derive(Clone, PartialEq, Eq)]
struct IKeyCredentialWithWindow(windows::core::IUnknown);

// SAFETY: as above, for `IKeyCredentialWithWindow`: `IInspectable`, then
// `RequestSignForWindowAsync(WindowId, IBuffer)`.
unsafe impl Interface for IKeyCredentialWithWindow {
    type Vtable = WithWindowVtbl;
    const IID: GUID = GUID::from_u128(0x050deb3b_b19c_4635_9df6_5650d66c62b1);
}

#[repr(C)]
struct WithWindowVtbl {
    base: IInspectable_Vtbl,
    request_sign_for_window_async: unsafe extern "system" fn(
        this: *mut c_void,
        window: WindowId,
        data: *mut c_void,
        result: *mut *mut c_void,
    ) -> HRESULT,
}

/// Wraps the raw `IAsyncOperation` pointer an ABI call wrote.
///
/// # Safety
/// `raw` must be the owned interface pointer of an `IAsyncOperation<T>` (or null after a failed
/// call, which `hr` then reports).
unsafe fn async_op<T: windows::core::RuntimeType + 'static>(
    hr: HRESULT,
    raw: *mut c_void,
) -> io::Result<IAsyncOperation<T>> {
    hr.ok().map_err(io_err)?;
    if raw.is_null() {
        return Err(io::Error::other("Windows returned no operation"));
    }
    // SAFETY: the caller's contract: `raw` is an owned `IAsyncOperation<T>` pointer.
    Ok(unsafe { IAsyncOperation::<T>::from_raw(raw) })
}

/// Whether this Windows build has the window-owned create call (the statics interface
/// resolves). No prompt.
pub fn window_bindings_available() -> bool {
    windows::core::factory::<KeyCredentialManager, IKeyCredentialManagerCreateWithWindowStatics>()
        .is_ok()
}

/// A Windows Hello call that has started and not been waited for. Dropping it neither waits
/// nor cancels; [`Pending::cancel`] closes the prompt.
pub struct Pending<T: windows::core::RuntimeType + 'static> {
    op: IAsyncOperation<T>,
}

impl<T: windows::core::RuntimeType + 'static> Pending<T> {
    /// Whether the call has finished (completed, failed or cancelled).
    pub fn finished(&self) -> bool {
        !matches!(self.op.Status(), Ok(windows_future::AsyncStatus::Started))
    }

    /// Cancels the call, which closes its prompt. Best effort.
    pub fn cancel(&self) {
        let _ = self.op.Cancel();
    }
}

/// A started create ([`start_create_for_window`]).
pub type PendingCreate = Pending<KeyCredentialRetrievalResult>;
/// A started sign ([`start_sign_for_window`]).
pub type PendingSign = Pending<KeyCredentialOperationResult>;
/// A started consent check ([`start_consent_for_window`]).
pub type PendingConsent = Pending<UserConsentVerificationResult>;

/// Why a key-credential call did not succeed: `KeyCredentialStatus` by name.
fn status_error(what: &str, status: KeyCredentialStatus) -> io::Error {
    let name = match status {
        KeyCredentialStatus::UnknownError => "UnknownError",
        KeyCredentialStatus::NotFound => "NotFound",
        KeyCredentialStatus::UserCanceled => "UserCanceled",
        KeyCredentialStatus::UserPrefersPassword => "UserPrefersPassword",
        KeyCredentialStatus::CredentialAlreadyExists => "CredentialAlreadyExists",
        KeyCredentialStatus::SecurityDeviceLocked => "SecurityDeviceLocked",
        _ => "an unknown status",
    };
    let kind = match status {
        KeyCredentialStatus::UserCanceled | KeyCredentialStatus::UserPrefersPassword => {
            io::ErrorKind::Interrupted
        }
        KeyCredentialStatus::NotFound => io::ErrorKind::NotFound,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, format!("Windows Hello {what} ended with {name}"))
}

/// Starts `RequestCreateForWindowAsync(hwnd, credential, ReplaceExisting)`: the Windows Hello
/// prompt owned by `hwnd`.
pub fn start_create_for_window(hwnd: isize, credential: &str) -> io::Result<PendingCreate> {
    let statics = windows::core::factory::<
        KeyCredentialManager,
        IKeyCredentialManagerCreateWithWindowStatics,
    >()
    .map_err(io_err)?;
    let name = HSTRING::from(credential);
    let mut raw = std::ptr::null_mut();
    // SAFETY: the vtable matches the interface (see its impl); `name` outlives the call, which
    // borrows the HSTRING's ABI handle without taking it; `raw` receives an owned pointer.
    let op = unsafe {
        let hr = (statics.vtable().request_create_for_window_async)(
            statics.as_raw(),
            WindowId::of(hwnd),
            std::mem::transmute_copy(&name),
            KeyCredentialCreationOption::ReplaceExisting,
            &mut raw,
        );
        async_op(hr, raw)?
    };
    Ok(Pending { op })
}

/// Waits for a started create.
pub fn finish_create(pending: PendingCreate) -> io::Result<()> {
    let result = pending.op.join().map_err(io_err)?;
    match result.Status().map_err(io_err)? {
        KeyCredentialStatus::Success => Ok(()),
        other => Err(status_error("create", other)),
    }
}

/// Opens `credential` without a prompt (`OpenAsync`).
fn open(credential: &str) -> io::Result<KeyCredential> {
    let result = KeyCredentialManager::OpenAsync(&HSTRING::from(credential))
        .and_then(|op| op.join())
        .map_err(io_err)?;
    match result.Status().map_err(io_err)? {
        KeyCredentialStatus::Success => result.Credential().map_err(io_err),
        other => Err(status_error("open", other)),
    }
}

/// Starts `RequestSignForWindowAsync(hwnd, data)` on `credential`: the Windows Hello prompt
/// owned by `hwnd`.
pub fn start_sign_for_window(
    hwnd: isize,
    credential: &str,
    data: &[u8],
) -> io::Result<PendingSign> {
    let with_window: IKeyCredentialWithWindow = open(credential)?.cast().map_err(io_err)?;
    let buffer = CryptographicBuffer::CreateFromByteArray(data).map_err(io_err)?;
    let mut raw = std::ptr::null_mut();
    // SAFETY: the vtable matches the interface; `buffer` outlives the call, which borrows it;
    // `raw` receives an owned pointer.
    let op = unsafe {
        let hr = (with_window.vtable().request_sign_for_window_async)(
            with_window.as_raw(),
            WindowId::of(hwnd),
            buffer.as_raw(),
            &mut raw,
        );
        async_op(hr, raw)?
    };
    Ok(Pending { op })
}

/// Waits for a started sign and returns the signature.
pub fn finish_sign(pending: PendingSign) -> io::Result<Zeroizing<Vec<u8>>> {
    let result = pending.op.join().map_err(io_err)?;
    match result.Status().map_err(io_err)? {
        KeyCredentialStatus::Success => {}
        other => return Err(status_error("sign", other)),
    }
    let mut bytes = windows::core::Array::<u8>::new();
    CryptographicBuffer::CopyToByteArray(&result.Result().map_err(io_err)?, &mut bytes)
        .map_err(io_err)?;
    let signature = Zeroizing::new(bytes.to_vec());
    // The `Array`'s own copy is freed by Windows' allocator when it drops; overwrite it first.
    for b in bytes.iter_mut() {
        *b = 0;
    }
    Ok(signature)
}

/// Starts `IUserConsentVerifierInterop::RequestVerificationForWindowAsync(hwnd, message)`: the
/// DPAPI route's gate, owned by `hwnd`. Kept only for the route probe; LILITH doesn't use it.
pub fn start_consent_for_window(hwnd: isize, message: &str) -> io::Result<PendingConsent> {
    let interop = windows::core::factory::<UserConsentVerifier, IUserConsentVerifierInterop>()
        .map_err(io_err)?;
    // SAFETY: a valid interop factory, a window handle and a message that outlives the call.
    let op = unsafe {
        interop.RequestVerificationForWindowAsync::<IAsyncOperation<UserConsentVerificationResult>>(
            HWND(hwnd as *mut c_void),
            &HSTRING::from(message),
        )
    }
    .map_err(io_err)?;
    Ok(Pending { op })
}

/// Waits for a started consent check: `true` only for `Verified`.
pub fn finish_consent(pending: PendingConsent) -> io::Result<bool> {
    let result = pending.op.join().map_err(io_err)?;
    Ok(result == UserConsentVerificationResult::Verified)
}

/// How long a Windows Hello prompt waits for you before it is cancelled (QA-088): a prompt you
/// can't see must not keep `lil` waiting, nor be answered hours later.
pub const HELLO_WAIT: Duration = Duration::from_secs(120);

/// How often [`wait_bounded`] looks.
const HELLO_POLL: Duration = Duration::from_millis(100);

/// Waits until `finished()` says so, for at most `timeout`; past that it calls `cancel()` (which
/// closes the prompt) and is a `TimedOut` error. `pause` is `std::thread::sleep` for real.
pub fn wait_bounded(
    finished: &dyn Fn() -> bool,
    cancel: &dyn Fn(),
    timeout: Duration,
    pause: &dyn Fn(Duration),
) -> io::Result<()> {
    let mut waited = Duration::ZERO;
    while !finished() {
        if waited >= timeout {
            cancel();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "the Windows Hello prompt wasn't answered within {} s, so it was cancelled",
                    timeout.as_secs()
                ),
            ));
        }
        pause(HELLO_POLL);
        waited += HELLO_POLL;
    }
    Ok(())
}

impl<T: windows::core::RuntimeType + 'static> Pending<T> {
    /// [`wait_bounded`] on this call, for at most [`HELLO_WAIT`].
    fn wait(&self) -> io::Result<()> {
        wait_bounded(
            &|| self.finished(),
            &|| self.cancel(),
            HELLO_WAIT,
            &std::thread::sleep,
        )
    }
}

/// Creates (or replaces) the Windows Hello key credential `credential`, the prompt owned by
/// `hwnd` (KEK route enrolment). Cancelled after [`HELLO_WAIT`].
pub fn kek_enroll_for_window(hwnd: isize, credential: &str) -> io::Result<()> {
    let pending = start_create_for_window(hwnd, credential)?;
    pending.wait()?;
    finish_create(pending)
}

/// Signs `challenge` with `credential`, the prompt owned by `hwnd`, and returns the KEK
/// ([`kek_from_signature`]). Cancelled after [`HELLO_WAIT`].
pub fn kek_derive_for_window(
    hwnd: isize,
    credential: &str,
    challenge: &[u8; 32],
) -> io::Result<Zeroizing<[u8; 32]>> {
    let pending = start_sign_for_window(hwnd, credential, challenge)?;
    pending.wait()?;
    let signature = finish_sign(pending)?;
    Ok(kek_from_signature(&signature))
}

/// QA-088 (Task 28 Phase B ruling): Windows Hello is raised only once its window is in front.
/// When Windows kept another window in front, the window's taskbar button flashes
/// ([`OwnerWindow::bring_to_front`]) and `brought_forward` waits, within [`HELLO_WAIT`], for you
/// to bring it forward; it never falls back to the passphrase. Not brought forward in time is a
/// `TimedOut` error. Pure over its closure.
pub fn front_or_wait(raise: Raise, brought_forward: impl FnOnce() -> bool) -> io::Result<()> {
    match raise {
        Raise::AlreadyInFront | Raise::SetForeground | Raise::AttachedInput => Ok(()),
        Raise::NotInFront if brought_forward() => Ok(()),
        Raise::NotInFront => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "the LILITH window wasn't brought forward within {} s (its taskbar button flashes), so Windows Hello wasn't asked",
                HELLO_WAIT.as_secs()
            ),
        )),
    }
}

/// Waits until `ready()`, for at most `timeout`; whether it got there. Pure over its closures.
pub fn wait_until(ready: &dyn Fn() -> bool, timeout: Duration, pause: &dyn Fn(Duration)) -> bool {
    let mut waited = Duration::ZERO;
    loop {
        if ready() {
            return true;
        }
        if waited >= timeout {
            return false;
        }
        pause(HELLO_POLL);
        waited += HELLO_POLL;
    }
}

/// What you chose in an [`OwnerWindow`] that asks (Task 28 fix round 2): Windows Hello Face can
/// complete a prompt with no deliberate act, so a merge's or an approval's prompt is raised
/// only after you clicked its accept button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// The accept button ("Merge", "Approve").
    Accepted,
    /// "Deny".
    Denied,
    /// The window was closed.
    Closed,
    /// Nothing was chosen within [`HELLO_WAIT`].
    TimedOut,
}

/// Waits until `read()` gives a choice, for at most `timeout`. Pure over its closures.
pub fn wait_for_choice(
    read: &dyn Fn() -> Option<Choice>,
    timeout: Duration,
    pause: &dyn Fn(Duration),
) -> Choice {
    let mut waited = Duration::ZERO;
    loop {
        if let Some(choice) = read() {
            return choice;
        }
        if waited >= timeout {
            return Choice::TimedOut;
        }
        pause(HELLO_POLL);
        waited += HELLO_POLL;
    }
}

/// Only [`Choice::Accepted`] goes on; anything else refuses with `PermissionDenied`, which
/// the caller treats as a refusal, not as a reason to ask for the passphrase. Pure.
pub fn choice_or_refuse(choice: Choice) -> io::Result<()> {
    let why = match choice {
        Choice::Accepted => return Ok(()),
        Choice::Denied => "you chose Deny",
        Choice::Closed => "the LILITH window was closed",
        Choice::TimedOut => "nothing was chosen in the LILITH window in time",
    };
    Err(io::Error::new(io::ErrorKind::PermissionDenied, why))
}

/// The order every asking prompt keeps (pure): your choice ([`choice_or_refuse`]), and only then
/// `call`, which raises Windows Hello. The window doesn't have to be in front when it is asked:
/// its buttons work only once it has been in front for [`BUTTONS_LIVE_AFTER`] ([`LiveClock`]),
/// so `choose` waits, the taskbar button flashing, until you bring it forward, choose, or
/// [`HELLO_WAIT`] passes (`TimedOut`, a refusal; never the passphrase).
pub fn ask_then<T>(
    choose: impl FnOnce() -> Choice,
    call: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    choice_or_refuse(choose())?;
    call()
}

/// Opens an [`OwnerWindow`] showing `shown` and tries to bring it to the front. With `accept`,
/// it has that button and "Deny", and `call` runs only after you clicked `accept`
/// ([`ask_then`]); without, `call` runs once the window is in front ([`front_or_wait`]).
fn in_front<T>(
    shown: &str,
    accept: Option<&str>,
    call: impl FnOnce(isize) -> io::Result<T>,
) -> io::Result<T> {
    let window = match accept {
        Some(label) => OwnerWindow::open_asking("LILITH: Windows Hello", shown, label)?,
        None => OwnerWindow::open("LILITH: Windows Hello", shown)?,
    };
    run_in(&window, accept.is_some(), call)
}

/// What [`run_in`] needs of a window: [`OwnerWindow`] for real, a fake in tests.
trait PromptWindow {
    fn raise(&self) -> Raise;
    fn wait_choice(&self, timeout: Duration) -> Choice;
    /// Waits until the window is the foreground window, for at most `timeout`.
    fn wait_front(&self, timeout: Duration) -> bool;
    fn hwnd(&self) -> isize;
}

impl PromptWindow for OwnerWindow {
    fn raise(&self) -> Raise {
        self.bring_to_front()
    }
    fn wait_choice(&self, timeout: Duration) -> Choice {
        OwnerWindow::wait_choice(self, timeout)
    }
    fn wait_front(&self, timeout: Duration) -> bool {
        OwnerWindow::wait_front(self, timeout)
    }
    fn hwnd(&self) -> isize {
        OwnerWindow::hwnd(self)
    }
}

/// [`in_front`]'s order, over any window (re-review m6): try to raise it (it flashes when
/// Windows refuses); when `asking`, wait for the choice ([`ask_then`]), otherwise for the window
/// to be in front ([`front_or_wait`]); run `call` with its handle only then. All within
/// [`HELLO_WAIT`].
fn run_in<W: PromptWindow, T>(
    window: &W,
    asking: bool,
    call: impl FnOnce(isize) -> io::Result<T>,
) -> io::Result<T> {
    let raise = window.raise();
    if asking {
        ask_then(|| window.wait_choice(HELLO_WAIT), || call(window.hwnd()))
    } else {
        front_or_wait(raise, || window.wait_front(HELLO_WAIT))?;
        call(window.hwnd())
    }
}

/// [`kek_enroll_for_window`], owned by an [`OwnerWindow`] in front that shows `shown`.
pub fn kek_enroll(credential: &str, shown: &str) -> io::Result<()> {
    in_front(shown, None, |hwnd| kek_enroll_for_window(hwnd, credential))
}

/// [`kek_derive_for_window`], owned by an [`OwnerWindow`] in front that shows `shown` (for a
/// merge, the summary and the audit, P4a-R38). With `accept` ("Merge", "Approve"), Windows
/// Hello is asked only after you clicked it; "Deny", closing the window or no choice within
/// [`HELLO_WAIT`] refuses (`PermissionDenied`). `ready` runs right before Windows Hello is
/// raised, after the click (Phase B review m2: the caller's face check, not up to 120 s
/// earlier); its error stops the call and is returned as it is.
pub fn kek_derive(
    credential: &str,
    challenge: &[u8; 32],
    shown: &str,
    accept: Option<&str>,
    ready: &dyn Fn() -> io::Result<()>,
) -> io::Result<Zeroizing<[u8; 32]>> {
    in_front(shown, accept, |hwnd| {
        ready()?;
        kek_derive_for_window(hwnd, credential, challenge)
    })
}

/// Whether the key credential `credential` exists (`OpenAsync`). No prompt.
pub fn credential_exists(credential: &str) -> io::Result<bool> {
    match open(credential) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Deletes the key credential `credential` (`DeleteAsync`). No prompt. A credential that isn't
/// there is not an error.
pub fn kek_remove(credential: &str) -> io::Result<()> {
    if !credential_exists(credential)? {
        return Ok(());
    }
    KeyCredentialManager::DeleteAsync(&HSTRING::from(credential))
        .and_then(|op| op.join())
        .map_err(io_err)
}

// ---------------------------------------------------------------------------------------------
// Face sign-in (owner decision 2026-10-24: Windows Hello unlocks merges only without it).
// ---------------------------------------------------------------------------------------------

/// Whether Windows Hello face recognition is enrolled for the user running this process
/// ([`face_sign_in`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FaceSignIn {
    /// No face camera Windows Hello can use, or none has a face enrolled for this user.
    Off,
    /// A face is enrolled for this user on a Windows Hello face camera.
    On,
    /// It couldn't be told; why.
    Unknown(String),
}

/// What one face camera (a facial-features biometric unit in the system pool) says about this
/// user's enrollments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FaceUnit {
    /// `WinBioEnumEnrollments` succeeded with this many enrolled sub-factors.
    Enrolled(usize),
    /// `WINBIO_E_UNKNOWN_ID`: this user has no template on the unit.
    NotEnrolled,
    /// Any other failure, with the call and the error.
    Failed(String),
}

/// What the face check read ([`face_sign_in_from`]'s input).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaceReading {
    /// Each system-pool face camera's answer for this user.
    pub face_units: Result<Vec<FaceUnit>, String>,
    /// The device instance ids of every unit WinBio lists to this program: face cameras in any
    /// pool, and fingerprint readers.
    pub listed_devices: Result<Vec<String>, String>,
    /// The device instance ids of the present devices of the Biometric device class.
    pub present_devices: Result<Vec<String>, String>,
}

/// The verdict (pure). Any enrollment on a face camera: on. A face camera that failed: unknown.
/// A present biometric device that WinBio doesn't list to this program could be a face camera
/// it hides (Enhanced Sign-in Security does), so unknown (Phase B review m1); a device WinBio
/// lists as a fingerprint reader is accounted for and doesn't count (m3). Any read that failed:
/// unknown. Otherwise off. A face camera that is unplugged when this runs isn't present, so it
/// reads off: the check runs again right before each Windows Hello prompt (m2).
pub fn face_sign_in_from(reading: FaceReading) -> FaceSignIn {
    let units = match reading.face_units {
        Ok(units) => units,
        Err(why) => return FaceSignIn::Unknown(why),
    };
    if units
        .iter()
        .any(|u| matches!(u, FaceUnit::Enrolled(n) if *n > 0))
    {
        return FaceSignIn::On;
    }
    if let Some(why) = units.iter().find_map(|u| match u {
        FaceUnit::Failed(why) => Some(why.clone()),
        _ => None,
    }) {
        return FaceSignIn::Unknown(why);
    }
    let present = match reading.present_devices {
        Ok(present) => present,
        Err(why) => return FaceSignIn::Unknown(why),
    };
    if present.is_empty() {
        return FaceSignIn::Off;
    }
    let listed = match reading.listed_devices {
        Ok(listed) => listed,
        Err(why) => return FaceSignIn::Unknown(why),
    };
    let unlisted = present
        .iter()
        .filter(|device| !listed.iter().any(|l| l.eq_ignore_ascii_case(device)))
        .count();
    if unlisted > 0 {
        return FaceSignIn::Unknown(format!(
            "{unlisted} biometric device(s) that WinBio doesn't list to LILITH could be a face camera (Enhanced Sign-in Security can hide one)"
        ));
    }
    FaceSignIn::Off
}

/// `WINBIO_TYPE_FACIAL_FEATURES` (winbio_types.h); not in `windows` 0.62.2.
const WINBIO_TYPE_FACIAL_FEATURES: u32 = 0x0000_0002;
/// `WINBIO_TYPE_FINGERPRINT` (winbio_types.h).
const WINBIO_TYPE_FINGERPRINT: u32 = 0x0000_0008;
/// `WINBIO_FLAG_DEFAULT` (winbio_types.h).
const WINBIO_FLAG_DEFAULT: u32 = 0;
/// `WINBIO_ID_TYPE_SID` (winbio_types.h).
const WINBIO_ID_TYPE_SID: u32 = 3;

/// How long the face check may take (Phase B review m5): its WinBio calls are synchronous calls
/// to the biometric service, which can be busy or starting.
pub const FACE_CHECK_LIMIT: Duration = Duration::from_secs(5);

/// Whether Windows Hello face recognition is enrolled for this user, read-only:
/// - `WinBioEnumBiometricUnits(WINBIO_TYPE_FACIAL_FEATURES)` lists the face cameras; for each
///   one in the system pool (the pool Windows Hello sign-in uses), `WinBioEnumEnrollments` in a
///   system-pool session asks for this user's enrollments, the user named by the SID of this
///   process's token;
/// - `WinBioEnumBiometricUnits(WINBIO_TYPE_FINGERPRINT)` and the face cameras give the devices
///   WinBio lists, and `SetupDiGetClassDevsW` for the Biometric device class (present devices)
///   gives the devices there are ([`face_sign_in_from`]).
///
/// No camera is turned on, nothing is captured, no prompt is shown and nothing is changed.
/// Relied on: that Windows Hello Face keeps its templates in the system pool under the user's
/// SID, that `WINBIO_E_UNKNOWN_ID` means none, and that a unit's `DeviceInstanceId` is its
/// device's instance id. It runs on its own thread for at most [`FACE_CHECK_LIMIT`]; past that,
/// and on any failure, it is [`FaceSignIn::Unknown`].
pub fn face_sign_in() -> FaceSignIn {
    within(FACE_CHECK_LIMIT, || face_sign_in_from(read_face())).unwrap_or_else(|| {
        FaceSignIn::Unknown(format!(
            "the biometric service didn't answer within {} s",
            FACE_CHECK_LIMIT.as_secs()
        ))
    })
}

/// `f`'s result when it finishes within `limit` on a thread of its own, else `None` (the
/// thread is left to finish by itself).
pub fn within<T: Send + 'static>(
    limit: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("face-check".into())
        .spawn(move || {
            let _ = tx.send(f());
        })
        .ok()?;
    rx.recv_timeout(limit).ok()
}

fn read_face() -> FaceReading {
    let (face_units, face_devices) = match face_units() {
        Ok((units, devices)) => (Ok(units), Ok(devices)),
        Err(why) => (Err(why.clone()), Err(why)),
    };
    let listed_devices = face_devices.and_then(|mut devices| {
        devices.extend(unit_devices(WINBIO_TYPE_FINGERPRINT)?);
        Ok(devices)
    });
    FaceReading {
        face_units,
        listed_devices,
        present_devices: biometric_devices(),
    }
}

/// A unit schema's device instance id.
fn instance_id(unit: &WINBIO_UNIT_SCHEMA) -> String {
    let end = unit
        .DeviceInstanceId
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(unit.DeviceInstanceId.len());
    String::from_utf16_lossy(&unit.DeviceInstanceId[..end])
}

/// The units WinBio lists for `factor`, copied out of WinBio's buffer.
fn list_units(factor: u32) -> Result<Vec<WINBIO_UNIT_SCHEMA>, String> {
    // SAFETY: the out-pointers are valid; WinBio's buffer is read within its reported length
    // and freed once with `WinBioFree`.
    unsafe {
        let mut schema: *mut WINBIO_UNIT_SCHEMA = std::ptr::null_mut();
        let mut count = 0usize;
        WinBioEnumBiometricUnits(factor, &mut schema, &mut count)
            .map_err(|e| format!("WinBioEnumBiometricUnits({factor:#x}): {e}"))?;
        if schema.is_null() {
            return Ok(Vec::new());
        }
        let units = std::slice::from_raw_parts(schema, count).to_vec();
        let _ = WinBioFree(schema.cast());
        Ok(units)
    }
}

/// The device instance ids of the units WinBio lists for `factor`.
fn unit_devices(factor: u32) -> Result<Vec<String>, String> {
    Ok(list_units(factor)?.iter().map(instance_id).collect())
}

/// The device instance ids of the present devices of the Biometric class (read-only device
/// enumeration).
fn biometric_devices() -> Result<Vec<String>, String> {
    // SAFETY: a device list for one class, read member by member and destroyed once; each
    // instance id is read into a local buffer.
    unsafe {
        let set = SetupDiGetClassDevsW(
            Some(&GUID_DEVCLASS_BIOMETRIC),
            windows::core::PCWSTR::null(),
            None,
            DIGCF_PRESENT,
        )
        .map_err(|e| format!("SetupDiGetClassDevsW: {e}"))?;
        let mut devices = Vec::new();
        let mut result = Ok(());
        for index in 0.. {
            let mut data = SP_DEVINFO_DATA {
                cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
                ..Default::default()
            };
            match SetupDiEnumDeviceInfo(set, index, &mut data) {
                Ok(()) => {}
                Err(e) if e.code() == ERROR_NO_MORE_ITEMS.to_hresult() => break,
                Err(e) => {
                    result = Err(format!("SetupDiEnumDeviceInfo: {e}"));
                    break;
                }
            }
            let mut buf = [0u16; 512];
            let mut needed = 0u32;
            if let Err(e) =
                SetupDiGetDeviceInstanceIdW(set, &data, Some(&mut buf), Some(&mut needed))
            {
                result = Err(format!("SetupDiGetDeviceInstanceIdW: {e}"));
                break;
            }
            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            devices.push(String::from_utf16_lossy(&buf[..end]));
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
        result.map(|()| devices)
    }
}

/// Closes a WinBio session on every exit path.
struct WinBioSession(u32);

impl Drop for WinBioSession {
    fn drop(&mut self) {
        // SAFETY: the handle came from `WinBioOpenSession` and is closed only here.
        let _ = unsafe { WinBioCloseSession(self.0) };
    }
}

/// This process's user as a WinBio identity (`WINBIO_ID_TYPE_SID`).
fn current_identity() -> Result<WINBIO_IDENTITY, String> {
    let sid = crate::sid::current_user_sid().map_err(|e| format!("the user's SID: {e}"))?;
    let mut psid = PSID::default();
    // SAFETY: `sid` outlives the call; the SID allocated by `ConvertStringSidToSidW` is copied
    // into the identity and freed once with `LocalFree`.
    unsafe {
        ConvertStringSidToSidW(&HSTRING::from(sid), &mut psid)
            .map_err(|e| format!("ConvertStringSidToSidW: {e}"))?;
        let len = GetLengthSid(psid) as usize;
        let mut identity = WINBIO_IDENTITY {
            Type: WINBIO_ID_TYPE_SID,
            ..Default::default()
        };
        let copied = if len <= identity.Value.AccountSid.Data.len() {
            let bytes = std::slice::from_raw_parts(psid.0.cast::<u8>(), len);
            identity.Value.AccountSid.Data[..len].copy_from_slice(bytes);
            identity.Value.AccountSid.Size = len as u32;
            Ok(identity)
        } else {
            Err(format!(
                "the user's SID is {len} bytes, too long for WinBio"
            ))
        };
        let _ = LocalFree(Some(HLOCAL(psid.0)));
        copied
    }
}

/// Each system-pool face camera's answer for this user, and the device instance ids of every
/// face camera WinBio lists (any pool).
fn face_units() -> Result<(Vec<FaceUnit>, Vec<String>), String> {
    let all = list_units(WINBIO_TYPE_FACIAL_FEATURES)?;
    let devices = all.iter().map(instance_id).collect();
    let units: Vec<u32> = all
        .iter()
        .filter(|u| u.PoolType == WINBIO_POOL_SYSTEM.0)
        .map(|u| u.UnitId)
        .collect();
    if units.is_empty() {
        return Ok((Vec::new(), devices));
    }
    let identity = current_identity()?;
    // SAFETY: WinBio calls with valid out-pointers; each subfactor buffer is freed once with
    // `WinBioFree`, and the session is closed by `WinBioSession`.
    unsafe {
        let session = WinBioSession(
            WinBioOpenSession(
                WINBIO_TYPE_FACIAL_FEATURES,
                WINBIO_POOL_SYSTEM,
                WINBIO_FLAG_DEFAULT,
                None,
                None,
            )
            .map_err(|e| format!("WinBioOpenSession: {e}"))?,
        );
        let answers = units
            .into_iter()
            .map(|unit| {
                let mut subfactors: *mut u8 = std::ptr::null_mut();
                let mut n = 0usize;
                let result = WinBioEnumEnrollments(
                    session.0,
                    unit,
                    &identity,
                    &mut subfactors,
                    Some(&mut n),
                );
                if !subfactors.is_null() {
                    let _ = WinBioFree(subfactors.cast());
                }
                match result {
                    Ok(()) => FaceUnit::Enrolled(n),
                    Err(e) if e.code() == WINBIO_E_UNKNOWN_ID => FaceUnit::NotEnrolled,
                    Err(e) => {
                        FaceUnit::Failed(format!("WinBioEnumEnrollments on unit {unit}: {e}"))
                    }
                }
            })
            .collect();
        Ok((answers, devices))
    }
}

// ---------------------------------------------------------------------------------------------
// DPAPI: kept only for the route probe's `dry` check; LILITH doesn't use it.
// ---------------------------------------------------------------------------------------------

const DPAPI_DESCRIPTION: &str = "LILITH reviewer key";

/// `CryptProtectData` for the current user, never showing UI.
pub fn dpapi_protect(data: &[u8]) -> io::Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(data.len()).map_err(|_| io::Error::other("too much data"))?,
        pbData: data.as_ptr().cast_mut(),
    };
    let mut sealed = CRYPT_INTEGER_BLOB::default();
    // SAFETY: `input` points into `data`, which outlives the call and is only read; `sealed` is
    // filled by the API with a LocalAlloc'd buffer that is copied and freed here.
    unsafe {
        CryptProtectData(
            &input,
            &HSTRING::from(DPAPI_DESCRIPTION),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut sealed,
        )
        .map_err(io_err)?;
        let out = std::slice::from_raw_parts(sealed.pbData, sealed.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(sealed.pbData.cast())));
        Ok(out)
    }
}

/// `CryptUnprotectData` for the current user, never showing UI. The plaintext's API buffer is
/// zeroed before it is freed.
pub fn dpapi_unprotect(blob: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(blob.len()).map_err(|_| io::Error::other("too much data"))?,
        pbData: blob.as_ptr().cast_mut(),
    };
    let mut opened = CRYPT_INTEGER_BLOB::default();
    // SAFETY: as in `dpapi_protect`; the output buffer is zeroed, then freed.
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut opened,
        )
        .map_err(io_err)?;
        let plain = std::slice::from_raw_parts_mut(opened.pbData, opened.cbData as usize);
        let out = Zeroizing::new(plain.to_vec());
        plain.fill(0);
        let _ = LocalFree(Some(HLOCAL(opened.pbData.cast())));
        Ok(out)
    }
}

// ---------------------------------------------------------------------------------------------
// The owner window.
// ---------------------------------------------------------------------------------------------

/// How [`OwnerWindow::bring_to_front`] got the window to the front, or that it didn't.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Raise {
    /// It was already the foreground window.
    AlreadyInFront,
    /// `SetForegroundWindow` alone.
    SetForeground,
    /// `SetForegroundWindow` with this thread's input attached to the foreground window's.
    AttachedInput,
    /// Windows kept another window in front; the taskbar button flashes instead.
    NotInFront,
}

impl Raise {
    /// A stable name, for records.
    pub fn name(self) -> &'static str {
        match self {
            Raise::AlreadyInFront => "already_in_front",
            Raise::SetForeground => "set_foreground",
            Raise::AttachedInput => "attached_input",
            Raise::NotInFront => "not_in_front",
        }
    }
}

/// A plain top-level window that owns a Windows Hello prompt and shows `body` (read-only),
/// optionally with an accept button and "Deny" ([`OwnerWindow::open_asking`]). It runs on its
/// own thread with a message loop, so it stays responsive while the caller waits; it closes
/// when this is dropped.
pub struct OwnerWindow {
    hwnd: isize,
    thread: Option<JoinHandle<()>>,
}

impl OwnerWindow {
    /// Opens the window, shown but not yet brought to the front.
    pub fn open(title: &str, body: &str) -> io::Result<Self> {
        Self::open_with(title, body, None)
    }

    /// The same window with two buttons at the bottom, `accept` and "Deny"
    /// ([`OwnerWindow::wait_choice`]).
    pub fn open_asking(title: &str, body: &str, accept: &str) -> io::Result<Self> {
        Self::open_with(title, body, Some(accept))
    }

    fn open_with(title: &str, body: &str, accept: Option<&str>) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel::<io::Result<isize>>();
        let title = HSTRING::from(title);
        // An EDIT control wants CRLF line ends.
        let body = HSTRING::from(body.replace("\r\n", "\n").replace('\n', "\r\n"));
        let accept = accept.map(HSTRING::from);
        let thread = std::thread::Builder::new()
            .name("hello-owner-window".into())
            .spawn(move || window_thread(&title, &body, accept.as_ref(), &tx))?;
        match rx.recv() {
            Ok(Ok(hwnd)) => Ok(Self {
                hwnd,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(io::Error::other("the owner window's thread ended early"))
            }
        }
    }

    /// The window handle, for the `*_for_window` calls.
    pub fn hwnd(&self) -> isize {
        self.hwnd
    }

    /// Waits for a click on the accept button or "Deny", or the window's closing, for at most
    /// `timeout` ([`wait_for_choice`]).
    pub fn wait_choice(&self, timeout: Duration) -> Choice {
        wait_for_choice(&|| choice_of(self.hwnd), timeout, &std::thread::sleep)
    }

    /// Waits until the window is the foreground window (you brought it forward), for at most
    /// `timeout` ([`wait_until`]).
    pub fn wait_front(&self, timeout: Duration) -> bool {
        let hwnd = HWND(self.hwnd as *mut c_void);
        // SAFETY: a plain query.
        let in_front = || unsafe { GetForegroundWindow() } == hwnd;
        wait_until(&in_front, timeout, &std::thread::sleep)
    }

    /// Brings the window to the front without any synthetic input: `SetForegroundWindow`, then
    /// once more with this thread's input attached to the foreground thread's. When Windows
    /// still keeps another window in front, the taskbar button flashes.
    pub fn bring_to_front(&self) -> Raise {
        let hwnd = HWND(self.hwnd as *mut c_void);
        // SAFETY: plain window calls on this process's own live window; the attach is undone
        // before returning.
        unsafe {
            if GetForegroundWindow() == hwnd {
                return Raise::AlreadyInFront;
            }
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            if SetForegroundWindow(hwnd).as_bool() && GetForegroundWindow() == hwnd {
                return Raise::SetForeground;
            }
            let front = GetForegroundWindow();
            let front_thread = GetWindowThreadProcessId(front, None);
            // The calling thread's input is attached, as the route probe measures it. If the probe
            // reports `not_in_front`, the first change to try is attaching the window's own
            // thread (`GetWindowThreadProcessId(hwnd)`) instead (Task 28 review, M7).
            let mine = GetCurrentThreadId();
            if front_thread != 0 && front_thread != mine {
                let attached = AttachThreadInput(mine, front_thread, true).as_bool();
                let _ = BringWindowToTop(hwnd);
                let _ = SetForegroundWindow(hwnd);
                if attached {
                    let _ = AttachThreadInput(mine, front_thread, false);
                }
                if GetForegroundWindow() == hwnd {
                    return Raise::AttachedInput;
                }
            }
            let flash = FLASHWINFO {
                cbSize: size_of::<FLASHWINFO>() as u32,
                hwnd,
                dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
                uCount: 0,
                dwTimeout: 0,
            };
            let _ = FlashWindowEx(&flash);
        }
        Raise::NotInFront
    }
}

impl Drop for OwnerWindow {
    fn drop(&mut self) {
        // SAFETY: posting to this process's own window; a stale handle just fails.
        unsafe {
            let _ = PostMessageW(
                Some(HWND(self.hwnd as *mut c_void)),
                WM_CLOSE,
                WPARAM(0),
                LPARAM(0),
            );
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        CHOICES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(h, _)| *h != self.hwnd);
    }
}

// ---------------------------------------------------------------------------------------------
// The buttons: only a real click accepts (re-review N1, m1).
// ---------------------------------------------------------------------------------------------

/// Where the input message being handled came from (`GetCurrentInputMessageSource`'s
/// `originId`).
///
/// What it reports for a *posted* mouse message (`PostMessage(WM_LBUTTONUP)` from another
/// program) is not documented: Microsoft documents only that a message sent with `SendMessage`
/// reports unavailable until `SendMessage` returns. So nothing here relies on a posted message
/// reporting [`InputOrigin::Unavailable`]: [`arms_accept`] needs `IMO_HARDWARE` *and* a pointer
/// device, and anything else, a failed query included, refuses. Whether a posted release
/// handled while the real cursor rests over the window could inherit the last hardware
/// source is Phase B Step 4's check (re-review 2 n5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputOrigin {
    /// A real device (`IMO_HARDWARE`).
    Hardware,
    /// `SendInput` (`IMO_INJECTED`).
    Injected,
    /// Windows itself (`IMO_SYSTEM`).
    System,
    /// Not an input message (`IMO_UNAVAILABLE`, documented for sent messages), or the query
    /// failed.
    Unavailable,
}

/// The device the input message being handled came from (`GetCurrentInputMessageSource`'s
/// `deviceType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputDevice {
    /// `IMDT_MOUSE`.
    Mouse,
    /// `IMDT_TOUCH`.
    Touch,
    /// `IMDT_PEN`.
    Pen,
    /// `IMDT_KEYBOARD`.
    Keyboard,
    /// `IMDT_TOUCHPAD`.
    Touchpad,
    /// `IMDT_UNAVAILABLE`, another value, or the query failed.
    Unavailable,
}

/// A message the accept button's subclass got, as [`arms_accept`] judges it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ButtonInput {
    /// The message.
    pub msg: u32,
    /// For a mouse message: its point is inside the button.
    pub over_button: bool,
    pub origin: InputOrigin,
    pub device: InputDevice,
    /// The message was sent from another thread or process (`InSendMessage`).
    pub sent_by_another_thread: bool,
    /// A real press on the button preceded this release ([`PressTracker`]).
    pub pressed_by_hardware: bool,
}

/// Whether a message arms the accept button for the click it is about to report (pure): only a
/// `WM_LBUTTONUP` inside the button, from real hardware (`IMO_HARDWARE`) that is a mouse, touch,
/// pen or touchpad, handled on the button's own thread, after a real press on the button
/// (re-review 2 N4; the touchpad by the fix round 4 ruling; the press by re-review 3 m1). No
/// key arms it: a program can move the focus to the button without hardware input, and the
/// owner's next Space must not merge. "Deny" takes any input (its subclass only guards its
/// label); closing the window refuses too.
pub fn arms_accept(input: ButtonInput) -> bool {
    input.msg == WM_LBUTTONUP
        && input.over_button
        && is_real_pointer(input.origin, input.device, input.sent_by_another_thread)
        && input.pressed_by_hardware
}

/// A `WM_COMMAND` the owner window got, as [`choice_from_command`] judges it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ButtonCommand {
    /// The notification is `BN_CLICKED`.
    pub clicked: bool,
    /// `lParam` is the accept button's own window.
    pub from_accept: bool,
    /// `lParam` is "Deny"'s own window.
    pub from_deny: bool,
    /// The accept button was armed by the hardware input that produced this click
    /// ([`arms_accept`]).
    pub armed: bool,
    /// The command was sent from another thread or process (`InSendMessage`).
    pub sent_by_another_thread: bool,
    /// The buttons were live: the window had been in front for [`BUTTONS_LIVE_AFTER`].
    pub enabled: bool,
}

/// The choice a `WM_COMMAND` makes, if any (pure): "Deny" from its own button, whatever sent it
/// (refusing is always safe); the accept button only for a `BN_CLICKED` from its own window,
/// handled on its thread, armed by real hardware input, while the buttons were live. A posted
/// or sent `WM_COMMAND`, `BM_CLICK`, UI Automation's Invoke and `SendInput` all fail one of these.
pub fn choice_from_command(c: ButtonCommand) -> Option<Choice> {
    if !c.clicked {
        return None;
    }
    if c.from_deny {
        return Some(Choice::Denied);
    }
    (c.from_accept && c.armed && !c.sent_by_another_thread && c.enabled).then_some(Choice::Accepted)
}

/// Why a click on the accept button didn't count (Task 28b), one value per gate: the window
/// says which in its status line ([`Refusal::message`]) and logs its name, so a refusal is
/// never silent. It carries nothing about the input itself: no point, no timing, no device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The click was the one that brought the window to the front, so the buttons were grey.
    NotInFront,
    /// The window hadn't been in front for [`BUTTONS_LIVE_AFTER`] yet: the buttons were grey.
    NotLiveYet,
    /// Not a real pointer click handled on the button's own thread: injected or system input,
    /// a key, a posted or sent message, `BM_CLICK` or UI Automation ([`arms_accept`]).
    NotPointer,
    /// No real press on the button came before the release ([`PressTracker`]).
    NoRealPress,
    /// A mouse or touchpad press less than [`HOVER_BEFORE_PRESS`] after real pointer moves
    /// brought the cursor onto the button ([`PressTracker`]).
    NoRest,
    /// A mouse or touchpad press with no real pointer move onto the button since the rest was
    /// last ended: the cursor got there by a warp, by the window or the button sliding under
    /// it, or (Task 28c) under a still hand after something Windows itself reports as a move
    /// (the window coming to the top, the button turning on, a click's release), which can't
    /// be told from a warp ([`PressTracker`]).
    NoMove,
}

impl Refusal {
    /// A stable name, for the log.
    pub fn name(self) -> &'static str {
        match self {
            Refusal::NotInFront => "not_in_front",
            Refusal::NotLiveYet => "not_live_yet",
            Refusal::NotPointer => "not_pointer",
            Refusal::NoRealPress => "no_real_press",
            Refusal::NoRest => "no_rest",
            Refusal::NoMove => "no_move",
        }
    }

    /// The status line for it (pure): which gate refused and exactly what to do next, nothing
    /// else. `accept` is the button's own label ("Merge", "Approve"). A still hand can't start
    /// the rest (Task 28c), so every message that leads to another click says to move the
    /// pointer a little on the button first; that works whatever Windows reports for its own
    /// moves.
    pub fn message(self, accept: &str) -> String {
        match self {
            Refusal::NotInFront => format!(
                "That click brought this window to the front. When the buttons turn on, move the pointer a little on {accept}, then click it."
            ),
            Refusal::NotLiveYet => format!(
                "Wait a moment: the buttons turn on a second after this window comes to the front. Then move the pointer a little on {accept} and click it."
            ),
            Refusal::NotPointer => format!(
                "That click didn't come from your mouse or touchpad, so it didn't count. Click {accept} with the pointer."
            ),
            Refusal::NoRealPress => format!(
                "Press and release on {accept} itself: a press that started somewhere else doesn't count."
            ),
            Refusal::NoRest => format!(
                "That click came too soon: move the pointer a little on {accept}, pause a moment, then click it."
            ),
            Refusal::NoMove => {
                format!("Move the pointer a little on {accept}, then click it.")
            }
        }
    }
}

/// The gate that refused a `WM_COMMAND` click of the accept button, if it was one and was
/// refused (pure). Grey buttons come first; then what the button's subclass found for the
/// release that produced the click (`release`, [`release_refusal`]); with no hardware release
/// behind it (a key, a posted or sent message, `BM_CLICK`, UI Automation), the pointer.
pub fn click_refusal(c: ButtonCommand, release: Option<Refusal>) -> Option<Refusal> {
    if !c.clicked || !c.from_accept || choice_from_command(c).is_some() {
        return None;
    }
    if !c.enabled {
        return Some(Refusal::NotLiveYet);
    }
    Some(release.unwrap_or(Refusal::NotPointer))
}

/// A press on the grey accept button reaches the window itself, not the button (pure):
/// `brought_forward` when that press is what brought the window to the front.
pub fn grey_click_refusal(brought_forward: bool) -> Refusal {
    if brought_forward {
        Refusal::NotInFront
    } else {
        Refusal::NotLiveYet
    }
}

/// How long the window must be in front before its buttons work (re-review m1): a click meant
/// for whatever was in front a moment ago doesn't count. Moving or resizing the window starts
/// it again (re-review 2 n2), so a program can't slide Merge under the cursor.
pub const BUTTONS_LIVE_AFTER: Duration = Duration::from_secs(1);

/// How often the live timer checks that the window is in front.
pub const LIVE_TICK: Duration = Duration::from_millis(250);

/// The window's time in front, in real time (pure: the caller passes `now`). The timer ticks
/// for the window's whole life until a choice is made, so the buttons go live whenever the
/// window gets to the front, however late (re-review 2 n3). A tick only samples: any program
/// can post `WM_TIMER`, so the number of ticks never counts, only the time since the first tick
/// that saw the window in front (re-review 3 I1).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LiveClock {
    /// When a tick first saw the window in front, since the last restart.
    in_front_since: Option<Instant>,
}

impl LiveClock {
    /// One tick of the timer at `now`; whether the buttons are live after it.
    pub fn tick(&mut self, in_front: bool, now: Instant) -> bool {
        if in_front {
            self.in_front_since.get_or_insert(now);
        } else {
            self.in_front_since = None;
        }
        self.is_live(now)
    }

    /// The window lost the front, moved or was resized: the delay starts again.
    pub fn restart(&mut self) {
        self.in_front_since = None;
    }

    /// Whether the window has been in front for at least [`BUTTONS_LIVE_AFTER`] at `now`.
    pub fn is_live(&self, now: Instant) -> bool {
        self.in_front_since
            .and_then(|since| now.checked_duration_since(since))
            .is_some_and(|d| d >= BUTTONS_LIVE_AFTER)
    }
}

/// What the buttons are doing, for the status line (Task 28b).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ButtonsState {
    /// Grey: the window isn't in front.
    NotInFront,
    /// Grey: in front, but not yet for [`BUTTONS_LIVE_AFTER`].
    #[default]
    TurningOn,
    /// Live.
    Live,
}

/// The buttons' state from whether the window is in front and whether [`LiveClock`] says live
/// (pure).
pub fn buttons_state(in_front: bool, live: bool) -> ButtonsState {
    match (in_front, live) {
        (_, true) => ButtonsState::Live,
        (true, false) => ButtonsState::TurningOn,
        (false, false) => ButtonsState::NotInFront,
    }
}

/// The asking window's status line (Task 28b; pure): the last refused click's message
/// ([`Refusal::message`]) until the buttons change state, otherwise what grey buttons wait for.
/// Live buttons say nothing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StatusLine {
    state: ButtonsState,
    refusal: Option<Refusal>,
}

impl StatusLine {
    /// The live timer's sample; whether the text may have changed.
    pub fn tick(&mut self, state: ButtonsState) -> bool {
        if state == self.state {
            return false;
        }
        *self = Self {
            state,
            refusal: None,
        };
        true
    }

    /// A click was refused while the buttons were in `state`.
    pub fn refuse(&mut self, refusal: Refusal, state: ButtonsState) {
        *self = Self {
            state,
            refusal: Some(refusal),
        };
    }

    /// The text, for the accept button labelled `accept`.
    pub fn text(&self, accept: &str) -> String {
        if let Some(refusal) = self.refusal {
            return refusal.message(accept);
        }
        match self.state {
            ButtonsState::NotInFront => "Bring this window to the front first (click it, or its taskbar button). The buttons turn on a second later.".to_owned(),
            ButtonsState::TurningOn => "The buttons turn on in a moment.".to_owned(),
            ButtonsState::Live => String::new(),
        }
    }
}

/// How long the cursor must rest on the accept button, by real pointer moves, before a mouse
/// or touchpad press on it counts (re-review 3 m3): a program that warps the cursor onto Merge
/// (`SetCursorPos`) doesn't make the owner's next click a click on Merge. 150 ms (plan 4a
/// ruling 2026-10-25, down from 300 ms, which refused brisk deliberate clicks): a warp's move
/// isn't a hardware one and starts the rest again, and a still hand sends no moves, so the
/// length only narrows a warp made while the hand is still moving. Real moves over the button
/// while it is grey count too ([`grey_rest_move`]). With raw input (Task 28d) the hand's raw
/// motion beyond [`RAW_JITTER_COUNTS`] ends the rest too, so the 150 ms run from its last real
/// move.
pub const HOVER_BEFORE_PRESS: Duration = Duration::from_millis(150);

/// The accept button's press state (pure: the caller passes what it judged and `now`). Only a
/// real press on the button lets its release arm it (re-review 3 m1): a posted
/// `WM_LBUTTONDOWN` gives the button its pushed state and the capture, so a hardware release
/// over it would otherwise click after a press made somewhere else.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PressTracker {
    /// When real pointer moves first had the cursor on the button, since it last left.
    hover_since: Option<Instant>,
    /// The press on the button that is down: whether it counts, or which gate refused it
    /// (Task 28b). `None`: no press since the last release or cancel.
    press: Option<Result<(), Refusal>>,
}

impl PressTracker {
    /// A `WM_MOUSEMOVE` to the button; `real` when it is a hardware pointer move inside it,
    /// handled on its own thread. Any other move starts the hover again.
    pub fn on_move(&mut self, real: bool, now: Instant) {
        if real {
            self.hover_since.get_or_insert(now);
        } else {
            self.hover_since = None;
        }
    }

    /// The cursor left the button (`WM_MOUSELEAVE`).
    pub fn on_leave(&mut self) {
        self.hover_since = None;
    }

    /// A `WM_LBUTTONDOWN` (or double-click) to the button; `real` as for [`Self::on_move`].
    /// A mouse or touchpad press counts only after [`HOVER_BEFORE_PRESS`]; touch and pen press
    /// where the contact is, not at the cursor, so they need no hover.
    /// A press with no hover at all is [`Refusal::NoMove`], one too soon [`Refusal::NoRest`].
    pub fn on_press(&mut self, real: bool, device: InputDevice, now: Instant) {
        let rested = match device {
            InputDevice::Touch | InputDevice::Pen => Ok(()),
            _ => match self.hover_since {
                None => Err(Refusal::NoMove),
                Some(since) => now
                    .checked_duration_since(since)
                    .is_some_and(|d| d >= HOVER_BEFORE_PRESS)
                    .then_some(())
                    .ok_or(Refusal::NoRest),
            },
        };
        self.press = Some(if real {
            rested
        } else {
            Err(Refusal::NoRealPress)
        });
    }

    /// The press is used up and the rest stays: the accept button's own release of the
    /// capture after a real release inside it (Task 28c). A click doesn't move the cursor.
    pub fn drop_press(&mut self) {
        self.press = None;
    }

    /// A `WM_LBUTTONUP` to the button: `Ok` when a real press preceded it, otherwise the gate
    /// that refused the press ([`Refusal::NoRealPress`] when there was none). The press is used
    /// up.
    pub fn on_release(&mut self) -> Result<(), Refusal> {
        self.press.take().unwrap_or(Err(Refusal::NoRealPress))
    }

    /// The button lost the capture, a mode was cancelled, or the delay restarted: the press and
    /// the hover are dropped.
    pub fn cancel(&mut self) {
        *self = Self::default();
    }
}

/// The gate that refused a release on the accept button, if it didn't arm it (Task 28b; pure):
/// the pointer first (hardware, a pointer device, on the button's own thread), then the press
/// (`press`, [`PressTracker::on_release`]). A release outside the button, or another message,
/// clicks nothing, so there is nothing to say.
pub fn release_refusal(input: ButtonInput, press: Result<(), Refusal>) -> Option<Refusal> {
    if arms_accept(input) || input.msg != WM_LBUTTONUP || !input.over_button {
        return None;
    }
    if !is_real_pointer(input.origin, input.device, input.sent_by_another_thread) {
        return Some(Refusal::NotPointer);
    }
    Some(press.err().unwrap_or(Refusal::NoRealPress))
}

/// Whether an input message is real pointer input handled on its window's own thread (pure):
/// `IMO_HARDWARE` from a mouse, touch, pen or touchpad, not sent from another thread.
pub fn is_real_pointer(origin: InputOrigin, device: InputDevice, sent: bool) -> bool {
    origin == InputOrigin::Hardware
        && matches!(
            device,
            InputDevice::Mouse | InputDevice::Touch | InputDevice::Pen | InputDevice::Touchpad
        )
        && !sent
}

/// Whether a `WM_MOUSEMOVE` the asking window gets counts towards the rest on the accept
/// button (plan 4a ruling 2026-10-25; pure). A grey button gets no mouse input, so the window
/// gets the moves over it: a real pointer move over the grey button counts, so a pointer parked
/// there before the buttons turn on isn't refused; any other move (elsewhere, or not real, a
/// `SetCursorPos` warp's included) starts the rest again ([`PressTracker::on_move`]; with raw
/// input, Windows' own move at the same place keeps it: [`ClickGate`]). It also
/// needs `moved` ([`cursor_moved`], Task 28b review m1): the window or the button sliding under
/// a still cursor makes Windows send a move, and that move never counts, whatever origin it
/// reports. Since Task 28c the live button's own moves are judged by the same rule, with `moved`
/// against the last move the window saw ([`ClickGate`]).
pub fn grey_rest_move(
    over_grey_accept: bool,
    moved: bool,
    origin: InputOrigin,
    device: InputDevice,
    sent: bool,
) -> bool {
    over_grey_accept && moved && is_real_pointer(origin, device, sent)
}

/// Whether the cursor's screen position `now` differs from where it was at the last restart of
/// the delay (Task 28b review m1; pure). A position that couldn't be read at the restart counts
/// as not moved.
pub fn cursor_moved(at_restart: Option<(i32, i32)>, now: (i32, i32)) -> bool {
    at_restart.is_some_and(|then| then != now)
}

/// A pointer message as the click gate judges it: where it came from, and where the cursor
/// really is (`GetCursorPos` when the message is handled, never the message's own point, which
/// a program that posts or sends it chooses: Task 28c review I1). `None` when it can't be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PointerMsg {
    pub origin: InputOrigin,
    pub device: InputDevice,
    /// Sent from another thread or process (`InSendMessage`).
    pub sent: bool,
    pub at: Option<(i32, i32)>,
    /// Where the cursor was when Windows made the message (`GetMessagePos`). For a real move
    /// it must match `at`: otherwise something moved the cursor while the move was being
    /// handled, and the move is judged as an own move at a new place (Task 28d review m1).
    pub message_at: Option<(i32, i32)>,
}

impl PointerMsg {
    fn real(self) -> bool {
        is_real_pointer(self.origin, self.device, self.sent)
    }

    /// An input message Windows itself queued (hardware, injected or its own), not one another
    /// program posted or sent: only those say where the cursor is (review I1).
    fn is_input(self) -> bool {
        !self.sent && self.origin != InputOrigin::Unavailable
    }
}

/// The asking window's click gates in one place (Task 28c; pure: the window procedure gathers
/// what Windows says and delegates every decision here, so a replay of a whole click sequence
/// tests what the window runs). It holds the live clock, the accept button's press and rest,
/// where the cursor was at the last move or restart, and whether the next left press is the one
/// that brought the window to the front.
///
/// **What counts towards the rest.** Only a real pointer move (hardware, a pointer device, not
/// sent) on the accept button that put the cursor somewhere new: its screen position differs
/// from the last move's, or from where the cursor was at the last restart of the delay. Any
/// other move there (with raw input, see below), and a move elsewhere, ends the rest. A real
/// move whose message Windows made at another place than where the cursor is when it is
/// handled is judged as an own move at a new place: a warp landed meanwhile (Task 28d review
/// m1). A hardware-reported move at the same
/// position ends nothing and counts for nothing (Task 28c): Windows makes such moves itself
/// under a still cursor (the window coming to the top, a capture released, and by themselves
/// whenever a window anywhere changes), and what it reports for them is undocumented, so they
/// never start a rest. That keeps a slide, and a warp followed by Windows' own move, from
/// counting even if that move reports hardware, as far as this window knows where the cursor
/// was (below): after a leave, or with no leave armed, it can't know, so the position is then
/// unknown.
///
/// **Measured on Windows build 26300, 2026-10-25: warps, Windows' own moves and the merged case
/// report `IMO_SYSTEM`; warps make no raw input** (the pointer-source probe, Task 28c Phase B).
/// A resting mouse made no raw input either, and a real hand makes `WM_INPUT` with a device
/// handle. Turning Merge on under the pointer sends no move of its own.
///
/// **Raw input** (Task 28d). The window registers for raw mouse input (`RIDEV_INPUTSINK`), so
/// it knows when the hand moves the mouse:
/// - raw motion is added up since the last real move (every packet's `|dx| + |dy|`, from a
///   device or with a null `hDevice`; a new absolute position counts as beyond the bound).
///   Beyond [`RAW_JITTER_COUNTS`] the hand moved: the rest ends, and the next real move starts
///   it again. Up to it is a hand settling on the mouse, or a press jolting it (round 1);
/// - an input move that isn't hardware, at the position last recorded and with no more than
///   that jitter since the last real move, is Windows' own move under a still hand: it keeps
///   the rest (it never starts one);
/// - that keep is on only while the last real move that put the cursor somewhere new came with
///   device raw motion since the previous such move or restart, so the device that made the
///   rest is known to make raw input. It is judged afresh at each such move, never kept for
///   the window: a rest made by a device that sends no raw input (some touchpads, a pen) gets
///   Task 28c's rule (round 1, review I1). "The last real move" here and below means the last
///   one that put the cursor somewhere new: a real move at the same place changes nothing;
/// - an own move anywhere else still ends the rest, and so does one after the hand moved:
///   "moved away for real, then warped back to the very same pixel", which arrives as one own
///   move at the old place when Windows merges the moves or this window never saw them, stays
///   refused.
///
/// Without raw input (registration failed, or another registration in this process took it,
/// checked before each move is judged) every move that isn't hardware ends the rest, even at
/// the same position, as in Task 28c.
///
/// **Known limits** (Task 28d round 1). A still click after a click on grey Merge, or after
/// Merge turns on under the pointer, still needs a small move first: the press on the window
/// (re-review N2) and the window's leave (review I2) forget where the cursor was, so there is
/// nothing to compare Windows' own move with. The status line says to move.
///
/// **Documented residuals** (Task 28d rounds 1 and 2, not measured):
/// - that a `WM_INPUT` always comes before a later `WM_LBUTTONDOWN` of the same hand, and what
///   a real click's own raw packet carries (the probe allowed no clicks);
/// - that a mouse's raw input comes before its own move, as in the two recorded motions (raw
///   at 457.116 ms, its move at 457.316 ms). If a mouse's last packet came after its move, the
///   counts would land after the reset and end the rest with no real move left: the still
///   click is refused (the safe side, but no better than Task 28c for that mouse);
///   `replay_the_hands_raw_after_its_move_is_refused` pins it;
/// - whether a `WM_INPUT` can be dropped or delayed relative to mouse messages while another
///   program floods this thread's posted queue.
///
/// The PIN at the Windows Hello prompt is still the real gate.
///
/// **Open under Hardware** (re-review N1, a documented residual, not seen on build 26300): a
/// warp that Windows merges with one of its own moves arrives as a single move at a new place;
/// if that reported hardware it would count, and no `GetCursorPos` rule can tell it from the
/// hand. Raw input could close it by also requiring device raw motion before a real move
/// counts (with the per-rest confirmation above); that isn't done, because build 26300 never
/// reports Hardware here. The test
/// `replay_a_warp_merged_with_windows_own_move_is_open_under_hardware` pins it in case a later
/// build changes what Windows reports. Every unlock still needs the owner's PIN at the Windows
/// Hello prompt.
///
/// **Where the cursor was** (Task 28c review I1, I2). The position is `GetCursorPos`, recorded
/// only from input messages, never from a posted or sent move. After a `WM_MOUSELEAVE`, and
/// while `TME_LEAVE` isn't armed on the window or the button that got the move, it is unknown:
/// the cursor may have moved where this window never saw it, so the first move back only
/// records it. The procedures arm `TME_LEAVE` on every move they get.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ClickGate {
    live: LiveClock,
    press: PressTracker,
    /// The cursor's screen position at the last move this window saw, or at the last restart
    /// (Task 28b review m1). `None` when it couldn't be read: then no move counts until one
    /// whose position is known.
    last_at: Option<(i32, i32)>,
    /// `WM_MOUSEACTIVATE` for a left press came; each live tick clears it.
    click_activated: bool,
    /// The accept button's own procedure is handling a real release inside it (Task 28c).
    own_release: bool,
    /// `TME_LEAVE` is armed on the window, and on the accept button (review I2).
    window_tracked: bool,
    button_tracked: bool,
    /// Raw mouse input is registered for the window, and still is (Task 28d).
    raw: bool,
    /// The raw motion since the last real move or restart, in counts: every packet's
    /// `|dx| + |dy|` (Task 28d round 1).
    raw_counts: u32,
    /// Some of that motion came from a device (`hDevice` not null).
    device_raw: bool,
    /// The last real move that put the cursor somewhere new came with device raw motion since
    /// the previous such move or restart: the device that made the rest makes raw input, so
    /// the same-place keep is on (Task 28d round 1, set afresh at each such move: review I1).
    raw_confirmed: bool,
    /// The last absolute raw position (a tablet, a remote session), to tell motion.
    last_absolute: Option<(i32, i32)>,
}

/// How much raw mouse motion since the last real move is still a still hand (Task 28d round
/// 1), in counts: every packet's `|dx| + |dy|`, added up. Up to this, a same-place own move
/// keeps the rest and a press counts; more is the hand moving, and the rest ends. The pointer
/// probe on the owner's PC (build 26300) saw both real hand motions end with raw motion that
/// moved no pixel and had no move after it: a hand settling on the mouse, (1, 0) once, and
/// (0, -2) then (-1, 0) 160 ms later, so any bound must allow at least 3. A press can jolt the
/// mouse the same way. A drift of 4 counts moves the cursor a few pixels at most at usual
/// pointer speeds, so it can't carry a click from somewhere else, and every unlock still needs
/// the owner's PIN at the Windows Hello prompt.
pub const RAW_JITTER_COUNTS: u32 = 4;

/// One mouse `WM_INPUT` as the click gate judges it (Task 28d): whether the header names a
/// device (`hDevice` isn't null), whether it is an absolute position (`MOUSE_MOVE_ABSOLUTE`),
/// and `lLastX`, `lLastY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawMouse {
    pub device: bool,
    pub absolute: bool,
    pub last: (i32, i32),
}

impl ClickGate {
    /// No time in front, no press, no rest.
    pub const fn new() -> Self {
        Self {
            live: LiveClock {
                in_front_since: None,
            },
            press: PressTracker {
                hover_since: None,
                press: None,
            },
            last_at: None,
            click_activated: false,
            own_release: false,
            window_tracked: false,
            button_tracked: false,
            raw: false,
            raw_counts: 0,
            device_raw: false,
            raw_confirmed: false,
            last_absolute: None,
        }
    }

    /// Whether raw mouse input was registered for the window (Task 28d). Without it, every
    /// move that isn't hardware ends the rest (Task 28c).
    pub fn raw_registered(&mut self, ok: bool) {
        self.raw = ok;
    }

    /// Raw mouse input is no longer this window's (another registration in this process took
    /// it): Task 28c's rule from now on.
    pub fn raw_lost(&mut self) {
        self.raw = false;
    }

    /// Whether raw mouse input is on for the window (Task 28d).
    pub fn raw_on(&self) -> bool {
        self.raw
    }

    /// A mouse `WM_INPUT` (Task 28d). Relative motion adds `|dx| + |dy|` to the motion since
    /// the last real move, from a device or not; an absolute position other than the last one
    /// is the hand moving. Beyond [`RAW_JITTER_COUNTS`] the hand moved: the rest ends, and the
    /// next real move starts it again.
    pub fn raw_input(&mut self, r: RawMouse, now: Instant) {
        let counts = if r.absolute {
            if self.last_absolute.replace(r.last) == Some(r.last) {
                0
            } else {
                RAW_JITTER_COUNTS + 1
            }
        } else {
            r.last
                .0
                .unsigned_abs()
                .saturating_add(r.last.1.unsigned_abs())
        };
        if counts == 0 {
            return;
        }
        self.raw_counts = self.raw_counts.saturating_add(counts);
        self.device_raw |= r.device;
        if self.raw_counts > RAW_JITTER_COUNTS {
            self.press.on_move(false, now);
        }
    }

    /// The window lost the front, moved or was resized, or Merge moved: the delay, the press
    /// and the rest start again, and `cursor` is where the cursor is now.
    pub fn restart(&mut self, cursor: Option<(i32, i32)>) {
        self.live.restart();
        self.press.cancel();
        self.last_at = cursor;
        self.raw_counts = 0;
        self.device_raw = false;
    }

    /// The live timer's sample at `now`; whether the buttons are live.
    pub fn tick(&mut self, in_front: bool, now: Instant) -> bool {
        self.click_activated = false;
        self.live.tick(in_front, now)
    }

    /// Whether the buttons are live at `now` by the window's own clock.
    pub fn is_live(&self, now: Instant) -> bool {
        self.live.is_live(now)
    }

    /// `WM_MOUSEACTIVATE` for a left press: that press brings the window to the front.
    pub fn mouse_activate(&mut self) {
        self.click_activated = true;
    }

    /// One move the window or the button got; `on_accept` when its point is on the accept
    /// button where this window gets it (the grey button for the window, the live one for the
    /// button). Whether it counted towards the rest.
    /// `tracked` when `TME_LEAVE` was armed on the window that got it.
    fn judge_move(&mut self, tracked: bool, on_accept: bool, m: PointerMsg, now: Instant) -> bool {
        let moved = tracked && m.at.is_some_and(|at| cursor_moved(self.last_at, at));
        // Task 28d: an input move that isn't hardware, at the place last recorded, with raw
        // input on and confirmed, and no more than jitter since the last real move: Windows'
        // own move under a still hand.
        let still = self.raw
            && self.raw_confirmed
            && tracked
            && m.is_input()
            && self.raw_counts <= RAW_JITTER_COUNTS
            && m.at.is_some()
            && m.at == self.last_at;
        // Review m1: a real move whose message Windows made at another place than where the
        // cursor is now: a warp landed while it was being handled. Judged as an own move at
        // a new place, it ends the rest and counts for nothing.
        let real = m.real() && m.message_at == m.at;
        let still = still && !(m.real() && !real);
        let before = self.last_at;
        if m.is_input() {
            self.last_at = m.at;
        }
        // A real move that put the cursor somewhere new (or somewhere this window didn't know):
        // the raw motion since the last such move starts again with it, and whether device
        // motion came before it says whether the device that made this rest makes raw input,
        // judged afresh at each such move (Task 28d round 1, review I1). Windows' own moves,
        // and a real move at the same place, don't start it again: the bound is on the drift
        // since the hand last put the cursor somewhere.
        if m.is_input() && real && m.at != before {
            self.raw_confirmed = self.device_raw;
            self.raw_counts = 0;
            self.device_raw = false;
        }
        let counts = real && grey_rest_move(on_accept, moved, m.origin, m.device, m.sent);
        // A real move on the button that left the cursor where it was, or (with raw input) any
        // still own move there, is Windows' own move under a still cursor: it neither counts
        // nor ends the rest.
        if counts || !(on_accept && (real || still)) {
            self.press.on_move(counts, now);
        }
        counts
    }

    /// A `WM_MOUSEMOVE` to the window itself (moves over the grey accept button come here);
    /// whether it counted towards the rest ([`grey_rest_move`]).
    pub fn window_move(&mut self, over_grey_accept: bool, m: PointerMsg, now: Instant) -> bool {
        self.judge_move(self.window_tracked, over_grey_accept, m, now)
    }

    /// After a move to the window: whether `TME_LEAVE` is now armed on it.
    pub fn window_tracking(&mut self, armed: bool) {
        self.window_tracked = armed;
        if !armed {
            self.last_at = None;
        }
    }

    /// `WM_MOUSELEAVE` to the window; `on_accept` when the cursor is now on the accept button
    /// (it turned on under the pointer), where the rest goes on. Either way where the cursor is
    /// is unknown until the next move (review I2).
    pub fn window_leave(&mut self, on_accept: bool) {
        self.window_tracked = false;
        self.last_at = None;
        if !on_accept {
            self.press.on_leave();
        }
    }

    /// A `WM_LBUTTONDOWN` to the window itself: a press on the grey accept button is refused,
    /// as the click that brought the window to the front or as one before the buttons were live.
    /// Any press here forgets where the cursor is (re-review N2): while the button is down,
    /// another thread can take the capture and move the cursor where this window never sees it.
    /// The rest is untouched, and the next move just records the place.
    pub fn window_press(&mut self, over_grey_accept: bool) -> Option<Refusal> {
        self.window_tracked = false;
        self.last_at = None;
        let brought_forward = std::mem::replace(&mut self.click_activated, false);
        over_grey_accept.then(|| grey_click_refusal(brought_forward))
    }

    /// A `WM_MOUSEMOVE` to the accept button; whether it counted towards the rest.
    pub fn button_move(&mut self, inside: bool, m: PointerMsg, now: Instant) -> bool {
        self.judge_move(self.button_tracked, inside, m, now)
    }

    /// After a move to the accept button: whether `TME_LEAVE` is now armed on it.
    pub fn button_tracking(&mut self, armed: bool) {
        self.button_tracked = armed;
        if !armed {
            self.last_at = None;
        }
    }

    /// `WM_MOUSELEAVE` to the accept button: the rest ends, and where the cursor is is unknown
    /// until the next move (review I2).
    pub fn button_leave(&mut self) {
        self.button_tracked = false;
        self.last_at = None;
        self.press.on_leave();
    }

    /// A `WM_LBUTTONDOWN` or `WM_LBUTTONDBLCLK` to the accept button.
    pub fn button_press(&mut self, inside: bool, m: PointerMsg, now: Instant) {
        self.press.on_press(inside && m.real(), m.device, now);
    }

    /// A `WM_LBUTTONUP` to the accept button: whether it arms the button ([`arms_accept`]),
    /// and otherwise which gate refused it ([`release_refusal`]).
    pub fn button_release(&mut self, inside: bool, m: PointerMsg) -> (bool, Option<Refusal>) {
        let press = self.press.on_release();
        let input = ButtonInput {
            msg: WM_LBUTTONUP,
            over_button: inside,
            origin: m.origin,
            device: m.device,
            sent_by_another_thread: m.sent,
            pressed_by_hardware: press.is_ok(),
        };
        self.own_release = inside && m.real();
        let armed = arms_accept(input);
        (armed, release_refusal(input, press))
    }

    /// The button's own procedure has handled the release.
    pub fn release_done(&mut self) {
        self.own_release = false;
    }

    /// `WM_CAPTURECHANGED` to the accept button. Within its own release after a real release
    /// inside it (the button gives the capture back), the press is gone and the rest stays, so
    /// clicking again without moving can count (Task 28c); any other loss of the capture drops
    /// both, and forgets where the cursor is (re-review N2): the capture may have gone to
    /// another thread, and the cursor with it, unseen.
    pub fn capture_changed(&mut self) {
        if self.own_release {
            self.press.drop_press();
        } else {
            self.press.cancel();
            self.button_tracked = false;
            self.last_at = None;
        }
    }

    /// `WM_CANCELMODE` to the accept button.
    pub fn cancel_mode(&mut self) {
        self.press.cancel();
    }
}

/// Whether a message to the read-only text or a button would change what it shows (re-review 3
/// m2, m4). The text and the labels are set when the window is made and never after, so these
/// are refused from anyone: `ES_READONLY` blocks only typing, and another program can still
/// send `WM_SETTEXT` or `EM_REPLACESEL`, or post `EM_SETREADONLY` and `WM_PASTE`. Pure.
pub fn text_change(msg: u32) -> bool {
    matches!(msg, WM_SETTEXT | EM_REPLACESEL | EM_SETREADONLY | WM_PASTE)
}

/// The accept button's control id.
const ID_ACCEPT: usize = 1;
/// "Deny"'s control id.
const ID_DENY: usize = 2;
/// The status line's control id (Task 28b).
const ID_STATUS: usize = 3;
/// `SS_NOPREFIX` (winuser.h; `windows` 0.62.2 has it only behind `Win32_System_SystemServices`):
/// an `&` in the status line shows as it is. Left-aligned, wrapped text is `SS_LEFT`, which is 0.
const SS_NOPREFIX: u32 = 0x80;
/// The timer that makes the buttons live.
const TIMER_LIVE: usize = 1;

/// The accept button the current hardware input armed, or 0; set and cleared around one call
/// of the button's own window procedure.
static ARMED: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

// Each asking window has its own thread ([`window_thread`]), so its state is kept in that
// thread's locals: process-local, out of reach of other programs (re-review 2 n4: a window's
// `GWLP_USERDATA` can be written from another process).
thread_local! {
    /// The BUTTON class's original window procedure, kept when the buttons are subclassed.
    static BUTTON_PROC: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    /// The EDIT class's original window procedure, kept when the text is subclassed.
    static EDIT_PROC: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    /// The asking window's click gates: its time in front, the accept button's press and rest
    /// (Task 28c).
    static GATE: std::cell::Cell<ClickGate> = const { std::cell::Cell::new(ClickGate::new()) };
    /// Why the release being handled by the accept button's own procedure didn't arm it, for
    /// the `WM_COMMAND` that procedure sends (Task 28b); set and cleared around that one call.
    static RELEASE_REFUSAL: std::cell::Cell<Option<Refusal>> = const { std::cell::Cell::new(None) };
    /// The asking window's status line (Task 28b).
    static STATUS: std::cell::Cell<StatusLine> = const {
        std::cell::Cell::new(StatusLine { state: ButtonsState::TurningOn, refusal: None })
    };
    /// The accept button's label, for the status line.
    static ACCEPT_LABEL: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
    /// The STATIC class's original window procedure, kept when the status line is subclassed.
    static STATUS_PROC: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    /// lil itself is setting the status line's text (the subclass refuses everyone else).
    static OWN_STATUS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The asking window's last known place and size, to tell a move or resize.
    static PLACE: std::cell::Cell<(i32, i32, i32, i32)> = const { std::cell::Cell::new((0, 0, 0, 0)) };
    /// The accept button's last known place and size in the window (re-review 3 m2).
    static ACCEPT_PLACE: std::cell::Cell<(i32, i32, i32, i32)> = const {
        std::cell::Cell::new((0, 0, 0, 0))
    };
}

/// Changes this thread's click gates ([`ClickGate`]).
fn with_gate<T>(f: impl FnOnce(&mut ClickGate) -> T) -> T {
    GATE.with(|c| {
        let mut gate = c.get();
        let out = f(&mut gate);
        c.set(gate);
        out
    })
}

/// The pointer message this thread is handling: its source, and where the cursor really is
/// (`GetCursorPos`, never the message's `lParam`: review I1).
fn pointer_msg() -> PointerMsg {
    let (origin, device) = current_input_source();
    PointerMsg {
        origin,
        device,
        // SAFETY: a query about the message this thread is handling.
        sent: unsafe { InSendMessage() }.as_bool(),
        at: cursor_screen(),
        message_at: Some(message_pos()),
    }
}

/// Registers this process's raw mouse input (usage page 1, usage 2) for `window`, delivered
/// even while it isn't in front (`RIDEV_INPUTSINK`); with `None`, removes it (`RIDEV_REMOVE`).
/// Whether that worked (Task 28d).
fn register_raw_mouse(window: Option<HWND>) -> bool {
    let device = RAWINPUTDEVICE {
        usUsagePage: 0x01,
        usUsage: 0x02,
        dwFlags: if window.is_some() {
            RIDEV_INPUTSINK
        } else {
            RIDEV_REMOVE
        },
        hwndTarget: window.unwrap_or_default(),
    };
    // SAFETY: a local structure of the size given; this process's own registration.
    unsafe { RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32) }.is_ok()
}

/// Whether this process's raw mouse input still goes to `window`, even while it isn't in front
/// (Task 28d). Registration is per process, so another one here would take it, and no
/// `WM_INPUT` would then say the hand moved.
fn raw_mouse_is_for(window: HWND) -> bool {
    let mut devices = [RAWINPUTDEVICE::default(); 8];
    let mut count = devices.len() as u32;
    // SAFETY: a local array of `count` entries of the size given.
    let got = unsafe {
        GetRegisteredRawInputDevices(
            Some(devices.as_mut_ptr()),
            &mut count,
            size_of::<RAWINPUTDEVICE>() as u32,
        )
    };
    got != u32::MAX
        && devices[..(got as usize).min(devices.len())]
            .iter()
            .any(|d| {
                d.usUsagePage == 0x01
                    && d.usUsage == 0x02
                    && d.hwndTarget == window
                    && d.dwFlags.0 & RIDEV_INPUTSINK.0 != 0
            })
}

/// Before a move to `window` (or its accept button) is judged: when raw input is on but no
/// longer this window's, Task 28c's rule from now on, and one line says so (Task 28d).
fn check_raw_mouse(window: HWND) {
    if with_gate(|g| g.raw_on()) && !raw_mouse_is_for(window) {
        with_gate(ClickGate::raw_lost);
        log_raw_mouse_off();
    }
}

/// The one line said when raw input is off for the asking window (Task 28d).
fn log_raw_mouse_off() {
    let label = ACCEPT_LABEL.with(|l| l.borrow().clone());
    log_refusal(format!(
        "lil: raw mouse input is off for this window: after anything changes on screen, move \
         the pointer a little on {label} before clicking it"
    ));
}

/// The mouse `WM_INPUT` this thread is handling (Task 28d); `None` when it isn't a mouse's. One
/// that can't be read is taken as the hand moving (beyond [`RAW_JITTER_COUNTS`], from no
/// device, so it never turns the same-place keep on), the safe side.
fn raw_mouse_input(lp: LPARAM) -> Option<RawMouse> {
    let mut raw = RAWINPUT::default();
    let mut size = size_of::<RAWINPUT>() as u32;
    // SAFETY: the handle comes with this thread's WM_INPUT; the buffer is a local RAWINPUT of
    // the size given, and a mouse's input fits it.
    let got = unsafe {
        GetRawInputData(
            HRAWINPUT(lp.0 as *mut c_void),
            RID_INPUT,
            Some((&mut raw as *mut RAWINPUT).cast::<c_void>()),
            &mut size,
            size_of::<RAWINPUTHEADER>() as u32,
        )
    };
    if got == u32::MAX || got == 0 {
        return Some(RawMouse {
            device: false,
            absolute: false,
            last: (RAW_JITTER_COUNTS as i32 + 1, 0),
        });
    }
    if raw.header.dwType != RIM_TYPEMOUSE.0 {
        return None;
    }
    // SAFETY: the header says this is a mouse's input, so `mouse` is the union's live field.
    let m = unsafe { raw.data.mouse };
    Some(RawMouse {
        device: !raw.header.hDevice.0.is_null(),
        absolute: m.usFlags.0 & MOUSE_MOVE_ABSOLUTE.0 != 0,
        last: (m.lLastX, m.lLastY),
    })
}

/// Where the cursor was when Windows made the message this thread is handling
/// (`GetMessagePos`, screen coordinates as `GetCursorPos` gives them).
fn message_pos() -> (i32, i32) {
    // SAFETY: a plain query about the message this thread is handling.
    let pos = unsafe { GetMessagePos() };
    (
        i32::from(pos as u16 as i16),
        i32::from((pos >> 16) as u16 as i16),
    )
}

/// Arms `TME_LEAVE` on `hwnd`; whether it is armed.
fn track_leave(hwnd: HWND) -> bool {
    let mut track = TRACKMOUSEEVENT {
        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    // SAFETY: a local structure, for this thread's own window.
    unsafe { TrackMouseEvent(&mut track) }.is_ok()
}

/// `control`'s place and size in its parent's client area.
fn place_in_parent(control: HWND) -> Option<(i32, i32, i32, i32)> {
    // SAFETY: queries into local structures, for this window's own control.
    unsafe {
        let parent = GetParent(control).ok()?;
        let mut rect = windows::Win32::Foundation::RECT::default();
        GetWindowRect(control, &mut rect).ok()?;
        let mut corners = [
            windows::Win32::Foundation::POINT {
                x: rect.left,
                y: rect.top,
            },
            windows::Win32::Foundation::POINT {
                x: rect.right,
                y: rect.bottom,
            },
        ];
        for p in &mut corners {
            if !ScreenToClient(parent, p).as_bool() {
                return None;
            }
        }
        Some((corners[0].x, corners[0].y, corners[1].x, corners[1].y))
    }
}

/// The choices made in this process's asking windows, by window handle. Written on the window's
/// thread, read by [`OwnerWindow::wait_choice`]; an entry goes when its window is dropped.
static CHOICES: std::sync::Mutex<Vec<(isize, Choice)>> = std::sync::Mutex::new(Vec::new());

fn record_choice(hwnd: HWND, choice: Choice) {
    let mut choices = CHOICES.lock().unwrap_or_else(|e| e.into_inner());
    let key = hwnd.0 as isize;
    // The first choice stands: closing the window after "Merge" doesn't undo it.
    if !choices.iter().any(|(h, _)| *h == key) {
        choices.push((key, choice));
    }
}

fn choice_of(hwnd: isize) -> Option<Choice> {
    CHOICES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(h, _)| *h == hwnd)
        .map(|(_, c)| *c)
}

/// The origin and device of the input message this thread is handling.
fn current_input_source() -> (InputOrigin, InputDevice) {
    let mut source = INPUT_MESSAGE_SOURCE::default();
    // SAFETY: a plain query into a local structure.
    if unsafe { GetCurrentInputMessageSource(&mut source) }.is_err() {
        return (InputOrigin::Unavailable, InputDevice::Unavailable);
    }
    let origin = match source.originId {
        IMO_HARDWARE => InputOrigin::Hardware,
        IMO_INJECTED => InputOrigin::Injected,
        IMO_SYSTEM => InputOrigin::System,
        _ => InputOrigin::Unavailable,
    };
    let device = match source.deviceType {
        IMDT_MOUSE => InputDevice::Mouse,
        IMDT_TOUCH => InputDevice::Touch,
        IMDT_PEN => InputDevice::Pen,
        IMDT_KEYBOARD => InputDevice::Keyboard,
        IMDT_TOUCHPAD => InputDevice::Touchpad,
        _ => InputDevice::Unavailable,
    };
    (origin, device)
}

/// A mouse message's point (`lParam`), in the receiving window's client coordinates.
fn message_point(lp: LPARAM) -> (i32, i32) {
    (
        i32::from((lp.0 & 0xffff) as u16 as i16),
        i32::from(((lp.0 >> 16) & 0xffff) as u16 as i16),
    )
}

/// Whether a mouse message's point (`lParam`, client coordinates) is inside `button`.
fn point_inside(button: HWND, lp: LPARAM) -> bool {
    let (x, y) = message_point(lp);
    let mut rect = windows::Win32::Foundation::RECT::default();
    // SAFETY: a query into a local structure, for this button.
    unsafe { GetClientRect(button, &mut rect) }.is_ok()
        && x >= rect.left
        && x < rect.right
        && y >= rect.top
        && y < rect.bottom
}

/// The original procedure kept in `slot`, or `None` before it was kept.
fn original_proc(slot: &'static std::thread::LocalKey<std::cell::Cell<isize>>) -> WNDPROC {
    let original = slot.with(|p| p.get());
    // SAFETY: a non-zero value is a class's own window procedure, kept on this thread when the
    // control was subclassed; it stays valid for the process's life. Zero is `None`.
    unsafe { std::mem::transmute::<isize, WNDPROC>(original) }
}

/// The accept button's subclass: a real pointer release over it, after a real press on it,
/// arms it ([`arms_accept`], [`PressTracker`]), for exactly the one call of its own procedure
/// that reports the click. Its label can't be changed, and moving or resizing it restarts the
/// delay (re-review 3 m2).
extern "system" fn accept_button_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(original) = original_proc(&BUTTON_PROC) else {
        // SAFETY: the default procedure for this window's message.
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    };
    let now = Instant::now();
    // SAFETY: this button's own messages, passed on to its class's procedure.
    unsafe {
        match msg {
            _ if text_change(msg) => return LRESULT(0),
            WM_MOUSEMOVE => {
                if let Ok(parent) = GetParent(hwnd) {
                    check_raw_mouse(parent);
                } else {
                    with_gate(ClickGate::raw_lost);
                }
                let m = pointer_msg();
                with_gate(|g| g.button_move(point_inside(hwnd, lp), m, now));
                // Armed on every move, so leaving always ends the rest and forgets where the
                // cursor was (review I2).
                let armed = track_leave(hwnd);
                with_gate(|g| g.button_tracking(armed));
            }
            WM_MOUSELEAVE => with_gate(ClickGate::button_leave),
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                let m = pointer_msg();
                with_gate(|g| g.button_press(point_inside(hwnd, lp), m, now));
            }
            WM_CAPTURECHANGED => with_gate(ClickGate::capture_changed),
            WM_CANCELMODE => with_gate(ClickGate::cancel_mode),
            WM_WINDOWPOSCHANGED => {
                // Moved, resized, or its place can't be read: the delay starts again.
                let place = place_in_parent(hwnd);
                if place.is_none_or(|place| ACCEPT_PLACE.with(|p| p.replace(place)) != place)
                    && let Ok(parent) = GetParent(hwnd)
                {
                    restart_live(parent);
                }
            }
            WM_LBUTTONUP => {
                let m = pointer_msg();
                let (armed, refusal) = with_gate(|g| g.button_release(point_inside(hwnd, lp), m));
                // The button's own procedure releases the capture (WM_CAPTURECHANGED, back
                // here) and reports the click (WM_COMMAND to the window) within this call.
                let result = if armed {
                    ARMED.store(hwnd.0 as isize, std::sync::atomic::Ordering::SeqCst);
                    let result = CallWindowProcW(Some(original), hwnd, msg, wp, lp);
                    ARMED.store(0, std::sync::atomic::Ordering::SeqCst);
                    result
                } else {
                    // Not armed: the click this release reports (if any) says why (Task 28b).
                    RELEASE_REFUSAL.with(|r| r.set(refusal));
                    let result = CallWindowProcW(Some(original), hwnd, msg, wp, lp);
                    RELEASE_REFUSAL.with(|r| r.set(None));
                    result
                };
                with_gate(ClickGate::release_done);
                return result;
            }
            _ => {}
        }
        CallWindowProcW(Some(original), hwnd, msg, wp, lp)
    }
}

/// "Deny"'s subclass: only its label is guarded (re-review 3 m2), so the two can't be swapped.
extern "system" fn deny_button_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: this button's own messages, passed on to its class's procedure.
    unsafe {
        match original_proc(&BUTTON_PROC) {
            _ if text_change(msg) => LRESULT(0),
            Some(original) => CallWindowProcW(Some(original), hwnd, msg, wp, lp),
            None => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// The read-only text's subclass: what it shows can't be changed (re-review 3 m4).
extern "system" fn shown_text_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: this control's own messages, passed on to its class's procedure.
    unsafe {
        match original_proc(&EDIT_PROC) {
            _ if text_change(msg) => LRESULT(0),
            Some(original) => CallWindowProcW(Some(original), hwnd, msg, wp, lp),
            None => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// The status line's subclass: only lil sets its text (Task 28b), so another program can't
/// write its own instructions into it.
extern "system" fn status_line_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: this control's own messages, passed on to its class's procedure.
    unsafe {
        match original_proc(&STATUS_PROC) {
            _ if text_change(msg) && !OWN_STATUS.with(|o| o.get()) => LRESULT(0),
            Some(original) => CallWindowProcW(Some(original), hwnd, msg, wp, lp),
            None => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// Sets the status line's text.
fn set_status_text(window: HWND, text: &str) {
    // SAFETY: the window's own child control, on its own thread.
    unsafe {
        if let Ok(status) = GetDlgItem(Some(window), ID_STATUS as i32) {
            OWN_STATUS.with(|o| o.set(true));
            let _ = SetWindowTextW(status, &HSTRING::from(text));
            OWN_STATUS.with(|o| o.set(false));
        }
    }
}

/// Shows [`STATUS`]'s text in the status line.
fn show_status(window: HWND) {
    let text = ACCEPT_LABEL.with(|l| STATUS.with(|s| s.get().text(&l.borrow())));
    set_status_text(window, &text);
}

/// The buttons' state now.
fn current_buttons_state(window: HWND) -> ButtonsState {
    // SAFETY: a plain query.
    let in_front = unsafe { GetForegroundWindow() } == window;
    buttons_state(in_front, GATE.with(|c| c.get().is_live(Instant::now())))
}

/// A click on the accept button didn't count: the status line says which gate refused it, and
/// one line on stderr names the gate (Task 28b), nothing about the input. Once a choice is
/// made, nothing is said.
fn refuse_click(window: HWND, refusal: Refusal) {
    if choice_of(window.0 as isize).is_some() {
        return;
    }
    let state = current_buttons_state(window);
    STATUS.with(|s| {
        let mut status = s.get();
        status.refuse(refusal, state);
        s.set(status);
    });
    show_status(window);
    let label = ACCEPT_LABEL.with(|l| l.borrow().clone());
    log_refusal(format!(
        "lil: a click on {label} didn't count: {}",
        refusal.name()
    ));
}

/// How many refusal lines may wait for stderr; more are dropped.
const REFUSAL_LOG_QUEUE: usize = 16;

/// Hands `line` to a logging thread that writes it to stderr (Task 28b review), so the window
/// thread never waits on stderr: a full pipe or a slow console can't stall the window. The
/// queue is bounded and `try_send` never blocks; a line that doesn't fit is dropped, and a
/// closed stderr just loses it.
fn log_refusal(line: String) {
    static LOG: std::sync::OnceLock<Option<mpsc::SyncSender<String>>> = std::sync::OnceLock::new();
    let log = LOG.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<String>(REFUSAL_LOG_QUEUE);
        std::thread::Builder::new()
            .name("hello-refusal-log".into())
            .spawn(move || {
                for line in rx {
                    let _ = writeln!(io::stderr(), "{line}");
                }
            })
            .ok()
            .map(|_| tx)
    });
    if let Some(tx) = log {
        let _ = tx.try_send(line);
    }
}

/// The cursor's screen position, if it can be read.
fn cursor_screen() -> Option<(i32, i32)> {
    let mut p = windows::Win32::Foundation::POINT::default();
    // SAFETY: a query into a local structure.
    unsafe { GetCursorPos(&mut p) }.ok().map(|()| (p.x, p.y))
}

/// Whether `point` (the window's client coordinates) is on the accept button; with
/// `grey_only`, only while the button is disabled.
fn over_accept(window: HWND, (x, y): (i32, i32), grey_only: bool) -> bool {
    // SAFETY: queries about the window's own child control.
    unsafe {
        GetDlgItem(Some(window), ID_ACCEPT as i32)
            .ok()
            .filter(|&b| !(grey_only && IsWindowEnabled(b).as_bool()))
            .and_then(place_in_parent)
            .is_some_and(|(left, top, right, bottom)| {
                x >= left && x < right && y >= top && y < bottom
            })
    }
}

fn set_buttons_enabled(window: HWND, enabled: bool) {
    // SAFETY: the window's own child controls.
    unsafe {
        for id in [ID_ACCEPT, ID_DENY] {
            if let Ok(b) = GetDlgItem(Some(window), id as i32) {
                let _ = EnableWindow(b, enabled);
            }
        }
    }
}

/// The asking window lost the front, moved or was resized: the buttons stop working until it
/// has been in front for [`BUTTONS_LIVE_AFTER`] again.
fn restart_live(window: HWND) {
    // Only a move away from here counts towards the rest (review m1).
    let cursor = cursor_screen();
    with_gate(|g| g.restart(cursor));
    set_buttons_enabled(window, false);
}

/// The height of the button row, in pixels.
const BUTTON_ROW: i32 = 48;

fn window_thread(
    title: &HSTRING,
    body: &HSTRING,
    accept: Option<&HSTRING>,
    tx: &mpsc::Sender<io::Result<isize>>,
) {
    // SAFETY: standard window creation and a message loop on this thread; the strings outlive
    // the calls that borrow them.
    unsafe {
        let class = WNDCLASSW {
            lpfnWndProc: Some(owner_wndproc),
            lpszClassName: w!("LILITH-hello-owner"),
            ..Default::default()
        };
        // A second registration in the same process fails harmlessly; the class is the same.
        RegisterClassW(&class);
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_VISIBLE;
        let hwnd = match CreateWindowExW(
            WS_EX_APPWINDOW,
            w!("LILITH-hello-owner"),
            title,
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            720,
            520,
            None,
            None,
            None,
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                let _ = tx.send(Err(io_err(e)));
                return;
            }
        };
        // Within the monitor's work area, centred, so the buttons are on screen at any scaling
        // (re-review m2).
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let work = info.rcWork;
            let (work_w, work_h) = (work.right - work.left, work.bottom - work.top);
            let (w, h) = (720.min(work_w - 32), 520.min(work_h - 32));
            let _ = SetWindowPos(
                hwnd,
                None,
                work.left + (work_w - w) / 2,
                work.top + (work_h - h) / 2,
                w,
                h,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        let font = GetStockObject(DEFAULT_GUI_FONT);
        let set_font = |control: HWND| {
            SendMessageW(
                control,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
        };
        let mut client = windows::Win32::Foundation::RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let row = if accept.is_some() { BUTTON_ROW } else { 0 };
        let edit_style = WINDOW_STYLE(
            WS_CHILD.0
                | WS_VISIBLE.0
                | WS_VSCROLL.0
                | ES_MULTILINE as u32
                | ES_READONLY as u32
                | ES_AUTOVSCROLL as u32,
        );
        if let Ok(edit) = CreateWindowExW(
            Default::default(),
            w!("EDIT"),
            body,
            edit_style,
            8,
            8,
            client.right - 16,
            client.bottom - 16 - row,
            Some(hwnd),
            None,
            None,
            None,
        ) {
            set_font(edit);
            // Subclassed so other programs can't change what it shows (shown_text_proc); the
            // original procedure is kept on this thread first.
            EDIT_PROC.with(|p| p.set(GetWindowLongPtrW(edit, GWLP_WNDPROC)));
            SetWindowLongPtrW(edit, GWLP_WNDPROC, shown_text_proc as *const () as isize);
            // The read-only text has the focus, so a stray Space or Enter clicks nothing.
            let _ = SetFocus(Some(edit));
        }
        if let Some(accept) = accept {
            // Plain push buttons, neither the default, both disabled until the window has been
            // in front for BUTTONS_LIVE_AFTER: only a deliberate click chooses.
            let button = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | WS_DISABLED.0);
            let (w, h) = (110, 30);
            let y = client.bottom - 8 - h;
            for (label, id, x) in [
                (accept, ID_ACCEPT, client.right - 2 * (w + 8)),
                (&HSTRING::from("Deny"), ID_DENY, client.right - (w + 8)),
            ] {
                if let Ok(b) = CreateWindowExW(
                    Default::default(),
                    w!("BUTTON"),
                    label,
                    button,
                    x,
                    y,
                    w,
                    h,
                    Some(hwnd),
                    Some(HMENU(id as *mut c_void)),
                    None,
                    None,
                ) {
                    set_font(b);
                    // Both are subclassed: the accept button so its own input can be checked
                    // (accept_button_proc), and both so their labels can't be swapped. The
                    // original procedure is kept on this thread first, so a subclass never
                    // runs without it.
                    BUTTON_PROC.with(|p| p.set(GetWindowLongPtrW(b, GWLP_WNDPROC)));
                    let proc = if id == ID_ACCEPT {
                        accept_button_proc as *const () as isize
                    } else {
                        deny_button_proc as *const () as isize
                    };
                    SetWindowLongPtrW(b, GWLP_WNDPROC, proc);
                    if id == ID_ACCEPT
                        && let Some(place) = place_in_parent(b)
                    {
                        ACCEPT_PLACE.with(|p| p.set(place));
                    }
                }
            }
            // The status line, left of the buttons: what grey buttons wait for, and why a click
            // didn't count (Task 28b). Subclassed so only lil sets its text.
            ACCEPT_LABEL.with(|l| *l.borrow_mut() = accept.to_string());
            let first = STATUS.with(|s| s.get().text(&accept.to_string()));
            if let Ok(status) = CreateWindowExW(
                Default::default(),
                w!("STATIC"),
                &HSTRING::from(first),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | SS_NOPREFIX),
                8,
                client.bottom - BUTTON_ROW + 4,
                (client.right - 2 * (w + 8) - 16).max(0),
                BUTTON_ROW - 8,
                Some(hwnd),
                Some(HMENU(ID_STATUS as *mut c_void)),
                None,
                None,
            ) {
                set_font(status);
                STATUS_PROC.with(|p| p.set(GetWindowLongPtrW(status, GWLP_WNDPROC)));
                SetWindowLongPtrW(status, GWLP_WNDPROC, status_line_proc as *const () as isize);
            }
            // Raw mouse input tells a still hand from a moving one (Task 28d). Without it,
            // Task 28c's rule: every move that isn't hardware ends the rest.
            let raw = register_raw_mouse(Some(hwnd));
            with_gate(|g| g.raw_registered(raw));
            if !raw {
                log_raw_mouse_off();
            }
            // The live timer runs until a choice is made (re-review 2 n3).
            SetTimer(Some(hwnd), TIMER_LIVE, LIVE_TICK.as_millis() as u32, None);
        }
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = tx.send(Ok(hwnd.0 as isize));
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

extern "system" fn owner_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: the window procedure of this module's own class.
    unsafe {
        match msg {
            WM_COMMAND => {
                let child = |id: usize| GetDlgItem(Some(hwnd), id as i32).ok();
                let sender = lp.0;
                let is = |b: Option<HWND>| b.is_some_and(|b| b.0 as isize == sender && sender != 0);
                let accept = child(ID_ACCEPT);
                let command = ButtonCommand {
                    clicked: ((wp.0 >> 16) & 0xffff) as u32 == BN_CLICKED,
                    from_accept: is(accept),
                    from_deny: is(child(ID_DENY)),
                    armed: sender != 0 && ARMED.load(std::sync::atomic::Ordering::SeqCst) == sender,
                    sent_by_another_thread: InSendMessage().as_bool(),
                    // Enabled and live by our own clock: another program can enable the button
                    // with EnableWindow, but it can't move the clock.
                    enabled: accept.is_some_and(|b| IsWindowEnabled(b).as_bool())
                        && GATE.with(|c| c.get().is_live(Instant::now())),
                };
                if let Some(choice) = choice_from_command(command) {
                    record_choice(hwnd, choice);
                    // One choice only: both buttons go grey.
                    let _ = KillTimer(Some(hwnd), TIMER_LIVE);
                    set_buttons_enabled(hwnd, false);
                    set_status_text(hwnd, "");
                } else if let Some(refusal) =
                    click_refusal(command, RELEASE_REFUSAL.with(|r| r.get()))
                {
                    refuse_click(hwnd, refusal);
                }
                LRESULT(0)
            }
            WM_MOUSEACTIVATE => {
                // A click on the window while another is in front (Task 28b).
                if ((lp.0 >> 16) & 0xffff) as u32 == WM_LBUTTONDOWN {
                    with_gate(ClickGate::mouse_activate);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_LBUTTONDOWN => {
                // A grey button gets no mouse input: a press on it comes here (Task 28b).
                let over = over_accept(hwnd, message_point(lp), true);
                if let Some(refusal) = with_gate(|g| g.window_press(over)) {
                    refuse_click(hwnd, refusal);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_MOUSEMOVE => {
                // Moves over the grey button come here too; real ones count towards the rest
                // (plan 4a ruling 2026-10-25). A move here anywhere else, or one that isn't
                // real, starts it again.
                // Only a cursor that moved since the last move or restart counts, so the window
                // or the button sliding under a still cursor never does (review m1, Task 28c).
                // Where the cursor is comes from GetCursorPos, and only from input messages
                // (review I1); TME_LEAVE is armed on every move (review I2). With raw input
                // still this window's, an own move at the same place keeps the rest (Task 28d).
                check_raw_mouse(hwnd);
                let m = pointer_msg();
                let over = over_accept(hwnd, message_point(lp), true);
                with_gate(|g| g.window_move(over, m, Instant::now()));
                let armed = track_leave(hwnd);
                with_gate(|g| g.window_tracking(armed));
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_MOUSELEAVE => {
                // The pointer left the window's own area. Onto the accept button (it turned on
                // under the pointer) the rest goes on, and the button's own moves carry it;
                // anywhere else it is gone.
                let mut cursor = windows::Win32::Foundation::POINT::default();
                let on_button = GetCursorPos(&mut cursor).is_ok()
                    && ScreenToClient(hwnd, &mut cursor).as_bool()
                    && over_accept(hwnd, (cursor.x, cursor.y), false);
                with_gate(|g| g.window_leave(on_button));
                match GetDlgItem(Some(hwnd), ID_ACCEPT as i32) {
                    // Track the button's own leave, so leaving it drops the rest (review m2).
                    Ok(accept) if on_button => {
                        let _ = track_leave(accept);
                    }
                    _ => {}
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_ACTIVATE => {
                if (wp.0 & 0xffff) as u32 == WA_INACTIVE {
                    // Not in front: the buttons stop working until it has been again for
                    // BUTTONS_LIVE_AFTER.
                    restart_live(hwnd);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_WINDOWPOSCHANGED => {
                // Moved or resized (re-review 2 n2): the delay starts again, so a program
                // can't keep Merge under the cursor.
                let mut rect = windows::Win32::Foundation::RECT::default();
                if GetWindowRect(hwnd, &mut rect).is_ok() {
                    let place = (rect.left, rect.top, rect.right, rect.bottom);
                    if PLACE.with(|p| p.replace(place)) != place {
                        restart_live(hwnd);
                    }
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_TIMER if wp.0 == TIMER_LIVE => {
                if choice_of(hwnd.0 as isize).is_some() {
                    let _ = KillTimer(Some(hwnd), TIMER_LIVE);
                } else {
                    let in_front = GetForegroundWindow() == hwnd;
                    let live = with_gate(|g| g.tick(in_front, Instant::now()));
                    set_buttons_enabled(hwnd, live);
                    let changed = STATUS.with(|s| {
                        let mut status = s.get();
                        let changed = status.tick(buttons_state(in_front, live));
                        s.set(status);
                        changed
                    });
                    if changed {
                        show_status(hwnd);
                    }
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                record_choice(hwnd, Choice::Closed);
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_INPUT => {
                // The hand moved the mouse: the rest ends (Task 28d). DefWindowProc cleans up.
                if let Some(r) = raw_mouse_input(lp) {
                    with_gate(|g| g.raw_input(r, Instant::now()));
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_DESTROY => {
                // This window's raw input goes with it; another's is left alone.
                if raw_mouse_is_for(hwnd) {
                    register_raw_mouse(None);
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{WM_CHAR, WM_KEYDOWN, WM_KEYUP};

    #[test] // spec: QA-088
    fn kek_is_sha256_over_the_domain_then_the_signature() {
        let signature = [7u8; 256];
        let mut input = KEK_DOMAIN.to_vec();
        input.extend_from_slice(&signature);
        let want: [u8; 32] = Sha256::digest(&input).into();
        assert_eq!(*kek_from_signature(&signature), want);
        assert_ne!(*kek_from_signature(&[8u8; 256]), want);
    }

    #[test]
    fn window_id_carries_the_handle() {
        assert_eq!(WindowId::of(0x1234_5678).value, 0x1234_5678);
        assert_eq!(size_of::<WindowId>(), 8);
    }

    /// Phase B ruling: a window Windows keeps behind flashes and is waited for; Windows Hello
    /// is used only once it is in front.
    #[test] // spec: QA-088
    fn windows_hello_is_used_only_when_its_window_is_in_front() {
        for ok in [
            Raise::AlreadyInFront,
            Raise::SetForeground,
            Raise::AttachedInput,
        ] {
            assert!(
                front_or_wait(ok, || panic!("waited while in front")).is_ok(),
                "{ok:?}"
            );
        }
        assert!(front_or_wait(Raise::NotInFront, || true).is_ok());
        let e = front_or_wait(Raise::NotInFront, || false).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        assert!(e.to_string().contains("brought forward"), "{e}");

        // The wait itself: until ready, at most the timeout.
        use std::cell::Cell;
        let polls = Cell::new(0);
        assert!(wait_until(
            &|| {
                polls.set(polls.get() + 1);
                polls.get() > 3
            },
            Duration::from_secs(1),
            &|_| {}
        ));
        let waited = Cell::new(Duration::ZERO);
        assert!(!wait_until(&|| false, Duration::from_secs(2), &|d| waited
            .set(waited.get() + d)));
        assert_eq!(waited.get(), Duration::from_secs(2));
    }

    #[test] // spec: QA-088
    fn an_unanswered_prompt_is_cancelled_after_the_wait() {
        use std::cell::Cell;
        // Never answered: cancelled once, at the limit, and a `TimedOut` error.
        let cancelled = Cell::new(0);
        let waited = Cell::new(Duration::ZERO);
        let e = wait_bounded(
            &|| false,
            &|| cancelled.set(cancelled.get() + 1),
            Duration::from_secs(2),
            &|d| waited.set(waited.get() + d),
        )
        .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        assert_eq!(cancelled.get(), 1);
        assert_eq!(waited.get(), Duration::from_secs(2));

        // Answered in time: no cancel.
        let polls = Cell::new(0);
        wait_bounded(
            &|| {
                polls.set(polls.get() + 1);
                polls.get() > 3
            },
            &|| panic!("cancelled an answered prompt"),
            Duration::from_secs(2),
            &|_| {},
        )
        .unwrap();
        assert_eq!(HELLO_WAIT, Duration::from_secs(120));
    }

    /// Fix round 2: Windows Hello is raised only after "Merge" / "Approve"; "Deny", closing the
    /// window or no choice in time refuses.
    #[test] // spec: QA-088, QA-036
    fn windows_hello_is_raised_only_after_the_accept_click() {
        use std::cell::Cell;
        let called = Cell::new(false);
        let call = || {
            called.set(true);
            Ok(())
        };
        ask_then(|| Choice::Accepted, call).unwrap();
        assert!(called.get());

        for refused in [Choice::Denied, Choice::Closed, Choice::TimedOut] {
            let called = Cell::new(false);
            let e = ask_then(
                || refused,
                || {
                    called.set(true);
                    Ok(())
                },
            )
            .unwrap_err();
            assert!(!called.get(), "Windows Hello was raised after {refused:?}");
            // A refusal, not a reason to fall back to the passphrase.
            assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{refused:?}");
        }
    }

    #[test] // spec: QA-088
    fn a_choice_not_made_in_time_is_a_timeout() {
        use std::cell::Cell;
        let waited = Cell::new(Duration::ZERO);
        let choice = wait_for_choice(&|| None, Duration::from_secs(1), &|d| {
            waited.set(waited.get() + d)
        });
        assert_eq!(choice, Choice::TimedOut);
        assert_eq!(waited.get(), Duration::from_secs(1));
        let polls = Cell::new(0);
        let choice = wait_for_choice(
            &|| {
                polls.set(polls.get() + 1);
                (polls.get() > 2).then_some(Choice::Denied)
            },
            Duration::from_secs(1),
            &|_| {},
        );
        assert_eq!(choice, Choice::Denied);
    }

    /// A real click on the accept button, as the owner window sees it.
    fn real_click() -> ButtonCommand {
        ButtonCommand {
            clicked: true,
            from_accept: true,
            from_deny: false,
            armed: true,
            sent_by_another_thread: false,
            enabled: true,
        }
    }

    /// A real mouse release on the accept button, as its subclass sees it.
    fn real_release() -> ButtonInput {
        ButtonInput {
            msg: WM_LBUTTONUP,
            over_button: true,
            origin: InputOrigin::Hardware,
            device: InputDevice::Mouse,
            sent_by_another_thread: false,
            pressed_by_hardware: true,
        }
    }

    /// Re-review N1 and re-review 2 N4: only a real pointer click on the accept button accepts.
    #[test] // spec: QA-088, QA-036
    fn only_a_real_click_on_the_accept_button_accepts() {
        assert_eq!(choice_from_command(real_click()), Some(Choice::Accepted));

        // Arming: a real mouse, touch, pen or touchpad release over the button, on its own
        // thread.
        for device in [
            InputDevice::Mouse,
            InputDevice::Touch,
            InputDevice::Pen,
            InputDevice::Touchpad,
        ] {
            let input = ButtonInput {
                device,
                ..real_release()
            };
            assert!(arms_accept(input), "{device:?}");
        }
        for origin in [
            // SendInput.
            InputOrigin::Injected,
            InputOrigin::System,
            // A posted or sent message (WM_LBUTTONUP, BM_CLICK, UI Automation's Invoke).
            InputOrigin::Unavailable,
        ] {
            let input = ButtonInput {
                origin,
                ..real_release()
            };
            assert!(!arms_accept(input), "{origin:?}");
            // Not armed, so its click doesn't accept.
            let c = ButtonCommand {
                armed: arms_accept(input),
                ..real_click()
            };
            assert_eq!(choice_from_command(c), None, "{origin:?}");
        }
        // A hardware release whose device isn't a pointer: no.
        for device in [InputDevice::Keyboard, InputDevice::Unavailable] {
            let input = ButtonInput {
                device,
                ..real_release()
            };
            assert!(!arms_accept(input), "{device:?}");
        }
        // N4: a real keyboard Space (or Enter, or any key) never arms Merge, whichever control
        // has the focus: its key-down, key-up and character messages don't arm, even if the
        // device were misreported as a mouse.
        for msg in [WM_KEYUP, WM_KEYDOWN, WM_CHAR] {
            for device in [InputDevice::Keyboard, InputDevice::Mouse] {
                let input = ButtonInput {
                    msg,
                    device,
                    ..real_release()
                };
                assert!(!arms_accept(input), "{msg:#x} {device:?}");
            }
        }
        // The press, not the release: no.
        let press = ButtonInput {
            msg: WM_LBUTTONDOWN,
            ..real_release()
        };
        assert!(!arms_accept(press));
        // Released outside the button (it held the capture): no.
        let outside = ButtonInput {
            over_button: false,
            ..real_release()
        };
        assert!(!arms_accept(outside));
        // Hardware input, but handled as a message another thread sent: no.
        let sent = ButtonInput {
            sent_by_another_thread: true,
            ..real_release()
        };
        assert!(!arms_accept(sent));

        // A WM_COMMAND posted or sent by another process: never armed.
        let forged = ButtonCommand {
            armed: false,
            ..real_click()
        };
        assert_eq!(choice_from_command(forged), None);
        // Sent from another thread even while armed: no.
        let sent = ButtonCommand {
            sent_by_another_thread: true,
            ..real_click()
        };
        assert_eq!(choice_from_command(sent), None);
        // Not BN_CLICKED: no.
        let other = ButtonCommand {
            clicked: false,
            ..real_click()
        };
        assert_eq!(choice_from_command(other), None);
        // Claiming the accept id from another window (lParam isn't the button): no.
        let elsewhere = ButtonCommand {
            from_accept: false,
            ..real_click()
        };
        assert_eq!(choice_from_command(elsewhere), None);
        // Before the window was in front long enough (m1): no.
        let early = ButtonCommand {
            enabled: false,
            ..real_click()
        };
        assert_eq!(choice_from_command(early), None);

        // "Deny" from its own button, whatever sent it: refusing is always safe.
        for (armed, sent, enabled) in [(false, true, false), (true, false, true)] {
            let deny = ButtonCommand {
                clicked: true,
                from_accept: false,
                from_deny: true,
                armed,
                sent_by_another_thread: sent,
                enabled,
            };
            assert_eq!(choice_from_command(deny), Some(Choice::Denied));
        }
    }

    /// Re-review m1, re-review 2 n2 and n3, re-review 3 I1: the buttons go live only after the
    /// window has been in front for [`BUTTONS_LIVE_AFTER`] of real time, however many timer
    /// messages arrive; losing the front, a move or a resize starts the delay again; and the
    /// timer keeps checking until they are live.
    #[test] // spec: QA-088, QA-036
    fn the_buttons_go_live_after_a_second_in_front_and_a_move_restarts_the_delay() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);

        // I1: 100 ticks inside the first second (posted WM_TIMERs) never make it live.
        let mut clock = LiveClock::default();
        for i in 0..100 {
            assert!(!clock.tick(true, at(i * 9)), "tick {i}");
        }
        // A full second after the first tick in front: live.
        assert!(!clock.tick(true, at(999)));
        assert!(clock.tick(true, at(1_000)));

        // n3: not in front for a long time (the raise hadn't won yet), then in front: the timer
        // kept checking, so the buttons still go live, a full second after it got there.
        let mut clock = LiveClock::default();
        for i in 0..100 {
            assert!(!clock.tick(false, at(i * 250)));
        }
        assert!(!clock.tick(true, at(30_000)));
        assert!(!clock.tick(true, at(30_999)));
        assert!(clock.tick(true, at(31_000)));

        // A tick not in front, then back: the second starts again from the new tick.
        assert!(!clock.tick(false, at(31_100)));
        assert!(!clock.tick(true, at(31_200)));
        assert!(!clock.tick(true, at(32_199)));
        assert!(clock.tick(true, at(32_200)));

        // n2: moved or resized (or deactivated) while live: not live, and a restart can't be
        // skipped: the first tick after it starts a new second, however late it comes.
        clock.restart();
        assert!(!clock.is_live(at(40_000)));
        assert!(!clock.tick(true, at(40_000)));
        for i in 0..100 {
            assert!(!clock.tick(true, at(40_000 + i * 9)));
        }
        assert!(clock.tick(true, at(41_000)));
        // Time that goes backwards (it can't with Instant) is never live.
        assert!(!clock.is_live(at(39_000)));
    }

    /// Re-review 3 m1 and m3: a release arms only after a hardware press on the button, and a
    /// mouse or touchpad press counts only after the cursor rested on the button by hardware
    /// moves for [`HOVER_BEFORE_PRESS`].
    #[test] // spec: QA-088, QA-036
    fn a_release_arms_only_after_a_real_press_on_a_hovered_button() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let hover = HOVER_BEFORE_PRESS.as_millis() as u64;

        // Hovered long enough by real moves, then a real press: the release arms.
        let mut p = PressTracker::default();
        p.on_move(true, at(0));
        p.on_move(true, at(hover / 2));
        p.on_press(true, InputDevice::Mouse, at(hover));
        assert_eq!(p.on_release(), Ok(()));
        // A release consumes the press: a second release doesn't arm.
        assert_eq!(p.on_release(), Err(Refusal::NoRealPress));

        // m1: no press at all (another program posted WM_LBUTTONDOWN, or the press was made
        // elsewhere): the release doesn't arm.
        let mut p = PressTracker::default();
        p.on_move(true, at(0));
        p.on_press(false, InputDevice::Mouse, at(hover * 2));
        assert_eq!(p.on_release(), Err(Refusal::NoRealPress));
        assert!(!arms_accept(ButtonInput {
            pressed_by_hardware: false,
            ..real_release()
        }));

        // m3: the cursor arrived without a real move (SetCursorPos), or a moment ago: a mouse
        // or touchpad press doesn't count.
        for device in [InputDevice::Mouse, InputDevice::Touchpad] {
            let mut p = PressTracker::default();
            p.on_press(true, device, at(hover * 2));
            assert_eq!(
                p.on_release(),
                Err(Refusal::NoMove),
                "{device:?} with no hover"
            );
            let mut p = PressTracker::default();
            p.on_move(true, at(0));
            p.on_press(true, device, at(hover - 1));
            assert_eq!(
                p.on_release(),
                Err(Refusal::NoRest),
                "{device:?} hovered too briefly"
            );
        }
        // A move that isn't a real pointer move, or the cursor leaving, starts the hover again.
        let mut p = PressTracker::default();
        p.on_move(true, at(0));
        p.on_move(false, at(hover / 2));
        p.on_move(true, at(hover));
        p.on_press(true, InputDevice::Mouse, at(hover + hover / 2));
        assert_eq!(p.on_release(), Err(Refusal::NoRest));
        let mut p = PressTracker::default();
        p.on_move(true, at(0));
        p.on_leave();
        p.on_press(true, InputDevice::Mouse, at(hover * 2));
        assert_eq!(p.on_release(), Err(Refusal::NoMove));
        // A press that isn't real is put down to the press, even after a long hover.
        let mut p = PressTracker::default();
        p.on_move(true, at(0));
        p.on_press(false, InputDevice::Mouse, at(hover * 2));
        assert_eq!(p.on_release(), Err(Refusal::NoRealPress));

        // Touch and pen press where the contact is, not at the cursor: no hover needed.
        for device in [InputDevice::Touch, InputDevice::Pen] {
            let mut p = PressTracker::default();
            p.on_press(true, device, at(0));
            assert_eq!(p.on_release(), Ok(()), "{device:?}");
        }

        // A lost capture, a cancelled mode or a restart of the delay drops the press and the
        // hover.
        let mut p = PressTracker::default();
        p.on_move(true, at(0));
        p.on_press(true, InputDevice::Mouse, at(hover));
        p.cancel();
        assert_eq!(p.on_release(), Err(Refusal::NoRealPress));
        p.on_press(true, InputDevice::Mouse, at(hover * 3));
        assert_eq!(
            p.on_release(),
            Err(Refusal::NoMove),
            "the hover went with the cancel"
        );
    }

    /// Task 28b: every gate that can refuse a click on the accept button has its own message
    /// and its own log name. The message tells the owner what to do next; it depends only on
    /// the gate and lil's own label, never on the input.
    #[test] // spec: QA-088
    fn each_refused_click_says_which_gate_refused_it() {
        let all = [
            Refusal::NotInFront,
            Refusal::NotLiveYet,
            Refusal::NotPointer,
            Refusal::NoRealPress,
            Refusal::NoRest,
            Refusal::NoMove,
        ];
        for (i, a) in all.iter().enumerate() {
            let text = a.message("Merge");
            assert!(!text.is_empty(), "{a:?}");
            // Task 28c: the status line is about 450 px wide and three lines tall in the
            // dialog font (about 80 characters a line), so every message fits in two lines
            // with room to spare, with "Approve" too.
            assert!(a.message("Approve").chars().count() <= 160, "{a:?}");
            assert!(
                a.name()
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{a:?}"
            );
            for b in &all[i + 1..] {
                assert_ne!(text, b.message("Merge"), "{a:?} {b:?}");
                assert_ne!(a.name(), b.name(), "{a:?} {b:?}");
            }
        }
        // Task 28c: a still hand can't start the rest, so every message that leads to another
        // click on the button says to move the pointer a little on it first.
        assert_eq!(
            Refusal::NoMove.message("Merge"),
            "Move the pointer a little on Merge, then click it."
        );
        assert_eq!(
            Refusal::NoMove.message("Approve"),
            "Move the pointer a little on Approve, then click it."
        );
        for a in [Refusal::NotInFront, Refusal::NotLiveYet, Refusal::NoRest] {
            assert!(
                a.message("Merge")
                    .contains("move the pointer a little on Merge"),
                "{a:?}"
            );
            assert!(!a.message("Merge").contains("Rest the pointer"), "{a:?}");
        }
        assert!(
            Refusal::NotLiveYet
                .message("Merge")
                .starts_with("Wait a moment")
        );
        assert!(Refusal::NotInFront.message("Merge").contains("front"));
        assert!(
            Refusal::NotPointer
                .message("Merge")
                .starts_with("That click didn't come from your mouse or touchpad")
        );
        assert!(Refusal::NoRealPress.message("Approve").contains("Approve"));
    }

    /// Plan 4a ruling 2026-10-25: real pointer moves over the grey accept button count towards
    /// the rest, so a pointer parked on it before the buttons turn on isn't refused; anything
    /// else there (a warp's move, injected, sent, a key's device) doesn't, and starts it again.
    #[test] // spec: QA-088
    fn real_moves_over_the_grey_button_count_towards_the_rest() {
        let pointers = [
            InputDevice::Mouse,
            InputDevice::Touch,
            InputDevice::Pen,
            InputDevice::Touchpad,
        ];
        for device in pointers {
            assert!(
                grey_rest_move(true, true, InputOrigin::Hardware, device, false),
                "{device:?}"
            );
            // Elsewhere in the window, it isn't on the button.
            assert!(
                !grey_rest_move(false, true, InputOrigin::Hardware, device, false),
                "{device:?}"
            );
            // Sent from another thread.
            assert!(
                !grey_rest_move(true, true, InputOrigin::Hardware, device, true),
                "{device:?}"
            );
        }
        for origin in [
            InputOrigin::Injected,
            InputOrigin::System,
            InputOrigin::Unavailable,
        ] {
            assert!(
                !grey_rest_move(true, true, origin, InputDevice::Mouse, false),
                "{origin:?}"
            );
        }
        for device in [InputDevice::Keyboard, InputDevice::Unavailable] {
            assert!(
                !grey_rest_move(true, true, InputOrigin::Hardware, device, false),
                "{device:?}"
            );
        }

        // The parked pointer: real moves over the grey button, then it turns on, and the owner
        // clicks without moving again: the press counts.
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let hover = HOVER_BEFORE_PRESS.as_millis() as u64;
        let mut p = PressTracker::default();
        p.on_move(
            grey_rest_move(true, true, InputOrigin::Hardware, InputDevice::Mouse, false),
            at(0),
        );
        p.on_move(
            grey_rest_move(true, true, InputOrigin::Hardware, InputDevice::Mouse, false),
            at(5),
        );
        p.on_press(true, InputDevice::Mouse, at(hover));
        assert_eq!(p.on_release(), Ok(()));
        // Parked, then the cursor is warped (the warp's move isn't a hardware one) and the owner
        // clicks at once: the rest started again at the warp, so the press doesn't count.
        let mut p = PressTracker::default();
        p.on_move(
            grey_rest_move(true, true, InputOrigin::Hardware, InputDevice::Mouse, false),
            at(0),
        );
        p.on_move(
            grey_rest_move(true, true, InputOrigin::System, InputDevice::Mouse, false),
            at(hover * 3),
        );
        p.on_press(true, InputDevice::Mouse, at(hover * 3 + 1));
        assert_eq!(p.on_release(), Err(Refusal::NoMove));
        // Parked, then moved off the button within the window: the rest is gone.
        let mut p = PressTracker::default();
        p.on_move(
            grey_rest_move(true, true, InputOrigin::Hardware, InputDevice::Mouse, false),
            at(0),
        );
        p.on_move(
            grey_rest_move(
                false,
                true,
                InputOrigin::Hardware,
                InputDevice::Mouse,
                false,
            ),
            at(1),
        );
        p.on_press(true, InputDevice::Mouse, at(hover * 3));
        assert_eq!(p.on_release(), Err(Refusal::NoMove));
    }

    /// Task 28b review m1: a move over the grey button counts towards the rest only when the
    /// cursor's screen position changed since the last restart. The window or the button sliding
    /// under a still cursor (which restarts the delay and makes Windows send a move) never counts,
    /// whatever origin that move reports.
    #[test] // spec: QA-088
    fn a_slide_under_a_still_cursor_never_counts_towards_the_rest() {
        // Where the cursor was at the restart, and where it is now.
        assert!(cursor_moved(Some((100, 200)), (101, 200)));
        assert!(cursor_moved(Some((100, 200)), (100, 199)));
        assert!(!cursor_moved(Some((100, 200)), (100, 200)));
        // The position at the restart couldn't be read: nothing counts.
        assert!(!cursor_moved(None, (100, 200)));

        // A still cursor: even a move reporting real hardware doesn't count.
        let still = cursor_moved(Some((100, 200)), (100, 200));
        assert!(!grey_rest_move(
            true,
            still,
            InputOrigin::Hardware,
            InputDevice::Mouse,
            false
        ));
        // After the slide the owner parks the pointer and clicks without moving: refused.
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let hover = HOVER_BEFORE_PRESS.as_millis() as u64;
        let mut p = PressTracker::default();
        p.on_move(
            grey_rest_move(
                true,
                still,
                InputOrigin::Hardware,
                InputDevice::Mouse,
                false,
            ),
            at(0),
        );
        p.on_press(true, InputDevice::Mouse, at(hover * 3));
        assert_eq!(p.on_release(), Err(Refusal::NoMove));
        // The owner's hand then really moves it onto the button: that counts.
        let moved = cursor_moved(Some((100, 200)), (104, 201));
        p.on_move(
            grey_rest_move(
                true,
                moved,
                InputOrigin::Hardware,
                InputDevice::Mouse,
                false,
            ),
            at(hover * 4),
        );
        p.on_press(true, InputDevice::Mouse, at(hover * 5));
        assert_eq!(p.on_release(), Ok(()));
    }

    /// Task 28b: a refused release names the first gate it failed: a pointer that isn't real
    /// hardware first, then the press (a real one, after the rest).
    #[test] // spec: QA-088
    fn a_refused_release_names_the_gate_it_failed() {
        assert_eq!(release_refusal(real_release(), Ok(())), None);
        let unpressed = ButtonInput {
            pressed_by_hardware: false,
            ..real_release()
        };
        for why in [Refusal::NoRealPress, Refusal::NoRest] {
            assert_eq!(release_refusal(unpressed, Err(why)), Some(why), "{why:?}");
        }
        for input in [
            ButtonInput {
                origin: InputOrigin::Injected,
                ..real_release()
            },
            ButtonInput {
                origin: InputOrigin::Unavailable,
                ..real_release()
            },
            ButtonInput {
                device: InputDevice::Keyboard,
                ..real_release()
            },
            ButtonInput {
                sent_by_another_thread: true,
                ..real_release()
            },
            // Not real and not pressed: the pointer is the first gate.
            ButtonInput {
                origin: InputOrigin::Injected,
                ..unpressed
            },
        ] {
            let press = if input.pressed_by_hardware {
                Ok(())
            } else {
                Err(Refusal::NoRest)
            };
            assert_eq!(
                release_refusal(input, press),
                Some(Refusal::NotPointer),
                "{input:?}"
            );
        }
        // Released outside the button, or not a release: no click follows, nothing to say.
        let outside = ButtonInput {
            over_button: false,
            ..unpressed
        };
        assert_eq!(release_refusal(outside, Err(Refusal::NoRest)), None);
        let press = ButtonInput {
            msg: WM_LBUTTONDOWN,
            ..real_release()
        };
        assert_eq!(release_refusal(press, Ok(())), None);
    }

    /// Task 28b: a click on the accept button that doesn't count is put down to one gate: grey
    /// buttons first, then what the release found, and with no hardware release behind it (a
    /// key, a posted or sent message, `BM_CLICK`, UI Automation), the pointer.
    #[test] // spec: QA-088
    fn a_refused_click_is_put_down_to_the_first_gate_it_failed() {
        assert_eq!(click_refusal(real_click(), None), None);
        let unarmed = ButtonCommand {
            armed: false,
            ..real_click()
        };
        // Grey: whatever the release found.
        for release in [None, Some(Refusal::NoRest), Some(Refusal::NotPointer)] {
            let grey = ButtonCommand {
                enabled: false,
                ..unarmed
            };
            assert_eq!(
                click_refusal(grey, release),
                Some(Refusal::NotLiveYet),
                "{release:?}"
            );
        }
        let grey_armed = ButtonCommand {
            enabled: false,
            ..real_click()
        };
        assert_eq!(click_refusal(grey_armed, None), Some(Refusal::NotLiveYet));
        // The release's own verdict.
        for why in [Refusal::NotPointer, Refusal::NoRealPress, Refusal::NoRest] {
            assert_eq!(click_refusal(unarmed, Some(why)), Some(why), "{why:?}");
        }
        // No hardware release behind it, or sent from another thread while armed.
        assert_eq!(click_refusal(unarmed, None), Some(Refusal::NotPointer));
        let sent = ButtonCommand {
            sent_by_another_thread: true,
            ..real_click()
        };
        assert_eq!(click_refusal(sent, None), Some(Refusal::NotPointer));
        // Not a click of the accept button: nothing to say.
        let deny = ButtonCommand {
            from_accept: false,
            from_deny: true,
            ..unarmed
        };
        assert_eq!(click_refusal(deny, None), None);
        let other = ButtonCommand {
            clicked: false,
            ..unarmed
        };
        assert_eq!(click_refusal(other, None), None);
        let elsewhere = ButtonCommand {
            from_accept: false,
            ..unarmed
        };
        assert_eq!(click_refusal(elsewhere, None), None);

        // A press on the grey button reaches the window itself: the click that brought the
        // window to the front, or one before the second was up.
        assert_eq!(grey_click_refusal(true), Refusal::NotInFront);
        assert_eq!(grey_click_refusal(false), Refusal::NotLiveYet);
    }

    /// Task 28b: grey buttons say what they wait for; a refused click's message stays until the
    /// buttons change (they turn on, or the window loses the front), and live buttons say
    /// nothing.
    #[test] // spec: QA-088
    fn grey_buttons_say_what_they_wait_for_and_a_refusal_shows_until_they_change() {
        use ButtonsState::{Live, NotInFront, TurningOn};
        assert_eq!(buttons_state(false, false), NotInFront);
        assert_eq!(buttons_state(true, false), TurningOn);
        assert_eq!(buttons_state(true, true), Live);

        let turning_on = "The buttons turn on in a moment.";
        let mut s = StatusLine::default();
        assert_eq!(s.text("Merge"), turning_on);
        // Not in front: say so, once.
        assert!(s.tick(NotInFront));
        assert!(
            s.text("Merge")
                .starts_with("Bring this window to the front first")
        );
        assert!(!s.tick(NotInFront));
        // In front, still grey.
        assert!(s.tick(TurningOn));
        assert_eq!(s.text("Merge"), turning_on);
        // A click on the grey button: its message stays while they are still grey.
        s.refuse(Refusal::NotLiveYet, TurningOn);
        assert_eq!(s.text("Merge"), Refusal::NotLiveYet.message("Merge"));
        assert!(!s.tick(TurningOn));
        assert_eq!(s.text("Merge"), Refusal::NotLiveYet.message("Merge"));
        // Live: the message goes, and nothing is said.
        assert!(s.tick(Live));
        assert_eq!(s.text("Merge"), "");
        // A refusal while live stays while live, and the next one replaces it.
        s.refuse(Refusal::NoRest, Live);
        assert!(!s.tick(Live));
        assert_eq!(s.text("Merge"), Refusal::NoRest.message("Merge"));
        s.refuse(Refusal::NotPointer, Live);
        assert_eq!(s.text("Merge"), Refusal::NotPointer.message("Merge"));
        // Losing the front ends it.
        assert!(s.tick(NotInFront));
        assert!(
            s.text("Merge")
                .starts_with("Bring this window to the front first")
        );
        // The click that brought the window forward is judged once it is in front, so its
        // message outlives the next tick.
        s.refuse(Refusal::NotInFront, TurningOn);
        assert!(!s.tick(TurningOn));
        assert_eq!(s.text("Approve"), Refusal::NotInFront.message("Approve"));
    }

    // ---------------------------------------------------------------------------------------
    // Task 28c: the owner's click sequences, replayed through the window's own gates.
    // ---------------------------------------------------------------------------------------

    /// What Windows reports for the `WM_MOUSEMOVE` it makes itself under a still cursor: when
    /// the window comes to the top, when Merge turns on under the pointer, and after a button
    /// releases the capture. Microsoft documents only that `IMO_SYSTEM` is "the system has
    /// injected the input message"; what these moves report on this PC is unverified (Task 28
    /// check 11), so every replay runs under each answer.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Fakes {
        /// No such move is sent.
        NoMove,
        /// It reports `IMO_SYSTEM` (the likeliest reading of the docs).
        System,
        /// It reports `IMO_HARDWARE` with the mouse.
        Hardware,
    }

    const ALL_FAKES: [Fakes; 3] = [Fakes::NoMove, Fakes::System, Fakes::Hardware];

    /// Every [`Fakes`], with raw mouse input registered and with its registration failed.
    const WITH_AND_WITHOUT_RAW: [(Fakes, bool); 6] = [
        (Fakes::NoMove, true),
        (Fakes::NoMove, false),
        (Fakes::System, true),
        (Fakes::System, false),
        (Fakes::Hardware, true),
        (Fakes::Hardware, false),
    ];

    /// Where the cursor is, as Windows routes mouse input to the asking window.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Under {
        Outside,
        /// The read-only text (its moves and presses go to the EDIT, not the gates).
        Text,
        /// The window itself: the button row, and Merge while it is grey.
        Window,
        /// Merge while it is live.
        Merge,
    }

    // The window on the screen: 720x520. Merge is 110x30 at the bottom right.
    const WINDOW: (i32, i32, i32, i32) = (400, 200, 1120, 720);
    const TEXT: (i32, i32, i32, i32) = (410, 240, 1110, 660);
    const MERGE_RECT: (i32, i32, i32, i32) = (870, 672, 980, 702);
    const MERGE: (i32, i32) = (925, 687);
    const ROW: (i32, i32) = (840, 690);
    const BODY: (i32, i32) = (700, 400);
    const TASKBAR: (i32, i32) = (700, 1060);
    const START: (i32, i32) = (200, 300);

    fn inside_rect((x, y): (i32, i32), (l, t, r, b): (i32, i32, i32, i32)) -> bool {
        x >= l && x < r && y >= t && y < b
    }

    /// One owner click as the window judged it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Click {
        Accepted,
        /// The gate's log name.
        Refused(&'static str),
    }

    /// The asking window as Windows drives it, over [`ClickGate`] and [`StatusLine`], with the
    /// same calls `owner_wndproc` and `accept_button_proc` make.
    struct Replay {
        fakes: Fakes,
        gate: ClickGate,
        status: StatusLine,
        t0: Instant,
        ms: u64,
        next_tick: u64,
        in_front: bool,
        enabled: bool,
        cursor: (i32, i32),
        /// `TME_LEAVE` is armed on the window, or on Merge: the procedures arm it on every
        /// move they get (and the window's leave arms it on Merge when it carries the rest),
        /// and Windows posts a leave only while it is armed (review m1).
        window_armed: bool,
        button_armed: bool,
        /// Raw mouse input is registered (Task 28d): the hand's moves and clicks make it;
        /// warps and Windows' own moves make none (measured on build 26300).
        raw: bool,
        /// What raw input the owner's mouse makes: from a device (`Some(true)`, the
        /// default), with a null `hDevice` (`Some(false)`), or none.
        hand_raw: Option<bool>,
        /// The device the owner's hand moves the pointer with (a mouse by default).
        hand_device: InputDevice,
        /// The hand's raw input comes after each of its moves, not before (review m4).
        raw_after_move: bool,
        clicks: Vec<Click>,
        trace: Vec<String>,
    }

    impl Replay {
        /// The window opens (its first placement restarts the delay), in front or behind, with
        /// raw mouse input registered.
        fn open(fakes: Fakes, in_front: bool) -> Self {
            Self::open_with(fakes, in_front, true)
        }

        /// The same, with the raw input registration failed (Task 28d: Task 28c's rule).
        fn open_without_raw(fakes: Fakes, in_front: bool) -> Self {
            Self::open_with(fakes, in_front, false)
        }

        fn open_with(fakes: Fakes, in_front: bool, raw: bool) -> Self {
            let mut gate = ClickGate::new();
            gate.raw_registered(raw);
            gate.restart(Some(START));
            Self {
                fakes,
                gate,
                status: StatusLine::default(),
                t0: Instant::now(),
                ms: 0,
                next_tick: 250,
                in_front,
                enabled: false,
                cursor: START,
                window_armed: false,
                button_armed: false,
                raw,
                hand_raw: Some(true),
                hand_device: InputDevice::Mouse,
                raw_after_move: false,
                clicks: Vec::new(),
                trace: Vec::new(),
            }
        }

        fn now(&self) -> Instant {
            self.t0 + Duration::from_millis(self.ms)
        }

        fn under(&self, p: (i32, i32)) -> Under {
            if inside_rect(p, MERGE_RECT) && self.enabled {
                Under::Merge
            } else if inside_rect(p, TEXT) {
                Under::Text
            } else if inside_rect(p, WINDOW) {
                Under::Window
            } else {
                Under::Outside
            }
        }

        fn msg(&self, origin: InputOrigin) -> PointerMsg {
            PointerMsg {
                origin,
                device: self.hand_device,
                sent: false,
                at: Some(self.cursor),
                message_at: Some(self.cursor),
            }
        }

        /// A move at the cursor, to whatever is under it.
        fn deliver(&mut self, origin: InputOrigin) {
            let m = self.msg(origin);
            self.deliver_msg(m);
        }

        /// A move message `m` to whatever is under the cursor. The procedures arm `TME_LEAVE`
        /// on every move they get.
        fn deliver_msg(&mut self, m: PointerMsg) {
            let now = self.now();
            match self.under(self.cursor) {
                Under::Merge => {
                    self.gate.button_move(true, m, now);
                    self.button_armed = true;
                    self.gate.button_tracking(true);
                }
                Under::Window => {
                    let grey = inside_rect(self.cursor, MERGE_RECT) && !self.enabled;
                    self.gate.window_move(grey, m, now);
                    self.window_armed = true;
                    self.gate.window_tracking(true);
                }
                Under::Text | Under::Outside => {}
            }
        }

        /// Another program posts (`sent: false`) or sends a `WM_MOUSEMOVE` to whatever is under
        /// the cursor, claiming the cursor is at `claim` (review I1).
        fn forged_move(&mut self, claim: (i32, i32), sent: bool) {
            self.deliver_msg(PointerMsg {
                origin: if sent {
                    InputOrigin::Hardware
                } else {
                    InputOrigin::Unavailable
                },
                device: InputDevice::Mouse,
                sent,
                at: Some(claim),
                message_at: Some(claim),
            });
        }

        /// The leaves Windows posts when the cursor goes from `from` to `to`: only to a window
        /// whose `TME_LEAVE` is armed.
        fn leave(&mut self, from: Under, to: Under) {
            if from == Under::Merge && to != Under::Merge && self.button_armed {
                self.button_armed = false;
                self.gate.button_leave();
            }
            if from == Under::Window && to != Under::Window && self.window_armed {
                self.window_armed = false;
                let carried = to == Under::Merge;
                self.gate.window_leave(carried);
                // Carrying the rest, the window arms the leave on Merge.
                self.button_armed |= carried;
            }
        }

        /// The cursor goes to `p` by moves this window never gets: over another program's
        /// window lying on top of it, or merged by Windows into one (review I2).
        fn unseen_to(&mut self, p: (i32, i32)) {
            let from = self.under(self.cursor);
            self.leave(from, Under::Outside);
            self.cursor = p;
        }

        /// The move Windows makes itself under a still cursor, as [`Fakes`] says.
        fn fake_move(&mut self) {
            match self.fakes {
                Fakes::NoMove => {}
                Fakes::System => self.deliver(InputOrigin::System),
                Fakes::Hardware => self.deliver(InputOrigin::Hardware),
            }
        }

        /// The cursor goes to `p` (moved by `origin`), with the leaves Windows posts on the way.
        fn place(&mut self, p: (i32, i32), origin: InputOrigin) {
            let (from, to) = (self.under(self.cursor), self.under(p));
            self.leave(from, to);
            self.cursor = p;
            self.deliver(origin);
        }

        /// Time passes; the live timer ticks every 250 ms.
        fn wait(&mut self, ms: u64) {
            let end = self.ms + ms;
            while self.next_tick <= end {
                self.ms = self.next_tick;
                self.next_tick += 250;
                let live = self.gate.tick(self.in_front, self.now());
                if live != self.enabled {
                    let was = self.under(self.cursor);
                    self.enabled = live;
                    // Merge turned on (or off) under the pointer: Windows routes it anew.
                    if inside_rect(self.cursor, MERGE_RECT) {
                        let now_under = self.under(self.cursor);
                        self.leave(was, now_under);
                        self.fake_move();
                    }
                }
                self.status
                    .tick(buttons_state(self.in_front, self.gate.is_live(self.now())));
            }
            self.ms = end;
        }

        /// A `WM_INPUT` from the owner's mouse (`last` is its motion), if raw input is
        /// registered and the mouse makes it ([`Self::hand_raw`]).
        fn raw_from_device(&mut self, last: (i32, i32)) {
            if self.raw
                && let Some(device) = self.hand_raw
            {
                let now = self.now();
                let r = RawMouse {
                    device,
                    absolute: false,
                    last,
                };
                self.gate.raw_input(r, now);
            }
        }

        /// The owner's hand settles on the mouse: raw `packets`, `gaps` ms apart, that move
        /// no pixel, so no move follows them.
        fn settle(&mut self, packets: &[(u64, (i32, i32))]) {
            for &(gap, last) in packets {
                self.wait(gap);
                self.raw_from_device(last);
            }
        }

        /// The owner's hand moves the mouse, but this window gets no move: the cursor went
        /// over another window, or Windows merged the moves with a warp that follows.
        fn hand_moves_unseen(&mut self) {
            self.raw_from_device((-12, 3));
        }

        /// The owner's hand moves the pointer to `p` in `steps` real moves, 10 ms apart; each
        /// makes raw input first.
        fn move_to(&mut self, p: (i32, i32), steps: i32) {
            let (x0, y0) = self.cursor;
            for i in 1..=steps {
                self.wait(10);
                let q = (x0 + (p.0 - x0) * i / steps, y0 + (p.1 - y0) * i / steps);
                let last = (q.0 - self.cursor.0, q.1 - self.cursor.1);
                if self.raw_after_move {
                    self.place(q, InputOrigin::Hardware);
                    self.raw_from_device(last);
                } else {
                    self.raw_from_device(last);
                    self.place(q, InputOrigin::Hardware);
                }
            }
        }

        /// The hand's last move, made by Windows at `a`; while this window handles it, another
        /// program warps the cursor to `y`, so `GetCursorPos` reads `y`; then the warp's own
        /// move comes (review m1).
        fn last_move_warped(&mut self, a: (i32, i32), y: (i32, i32)) {
            self.wait(10);
            self.raw_from_device((a.0 - self.cursor.0, a.1 - self.cursor.1));
            let (from, to) = (self.under(self.cursor), self.under(y));
            self.leave(from, to);
            self.cursor = y;
            self.deliver_msg(PointerMsg {
                origin: InputOrigin::Hardware,
                device: self.hand_device,
                sent: false,
                at: Some(y),
                message_at: Some(a),
            });
            self.deliver(InputOrigin::System);
        }

        /// One of Windows' own moves under the still cursor, as measured on build 26300: it
        /// comes by itself whenever a window anywhere changes, and reports `IMO_SYSTEM`.
        fn background_move(&mut self) {
            self.deliver(InputOrigin::System);
        }

        /// A small real move on the button and back, as the status line asks.
        fn nudge(&mut self) {
            let (x, y) = self.cursor;
            self.move_to((x + 2, y + 1), 2);
            self.move_to((x, y), 2);
        }

        /// Another program warps the cursor to `p` (`SetCursorPos`).
        fn warp(&mut self, p: (i32, i32)) {
            self.place(p, InputOrigin::System);
        }

        /// The window or Merge slides under the still cursor: the delay restarts.
        /// Another program warps the cursor to `p` and at once makes Windows send one of its own
        /// moves (it shows or hides a window of its own, or changes the z-order), before this
        /// window's thread pumps its queue: Windows merges the two into one move at `p`, which
        /// reports what Windows' own moves report (Task 28c re-review N1).
        fn merged_warp(&mut self, p: (i32, i32)) {
            let origin = match self.fakes {
                // No own move: only the warp's.
                Fakes::NoMove | Fakes::System => InputOrigin::System,
                Fakes::Hardware => InputOrigin::Hardware,
            };
            self.place(p, origin);
        }

        fn slide(&mut self) {
            self.gate.restart(Some(self.cursor));
            self.enabled = false;
            self.fake_move();
        }

        /// The owner clicks the flashing taskbar button: the window comes to the front, and
        /// the cursor is on the taskbar.
        fn activate_by_taskbar(&mut self) {
            assert_eq!(self.under(self.cursor), Under::Outside);
            self.in_front = true;
        }

        fn refuse(&mut self, refusal: Refusal) {
            let state = buttons_state(self.in_front, self.gate.is_live(self.now()));
            self.status.refuse(refusal, state);
            self.clicks.push(Click::Refused(refusal.name()));
        }

        /// The owner presses and releases (80 ms) without moving.
        fn click(&mut self) {
            self.click_from(InputOrigin::Hardware);
        }

        fn click_from(&mut self, origin: InputOrigin) {
            let under = self.under(self.cursor);
            if !self.in_front && under != Under::Outside {
                // WM_MOUSEACTIVATE, then the window comes to the top under the cursor.
                self.gate.mouse_activate();
                self.in_front = true;
                self.fake_move();
            }
            let at = self.ms;
            match under {
                Under::Window => {
                    let grey = inside_rect(self.cursor, MERGE_RECT) && !self.enabled;
                    if let Some(refusal) = self.gate.window_press(grey) {
                        self.refuse(refusal);
                    }
                    self.wait(80);
                }
                Under::Merge => {
                    // A real click's raw input: buttons, no motion.
                    if origin == InputOrigin::Hardware {
                        self.raw_from_device((0, 0));
                    }
                    let (m, now) = (self.msg(origin), self.now());
                    self.gate.button_press(true, m, now);
                    self.wait(80);
                    let (armed, refusal) = self.gate.button_release(true, self.msg(origin));
                    // Within the button's own procedure: ReleaseCapture, then BN_CLICKED.
                    self.gate.capture_changed();
                    let command = ButtonCommand {
                        clicked: true,
                        from_accept: true,
                        from_deny: false,
                        armed,
                        sent_by_another_thread: false,
                        enabled: self.enabled && self.gate.is_live(self.now()),
                    };
                    self.gate.release_done();
                    match choice_from_command(command) {
                        Some(Choice::Accepted) => self.clicks.push(Click::Accepted),
                        _ => {
                            let refusal = click_refusal(command, refusal).expect("a refusal");
                            self.refuse(refusal);
                        }
                    }
                    // The capture went: Windows makes a move under the cursor.
                    self.fake_move();
                }
                Under::Text | Under::Outside => self.wait(80),
            }
            if inside_rect(self.cursor, MERGE_RECT) {
                let said = self.status.text("Merge");
                self.trace
                    .push(format!("{at:>5} ms: {:?} | {said}", self.clicks.last()));
            }
        }

        fn report(&self, name: &str) {
            println!("{name} ({:?}, raw {}):", self.fakes, self.raw);
            for line in &self.trace {
                println!("  {line}");
            }
        }
    }

    fn refused(name: &'static str) -> Click {
        Click::Refused(name)
    }

    /// Each natural sequence: the first refusal it gets, then what follows when the owner
    /// clicks again without moving and, if that is refused too, moves a little as the status
    /// line says. With Windows' own moves reported as `IMO_SYSTEM`, a still click after Merge
    /// turns on, after the window comes to the top or after a click can't be told from a warp,
    /// so the line says to move and the click after that merges. With raw input (Task 28d) a
    /// still click after a click on live Merge counts (`kept_with_raw`): the place is known
    /// and the hand didn't move. After a click on grey Merge, or Merge turning on under the
    /// pointer, the place is unknown (Task 28c re-review N2, review I2), so it still doesn't.
    fn assert_second_or_after_a_nudge(r: &Replay, first: &'static str, kept_with_raw: bool) {
        assert_eq!(r.clicks[0], refused(first), "{:?}", r.fakes);
        match r.fakes {
            Fakes::NoMove | Fakes::Hardware => {
                assert_eq!(r.clicks[1..], [Click::Accepted], "{:?}", r.fakes)
            }
            Fakes::System if r.raw && kept_with_raw => {
                assert_eq!(r.clicks[1..], [Click::Accepted], "{:?}", r.fakes)
            }
            Fakes::System => assert_eq!(
                r.clicks[1..],
                [refused("no_move"), Click::Accepted],
                "{:?}",
                r.fakes
            ),
        }
    }

    /// Clicks again without moving, then (if still refused) moves a little and clicks.
    fn click_again_then_nudge(r: &mut Replay) {
        r.click();
        r.wait(1_000);
        if r.clicks.last() != Some(&Click::Accepted) {
            r.nudge();
            r.wait(300);
            r.click();
        }
    }

    /// The window opens behind and flashes; the owner clicks its taskbar button, moves onto
    /// Merge while it is still grey, holds still and clicks; then clicks again without moving.
    #[test] // spec: QA-088
    fn replay_behind_then_taskbar_then_still_clicks_on_merge() {
        for (fakes, raw) in WITH_AND_WITHOUT_RAW {
            let mut r = Replay::open_with(fakes, false, raw);
            r.wait(2_000);
            r.move_to(TASKBAR, 20);
            r.activate_by_taskbar();
            r.wait(100);
            r.move_to(ROW, 30);
            r.move_to(MERGE, 8);
            r.wait(200);
            r.click(); // about 0.7 s after it came to the front: grey
            r.wait(1_500);
            click_again_then_nudge(&mut r);
            r.report("behind, taskbar");
            assert_second_or_after_a_nudge(&r, "not_live_yet", false);
        }
    }

    /// The window opens behind; the owner's very first click is on grey Merge itself, which
    /// brings the window forward; then they wait and click again without moving.
    #[test] // spec: QA-088
    fn replay_behind_then_first_click_on_merge_brings_it_forward() {
        for (fakes, raw) in WITH_AND_WITHOUT_RAW {
            let mut r = Replay::open_with(fakes, false, raw);
            r.wait(2_000);
            r.move_to(ROW, 30);
            r.move_to(MERGE, 8);
            r.wait(300);
            r.click(); // brings the window forward
            r.wait(1_500);
            click_again_then_nudge(&mut r);
            r.report("behind, first click on Merge");
            assert_second_or_after_a_nudge(&r, "not_in_front", false);
        }
    }

    /// The window opens behind; the owner clicks into the text to bring it forward, then moves
    /// onto Merge and clicks while it is grey, then again without moving.
    #[test] // spec: QA-088
    fn replay_behind_then_click_in_the_text_then_merge() {
        for (fakes, raw) in WITH_AND_WITHOUT_RAW {
            let mut r = Replay::open_with(fakes, false, raw);
            r.wait(2_000);
            r.move_to(BODY, 20);
            r.click(); // into the text: the window comes forward
            r.move_to(ROW, 30);
            r.move_to(MERGE, 8);
            r.wait(150);
            r.click();
            r.wait(1_500);
            click_again_then_nudge(&mut r);
            r.report("behind, click in the text");
            assert_second_or_after_a_nudge(&r, "not_live_yet", false);
        }
    }

    /// The window opens in front. The owner reads, moves onto live Merge and clicks at once
    /// (too soon), then clicks again without moving. A deliberate first click merges at once.
    #[test] // spec: QA-088
    fn replay_in_front_a_quick_click_then_a_still_click() {
        for (fakes, raw) in WITH_AND_WITHOUT_RAW {
            let mut r = Replay::open_with(fakes, true, raw);
            r.wait(3_000);
            r.move_to(ROW, 30);
            r.move_to(MERGE, 5);
            r.click(); // 50 ms after reaching Merge
            r.wait(700);
            click_again_then_nudge(&mut r);
            r.report("in front, quick click");
            assert_second_or_after_a_nudge(&r, "no_rest", true);

            let mut r = Replay::open_with(fakes, true, raw);
            r.wait(3_000);
            r.move_to(ROW, 30);
            r.move_to(MERGE, 5);
            r.wait(300);
            r.click();
            r.report("in front, deliberate click");
            assert_eq!(r.clicks, [Click::Accepted], "{fakes:?}, raw {raw}");
        }
    }

    /// Every natural first refusal, then the owner does what the status line says (moves the
    /// pointer a little on Merge and clicks): the second deliberate click merges, whatever
    /// Windows reports for its own moves.
    #[test] // spec: QA-088
    fn replay_doing_what_the_status_line_says_merges_on_the_second_click() {
        for (fakes, raw) in WITH_AND_WITHOUT_RAW {
            // Behind; taskbar; onto grey Merge; clicked too early.
            let mut r = Replay::open_with(fakes, false, raw);
            r.wait(2_000);
            r.move_to(TASKBAR, 20);
            r.activate_by_taskbar();
            r.move_to(MERGE, 30);
            r.click();
            r.wait(1_500);
            r.nudge();
            r.wait(300);
            r.click();
            r.report("behind, taskbar, as the line says");
            assert_eq!(
                r.clicks,
                [refused("not_live_yet"), Click::Accepted],
                "{fakes:?}, raw {raw}"
            );

            // Behind; the first click on grey Merge brings it forward.
            let mut r = Replay::open_with(fakes, false, raw);
            r.wait(2_000);
            r.move_to(MERGE, 30);
            r.click();
            r.wait(1_500);
            r.nudge();
            r.wait(300);
            r.click();
            assert_eq!(
                r.clicks,
                [refused("not_in_front"), Click::Accepted],
                "{fakes:?}, raw {raw}"
            );

            // In front; the window opened under the still pointer, right on Merge.
            let mut r = Replay::open_with(fakes, true, raw);
            r.cursor = MERGE;
            r.gate.restart(Some(MERGE));
            r.wait(2_000);
            r.click();
            r.wait(500);
            r.nudge();
            r.wait(300);
            r.click();
            assert_eq!(
                r.clicks,
                [refused("no_move"), Click::Accepted],
                "{fakes:?}, raw {raw}"
            );

            // In front; too quick a click.
            let mut r = Replay::open_with(fakes, true, raw);
            r.wait(3_000);
            r.move_to(ROW, 30);
            r.move_to(MERGE, 5);
            r.click();
            r.wait(500);
            r.nudge();
            r.wait(300);
            r.click();
            assert_eq!(
                r.clicks,
                [refused("no_rest"), Click::Accepted],
                "{fakes:?}, raw {raw}"
            );
        }
    }

    /// Task 28c: a click's own release (the button gives the capture back after a real release
    /// inside it) ends the press but not the rest. Any other loss of the capture, a cancelled
    /// mode, or a release that isn't a real one inside the button ends both.
    #[test] // spec: QA-088
    fn only_the_buttons_own_real_release_keeps_the_rest() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let real = |x: i32| PointerMsg {
            origin: InputOrigin::Hardware,
            device: InputDevice::Mouse,
            sent: false,
            at: Some((x, 10)),
            message_at: Some((x, 10)),
        };
        // Rested from 0 ms; a press at 50 ms is too soon.
        let rested = || {
            let mut g = ClickGate::new();
            g.restart(Some((0, 0)));
            g.button_tracking(true);
            assert!(g.button_move(true, real(1), at(0)));
            g.button_press(true, real(1), at(50));
            g
        };
        // The owner's own release: the rest goes on, so a still press at 400 ms counts.
        let mut g = rested();
        assert_eq!(
            g.button_release(true, real(1)),
            (false, Some(Refusal::NoRest))
        );
        g.capture_changed();
        g.release_done();
        g.button_press(true, real(1), at(400));
        assert_eq!(g.button_release(true, real(1)), (true, None));
        // The press itself is still used up: a second release without a press arms nothing.
        g.capture_changed();
        g.release_done();
        assert!(!g.button_release(true, real(1)).0);

        // A posted (not hardware) release, a release outside the button, a capture lost at
        // any other time, or WM_CANCELMODE: the rest is gone.
        let posted = PointerMsg {
            origin: InputOrigin::Unavailable,
            ..real(1)
        };
        for end in 0..4 {
            let mut g = rested();
            match end {
                0 => {
                    g.button_release(true, posted);
                    g.capture_changed();
                    g.release_done();
                }
                1 => {
                    g.button_release(false, real(1));
                    g.capture_changed();
                    g.release_done();
                }
                2 => g.capture_changed(),
                _ => g.cancel_mode(),
            }
            g.button_press(true, real(1), at(400));
            assert_eq!(
                g.button_release(true, real(1)),
                (false, Some(Refusal::NoMove)),
                "end {end}"
            );
        }

        // A hardware-reported move that left the cursor where it was neither counts nor ends
        // the rest; one that isn't hardware ends it even at the same place.
        let mut g = rested();
        g.button_release(true, real(1));
        g.capture_changed();
        g.release_done();
        assert!(!g.button_move(true, real(1), at(200)));
        g.button_press(true, real(1), at(400));
        assert_eq!(g.button_release(true, real(1)), (true, None));
        let mut g = rested();
        g.button_release(true, real(1));
        g.capture_changed();
        g.release_done();
        let system = PointerMsg {
            origin: InputOrigin::System,
            ..real(1)
        };
        assert!(!g.button_move(true, system, at(200)));
        g.button_press(true, real(1), at(400));
        assert_eq!(g.button_release(true, real(1)).1, Some(Refusal::NoMove));
        // After a restart, a hardware move at the cursor's place then never counts.
        let mut g = ClickGate::new();
        g.restart(Some((1, 10)));
        g.button_tracking(true);
        assert!(!g.button_move(true, real(1), at(0)));
        g.button_press(true, real(1), at(400));
        assert_eq!(g.button_release(true, real(1)).1, Some(Refusal::NoMove));

        // Review I1: a posted or sent move never records where the cursor is, whatever it
        // claims; a real move then at the cursor's true place still doesn't count.
        for forged in [
            PointerMsg {
                origin: InputOrigin::Unavailable,
                ..real(5)
            },
            PointerMsg {
                sent: true,
                ..real(5)
            },
        ] {
            let mut g = ClickGate::new();
            g.restart(Some((1, 10)));
            g.button_tracking(true);
            assert!(!g.button_move(true, forged, at(0)));
            assert!(!g.button_move(true, real(1), at(10)), "{forged:?}");
        }
        // Review I2: after a leave, or while the leave isn't armed, the first move back only
        // records where the cursor is; the next one at a new place counts.
        for forget in 0..2 {
            let mut g = ClickGate::new();
            g.restart(Some((0, 0)));
            g.button_tracking(true);
            assert!(g.button_move(true, real(1), at(0)));
            if forget == 0 {
                g.button_leave();
            } else {
                g.button_tracking(false);
            }
            g.button_tracking(false);
            assert!(!g.button_move(true, real(7), at(10)), "forget {forget}");
            g.button_tracking(true);
            assert!(!g.button_move(true, real(7), at(20)), "forget {forget}");
            assert!(g.button_move(true, real(8), at(30)), "forget {forget}");
        }
        // The window's own leave forgets it too, even when it carries the rest onto Merge.
        let mut g = ClickGate::new();
        g.restart(Some((0, 0)));
        g.window_tracking(true);
        assert!(g.window_move(true, real(1), at(0)));
        g.window_leave(true);
        assert!(!g.button_move(true, real(9), at(10)));
    }

    /// Clicks, then a still click 500 ms later, then another: none may merge.
    fn three_still_clicks(r: &mut Replay) {
        for _ in 0..3 {
            r.click();
            r.wait(500);
        }
    }

    /// Review I1: a posted or sent move, whatever point it claims, never sets where the gate
    /// thinks the cursor is. Otherwise, after a warp or a slide, Windows' own move under the
    /// still cursor would look like a move somewhere new and count towards the rest.
    #[test] // spec: QA-088, QA-036
    fn replay_posted_or_sent_moves_cant_plant_a_cursor_position() {
        for (fakes, raw) in WITH_AND_WITHOUT_RAW {
            for sent in [false, true] {
                // A warp onto live Merge, then a forged move claiming the cursor is elsewhere.
                let mut r = Replay::open_with(fakes, true, raw);
                r.wait(3_000);
                r.move_to(ROW, 30);
                r.wait(500);
                r.warp(MERGE);
                r.forged_move((0, 0), sent);
                three_still_clicks(&mut r);
                r.report("warp, then a forged move");
                assert!(
                    r.clicks.iter().all(|c| *c == refused("no_move")),
                    "warp, {fakes:?}, raw {raw}, sent {sent}: {:?}",
                    r.clicks
                );

                // Merge slides under the parked pointer; a forged move claims it is elsewhere;
                // Merge turns on again under the pointer.
                let mut r = Replay::open_with(fakes, true, raw);
                r.wait(3_000);
                r.move_to((MERGE.0 - 200, MERGE.1), 20);
                r.wait(500);
                r.cursor = MERGE;
                r.slide();
                r.forged_move((0, 0), sent);
                r.wait(1_500);
                three_still_clicks(&mut r);
                assert!(
                    r.clicks.iter().all(|c| *c == refused("no_move")),
                    "slide, {fakes:?}, raw {raw}, sent {sent}: {:?}",
                    r.clicks
                );
            }
        }
    }

    /// Review I2: the cursor reaches Merge by moves this window never gets (over another
    /// program's window lying on top of it, which then goes). Windows' own move when Merge is
    /// uncovered under the still hand must not count as a move somewhere new: after a leave
    /// the position is unknown, and the first move back only records it.
    #[test] // spec: QA-088, QA-036
    fn replay_moves_this_window_never_saw_dont_count() {
        for (fakes, raw) in WITH_AND_WITHOUT_RAW {
            let mut r = Replay::open_with(fakes, true, raw);
            r.wait(3_000);
            r.move_to(ROW, 30);
            r.wait(500);
            // An overlay lies over Merge; the hand moves onto it, then it goes.
            r.unseen_to(MERGE);
            r.fake_move();
            r.wait(500);
            three_still_clicks(&mut r);
            r.report("unseen moves");
            assert!(
                r.clicks.iter().all(|c| *c == refused("no_move")),
                "{fakes:?}, raw {raw}: {:?}",
                r.clicks
            );
            // The same while Merge is still grey, then it turns on under the pointer.
            let mut r = Replay::open_with(fakes, false, raw);
            r.wait(2_000);
            r.move_to(TASKBAR, 20);
            r.activate_by_taskbar();
            r.move_to(ROW, 10);
            r.unseen_to(MERGE);
            r.fake_move();
            r.wait(1_500);
            three_still_clicks(&mut r);
            assert!(
                r.clicks.iter().all(|c| *c == refused("no_move")),
                "grey, {fakes:?}, raw {raw}: {:?}",
                r.clicks
            );
        }
    }

    /// Task 28c re-review N1, **pinned as open** (ruled a documented residual 2026-10-25): a warp
    /// merged with Windows' own move into one move. While Merge is grey, the cursor is on the
    /// window's own row; another program warps it onto grey Merge and at once triggers one of
    /// Windows' own moves, so the window gets a single move at the new place. If Windows' own
    /// moves report `IMO_HARDWARE`, that move is a real move somewhere new, the rest starts,
    /// and a still owner click once Merge is live merges. No `GetCursorPos` rule can tell it
    /// from the hand. **Measured on Windows build 26300, 2026-10-25: warps, Windows' own moves
    /// and the merged case report `IMO_SYSTEM`; warps make no raw input** (the pointer-source
    /// probe), so on that build this is the System row, refused. The Hardware row is still open:
    /// it could be closed by also requiring device raw motion before a real move counts (with
    /// the per-rest confirmation of Task 28d review I1), since the hand's moves come with raw
    /// input and a warp makes none. That isn't done, because build 26300 never reports Hardware
    /// here; it stays on the backlog. Kept as a pin, because what Windows reports here is
    /// undocumented and a later build could change it. The Merge gate is defence in depth:
    /// every unlock still needs the owner's PIN at the Windows Hello prompt. When it is closed,
    /// the Hardware row becomes all `no_move`.
    #[test] // spec: QA-088, QA-036
    fn replay_a_warp_merged_with_windows_own_move_is_open_under_hardware() {
        for (fakes, raw) in WITH_AND_WITHOUT_RAW {
            let mut r = Replay::open_with(fakes, true, raw);
            r.move_to(ROW, 20);
            r.wait(100);
            r.merged_warp(MERGE);
            r.wait(1_500);
            three_still_clicks(&mut r);
            r.report("merged warp");
            match fakes {
                Fakes::NoMove | Fakes::System => assert!(
                    r.clicks.iter().all(|c| *c == refused("no_move")),
                    "{fakes:?}, raw {raw}: {:?}",
                    r.clicks
                ),
                // OPEN, not seen on build 26300: a still owner click merges.
                Fakes::Hardware => assert_eq!(r.clicks[0], Click::Accepted, "{fakes:?}, raw {raw}"),
            }
        }
    }

    /// The owner's hand moves onto live Merge and stops there.
    fn rest_on_merge(r: &mut Replay) {
        r.wait(3_000);
        r.move_to(ROW, 30);
        r.move_to(MERGE, 5);
    }

    /// The owner rests on Merge among Windows' own background moves, clicks 100 ms after the
    /// hand stopped, then clicks again without moving.
    fn still_clicks_among_background_moves(r: &mut Replay) {
        rest_on_merge(r);
        r.wait(40);
        r.background_move();
        r.wait(40);
        r.background_move();
        r.wait(20);
        r.click(); // 100 ms after the hand stopped
        r.wait(200);
        r.background_move();
        r.wait(300);
        r.background_move();
        r.wait(100);
        r.click();
    }

    /// The owner rests on Merge for a second of Windows' own background moves, then clicks.
    fn a_click_after_a_second_of_background_moves(r: &mut Replay) {
        rest_on_merge(r);
        for _ in 0..4 {
            r.wait(250);
            r.background_move();
        }
        r.wait(100);
        r.click();
    }

    /// Task 28d: Windows' own moves at the same place (measured on build 26300: about 0.9
    /// bursts a second by themselves, whenever a window anywhere changes, all `IMO_SYSTEM`)
    /// keep the rest the owner's hand made, while no more than jitter came since. A still click
    /// counts once the hand has rested 150 ms; one sooner is `no_rest`, and a still click after
    /// it counts.
    #[test] // spec: QA-088
    fn replay_a_still_click_after_windows_own_moves_at_the_same_place_counts() {
        for fakes in ALL_FAKES {
            let mut r = Replay::open(fakes, true);
            still_clicks_among_background_moves(&mut r);
            r.report("background moves");
            assert_eq!(r.clicks, [refused("no_rest"), Click::Accepted], "{fakes:?}");

            let mut r = Replay::open(fakes, true);
            a_click_after_a_second_of_background_moves(&mut r);
            assert_eq!(r.clicks, [Click::Accepted], "{fakes:?}");
        }
    }

    /// Task 28d: the case Task 28c rejected stays closed. The hand rested on Merge, then moved
    /// away where this window saw no move of it (Windows merged it with what came next), and a
    /// warp put the cursor back on the very same pixel: the window gets one own move at the old
    /// place. The raw motion says the hand moved, so no still click counts, whichever of the
    /// warp's move and the `WM_INPUT` this window gets first.
    #[test] // spec: QA-088, QA-036
    fn replay_the_hand_moves_away_and_a_warp_puts_the_cursor_back_never_merges() {
        for (fakes, raw) in WITH_AND_WITHOUT_RAW {
            for warp_first in [false, true] {
                let mut r = Replay::open_with(fakes, true, raw);
                rest_on_merge(&mut r);
                r.wait(500);
                if warp_first {
                    r.warp(MERGE);
                    r.hand_moves_unseen();
                } else {
                    r.hand_moves_unseen();
                    r.warp(MERGE);
                }
                r.wait(200);
                r.background_move();
                three_still_clicks(&mut r);
                r.report("hand away, warped back");
                assert!(
                    r.clicks.iter().all(|c| *c == refused("no_move")),
                    "{fakes:?}, raw {raw}, warp first {warp_first}: {:?}",
                    r.clicks
                );
            }
        }
    }

    /// Task 28d: an own move at a new place still ends the rest, raw input or not (here a warp
    /// within Merge after the hand rested on it), and Windows' own moves there after it don't
    /// bring the rest back.
    #[test] // spec: QA-088, QA-036
    fn replay_an_own_move_at_a_new_place_ends_the_rest() {
        for (fakes, raw) in WITH_AND_WITHOUT_RAW {
            let mut r = Replay::open_with(fakes, true, raw);
            rest_on_merge(&mut r);
            r.wait(500);
            r.warp((MERGE.0 + 3, MERGE.1 + 1));
            r.wait(200);
            r.background_move();
            three_still_clicks(&mut r);
            r.report("own move at a new place");
            assert!(
                r.clicks.iter().all(|c| *c == refused("no_move")),
                "{fakes:?}, raw {raw}: {:?}",
                r.clicks
            );
        }
    }

    /// Task 28d: raw motion from the mouse beyond the jitter bound (here 15 counts) ends the
    /// rest even when this window gets no move at all. Clicks are refused until a real move on
    /// Merge and 150 ms of rest after it.
    #[test] // spec: QA-088, QA-036
    fn replay_raw_motion_without_a_move_ends_the_rest() {
        for fakes in ALL_FAKES {
            let mut r = Replay::open(fakes, true);
            rest_on_merge(&mut r);
            r.wait(500);
            r.hand_moves_unseen();
            r.click();
            r.wait(300);
            r.click();
            r.wait(300);
            r.move_to((MERGE.0 + 2, MERGE.1 + 1), 1);
            r.wait(100);
            r.click(); // 100 ms after the real move
            r.wait(100);
            r.click();
            r.report("raw motion, no move");
            assert_eq!(
                r.clicks,
                [
                    refused("no_move"),
                    refused("no_move"),
                    refused("no_rest"),
                    Click::Accepted
                ],
                "{fakes:?}"
            );
        }
    }

    /// Task 28d: when raw input can't be registered, or stops being this window's, the gate
    /// keeps Task 28c's rule: every own move ends the rest, even at the same place.
    #[test] // spec: QA-088, QA-036
    fn replay_without_raw_input_every_own_move_ends_the_rest() {
        for fakes in ALL_FAKES {
            for lost in [false, true] {
                let open = |in_front| {
                    let mut r = if lost {
                        Replay::open(fakes, in_front)
                    } else {
                        Replay::open_without_raw(fakes, in_front)
                    };
                    if lost {
                        r.gate.raw_lost();
                    }
                    r
                };
                let mut r = open(true);
                assert!(!r.gate.raw_on(), "lost {lost}");
                still_clicks_among_background_moves(&mut r);
                r.report("no raw input");
                assert_eq!(
                    r.clicks,
                    [refused("no_move"), refused("no_move")],
                    "{fakes:?}, lost {lost}"
                );

                let mut r = open(true);
                a_click_after_a_second_of_background_moves(&mut r);
                assert_eq!(r.clicks, [refused("no_move")], "{fakes:?}, lost {lost}");
            }
        }
    }

    /// Task 28d round 1: how much raw motion is the hand moving. Every packet's `|dx| + |dy|`
    /// is added up since the last real move, from a device or with a null `hDevice`; up to
    /// [`RAW_JITTER_COUNTS`] is a hand settling on the mouse and keeps the rest, more ends it.
    /// An absolute position other than the last one (the first one included) is the hand
    /// moving.
    #[test] // spec: QA-088
    fn raw_motion_ends_the_rest_only_beyond_the_jitter_bound() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let real = |x: i32| PointerMsg {
            origin: InputOrigin::Hardware,
            device: InputDevice::Mouse,
            sent: false,
            at: Some((x, 10)),
            message_at: Some((x, 10)),
        };
        // A gate rested on the button from 0 ms (after `before`), then fed `raws`; whether a
        // press at 400 ms still counts.
        let still_rested = |before: &[RawMouse], raws: &[RawMouse]| {
            let mut g = ClickGate::new();
            g.raw_registered(true);
            for r in before {
                g.raw_input(*r, at(0));
            }
            g.restart(Some((0, 0)));
            g.button_tracking(true);
            assert!(g.button_move(true, real(1), at(0)));
            for r in raws {
                g.raw_input(*r, at(100));
            }
            g.button_press(true, real(1), at(400));
            g.button_release(true, real(1)).0
        };
        let rel = |device, last| RawMouse {
            device,
            absolute: false,
            last,
        };
        let abs = |device, last| RawMouse {
            device,
            absolute: true,
            last,
        };
        assert!(still_rested(&[], &[]));
        assert!(still_rested(&[], &[rel(true, (0, 0))]), "buttons only");
        // The probe's recorded settling (measured, so any bound must allow it).
        const { assert!(RAW_JITTER_COUNTS >= 3) };
        assert!(
            still_rested(&[], &[rel(true, (1, 0))]),
            "the probe's (1, 0)"
        );
        assert!(
            still_rested(&[], &[rel(true, (0, -2)), rel(true, (-1, 0))]),
            "the probe's (0, -2), (-1, 0)"
        );
        // The bound itself, in one packet or a count at a time, and one count past it.
        let bound = RAW_JITTER_COUNTS as i32;
        let half = bound / 2;
        assert!(
            still_rested(&[], &[rel(true, (bound - half, -half))]),
            "the bound"
        );
        assert!(
            !still_rested(&[], &[rel(true, (bound - half + 1, -half))]),
            "one past it"
        );
        let counts = |n: i32, device: bool| vec![rel(device, (0, -1)); n as usize];
        assert!(
            still_rested(&[], &counts(bound, true)),
            "the bound, a count at a time"
        );
        assert!(
            !still_rested(&[], &counts(bound + 1, true)),
            "one past it, a count at a time"
        );
        assert!(
            !still_rested(&[], &[rel(false, (bound + 1, 0))]),
            "no device"
        );
        assert!(
            !still_rested(&[], &[rel(false, (bound - 1, 0)), rel(true, (0, 2))]),
            "no device, then a device"
        );
        assert!(!still_rested(&[], &[abs(true, (300, 300))]), "the first");
        assert!(!still_rested(&[], &[abs(false, (300, 300))]), "no device");
        let first = [abs(true, (300, 300))];
        assert!(still_rested(&first, &[abs(true, (300, 300))]), "same place");
        assert!(!still_rested(&first, &[abs(true, (301, 300))]), "new place");

        // The sum runs from the last real move: Windows' own moves between the packets don't
        // start it again, so (bound - 1) + 2 counts around one is the hand moving.
        let system = PointerMsg {
            origin: InputOrigin::System,
            ..real(1)
        };
        let mut g = ClickGate::new();
        g.raw_registered(true);
        g.restart(Some((0, 0)));
        g.button_tracking(true);
        g.raw_input(rel(true, (1, 0)), at(0));
        assert!(g.button_move(true, real(1), at(0)));
        g.raw_input(rel(true, (bound - 1, 0)), at(100));
        assert!(!g.button_move(true, system, at(150)));
        g.raw_input(rel(true, (0, 2)), at(200));
        g.button_press(true, real(1), at(400));
        assert!(!g.button_release(true, real(1)).0);
    }

    /// Task 28d rounds 1 and 2: the same-place keep is on only while the real move that made
    /// the rest came with device raw motion since the real move before it. With raw motion
    /// from a null `hDevice` only, or none, Task 28c's rule.
    #[test] // spec: QA-088, QA-036
    fn the_same_place_keep_needs_device_raw_with_real_moves() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let real = |x: i32| PointerMsg {
            origin: InputOrigin::Hardware,
            device: InputDevice::Mouse,
            sent: false,
            at: Some((x, 10)),
            message_at: Some((x, 10)),
        };
        let system = PointerMsg {
            origin: InputOrigin::System,
            ..real(2)
        };
        // Rested by real moves to (2, 10), with `raw` between them; Windows' own move there;
        // a press.
        let kept = |raw: Option<RawMouse>| {
            let mut g = ClickGate::new();
            g.raw_registered(true);
            g.restart(Some((0, 0)));
            g.button_tracking(true);
            assert!(g.button_move(true, real(1), at(0)));
            if let Some(r) = raw {
                g.raw_input(r, at(5));
            }
            assert!(g.button_move(true, real(2), at(10)));
            assert!(!g.button_move(true, system, at(300)));
            g.button_press(true, real(2), at(400));
            g.button_release(true, real(2)).0
        };
        let rel = |device| RawMouse {
            device,
            absolute: false,
            last: (1, 0),
        };
        assert!(kept(Some(rel(true))), "device raw, then a real move");
        assert!(!kept(Some(rel(false))), "raw with no device only");
        assert!(!kept(None), "no raw input");
        // Device raw that a restart came after, not a real move, doesn't turn it on either.
        let mut g = ClickGate::new();
        g.raw_registered(true);
        g.raw_input(rel(true), at(0));
        g.restart(Some((0, 0)));
        g.button_tracking(true);
        assert!(g.button_move(true, real(1), at(10)));
        let system_here = PointerMsg {
            origin: InputOrigin::System,
            ..real(1)
        };
        assert!(!g.button_move(true, system_here, at(300)));
        g.button_press(true, real(1), at(400));
        assert!(!g.button_release(true, real(1)).0, "raw before a restart");
    }

    /// Task 28d round 1: the hand settling on the mouse, as the probe recorded it on the
    /// owner's PC (raw motion that moved no pixel, with no move after it), keeps the rest: a
    /// still click among Windows' own moves counts. Settling to the bound
    /// ([`RAW_JITTER_COUNTS`]) still does; one count more is the hand moving.
    #[test] // spec: QA-088
    fn replay_the_recorded_settling_keeps_the_rest() {
        // Recorded on the owner's PC, so any bound must allow them.
        const { assert!(RAW_JITTER_COUNTS >= 3) };
        let bound = RAW_JITTER_COUNTS as i32;
        type Settle = (&'static str, Vec<(u64, (i32, i32))>, bool);
        let patterns: [Settle; 4] = [
            ("(1, 0) after 61 ms", vec![(61, (1, 0))], true),
            (
                "(0, -2), then (-1, 0)",
                vec![(5, (0, -2)), (160, (-1, 0))],
                true,
            ),
            (
                "the bound",
                vec![(5, (0, -2)), (100, (-1, 0)), (50, (bound - 3, 0))],
                true,
            ),
            (
                "one count past the bound",
                vec![(5, (0, -2)), (100, (-1, 0)), (50, (bound - 2, 0))],
                false,
            ),
        ];
        for fakes in ALL_FAKES {
            for (name, packets, merges) in &patterns {
                let (name, merges) = (*name, *merges);
                let mut r = Replay::open(fakes, true);
                rest_on_merge(&mut r);
                r.settle(packets);
                r.wait(100);
                r.background_move();
                r.wait(150);
                r.background_move();
                r.wait(100);
                r.click();
                r.report(name);
                let want = if merges {
                    Click::Accepted
                } else {
                    refused("no_move")
                };
                assert_eq!(r.clicks, [want], "{fakes:?}, {name}");
            }
        }
    }

    /// Task 28d round 1: a press that jolts the mouse by a count just before it is still a
    /// click on Merge.
    #[test] // spec: QA-088
    fn replay_a_press_with_a_one_count_jolt_counts() {
        for (fakes, raw) in WITH_AND_WITHOUT_RAW {
            let mut r = Replay::open_with(fakes, true, raw);
            rest_on_merge(&mut r);
            r.wait(300);
            r.raw_from_device((0, 1));
            r.click();
            r.report("a jolt");
            assert_eq!(r.clicks, [Click::Accepted], "{fakes:?}, raw {raw}");
        }
    }

    /// Task 28d round 1: a mouse that makes no raw input, or only raw input with a null
    /// `hDevice` (injected, or a remote session), never turns the same-place keep on: Windows'
    /// own moves end the rest, as in Task 28c, and the warp-back case stays refused.
    #[test] // spec: QA-088, QA-036
    fn replay_without_device_raw_input_with_real_moves_task_28c_holds() {
        for fakes in ALL_FAKES {
            for hand_raw in [None, Some(false)] {
                let mut r = Replay::open(fakes, true);
                r.hand_raw = hand_raw;
                still_clicks_among_background_moves(&mut r);
                r.report("no device raw input");
                assert_eq!(
                    r.clicks,
                    [refused("no_move"), refused("no_move")],
                    "{fakes:?}, {hand_raw:?}"
                );

                let mut r = Replay::open(fakes, true);
                r.hand_raw = hand_raw;
                rest_on_merge(&mut r);
                r.wait(500);
                r.warp(MERGE);
                three_still_clicks(&mut r);
                assert!(
                    r.clicks.iter().all(|c| *c == refused("no_move")),
                    "warp back, {fakes:?}, {hand_raw:?}: {:?}",
                    r.clicks
                );
            }
        }
    }

    /// Task 28d review I1: the same-place keep is judged per rest, not kept for the window. The
    /// mouse moves (its raw input confirms the keep), then a device that makes no raw input (a
    /// touchpad) moves the pointer on Merge and rests there; the finger then moves away where
    /// this window sees nothing of it, and a warp puts the cursor back, onto where the touchpad
    /// left it or where the mouse did. No still click counts.
    #[test] // spec: QA-088, QA-036
    fn replay_a_rest_made_by_a_device_without_raw_input_isnt_kept() {
        let touchpad_spot = (MERGE.0 + 6, MERGE.1 + 2);
        for fakes in ALL_FAKES {
            for warp_to in [touchpad_spot, MERGE] {
                let mut r = Replay::open(fakes, true);
                rest_on_merge(&mut r);
                r.wait(300);
                r.hand_raw = None;
                r.hand_device = InputDevice::Touchpad;
                r.move_to(touchpad_spot, 3);
                r.wait(300);
                r.warp(warp_to);
                r.wait(200);
                r.background_move();
                three_still_clicks(&mut r);
                r.report("a touchpad's rest, warped back");
                assert!(
                    r.clicks.iter().all(|c| *c == refused("no_move")),
                    "{fakes:?}, warp to {warp_to:?}: {:?}",
                    r.clicks
                );
            }
        }
    }

    /// Task 28d review m1: a warp that lands while this window handles the hand's last real
    /// move. That move's message was made where the hand put the cursor, but `GetCursorPos`
    /// then reads the warp's place, so the move is judged as an own move at a new place: the
    /// rest ends, and the warp's own move after it doesn't keep anything (as in Task 28c).
    #[test] // spec: QA-088, QA-036
    fn replay_a_warp_while_the_hands_last_move_is_handled_never_merges() {
        for (fakes, raw) in WITH_AND_WITHOUT_RAW {
            let mut r = Replay::open_with(fakes, true, raw);
            r.wait(3_000);
            r.move_to(ROW, 30);
            r.move_to((MERGE.0 - 10, MERGE.1), 5);
            r.wait(200);
            r.last_move_warped((MERGE.0 - 9, MERGE.1), (MERGE.0 + 20, MERGE.1 + 5));
            r.wait(300);
            r.background_move();
            three_still_clicks(&mut r);
            r.report("warped during the last move");
            assert!(
                r.clicks.iter().all(|c| *c == refused("no_move")),
                "{fakes:?}, raw {raw}: {:?}",
                r.clicks
            );
        }
    }

    /// Task 28d review m4: the design relies on a mouse's raw input coming before its own move,
    /// as recorded on the owner's PC. If it came after, the last packet's counts would land
    /// after the reset and end the rest with no real move left, so the still click is refused:
    /// the safe side, and no better than Task 28c for that mouse. Pinned so the dependency shows.
    #[test] // spec: QA-088, QA-036
    fn replay_the_hands_raw_after_its_move_is_refused() {
        for fakes in ALL_FAKES {
            for raw_after_move in [false, true] {
                let mut r = Replay::open(fakes, true);
                r.raw_after_move = raw_after_move;
                a_click_after_a_second_of_background_moves(&mut r);
                r.report("raw after the move");
                let want = if raw_after_move {
                    refused("no_move")
                } else {
                    Click::Accepted
                };
                assert_eq!(
                    r.clicks,
                    std::slice::from_ref(&want),
                    "{fakes:?}, after {raw_after_move}"
                );

                // Without Windows' own moves the click still waits on the hand's last packet.
                let mut r = Replay::open(fakes, true);
                r.raw_after_move = raw_after_move;
                rest_on_merge(&mut r);
                r.wait(300);
                r.click();
                assert_eq!(r.clicks, [want], "{fakes:?}, after {raw_after_move}, alone");
            }
        }
    }

    /// Task 28c re-review N2: a press on the window itself, or the accept button losing the
    /// capture to anything but its own real release, forgets where the cursor is: another
    /// thread holding the capture could have moved it unseen. A press doesn't move the cursor,
    /// so the next move just records it, and the rest itself is untouched.
    #[test] // spec: QA-088, QA-036
    fn a_press_or_a_lost_capture_forgets_where_the_cursor_was() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let real = |x: i32| PointerMsg {
            origin: InputOrigin::Hardware,
            device: InputDevice::Mouse,
            sent: false,
            at: Some((x, 10)),
            message_at: Some((x, 10)),
        };
        // The window's own press (grey Merge, or the click that brings it forward).
        let mut g = ClickGate::new();
        g.restart(Some((0, 0)));
        g.window_tracking(true);
        assert!(g.window_move(true, real(1), at(0)));
        assert_eq!(g.window_press(true), Some(Refusal::NotLiveYet));
        // Windows' own move after the capture went, at a place this window never saw.
        assert!(!g.window_move(true, real(5), at(10)));
        g.window_tracking(true);
        assert!(g.window_move(true, real(6), at(20)));
        // The rest from before the press is still there.
        g.button_tracking(true);
        g.button_press(true, real(6), at(400));
        assert_eq!(g.button_release(true, real(6)), (true, None));

        // Merge loses the capture during a held press, not by its own real release.
        let mut g = ClickGate::new();
        g.restart(Some((0, 0)));
        g.button_tracking(true);
        assert!(g.button_move(true, real(1), at(0)));
        g.button_press(true, real(1), at(400));
        g.capture_changed();
        assert!(!g.button_move(true, real(5), at(410)));
        g.button_tracking(true);
        assert!(g.button_move(true, real(6), at(420)));
    }

    /// The gates the replays must not loosen: a warp, a slide, the live delay and input that
    /// isn't a real pointer never merge, however long the owner then waits or clicks.
    #[test] // spec: QA-088, QA-036
    fn replay_a_warp_a_slide_or_injected_input_never_merges() {
        for (fakes, raw) in WITH_AND_WITHOUT_RAW {
            // A warp onto live Merge while the owner's hand is still: no click counts.
            let mut r = Replay::open_with(fakes, true, raw);
            r.wait(3_000);
            r.move_to(ROW, 30);
            r.wait(500);
            r.warp(MERGE);
            for _ in 0..4 {
                r.click();
                r.wait(400);
            }
            r.report("warp");
            assert!(
                r.clicks.iter().all(|c| *c == refused("no_move")),
                "warp, {fakes:?}, raw {raw}: {:?}",
                r.clicks
            );

            // The owner rested on Merge and moved away; the cursor was warped back.
            let mut r = Replay::open_with(fakes, true, raw);
            r.wait(3_000);
            r.move_to(MERGE, 30);
            r.wait(500);
            r.move_to(BODY, 10);
            r.warp(MERGE);
            r.click();
            r.wait(400);
            r.click();
            r.report("warp back");
            assert!(
                r.clicks.iter().all(|c| *c == refused("no_move")),
                "warp back, {fakes:?}, raw {raw}: {:?}",
                r.clicks
            );

            // Merge slides under the parked pointer: no click counts, while it is grey or
            // after it turns on again.
            let mut r = Replay::open_with(fakes, true, raw);
            r.wait(3_000);
            r.move_to((MERGE.0 - 200, MERGE.1), 20);
            r.wait(500);
            r.cursor = MERGE; // the window moved 200 px left under the still cursor
            r.slide();
            for _ in 0..6 {
                r.wait(400);
                r.click();
            }
            r.report("slide");
            assert!(
                r.clicks.iter().all(|c| *c != Click::Accepted),
                "slide, {fakes:?}, raw {raw}: {:?}",
                r.clicks
            );

            // Clicks that aren't from the hardware pointer, after a real rest: never.
            let mut r = Replay::open_with(fakes, true, raw);
            r.wait(3_000);
            r.move_to(MERGE, 30);
            r.wait(500);
            for origin in [
                InputOrigin::Injected,
                InputOrigin::System,
                InputOrigin::Unavailable,
            ] {
                r.click_from(origin);
                r.wait(400);
            }
            assert!(
                r.clicks.iter().all(|c| *c == refused("not_pointer")),
                "injected, {fakes:?}, raw {raw}: {:?}",
                r.clicks
            );

            // In front, rested on Merge, but clicked within the live delay: grey.
            let mut r = Replay::open_with(fakes, true, raw);
            r.move_to(MERGE, 30);
            r.wait(300);
            r.click();
            assert_eq!(
                r.clicks,
                [refused("not_live_yet")],
                "early, {fakes:?}, raw {raw}"
            );
        }
    }

    /// Re-review 3 m2 and m4: other programs can't relabel the buttons or rewrite the text.
    #[test] // spec: QA-088, QA-036
    fn the_shown_text_and_the_labels_refuse_changes() {
        for msg in [WM_SETTEXT, EM_REPLACESEL, EM_SETREADONLY, WM_PASTE] {
            assert!(text_change(msg), "{msg:#x}");
        }
        for msg in [WM_SETFONT, WM_LBUTTONUP, WM_KEYDOWN, WM_TIMER] {
            assert!(!text_change(msg), "{msg:#x}");
        }
    }

    /// Owner decision 2026-10-24: whether face sign-in is on, from the face cameras' answers;
    /// anything that can't be told is unknown, never off.
    #[test] // spec: QA-088
    fn face_sign_in_is_on_when_any_camera_has_this_users_face_and_unknown_when_unsure() {
        use FaceUnit::*;
        let ids = |list: &[&str]| -> Result<Vec<String>, String> {
            Ok(list.iter().map(|s| s.to_string()).collect())
        };
        let reading = |units: Vec<FaceUnit>, listed: &[&str], present: &[&str]| FaceReading {
            face_units: Ok(units),
            listed_devices: ids(listed),
            present_devices: ids(present),
        };
        // No biometric device at all: face can't sign in on this PC.
        assert_eq!(
            face_sign_in_from(reading(vec![], &[], &[])),
            FaceSignIn::Off
        );
        // m1: a present device WinBio doesn't list (hidden, as Enhanced Sign-in Security
        // does) could be a face camera: can't tell, also when another face camera is listed.
        assert!(matches!(
            face_sign_in_from(reading(vec![], &[], &["SWD\\HIDDEN"])),
            FaceSignIn::Unknown(_)
        ));
        assert!(matches!(
            face_sign_in_from(reading(
                vec![NotEnrolled],
                &["USB\\WEBCAM"],
                &["USB\\WEBCAM", "SWD\\HIDDEN"]
            )),
            FaceSignIn::Unknown(_)
        ));
        // m3: a fingerprint reader WinBio lists is accounted for: off (ids compare
        // case-insensitively).
        assert_eq!(
            face_sign_in_from(reading(vec![], &["usb\\fp"], &["USB\\FP"])),
            FaceSignIn::Off
        );
        // Every present device listed, no enrollment for this user: off.
        assert_eq!(
            face_sign_in_from(reading(
                vec![NotEnrolled, Enrolled(0)],
                &["A", "B"],
                &["A", "B"]
            )),
            FaceSignIn::Off
        );
        // Any enrollment: on, even with another camera failing or a hidden device.
        assert_eq!(
            face_sign_in_from(reading(vec![NotEnrolled, Enrolled(1)], &[], &["X"])),
            FaceSignIn::On
        );
        assert_eq!(
            face_sign_in_from(reading(vec![Failed("x".into()), Enrolled(2)], &[], &[])),
            FaceSignIn::On
        );
        assert_eq!(
            face_sign_in_from(reading(
                vec![NotEnrolled, Failed("E_ACCESSDENIED".into())],
                &[],
                &[]
            )),
            FaceSignIn::Unknown("E_ACCESSDENIED".into())
        );
        // Any read that failed: can't tell.
        let failed = |face_units, listed_devices, present_devices| {
            face_sign_in_from(FaceReading {
                face_units,
                listed_devices,
                present_devices,
            })
        };
        assert_eq!(
            failed(Err("WinBio: off".into()), ids(&[]), ids(&[])),
            FaceSignIn::Unknown("WinBio: off".into())
        );
        assert_eq!(
            failed(Ok(vec![]), ids(&[]), Err("SetupDi".into())),
            FaceSignIn::Unknown("SetupDi".into())
        );
        assert_eq!(
            failed(
                Ok(vec![]),
                Err("fingerprint list".into()),
                ids(&["USB\\FP"])
            ),
            FaceSignIn::Unknown("fingerprint list".into())
        );
    }

    /// m5: a face check that doesn't finish within its limit gives up (the caller then reads
    /// unknown); one that does gives its answer.
    #[test] // spec: QA-088
    fn a_face_check_that_hangs_gives_up_at_the_limit() {
        assert_eq!(within(Duration::from_secs(5), || 7), Some(7));
        let started = Instant::now();
        let r = within(Duration::from_millis(100), || {
            std::thread::sleep(Duration::from_secs(2));
            7
        });
        assert_eq!(r, None);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(FACE_CHECK_LIMIT, Duration::from_secs(5));
    }

    /// Reads this PC's real face enrollment state (read-only: no camera, no prompt, nothing
    /// changed). Run by hand: `cargo nextest run -p lil-platform --run-ignored only -E
    /// 'test(face_sign_in_can_be_read)'`.
    #[test]
    #[ignore = "desktop"]
    fn face_sign_in_can_be_read_on_this_pc() {
        let face = face_sign_in();
        println!("face sign-in: {face:?}");
        assert!(!matches!(face, FaceSignIn::Unknown(_)), "{face:?}");
    }

    /// A window for [`run_in`]: records what was asked of it.
    struct FakeWindow {
        raise: Raise,
        choice: Choice,
        /// Whether the owner brings it forward within the wait.
        brought_forward: bool,
        log: std::cell::RefCell<Vec<&'static str>>,
    }

    impl PromptWindow for FakeWindow {
        fn raise(&self) -> Raise {
            self.log.borrow_mut().push("raise");
            self.raise
        }
        fn wait_choice(&self, timeout: Duration) -> Choice {
            assert_eq!(timeout, HELLO_WAIT);
            self.log.borrow_mut().push("choice");
            self.choice
        }
        fn wait_front(&self, timeout: Duration) -> bool {
            assert_eq!(timeout, HELLO_WAIT);
            self.log.borrow_mut().push("wait_front");
            self.brought_forward
        }
        fn hwnd(&self) -> isize {
            42
        }
    }

    /// Re-review m6: `in_front`'s glue keeps the order, over a fake window. Phase B ruling: a
    /// window Windows keeps behind is waited for (it flashes), never a passphrase fallback.
    #[test] // spec: QA-088
    fn the_window_is_raised_then_asked_and_only_then_windows_hello_runs() {
        let window = |raise, choice| FakeWindow {
            raise,
            choice,
            brought_forward: true,
            log: Default::default(),
        };
        let w = window(Raise::SetForeground, Choice::Accepted);
        let hwnd = run_in(&w, true, |h| {
            w.log.borrow_mut().push("hello");
            Ok(h)
        })
        .unwrap();
        assert_eq!(hwnd, 42);
        assert_eq!(*w.log.borrow(), ["raise", "choice", "hello"]);

        for choice in [Choice::Denied, Choice::Closed, Choice::TimedOut] {
            let w = window(Raise::SetForeground, choice);
            let e = run_in(&w, true, |_| -> io::Result<()> { panic!("hello ran") }).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(*w.log.borrow(), ["raise", "choice"]);
        }

        // Not in front: still asked (the buttons go live only once you brought it forward), and
        // Windows Hello runs only after the click.
        let w = window(Raise::NotInFront, Choice::Accepted);
        run_in(&w, true, |_| {
            w.log.borrow_mut().push("hello");
            Ok(())
        })
        .unwrap();
        assert_eq!(*w.log.borrow(), ["raise", "choice", "hello"]);
        // Not brought forward in time: the choice times out, a refusal, never a fallback.
        let w = window(Raise::NotInFront, Choice::TimedOut);
        let e = run_in(&w, true, |_| -> io::Result<()> { panic!("hello ran") }).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);

        // Not asking (enrolment): raised, then Windows Hello, no choice.
        let w = window(Raise::AttachedInput, Choice::Denied);
        run_in(&w, false, |_| Ok(())).unwrap();
        assert_eq!(*w.log.borrow(), ["raise"]);
        // Not in front: it waits until you bring it forward, then Windows Hello.
        let w = window(Raise::NotInFront, Choice::Denied);
        run_in(&w, false, |_| {
            w.log.borrow_mut().push("hello");
            Ok(())
        })
        .unwrap();
        assert_eq!(*w.log.borrow(), ["raise", "wait_front", "hello"]);
        // Never brought forward: a timeout, and Windows Hello isn't raised.
        let w = FakeWindow {
            brought_forward: false,
            ..window(Raise::NotInFront, Choice::Accepted)
        };
        let e = run_in(&w, false, |_| -> io::Result<()> { panic!("hello ran") }).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
    }
}
