//! The current user's SID from the process token.

use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_DATA, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, IsTokenRestricted,
    TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_USER, TokenIntegrityLevel, TokenIsAppContainer,
    TokenUser,
};
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

/// What a pipe peer's token says of its trust (ARCH-027), read from one token handle: its
/// mandatory integrity level (the last sub-authority of its label SID: `0x1000` Low, `0x2000`
/// Medium, `0x3000` High), whether it is restricted (`IsTokenRestricted`: restricting SIDs, a
/// write-restricted token among them) and whether it is an AppContainer token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenTrust {
    pub integrity: u32,
    pub restricted: bool,
    pub app_container: bool,
}

/// [`TokenTrust`] of the token of `process` (open with at least
/// `PROCESS_QUERY_LIMITED_INFORMATION`). On failure, the name of the call that failed.
pub(crate) fn token_trust(
    process: HANDLE,
) -> Result<TokenTrust, (&'static str, windows::core::Error)> {
    // SAFETY: as `token_user_sid`: every pointer is valid for its call, `buf` is 8-byte
    // aligned and sized by the first call, and the label SID lives in `buf` while it is read.
    unsafe {
        let mut handle = HANDLE::default();
        OpenProcessToken(process, TOKEN_QUERY, &mut handle).map_err(|e| ("OpenProcessToken", e))?;
        let token = Token(handle);
        let mut needed = 0u32;
        let first = GetTokenInformation(token.0, TokenIntegrityLevel, None, 0, &mut needed);
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
            TokenIntegrityLevel,
            Some(buf.as_mut_ptr().cast()),
            needed,
            &mut needed,
        )
        .map_err(|e| ("GetTokenInformation", e))?;
        let label = &*buf.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
        let sid = label.Label.Sid;
        let count = *GetSidSubAuthorityCount(sid);
        if count == 0 {
            return Err((
                "GetSidSubAuthorityCount",
                windows::core::Error::new(ERROR_INVALID_DATA.to_hresult(), "no sub-authority"),
            ));
        }
        let integrity = *GetSidSubAuthority(sid, u32::from(count) - 1);
        // FALSE with no error is "not restricted"; FALSE with an error is a failure.
        let restricted = match IsTokenRestricted(token.0) {
            Ok(()) => true,
            Err(e) if e.code().is_ok() => false,
            Err(e) => return Err(("IsTokenRestricted", e)),
        };
        let mut app_container = 0u32;
        let mut len = 0u32;
        GetTokenInformation(
            token.0,
            TokenIsAppContainer,
            Some((&raw mut app_container).cast()),
            u32::try_from(size_of::<u32>()).unwrap_or(4),
            &mut len,
        )
        .map_err(|e| ("GetTokenInformation", e))?;
        Ok(TokenTrust {
            integrity,
            restricted,
            app_container: app_container != 0,
        })
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
