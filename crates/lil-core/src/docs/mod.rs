//! The `docs` module (CORE-014), holds only: documents themselves are plan 5.
//!
//! - `holds`: the first app to open a document holds it (APP-103), in the core's memory and
//!   tied to the app's connection (APP-104); the core takes its own short hold for each
//!   operation on a closed document (APP-105);
//! - `tools`: `doc.list_open`.

pub mod holds;
pub mod tools;

pub use holds::{HoldGuard, HoldOwner, HoldView, Holds};
pub use tools::register_doc_tools;

/// The `module` of this module's tools (CORE-058).
pub const DOCS_MODULE: &str = "docs";
