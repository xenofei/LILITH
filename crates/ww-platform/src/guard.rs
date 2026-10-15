//! The harness guard (QA-038, QA-040): once armed, no `Env` method may return the owner's real
//! Worldwright locations and no stable or dev `Env` may be built.
//!
//! `ww-testkit` arms the guard from `TestEnv::new()`. The guard is process-global and never
//! disarms. Production code never arms it, so the checks are a single `OnceLock` read.
//!
//! What counts as "real" (decided by [`RealLocations`]):
//!
//! - a path under the real roaming or local root whose first component below that root starts
//!   with `Worldwright` (so `Worldwright`, `Worldwright-dev`, `Worldwright-test-x`, ...);
//! - any path with a `.worldwright-local` component;
//! - the real default Claude Code profile, `<real user profile>\.claude`, and anything below
//!   it (the rest of the user profile is not real: `%TEMP%` lives under it);
//! - the real registry root, `HKCU\Software\Worldwright`, and its `-<channel>` variants, except
//!   `HKCU\Software\Worldwright-test-*`;
//! - the real credential prefix, `Worldwright/`, and its `-<channel>` variants, except
//!   `Worldwright-test-*`.
//!
//! Paths are compared by prefix, not substring, and case-insensitively, after stripping a
//! `\\?\` prefix. So a test directory such as `%TEMP%\.tmpXXXX\local\Worldwright-test-<id>`
//! (the temp folder lives under the real local root) is not real, but
//! `%LOCALAPPDATA%\Worldwright-test-<id>` is.
//!
//! Matching is textual: it catches accidents, not deliberate evasion via 8.3 names or
//! symlinks. QA-040's "registered workspace" check is added in plan 2, once the workspace
//! registry exists.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::env::{Channel, EnvError, normalise_for_hash};
use crate::known_folders;

static ARMED: OnceLock<RealLocations> = OnceLock::new();

/// The owner's real Worldwright locations (the stable channel's), which tests must never touch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealLocations {
    /// The real roaming root (`%APPDATA%`), not the Worldwright folder inside it.
    pub roaming: PathBuf,
    /// The real local root (`%LOCALAPPDATA%`), not the Worldwright folder inside it.
    pub local: PathBuf,
    /// The real user profile (`%USERPROFILE%`). Only `<user_profile>\.claude` is real.
    pub user_profile: PathBuf,
    /// `HKCU\Software\Worldwright`
    pub registry_root: String,
    /// `Worldwright/`
    pub credential_prefix: String,
}

impl RealLocations {
    /// The stable channel's real locations, from the known folders. Read-only: it creates
    /// nothing, and it does not build an `Env`, so it keeps working after the guard is armed.
    pub fn from_system() -> Result<RealLocations, EnvError> {
        let app = Channel::Stable.app_name();
        Ok(RealLocations {
            roaming: known_folders::roaming_app_data()?,
            local: known_folders::local_app_data()?,
            user_profile: known_folders::profile()?,
            registry_root: format!(r"HKCU\Software\{app}"),
            credential_prefix: format!("{app}/"),
        })
    }

    /// True when `path` is one of the real data folders (or inside one), the real default
    /// Claude Code profile (or inside it), or is in a `.worldwright-local` folder.
    pub fn is_real_path(&self, path: &Path) -> bool {
        let p = normalise_for_hash(path);
        if p.split('\\').any(|c| c == ".worldwright-local") {
            return true;
        }
        let worldwright_folder = [&self.roaming, &self.local].into_iter().any(|root| {
            first_component_below(&p, &normalise_for_hash(root))
                .is_some_and(|c| c.starts_with("worldwright"))
        });
        worldwright_folder
            || first_component_below(&p, &normalise_for_hash(&self.user_profile))
                .is_some_and(|c| c == ".claude")
    }

    /// True for the real registry root, its non-test channel variants and anything below them.
    pub fn is_real_registry(&self, value: &str) -> bool {
        is_real_name(value, &self.registry_root, &['-', '\\'])
    }

    /// True for the real credential prefix, its non-test channel variants and anything below.
    pub fn is_real_credential(&self, value: &str) -> bool {
        is_real_name(
            value,
            self.credential_prefix.trim_end_matches('/'),
            &['-', '/'],
        )
    }
}

/// `p` and `root` are already normalised. The first path component of `p` below `root`.
fn first_component_below<'a>(p: &'a str, root: &str) -> Option<&'a str> {
    let rest = p
        .strip_prefix(root.trim_end_matches('\\'))?
        .strip_prefix('\\')?;
    rest.split('\\').next()
}

/// `value` equals `real_root` (ASCII case-insensitively), or continues it with one of `seps`,
/// unless the continuation is a `-test-` channel.
fn is_real_name(value: &str, real_root: &str, seps: &[char]) -> bool {
    let v = value.to_ascii_lowercase();
    let r = real_root.to_ascii_lowercase();
    let Some(rest) = v.strip_prefix(r.as_str()) else {
        return false;
    };
    if rest.is_empty() {
        return true;
    }
    if rest.starts_with("-test-") {
        return false;
    }
    rest.starts_with(|c| seps.contains(&c))
}

/// Arms the process-global guard. Idempotent: the first call wins, later calls do nothing.
/// Once armed, every `Env` method that returns a path or a name checks it, and building a
/// stable or dev `Env` panics.
pub fn arm_test_guard(real: RealLocations) {
    let _ = ARMED.set(real);
}

fn trip(value: &str) -> ! {
    panic!("test resolved a real Worldwright location: {value}");
}

pub(crate) fn check_path(path: PathBuf) -> PathBuf {
    if let Some(real) = ARMED.get()
        && real.is_real_path(&path)
    {
        trip(&path.display().to_string());
    }
    path
}

pub(crate) fn check_registry(value: String) -> String {
    if let Some(real) = ARMED.get()
        && real.is_real_registry(&value)
    {
        trip(&value);
    }
    value
}

pub(crate) fn check_credential(value: String) -> String {
    if let Some(real) = ARMED.get()
        && real.is_real_credential(&value)
    {
        trip(&value);
    }
    value
}

/// Names that carry only the channel (pipe, AUMID): once armed, only test channels may
/// produce them.
pub(crate) fn check_channel_name(channel: &Channel, value: String) -> String {
    if ARMED.get().is_some() && !matches!(channel, Channel::Test(_)) {
        trip(&value);
    }
    value
}

pub(crate) fn check_channel_constructible(channel: &Channel) {
    if ARMED.get().is_some() && !matches!(channel, Channel::Test(_)) {
        trip(&format!("an Env for the {channel} channel"));
    }
}
