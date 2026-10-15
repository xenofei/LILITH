//! Workspace-format types (lossless round-trip; `docs/reference/workspace-format.md`), link
//! syntax and `ww://` refs, and schema generation (ARCH-110).
//!
//! Platform-neutral: this crate has no Qt or Windows dependencies (ARCH-111, QA-027).

pub mod suite;

pub use suite::{SuiteToml, WorkspaceEntry};
