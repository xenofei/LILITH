//! Machine-specific values behind one provider (ARCH-095, ARCH-097, QA-038).
//!
//! `ww-platform` is the only crate allowed to call Windows APIs (ARCH-111). Every other crate
//! asks [`Env`] for data folders, per-workspace local data, the registry root, the credential
//! prefix, the core pipe name, the notification identity and the Claude Code profile folders.

pub mod credentials;
pub mod env;
pub mod guard;
pub mod known_folders;
pub mod sid;
pub mod volume;

pub use credentials::{
    Credential, CredentialError, CredentialStore, MemoryCredentialStore, WindowsCredentialStore,
};
pub use env::{Channel, Env, EnvError, Roots};
pub use guard::{RealLocations, arm_test_guard};
pub use volume::{SystemVolumeResolver, VolumeResolver, volume_root};
