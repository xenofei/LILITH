//! Worldwright core. All file writes in this crate go through [`store`] (ARCH-112); the core's
//! SQLite databases are [`state`] (CORE-004, CORE-005).
//!
//! `config`, `server`, `rpc`, `identity`, `workspace` and `session` are infrastructure, not
//! CORE-014 modules.

pub mod config;
pub mod identity;
pub mod rpc;
pub mod server;
pub mod session;
pub mod state;
pub mod store;
pub mod workspace;

pub use config::{CoreConfig, CoreExit};
pub use server::{
    ConnCtx, CoreThread, Listener, OUTBOUND_CAPACITY, Router, RunningCore, ServerStats, claim_pipe,
    serve, start, start_claimed, start_in_thread,
};
