//! The asset services the core's tools reach through traits (P4b-R32; CORE-010, CORE-014):
//! `interchange` and `docs` stay leaves that only `server` names, so `tools` defines what it
//! needs and `server` wires it, as it does the holds and the engines.
//!
//! - [`AssetUse`]: whether a job, a sign-off (the version lock) or a document hold uses an
//!   asset, for `asset.trash`'s refusal `asset.in_use` (CORE-157). `server` implements it over
//!   the jobs, the store's version locks and the holds.
//! - [`Thumbnails`]: the thumbnail cache (APP-038), for `app/thumbs.request` and the pick log's
//!   copy. `interchange::thumbs` implements it.
//! - [`ResourceIds`]: a restored resource's new id written inside its file (CORE-037), for
//!   `trash.restore {to}`; `tools` never names `resources`, so `server` wires it from there.
//!
//! [`AssetServices`] holds them as **late-bound slots**: `core_registry` builds the tools before
//! the dispatcher, while the jobs and the engines exist only after it, so `server` makes the
//! slots, passes them to `core_registry`, and fills each once what it needs exists. Tests pass
//! [`AssetServices::unwired`]. An empty `asset_use` fails closed (`asset.trash` refuses with
//! `state.unavailable`, never "not in use"); an empty `thumbs` makes the pick log skip its
//! thumbnail copy; an empty `resource_ids` refuses a resource's `trash.restore {to}` with
//! `state.unavailable` (its id would stay the old one inside the file).

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use futures_util::future::BoxFuture;
use lil_protocol::ErrorBody;
use lil_protocol::app::ThumbItem;

use crate::store::WsPath;
use crate::workspace::OpenWorkspace;

/// What uses an asset now (CORE-157).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InUse {
    /// An unfinished job names it.
    Job,
    /// A sign-off holds its version lock.
    Signoff,
    /// A document hold names a path inside it.
    Hold,
}

/// Whether an asset is in use (P4b-R32).
pub trait AssetUse: Send + Sync {
    /// What uses `project`/`asset_id` of `workspace` now, or `None`. The workspace is the
    /// call's: its jobs, its store's version locks and the holds on its paths are the ones that
    /// count.
    ///
    /// # Errors
    /// It couldn't be told (a database failed): the caller refuses, failing closed.
    fn in_use<'a>(
        &'a self,
        workspace: &'a OpenWorkspace,
        project: &'a str,
        asset_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<InUse>, ErrorBody>>;
}

/// A restored resource file's new id (CORE-037): `trash.restore {to}` of a resource file.
pub trait ResourceIds: Send + Sync {
    /// The resource file `bytes` with its `id` set to `new_id` by a targeted edit (only that
    /// value changes).
    ///
    /// # Errors
    /// Why it can't: `new_id` isn't a resource id, or `bytes` isn't a resource file.
    fn with_id(&self, bytes: &[u8], new_id: &str) -> Result<Vec<u8>, String>;
}

/// One thumbnail's state (Appendix H "Thumbnails"): `state` is `ready`, `pending` or `icon`;
/// `file` is the cached PNG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbState {
    pub path: String,
    pub sha256: Option<String>,
    pub media_kind: String,
    pub renderer: String,
    pub state: String,
    pub file: Option<PathBuf>,
    pub px: u32,
}

/// The thumbnail cache (APP-038; P4b-R12).
pub trait Thumbnails: Send + Sync {
    /// The states of `items` at `px`, rendering what is missing in the background.
    fn request<'a>(&'a self, items: &'a [ThumbItem], px: u32) -> BoxFuture<'a, Vec<ThumbState>>;
    /// `path`'s thumbnail at `px`, rendered now if it is missing; `expected_sha256` is the hash
    /// the caller knows for the bytes.
    fn ensure<'a>(
        &'a self,
        path: &'a WsPath,
        expected_sha256: Option<&'a str>,
        px: u32,
    ) -> BoxFuture<'a, ThumbState>;
}

/// The late-bound asset services (module docs). Clones share the slots.
#[derive(Clone, Default)]
pub struct AssetServices {
    pub asset_use: Arc<OnceLock<Arc<dyn AssetUse>>>,
    pub thumbs: Arc<OnceLock<Arc<dyn Thumbnails>>>,
    pub resource_ids: Arc<OnceLock<Arc<dyn ResourceIds>>>,
}

impl AssetServices {
    /// Empty slots: what `server` starts from, and what tests pass.
    pub fn unwired() -> AssetServices {
        AssetServices::default()
    }
}

impl std::fmt::Debug for AssetServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssetServices")
            .field("asset_use", &self.asset_use.get().is_some())
            .field("thumbs", &self.thumbs.get().is_some())
            .field("resource_ids", &self.resource_ids.get().is_some())
            .finish()
    }
}
