//! `lil_platform::hello`'s calls that never raise a Windows Hello prompt (QA-088, Task 28).
//!
//! The prompting calls (create, sign, consent) are not run by any automated test: they need a
//! person, and the route step's probe (`examples/hello-probe.rs`) covers them once, with the
//! owner. DPAPI never shows UI here (`CRYPTPROTECT_UI_FORBIDDEN`) and writes nothing.

use lil_platform::hello;

#[test] // spec: QA-088
fn dpapi_round_trips_for_the_current_user() {
    let plain = b"-----BEGIN PRIVATE KEY-----\nnot a real key\n-----END PRIVATE KEY-----\n";
    let sealed = hello::dpapi_protect(plain).unwrap();
    assert!(!sealed.windows(plain.len()).any(|w| w == plain));
    assert_eq!(&hello::dpapi_unprotect(&sealed).unwrap()[..], &plain[..]);
}

#[test]
fn a_damaged_dpapi_blob_is_an_error() {
    let mut sealed = hello::dpapi_protect(b"secret").unwrap();
    let last = sealed.len() - 1;
    sealed[last] ^= 0xff;
    assert!(hello::dpapi_unprotect(&sealed).is_err());
    assert!(hello::dpapi_unprotect(b"not a DPAPI blob").is_err());
}

#[test]
fn a_missing_credential_is_not_found_and_removing_it_is_fine() {
    if !hello::supported() {
        eprintln!("SKIPPED: Windows Hello is not set up on this machine");
        return;
    }
    let name = format!("LILITH-test-hello-{}", std::process::id());
    assert!(!hello::credential_exists(&name).unwrap());
    hello::kek_remove(&name).unwrap();
}

#[test]
fn challenges_are_random() {
    assert_ne!(
        hello::new_challenge().unwrap(),
        hello::new_challenge().unwrap()
    );
}
