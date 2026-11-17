//! The core's thumbnail service (APP-038; Appendix H "Thumbnails"; P4b-R6, P4b-R12, P4b-R36):
//! the core's `interchange` owns thumbnails, not the apps.
//!
//! - `renderers`: the registry, `image@1` (PNG, JPEG, WebP) and `icon@1` (every other kind),
//!   and the guards an untrusted file passes before it is decoded;
//! - `cache`: the cache files in the per-workspace local data folder, keyed by content;
//! - `service`: [`ThumbService`], the [`Thumbnails`](crate::tools::Thumbnails) of
//!   `app/thumbs.request` and the pick log, and its clean-up.
//!
//! [`tier_px`] (the fixed tiers × screen scale) lives with the `Thumbnails` trait in
//! `tools::services`, so `rpc` checks a request by the same rule without naming this module.

pub mod cache;
pub mod renderers;
pub mod service;

pub use crate::tools::{THUMB_MAX_PX, THUMB_TIERS, tier_px};
pub use cache::{KEEP_UNTOUCHED, THUMBS_DIR, entry_name, entry_path, parse_entry, thumbs_dir};
pub use renderers::{
    ALLOC_LARGE, ALLOC_TIERS, DECODED_EXTENSIONS, ICON, IMAGE, IconRenderer, ImageRenderer,
    MAX_FILE_BYTES, MAX_PIXELS, MAX_SIDE, RenderError, Renderer, Sniffed, alloc_limit,
    check_header, default_renderers, header_size, jpeg_complete, render_image, sniff,
};
pub use service::{
    CLEAN_UP_PERIOD, DECODE_DEADLINE, INLINE_WAIT, LiveSource, THUMBS_TOPIC, ThumbOptions,
    ThumbService, default_permits,
};
