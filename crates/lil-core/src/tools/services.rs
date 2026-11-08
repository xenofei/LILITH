//! The asset services the core's tools reach through traits (P4b-R32; CORE-010, CORE-014):
//! `interchange` and `docs` stay leaves that only `server` names, so `tools` defines what it
//! needs and `server` wires it, as it does the holds and the engines.
//!
//! - [`AssetUse`]: whether a job, a sign-off (the version lock) or a document hold uses an
//!   asset, for `asset.trash`'s refusal `asset.in_use` (CORE-157). `server` implements it over
//!   the jobs, the store's version locks and the holds.
//! - [`Thumbnails`]: the thumbnail cache (APP-038), for `app/thumbs.request` and the pick log's
//!   copy. `interchange::thumbs` implements it. [`tier_px`] and [`thumbs_request_px`] are the
//!   size rules of `app/thumbs.request` (Appendix H "Thumbnails"), here so that `rpc` (which
//!   checks the request) and `interchange` (which renders) share them without naming each
//!   other.
//!
//! [`AssetServices`] holds them as **late-bound slots**: `core_registry` builds the tools before
//! the dispatcher, while the jobs and the engines exist only after it, so `server` makes the
//! slots, passes them to `core_registry`, and fills each once what it needs exists. Tests pass
//! [`AssetServices::unwired`]. An empty `asset_use` fails closed (`asset.trash` refuses with
//! `state.unavailable`, never "not in use"); an empty `thumbs` makes the pick log skip its
//! thumbnail copy.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use futures_util::future::BoxFuture;
use lil_protocol::ErrorBody;
use lil_protocol::app::{ThumbItem, ThumbsRequestParams};

use crate::store::WsPath;

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
    /// What uses `project`/`asset_id` now, or `None`.
    ///
    /// # Errors
    /// It couldn't be told (a database failed): the caller refuses, failing closed.
    fn in_use<'a>(
        &'a self,
        project: &'a str,
        asset_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<InUse>, ErrorBody>>;
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

/// The fixed thumbnail tiers in px at scale 1 (APP-038).
pub const THUMB_TIERS: [u32; 5] = [96, 160, 256, 384, 512];
/// The largest `round(tier × scale)` (Appendix H "Thumbnails").
pub const THUMB_MAX_PX: u32 = 2048;
/// The screen scales `app/thumbs.request` takes.
pub const THUMB_SCALES: std::ops::RangeInclusive<f64> = 1.0..=4.0;
/// The sizes `large_px` takes instead of a tier.
pub const THUMB_LARGE_PX: std::ops::RangeInclusive<u32> = 513..=4096;
/// How many items one `app/thumbs.request` takes at most.
pub const THUMB_MAX_ITEMS: usize = 500;

/// A tier's size on a screen of `scale`: `round(tier × scale)`, at most [`THUMB_MAX_PX`].
///
/// # Errors
/// Why `tier` isn't one of [`THUMB_TIERS`] or `scale` isn't in [`THUMB_SCALES`].
pub fn tier_px(tier: u32, scale: f64) -> Result<u32, String> {
    if !THUMB_TIERS.contains(&tier) {
        return Err(format!(
            "tier {tier} is not one of 96, 160, 256, 384 and 512"
        ));
    }
    if !THUMB_SCALES.contains(&scale) {
        return Err(format!("scale {scale} is not between 1.0 and 4.0"));
    }
    // A finite value between 96 and 512 × 4.0 = 2048, so the cast loses nothing.
    let px = (f64::from(tier) * scale).round() as u32;
    Ok(px.min(THUMB_MAX_PX))
}

/// The size `app/thumbs.request {items, tier, scale?, large_px?}` asks for: [`tier_px`] of its
/// `tier` and `scale` (default 1), or its `large_px`, which comes instead of `tier`.
///
/// # Errors
/// Why the request isn't one: no item or more than [`THUMB_MAX_ITEMS`], neither or both of
/// `tier` and `large_px`, a `large_px` outside [`THUMB_LARGE_PX`], or [`tier_px`]'s.
pub fn thumbs_request_px(p: &ThumbsRequestParams) -> Result<u32, String> {
    if p.items.is_empty() || p.items.len() > THUMB_MAX_ITEMS {
        return Err(format!(
            "items holds {} entries; it takes 1 to {THUMB_MAX_ITEMS}",
            p.items.len()
        ));
    }
    let scale = p.scale.unwrap_or(1.0);
    match (p.tier, p.large_px) {
        (Some(tier), None) => tier_px(tier, scale),
        (None, Some(px)) if THUMB_LARGE_PX.contains(&px) => {
            if !THUMB_SCALES.contains(&scale) {
                return Err(format!("scale {scale} is not between 1.0 and 4.0"));
            }
            Ok(px)
        }
        (None, Some(px)) => Err(format!("large_px {px} is not between 513 and 4096")),
        (Some(_), Some(_)) => Err("give tier or large_px, not both".to_string()),
        (None, None) => Err("give tier (or large_px)".to_string()),
    }
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
            .finish()
    }
}
