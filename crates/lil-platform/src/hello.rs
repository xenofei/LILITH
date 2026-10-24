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
//! complete a prompt with no deliberate act).
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
use std::time::Duration;

use sha2::{Digest, Sha256};
use windows::Security::Credentials::UI::{UserConsentVerificationResult, UserConsentVerifier};
use windows::Security::Credentials::{
    KeyCredential, KeyCredentialCreationOption, KeyCredentialManager, KeyCredentialOperationResult,
    KeyCredentialRetrievalResult, KeyCredentialStatus,
};
use windows::Security::Cryptography::CryptographicBuffer;
use windows::Win32::Foundation::{HLOCAL, HWND, LPARAM, LRESULT, LocalFree, WPARAM};
use windows::Win32::Graphics::Gdi::{DEFAULT_GUI_FONT, GetStockObject};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::System::TpmBaseServices::{TPM_DEVICE_INFO, Tbsi_GetDeviceInfo};
use windows::Win32::System::WinRT::IUserConsentVerifierInterop;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow,
    DispatchMessageW, ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY, FLASHW_ALL, FLASHW_TIMERNOFG,
    FLASHWINFO, FlashWindowEx, GetClientRect, GetDlgItem, GetForegroundWindow, GetMessageW,
    GetWindowThreadProcessId, HMENU, MSG, PostMessageW, PostQuitMessage, RegisterClassW,
    SW_SHOWNORMAL, SendMessageW, SetForegroundWindow, ShowWindow, TranslateMessage, WINDOW_STYLE,
    WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_SETFONT, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_EX_APPWINDOW,
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
    let raise = window.bring_to_front();
    match accept {
        Some(_) => ask_then(
            raise,
            || window.wait_choice(HELLO_WAIT),
            || call(window.hwnd()),
        ),
        None => {
            front_or_refuse(raise)?;
            call(window.hwnd())
        }
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

/// The accept button's control id.
const ID_ACCEPT: usize = 1;
/// "Deny"'s control id.
const ID_DENY: usize = 2;

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
            // The read-only text has the focus, so a stray Space or Enter clicks nothing.
            let _ = SetFocus(Some(edit));
        }
        if let Some(accept) = accept {
            // Plain push buttons, neither the default: only a deliberate click chooses.
            let button = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0);
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
                }
            }
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
                // A button click: the low word is the control id.
                let choice = match wp.0 & 0xffff {
                    ID_ACCEPT => Some(Choice::Accepted),
                    ID_DENY => Some(Choice::Denied),
                    _ => None,
                };
                if let Some(choice) = choice {
                    record_choice(hwnd, choice);
                    // One choice only: both buttons go grey.
                    for id in [ID_ACCEPT, ID_DENY] {
                        if let Ok(b) = GetDlgItem(Some(hwnd), id as i32) {
                            let _ = EnableWindow(b, false);
                        }
                    }
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
}
