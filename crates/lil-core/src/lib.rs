//! LILITH core. All file writes in this crate go through [`store`] (ARCH-112); the core's
//! SQLite databases are [`state`] (CORE-004, CORE-005).
//!
//! `config`, `server`, `transport`, `rpc`, `identity`, `workspace`, `session`, `events`,
//! `lifecycle` and `desktop` (P4a-R24) are infrastructure, not CORE-014 modules. The registry and the dispatcher are the [`tools`]
//! module.
//!
//! Lint policy: `clippy::result_large_err` is allowed crate-wide. The plan fixes many
//! signatures that return `Result<_, ErrorBody>`; `ErrorBody` is the wire error result, built
//! at most once per call, so boxing it would only add noise.
#![allow(
    clippy::result_large_err,
    reason = "ErrorBody is the wire error result the plan's signatures return; see the crate docs"
)]

pub mod config;
pub mod confirm;
pub mod desktop;
pub mod docs;
pub mod engines;
pub mod events;
pub mod graph;
pub mod identity;
pub mod index;
pub mod interchange;
pub mod jobs;
pub mod lifecycle;
pub mod meter;
pub mod pipeline;
pub mod projects;
pub mod resources;
pub mod rpc;
pub mod server;
pub mod session;
pub mod settings;
pub mod state;
pub mod store;
pub mod telemetry;
pub mod tools;
pub mod transport;
pub mod watch;
pub mod workspace;

pub use config::{CoreConfig, CoreExit};
pub use lifecycle::{DrainReason, Lifecycle, ShutdownReport};
pub use server::{
    ConnCtx, CoreThread, Listener, MAX_IN_FLIGHT_PER_CONN, OUTBOUND_CAPACITY, Router, RunningCore,
    ServerStats, claim_pipe, serve, start, start_claimed, start_in_thread,
};
