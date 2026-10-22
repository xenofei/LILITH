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
