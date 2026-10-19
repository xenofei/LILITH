//! `.lilref` pointer files (APP-021, APP-055, CORE-047): `{"lilref": 1, "ref":
//! "lil://…@<v>#sha256=<hex>", "title"?}`.
//!
//! Windows Terminal only opens `http`, `https` and `file` links, so a link printed in Claude
//! Code output points to a small `.lilref` file, which the suite is associated with. The file
//! names a sealed version, so its ref carries the version's content hash (CORE-047): a ref
//! without `#sha256=` is refused, as is a candidate ref (viewing only, APP-057) and any
//! `lilref` version but 1. [`write_lilref`] writes the form the spec shows, one line with a
//! space after each colon and comma and no trailing newline, so a file this module wrote
//! reads back and writes again byte for byte.

use serde::Deserialize;

use crate::refs::{AssetRef, SCHEME};

/// The one `lilref` version.
pub const LILREF_VERSION: u64 = 1;

/// A parsed `.lilref` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LilRef {
    /// A sealed `lil://` ref with `#sha256=<64 hex>`, as written.
    pub r#ref: String,
    pub title: Option<String>,
}

/// The file's JSON shape. Unknown fields are refused, so nothing a file carries is dropped by
/// a read and write.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    lilref: serde_json::Value,
    r#ref: String,
    #[serde(default)]
    title: Option<String>,
}

/// Parses the text of a `.lilref` file.
///
/// # Errors
/// Why the text is not one: not JSON, a missing or unknown field, a `lilref` version other than
/// 1, a `ref` that is not a sealed `lil://` ref carrying `#sha256=`.
pub fn parse_lilref(text: &str) -> Result<LilRef, String> {
    let raw: Raw = serde_json::from_str(text).map_err(|e| format!("not a .lilref file: {e}"))?;
    if raw.lilref.as_u64() != Some(LILREF_VERSION) {
        return Err(format!(
            "unsupported lilref version {}; this build reads version {LILREF_VERSION}",
            raw.lilref
        ));
    }
    check_ref(&raw.r#ref)?;
    Ok(LilRef {
        r#ref: raw.r#ref,
        title: raw.title,
    })
}

/// The text of a `.lilref` file for `r`: `{"lilref": 1, "ref": "…"}`, with `"title": "…"` after
/// the ref when there is one.
///
/// # Panics
/// If `r.ref` is not a sealed `lil://` ref carrying `#sha256=` (the ref must carry its hash,
/// CORE-047); check a ref you didn't build with [`parse_lilref`].
pub fn write_lilref(r: &LilRef) -> String {
    if let Err(e) = check_ref(&r.r#ref) {
        panic!("a .lilref names a sealed ref with its hash: {e}");
    }
    let mut out = format!(
        "{{\"lilref\": {LILREF_VERSION}, \"ref\": {}",
        json_string(&r.r#ref)
    );
    if let Some(title) = &r.title {
        out.push_str(", \"title\": ");
        out.push_str(&json_string(title));
    }
    out.push('}');
    out
}

fn json_string(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// `r` is a sealed `lil://` ref that carries its content hash, written the canonical way.
fn check_ref(r: &str) -> Result<(), String> {
    if !r.starts_with(SCHEME) {
        return Err(format!("`{r}` is not a lil:// ref"));
    }
    match r.parse::<AssetRef>()? {
        AssetRef::Sealed {
            sha256: Some(_), ..
        } => Ok(()),
        AssetRef::Sealed { sha256: None, .. } => Err(format!(
            "`{r}` has no `#sha256=<hex>`: a .lilref names a sealed version with its content hash"
        )),
        AssetRef::Candidate { .. } => Err(format!(
            "`{r}` is a candidate ref: candidates are for viewing only, a .lilref names a sealed version"
        )),
    }
}
