//! The environment provider: every machine-specific value comes from [`Env`] (ARCH-095,
//! ARCH-097, QA-038).
//!
//! Naming contract (QA-038):
//!
//! | Value             | Stable channel                       | Other channels                   |
//! |-------------------|--------------------------------------|----------------------------------|
//! | Data folders      | `<roaming>\LILITH`, `<local>\LILITH` | `LILITH-<channel>`               |
//! | Registry root     | `HKCU\Software\LILITH`               | `HKCU\Software\LILITH-<channel>` |
//! | Credential prefix | `LILITH/`                            | `LILITH-<channel>/`              |
//!
//! Legacy names (R4): the roaming folder under the legacy app name
//! ([`names::LEGACY_APP_NAME`]) is read, never written: its `suite.toml` when the new one is
//! missing ([`Env::suite_toml_to_read`]).

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::paths::strip_verbatim;
use crate::process_env::{
    ENV_CHANNEL, ENV_TEST_IDLE_SECS, ENV_TEST_PARENT_PID, ENV_TEST_ROOT, TEST_PIPE_SID,
    TestOverrides,
};
use crate::volume::{SystemVolumeResolver, VolumeResolver};
use crate::{guard, known_folders, names, sid};

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
    /// Used for the notification AUMID, the shortcut's name and the scheme's display name.
    pub(crate) fn capitalised(&self) -> String {
        match self {
            Channel::Stable => "Stable".to_string(),
            Channel::Dev => "Dev".to_string(),
            Channel::Test(id) => format!("Test-{id}"),
        }
    }

    /// `LILITH` for stable, `LILITH-<channel>` otherwise ([`names::APP_NAME`]).
    pub(crate) fn app_name(&self) -> String {
        Channel::named(names::APP_NAME, self)
    }

    /// [`Channel::app_name`] under the legacy app name (R4 readers only).
    pub(crate) fn legacy_app_name(&self) -> String {
        Channel::named(names::LEGACY_APP_NAME, self)
    }

    /// `app` for stable, `app-<channel>` otherwise.
    fn named(app: &str, channel: &Channel) -> String {
        match channel {
            Channel::Stable => app.to_string(),
            other => format!("{app}-{other}"),
        }
    }
}

impl FromStr for Channel {
    type Err = EnvError;

    /// Parses the display form: `stable`, `dev` or `test-<id>` with a valid id.
    fn from_str(s: &str) -> Result<Channel, EnvError> {
        let channel = match s {
            "stable" => Channel::Stable,
            "dev" => Channel::Dev,
            _ => match s.strip_prefix("test-") {
                Some(id) => Channel::Test(id.to_string()),
                None => return Err(EnvError::InvalidChannel(s.to_string())),
            },
        };
        if channel.is_valid() {
            Ok(channel)
        } else {
            Err(EnvError::InvalidChannel(s.to_string()))
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
    /// The process asked for a channel this build does not serve (a stable build serves only
    /// `stable`; a dev build serves `dev` and `test-<id>`).
    ChannelNotAllowed { build: String, requested: String },
    /// A test channel was requested but `LIL_TEST_ROOT` is not set.
    TestRootMissing,
    /// `LIL_TEST_ROOT` is not an absolute path to an existing directory.
    TestRootInvalid(PathBuf),
}

impl fmt::Display for EnvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EnvError::Windows { call, message } => write!(f, "{call} failed: {message}"),
            EnvError::Io { path, source } => write!(f, "{}: {source}", path.display()),
            EnvError::InvalidChannel(c) => write!(f, "invalid channel `{c}`"),
            EnvError::ChannelNotAllowed { build, requested } => write!(
                f,
                "a {build} build cannot run on the `{requested}` channel ({ENV_CHANNEL})"
            ),
            EnvError::TestRootMissing => {
                write!(f, "a test channel needs {ENV_TEST_ROOT} to be set")
            }
            EnvError::TestRootInvalid(p) => write!(
                f,
                "{ENV_TEST_ROOT} must be an absolute path to an existing folder: {}",
                p.display()
            ),
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

/// The machine id on every test channel (ARCH-081).
pub const TEST_MACHINE_ID: &str = "testmachine";

/// Machine-specific values for one channel.
#[derive(Debug, Clone)]
pub struct Env {
    channel: Channel,
    roots: Roots,
    user_sid: String,
    volumes: Arc<dyn VolumeResolver>,
    /// The folder of the AUMID shortcut: `FOLDERID_Programs` from [`Env::from_system`], else
    /// `<user_profile>\Programs` (on a test channel `<test root>\profile\Programs`).
    programs: PathBuf,
    /// A desktop-group test channel registers under the shell's classes root
    /// ([`Env::with_shell_classes_root`]).
    shell_classes: bool,
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
        let programs = known_folders::programs()?;
        Ok(Env::with_roots(channel, roots, sid::current_user_sid()?).with_programs_dir(programs))
    }

    /// The environment of the running process: [`Env::from_vars`] with the compile-time build
    /// channel and the real process variables.
    pub fn from_process() -> Result<Env, EnvError> {
        Env::from_vars(crate::process_env::BUILD_CHANNEL, &|name| {
            std::env::var(name).ok()
        })
    }

    /// Resolves the channel from the build channel and the `LIL_CHANNEL` variable (ARCH-094).
    ///
    /// - A `stable` build serves only `stable`; any other `LIL_CHANNEL` is refused.
    /// - Any other build serves `dev` (the default) and `test-<id>`.
    /// - A test channel needs `LIL_TEST_ROOT`, an absolute existing folder, and uses
    ///   `<root>\roaming`, `<root>\local` and `<root>\profile` with the fixed
    ///   [`TEST_PIPE_SID`]. The harness guard is armed with the owner's real locations
    ///   (including their registered workspaces) *before* the `Env` is built (QA-040).
    pub fn from_vars(
        build_channel: &str,
        var: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Env, EnvError> {
        let requested = var(ENV_CHANNEL);
        let not_allowed = |requested: &str| EnvError::ChannelNotAllowed {
            build: build_channel.to_string(),
            requested: requested.to_string(),
        };
        let channel = if build_channel == "stable" {
            match requested.as_deref() {
                None | Some("stable") => Channel::Stable,
                Some(other) => return Err(not_allowed(other)),
            }
        } else {
            match requested.as_deref() {
                None | Some("dev") => Channel::Dev,
                Some(other) => match other.parse::<Channel>() {
                    Ok(channel @ Channel::Test(_)) => channel,
                    Err(e) if other.starts_with("test-") => return Err(e),
                    _ => return Err(not_allowed(other)),
                },
            }
        };
        if !matches!(channel, Channel::Test(_)) {
            return Env::from_system(channel);
        }
        let root = PathBuf::from(var(ENV_TEST_ROOT).ok_or(EnvError::TestRootMissing)?);
        if !root.is_absolute() || !root.is_dir() {
            return Err(EnvError::TestRootInvalid(root));
        }
        guard::arm_test_guard(guard::RealLocations::from_system()?);
        let roots = Roots {
            roaming: root.join("roaming"),
            local: root.join("local"),
            user_profile: root.join("profile"),
        };
        Ok(Env::with_roots(channel, roots, TEST_PIPE_SID.to_string()))
    }

    /// The test-only knobs from the process variables: all `None` unless this is a test
    /// channel. A value that does not parse is `None`.
    pub fn test_overrides(&self, var: &dyn Fn(&str) -> Option<String>) -> TestOverrides {
        if !matches!(self.channel, Channel::Test(_)) {
            return TestOverrides::default();
        }
        TestOverrides {
            parent_pid: var(ENV_TEST_PARENT_PID).and_then(|v| v.trim().parse().ok()),
            idle_secs: var(ENV_TEST_IDLE_SECS).and_then(|v| v.trim().parse().ok()),
        }
    }

    /// Builds an `Env` from explicit roots and SID. Used by `lil-testkit`; never touches the
    /// system known folders. Volume lookups use the real resolver unless replaced with
    /// [`Env::with_volume_resolver`].
    ///
    /// # Panics
    /// - If `channel` fails [`Channel::is_valid`] (a programming error in the caller).
    /// - If the test guard is armed and `channel` is `Stable` or `Dev` (QA-040).
    pub fn with_roots(channel: Channel, roots: Roots, user_sid: String) -> Env {
        assert!(channel.is_valid(), "invalid channel `{channel}`");
        guard::check_channel_constructible(&channel);
        let programs = roots.user_profile.join("Programs");
        Env {
            channel,
            roots,
            user_sid,
            volumes: Arc::new(SystemVolumeResolver),
            programs,
            shell_classes: false,
        }
    }

    /// The desktop group's opt-in (Appendix G: the test classes root is for test channels
    /// *outside* the desktop group): this test channel's [`Env::classes_root`] becomes the
    /// shell's own, `HKCU\Software\Classes`, so its `lil-test-<id>` scheme resolves and its
    /// toast CLSID is registered where COM reads it. Only the scheme and the CLSID are written
    /// there, never the `.lilref` association ([`crate::desktop::register_handlers`]), and the
    /// desktop group's cleanup removes exactly those names. The armed guard still trips on it.
    ///
    /// # Panics
    /// On a stable or dev channel (a programming error).
    #[must_use]
    pub fn with_shell_classes_root(mut self) -> Env {
        assert!(
            matches!(&self.channel, Channel::Test(id)
                if id.starts_with(crate::desktop::DESKTOP_TEST_ID_PREFIX)),
            "only a desktop-group test channel takes the shell classes root this way"
        );
        self.shell_classes = true;
        self
    }

    /// Whether this is a desktop-group test channel on the shell's classes root.
    pub(crate) fn uses_shell_classes_root(&self) -> bool {
        self.shell_classes
    }

    /// Replaces the folder the AUMID shortcut goes in. [`Env::from_system`] sets the real
    /// `FOLDERID_Programs`; the desktop group's host run sets it for a test channel, so Windows
    /// knows that channel's AUMID (P4a-R39). The guard still checks the shortcut it gives.
    #[must_use]
    pub fn with_programs_dir(mut self, programs: PathBuf) -> Env {
        self.programs = programs;
        self
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

    /// `<roaming>\LILITH[-<channel>]`
    pub fn data_dir(&self) -> PathBuf {
        guard::check_path(self.roots.roaming.join(self.channel.app_name()))
    }

    /// `<local>\LILITH[-<channel>]`
    pub fn local_dir(&self) -> PathBuf {
        guard::check_path(self.roots.local.join(self.channel.app_name()))
    }

    /// The installed program folder, `<local root>\LILITHSuite\current`. Only the stable
    /// channel is installed; the other channels run from a build folder (ARCH-094). It is a
    /// sibling of the data folders, never a parent: nothing the suite writes goes inside it.
    pub fn install_dir(&self) -> Option<PathBuf> {
        match self.channel {
            Channel::Stable => Some(guard::check_path(
                self.roots
                    .local
                    .join(format!("{}Suite", names::APP_NAME))
                    .join("current"),
            )),
            Channel::Dev | Channel::Test(_) => None,
        }
    }

    /// `data_dir\suite.toml`: the workspace registry.
    pub fn suite_toml(&self) -> PathBuf {
        self.data_dir().join("suite.toml")
    }

    /// The `suite.toml` to read: [`Env::suite_toml`] if it exists, else the legacy folder's
    /// file if that exists (R4), else [`Env::suite_toml`]. Writes always go to
    /// [`Env::suite_toml`].
    ///
    /// Only existence is checked; nothing is created or written. A file whose existence can't
    /// be checked (new or legacy) counts as existing, so its read reports the error: a legacy
    /// file that is there but can't be read is never taken for a missing one, or a write would
    /// start from an empty text and drop its settings (caps among them).
    pub fn suite_toml_to_read(&self) -> PathBuf {
        let new = self.suite_toml();
        let new_exists = new.try_exists();
        let legacy = self.legacy_data_dir().join("suite.toml");
        choose_suite_toml(new, new_exists, legacy, |legacy| legacy.try_exists())
    }

    /// `<roaming>\<legacy app name>[-<channel>]`: read only, for [`Env::suite_toml_to_read`]
    /// (R4). Nothing is ever written there.
    pub fn legacy_data_dir(&self) -> PathBuf {
        guard::check_path(self.roots.roaming.join(self.channel.legacy_app_name()))
    }

    /// `local_dir\state.db`: the suite state database (CORE-005).
    pub fn suite_state_db(&self) -> PathBuf {
        self.local_dir().join("state.db")
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
    /// - Otherwise: `<volume root>\.lilith-local\<hash>` ([`names::LOCAL_DIR`]).
    pub fn workspace_local_dir_for_canonical(&self, canonical: &Path) -> Result<PathBuf, EnvError> {
        self.local_dir_under(canonical, &self.local_dir(), names::LOCAL_DIR)
    }

    /// `<local>\workspaces\<hash>` when `canonical` is on `local`'s volume, else
    /// `<volume root>\<volume_dir>\<hash>`.
    fn local_dir_under(
        &self,
        canonical: &Path,
        local: &Path,
        volume_dir: &str,
    ) -> Result<PathBuf, EnvError> {
        let hash = Self::workspace_hash(canonical);
        let ws = strip_verbatim(canonical);
        let local = local.to_path_buf();
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
            Ok(guard::check_path(ws_volume.join(volume_dir).join(hash)))
        }
    }

    /// `HKCU\Software\LILITH[-<channel>]`
    pub fn registry_root(&self) -> String {
        guard::check_registry(format!(r"HKCU\Software\{}", self.channel.app_name()))
    }

    /// `LILITH[-<channel>]/`
    pub fn credential_prefix(&self) -> String {
        guard::check_credential(format!("{}/", self.channel.app_name()))
    }

    /// `\\.\pipe\lilith-core-<userSID>-<channel>`. The legacy pipe is not served (R4, D8).
    pub fn pipe_name(&self) -> String {
        guard::check_channel_name(
            &self.channel,
            format!(r"\\.\pipe\lilith-core-{}-{}", self.user_sid, self.channel),
        )
    }

    /// `LILITH.Suite.<Channel>` with the capitalised channel: `Stable`, `Dev`, `Test-<id>`.
    pub fn notification_aumid(&self) -> String {
        guard::check_channel_name(
            &self.channel,
            format!("{}.Suite.{}", names::APP_NAME, self.channel.capitalised()),
        )
    }

    /// The channel's activation scheme (P4a-R2): `lil` for stable, `lil-dev` for dev,
    /// `lil-test-<id>` for a test channel.
    ///
    /// # Panics
    /// On a test channel whose id holds `_`: a URL scheme is letters, digits, `+`, `-` and `.`
    /// only (RFC 3986 §3.1), and the test kit's and the desktop group's ids are hex.
    pub fn activation_scheme(&self) -> String {
        let scheme = match self.channel {
            Channel::Stable => "lil".to_string(),
            Channel::Dev => "lil-dev".to_string(),
            Channel::Test(ref id) => {
                assert!(
                    id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
                    "test channel `{id}` can't make a URL scheme (letters, digits and `-` only)"
                );
                format!("lil-test-{id}")
            }
        };
        guard::check_channel_name(&self.channel, scheme)
    }

    /// The per-user classes root the handlers and the toast CLSID are registered under
    /// (Appendix G): `HKCU\Software\Classes` for stable and dev, and for a desktop-group test
    /// channel that opted in ([`Env::with_shell_classes_root`]); `<registry root>\Classes`
    /// (`HKCU\Software\LILITH-test-<id>\Classes`) for every other test channel, which the shell
    /// never reads.
    pub fn classes_root(&self) -> String {
        let root = match self.channel {
            Channel::Test(_) if !self.shell_classes => format!(r"{}\Classes", self.registry_root()),
            _ => r"HKCU\Software\Classes".to_string(),
        };
        guard::check_classes_root(root)
    }

    /// The AUMID shortcut (APP-022): `<Programs>\LILITH.lnk` for stable,
    /// `<Programs>\LILITH (<Channel>).lnk` otherwise, with the capitalised channel.
    pub fn start_menu_shortcut(&self) -> PathBuf {
        let name = match self.channel {
            Channel::Stable => format!("{}.lnk", names::APP_NAME),
            _ => format!("{} ({}).lnk", names::APP_NAME, self.channel.capitalised()),
        };
        guard::check_shortcut(self.programs.join(name))
    }

    /// The shortcut's `System.AppUserModel.ToastActivatorCLSID` (P4a-R37): fixed per channel and
    /// different for each, in the registry form `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`. It is
    /// the first 16 bytes of SHA-256 over the UTF-8 text `<AUMID> ToastActivatorCLSID`
    /// (`LILITH.Suite.<Channel> ToastActivatorCLSID`), with the version nibble set to 5 and the
    /// RFC 4122 variant bits set: a v5-style layout, not an RFC 4122 v5 UUID (that one is SHA-1
    /// over a namespace).
    pub fn toast_activator_clsid(&self) -> String {
        let aumid = format!("{}.Suite.{}", names::APP_NAME, self.channel.capitalised());
        let digest = Sha256::digest(format!("{aumid} ToastActivatorCLSID").as_bytes());
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        bytes[6] = (bytes[6] & 0x0F) | 0x50;
        bytes[8] = (bytes[8] & 0x3F) | 0x80;
        let v = u128::from_be_bytes(bytes);
        guard::check_channel_name(
            &self.channel,
            format!(
                "{{{:08X}-{:04X}-{:04X}-{:04X}-{:012X}}}",
                v >> 96,
                (v >> 80) & 0xFFFF,
                (v >> 64) & 0xFFFF,
                (v >> 48) & 0xFFFF,
                v & 0xFFFF_FFFF_FFFF
            ),
        )
    }

    /// `local_dir\apps\<app>`: an app's own state (P4a-R16).
    ///
    /// # Panics
    /// If `app` isn't 1 to 64 ASCII letters, digits, `-` or `_` (a programming error).
    pub fn app_state_dir(&self, app: &str) -> PathBuf {
        assert!(is_valid_app(app), "invalid app name `{app}`");
        self.local_dir().join("apps").join(app)
    }

    /// An app's single-instance pipe (APP-013), `\\.\pipe\lilith-app-<app>-<userSID>-<channel>`,
    /// or with `confirm` its confirm window's,
    /// `\\.\pipe\lilith-app-<app>-confirm-<userSID>-<channel>` (P4a-R11).
    ///
    /// # Panics
    /// If `app` isn't 1 to 64 ASCII letters, digits, `-` or `_` (a programming error).
    pub fn app_pipe_name(&self, app: &str, confirm: bool) -> String {
        assert!(is_valid_app(app), "invalid app name `{app}`");
        let confirm = if confirm { "-confirm" } else { "" };
        guard::check_channel_name(
            &self.channel,
            format!(
                r"\\.\pipe\lilith-app-{app}{confirm}-{}-{}",
                self.user_sid, self.channel
            ),
        )
    }

    /// `local_dir\diagnostics`: the diagnostics bundles (P4a-R19).
    pub fn diagnostics_dir(&self) -> PathBuf {
        self.local_dir().join("diagnostics")
    }

    /// `local_dir\cache\lilref`: the `.lilref` files `lilref.write` makes (APP-021).
    pub fn lilref_dir(&self) -> PathBuf {
        self.local_dir().join("cache").join("lilref")
    }

    /// The `<machine>` of append-log names (ARCH-081): [`TEST_MACHINE_ID`] on test channels,
    /// else [`machine_id_from`] of the computer name (`COMPUTERNAME`; `unknown` when unset).
    pub fn machine_id(&self) -> String {
        match self.channel {
            Channel::Test(_) => TEST_MACHINE_ID.to_string(),
            Channel::Stable | Channel::Dev => match std::env::var("COMPUTERNAME") {
                Ok(name) if !name.is_empty() => machine_id_from(&name),
                _ => "unknown".to_string(),
            },
        }
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

/// [`Env::suite_toml_to_read`]'s choice: `new` unless it is known to be missing, then `legacy`
/// unless that is known to be missing, else `new`. An existence check that fails counts as
/// "there", so the read that follows reports the error instead of a missing file.
fn choose_suite_toml(
    new: PathBuf,
    new_exists: std::io::Result<bool>,
    legacy: PathBuf,
    legacy_exists: impl FnOnce(&Path) -> std::io::Result<bool>,
) -> PathBuf {
    if new_exists.unwrap_or(true) || !legacy_exists(&legacy).unwrap_or(true) {
        new
    } else {
        legacy
    }
}

/// An app name that can go into folder and pipe names: 1 to 64 ASCII letters, digits, `-` or `_`.
fn is_valid_app(app: &str) -> bool {
    (1..=64).contains(&app.len())
        && app
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The machine id of append-log names (ARCH-081): `computer_name` lowercased, with every
/// character outside `[a-z0-9-]` replaced by `-`, one `-` per character. Lowercasing is ASCII:
/// a non-ASCII letter is outside the set either way, so each input character maps to exactly
/// one output character.
pub fn machine_id_from(computer_name: &str) -> String {
    computer_name
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
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
        assert_eq!(e.data_dir(), PathBuf::from(r"C:\R\LILITH"));
        assert_eq!(e.registry_root(), r"HKCU\Software\LILITH");
        assert_eq!(e.credential_prefix(), "LILITH/");
    }

    #[test] // spec: QA-038
    fn dev_paths_are_suffixed() {
        let e = env(Channel::Dev);
        assert_eq!(e.data_dir(), PathBuf::from(r"C:\R\LILITH-dev"));
        assert_eq!(e.registry_root(), r"HKCU\Software\LILITH-dev");
        assert_eq!(e.credential_prefix(), "LILITH-dev/");
    }

    #[test] // spec: QA-038
    fn local_logs_and_claude_dirs_follow_channel() {
        let e = env(Channel::Test("abc".into()));
        assert_eq!(e.local_dir(), PathBuf::from(r"C:\L\LILITH-test-abc"));
        assert_eq!(e.logs_dir(), PathBuf::from(r"C:\L\LILITH-test-abc\logs"));
        assert_eq!(
            e.claude_profiles_dir(),
            PathBuf::from(r"C:\L\LILITH-test-abc\claude-profiles")
        );
        assert_eq!(
            e.claude_default_profile_dir(),
            PathBuf::from(r"C:\U\.claude")
        );
        assert_eq!(e.registry_root(), r"HKCU\Software\LILITH-test-abc");
        assert_eq!(e.credential_prefix(), "LILITH-test-abc/");
    }

    #[test] // spec: ARCH-095
    fn pipe_name_format() {
        assert_eq!(
            env(Channel::Dev).pipe_name(),
            r"\\.\pipe\lilith-core-S-1-5-21-1-2-3-1001-dev"
        );
        assert_eq!(
            env(Channel::Test("x1".into())).pipe_name(),
            r"\\.\pipe\lilith-core-S-1-5-21-1-2-3-1001-test-x1"
        );
    }

    #[test] // spec: ARCH-095
    fn aumid_uses_capitalised_channel() {
        assert_eq!(
            env(Channel::Stable).notification_aumid(),
            "LILITH.Suite.Stable"
        );
        assert_eq!(env(Channel::Dev).notification_aumid(), "LILITH.Suite.Dev");
        assert_eq!(
            env(Channel::Test("abc".into())).notification_aumid(),
            "LILITH.Suite.Test-abc"
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

    /// A variable lookup over fixed pairs.
    fn vars<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_string())
        }
    }

    #[test] // spec: ARCH-095
    fn channel_parses_from_its_display_form() {
        assert_eq!("stable".parse::<Channel>().unwrap(), Channel::Stable);
        assert_eq!("dev".parse::<Channel>().unwrap(), Channel::Dev);
        assert_eq!(
            "test-ab_1".parse::<Channel>().unwrap(),
            Channel::Test("ab_1".into())
        );
        for bad in [
            "", "Stable", "test-", "test-a b", "test", "prod", "test-a/b",
        ] {
            assert!(bad.parse::<Channel>().is_err(), "{bad:?}");
        }
    }

    #[test] // spec: ARCH-095
    fn dev_build_defaults_to_dev() {
        let env = Env::from_vars("dev", &vars(&[])).unwrap();
        assert_eq!(env.channel(), &Channel::Dev);
        let env = Env::from_vars("dev", &vars(&[(ENV_CHANNEL, "dev")])).unwrap();
        assert_eq!(env.channel(), &Channel::Dev);
    }

    #[test] // spec: ARCH-095
    fn stable_build_refuses_other_channels() {
        let env = Env::from_vars("stable", &vars(&[])).unwrap();
        assert_eq!(env.channel(), &Channel::Stable);
        let env = Env::from_vars("stable", &vars(&[(ENV_CHANNEL, "stable")])).unwrap();
        assert_eq!(env.channel(), &Channel::Stable);
        for requested in ["dev", "test-ab", "staging", ""] {
            let got = Env::from_vars("stable", &vars(&[(ENV_CHANNEL, requested)]));
            assert!(
                matches!(
                    &got,
                    Err(EnvError::ChannelNotAllowed { build, requested: r })
                        if build == "stable" && r == requested
                ),
                "{requested:?}: {got:?}"
            );
        }
    }

    #[test] // spec: ARCH-095
    fn dev_build_refuses_unknown_channel() {
        for requested in ["stable", "staging", "Dev", ""] {
            let got = Env::from_vars("dev", &vars(&[(ENV_CHANNEL, requested)]));
            assert!(
                matches!(
                    &got,
                    Err(EnvError::ChannelNotAllowed { build, requested: r })
                        if build == "dev" && r == requested
                ),
                "{requested:?}: {got:?}"
            );
        }
        // A test channel whose id cannot go into names is an invalid channel, not "not allowed".
        let got = Env::from_vars("dev", &vars(&[(ENV_CHANNEL, "test-a b")]));
        assert!(matches!(got, Err(EnvError::InvalidChannel(_))), "{got:?}");
    }

    #[test] // spec: ARCH-095
    fn test_channel_requires_existing_test_root() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file.txt");
        std::fs::write(&file, b"x").unwrap();
        let missing = dir.path().join("missing");

        let no_root = Env::from_vars("dev", &vars(&[(ENV_CHANNEL, "test-ab")]));
        assert!(
            matches!(no_root, Err(EnvError::TestRootMissing)),
            "{no_root:?}"
        );

        for bad in [
            "relative\\root".to_string(),
            missing.to_string_lossy().into_owned(),
            file.to_string_lossy().into_owned(),
            String::new(),
        ] {
            let got = Env::from_vars(
                "dev",
                &vars(&[(ENV_CHANNEL, "test-ab"), (ENV_TEST_ROOT, &bad)]),
            );
            assert!(
                matches!(&got, Err(EnvError::TestRootInvalid(p)) if p == Path::new(&bad)),
                "{bad:?}: {got:?}"
            );
        }
    }

    #[test] // spec: ARCH-095
    fn test_overrides_only_on_test_channels() {
        let all = [
            (ENV_TEST_PARENT_PID, "4242"),
            (ENV_TEST_IDLE_SECS, "7"),
            (ENV_TEST_ROOT, r"C:\Ignored"),
        ];
        let test = Env::with_roots(Channel::Test("ab".into()), roots(), "S-1".into());
        assert_eq!(
            test.test_overrides(&vars(&all)),
            TestOverrides {
                parent_pid: Some(4242),
                idle_secs: Some(7)
            }
        );
        // Unset or unparsable values are None.
        assert_eq!(test.test_overrides(&vars(&[])), TestOverrides::default());
        assert_eq!(
            test.test_overrides(&vars(&[
                (ENV_TEST_PARENT_PID, "pid"),
                (ENV_TEST_IDLE_SECS, "-1")
            ])),
            TestOverrides::default()
        );
        for channel in [Channel::Stable, Channel::Dev] {
            let e = env(channel);
            assert_eq!(e.test_overrides(&vars(&all)), TestOverrides::default());
        }
    }

    #[test] // spec: ARCH-094
    fn install_dir_only_for_stable_and_data_never_inside_it() {
        let stable = env(Channel::Stable);
        let install = stable.install_dir().expect("stable has an install dir");
        assert_eq!(install, PathBuf::from(r"C:\L\LILITHSuite\current"));
        assert!(!stable.data_dir().starts_with(&install));
        assert!(!stable.local_dir().starts_with(&install));
        assert!(!stable.suite_toml().starts_with(&install));
        assert!(!stable.suite_state_db().starts_with(&install));
        assert_eq!(
            stable.suite_toml(),
            PathBuf::from(r"C:\R\LILITH\suite.toml")
        );
        assert_eq!(
            stable.suite_state_db(),
            PathBuf::from(r"C:\L\LILITH\state.db")
        );
        for channel in [Channel::Dev, Channel::Test("ab".into())] {
            let e = env(channel);
            assert_eq!(e.install_dir(), None);
            assert!(e.suite_toml().ends_with("suite.toml"));
            assert!(e.suite_state_db().ends_with("state.db"));
        }
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
            PathBuf::from(format!(r"C:\L\LILITH\workspaces\{h}"))
        );
    }

    #[test] // spec: ARCH-097
    fn other_volume_goes_to_lilith_local() {
        let e = env(Channel::Stable);
        let ws = Path::new(r"D:\x");
        let h = Env::workspace_hash(ws);
        assert_eq!(
            e.workspace_local_dir_for_canonical(ws).unwrap(),
            PathBuf::from(format!(r"D:\.lilith-local\{h}"))
        );
    }

    #[test] // spec: ARCH-097
    fn verbatim_workspace_path_is_resolved_without_prefix() {
        let e = env(Channel::Stable);
        let ws = Path::new(r"\\?\D:\x");
        let h = Env::workspace_hash(ws);
        assert_eq!(
            e.workspace_local_dir_for_canonical(ws).unwrap(),
            PathBuf::from(format!(r"D:\.lilith-local\{h}"))
        );
    }

    /// The legacy data folder (R4) is read only for `suite.toml`.
    #[test] // spec: ARCH-097
    fn legacy_data_dir_is_the_old_layout() {
        use crate::names::LEGACY_APP_NAME;
        let e = env(Channel::Dev);
        assert_eq!(
            e.legacy_data_dir(),
            PathBuf::from(format!(r"C:\R\{LEGACY_APP_NAME}-dev"))
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
        let want = PathBuf::from(r"C:\L\LILITH\workspaces").join(Env::workspace_hash(&canonical));
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

    #[test] // spec: ARCH-081
    fn machine_id_is_lowercase_and_restricted() {
        assert_eq!(machine_id_from("DESK_01.Ünï"), "desk-01--n-");
        assert_eq!(machine_id_from("Build-Box-7"), "build-box-7");
        for name in ["DESK_01.Ünï", r"a b\c", "Ünïcödé space", "😀x", "A-Z_0.9"] {
            let id = machine_id_from(name);
            assert_eq!(id.chars().count(), name.chars().count(), "{name:?}");
            assert!(
                id.bytes()
                    .all(|c| matches!(c, b'a'..=b'z' | b'0'..=b'9' | b'-')),
                "{name:?} -> {id:?}"
            );
        }
        // `Env::machine_id` is the pure function over the real name.
        let real = std::env::var("COMPUTERNAME").unwrap_or_default();
        let want = if real.is_empty() {
            "unknown".to_string()
        } else {
            machine_id_from(&real)
        };
        assert_eq!(env(Channel::Dev).machine_id(), want);
    }

    #[test] // spec: ARCH-081
    fn test_channel_machine_id_is_testmachine() {
        assert_eq!(env(Channel::Test("ab".into())).machine_id(), "testmachine");
    }

    /// A `suite.toml` whose existence can't be checked is never taken for a missing one: the
    /// read must report the error, or a write would start from an empty text and drop the
    /// legacy file's settings (its caps among them).
    #[test] // spec: CORE-007, ARCH-090
    fn suite_toml_choice_treats_an_unchecked_file_as_present() {
        let (new, legacy) = (
            PathBuf::from(r"C:\R\new.toml"),
            PathBuf::from(r"C:\R\old.toml"),
        );
        let denied = || io::Error::from_raw_os_error(5);
        let pick = |n: io::Result<bool>, l: Option<io::Result<bool>>| {
            choose_suite_toml(new.clone(), n, legacy.clone(), |_| {
                l.expect("the legacy file is looked at only when the new one is missing")
            })
        };
        assert_eq!(pick(Ok(true), None), new);
        assert_eq!(pick(Err(denied()), None), new);
        assert_eq!(pick(Ok(false), Some(Ok(true))), legacy);
        assert_eq!(pick(Ok(false), Some(Err(denied()))), legacy);
        assert_eq!(pick(Ok(false), Some(Ok(false))), new);
    }

    #[test]
    #[should_panic(expected = "invalid channel")]
    fn with_roots_panics_on_invalid_channel() {
        let _ = Env::with_roots(Channel::Test("a/b".into()), roots(), "S-1".into());
    }
}
