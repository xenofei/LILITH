//! The suite's names in one place: the new names, and every legacy name the code still reads
//! for one release (R4). PR-D removes the `LEGACY_*` constants.
//!
//! Code that needs a legacy name uses one of these constants, so the legacy spellings appear
//! only here and on the lines that use them. The one exception is the reference URI scheme,
//! which lives in `ww-schema` (it depends on no other crate, QA-027).

/// The product name, used for the data folders, the registry root and the credential prefixes.
pub const APP_NAME: &str = "LILITH";
/// The product name before the rename.
pub const LEGACY_APP_NAME: &str = "Worldwright";

/// The home folder below `%USERPROFILE%` (owner tooling: reviewer key, privacy denylist).
pub const HOME_DIR: &str = ".lilith";
/// The home folder before the rename.
pub const LEGACY_HOME_DIR: &str = ".worldwright";

/// The per-volume folder for workspace-local data when the workspace is not on the system
/// volume.
pub const LOCAL_DIR: &str = ".lilith-local";
/// The per-volume local-data folder before the rename.
pub const LEGACY_LOCAL_DIR: &str = ".worldwright-local";

/// The owner-tooling credential prefix (the agent token): every use.
pub const DEV_CREDENTIAL_PREFIX: &str = "LILITH-dev/";
/// The owner-tooling credential prefix before the rename: read and delete only.
pub const LEGACY_DEV_CREDENTIAL_PREFIX: &str = "Worldwright-dev/";

/// The environment variable that holds the privacy denylist (CI).
pub const DENYLIST_ENV: &str = "LIL_PRIVACY_DENYLIST";
/// The denylist variable before the rename.
pub const LEGACY_DENYLIST_ENV: &str = "WW_PRIVACY_DENYLIST";
