//! Windows Hello for the reviewer key (QA-088, P4a-R23, P4a-R38; plan 4a Task 28).
//!
//! Two routes can unlock the reviewer App key with Windows Hello instead of the passphrase:
//!
//! - **KEK:** a Windows Hello key credential ([`KeyCredentialManager`]) signs a fixed 32-byte
//!   challenge, and the key-encryption key is SHA-256 over [`KEK_DOMAIN`] followed by the raw
//!   signature ([`kek_from_signature`], P4a-R23). Spike 11 (S1.8) found 10 of 10 signatures over
//!   one challenge byte-identical, across two processes.
//! - **DPAPI:** the key is protected for the current user ([`dpapi_protect`]) and Windows Hello's
//!   consent prompt ([`consent`]) gates its use. The gate is a check in the UI, not encryption.
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
//! `IInspectable`. Which route the suite uses is Task 28's Step 0 (the probe,
//! `examples/hello-probe.rs`); this module has both.
//!
//! **Secrets.** The signature and the KEK live in [`Zeroizing`] buffers and are never logged;
//! DPAPI's plaintext comes back in one. Nothing here prints.
//!
//! **Prompts.** [`kek_enroll`], [`kek_derive`], [`consent`] and their `*_for_window` and
//! `start_*` forms raise a Windows Hello prompt. [`supported`], [`tpm_present`],
//! [`credential_exists`], [`kek_remove`], [`window_bindings_available`] and the DPAPI calls never
//! do.

use std::ffi::c_void;
use std::io;
use std::sync::mpsc;
use std::thread::JoinHandle;

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
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow,
    DispatchMessageW, ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY, FLASHW_ALL, FLASHW_TIMERNOFG,
    FLASHWINFO, FlashWindowEx, GetClientRect, GetForegroundWindow, GetMessageW,
    GetWindowThreadProcessId, MSG, PostMessageW, PostQuitMessage, RegisterClassW, SW_SHOWNORMAL,
    SendMessageW, SetForegroundWindow, ShowWindow, TranslateMessage, WINDOW_STYLE, WM_CLOSE,
    WM_DESTROY, WM_SETFONT, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_EX_APPWINDOW, WS_OVERLAPPED,
    WS_SYSMENU, WS_VISIBLE, WS_VSCROLL,
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
    kek.copy_from_slice(&hasher.finalize());
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
/// DPAPI route's gate, owned by `hwnd`.
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

/// Creates (or replaces) the Windows Hello key credential `credential`, the prompt owned by
/// `hwnd` (KEK route enrolment).
pub fn kek_enroll_for_window(hwnd: isize, credential: &str) -> io::Result<()> {
    finish_create(start_create_for_window(hwnd, credential)?)
}

/// Signs `challenge` with `credential`, the prompt owned by `hwnd`, and returns the KEK
/// ([`kek_from_signature`]).
pub fn kek_derive_for_window(
    hwnd: isize,
    credential: &str,
    challenge: &[u8; 32],
) -> io::Result<Zeroizing<[u8; 32]>> {
    let signature = finish_sign(start_sign_for_window(hwnd, credential, challenge)?)?;
    Ok(kek_from_signature(&signature))
}

/// The consent check, owned by `hwnd`: `true` when Windows Hello verified the user.
pub fn consent_for_window(hwnd: isize, message: &str) -> io::Result<bool> {
    finish_consent(start_consent_for_window(hwnd, message)?)
}

/// [`kek_enroll_for_window`] with an [`OwnerWindow`] opened and brought to the front for it.
pub fn kek_enroll(credential: &str) -> io::Result<()> {
    let window = OwnerWindow::open(
        "LILITH: Windows Hello",
        "Setting up Windows Hello for the reviewer key.",
    )?;
    window.bring_to_front();
    kek_enroll_for_window(window.hwnd(), credential)
}

/// [`kek_derive_for_window`] with an [`OwnerWindow`] opened and brought to the front for it.
pub fn kek_derive(credential: &str, challenge: &[u8; 32]) -> io::Result<Zeroizing<[u8; 32]>> {
    let window = OwnerWindow::open("LILITH: Windows Hello", "Unlocking the reviewer key.")?;
    window.bring_to_front();
    kek_derive_for_window(window.hwnd(), credential, challenge)
}

/// [`consent_for_window`] with an [`OwnerWindow`] showing `message`, brought to the front.
pub fn consent(message: &str) -> io::Result<bool> {
    let window = OwnerWindow::open("LILITH: Windows Hello", message)?;
    window.bring_to_front();
    consent_for_window(window.hwnd(), message)
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
// DPAPI (the DPAPI route).
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

/// A plain top-level window that owns a Windows Hello prompt and shows `body` (read-only). It
/// runs on its own thread with a message loop, so it stays responsive while the caller waits
/// on Windows Hello; it closes when this is dropped.
pub struct OwnerWindow {
    hwnd: isize,
    thread: Option<JoinHandle<()>>,
}

impl OwnerWindow {
    /// Opens the window, shown but not yet brought to the front.
    pub fn open(title: &str, body: &str) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel::<io::Result<isize>>();
        let title = HSTRING::from(title);
        // An EDIT control wants CRLF line ends.
        let body = HSTRING::from(body.replace("\r\n", "\n").replace('\n', "\r\n"));
        let thread = std::thread::Builder::new()
            .name("hello-owner-window".into())
            .spawn(move || window_thread(&title, &body, &tx))?;
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
    }
}

fn window_thread(title: &HSTRING, body: &HSTRING, tx: &mpsc::Sender<io::Result<isize>>) {
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
            480,
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
        let mut client = windows::Win32::Foundation::RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
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
            client.bottom - 16,
            Some(hwnd),
            None,
            None,
            None,
        ) {
            let font = GetStockObject(DEFAULT_GUI_FONT);
            SendMessageW(
                edit,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
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
            WM_CLOSE => {
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
}
