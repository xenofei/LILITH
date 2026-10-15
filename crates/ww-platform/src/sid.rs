//! The current user's SID from the process token.

use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_DATA, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use windows::core::PWSTR;

use crate::env::EnvError;

fn err(call: &'static str, e: windows::core::Error) -> EnvError {
    EnvError::Windows {
        call,
        message: e.to_string(),
    }
}

/// Closes the token handle on every exit path.
struct Token(HANDLE);

impl Drop for Token {
    fn drop(&mut self) {
        // SAFETY: the handle came from `OpenProcessToken` and is closed only here.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// The SID string (`S-1-5-21-...`) of the user running this process.
pub fn current_user_sid() -> Result<String, EnvError> {
    // SAFETY: the pseudo-handle from `GetCurrentProcess` is always valid and needs no closing.
    token_user_sid(unsafe { GetCurrentProcess() }).map_err(|(call, e)| err(call, e))
}

/// The SID string of the token user of `process`, which must be open with at least
/// `PROCESS_QUERY_LIMITED_INFORMATION`. On failure, the name of the call that failed.
pub(crate) fn token_user_sid(
    process: HANDLE,
) -> Result<String, (&'static str, windows::core::Error)> {
    // SAFETY: every pointer handed to Windows is valid for the call. `buf` is 8-byte aligned
    // (`Vec<u64>`) and sized by the first `GetTokenInformation` call, so reading a `TOKEN_USER`
    // from it is sound. The string allocated by `ConvertSidToStringSidW` is copied and then
    // freed once with `LocalFree`.
    unsafe {
        let mut handle = HANDLE::default();
        OpenProcessToken(process, TOKEN_QUERY, &mut handle).map_err(|e| ("OpenProcessToken", e))?;
        let token = Token(handle);

        // First call reports the required size (and fails with ERROR_INSUFFICIENT_BUFFER).
        let mut needed = 0u32;
        let first = GetTokenInformation(token.0, TokenUser, None, 0, &mut needed);
        if needed == 0 {
            return Err((
                "GetTokenInformation",
                first
                    .err()
                    .unwrap_or_else(windows::core::Error::from_thread),
            ));
        }
        let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            needed,
            &mut needed,
        )
        .map_err(|e| ("GetTokenInformation", e))?;

        let token_user = &*buf.as_ptr().cast::<TOKEN_USER>();
        let mut pwstr = PWSTR::null();
        ConvertSidToStringSidW(token_user.User.Sid, &mut pwstr)
            .map_err(|e| ("ConvertSidToStringSidW", e))?;
        let sid = pwstr.to_string().map_err(|e| {
            (
                "ConvertSidToStringSidW",
                windows::core::Error::new(ERROR_INVALID_DATA.to_hresult(), e.to_string()),
            )
        });
        let _ = LocalFree(Some(HLOCAL(pwstr.as_ptr().cast())));
        sid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read-only: queries the process token and writes nothing.
    #[test] // spec: ARCH-095
    fn current_user_sid_looks_like_a_sid() {
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-"), "{sid}");
        assert!(
            sid.bytes()
                .all(|b| b.is_ascii_digit() || b == b'S' || b == b'-')
        );
    }
}
