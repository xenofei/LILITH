//! The harness guard (QA-038, QA-040): once armed, no `Env` method may return the owner's real
//! `<App>` locations (either app name, see below) and no stable or dev `Env` may be built.
//!
//! `lil-testkit` arms the guard from `TestEnv::new()`. The guard is process-global and never
//! disarms. Production code never arms it, so the checks are a single `OnceLock` read.
//!
//! What counts as "real" (decided by [`RealLocations`]). Each rule covers both app names, the
//! new [`names::APP_NAME`] and the legacy [`names::LEGACY_APP_NAME`] (R4), written `<App>`
//! here:
//!
//! - a path under the real roaming or local root whose first component below that root starts
//!   with `<App>` (so `<App>`, `<App>-dev`, `<App>-test-x`, ...);
//! - any path with a [`names::LOCAL_DIR`] or [`names::LEGACY_LOCAL_DIR`] component;
//! - the real default Claude Code profile, `<real user profile>\.claude`, and anything below
//!   it (the rest of the user profile is not real: `%TEMP%` lives under it);
//! - the real registry root, `HKCU\Software\<App>`, and its `-<channel>` variants, except
//!   `HKCU\Software\<App>-test-*`;
//! - the real credential prefix, `<App>/`, and its `-<channel>` variants, except
//!   `<App>-test-*`.
//!
//! The registry root and credential prefix in [`RealLocations`] are built under one app name;
//! the guard derives the other name's root and prefix from them.
//!
//! Paths are compared by prefix, not substring, and case-insensitively, after stripping a
//! `\\?\` prefix. So a test directory such as `%TEMP%\.tmpXXXX\local\<App>-test-<id>`
//! (the temp folder lives under the real local root) is not real, but
//! `%LOCALAPPDATA%\<App>-test-<id>` is.
//!
//! Workspaces are a separate check, [`check_workspace`]: a workspace registered in a real
//! `suite.toml` (`default_workspace` or `[[workspaces]]`), or anything below it, is real.
//! [`RealLocations::from_system`] reads those files, read-only, from every non-test `<App>` or
//! `<App>-<channel>` roaming folder under either name (the real stable and dev registries).
//! That read is the only access to the owner's real suite files.
//!
//! Matching is textual: it catches accidents, not deliberate evasion via 8.3 names or
//! symlinks.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::env::{Channel, EnvError, normalise_for_hash};
use crate::known_folders;
use crate::names;

static ARMED: OnceLock<RealLocations> = OnceLock::new();

/// Both app names: the guard treats each as real (R4).
const APP_NAMES: [&str; 2] = [names::APP_NAME, names::LEGACY_APP_NAME];
/// Both per-volume local-data folder names.
const LOCAL_DIRS: [&str; 2] = [names::LOCAL_DIR, names::LEGACY_LOCAL_DIR];

/// The owner's real `<App>` locations (the stable channel's, under either app name), which
/// tests must never touch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealLocations {
    /// The real roaming root (`%APPDATA%`), not the `<App>` folder inside it.
    pub roaming: PathBuf,
    /// The real local root (`%LOCALAPPDATA%`), not the `<App>` folder inside it.
    pub local: PathBuf,
    /// The real user profile (`%USERPROFILE%`). Only `<user_profile>\.claude` is real.
    pub user_profile: PathBuf,
    /// `HKCU\Software\<App>` (the guard derives the other app name's root from it)
    pub registry_root: String,
    /// `<App>/` (the guard derives the other app name's prefix from it)
    pub credential_prefix: String,
    /// Every workspace registered in a real `suite.toml` (see [`RealLocations::registered_under`]).
    pub workspaces: Vec<PathBuf>,
}

impl RealLocations {
    /// The stable channel's real locations, from the known folders. Read-only: it creates
    /// nothing, and it does not build an `Env`, so it keeps working after the guard is armed.
    pub fn from_system() -> Result<RealLocations, EnvError> {
        let app = Channel::Stable.app_name();
        let roaming = known_folders::roaming_app_data()?;
        let workspaces = RealLocations::registered_under(&roaming);
        Ok(RealLocations {
            roaming,
            local: known_folders::local_app_data()?,
            user_profile: known_folders::profile()?,
            registry_root: format!(r"HKCU\Software\{app}"),
            credential_prefix: format!("{app}/"),
            workspaces,
        })
    }

    /// The workspaces registered under `roaming`: reads, read-only, the `suite.toml` of every
    /// folder directly below it named `<App>` or `<App>-<channel>` under either app name,
    /// except `<App>-test-*`. That covers the real stable and dev registries. Folders are read
    /// in name order; a missing or malformed file contributes nothing.
    pub fn registered_under(roaming: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(roaming) else {
            return Vec::new();
        };
        let mut folders: Vec<(String, PathBuf)> = entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                is_registry_folder(&name).then(|| (name.to_ascii_lowercase(), entry.path()))
            })
            .collect();
        folders.sort();
        let mut found = Vec::new();
        for (_, folder) in folders {
            let Ok(text) = std::fs::read_to_string(folder.join("suite.toml")) else {
                continue;
            };
            for ws in parse_registered_workspaces(&text) {
                if !found.contains(&ws) {
                    found.push(ws);
                }
            }
        }
        found
    }

    /// True when `path` is a real registered workspace or inside one.
    pub fn is_real_workspace(&self, path: &Path) -> bool {
        let p = normalise_for_hash(path);
        self.workspaces.iter().any(|ws| {
            let ws = normalise_for_hash(ws);
            p == ws
                || p.strip_prefix(ws.trim_end_matches('\\'))
                    .is_some_and(|rest| rest.starts_with('\\'))
        })
    }

    /// True when `path` is one of the real data folders under either app name (or inside one),
    /// the real default Claude Code profile (or inside it), or is in a local-data folder under
    /// either name.
    pub fn is_real_path(&self, path: &Path) -> bool {
        let p = normalise_for_hash(path);
        if p.split('\\')
            .any(|c| LOCAL_DIRS.iter().any(|d| c.eq_ignore_ascii_case(d)))
        {
            return true;
        }
        let app_folder = [&self.roaming, &self.local].into_iter().any(|root| {
            first_component_below(&p, &normalise_for_hash(root)).is_some_and(|c| {
                APP_NAMES
                    .iter()
                    .any(|app| starts_with_ignore_ascii_case(c, app))
            })
        });
        app_folder
            || first_component_below(&p, &normalise_for_hash(&self.user_profile))
                .is_some_and(|c| c == ".claude")
    }

    /// True for the real registry root under either app name, its non-test channel variants
    /// and anything below them.
    pub fn is_real_registry(&self, value: &str) -> bool {
        with_both_app_names(&self.registry_root)
            .iter()
            .any(|root| is_real_name(value, root, &['-', '\\']))
    }

    /// True for the real credential prefix under either app name, its non-test channel variants
    /// and anything below.
    pub fn is_real_credential(&self, value: &str) -> bool {
        with_both_app_names(self.credential_prefix.trim_end_matches('/'))
            .iter()
            .any(|root| is_real_name(value, root, &['-', '/']))
    }
}

/// `<App>` or `<App>-<channel>` under either app name, but not a test channel
/// (`<App>-test-*`). `<App>Suite` (the install folder) is not one: no hyphen.
fn is_registry_folder(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    APP_NAMES.iter().any(|app| {
        let app = app.to_ascii_lowercase();
        n == app
            || n.strip_prefix(&app)
                .and_then(|rest| rest.strip_prefix('-'))
                .is_some_and(|channel| !channel.starts_with("test-"))
    })
}

/// `s` starts with `prefix`, ASCII case-insensitively.
fn starts_with_ignore_ascii_case(s: &str, prefix: &str) -> bool {
    s.as_bytes()
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix.as_bytes()))
}

/// `root` (a real registry root or credential prefix without its `/`) and the same root under
/// the other app name, when `root` ends with one of them (ASCII case-insensitively). A root
/// that ends with neither is returned alone.
fn with_both_app_names(root: &str) -> Vec<String> {
    for app in APP_NAMES {
        let Some(cut) = root.len().checked_sub(app.len()) else {
            continue;
        };
        if root
            .get(cut..)
            .is_some_and(|tail| tail.eq_ignore_ascii_case(app))
        {
            let base = &root[..cut];
            return APP_NAMES.iter().map(|a| format!("{base}{a}")).collect();
        }
    }
    vec![root.to_string()]
}

/// The workspace paths a `suite.toml` registers: `default_workspace` first, then each
/// `[[workspaces]]` `path`, without repeats. Entries that are not non-empty strings are
/// skipped; text that is not TOML registers nothing.
pub fn parse_registered_workspaces(suite_toml: &str) -> Vec<PathBuf> {
    let Ok(table) = suite_toml.parse::<toml::Table>() else {
        return Vec::new();
    };
    let default = table.get("default_workspace");
    let listed = table
        .get("workspaces")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .map(|entry| entry.get("path"));
    let mut found: Vec<PathBuf> = Vec::new();
    for value in std::iter::once(default).chain(listed) {
        if let Some(path) = value
            .and_then(toml::Value::as_str)
            .filter(|s| !s.is_empty())
        {
            let path = PathBuf::from(path);
            if !found.contains(&path) {
                found.push(path);
            }
        }
    }
    found
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
    panic!("test resolved a real {} location: {value}", names::APP_NAME);
}

pub(crate) fn check_path(path: PathBuf) -> PathBuf {
    if let Some(real) = ARMED.get()
        && real.is_real_path(&path)
    {
        trip(&path.display().to_string());
    }
    path
}

/// Panics when the guard is armed and `path` is, or is under, a workspace registered in a real
/// `suite.toml`. Returns `path` otherwise. The core calls it before it opens a workspace, so a
/// test channel cannot touch the owner's real work (QA-040).
pub fn check_workspace(path: &Path) -> PathBuf {
    if let Some(real) = ARMED.get()
        && real.is_real_workspace(path)
    {
        trip(&path.display().to_string());
    }
    path.to_path_buf()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::names::{APP_NAME, LEGACY_APP_NAME, LEGACY_LOCAL_DIR, LOCAL_DIR};

    /// Fake real locations whose registry root and credential prefix are under `app`.
    fn fake_real(app: &str) -> RealLocations {
        RealLocations {
            roaming: PathBuf::from(r"C:\Fake\AppData\Roaming"),
            local: PathBuf::from(r"C:\Fake\AppData\Local"),
            user_profile: PathBuf::from(r"C:\Fake"),
            registry_root: format!(r"HKCU\Software\{app}"),
            credential_prefix: format!("{app}/"),
            workspaces: Vec::new(),
        }
    }

    /// Both app names, whichever one the real locations were built from (R4).
    const APPS: [&str; 2] = [APP_NAME, LEGACY_APP_NAME];

    #[test] // spec: QA-040
    fn guard_blocks_lilith_and_legacy_data_folders() {
        for app in APPS {
            let real = fake_real(app);
            for root in [r"C:\Fake\AppData\Roaming", r"c:\fake\appdata\LOCAL"] {
                for name in APPS {
                    let lower = name.to_ascii_lowercase();
                    for folder in [
                        name.to_string(),
                        format!("{lower}-dev"),
                        format!("{lower}-test-x"),
                        format!("{}-Test-x", name.to_ascii_uppercase()),
                    ] {
                        for p in [
                            format!(r"{root}\{folder}"),
                            format!(r"{root}\{folder}\logs"),
                        ] {
                            assert!(real.is_real_path(Path::new(&p)), "{app}: {p}");
                        }
                    }
                }
            }
            for dir in [LOCAL_DIR, LEGACY_LOCAL_DIR] {
                for p in [format!(r"D:\{dir}"), format!(r"\\?\E:\{dir}\abc")] {
                    assert!(real.is_real_path(Path::new(&p)), "{app}: {p}");
                }
            }
            for p in [
                r"D:\.LiLiTh-Local\abc",
                r"D:\.Worldwright-Local\abc", // legacy-guard
            ] {
                assert!(real.is_real_path(Path::new(p)), "{app}: {p}");
            }
            for name in APPS {
                for p in [
                    format!(r"C:\Fake\AppData\Local\Temp\{name}\x"),
                    format!(r"C:\Elsewhere\{name}-test-abc"),
                    format!(r"C:\Fake\AppData\Roaming2\{name}"),
                    format!(r"D:\{name}-local\abc"),
                    format!(r"D:\.{}-localx\abc", name.to_ascii_lowercase()),
                ] {
                    assert!(!real.is_real_path(Path::new(&p)), "{app}: {p}");
                }
            }
        }
    }

    #[test] // spec: QA-040
    fn lilithsuite_is_not_a_channel_folder() {
        let dir = tempfile::tempdir().unwrap();
        let write = |folder: &str, ws: &str| {
            let p = dir.path().join(folder);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(
                p.join("suite.toml"),
                format!("default_workspace = '{ws}'\n"),
            )
            .unwrap();
        };
        write("LILITH", r"C:\Ws\Stable");
        write("lilith-dev", r"C:\Ws\Dev");
        write("LILITH-test-ab", r"C:\Ws\Test");
        write("LILITHSuite", r"C:\Ws\NotAChannelFolder");
        write("LILITHSuite.Dev", r"C:\Ws\NotAChannelFolderEither");
        write(LEGACY_APP_NAME, r"C:\Ws\LegacyStable");
        assert_eq!(
            RealLocations::registered_under(dir.path()),
            [
                PathBuf::from(r"C:\Ws\Stable"),
                PathBuf::from(r"C:\Ws\Dev"),
                PathBuf::from(r"C:\Ws\LegacyStable"),
            ]
        );
    }

    #[test] // spec: QA-040
    fn guard_blocks_both_real_credential_prefixes() {
        for app in APPS {
            let real = fake_real(app);
            for name in APPS {
                let upper = name.to_ascii_uppercase();
                for t in [
                    format!("{name}/x"),
                    format!("{name}/"),
                    format!("{name}-dev/x"),
                    format!("{upper}-DEV/x"),
                    format!("{name}-stable/x"),
                ] {
                    assert!(real.is_real_credential(&t), "{app}: {t}");
                }
                for t in [
                    format!("{name}-test-abc/x"),
                    format!("{upper}-TEST-abc/x"),
                    format!("{name}Suite/x"),
                    format!("{name}x/y"),
                ] {
                    assert!(!real.is_real_credential(&t), "{app}: {t}");
                }
            }
            assert!(!real.is_real_credential("Other/x"), "{app}");
        }
    }

    #[test] // spec: QA-040
    fn guard_blocks_both_registry_roots() {
        for app in APPS {
            let real = fake_real(app);
            for name in APPS {
                let upper = name.to_ascii_uppercase();
                for v in [
                    format!(r"HKCU\Software\{name}"),
                    format!(r"hkcu\software\{upper}-dev"),
                    format!(r"HKCU\Software\{name}\Sub"),
                    format!(r"HKCU\Software\{name}-stable\Sub"),
                ] {
                    assert!(real.is_real_registry(&v), "{app}: {v}");
                }
                for v in [
                    format!(r"HKCU\Software\{name}-test-abc"),
                    format!(r"HKCU\Software\{name}Suite"),
                    format!(r"HKCU\Software\Other\{name}"),
                ] {
                    assert!(!real.is_real_registry(&v), "{app}: {v}");
                }
            }
            assert!(!real.is_real_registry(r"HKCU\Software\Other"), "{app}");
        }
    }

    #[test] // spec: QA-040
    fn parse_registered_workspaces_reads_both_keys_and_ignores_garbage() {
        let text = r"
default_workspace = 'C:\Ws\Default'
unknown_key = 1

[[workspaces]]
path = 'C:\Ws\One'
label = 'kept by the type, ignored here'

[[workspaces]]
other = 'no path'

[[workspaces]]
path = 5

[[workspaces]]
path = ''

[[workspaces]]
path = 'C:\Ws\Default'

[[workspaces]]
path = 'C:\Ws\Two'
";
        assert_eq!(
            parse_registered_workspaces(text),
            [
                PathBuf::from(r"C:\Ws\Default"),
                PathBuf::from(r"C:\Ws\One"),
                PathBuf::from(r"C:\Ws\Two"),
            ]
        );
        assert_eq!(
            parse_registered_workspaces(r"default_workspace = 'C:\Ws\Only'"),
            [PathBuf::from(r"C:\Ws\Only")]
        );
        for garbage in [
            "",
            "not toml [[[",
            "default_workspace = 5",
            "workspaces = 'x'",
            "workspaces = [1, 2]",
            "[workspaces]\npath = 'C:/Ws/NotAnArray'",
            "\u{0}\u{1}binary",
        ] {
            assert!(
                parse_registered_workspaces(garbage).is_empty(),
                "{garbage:?}"
            );
        }
    }

    #[test] // spec: QA-040
    fn registered_under_reads_stable_and_dev_but_not_test_folders() {
        let dir = tempfile::tempdir().unwrap();
        let write = |folder: &str, ws: &str| {
            let p = dir.path().join(folder);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(
                p.join("suite.toml"),
                format!("default_workspace = '{ws}'\n"),
            )
            .unwrap();
        };
        let legacy = LEGACY_APP_NAME;
        write(APP_NAME, r"C:\Ws\Stable");
        write(&format!("{APP_NAME}-dev"), r"C:\Ws\Dev");
        write(&format!("{APP_NAME}-test-ab"), r"C:\Ws\Test");
        write("Other", r"C:\Ws\Other");
        write(&format!("{APP_NAME}Suite"), r"C:\Ws\NotAChannelFolder");
        write(&format!("{legacy}-test-ab"), r"C:\Ws\LegacyTest");
        // Missing and malformed files contribute nothing.
        std::fs::create_dir_all(dir.path().join(format!("{APP_NAME}-empty"))).unwrap();
        let bad = dir.path().join(format!("{legacy}-bad"));
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("suite.toml"), "][ not toml").unwrap();
        // A stray file named like a folder is skipped.
        std::fs::write(dir.path().join(format!("{APP_NAME}-file")), b"x").unwrap();

        assert_eq!(
            RealLocations::registered_under(dir.path()),
            [PathBuf::from(r"C:\Ws\Stable"), PathBuf::from(r"C:\Ws\Dev")]
        );
        assert!(RealLocations::registered_under(&dir.path().join("missing")).is_empty());
    }

    #[test] // spec: QA-040
    fn is_real_workspace_matches_by_component_prefix() {
        let mut real = RealLocations {
            roaming: PathBuf::from(r"C:\R"),
            local: PathBuf::from(r"C:\L"),
            user_profile: PathBuf::from(r"C:\U"),
            registry_root: format!(r"HKCU\Software\{APP_NAME}"),
            credential_prefix: format!("{APP_NAME}/"),
            workspaces: vec![PathBuf::from(r"C:\Ws\One")],
        };
        assert!(real.is_real_workspace(Path::new(r"c:\ws\one")));
        assert!(real.is_real_workspace(Path::new(r"C:\Ws\One\a\b")));
        assert!(real.is_real_workspace(Path::new(r"\\?\C:\Ws\One\")));
        assert!(!real.is_real_workspace(Path::new(r"C:\Ws\Onex")));
        assert!(!real.is_real_workspace(Path::new(r"C:\Ws")));
        real.workspaces.clear();
        assert!(!real.is_real_workspace(Path::new(r"C:\Ws\One")));
    }

    #[test] // spec: QA-040
    fn check_workspace_is_a_no_op_while_the_guard_is_not_armed() {
        // Unit tests never arm the process-global guard.
        let p = PathBuf::from(r"C:\Anything\At\All");
        assert_eq!(check_workspace(&p), p);
    }
}
