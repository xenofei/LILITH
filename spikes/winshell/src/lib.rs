//! Spike 11 (plan 4a Task 1): shared names, pure helpers and thin Windows wrappers for the
//! `winshell-*` binaries. Throwaway code: the plan re-implements what it needs (spikes/README.md).

pub mod args;
pub mod text;
pub mod win;

/// The spike's AUMID (S1.2).
pub const AUMID: &str = "LILITH.Spike.Notify";
/// The spike's activation scheme (S1.3).
pub const SCHEME: &str = "lilspike";
/// The URL every confirmation toast launches (S1.3, S1.4).
pub const CONFIRM_URL: &str = "lilspike:confirm/cf_0123456789abcdef";
/// The progress toast's Tag and Group (S1.5).
pub const PROGRESS_TAG: &str = "j_spike";
pub const PROGRESS_GROUP: &str = "job_progress";
/// The spike's single-instance pipe (S1.4).
pub const PIPE_NAME: &str = r"\\.\pipe\lilith-spike-winshell-app";
/// The Windows Hello credential's name (S1.8).
pub const CREDENTIAL_NAME: &str = "LILITH-spike-kek";
/// The power request's reason string (S1.7); `powercfg /requests` shows it.
pub const POWER_REASON: &str = "LILITH spike 11 keep-awake check";
/// The one fixed 32-byte challenge every signature is taken over (S1.8 c).
pub const CHALLENGE: [u8; 32] = *b"LILITH spike 11 fixed challenge!";

/// The spike's toast activator CLSID: set on the shortcut, and registered with a
/// `LocalServer32` of `winshell-app.exe`, only for the toast-persistence check (S1.2p).
pub const TOAST_CLSID: &str = "{6F1A0C52-3B7E-4E0D-9A11-5C2B8D7E4F31}";
pub const TOAST_CLSID_U128: u128 = 0x6F1A0C52_3B7E_4E0D_9A11_5C2B8D7E4F31;

#[cfg(test)]
mod tests {
    #[test]
    fn toast_clsid_string_and_value_agree() {
        let g = windows::core::GUID::from_u128(super::TOAST_CLSID_U128);
        assert_eq!(format!("{{{g:?}}}"), super::TOAST_CLSID);
    }
}
