//! The environment provider: every machine-specific value comes from [`Env`] (ARCH-095,
//! ARCH-097, QA-038).
//!
//! Naming contract (QA-038):
//!
//! | Value             | Stable channel                                 | Other channels                        |
//! |-------------------|------------------------------------------------|---------------------------------------|
//! | Data folders      | `<roaming>\Worldwright`, `<local>\Worldwright` | `Worldwright-<channel>`               |
//! | Registry root     | `HKCU\Software\Worldwright`                    | `HKCU\Software\Worldwright-<channel>` |
//! | Credential prefix | `Worldwright/`                                 | `Worldwright-<channel>/`              |

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::volume::{SystemVolumeResolver, VolumeResolver};
use crate::{guard, known_folders, sid};

/// Release channel. Display form: `stable`, `dev` or `test-<id>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Channel {
    Stable,
    Dev,
    /// An isolated test channel. The id must satisfy [`Channel::is_valid`] because it ends up
    /// in folder names, registry keys, credential names and pipe names.
    Test(String),
}

impl Channel {
    /// True when the channel is safe to embed in paths, registry keys and pipe names:
    /// a `Test` id is 1 to 64 ASCII letters, digits, `-` or `_`.
    pub fn is_valid(&self) -> bool {
        match self {
            Channel::Stable | Channel::Dev => true,
            Channel::Test(id) => {
                (1..=64).contains(&id.len())
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            }
        }
    }

    /// Display form with the first letter capitalised: `Stable`, `Dev` or `Test-<id>`.
    /// Used for the notification AUMID.
    fn capitalised(&self) -> String {
        match self {
            Channel::Stable => "Stable".to_string(),
            Channel::Dev => "Dev".to_string(),
            Channel::Test(id) => format!("Test-{id}"),
        }
    }

    /// `Worldwright` for stable, `Worldwright-<channel>` otherwise.
    pub(crate) fn app_name(&self) -> String {
        match self {
            Channel::Stable => "Worldwright".to_string(),
            other => format!("Worldwright-{other}"),
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Channel::Stable => f.write_str("stable"),
            Channel::Dev => f.write_str("dev"),
            Channel::Test(id) => write!(f, "test-{id}"),
        }
    }
}

/// The three per-user roots everything else hangs off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roots {
    pub roaming: PathBuf,
    pub local: PathBuf,
    pub user_profile: PathBuf,
}

#[derive(Debug)]
pub enum EnvError {
    /// A Windows call failed.
    Windows { call: &'static str, message: String },
    /// A filesystem call failed.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The channel cannot be embedded in paths, registry keys or pipe names.
    InvalidChannel(String),
}

impl fmt::Display for EnvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EnvError::Windows { call, message } => write!(f, "{call} failed: {message}"),
            EnvError::Io { path, source } => write!(f, "{}: {source}", path.display()),
            EnvError::InvalidChannel(c) => write!(f, "invalid channel `{c}`"),
        }
    }
}

impl std::error::Error for EnvError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            EnvError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Machine-specific values for one channel.
#[derive(Debug, Clone)]
pub struct Env {
    channel: Channel,
    roots: Roots,
    user_sid: String,
    volumes: Arc<dyn VolumeResolver>,
}

impl Env {
    /// Resolves the roots and the user SID from the running system.
    pub fn from_system(channel: Channel) -> Result<Env, EnvError> {
        if !channel.is_valid() {
            return Err(EnvError::InvalidChannel(channel.to_string()));
        }
        let roots = Roots {
            roaming: known_folders::roaming_app_data()?,
            local: known_folders::local_app_data()?,
            user_profile: known_folders::profile()?,
        };
        Ok(Env::with_roots(channel, roots, sid::current_user_sid()?))
    }

    /// Builds an `Env` from explicit roots and SID. Used by `ww-testkit`; never touches the
    /// system known folders. Volume lookups use the real resolver unless replaced with
    /// [`Env::with_volume_resolver`].
    ///
    /// # Panics
    /// - If `channel` fails [`Channel::is_valid`] (a programming error in the caller).
    /// - If the test guard is armed and `channel` is `Stable` or `Dev` (QA-040).
    pub fn with_roots(channel: Channel, roots: Roots, user_sid: String) -> Env {
        assert!(channel.is_valid(), "invalid channel `{channel}`");
        guard::check_channel_constructible(&channel);
        Env {
            channel,
            roots,
            user_sid,
            volumes: Arc::new(SystemVolumeResolver),
        }
    }

    /// Replaces the volume resolver (tests inject a fake).
    #[must_use]
    pub fn with_volume_resolver(mut self, volumes: Arc<dyn VolumeResolver>) -> Env {
        self.volumes = volumes;
        self
    }

    pub fn channel(&self) -> &Channel {
        &self.channel
    }

    /// `<roaming>\Worldwright[-<channel>]`
    pub fn data_dir(&self) -> PathBuf {
        guard::check_path(self.roots.roaming.join(self.channel.app_name()))
    }

    /// `<local>\Worldwright[-<channel>]`
    pub fn local_dir(&self) -> PathBuf {
        guard::check_path(self.roots.local.join(self.channel.app_name()))
    }

    /// `local_dir\logs`
    pub fn logs_dir(&self) -> PathBuf {
        guard::check_path(self.local_dir().join("logs"))
    }

    /// First 16 lowercase hex characters of SHA-256 over the canonical workspace path.
    ///
    /// `canonical` must already be canonical (`std::fs::canonicalize`); this function only
    /// normalises its spelling: strips a `\\?\` prefix, ASCII-lowercases and uses `\`
    /// separators. It does not touch the filesystem.
    pub fn workspace_hash(canonical: &Path) -> String {
        let normalised = normalise_for_hash(canonical);
        let digest = Sha256::digest(normalised.as_bytes());
        digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
    }

    /// Per-workspace local data folder. Canonicalises `workspace` (so it must exist), then
    /// defers to [`Env::workspace_local_dir_for_canonical`].
    pub fn workspace_local_dir(&self, workspace: &Path) -> Result<PathBuf, EnvError> {
        let canonical = std::fs::canonicalize(workspace).map_err(|source| EnvError::Io {
            path: workspace.to_path_buf(),
            source,
        })?;
        self.workspace_local_dir_for_canonical(&canonical)
    }

    /// The pure half of [`Env::workspace_local_dir`]: `canonical` is already canonical.
    ///
    /// - Same volume as `local_dir`: `<local_dir>\workspaces\<hash>`.
    /// - Otherwise: `<volume root>\.worldwright-local\<hash>`.
    pub fn workspace_local_dir_for_canonical(&self, canonical: &Path) -> Result<PathBuf, EnvError> {
        let hash = Self::workspace_hash(canonical);
        let ws = strip_verbatim(canonical);
        let local = self.local_dir();
        let ws_volume = self
            .volumes
            .volume_root(&ws)
            .map_err(|source| EnvError::Io { path: ws, source })?;
        let local_volume = self
            .volumes
            .volume_root(&local)
            .map_err(|source| EnvError::Io {
                path: local.clone(),
                source,
            })?;
        if normalise_for_hash(&ws_volume) == normalise_for_hash(&local_volume) {
            Ok(guard::check_path(local.join("workspaces").join(hash)))
        } else {
            Ok(guard::check_path(
                ws_volume.join(".worldwright-local").join(hash),
            ))
        }
    }

    /// `HKCU\Software\Worldwright[-<channel>]`
    pub fn registry_root(&self) -> String {
        guard::check_registry(format!(r"HKCU\Software\{}", self.channel.app_name()))
    }

    /// `Worldwright[-<channel>]/`
    pub fn credential_prefix(&self) -> String {
        guard::check_credential(format!("{}/", self.channel.app_name()))
    }

    /// `\\.\pipe\worldwright-core-<userSID>-<channel>`
    pub fn pipe_name(&self) -> String {
        guard::check_channel_name(
            &self.channel,
            format!(
                r"\\.\pipe\worldwright-core-{}-{}",
                self.user_sid, self.channel
            ),
        )
    }

    /// `Worldwright.Suite.<Channel>` with the capitalised channel: `Stable`, `Dev`, `Test-<id>`.
    pub fn notification_aumid(&self) -> String {
        guard::check_channel_name(
            &self.channel,
            format!("Worldwright.Suite.{}", self.channel.capitalised()),
        )
    }

    /// `<user_profile>\.claude`
    pub fn claude_default_profile_dir(&self) -> PathBuf {
        guard::check_path(self.roots.user_profile.join(".claude"))
    }

    /// `local_dir\claude-profiles`
    pub fn claude_profiles_dir(&self) -> PathBuf {
        self.local_dir().join("claude-profiles")
    }
}

/// Removes a leading `\\?\` (and turns `\\?\UNC\server\share` into `\\server\share`).
fn strip_verbatim(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

/// The spelling that is hashed: no `\\?\`, ASCII-lowercase, `\` separators, no trailing
/// separator (except a bare root such as `c:\`).
pub(crate) fn normalise_for_hash(path: &Path) -> String {
    let stripped = strip_verbatim(path);
    let mut s = stripped.to_string_lossy().replace('/', "\\");
    s.make_ascii_lowercase();
    while s.len() > 3 && s.ends_with('\\') {
        s.pop();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    /// Fake resolver: the volume root is the drive prefix (`C:\`), no filesystem access.
    #[derive(Debug)]
    struct DriveLetterVolumes;

    impl VolumeResolver for DriveLetterVolumes {
        fn volume_root(&self, path: &Path) -> io::Result<PathBuf> {
            let s = path.to_string_lossy();
            let b = s.as_bytes();
            if b.len() >= 2 && b[1] == b':' {
                Ok(PathBuf::from(format!("{}:\\", s[..1].to_ascii_uppercase())))
            } else {
                Err(io::Error::new(io::ErrorKind::InvalidInput, "no drive"))
            }
        }
    }

    fn roots() -> Roots {
        Roots {
            roaming: PathBuf::from(r"C:\R"),
            local: PathBuf::from(r"C:\L"),
            user_profile: PathBuf::from(r"C:\U"),
        }
    }

    fn env(channel: Channel) -> Env {
        Env::with_roots(channel, roots(), "S-1-5-21-1-2-3-1001".to_string())
            .with_volume_resolver(Arc::new(DriveLetterVolumes))
    }

    #[test] // spec: QA-038
    fn stable_paths() {
        let e = env(Channel::Stable);
        assert_eq!(e.data_dir(), PathBuf::from(r"C:\R\Worldwright"));
        assert_eq!(e.registry_root(), r"HKCU\Software\Worldwright");
        assert_eq!(e.credential_prefix(), "Worldwright/");
    }

    #[test] // spec: QA-038
    fn dev_paths_are_suffixed() {
        let e = env(Channel::Dev);
        assert_eq!(e.data_dir(), PathBuf::from(r"C:\R\Worldwright-dev"));
        assert_eq!(e.registry_root(), r"HKCU\Software\Worldwright-dev");
        assert_eq!(e.credential_prefix(), "Worldwright-dev/");
    }

    #[test] // spec: QA-038
    fn local_logs_and_claude_dirs_follow_channel() {
        let e = env(Channel::Test("abc".into()));
        assert_eq!(e.local_dir(), PathBuf::from(r"C:\L\Worldwright-test-abc"));
        assert_eq!(
            e.logs_dir(),
            PathBuf::from(r"C:\L\Worldwright-test-abc\logs")
        );
        assert_eq!(
            e.claude_profiles_dir(),
            PathBuf::from(r"C:\L\Worldwright-test-abc\claude-profiles")
        );
        assert_eq!(
            e.claude_default_profile_dir(),
            PathBuf::from(r"C:\U\.claude")
        );
        assert_eq!(e.registry_root(), r"HKCU\Software\Worldwright-test-abc");
        assert_eq!(e.credential_prefix(), "Worldwright-test-abc/");
    }

    #[test] // spec: ARCH-095
    fn pipe_name_format() {
        assert_eq!(
            env(Channel::Dev).pipe_name(),
            r"\\.\pipe\worldwright-core-S-1-5-21-1-2-3-1001-dev"
        );
        assert_eq!(
            env(Channel::Test("x1".into())).pipe_name(),
            r"\\.\pipe\worldwright-core-S-1-5-21-1-2-3-1001-test-x1"
        );
    }

    #[test] // spec: ARCH-095
    fn aumid_uses_capitalised_channel() {
        assert_eq!(
            env(Channel::Stable).notification_aumid(),
            "Worldwright.Suite.Stable"
        );
        assert_eq!(
            env(Channel::Dev).notification_aumid(),
            "Worldwright.Suite.Dev"
        );
        assert_eq!(
            env(Channel::Test("abc".into())).notification_aumid(),
            "Worldwright.Suite.Test-abc"
        );
    }

    #[test] // spec: ARCH-095
    fn channel_display_and_validity() {
        assert_eq!(Channel::Stable.to_string(), "stable");
        assert_eq!(Channel::Dev.to_string(), "dev");
        assert_eq!(Channel::Test("a_b-1".into()).to_string(), "test-a_b-1");
        assert!(Channel::Test("a_b-1".into()).is_valid());
        for bad in ["", "a b", "a\\b", "a/b", "a.b", "..", "x\u{e9}"] {
            assert!(!Channel::Test(bad.into()).is_valid(), "{bad:?}");
        }
        assert!(!Channel::Test("a".repeat(65)).is_valid());
    }

    #[test] // spec: ARCH-097
    fn workspace_hash_is_16_hex_and_case_insensitive() {
        let a = Env::workspace_hash(Path::new(r"C:\Ws\A"));
        let b = Env::workspace_hash(Path::new(r"c:\ws\a"));
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        assert!(a.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')));
    }

    /// Golden literal: pins the normalisation and the hash so a later change cannot silently
    /// move on-disk locations. Computed once, independently of this code, as SHA-256 of the
    /// bytes `c:\ws\a` (first 8 bytes, lowercase hex). If this fails, existing per-workspace
    /// data would be orphaned.
    #[test] // spec: QA-038
    fn workspace_hash_golden_literal() {
        assert_eq!(
            Env::workspace_hash(Path::new(r"C:\ws\a")),
            "f40a274106259d18"
        );
        assert_eq!(
            Env::workspace_hash(Path::new(r"\\?\C:\WS\A\")),
            "f40a274106259d18"
        );
    }

    #[test] // spec: ARCH-097
    fn workspace_hash_ignores_verbatim_prefix_and_separator_style() {
        let plain = Env::workspace_hash(Path::new(r"C:\Ws\A"));
        assert_eq!(plain, Env::workspace_hash(Path::new(r"\\?\C:\Ws\A")));
        assert_eq!(plain, Env::workspace_hash(Path::new("C:/Ws/A")));
        assert_eq!(plain, Env::workspace_hash(Path::new(r"C:\Ws\A\")));
        assert_ne!(plain, Env::workspace_hash(Path::new(r"C:\Ws\B")));
        assert_eq!(
            Env::workspace_hash(Path::new(r"\\?\UNC\srv\share\x")),
            Env::workspace_hash(Path::new(r"\\srv\share\x"))
        );
    }

    #[test] // spec: ARCH-097
    fn workspace_hash_matches_sha256_of_normalised_path() {
        let digest = Sha256::digest(br"c:\ws\a");
        let want: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(Env::workspace_hash(Path::new(r"C:\Ws\A")), want);
    }

    #[test] // spec: ARCH-097
    fn same_volume_goes_under_local() {
        let e = env(Channel::Stable);
        let ws = Path::new(r"C:\x");
        let h = Env::workspace_hash(ws);
        assert_eq!(
            e.workspace_local_dir_for_canonical(ws).unwrap(),
            PathBuf::from(format!(r"C:\L\Worldwright\workspaces\{h}"))
        );
    }

    #[test] // spec: ARCH-097
    fn other_volume_goes_to_worldwright_local() {
        let e = env(Channel::Stable);
        let ws = Path::new(r"D:\x");
        let h = Env::workspace_hash(ws);
        assert_eq!(
            e.workspace_local_dir_for_canonical(ws).unwrap(),
            PathBuf::from(format!(r"D:\.worldwright-local\{h}"))
        );
    }

    #[test] // spec: ARCH-097
    fn verbatim_workspace_path_is_resolved_without_prefix() {
        let e = env(Channel::Stable);
        let ws = Path::new(r"\\?\D:\x");
        let h = Env::workspace_hash(ws);
        assert_eq!(
            e.workspace_local_dir_for_canonical(ws).unwrap(),
            PathBuf::from(format!(r"D:\.worldwright-local\{h}"))
        );
    }

    /// `workspace_local_dir` canonicalises through the filesystem. The temp directory is a
    /// real path; the provider only reads it.
    #[test] // spec: ARCH-097
    fn workspace_local_dir_canonicalises_existing_path() {
        #[derive(Debug)]
        struct OneVolume;
        impl VolumeResolver for OneVolume {
            fn volume_root(&self, _: &Path) -> io::Result<PathBuf> {
                Ok(PathBuf::from(r"C:\"))
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(tmp.path()).unwrap();
        let e = env(Channel::Stable).with_volume_resolver(Arc::new(OneVolume));
        let got = e.workspace_local_dir(tmp.path()).unwrap();
        let want =
            PathBuf::from(r"C:\L\Worldwright\workspaces").join(Env::workspace_hash(&canonical));
        assert_eq!(got, want);
        // A missing path is an Io error, not a panic.
        let missing = tmp.path().join("does-not-exist");
        assert!(matches!(
            e.workspace_local_dir(&missing),
            Err(EnvError::Io { .. })
        ));
    }

    #[test] // spec: QA-038
    fn from_system_rejects_invalid_channel() {
        // Validation happens before any system lookup.
        assert!(matches!(
            Env::from_system(Channel::Test("a b".into())),
            Err(EnvError::InvalidChannel(_))
        ));
    }

    #[test]
    #[should_panic(expected = "invalid channel")]
    fn with_roots_panics_on_invalid_channel() {
        let _ = Env::with_roots(Channel::Test("a/b".into()), roots(), "S-1".into());
    }
}
