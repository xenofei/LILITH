//! `lilref.write` (APP-021; Appendix G "`.lilref` files"): a small `.lilref` file in the cache
//! that names a sealed version, so a link printed in Claude Code output opens it in the suite
//! (Windows Terminal opens only `http`, `https` and `file` links).
//!
//! The file is `<local dir>\cache\lilref\<first 16 hex of SHA-256 of the ref>.lilref`, holding
//! `lil_schema::write_lilref`'s text for the ref in its canonical form (a legacy `ww://` ref is
//! written as `lil://`, so both name one file). Writing the same ref again replaces the file.
//! It is written through `fsops::write_suite_cache_file`, which refuses any other folder.

use std::path::PathBuf;

use lil_platform::Env;
use lil_protocol::{ErrorBody, codes};
use lil_schema::refs::AssetRef;
use lil_schema::{LilRef, parse_lilref};
use sha2::{Digest as _, Sha256};

use crate::store::fsops;

/// The file name of the `.lilref` for the canonical ref text `r`.
pub fn lilref_file_name(r: &str) -> String {
    let digest = Sha256::digest(r.as_bytes());
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("{hex}.lilref")
}

/// Writes the `.lilref` file for `r` with `title` and returns its path. The errors carry an
/// empty trace id; the caller sets its own.
///
/// # Errors
/// - `ref.invalid`: `r` is a candidate ref, or a sealed ref without `#sha256=`;
/// - `params.invalid`: a `title` `parse_lilref` refuses (over 200 characters, or a control or
///   text-direction character);
/// - `store.io`: the file can't be written.
pub fn write_lilref(env: &Env, r: &AssetRef, title: Option<&str>) -> Result<PathBuf, ErrorBody> {
    let text = r.to_string();
    if !matches!(
        r,
        AssetRef::Sealed {
            sha256: Some(_),
            ..
        }
    ) {
        return Err(ErrorBody::new(
            &codes::REF_INVALID,
            format!(
                "{text} is not a sealed ref with its content hash (#sha256=…): a .lilref names a \
                 sealed version; asset.get gives the ref of an approved version."
            ),
            "",
        ));
    }
    // `parse_lilref` checks the ref and the title as a reader will; `write_lilref` then can't
    // panic.
    let candidate = serde_json::json!({"lilref": 1, "ref": text, "title": title}).to_string();
    let file = parse_lilref(&candidate).map_err(|why| {
        ErrorBody::new(
            &codes::PARAMS_INVALID,
            format!("The .lilref can't be written: {why}."),
            "",
        )
    })?;
    let contents = lil_schema::write_lilref(&LilRef {
        r#ref: file.r#ref,
        title: file.title,
    });
    let path = env.lilref_dir().join(lilref_file_name(&text));
    fsops::write_suite_cache_file(env, &path, contents.as_bytes()).map_err(|e| {
        tracing::warn!(file = %path.display(), error = %e, "a .lilref file can't be written");
        ErrorBody::new(
            &codes::STORE_IO,
            format!("The .lilref file {} can't be written: {e}.", path.display()),
            "",
        )
    })?;
    Ok(path)
}
