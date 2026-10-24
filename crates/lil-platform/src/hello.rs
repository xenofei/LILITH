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
//! **In front, or not at all (QA-088).** [`kek_enroll`] and [`kek_derive`] open an
//! [`OwnerWindow`] that shows what you are confirming, bring it to the front, and refuse when it
//! didn't get there ([`front_or_refuse`]); the caller then asks for the passphrase. Every
//! prompt is cancelled after [`HELLO_WAIT`] ([`wait_bounded`]). With an accept label ("Merge",
//! "Approve"), [`kek_derive`]'s window has that button and "Deny", and Windows Hello is raised
//! only after the accept click ([`ask_then`]); "Deny", closing the window or no choice within
//! [`HELLO_WAIT`] refuses with `PermissionDenied` (Task 28 fix round 2: Windows Hello Face can
//! complete a prompt with no deliberate act). Only a real pointer click accepts
//! ([`choice_from_command`]): the accept button is subclassed and armed only by a `WM_LBUTTONUP`
//! inside it from a real mouse, touch, pen or touchpad, on its own thread ([`arms_accept`]:
//! `GetCurrentInputMessageSource` is `IMO_HARDWARE` with a pointer device, `InSendMessage` is
//! false), after a real press on it that, for a mouse or touchpad, followed a real hover of
//! [`HOVER_BEFORE_PRESS`] ([`PressTracker`]); and only a `BN_CLICKED` from that button's own
//! window counts, so a posted or sent `WM_COMMAND`, `BM_CLICK`, UI Automation's Invoke,
//! `SendInput` and any key (Space, Enter) are ignored. The buttons work only once the window
//! has been in front for [`BUTTONS_LIVE_AFTER`] of real time ([`LiveClock`]), and stop when it
//! loses the front, or it or the accept button moves or is resized. The text and the labels
//! refuse changes from other programs ([`text_change`]). "Deny" accepts any source: refusing is
//! always safe.
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
//! `IInspectable`. Whether the prompt then comes to the front by itself is Task 28's Step 0 (the
//! probe).
//!
//! **Secrets.** The signature and the KEK live in [`Zeroizing`] buffers and are never logged;
//! DPAPI's plaintext comes back in one. Nothing here prints.
//!
//! **Prompts.** [`kek_enroll`], [`kek_derive`], their `*_for_window` forms and the `start_*`
//! calls raise a Windows Hello prompt. [`supported`], [`tpm_present`], [`credential_exists`],
//! [`kek_remove`], [`window_bindings_available`] and the DPAPI calls never do.

use std::ffi::c_void;
use std::io;
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
use windows::Win32::Foundation::{HLOCAL, HWND, LPARAM, LRESULT, LocalFree, WPARAM};
use windows::Win32::Graphics::Gdi::{
    DEFAULT_GUI_FONT, GetMonitorInfoW, GetStockObject, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromWindow, ScreenToClient,
};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::System::TpmBaseServices::{TPM_DEVICE_INFO, Tbsi_GetDeviceInfo};
use windows::Win32::System::WinRT::IUserConsentVerifierInterop;
use windows::Win32::UI::Controls::{EM_REPLACESEL, EM_SETREADONLY, WM_MOUSELEAVE};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, IsWindowEnabled, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::Input::{
    GetCurrentInputMessageSource, IMDT_KEYBOARD, IMDT_MOUSE, IMDT_PEN, IMDT_TOUCH, IMDT_TOUCHPAD,
    IMO_HARDWARE, IMO_INJECTED, IMO_SYSTEM, INPUT_MESSAGE_SOURCE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, BringWindowToTop, CW_USEDEFAULT, CallWindowProcW, CreateWindowExW, DefWindowProcW,
    DestroyWindow, DispatchMessageW, ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY, FLASHW_ALL,
    FLASHW_TIMERNOFG, FLASHWINFO, FlashWindowEx, GWLP_WNDPROC, GetClientRect, GetDlgItem,
    GetForegroundWindow, GetMessageW, GetParent, GetWindowLongPtrW, GetWindowRect,
    GetWindowThreadProcessId, HMENU, InSendMessage, KillTimer, MSG, PostMessageW, PostQuitMessage,
    RegisterClassW, SW_SHOWNORMAL, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW, SetForegroundWindow,
    SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, WA_INACTIVE,
    WINDOW_STYLE, WM_ACTIVATE, WM_CANCELMODE, WM_CAPTURECHANGED, WM_CLOSE, WM_COMMAND, WM_DESTROY,
    WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PASTE, WM_SETFONT, WM_SETTEXT,
    WM_TIMER, WM_WINDOWPOSCHANGED, WNDCLASSW, WNDPROC, WS_CAPTION, WS_CHILD, WS_DISABLED,
    WS_EX_APPWINDOW, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
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

/// QA-088: Windows Hello is used only when its window is in front; otherwise the caller uses
/// the passphrase (or refuses without a terminal). Pure.
pub fn front_or_refuse(raise: Raise) -> io::Result<()> {
    match raise {
        Raise::AlreadyInFront | Raise::SetForeground | Raise::AttachedInput => Ok(()),
        Raise::NotInFront => Err(io::Error::other(
            "the Windows Hello window couldn't be brought to the front of the other windows, so Windows Hello isn't used",
        )),
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

/// The order every asking prompt keeps (pure): the window in front ([`front_or_refuse`]), then
/// your choice ([`choice_or_refuse`]), and only then `call`, which raises Windows Hello.
pub fn ask_then<T>(
    raise: Raise,
    choose: impl FnOnce() -> Choice,
    call: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    front_or_refuse(raise)?;
    choice_or_refuse(choose())?;
    call()
}

/// Opens an [`OwnerWindow`] showing `shown` and brings it to the front. With `accept`, it has
/// that button and "Deny", and `call` runs only after you clicked `accept` ([`ask_then`]);
/// without, `call` runs as soon as the window is in front ([`front_or_refuse`]).
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
    fn hwnd(&self) -> isize;
}

impl PromptWindow for OwnerWindow {
    fn raise(&self) -> Raise {
        self.bring_to_front()
    }
    fn wait_choice(&self, timeout: Duration) -> Choice {
        OwnerWindow::wait_choice(self, timeout)
    }
    fn hwnd(&self) -> isize {
        OwnerWindow::hwnd(self)
    }
}

/// [`in_front`]'s order, over any window (re-review m6): raise it; when `asking`, wait for the
/// choice ([`ask_then`]); run `call` with its handle only then.
fn run_in<W: PromptWindow, T>(
    window: &W,
    asking: bool,
    call: impl FnOnce(isize) -> io::Result<T>,
) -> io::Result<T> {
    let raise = window.raise();
    if asking {
        ask_then(
            raise,
            || window.wait_choice(HELLO_WAIT),
            || call(window.hwnd()),
        )
    } else {
        front_or_refuse(raise)?;
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
/// [`HELLO_WAIT`] refuses (`PermissionDenied`).
pub fn kek_derive(
    credential: &str,
    challenge: &[u8; 32],
    shown: &str,
    accept: Option<&str>,
) -> io::Result<Zeroizing<[u8; 32]>> {
    in_front(shown, accept, |hwnd| {
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
        && input.origin == InputOrigin::Hardware
        && matches!(
            input.device,
            InputDevice::Mouse | InputDevice::Touch | InputDevice::Pen | InputDevice::Touchpad
        )
        && !input.sent_by_another_thread
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

/// How long the cursor must rest on the accept button, by real pointer moves, before a mouse
/// or touchpad press on it counts (re-review 3 m3): a program that warps the cursor onto Merge
/// (`SetCursorPos`) doesn't make the owner's next click a click on Merge.
pub const HOVER_BEFORE_PRESS: Duration = Duration::from_millis(300);

/// The accept button's press state (pure: the caller passes what it judged and `now`). Only a
/// real press on the button lets its release arm it (re-review 3 m1): a posted
/// `WM_LBUTTONDOWN` gives the button its pushed state and the capture, so a hardware release
/// over it would otherwise click after a press made somewhere else.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PressTracker {
    /// When real pointer moves first had the cursor on the button, since it last left.
    hover_since: Option<Instant>,
    /// A real press on the button is down.
    pressed: bool,
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
    pub fn on_press(&mut self, real: bool, device: InputDevice, now: Instant) {
        let rested = match device {
            InputDevice::Touch | InputDevice::Pen => true,
            _ => self
                .hover_since
                .and_then(|since| now.checked_duration_since(since))
                .is_some_and(|d| d >= HOVER_BEFORE_PRESS),
        };
        self.pressed = real && rested;
    }

    /// A `WM_LBUTTONUP` to the button: whether a real press preceded it. The press is used up.
    pub fn on_release(&mut self) -> bool {
        std::mem::take(&mut self.pressed)
    }

    /// The button lost the capture, a mode was cancelled, or the delay restarted: the press and
    /// the hover are dropped.
    pub fn cancel(&mut self) {
        *self = Self::default();
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
    /// The asking window's time in front.
    static LIVE: std::cell::Cell<LiveClock> = const {
        std::cell::Cell::new(LiveClock { in_front_since: None })
    };
    /// The accept button's press and hover.
    static PRESS: std::cell::Cell<PressTracker> = const {
        std::cell::Cell::new(PressTracker { hover_since: None, pressed: false })
    };
    /// The asking window's last known place and size, to tell a move or resize.
    static PLACE: std::cell::Cell<(i32, i32, i32, i32)> = const { std::cell::Cell::new((0, 0, 0, 0)) };
    /// The accept button's last known place and size in the window (re-review 3 m2).
    static ACCEPT_PLACE: std::cell::Cell<(i32, i32, i32, i32)> = const {
        std::cell::Cell::new((0, 0, 0, 0))
    };
}

/// Changes this thread's accept-button press state.
fn with_press<T>(f: impl FnOnce(&mut PressTracker) -> T) -> T {
    PRESS.with(|c| {
        let mut press = c.get();
        let out = f(&mut press);
        c.set(press);
        out
    })
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

/// Whether a mouse message's point (`lParam`, client coordinates) is inside `button`.
fn point_inside(button: HWND, lp: LPARAM) -> bool {
    let x = i32::from((lp.0 & 0xffff) as u16 as i16);
    let y = i32::from(((lp.0 >> 16) & 0xffff) as u16 as i16);
    let mut rect = windows::Win32::Foundation::RECT::default();
    // SAFETY: a query into a local structure, for this button.
    unsafe { GetClientRect(button, &mut rect) }.is_ok()
        && x >= rect.left
        && x < rect.right
        && y >= rect.top
        && y < rect.bottom
}

/// Whether the mouse message being handled is a real pointer message inside `button`, handled
/// on its own thread: hardware origin, a mouse, touch, pen or touchpad, not sent.
fn real_pointer_inside(button: HWND, lp: LPARAM) -> (bool, InputDevice) {
    let (origin, device) = current_input_source();
    // SAFETY: a query about the message this thread is handling.
    let sent = unsafe { InSendMessage() }.as_bool();
    let pointer = matches!(
        device,
        InputDevice::Mouse | InputDevice::Touch | InputDevice::Pen | InputDevice::Touchpad
    );
    (
        origin == InputOrigin::Hardware && pointer && !sent && point_inside(button, lp),
        device,
    )
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
                let (real, _) = real_pointer_inside(hwnd, lp);
                with_press(|p| p.on_move(real, now));
                if real {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut track);
                }
            }
            WM_MOUSELEAVE => with_press(PressTracker::on_leave),
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                let (real, device) = real_pointer_inside(hwnd, lp);
                with_press(|p| p.on_press(real, device, now));
            }
            WM_CAPTURECHANGED | WM_CANCELMODE => with_press(PressTracker::cancel),
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
                let (origin, device) = current_input_source();
                let input = ButtonInput {
                    msg,
                    over_button: point_inside(hwnd, lp),
                    origin,
                    device,
                    sent_by_another_thread: InSendMessage().as_bool(),
                    pressed_by_hardware: with_press(PressTracker::on_release),
                };
                if arms_accept(input) {
                    ARMED.store(hwnd.0 as isize, std::sync::atomic::Ordering::SeqCst);
                    let result = CallWindowProcW(Some(original), hwnd, msg, wp, lp);
                    ARMED.store(0, std::sync::atomic::Ordering::SeqCst);
                    return result;
                }
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
    LIVE.with(|c| {
        let mut clock = c.get();
        clock.restart();
        c.set(clock);
    });
    with_press(PressTracker::cancel);
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
            let (ww, wh) = (work.right - work.left, work.bottom - work.top);
            let (w, h) = (720.min(ww - 32), 520.min(wh - 32));
            let _ = SetWindowPos(
                hwnd,
                None,
                work.left + (ww - w) / 2,
                work.top + (wh - h) / 2,
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
                        && LIVE.with(|c| c.get().is_live(Instant::now())),
                };
                if let Some(choice) = choice_from_command(command) {
                    record_choice(hwnd, choice);
                    // One choice only: both buttons go grey.
                    let _ = KillTimer(Some(hwnd), TIMER_LIVE);
                    set_buttons_enabled(hwnd, false);
                }
                LRESULT(0)
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
                    let live = LIVE.with(|c| {
                        let mut clock = c.get();
                        let live = clock.tick(in_front, Instant::now());
                        c.set(clock);
                        live
                    });
                    set_buttons_enabled(hwnd, live);
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                record_choice(hwnd, Choice::Closed);
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
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

    #[test] // spec: QA-088
    fn windows_hello_is_used_only_when_its_window_is_in_front() {
        for ok in [
            Raise::AlreadyInFront,
            Raise::SetForeground,
            Raise::AttachedInput,
        ] {
            assert!(front_or_refuse(ok).is_ok(), "{ok:?}");
        }
        let e = front_or_refuse(Raise::NotInFront).unwrap_err();
        // Not `Interrupted` (a cancel): the caller treats it as Hello being unavailable.
        assert_ne!(e.kind(), io::ErrorKind::Interrupted);
        assert!(e.to_string().contains("front"), "{e}");
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
    /// window or no choice in time refuses, and a window not in front never asks at all.
    #[test] // spec: QA-088, QA-036
    fn windows_hello_is_raised_only_after_the_accept_click() {
        use std::cell::Cell;
        let called = Cell::new(false);
        let call = || {
            called.set(true);
            Ok(())
        };
        ask_then(Raise::SetForeground, || Choice::Accepted, call).unwrap();
        assert!(called.get());

        for refused in [Choice::Denied, Choice::Closed, Choice::TimedOut] {
            let called = Cell::new(false);
            let e = ask_then(
                Raise::AlreadyInFront,
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

        // Not in front: nobody is asked, Hello isn't raised, and it isn't a refusal.
        let asked = Cell::new(false);
        let e = ask_then(
            Raise::NotInFront,
            || {
                asked.set(true);
                Choice::Accepted
            },
            || -> io::Result<()> { panic!("Windows Hello was raised behind other windows") },
        )
        .unwrap_err();
        assert!(!asked.get());
        assert_ne!(e.kind(), io::ErrorKind::PermissionDenied);
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
        assert!(p.on_release());
        // A release consumes the press: a second release doesn't arm.
        assert!(!p.on_release());

        // m1: no press at all (another program posted WM_LBUTTONDOWN, or the press was made
        // elsewhere): the release doesn't arm.
        let mut p = PressTracker::default();
        p.on_move(true, at(0));
        p.on_press(false, InputDevice::Mouse, at(hover * 2));
        assert!(!p.on_release());
        assert!(!arms_accept(ButtonInput {
            pressed_by_hardware: false,
            ..real_release()
        }));

        // m3: the cursor arrived without a real move (SetCursorPos), or a moment ago: a mouse
        // or touchpad press doesn't count.
        for device in [InputDevice::Mouse, InputDevice::Touchpad] {
            let mut p = PressTracker::default();
            p.on_press(true, device, at(hover * 2));
            assert!(!p.on_release(), "{device:?} with no hover");
            let mut p = PressTracker::default();
            p.on_move(true, at(0));
            p.on_press(true, device, at(hover - 1));
            assert!(!p.on_release(), "{device:?} hovered too briefly");
        }
        // A move that isn't a real pointer move, or the cursor leaving, starts the hover again.
        let mut p = PressTracker::default();
        p.on_move(true, at(0));
        p.on_move(false, at(hover / 2));
        p.on_move(true, at(hover));
        p.on_press(true, InputDevice::Mouse, at(hover + hover / 2));
        assert!(!p.on_release());
        let mut p = PressTracker::default();
        p.on_move(true, at(0));
        p.on_leave();
        p.on_press(true, InputDevice::Mouse, at(hover * 2));
        assert!(!p.on_release());

        // Touch and pen press where the contact is, not at the cursor: no hover needed.
        for device in [InputDevice::Touch, InputDevice::Pen] {
            let mut p = PressTracker::default();
            p.on_press(true, device, at(0));
            assert!(p.on_release(), "{device:?}");
        }

        // A lost capture, a cancelled mode or a restart of the delay drops the press and the
        // hover.
        let mut p = PressTracker::default();
        p.on_move(true, at(0));
        p.on_press(true, InputDevice::Mouse, at(hover));
        p.cancel();
        assert!(!p.on_release());
        p.on_press(true, InputDevice::Mouse, at(hover * 3));
        assert!(!p.on_release(), "the hover went with the cancel");
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

    /// A window for [`run_in`]: records what was asked of it.
    struct FakeWindow {
        raise: Raise,
        choice: Choice,
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
        fn hwnd(&self) -> isize {
            42
        }
    }

    /// Re-review m6: `in_front`'s glue keeps the order, over a fake window.
    #[test] // spec: QA-088
    fn the_window_is_raised_then_asked_and_only_then_windows_hello_runs() {
        let window = |raise, choice| FakeWindow {
            raise,
            choice,
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

        let w = window(Raise::NotInFront, Choice::Accepted);
        assert!(run_in(&w, true, |_| -> io::Result<()> { panic!("hello ran") }).is_err());
        assert_eq!(*w.log.borrow(), ["raise"]);

        // Not asking (enrolment): raised, then Windows Hello, no choice.
        let w = window(Raise::AttachedInput, Choice::Denied);
        run_in(&w, false, |_| Ok(())).unwrap();
        assert_eq!(*w.log.borrow(), ["raise"]);
        let w = window(Raise::NotInFront, Choice::Accepted);
        assert!(run_in(&w, false, |_| -> io::Result<()> { panic!("hello ran") }).is_err());
    }
}
