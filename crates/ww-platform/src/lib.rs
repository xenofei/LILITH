//! Machine-specific values behind one provider (ARCH-095, ARCH-097, QA-038).
//!
//! `ww-platform` is the only crate allowed to call Windows APIs (ARCH-111). Every other crate
//! asks [`Env`] for data folders, per-workspace local data, the registry root, the credential
//! prefix, the core pipe name, the notification identity and the Claude Code profile folders.
//! It also owns the process environment variables, the clock and id traits, and the harness
//! guard.

pub mod clock;
pub mod credentials;
pub mod env;
pub mod guard;
pub mod ids;
pub mod known_folders;
pub mod pipe;
pub mod process;
pub mod process_env;
pub mod session_end;
pub mod sid;
pub mod sockets;
pub mod volume;

pub use clock::{Clock, SystemClock};
pub use credentials::{
    Credential, CredentialError, CredentialStore, MemoryCredentialStore, WindowsCredentialStore,
};
pub use env::{Channel, Env, EnvError, Roots};
pub use guard::{RealLocations, arm_test_guard, check_workspace, parse_registered_workspaces};
pub use ids::{IdSource, RandomIds};
pub use process_env::{
    BUILD_CHANNEL, ENV_CHANNEL, ENV_TEST_IDLE_SECS, ENV_TEST_PARENT_PID, ENV_TEST_ROOT,
    TEST_PIPE_SID, TestOverrides,
};
pub use volume::{SystemVolumeResolver, VolumeResolver, volume_root};
