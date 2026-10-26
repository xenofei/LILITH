//! Parsers for text that comes from outside the suite (QA-050), shared by the core and the apps:
//!
//! - [`lil_url`]: the `lil:` activation URLs and raw `lil://` refs (APP-020; Appendix G "`lil:`
//!   URLs");
//! - [`text`]: the shown-text normaliser for notification text (Appendix G "Toast XML");
//! - [`model_header`]: model families from safetensors and GGUF headers (CORE-164).
//!
//! Platform-neutral: this crate has no Qt or Windows dependencies (ARCH-111, QA-027).
//!
//! The public signatures come first (QA-017): plan 4a's Task 3 writes them with `todo!()`
//! bodies for the spec tests, and Task 4 fills them.

pub mod lil_url;
pub mod model_header;
pub mod text;

pub use lil_url::{
    LilUrl, UrlRefusal, confirm_url, deny_url, open_url, parse_lil_url, strip_argument, task_url,
};
pub use model_header::{Family, HeaderError, gguf_family, safetensors_family};
pub use text::{shown_text, without_default_ignorables};
