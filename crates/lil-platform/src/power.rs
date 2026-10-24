//! Keep-awake (ARCH-127; Appendix G "Keep-awake"): one system-required power request,
//! `PowerSetRequest(PowerRequestSystemRequired)` on a `PowerCreateRequest` object with a reason
//! string, released with `PowerClearRequest` (Task 1's S1.7). It is a runtime request: it
//! changes no power plan or power setting, the display may still turn off, and it never blocks
//! a sleep or shutdown you start.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::{Mutex, PoisonError};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Power::{
    PowerClearRequest, PowerCreateRequest, PowerRequestSystemRequired, PowerSetRequest,
};
use windows::Win32::System::Threading::{
    POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0,
};
use windows::core::PWSTR;

use crate::process::io_error;

/// The reason Windows shows for the request (`powercfg /requests`).
pub const POWER_REASON: &str = "LILITH: GPU work is running";

/// The request object, made on first use, and whether it is held. Dropping it closes the
/// object, which releases a held request.
#[derive(Debug, Default)]
pub(crate) struct PowerRequest {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    /// `None` until made; `Some(Err)` when this PC refused it.
    request: Option<Result<OwnedHandle, i32>>,
    held: bool,
}

impl PowerRequest {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether the request object can be made on this PC. Makes it, without setting it.
    pub(crate) fn available(&self) -> bool {
        let mut state = self.lock();
        handle(&mut state).is_ok()
    }

    /// Sets or clears the request; a call that changes nothing calls nothing.
    pub(crate) fn set(&self, held: bool) -> io::Result<()> {
        let mut state = self.lock();
        if state.held == held {
            return Ok(());
        }
        let request = handle(&mut state)?;
        // SAFETY: `request` is the live request object this struct owns.
        let done = unsafe {
            if held {
                PowerSetRequest(request, PowerRequestSystemRequired)
            } else {
                PowerClearRequest(request, PowerRequestSystemRequired)
            }
        };
        done.map_err(|e| io_error(&e))?;
        state.held = held;
        Ok(())
    }
}

/// The request object, made on first use with the reason string.
fn handle(state: &mut State) -> io::Result<HANDLE> {
    let made = state.request.get_or_insert_with(|| {
        let mut reason: Vec<u16> = POWER_REASON.encode_utf16().chain([0]).collect();
        let context = REASON_CONTEXT {
            // POWER_REQUEST_CONTEXT_VERSION.
            Version: 0,
            Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
            Reason: REASON_CONTEXT_0 {
                SimpleReasonString: PWSTR(reason.as_mut_ptr()),
            },
        };
        // SAFETY: the context and its NUL-terminated string outlive the call, which copies the
        // string. The returned handle is owned by the `OwnedHandle` and closed with it.
        match unsafe { PowerCreateRequest(&context) } {
            Ok(h) => Ok(unsafe { OwnedHandle::from_raw_handle(h.0) }),
            Err(e) => Err(e.code().0),
        }
    });
    match made {
        Ok(owned) => Ok(HANDLE(owned.as_raw_handle())),
        Err(code) => Err(io_error(&windows::core::Error::from_hresult(
            windows::core::HRESULT(*code),
        ))),
    }
}
