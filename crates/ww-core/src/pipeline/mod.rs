//! The `pipeline` module (CORE-014): plain functions over [`Store`](crate::store::Store) and
//! [`Index`](crate::index::Index) that take no `tools` types (CORE-010), so the tools and the
//! live-topic snapshot providers in `tools/` call them without a module cycle. Only the
//! direction `tools` → `pipeline` exists.
//!
//! - [`detail`]: `asset.get`'s [`AssetDetail`] and `assets.query`'s [`AssetSummary`];
//! - [`record`]: the Record stage's [`RecordPlan`] and [`apply_record`], and
//!   [`start_candidate_run`];
//! - [`signoff`]: [`plan_signoff`], [`signed_off_by`] and [`auto_pick`].

pub mod detail;
pub mod record;
pub mod signoff;

pub use detail::{
    AssetDetail, AssetSummary, FileEntry, NextVersions, VersionEntry, asset_detail, asset_summary,
    file_state_wire,
};
pub use record::{
    HandOff, RecordBranch, RecordOutcome, RecordPlan, RecordSlot, apply_record, asset_topic,
    start_candidate_run,
};
pub use signoff::{auto_pick, plan_signoff, reopened, signed_off_by};
