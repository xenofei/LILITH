//! RPC methods, events, result and error types, effect classes
//! (`read | write | destructive | exec`), the protocol version, and tool descriptors and
//! lowering (ARCH-110). [`lowering`] is the MCP lowering (ARCH-039).
//!
//! Platform-neutral: this crate has no Qt or Windows dependencies (ARCH-111, QA-027).

pub mod app;
pub mod canonical;
pub mod codes;
pub mod estimate;
pub mod events;
pub mod jsonrpc;
pub mod lowering;
pub mod outcome;
pub mod session;
pub mod tools;

pub use canonical::{args_hash, canonical_json};
pub use codes::CodeSpec;
pub use events::*;
pub use jsonrpc::*;
pub use outcome::*;
pub use session::*;
pub use tools::*;
