//! ComfyUI (ARCH-064, ARCH-065, ARCH-068): the [`supervisor`] of an owned or attached engine,
//! its HTTP [`client`] and websocket ([`ws`]), the `comfy.prompt` jobs ([`handler`]) with their
//! uploads ([`uploads`]) and staged outputs ([`salvage`]), and node installs through comfy-cli
//! ([`install`]).

pub mod client;
pub mod handler;
pub mod install;
pub mod salvage;
pub mod supervisor;
pub mod uploads;
pub mod ws;

pub use client::{
    ComfyClient, ComfyError, OutputFile, QueueEntry, QueueView, RemoteJob, RemoteState,
};
pub use handler::{
    ComfyPromptHandler, DEFAULT_EXPECTED_OUTPUT_BYTES, DEFAULT_EXPECTED_SECS,
    DEFAULT_PREVIEW_METHOD, OOM_RETRIES, PROMPT_KIND, PromptArgs, PromptInput, confine_input,
    is_oom, prefix_outputs, prompt_spec, rejection_detail, safe_name,
};
pub use install::{INSTALL_KIND, NodeInstallHandler};
pub use supervisor::ComfyEndpoint;
pub use uploads::{UPLOAD_KEEP, Uploads, upload_name};
pub use ws::{ComfyEvent, ComfySocket, MAX_PREVIEW_BYTES, PreviewFormat};
