//! The store: the only module in `ww-core` allowed to write files (ARCH-112, QA-024), and the
//! workspace write path (ARCH-002, ARCH-074, ARCH-075, ARCH-078, ARCH-080, ARCH-081; CORE-014).
//!
//! - [`write`]: [`Store`], the hash-checked atomic write, edits, manifest `locks`, recovery and
//!   errors;
//! - [`order`]: [`Store::commit`] in the ARCH-075 order, the orphan list and run ids;
//! - [`versions`]: the per-asset version lock, sealed read-only versions and legacy folders;
//! - [`trash`]: trash entries with restore records, the 30-day purge and rejected-run
//!   retention;
//! - [`logs`]: the locked per-machine monthly append logs;
//! - [`staging`]: staging handles;
//! - [`suite_toml`]: `suite.toml` writes (the core is the file's only writer, CORE-007);
//! - [`paths`]: workspace-relative paths and the paths the store never writes;
//! - [`failpoints`]: the test-channel fail points (QA-066);
//! - [`fsops`]: every file-mutating call of the crate, one
//!   `#[allow(clippy::disallowed_methods)]` per function (enforced by
//!   `crates/ww-testkit/tests/single_write_allowance.rs`). SQLite creating and writing its own
//!   database files, and the log appender writing the core's log, are not `ww-core` file calls;
//!   the folders for them are created through [`fsops::ensure_dir`].

pub mod failpoints;
pub mod fsops;
pub mod logs;
pub mod order;
pub mod paths;
pub mod staging;
pub mod suite_toml;
pub mod trash;
pub mod versions;
pub mod write;

pub use failpoints::{ENV_FAILPOINTS, FailAction, FailPoints, armed};
pub use fsops::{ensure_dir, write_file_atomic};
pub use logs::{LogKind, append_loose_usage};
pub use order::{CommitOp, CommitReport, ManifestEdit, OutputMove, TextEdit};
pub use paths::{WsPath, path_key, writable};
pub use staging::{STAGING_DIR, StagingHandle};
pub use suite_toml::register_workspace;
pub use trash::{MaintenanceReport, REJECTED_KEEP_DAYS, TRASH_DIR, TRASH_KEEP_DAYS, TrashRecord};
pub use versions::{SNAPSHOT, SealRequest, Sealed, VersionDir};
pub use write::{
    ChangeKind, ChangeSink, Expect, Phase, RecoveryReport, Step, Store, StoreError, StoreEvent,
    Written, lfs_pointer_oid, next_step,
};
