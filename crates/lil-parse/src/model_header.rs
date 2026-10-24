//! Model families from file headers (CORE-164; Appendix G "Model inventory report"): the
//! safetensors `__metadata__` keys `modelspec.architecture` and `ss_base_model_version`, and
//! GGUF's `general.architecture`, within fixed limits. Pickle formats are never opened.
//!
//! Every length is checked against its limit before anything it announces is read, and
//! nothing is allocated from an announced length: buffers grow as their bytes arrive, and GGUF
//! values other than the keys and the family are skipped, never stored. At most 100 MiB of
//! either header is read.

use std::io::{self, Read};

/// The largest safetensors header read.
const MAX_SAFETENSORS_HEADER: u64 = 100 * 1024 * 1024;
/// The most GGUF keys before `general.architecture`.
const MAX_GGUF_KEYS_FIRST: u64 = 1024;
/// The longest string held in memory: a key or the family. A skipped value has no limit of its
/// own; the 100 MiB read bound covers it.
const MAX_STRING: u64 = 64 * 1024;
/// The deepest GGUF array of arrays skipped.
const MAX_GGUF_ARRAY_DEPTH: u32 = 8;
/// The most GGUF header bytes read, as for safetensors.
const MAX_GGUF_HEADER: u64 = MAX_SAFETENSORS_HEADER;

/// A model family read from a header, or none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Family {
    Known(String),
    Unknown,
}

/// A header that couldn't be read.
#[derive(Debug)]
pub enum HeaderError {
    Io(std::io::Error),
    Malformed(&'static str),
}

/// The family in a safetensors header; a header over 100 MiB, a family over 64 KiB, or a
/// candidate key given twice, is [`Family::Unknown`].
///
/// # Errors
/// [`HeaderError`] when the header can't be read or isn't a safetensors header (which includes
/// a repeated `__metadata__`, as the reference loader refuses it).
pub fn safetensors_family(r: &mut dyn std::io::Read) -> Result<Family, HeaderError> {
    let len = u64::from_le_bytes(read_array(r).map_err(HeaderError::Io)?);
    if len > MAX_SAFETENSORS_HEADER {
        return Ok(Family::Unknown);
    }
    let mut header = Vec::new();
    r.take(len)
        .read_to_end(&mut header)
        .map_err(HeaderError::Io)?;
    if header.len() as u64 != len {
        return Err(HeaderError::Io(io::ErrorKind::UnexpectedEof.into()));
    }
    // The header is walked, never built: a tree of its tensor entries could take many times
    // its size in memory. Only keys and the family candidates are decoded.
    let mut metadata = None;
    let mut repeated = false;
    walk_object(&header, 0, &mut |key, value| {
        if key == "__metadata__" {
            repeated |= metadata.replace(value).is_some();
        }
    })
    .map_err(HeaderError::Malformed)?;
    if repeated {
        return Err(HeaderError::Malformed(
            "the safetensors header repeats __metadata__",
        ));
    }
    let Some(metadata) = metadata.filter(|m| m.first() == Some(&b'{')) else {
        return Ok(Family::Unknown);
    };
    const KEYS: [&str; 2] = ["modelspec.architecture", "ss_base_model_version"];
    let mut found: [Option<&[u8]>; 2] = [None; 2];
    walk_object(metadata, 1, &mut |key, value| {
        if let Some(i) = KEYS.iter().position(|k| *k == key) {
            repeated |= found[i].replace(value).is_some();
        }
    })
    .map_err(HeaderError::Malformed)?;
    if repeated {
        // Loaders disagree on which copy wins, so neither is shown.
        return Ok(Family::Unknown);
    }
    for value in found.into_iter().flatten() {
        if value.first() != Some(&b'"') {
            continue;
        }
        // An escape is at most 6 bytes for one, so a longer text can't decode within the cap.
        if value.len() as u64 > 6 * MAX_STRING + 2 {
            return Ok(Family::Unknown);
        }
        let family: String =
            serde_json::from_slice(value).map_err(|_| HeaderError::Malformed(MALFORMED))?;
        if family.len() as u64 > MAX_STRING {
            return Ok(Family::Unknown);
        }
        if !family.is_empty() {
            return Ok(Family::Known(family));
        }
    }
    Ok(Family::Unknown)
}

const MALFORMED: &str = "the safetensors header isn't a JSON object";
/// The deepest JSON document read: serde_json's limit (its recursion budget of 128 runs out at
/// the 128th open bracket), which the safetensors loader inherits.
const MAX_JSON_DEPTH: usize = 127;

/// Walks the JSON object that is the whole of `json` (whitespace around it allowed), nested
/// `enclosing` levels deep in its document, calling `each` with every member's key and the bytes
/// of its value. Values are checked as JSON but not decoded.
fn walk_object<'a>(
    json: &'a [u8],
    enclosing: usize,
    each: &mut dyn FnMut(&str, &'a [u8]),
) -> Result<(), &'static str> {
    let max_depth = MAX_JSON_DEPTH - enclosing - 1;
    let mut at = json_whitespace_end(json, 0);
    if json.get(at) != Some(&b'{') {
        return Err(MALFORMED);
    }
    at = json_whitespace_end(json, at + 1);
    if json.get(at) == Some(&b'}') {
        return only_whitespace_after(json, at + 1);
    }
    loop {
        if json.get(at) != Some(&b'"') {
            return Err(MALFORMED);
        }
        let key_end = json_string_end(json, at)?;
        let key: String = serde_json::from_slice(&json[at..key_end]).map_err(|_| MALFORMED)?;
        at = json_whitespace_end(json, key_end);
        if json.get(at) != Some(&b':') {
            return Err(MALFORMED);
        }
        let start = json_whitespace_end(json, at + 1);
        let end = json_value_end(json, start, max_depth)?;
        each(&key, &json[start..end]);
        at = json_whitespace_end(json, end);
        match json.get(at) {
            Some(b',') => at = json_whitespace_end(json, at + 1),
            Some(b'}') => return only_whitespace_after(json, at + 1),
            _ => return Err(MALFORMED),
        }
    }
}

fn only_whitespace_after(json: &[u8], at: usize) -> Result<(), &'static str> {
    if json_whitespace_end(json, at) == json.len() {
        Ok(())
    } else {
        Err(MALFORMED)
    }
}

fn json_whitespace_end(json: &[u8], at: usize) -> usize {
    at + json[at.min(json.len())..]
        .iter()
        .take_while(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        .count()
}

/// The end of the string starting at `at` (its `"`).
fn json_string_end(json: &[u8], at: usize) -> Result<usize, &'static str> {
    let mut i = at + 1;
    while let Some(&b) = json.get(i) {
        match b {
            b'"' => return Ok(i + 1),
            b'\\' => i += 2,
            _ => i += 1,
        }
    }
    Err(MALFORMED)
}

/// The end of the member key starting at `at` and its `:`.
fn json_key_end(json: &[u8], at: usize) -> Result<usize, &'static str> {
    if json.get(at) != Some(&b'"') {
        return Err(MALFORMED);
    }
    let at = json_whitespace_end(json, json_string_end(json, at)?);
    if json.get(at) != Some(&b':') {
        return Err(MALFORMED);
    }
    Ok(at + 1)
}

/// The end of a scalar: `true`, `false`, `null` or a run of number characters.
fn json_scalar_end(json: &[u8], at: usize) -> Result<usize, &'static str> {
    let rest = &json[at.min(json.len())..];
    for word in [&b"true"[..], b"false", b"null"] {
        if rest.starts_with(word) {
            return Ok(at + word.len());
        }
    }
    let len = rest
        .iter()
        .take_while(|b| matches!(b, b'-' | b'+' | b'.' | b'0'..=b'9' | b'e' | b'E'))
        .count();
    if len == 0 {
        Err(MALFORMED)
    } else {
        Ok(at + len)
    }
}

/// The end of the JSON value starting at `at`, nested at most `max_depth` levels, checked
/// without recursion or allocation: brackets must pair, members are `"key": value` separated by
/// commas, and scalars are as [`json_scalar_end`] reads them. Strings are skipped by their
/// quotes, not decoded.
fn json_value_end(json: &[u8], at: usize, max_depth: usize) -> Result<usize, &'static str> {
    let mut closers = [0_u8; MAX_JSON_DEPTH];
    let mut depth = 0;
    let mut i = at;
    'value: loop {
        i = json_whitespace_end(json, i);
        match json.get(i) {
            Some(&open @ (b'{' | b'[')) => {
                if depth >= max_depth {
                    return Err("the safetensors header is nested too deeply");
                }
                let close = if open == b'{' { b'}' } else { b']' };
                closers[depth] = close;
                depth += 1;
                i = json_whitespace_end(json, i + 1);
                if json.get(i) == Some(&close) {
                    depth -= 1;
                    i += 1;
                } else {
                    if close == b'}' {
                        i = json_key_end(json, i)?;
                    }
                    continue 'value;
                }
            }
            Some(b'"') => i = json_string_end(json, i)?,
            _ => i = json_scalar_end(json, i)?,
        }
        // After a value: close containers, or go on to the next element.
        loop {
            if depth == 0 {
                return Ok(i);
            }
            i = json_whitespace_end(json, i);
            match json.get(i) {
                Some(b',') => {
                    i = json_whitespace_end(json, i + 1);
                    if closers[depth - 1] == b'}' {
                        i = json_key_end(json, i)?;
                    }
                    continue 'value;
                }
                Some(&c) if c == closers[depth - 1] => {
                    depth -= 1;
                    i += 1;
                }
                _ => return Err(MALFORMED),
            }
        }
    }
}

/// The family in a GGUF header (versions 2 and 3; another version is [`Family::Unknown`]);
/// more than 1,024 keys before `general.architecture`, `general.architecture` given twice, a
/// key or family over 64 KiB, or a key-value section over 100 MiB, is [`Family::Unknown`]. Other
/// values are skipped whatever their length.
///
/// # Errors
/// [`HeaderError`] when the header can't be read or isn't a GGUF header.
pub fn gguf_family(r: &mut dyn std::io::Read) -> Result<Family, HeaderError> {
    let mut bounded = r.take(MAX_GGUF_HEADER);
    match read_gguf(&mut bounded) {
        Ok(family) => Ok(family),
        Err(Stop::Limit) => Ok(Family::Unknown),
        Err(Stop::Io(e)) if bounded.limit() == 0 && e.kind() == io::ErrorKind::UnexpectedEof => {
            Ok(Family::Unknown)
        }
        Err(Stop::Io(e)) => Err(HeaderError::Io(e)),
        Err(Stop::Malformed(why)) => Err(HeaderError::Malformed(why)),
    }
}

/// Why reading a GGUF header stopped early.
enum Stop {
    /// A limit was passed: the family is unknown.
    Limit,
    Io(io::Error),
    Malformed(&'static str),
}

impl From<io::Error> for Stop {
    fn from(e: io::Error) -> Stop {
        Stop::Io(e)
    }
}

const GGUF_STRING: u32 = 8;
const GGUF_ARRAY: u32 = 9;

fn read_gguf(r: &mut dyn Read) -> Result<Family, Stop> {
    if read_array::<4>(r)? != *b"GGUF" {
        return Err(Stop::Malformed("not a GGUF file"));
    }
    let version = u32::from_le_bytes(read_array(r)?);
    if !matches!(version, 2 | 3) {
        return Ok(Family::Unknown);
    }
    let _tensors = read_u64(r)?;
    let keys = read_u64(r)?;
    let mut family = None;
    for index in 0..keys {
        if family.is_none() && index > MAX_GGUF_KEYS_FIRST {
            return Err(Stop::Limit);
        }
        let key = read_string(r)?;
        let ty = u32::from_le_bytes(read_array(r)?);
        if key == b"general.architecture" {
            // A second one: llama.cpp refuses the file, so no family is shown.
            if family.is_some() || ty != GGUF_STRING {
                return Ok(Family::Unknown);
            }
            let value = String::from_utf8(read_string(r)?)
                .map_err(|_| Stop::Malformed("general.architecture isn't UTF-8"))?;
            if value.is_empty() {
                return Ok(Family::Unknown);
            }
            family = Some(value);
            continue;
        }
        skip_value(r, ty, 0)?;
    }
    // The rest of the key-value section is read, so a repeated key is seen.
    Ok(family.map_or(Family::Unknown, Family::Known))
}

/// Skips one value of type `ty` by its declared length.
fn skip_value(r: &mut dyn Read, ty: u32, depth: u32) -> Result<(), Stop> {
    if let Some(size) = fixed_size(ty) {
        return skip(r, size);
    }
    match ty {
        GGUF_STRING => {
            let len = read_u64(r)?;
            skip(r, len)
        }
        GGUF_ARRAY => {
            if depth >= MAX_GGUF_ARRAY_DEPTH {
                return Err(Stop::Malformed("GGUF arrays are nested too deeply"));
            }
            let element = u32::from_le_bytes(read_array(r)?);
            let count = read_u64(r)?;
            if let Some(size) = fixed_size(element) {
                let total = count
                    .checked_mul(size)
                    .ok_or(Stop::Malformed("a GGUF array is longer than any file"))?;
                return skip(r, total);
            }
            if !matches!(element, GGUF_STRING | GGUF_ARRAY) {
                return Err(Stop::Malformed("unknown GGUF value type"));
            }
            for _ in 0..count {
                skip_value(r, element, depth + 1)?;
            }
            Ok(())
        }
        _ => Err(Stop::Malformed("unknown GGUF value type")),
    }
}

/// The size of a GGUF scalar type, in bytes.
fn fixed_size(ty: u32) -> Option<u64> {
    match ty {
        0 | 1 | 7 => Some(1), // u8, i8, bool
        2 | 3 => Some(2),     // u16, i16
        4..=6 => Some(4),     // u32, i32, f32
        10..=12 => Some(8),   // u64, i64, f64
        _ => None,
    }
}

/// A GGUF string's bytes, at most 64 KiB.
fn read_string(r: &mut dyn Read) -> Result<Vec<u8>, Stop> {
    let len = read_u64(r)?;
    if len > MAX_STRING {
        return Err(Stop::Limit);
    }
    let mut bytes = Vec::new();
    if r.take(len).read_to_end(&mut bytes)? as u64 != len {
        return Err(Stop::Io(io::ErrorKind::UnexpectedEof.into()));
    }
    Ok(bytes)
}

fn read_u64(r: &mut dyn Read) -> io::Result<u64> {
    read_array(r).map(u64::from_le_bytes)
}

fn read_array<const N: usize>(r: &mut dyn Read) -> io::Result<[u8; N]> {
    let mut bytes = [0; N];
    r.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// Reads and drops `n` bytes; fewer is an unexpected end of file.
fn skip(r: &mut dyn Read, n: u64) -> Result<(), Stop> {
    if io::copy(&mut r.take(n), &mut io::sink())? != n {
        return Err(Stop::Io(io::ErrorKind::UnexpectedEof.into()));
    }
    Ok(())
}
