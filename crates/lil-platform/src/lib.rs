//! Machine-specific values behind one provider (ARCH-095, ARCH-097, QA-038).
//!
//! `lil-platform` is the only crate allowed to call Windows APIs (ARCH-111). Every other crate
//! asks [`Env`] for data folders, per-workspace local data, the registry root, the credential
//! prefix, the core pipe name, the notification identity and the Claude Code profile folders.
//! It also owns the process environment variables, the clock and id traits, the harness
//! guard, the file layer (`fs`, `paths`) every file write goes through, the hardware and
//! engine-process layer (`hw`, `job_object`, `engine_spawn`), the desktop (`desktop`: toasts,
//! the AUMID shortcut, the handlers, fullscreen, the foreground and the power request; and its
//! fake, `desktop_fake`) and the shared log setup (`logs`).

mod classes;
pub mod clock;
pub mod credentials;
pub mod desktop;
pub mod desktop_fake;
pub mod engine_spawn;
pub mod env;
pub mod fs;
pub mod guard;
pub mod hw;
pub mod hw_fake;
mod hw_nvml;
mod hw_pdh;
pub mod ids;
pub mod job_object;
pub mod known_folders;
pub mod logs;
pub mod names;
pub mod paths;
pub mod pipe;
mod power;
pub mod process;
pub mod process_env;
pub mod session_end;
mod shortcut;
pub mod sid;
pub mod sockets;
pub mod sqlite_files;
mod toast;
pub mod volume;
pub mod watcher;

pub use clock::{Clock, SystemClock};
pub use credentials::{
    Credential, CredentialError, CredentialStore, MemoryCredentialStore, WindowsCredentialStore,
};
pub use env::{Channel, Env, EnvError, Roots, TEST_MACHINE_ID, machine_id_from};
pub use fs::{FileIdentity, Probe, SystemProbe, VolumeInfo};
pub use guard::{RealLocations, arm_test_guard, check_workspace, parse_registered_workspaces};
pub use ids::{IdSource, RandomIds};
pub use process_env::{
    BUILD_CHANNEL, ENV_CHANNEL, ENV_TEST_DOCTOR_STDIN, ENV_TEST_IDLE_SECS, ENV_TEST_PARENT_PID,
    ENV_TEST_ROOT, TEST_PIPE_SID, TestOverrides,
};
pub use volume::{SystemVolumeResolver, VolumeResolver, volume_root};
