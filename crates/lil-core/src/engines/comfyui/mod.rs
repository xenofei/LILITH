//! ComfyUI (ARCH-064, ARCH-065, ARCH-068): the [`supervisor`] of an owned or attached engine,
//! its HTTP [`client`], and node installs through comfy-cli ([`install`]).

pub mod client;
pub mod install;
pub mod supervisor;

pub use client::{ComfyClient, ComfyError};
pub use install::{INSTALL_KIND, NodeInstallHandler};
pub use supervisor::ComfyEndpoint;
