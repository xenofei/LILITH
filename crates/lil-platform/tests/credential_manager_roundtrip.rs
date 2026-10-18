//! The one test that touches the real Windows Credential Manager (ruling R28).
//!
//! It round-trips a fresh `LILITH-test-<8 hex>/roundtrip` target and deletes it again,
//! through a drop guard, so the target is removed even when an assertion fails. No test
//! touches a stable or dev channel target, or an owner-tooling one, under either app name.

use std::time::{SystemTime, UNIX_EPOCH};

use lil_platform::credentials::{Credential, CredentialStore, WindowsCredentialStore};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// Deletes the target when dropped, also while unwinding from a failed assertion.
struct DeleteOnDrop(String);

impl Drop for DeleteOnDrop {
    fn drop(&mut self) {
        let _ = WindowsCredentialStore.delete(&self.0);
    }
}

/// Eight hex characters from the process id and the clock.
fn fresh_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let digest = Sha256::digest(format!("{}-{nanos}", std::process::id()).as_bytes());
    digest[..4].iter().map(|b| format!("{b:02x}")).collect()
}

#[test] // spec: QA-079
fn test_target_round_trips_through_credential_manager() {
    let target = format!("LILITH-test-{}/roundtrip", fresh_id());
    let _cleanup = DeleteOnDrop(target.clone());
    let store = WindowsCredentialStore;

    assert_eq!(
        store.get(&target).unwrap(),
        None,
        "the target must be fresh"
    );

    let first = Credential {
        username: "x-access-token".into(),
        secret: Zeroizing::new("not-a-real-token-1".into()),
        comment: Some("expires=2027-01-31".into()),
    };
    store.set(&target, &first).unwrap();
    assert_eq!(store.get(&target).unwrap(), Some(first));

    // Replacing keeps one credential; no comment reads back as none.
    let second = Credential {
        username: "x-access-token".into(),
        secret: Zeroizing::new("not-a-real-token-2".into()),
        comment: None,
    };
    store.set(&target, &second).unwrap();
    assert_eq!(store.get(&target).unwrap(), Some(second));

    assert!(store.delete(&target).unwrap());
    assert_eq!(store.get(&target).unwrap(), None);
    assert!(!store.delete(&target).unwrap());
}
