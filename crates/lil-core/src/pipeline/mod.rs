//! The `pipeline` module (CORE-014): plain functions over [`Store`](crate::store::Store) and
//! [`Index`](crate::index::Index) that take no `tools` types (CORE-010), so the tools and the
//! live-topic snapshot providers in `tools/` call them without a module cycle. Only the
//! direction `tools` → `pipeline` exists.
//!
//! - [`detail`]: `asset.get`'s [`AssetDetail`] and `assets.query`'s [`AssetSummary`];
//! - [`record`]: the Record stage's [`RecordPlan`] and [`apply_record`], and
//!   [`start_candidate_run`];
//! - [`signoff`]: [`plan_signoff`] and [`plan_signoff_with`] (with [`Chosen`],
//!   [`chosen_files`] and [`SignoffRecord`]), [`signed_off_by`], [`auto_pick`], and a restore's
//!   run ([`restored_run`], [`add_restored_run`]);
//! - [`status`]: [`status_by`], the status log's `by` for a caller, and [`actor_of`], the
//!   `actor` of a caller's work;
//! - [`collections`]: `registry/collections.yaml`'s saved searches and collections (CORE-155).

pub mod collections;
pub mod detail;
pub mod record;
pub mod signoff;
pub mod status;

pub use collections::QUICK_COLLECTION_ID;
pub use detail::{
    AssetDetail, AssetSummary, DisplayEntry, FileEntry, LastChange, NextVersions, UseEntry,
    UseError, UsedByEntry, VersionEntry, asset_detail, asset_summary, file_state_wire,
};
pub use record::{
    HandOff, RecordBranch, RecordOutcome, RecordPlan, RecordSlot, apply_record, asset_topic,
    merge_warnings, start_candidate_run,
};
pub use signoff::{
    Chosen, ChosenFiles, RestoredRun, SignedOffBy, SignoffRecord, add_restored_run, auto_pick,
    chosen_files, plan_signoff, plan_signoff_with, restored_run, signed_off_by,
};
pub use status::{actor_of, status_by};
