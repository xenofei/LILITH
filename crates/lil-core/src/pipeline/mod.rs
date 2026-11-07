//! The `pipeline` module (CORE-014): plain functions over [`Store`](crate::store::Store) and
//! [`Index`](crate::index::Index) that take no `tools` types (CORE-010), so the tools and the
//! live-topic snapshot providers in `tools/` call them without a module cycle. Only the
//! direction `tools` → `pipeline` exists.
//!
//! - [`detail`]: `asset.get`'s [`AssetDetail`] and `assets.query`'s [`AssetSummary`];
//! - [`record`]: the Record stage's [`RecordPlan`] and [`apply_record`], and
//!   [`start_candidate_run`];
//! - [`signoff`]: [`plan_signoff`] (with [`Chosen`] and [`chosen_files`]), [`signed_off_by`]
//!   and [`auto_pick`];
//! - [`status`]: [`status_by`], the status log's `by` for a caller.

pub mod detail;
pub mod record;
pub mod signoff;
pub mod status;

pub use detail::{
    AssetDetail, AssetSummary, DisplayEntry, FileEntry, LastChange, NextVersions, UseEntry,
    UseError, UsedByEntry, VersionEntry, asset_detail, asset_summary, file_state_wire,
};
pub use record::{
    HandOff, RecordBranch, RecordOutcome, RecordPlan, RecordSlot, apply_record, asset_topic,
    start_candidate_run,
};
pub use signoff::{Chosen, ChosenFiles, auto_pick, chosen_files, plan_signoff, signed_off_by};
pub use status::status_by;
