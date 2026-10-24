//! ComfyUI (ARCH-064, ARCH-065, ARCH-068, ARCH-069): the [`supervisor`] of an owned or
//! attached engine, its HTTP [`client`] and websocket ([`ws`]), the `comfy.prompt` jobs
//! ([`handler`]) with their graph checks ([`graph`]), uploads ([`uploads`]) and staged outputs
//! ([`salvage`]), node installs through comfy-cli ([`install`]), outside jobs ([`outside`]), and
//! the model inventory with the environment capture ([`inventory`], [`environment`]).

pub mod client;
pub mod environment;
pub mod graph;
pub mod handler;
pub mod install;
pub mod inventory;
pub mod outside;
pub mod salvage;
pub mod supervisor;
pub mod uploads;
pub mod ws;

pub use client::{
    ComfyClient, ComfyError, OutputFile, QueueEntry, QueueView, RemoteJob, RemoteState,
};
pub use environment::EnvCapture;
pub use graph::{MODEL_EXTENSIONS, ModelRef, apply_node_replacements, model_refs, partner_nodes};
pub use handler::{
    ComfyPromptHandler, DEFAULT_EXPECTED_OUTPUT_BYTES, DEFAULT_EXPECTED_SECS,
    DEFAULT_PREVIEW_METHOD, OOM_RETRIES, PROMPT_KIND, PromptArgs, PromptInput, comfy_estimate_key,
    confine_input, is_oom, prefix_outputs, prompt_spec, rejection_detail, safe_name,
};
pub use install::{INSTALL_KIND, NodeInstallHandler};
pub use inventory::{INVENTORY_KIND, InventoryHandler};
pub use outside::{OutsideWatch, POLL_EVERY};
pub use supervisor::ComfyEndpoint;
pub use uploads::{UPLOAD_KEEP, Uploads, upload_name};
pub use ws::{ComfyEvent, ComfySocket, MAX_PREVIEW_BYTES, PreviewFormat};
