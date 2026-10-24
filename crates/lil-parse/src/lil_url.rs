//! `lil:` URLs (APP-020, QA-050; Appendix G "`lil:` URLs"): `<scheme>:confirm/<id>`,
//! `<scheme>:deny/<id>/<token>`, `<scheme>:task/<job id>`, `<scheme>:open/<base64url target>`
//! and a raw `lil://` ref.
//!
//! Every allocation is bounded by the argument's own length: an `open` target is decoded only
//! after its base64url text is known to hold at most 4,096 bytes, and a raw ref is decoded into
//! a buffer no longer than the argument.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use lil_schema::AssetRef;

/// The longest argument, in `char`s.
const MAX_ARGUMENT_CHARS: usize = 8192;
/// The longest `open` target, in UTF-8 bytes.
const MAX_TARGET_BYTES: usize = 4096;
/// The longest unpadded base64url text of [`MAX_TARGET_BYTES`] bytes.
const MAX_TARGET_BASE64: usize = MAX_TARGET_BYTES / 3 * 4 + 2;
/// How many `char`s of a refused argument are shown.
const SHOWN_CHARS: usize = 80;

/// What an accepted argument asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LilUrl {
    /// `confirm/<cf_ id>`.
    Confirm(String),
    /// `deny/<cf_ id>/<32 hex token>`.
    Deny { id: String, token: String },
    /// `task/<j_ id>`.
    Task(String),
    /// `open/<base64url>`: the decoded target.
    Open(String),
    /// A raw `lil://` ref, after one percent-decode.
    Ref(String),
}

/// A refused argument: nothing happens, and the app says "This link isn't one LILITH opens:
/// <shown>".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlRefusal {
    /// [`crate::shown_text`] of the argument, at most 80 characters.
    pub shown: String,
    pub reason: &'static str,
}

/// The argument without surrounding spaces and one pair of surrounding `"`.
pub fn strip_argument(raw: &str) -> &str {
    let s = raw.trim_matches(' ');
    s.strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(s)
}

/// Parses one command-line argument under the channel's scheme (`lil`, `lil-dev` or
/// `lil-test-<id>`).
///
/// # Errors
/// [`UrlRefusal`] for anything Appendix G doesn't accept.
pub fn parse_lil_url(arg: &str, channel_scheme: &str) -> Result<LilUrl, UrlRefusal> {
    classify(arg, channel_scheme).map_err(|reason| UrlRefusal {
        shown: crate::shown_text(arg, SHOWN_CHARS),
        reason,
    })
}

/// `<scheme>:open/<base64url of the UTF-8 target, without padding>`.
pub fn open_url(channel_scheme: &str, target: &str) -> String {
    format!("{channel_scheme}:open/{}", URL_SAFE_NO_PAD.encode(target))
}

/// `<scheme>:confirm/<id>`.
pub fn confirm_url(channel_scheme: &str, id: &str) -> String {
    format!("{channel_scheme}:confirm/{id}")
}

/// `<scheme>:deny/<id>/<token>`.
pub fn deny_url(channel_scheme: &str, id: &str, token: &str) -> String {
    format!("{channel_scheme}:deny/{id}/{token}")
}

/// `<scheme>:task/<job id>`.
pub fn task_url(channel_scheme: &str, job_id: &str) -> String {
    format!("{channel_scheme}:task/{job_id}")
}

fn classify(arg: &str, channel_scheme: &str) -> Result<LilUrl, &'static str> {
    if arg.len() > MAX_ARGUMENT_CHARS && arg.chars().count() > MAX_ARGUMENT_CHARS {
        return Err("the argument is longer than 8,192 characters");
    }
    let Some((scheme, rest)) = arg.split_once(':') else {
        return Err("the argument has no scheme");
    };
    if scheme.eq_ignore_ascii_case("lil")
        && let Some(after) = rest.strip_prefix("//")
    {
        return raw_ref(after).map(LilUrl::Ref);
    }
    if !scheme.eq_ignore_ascii_case(channel_scheme) {
        return Err("the link is for another channel or program");
    }
    let Some((verb, tail)) = rest.split_once('/') else {
        return Err("the link names no action");
    };
    let tail = tail.strip_suffix('/').unwrap_or(tail);
    match verb {
        "confirm" => confirmation_id(tail).map(|id| LilUrl::Confirm(id.into())),
        "deny" => {
            let Some((id, token)) = tail.split_once('/') else {
                return Err("a deny link names a confirmation and a token");
            };
            let id = confirmation_id(id)?;
            if !is_lower_hex(token, 32) {
                return Err("the deny token isn't 32 lowercase hex characters");
            }
            Ok(LilUrl::Deny {
                id: id.into(),
                token: token.into(),
            })
        }
        "task" => match tail.strip_prefix("j_") {
            Some(hex) if is_lower_hex(hex, 16) => Ok(LilUrl::Task(tail.into())),
            _ => Err("the job id isn't j_ and 16 lowercase hex characters"),
        },
        "open" => open_target(tail).map(LilUrl::Open),
        _ => Err("the link's action isn't one LILITH knows"),
    }
}

/// `^cf_[0-9a-f]{16}$`.
fn confirmation_id(id: &str) -> Result<&str, &'static str> {
    match id.strip_prefix("cf_") {
        Some(hex) if is_lower_hex(hex, 16) => Ok(id),
        _ => Err("the confirmation id isn't cf_ and 16 lowercase hex characters"),
    }
}

fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// An `open` target: unpadded base64url of 1–4,096 bytes of UTF-8 with no control character,
/// which must be a ref when it starts with `lil://`.
fn open_target(text: &str) -> Result<String, &'static str> {
    if text.len() > MAX_TARGET_BASE64 {
        return Err("the target is longer than 4,096 bytes");
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|_| "the target isn't base64url without padding")?;
    if bytes.is_empty() || bytes.len() > MAX_TARGET_BYTES {
        return Err("the target isn't 1 to 4,096 bytes long");
    }
    let target = String::from_utf8(bytes).map_err(|_| "the target isn't UTF-8")?;
    if target.chars().any(char::is_control) {
        return Err("the target holds a control character");
    }
    if target
        .get(..lil_schema::SCHEME.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(lil_schema::SCHEME))
        && target.parse::<AssetRef>().is_err()
    {
        return Err("the target starts with lil:// but isn't a ref");
    }
    Ok(target)
}

/// A raw ref after its `lil://`: its `%HH` escapes decoded once as UTF-8, refusing an escaped
/// control character or `%`, then parsed as a ref. The result is written with `lil://`.
fn raw_ref(after_scheme: &str) -> Result<String, &'static str> {
    let mut bytes = Vec::with_capacity(lil_schema::SCHEME.len() + after_scheme.len());
    bytes.extend_from_slice(lil_schema::SCHEME.as_bytes());
    let mut rest = after_scheme.bytes();
    while let Some(b) = rest.next() {
        if b != b'%' {
            bytes.push(b);
            continue;
        }
        let (Some(hi), Some(lo)) = (
            rest.next().and_then(hex_value),
            rest.next().and_then(hex_value),
        ) else {
            return Err("a % in the ref isn't followed by two hex digits");
        };
        let decoded = hi << 4 | lo;
        if decoded < 0x20 || decoded == 0x7f {
            return Err("the ref escapes a control character");
        }
        if decoded == b'%' {
            return Err("the ref escapes a %, and escapes are decoded only once");
        }
        bytes.push(decoded);
    }
    let decoded = String::from_utf8(bytes).map_err(|_| "the decoded ref isn't UTF-8")?;
    if decoded.chars().any(char::is_control) {
        return Err("the ref holds a control character");
    }
    if decoded.parse::<AssetRef>().is_err() {
        return Err("the argument isn't a lil:// ref");
    }
    Ok(decoded)
}

fn hex_value(b: u8) -> Option<u8> {
    char::from(b).to_digit(16).map(|d| d as u8)
}
