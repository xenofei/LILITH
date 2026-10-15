//! The store: the only module in `ww-core` allowed to write files (ARCH-112, QA-024).
//!
//! Every file-mutating call of the crate lives in [`fsops`], one
//! `#[allow(clippy::disallowed_methods)]` per function (enforced by
//! `crates/ww-testkit/tests/single_write_allowance.rs`). SQLite creating and writing its own
//! database files, and the log appender writing the core's log, are not `ww-core` file calls;
//! the folders for them are created through [`fsops::ensure_dir`].

pub mod fsops;

pub use fsops::{ensure_dir, write_file_atomic};
