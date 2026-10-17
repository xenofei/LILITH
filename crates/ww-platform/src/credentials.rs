//! Windows Credential Manager behind [`CredentialStore`] (QA-079, ruling R28).
//!
//! `ww-platform` is the only crate allowed to call Windows APIs (ARCH-111), so the generic
//! credentials (`CredReadW`, `CredWriteW`, `CredDeleteW`) are wrapped here. Other crates use
//! the trait, with [`MemoryCredentialStore`] standing in for Credential Manager in tests.
//!
//! **Targets.** The real store, [`WindowsCredentialStore`], only touches targets under
//! [`DEV_PREFIX`] (owner tooling such as the agent token), under the legacy owner-tooling
//! prefix [`names::LEGACY_DEV_CREDENTIAL_PREFIX`] (R4; Task A2 limits it to reads and
//! deletes), or under the test prefix `<legacy app name>-test-<id>/` (tests; it moves to the new name in Task B3).
//! Anything else, including either name's stable channel prefix and other channels, is refused
//! before any Windows call ([`check_target`]). Once the harness guard is armed, an
//! owner-tooling target under either name also trips it (QA-040).
//!
//! **Secrets** are kept in [`Zeroizing`] strings, never printed, and `Debug` redacts them.
//! A credential's comment is non-secret metadata (for the agent token, the expiry the owner
//! entered).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Mutex;

use windows::Win32::Foundation::ERROR_NOT_FOUND;
use windows::Win32::Security::Credentials::{
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW,
    CredWriteW,
};
use windows::core::{PCWSTR, PWSTR};
use zeroize::{Zeroize, Zeroizing};

use crate::env::Channel;
use crate::guard;
use crate::names;

/// The owner-tooling prefix, every use. `Env::credential_prefix` for the dev channel moves to
/// it in Task B3.
pub const DEV_PREFIX: &str = names::DEV_CREDENTIAL_PREFIX;
/// Test channels keep the legacy name until Task B3 (`Env::credential_prefix`).
const TEST_PREFIX: &str = "Worldwright-test-";
/// Credential Manager's limit on a generic credential's secret, in bytes.
pub const MAX_SECRET_BYTES: usize = 2560;
/// Credential Manager's limit on a comment, in UTF-16 units.
pub const MAX_COMMENT_CHARS: usize = 256;
const MAX_TARGET_CHARS: usize = 32767;

/// One generic credential.
#[derive(Clone, PartialEq, Eq)]
pub struct Credential {
    pub username: String,
    pub secret: Zeroizing<String>,
    /// Non-secret metadata, at most [`MAX_COMMENT_CHARS`] characters.
    pub comment: Option<String>,
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credential")
            .field("username", &self.username)
            .field("secret", &"<redacted>")
            .field("comment", &self.comment)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialError {
    /// The target is outside the owner-tooling and test prefixes ([`check_target`]).
    TargetRefused(String),
    /// The credential can't be stored as given (too long, a NUL character, ...).
    Invalid(String),
    /// A Windows call failed.
    Windows { call: &'static str, message: String },
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CredentialError::TargetRefused(t) => write!(
                f,
                "credential target {t:?} is refused: only {DEV_PREFIX}..., {}... and {TEST_PREFIX}<id>/... are used",
                names::LEGACY_DEV_CREDENTIAL_PREFIX
            ),
            CredentialError::Invalid(why) => write!(f, "the credential can't be stored: {why}"),
            CredentialError::Windows { call, message } => write!(f, "{call} failed: {message}"),
        }
    }
}

impl std::error::Error for CredentialError {}

/// Generic credentials by target name.
pub trait CredentialStore {
    /// `Ok(None)` when no credential has this target.
    fn get(&self, target: &str) -> Result<Option<Credential>, CredentialError>;
    /// Creates or replaces the credential.
    fn set(&self, target: &str, credential: &Credential) -> Result<(), CredentialError>;
    /// Removes the credential; `Ok(false)` when there was none.
    fn delete(&self, target: &str) -> Result<bool, CredentialError>;
}

/// What a credential call does with its target ([`check_target`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetUse {
    Read,
    Write,
    Delete,
}

/// `Ok` for (ASCII case-insensitive, like Credential Manager):
///
/// - `<DEV_PREFIX><name>`, every use;
/// - `<LEGACY_DEV_CREDENTIAL_PREFIX><name>`, the legacy owner-tooling entries (R4). Every use
///   for now: Task A2 refuses [`TargetUse::Write`] here when `agent-token set` moves to
///   [`DEV_PREFIX`];
/// - `<TEST_PREFIX><id>/<name>`, every use, where `<id>` is a valid test channel id.
///
/// `<name>` is never empty, and no control characters are allowed anywhere.
pub fn check_target(target: &str, _use: TargetUse) -> Result<(), CredentialError> {
    let refused = || {
        Err(CredentialError::TargetRefused(
            target.escape_debug().to_string(),
        ))
    };
    if target.chars().count() > MAX_TARGET_CHARS || target.chars().any(char::is_control) {
        return refused();
    }
    let lower = target.to_ascii_lowercase();
    let legacy_dev = names::LEGACY_DEV_CREDENTIAL_PREFIX.to_ascii_lowercase();
    let name = if let Some(rest) = lower.strip_prefix(&DEV_PREFIX.to_ascii_lowercase()) {
        rest
    } else if let Some(rest) = lower.strip_prefix(&legacy_dev) {
        rest
    } else if let Some(rest) = lower.strip_prefix(&TEST_PREFIX.to_ascii_lowercase()) {
        match rest.split_once('/') {
            Some((id, name)) if Channel::Test(id.to_string()).is_valid() => name,
            _ => return refused(),
        }
    } else {
        return refused();
    };
    if name.is_empty() {
        return refused();
    }
    Ok(())
}

/// The limits Credential Manager enforces, checked up front so the error is plain.
fn check_credential(credential: &Credential) -> Result<(), CredentialError> {
    let invalid = |why: &str| Err(CredentialError::Invalid(why.to_string()));
    if credential.secret.len() > MAX_SECRET_BYTES {
        return invalid("the secret is longer than Credential Manager allows");
    }
    if credential.username.contains('\0') {
        return invalid("the user name contains a NUL character");
    }
    if let Some(comment) = &credential.comment {
        if comment.contains('\0') {
            return invalid("the comment contains a NUL character");
        }
        if comment.encode_utf16().count() > MAX_COMMENT_CHARS {
            return invalid("the comment is longer than Credential Manager allows");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The in-memory fake.
// ---------------------------------------------------------------------------------------------

/// An in-memory store for tests. It applies the same target and credential checks as the real
/// one, and matches targets case-insensitively like Credential Manager.
#[derive(Debug, Default)]
pub struct MemoryCredentialStore {
    items: Mutex<BTreeMap<String, Credential>>,
}

impl MemoryCredentialStore {
    pub fn new() -> MemoryCredentialStore {
        MemoryCredentialStore::default()
    }
}

impl MemoryCredentialStore {
    fn items(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Credential>> {
        // Every edit is a single map call, so a poisoned lock still holds a consistent map.
        self.items.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl CredentialStore for MemoryCredentialStore {
    fn get(&self, target: &str) -> Result<Option<Credential>, CredentialError> {
        check_target(target, TargetUse::Read)?;
        Ok(self.items().get(&target.to_ascii_lowercase()).cloned())
    }

    fn set(&self, target: &str, credential: &Credential) -> Result<(), CredentialError> {
        check_target(target, TargetUse::Write)?;
        check_credential(credential)?;
        self.items()
            .insert(target.to_ascii_lowercase(), credential.clone());
        Ok(())
    }

    fn delete(&self, target: &str) -> Result<bool, CredentialError> {
        check_target(target, TargetUse::Delete)?;
        Ok(self.items().remove(&target.to_ascii_lowercase()).is_some())
    }
}

// ---------------------------------------------------------------------------------------------
// The real store.
// ---------------------------------------------------------------------------------------------

/// Windows Credential Manager, generic credentials persisted for this user on this machine
/// (`CRED_PERSIST_LOCAL_MACHINE`: they don't roam).
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsCredentialStore;

/// NUL-terminated UTF-16.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn windows_error(call: &'static str, e: &windows::core::Error) -> CredentialError {
    CredentialError::Windows {
        call,
        message: e.to_string(),
    }
}

fn is_not_found(e: &windows::core::Error) -> bool {
    e.code() == ERROR_NOT_FOUND.to_hresult()
}

/// The checks every real call makes before it reaches Windows: the prefixes and the use, then
/// the harness guard (an owner-tooling target under either name trips it once a test has armed
/// it).
fn checked_target(target: &str, use_: TargetUse) -> Result<Vec<u16>, CredentialError> {
    check_target(target, use_)?;
    guard::check_credential(target.to_string());
    Ok(wide(target))
}

/// Owns the buffer `CredReadW` allocated: zeroes the secret, then frees it, on every path.
struct ReadBuffer(*mut CREDENTIALW);

impl Drop for ReadBuffer {
    fn drop(&mut self) {
        // SAFETY: the pointer came from a successful `CredReadW` and is freed only here. The
        // blob pointer and size describe memory inside that same, still live, buffer.
        unsafe {
            let c = &*self.0;
            if !c.CredentialBlob.is_null() {
                std::ptr::write_bytes(c.CredentialBlob, 0, c.CredentialBlobSize as usize);
            }
            CredFree(self.0.cast_const().cast());
        }
    }
}

/// The text of a string field from Credential Manager, or `None` when it is null.
///
/// # Safety
/// `p` is null or points at a valid NUL-terminated UTF-16 string.
unsafe fn read_pwstr(p: PWSTR, field: &str) -> Result<Option<String>, CredentialError> {
    if p.is_null() {
        return Ok(None);
    }
    // SAFETY: the caller guarantees a valid NUL-terminated string.
    unsafe { p.to_string() }
        .map(Some)
        .map_err(|_| CredentialError::Windows {
            call: "CredReadW",
            message: format!("the credential's {field} is not valid UTF-16"),
        })
}

impl CredentialStore for WindowsCredentialStore {
    fn get(&self, target: &str) -> Result<Option<Credential>, CredentialError> {
        let name = checked_target(target, TargetUse::Read)?;
        let mut ptr: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: `name` is NUL-terminated and outlives the call; on success `ptr` receives a
        // buffer that `ReadBuffer` frees.
        let read = unsafe { CredReadW(PCWSTR(name.as_ptr()), CRED_TYPE_GENERIC, None, &mut ptr) };
        match read {
            Ok(()) => {}
            Err(e) if is_not_found(&e) => return Ok(None),
            Err(e) => return Err(windows_error("CredReadW", &e)),
        }
        let buffer = ReadBuffer(ptr);
        // SAFETY: `CredReadW` succeeded, so `buffer.0` points at a valid CREDENTIALW whose blob
        // is `CredentialBlobSize` bytes (or null) and whose strings are null or NUL-terminated.
        // Everything is copied out before `buffer` is dropped.
        unsafe {
            let c = &*buffer.0;
            let mut bytes = Zeroizing::new(Vec::new());
            if !c.CredentialBlob.is_null() {
                bytes.extend_from_slice(std::slice::from_raw_parts(
                    c.CredentialBlob,
                    c.CredentialBlobSize as usize,
                ));
            }
            let secret = match String::from_utf8(std::mem::take(&mut *bytes)) {
                Ok(s) => Zeroizing::new(s),
                Err(e) => {
                    e.into_bytes().zeroize();
                    return Err(CredentialError::Windows {
                        call: "CredReadW",
                        message: "the stored secret is not UTF-8".to_string(),
                    });
                }
            };
            Ok(Some(Credential {
                username: read_pwstr(c.UserName, "user name")?.unwrap_or_default(),
                secret,
                comment: read_pwstr(c.Comment, "comment")?,
            }))
        }
    }

    fn set(&self, target: &str, credential: &Credential) -> Result<(), CredentialError> {
        let mut name = checked_target(target, TargetUse::Write)?;
        check_credential(credential)?;
        let mut user = wide(&credential.username);
        let mut comment = credential.comment.as_deref().map(wide);
        let mut blob = Zeroizing::new(credential.secret.as_bytes().to_vec());
        let c = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(name.as_mut_ptr()),
            Comment: comment
                .as_mut()
                .map_or(PWSTR::null(), |c| PWSTR(c.as_mut_ptr())),
            // At most MAX_SECRET_BYTES (checked above), so it fits in a u32.
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: if blob.is_empty() {
                std::ptr::null_mut()
            } else {
                blob.as_mut_ptr()
            },
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            UserName: PWSTR(user.as_mut_ptr()),
            ..Default::default()
        };
        // SAFETY: every pointer in `c` points into a buffer that lives until after the call.
        unsafe { CredWriteW(&c, 0) }.map_err(|e| windows_error("CredWriteW", &e))
    }

    fn delete(&self, target: &str) -> Result<bool, CredentialError> {
        let name = checked_target(target, TargetUse::Delete)?;
        // SAFETY: `name` is NUL-terminated and outlives the call.
        match unsafe { CredDeleteW(PCWSTR(name.as_ptr()), CRED_TYPE_GENERIC, None) } {
            Ok(()) => Ok(true),
            Err(e) if is_not_found(&e) => Ok(false),
            Err(e) => Err(windows_error("CredDeleteW", &e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::names::{DEV_CREDENTIAL_PREFIX, LEGACY_DEV_CREDENTIAL_PREFIX};

    const USES: [TargetUse; 3] = [TargetUse::Read, TargetUse::Write, TargetUse::Delete];

    fn cred(secret: &str) -> Credential {
        Credential {
            username: "x-access-token".into(),
            secret: Zeroizing::new(secret.into()),
            comment: Some("expires=2027-01-31".into()),
        }
    }

    const REFUSED: &[&str] = &[
        "Worldwright/github/agent-token",
        "Worldwright-stable/x",
        "Worldwright-nightly/x",
        "Worldwright-dev",
        "Worldwright-dev/",
        "Worldwright-devx/y",
        "Worldwright-dev-x/y",
        "Worldwright-test-/x",
        "Worldwright-test-a b/x",
        "Worldwright-test-a.b/x",
        "Worldwright-test-abc",
        "Worldwright-test-abc/",
        "git:https://github.com",
        "gh:github.com:",
        "",
        "Worldwright-dev/a\nb",
        "Worldwright-dev/a\0b",
        "LILITH/github/agent-token",
        "LILITH-stable/x",
        "LILITH-nightly/x",
        "LILITH-dev",
        "LILITH-dev/",
        "LILITH-devx/y",
        "LILITH-dev-x/y",
        "LILITHSuite/x",
        "LILITH-dev/a\nb",
        "LILITH-dev/a\0b",
        // Test channels keep the legacy prefix until Task B3.
        "LILITH-test-0a1b2c3d/x",
    ];

    #[test] // spec: QA-079
    fn targets_outside_dev_and_test_prefixes_are_refused() {
        for use_ in USES {
            for t in REFUSED {
                assert!(
                    matches!(
                        check_target(t, use_),
                        Err(CredentialError::TargetRefused(_))
                    ),
                    "{t:?} {use_:?}"
                );
            }
            for t in [
                "LILITH-dev/github/agent-token",
                "lilith-DEV/x",
                "Worldwright-test-0a1b2c3d/roundtrip",
                "Worldwright-test-a_b-1/x/y",
            ] {
                assert_eq!(check_target(t, use_), Ok(()), "{t:?} {use_:?}");
            }
            assert!(check_target(&format!("Worldwright-test-{}/x", "a".repeat(65)), use_).is_err());
        }
    }

    #[test] // spec: QA-079
    fn check_target_accepts_the_new_dev_prefix_for_every_use() {
        for use_ in USES {
            for t in [
                format!("{DEV_CREDENTIAL_PREFIX}github/agent-token"),
                format!("{DEV_CREDENTIAL_PREFIX}x"),
                format!("{}x", DEV_CREDENTIAL_PREFIX.to_ascii_uppercase()),
                "lilith-DEV/a/b".to_string(),
            ] {
                assert_eq!(check_target(&t, use_), Ok(()), "{t:?} {use_:?}");
            }
        }
        // The fake store writes, reads and deletes it.
        let store = MemoryCredentialStore::new();
        let t = format!("{DEV_CREDENTIAL_PREFIX}github/agent-token");
        store.set(&t, &cred("s")).unwrap();
        assert_eq!(store.get(&t).unwrap(), Some(cred("s")));
        assert_eq!(store.delete(&t), Ok(true));
    }

    /// The legacy owner-tooling prefix is still read and deleted (R4), under any case.
    #[test] // spec: QA-079
    fn check_target_reads_and_deletes_both_dev_prefixes() {
        for prefix in [DEV_CREDENTIAL_PREFIX, LEGACY_DEV_CREDENTIAL_PREFIX] {
            for t in [
                format!("{prefix}x"),
                format!("{prefix}github/agent-token"),
                format!("{}x", prefix.to_ascii_uppercase()),
                format!("{}x", prefix.to_ascii_lowercase()),
            ] {
                for use_ in [TargetUse::Read, TargetUse::Delete] {
                    assert_eq!(check_target(&t, use_), Ok(()), "{t:?} {use_:?}");
                }
                let store = MemoryCredentialStore::new();
                assert_eq!(store.get(&t), Ok(None), "{t:?}");
                assert_eq!(store.delete(&t), Ok(false), "{t:?}");
            }
            // Either prefix with no name is refused for every use.
            for use_ in USES {
                assert!(check_target(prefix, use_).is_err(), "{prefix:?} {use_:?}");
            }
        }
    }

    /// The real store refuses before calling Windows, so these never reach Credential Manager.
    #[test] // spec: QA-079
    fn real_store_refuses_other_targets_before_any_windows_call() {
        let store = WindowsCredentialStore;
        for t in REFUSED {
            assert!(
                matches!(store.get(t), Err(CredentialError::TargetRefused(_))),
                "{t:?}"
            );
            assert!(
                matches!(
                    store.set(t, &cred("s")),
                    Err(CredentialError::TargetRefused(_))
                ),
                "{t:?}"
            );
            assert!(
                matches!(store.delete(t), Err(CredentialError::TargetRefused(_))),
                "{t:?}"
            );
        }
    }

    #[test] // spec: QA-079
    fn invalid_credentials_are_refused_before_any_windows_call() {
        let store = WindowsCredentialStore;
        let target = "Worldwright-test-00000000/never-written";
        let too_long = cred(&"x".repeat(MAX_SECRET_BYTES + 1));
        let mut long_comment = cred("s");
        long_comment.comment = Some("c".repeat(MAX_COMMENT_CHARS + 1));
        let mut nul_user = cred("s");
        nul_user.username = "a\0b".into();
        let mut nul_comment = cred("s");
        nul_comment.comment = Some("a\0b".into());
        for c in [too_long, long_comment, nul_user, nul_comment] {
            assert!(
                matches!(store.set(target, &c), Err(CredentialError::Invalid(_))),
                "{c:?}"
            );
            assert!(matches!(
                MemoryCredentialStore::new().set(target, &c),
                Err(CredentialError::Invalid(_))
            ));
        }
    }

    #[test] // spec: QA-079
    fn memory_store_round_trip() {
        let store = MemoryCredentialStore::new();
        let t = "LILITH-dev/github/agent-token";
        assert_eq!(store.get(t), Ok(None));
        store.set(t, &cred("one")).unwrap();
        assert_eq!(store.get(t).unwrap(), Some(cred("one")));
        // Targets match case-insensitively, like Credential Manager.
        assert_eq!(
            store.get("lilith-DEV/GITHUB/agent-TOKEN").unwrap(),
            Some(cred("one"))
        );
        store.set(t, &cred("two")).unwrap();
        assert_eq!(store.get(t).unwrap().unwrap().secret.as_str(), "two");
        assert_eq!(store.delete(t), Ok(true));
        assert_eq!(store.delete(t), Ok(false));
        assert_eq!(store.get(t), Ok(None));
        for bad in REFUSED {
            assert!(matches!(
                store.get(bad),
                Err(CredentialError::TargetRefused(_))
            ));
        }
    }

    #[test] // spec: QA-079
    fn debug_never_shows_the_secret() {
        let c = cred("github_pat_SECRETSECRET");
        let shown = format!("{c:?}");
        assert!(!shown.contains("SECRET"), "{shown}");
        assert!(shown.contains("redacted"), "{shown}");
        let store = MemoryCredentialStore::new();
        store.set("LILITH-dev/x", &c).unwrap();
        assert!(!format!("{store:?}").contains("SECRET"));
    }
}
