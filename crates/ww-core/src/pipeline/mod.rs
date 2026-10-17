//! The `pipeline` module (CORE-014): plain functions over [`Store`](crate::store::Store) and
//! [`Index`](crate::index::Index) that take no `tools` types (CORE-010), so the tools and the
//! live-topic snapshot providers in `tools/` call them without a module cycle. Only the
//! direction `tools` → `pipeline` exists.
//!
//! - [`detail`]: `asset.get`'s [`AssetDetail`] and `assets.query`'s [`AssetSummary`].

pub mod detail;

pub use detail::{
    AssetDetail, AssetSummary, FileEntry, NextVersions, VersionEntry, asset_detail, asset_summary,
    file_state_wire,
};
