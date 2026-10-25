//! Workspace-format types (lossless round-trip; `docs/reference/workspace-format.md`), link
//! syntax and `lil://` refs, and schema generation (ARCH-110).
//!
//! Platform-neutral: this crate has no Qt or Windows dependencies (ARCH-111, QA-027).
//!
//! - [`manifest`]: asset manifests as an ordered map with typed views (ARCH-076, CORE-148);
//! - [`refs`], [`version`], [`content_hash`]: links, `lil://` refs and content hashes (ARCH-042,
//!   CORE-031, CORE-047);
//! - [`lilref`]: `.lilref` pointer files (APP-021, CORE-047);
//! - [`status`]: the status machine (CORE-048);
//! - [`paths`]: workspace-relative path rules (CORE-040);
//! - [`yaml_edit`], [`workspace_yaml`], [`suite_edit`]: comment-preserving YAML and TOML edits;
//! - [`logs`]: append-log lines (ARCH-081); [`time`]: UTC dates;
//! - [`money`]: integer micro-unit amounts and currencies (CORE-013);
//! - [`engine_files`]: `registry/models.yaml` and a node pack's `pyproject.toml` version.

pub mod content_hash;
pub mod engine_files;
pub mod lilref;
pub mod logs;
pub mod manifest;
pub mod money;
pub mod pass_env;
pub mod paths;
pub mod refs;
pub mod status;
pub mod suite;
pub mod suite_edit;
pub mod time;
pub mod version;
pub mod workspace_yaml;
pub mod yaml_edit;

pub use content_hash::{is_os_metadata, sha256_hex, version_content_hash};
pub use lilref::{LilRef, parse_lilref, write_lilref};
pub use manifest::{
    CandidateFile, CandidateRun, EditError, ExternalFile, HumanEdit, Input, Manifest, ManifestView,
    PinnedInput, SchemaWarning, SignedOff, bare_sha256, hash_value, remove_key,
};
pub use money::{Currency, Money, Rate};
pub use paths::{PATH_BUDGET, PathProblem, check_rel_path};
pub use refs::{AssetRef, LEGACY_SCHEME, LinkRef, SCHEME, is_asset_id, is_run_id, is_slug};
pub use status::{Status, StatusEvent, Transition, transition};
pub use suite::{
    DesktopToml, NOTIFICATION_CATEGORIES, PowerToml, SUITE_STATE_SCHEMA, SuiteToml,
    WORKSPACE_STATE_SCHEMA, WorkspaceEntry,
};
pub use version::{Bump, Version};
pub use yaml_edit::{YamlEdit, YamlEditError, YamlValue};
