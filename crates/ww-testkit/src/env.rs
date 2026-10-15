//! `TestEnv`: an `Env` whose every root lives under a fresh temporary directory (QA-038,
//! QA-040).
//!
//! Creating a `TestEnv` arms the process-global harness guard in `ww-platform`. From then on,
//! any `Env` method in the process that would return the owner's real Worldwright location
//! (roaming or local `Worldwright*` folders, `.worldwright-local`, the real registry root or
//! credential prefix) panics with `test resolved a real Worldwright location: ...`, and
//! building a stable or dev `Env` panics too.

use std::hash::{BuildHasher, RandomState};
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use tempfile::TempDir;
use ww_platform::{Channel, Env, RealLocations, Roots, arm_test_guard};

/// A fake but well-formed user SID for test pipe names.
const TEST_SID: &str = "S-1-5-21-1000-2000-3000-1001";

/// An isolated `Env`: channel `Test(<8 lowercase hex>)`, roots under a fresh temporary
/// directory that is deleted on drop.
pub struct TestEnv {
    pub env: Env,
    _dir: TempDir,
}

impl TestEnv {
    /// Arms the guard with the real locations, then builds the isolated `Env`.
    ///
    /// # Panics
    /// If the known folders cannot be resolved or the temporary directory cannot be created.
    pub fn new() -> TestEnv {
        static REAL: OnceLock<RealLocations> = OnceLock::new();
        let real = REAL.get_or_init(|| {
            RealLocations::from_system().expect("cannot resolve the real known folders")
        });
        arm_test_guard(real.clone());

        let dir = tempfile::Builder::new()
            .prefix("ww-testenv-")
            .tempdir()
            .expect("cannot create the test directory");
        let roots = Roots {
            roaming: dir.path().join("roaming"),
            local: dir.path().join("local"),
            user_profile: dir.path().join("profile"),
        };
        for r in [&roots.roaming, &roots.local, &roots.user_profile] {
            std::fs::create_dir_all(r).expect("cannot create a test root");
        }
        let env = Env::with_roots(Channel::Test(unique_id()), roots, TEST_SID.to_string());
        TestEnv { env, _dir: dir }
    }

    /// The temporary directory that holds every root.
    pub fn root(&self) -> &Path {
        self._dir.path()
    }
}

impl Default for TestEnv {
    fn default() -> Self {
        TestEnv::new()
    }
}

/// 8 lowercase hex characters, distinct per call within a process and random across processes.
/// Lowercase only: Windows paths, registry keys and pipe names are case-insensitive, so mixed
/// case ids could collide.
fn unique_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let h = RandomState::new().hash_one((std::process::id(), n));
    format!("{:08x}", h as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ww_platform::{Channel, Env, RealLocations, Roots};

    #[test] // spec: QA-040
    fn testenv_paths_are_under_tempdir() {
        let t = TestEnv::new();
        let base = t.root().to_path_buf();
        for p in [
            t.env.data_dir(),
            t.env.local_dir(),
            t.env.logs_dir(),
            t.env.claude_profiles_dir(),
            t.env.claude_default_profile_dir(),
        ] {
            assert!(
                p.starts_with(&base),
                "{} not under {}",
                p.display(),
                base.display()
            );
        }
        assert!(matches!(t.env.channel(), Channel::Test(_)));
    }

    #[test] // spec: QA-040
    fn testenv_ids_are_lowercase_alphanumeric_and_distinct() {
        let a = TestEnv::new();
        let b = TestEnv::new();
        let id = |t: &TestEnv| match t.env.channel() {
            Channel::Test(id) => id.clone(),
            other => panic!("not a test channel: {other}"),
        };
        for t in [&a, &b] {
            let id = id(t);
            assert_eq!(id.len(), 8);
            assert!(
                id.bytes()
                    .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase()),
                "{id}"
            );
        }
        assert_ne!(id(&a), id(&b));
        assert_ne!(a.env.data_dir(), b.env.data_dir());
    }

    #[test] // spec: QA-040
    #[should_panic(expected = "real Worldwright location")]
    fn guard_panics_on_real_appdata() {
        let _t = TestEnv::new(); // arms the guard
        let real = RealLocations::from_system().expect("known folders");
        let roots = Roots {
            roaming: real.roaming.clone(),
            local: real.local.clone(),
            user_profile: real.local.clone(),
        };
        let env = Env::with_roots(Channel::Test("abcd1234".into()), roots, "S-1".into());
        let _ = env.data_dir();
    }

    #[test] // spec: QA-040
    #[should_panic(expected = "real Worldwright location")]
    fn guard_panics_on_stable_channel_in_tests() {
        let t = TestEnv::new();
        let roots = Roots {
            roaming: t.root().join("r"),
            local: t.root().join("l"),
            user_profile: t.root().join("u"),
        };
        let _ = Env::with_roots(Channel::Stable, roots, "S-1".into());
    }

    #[test] // spec: QA-040
    fn registry_and_credentials_are_test_scoped() {
        let t = TestEnv::new();
        assert!(
            t.env
                .registry_root()
                .starts_with(r"HKCU\Software\Worldwright-test-")
        );
        assert!(t.env.credential_prefix().starts_with("Worldwright-test-"));
        assert!(t.env.pipe_name().contains("-test-"));
        assert!(
            t.env
                .notification_aumid()
                .starts_with("Worldwright.Suite.Test-")
        );
    }

    #[test] // spec: QA-040
    fn second_testenv_does_not_trip_the_guard() {
        let _a = TestEnv::new();
        let b = TestEnv::new();
        let _ = b
            .env
            .workspace_local_dir(b.root())
            .expect("same-volume workspace");
    }
}
