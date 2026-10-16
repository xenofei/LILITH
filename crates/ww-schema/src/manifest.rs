//! Asset manifests (I5; ARCH-076, CORE-148), lossless.
//!
//! The ordered JSON map (`serde_json` with `preserve_order`) is the truth. Typed views
//! ([`ManifestView`]) read it; setters edit it in place, so unknown and retired fields stay in
//! their original place and an edit is a minimal diff:
//! - an existing key keeps its position, a new key is appended;
//! - a removed key is removed by shifting the keys after it (`shift_remove`), never by moving
//!   the last key into its slot (`Map::remove` under `preserve_order` is `swap_remove`, and is
//!   disallowed in this crate's `clippy.toml`);
//! - no setter adds `schema_version`, so a legacy manifest never gains it;
//! - the text keeps the line ending it was read with.
//!
//! Schema violations are [`SchemaWarning`]s, never parse errors: every view field is read
//! leniently, and a wrong type gives a warning and the field's default.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::status::Status;
use crate::version::Version;

/// A schema violation: a JSON pointer into the manifest and what is wrong there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaWarning {
    pub pointer: String,
    pub message: String,
}

/// A manifest: the ordered map, the line ending of its text, and whether the text started
/// with a UTF-8 byte-order mark (kept on output).
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    map: Map<String, Value>,
    eol: &'static str,
    bom: bool,
}

/// Why [`Manifest::push`] refused an edit. Nothing was changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// The key holds a value that is neither a list nor `null`.
    NotAList { key: String },
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditError::NotAList { key } => {
                write!(
                    f,
                    "`{key}` holds a value that is not a list; nothing was changed"
                )
            }
        }
    }
}

impl std::error::Error for EditError {}

/// The UTF-8 byte-order mark.
pub(crate) const BOM: char = '\u{feff}';

/// The typed read of a manifest.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ManifestView {
    pub schema_version: Option<u32>,
    pub asset_id: String,
    pub project: String,
    pub asset_type: Option<String>,
    pub status: Option<Status>,
    pub version: Option<Version>,
    pub approved_version: Option<Version>,
    pub creation_uuid: Option<String>,
    pub inputs: Vec<Input>,
    pub pinned_inputs: Vec<PinnedInput>,
    pub candidate_runs: Vec<CandidateRun>,
    /// Key: path relative to the asset folder; value: bare 64-hex, read from `sha256:<hex>`
    /// or `<hex>`.
    pub hashes: BTreeMap<String, String>,
    pub locks: Vec<String>,
    pub human_edits: Vec<HumanEdit>,
    pub external: Vec<ExternalFile>,
    pub has_working_doc: bool,
}

/// An `inputs` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    pub r#ref: String,
    pub role: Option<String>,
    /// Bare hex.
    pub sha256: Option<String>,
}

/// A `pinned_inputs` entry (I6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedInput {
    pub r#ref: String,
    pub reason: Option<String>,
    pub by: Option<String>,
    pub date: Option<String>,
}

/// A `candidate_runs` entry (I5).
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateRun {
    pub run_id: String,
    pub path: Option<String>,
    /// `open`, `selected` or `rejected`.
    pub status: Option<String>,
    pub created: Option<String>,
    pub target_version: Option<Version>,
    pub files: Vec<CandidateFile>,
    pub selected: Vec<String>,
    pub signed_off: Option<SignedOff>,
    pub pinned: bool,
    pub trashed_utc: Option<String>,
}

/// A `candidate_runs[].files` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateFile {
    /// Asset-relative (I5).
    pub path: String,
    pub seed: Option<i64>,
    /// `candidate`, `derived` or `repair`.
    pub role: String,
    pub from: Option<String>,
    /// Bare hex; absent in pre-2b runs.
    pub sha256: Option<String>,
}

/// `candidate_runs[].signed_off`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedOff {
    pub by: String,
    pub date: String,
    pub version: Version,
}

/// A `human_edits` entry. I5's `author` and `note` are read when present and otherwise
/// ignored; the core writes `path`, `sha256`, `previous_sha256` and `date`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HumanEdit {
    pub path: String,
    /// Bare hex.
    pub sha256: String,
    /// Bare hex.
    pub previous_sha256: Option<String>,
    pub date: Option<String>,
}

/// An `external` entry (a link-external file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalFile {
    pub path: String,
    /// Bare hex.
    pub sha256: String,
}

/// `sha256:<64 hex>` or `<64 hex>` (lowercase) → the hex; anything else → `None`.
pub fn bare_sha256(value: &str) -> Option<&str> {
    let hex = value.strip_prefix("sha256:").unwrap_or(value);
    (hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))).then_some(hex)
}

/// `sha256:<hex>`, the form the core writes into `hashes`.
pub fn hash_value(hex: &str) -> String {
    format!("sha256:{hex}")
}

/// Removes `key` from a nested map by shifting the keys after it (never `swap_remove`).
pub fn remove_key(map: &mut Map<String, Value>, key: &str) -> Option<Value> {
    map.shift_remove(key)
}

impl Manifest {
    /// Parses a manifest. Only text that isn't a JSON object is an error; schema violations
    /// are returned as warnings.
    ///
    /// A leading UTF-8 byte-order mark is skipped and written back by `to_text`. A key that
    /// appears twice in one object is a warning: the map keeps the first position and the last
    /// value (serde_json's behaviour), so `to_text` writes it once.
    pub fn parse(text: &str) -> Result<(Manifest, Vec<SchemaWarning>), serde_json::Error> {
        let (bom, text) = match text.strip_prefix(BOM) {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        let map: Map<String, Value> = serde_json::from_str(text)?;
        let eol = match text.find('\n') {
            Some(i) if text[..i].ends_with('\r') => "\r\n",
            _ => "\n",
        };
        let manifest = Manifest { map, eol, bom };
        let mut warnings = duplicate_keys(text);
        warnings.extend(read_view(&manifest.map).1);
        Ok((manifest, warnings))
    }

    /// A new manifest: `schema_version` 1, `status` `planned`, `version` `1.0.0`.
    pub fn new(project: &str, asset_id: &str, asset_type: &str, creation_uuid: &str) -> Manifest {
        let mut map = Map::new();
        let mut put = |k: &str, v: Value| {
            map.insert(k.to_string(), v);
        };
        put("schema_version", Value::from(1));
        put("asset_id", Value::from(asset_id));
        put("project", Value::from(project));
        put("creation_uuid", Value::from(creation_uuid));
        put("type", Value::from(asset_type));
        put("version", Value::from("1.0.0"));
        put("approved_version", Value::Null);
        put("status", Value::from(Status::Planned.as_str()));
        put("inputs", Value::Array(Vec::new()));
        put("pinned_inputs", Value::Array(Vec::new()));
        put("candidate_runs", Value::Array(Vec::new()));
        put("human_edits", Value::Array(Vec::new()));
        put("locks", Value::Array(Vec::new()));
        put("hashes", Value::Object(Map::new()));
        Manifest {
            map,
            eol: "\n",
            bom: false,
        }
    }

    /// `serde_json` pretty (2 spaces) and a final line break, with the line ending `parse`
    /// saw: `\r\n` when the text's first line break is CRLF, else `\n`. A byte-order mark the
    /// text started with is written back.
    pub fn to_text(&self) -> String {
        // Line breaks inside strings are escaped (`\n`), so every raw '\n' is a line end.
        let mut text = serde_json::to_string_pretty(&self.map).expect("a JSON map serialises");
        text.push('\n');
        if self.eol == "\r\n" {
            text = text.replace('\n', "\r\n");
        }
        if self.bom {
            text.insert(0, BOM);
        }
        text
    }

    /// The line ending `to_text` writes.
    pub fn eol(&self) -> &'static str {
        self.eol
    }

    /// The ordered map.
    pub fn as_map(&self) -> &Map<String, Value> {
        &self.map
    }

    /// The typed read.
    pub fn view(&self) -> ManifestView {
        read_view(&self.map).0
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.map.get(key)
    }

    /// Sets `key`: an existing key keeps its position, a new one is appended.
    pub fn set(&mut self, key: &str, value: Value) {
        // IndexMap's insert keeps the position of an existing key.
        self.map.insert(key.to_string(), value);
    }

    /// The `candidate_runs` entries that are objects, for in-place edits.
    pub fn runs_mut(&mut self) -> Vec<&mut Map<String, Value>> {
        match self.map.get_mut("candidate_runs") {
            Some(Value::Array(runs)) => runs.iter_mut().filter_map(Value::as_object_mut).collect(),
            _ => Vec::new(),
        }
    }

    /// Appends `value` to the list `list_key`. A missing key gets a one-item list at the end, a
    /// `null` value becomes a one-item list in place (ruling Q5). Any other value is
    /// [`EditError::NotAList`], and nothing changes.
    pub fn push(&mut self, list_key: &str, value: Value) -> Result<(), EditError> {
        match self.map.get_mut(list_key) {
            Some(Value::Array(list)) => list.push(value),
            Some(null @ Value::Null) => *null = Value::Array(vec![value]),
            Some(_) => {
                return Err(EditError::NotAList {
                    key: list_key.to_string(),
                });
            }
            None => {
                self.map
                    .insert(list_key.to_string(), Value::Array(vec![value]));
            }
        }
        Ok(())
    }

    /// Removes `key`; the keys after it keep their order.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.map.shift_remove(key)
    }
}

/// A warning for every key that appears more than once in one object of `text`, which
/// already parsed. serde_json keeps such a key once, at its first position, with the last
/// value; this second pass over the text is the only way to notice.
fn duplicate_keys(text: &str) -> Vec<SchemaWarning> {
    let mut found = Vec::new();
    let mut de = serde_json::Deserializer::from_str(text);
    let walk = Walk {
        at: String::new(),
        found: &mut found,
    };
    // The text already parsed as a JSON object, so the walk can't fail.
    let _ = serde::de::DeserializeSeed::deserialize(walk, &mut de);
    found
        .into_iter()
        .map(|pointer| SchemaWarning {
            pointer,
            message: "duplicate key: kept once, at its first position, with the last value".into(),
        })
        .collect()
}

/// Walks a JSON value, recording the pointers of repeated object keys.
struct Walk<'a> {
    at: String,
    found: &'a mut Vec<String>,
}

impl<'de> serde::de::DeserializeSeed<'de> for Walk<'_> {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> serde::de::Visitor<'de> for Walk<'_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E>(self, _: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E>(self, _: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E>(self, _: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E>(self, _: f64) -> Result<(), E> {
        Ok(())
    }

    fn visit_str<E>(self, _: &str) -> Result<(), E> {
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let Walk { at, found } = self;
        let mut i = 0usize;
        while seq
            .next_element_seed(Walk {
                at: format!("{at}/{i}"),
                found: &mut *found,
            })?
            .is_some()
        {
            i += 1;
        }
        Ok(())
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let Walk { at, found } = self;
        let mut seen = std::collections::HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            let pointer = format!("{at}/{}", esc(&key));
            if !seen.insert(key) {
                found.push(pointer.clone());
            }
            map.next_value_seed(Walk {
                at: pointer,
                found: &mut *found,
            })?;
        }
        Ok(())
    }
}

/// `key` escaped for a JSON pointer (RFC 6901).
fn esc(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// Collects warnings while the view is read.
struct Reader {
    warnings: Vec<SchemaWarning>,
}

/// A JSON object and its pointer.
#[derive(Clone, Copy)]
struct Obj<'a> {
    map: &'a Map<String, Value>,
    at: &'a str,
}

impl Reader {
    fn warn(&mut self, pointer: String, message: impl Into<String>) {
        self.warnings.push(SchemaWarning {
            pointer,
            message: message.into(),
        });
    }

    /// The value of `key`, treating `null` as absent.
    fn field<'a>(o: Obj<'a>, key: &str) -> Option<&'a Value> {
        o.map.get(key).filter(|v| !v.is_null())
    }

    fn ptr(o: Obj<'_>, key: &str) -> String {
        format!("{}/{}", o.at, esc(key))
    }

    fn opt_str(&mut self, o: Obj<'_>, key: &str) -> Option<String> {
        match Self::field(o, key)? {
            Value::String(s) => Some(s.clone()),
            _ => {
                self.warn(Self::ptr(o, key), "expected a string");
                None
            }
        }
    }

    /// A string field an element can't do without: `None` without a warning, so the caller
    /// warns once for the element it skips.
    fn req_str(o: Obj<'_>, key: &str) -> Option<String> {
        Self::field(o, key)?.as_str().map(str::to_string)
    }

    fn opt_version(&mut self, o: Obj<'_>, key: &str) -> Option<Version> {
        let s = self.opt_str(o, key)?;
        match s.parse() {
            Ok(v) => Some(v),
            Err(e) => {
                self.warn(Self::ptr(o, key), e);
                None
            }
        }
    }

    fn opt_sha(&mut self, o: Obj<'_>, key: &str) -> Option<String> {
        let value = Self::field(o, key)?;
        match value.as_str().and_then(bare_sha256) {
            Some(hex) => Some(hex.to_string()),
            None => {
                self.warn(
                    Self::ptr(o, key),
                    "expected `sha256:<64 hex>` or `<64 hex>`",
                );
                None
            }
        }
    }

    fn opt_bool(&mut self, o: Obj<'_>, key: &str) -> bool {
        match Self::field(o, key) {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => {
                self.warn(Self::ptr(o, key), "expected true or false");
                false
            }
        }
    }

    fn opt_i64(&mut self, o: Obj<'_>, key: &str) -> Option<i64> {
        let n = Self::field(o, key)?.as_i64();
        if n.is_none() {
            self.warn(Self::ptr(o, key), "expected an integer");
        }
        n
    }

    /// The elements of the list `key` (absent or null: none).
    fn list<'a>(&mut self, o: Obj<'a>, key: &str) -> &'a [Value] {
        match Self::field(o, key) {
            None => &[],
            Some(Value::Array(items)) => items,
            Some(_) => {
                self.warn(Self::ptr(o, key), "expected a list");
                &[]
            }
        }
    }

    /// Each element of the list `key` that is an object, with its pointer.
    fn objects<'a>(&mut self, o: Obj<'a>, key: &str) -> Vec<(String, &'a Map<String, Value>)> {
        let base = Self::ptr(o, key);
        let mut out = Vec::new();
        for (i, item) in self.list(o, key).iter().enumerate() {
            match item {
                Value::Object(m) => out.push((format!("{base}/{i}"), m)),
                _ => self.warn(format!("{base}/{i}"), "skipped: expected an object"),
            }
        }
        out
    }

    fn strings(&mut self, o: Obj<'_>, key: &str) -> Vec<String> {
        let base = Self::ptr(o, key);
        let mut out = Vec::new();
        for (i, item) in self.list(o, key).iter().enumerate() {
            match item {
                Value::String(s) => out.push(s.clone()),
                _ => self.warn(format!("{base}/{i}"), "skipped: expected a string"),
            }
        }
        out
    }

    fn skipped(&mut self, at: &str, needs: &str) {
        self.warn(at.to_string(), format!("skipped: {needs}"));
    }
}

/// Reads the typed view, leniently: every problem is a warning and the field's default.
fn read_view(map: &Map<String, Value>) -> (ManifestView, Vec<SchemaWarning>) {
    let mut r = Reader {
        warnings: Vec::new(),
    };
    let root = Obj { map, at: "" };
    let mut v = ManifestView::default();

    match map.get("schema_version") {
        None => r.warn(
            "/schema_version".into(),
            "missing: a legacy manifest (read as is; edits never add it)",
        ),
        Some(value) => match value.as_u64().and_then(|n| u32::try_from(n).ok()) {
            Some(n) => {
                if n > 1 {
                    r.warn(
                        "/schema_version".into(),
                        format!("schema version {n} is newer than this suite's (1)"),
                    );
                }
                v.schema_version = Some(n);
            }
            None => r.warn("/schema_version".into(), "expected a small whole number"),
        },
    }
    for (key, slot) in [("asset_id", &mut v.asset_id), ("project", &mut v.project)] {
        match map.get(key) {
            Some(Value::String(s)) => *slot = s.clone(),
            Some(_) => r.warn(format!("/{key}"), "expected a string"),
            None => r.warn(format!("/{key}"), "missing"),
        }
    }
    v.asset_type = r.opt_str(root, "type");
    if let Some(s) = r.opt_str(root, "status") {
        match s.parse() {
            Ok(st) => v.status = Some(st),
            Err(e) => r.warn("/status".into(), e),
        }
    }
    v.version = r.opt_version(root, "version");
    v.approved_version = r.opt_version(root, "approved_version");
    v.creation_uuid = r.opt_str(root, "creation_uuid");

    for (at, m) in r.objects(root, "inputs") {
        let o = Obj { map: m, at: &at };
        let Some(r#ref) = Reader::req_str(o, "ref") else {
            r.skipped(&at, "`ref` is missing or not a string");
            continue;
        };
        let role = r.opt_str(o, "role");
        let sha256 = r.opt_sha(o, "sha256");
        v.inputs.push(Input {
            r#ref,
            role,
            sha256,
        });
    }
    for (at, m) in r.objects(root, "pinned_inputs") {
        let o = Obj { map: m, at: &at };
        let Some(r#ref) = Reader::req_str(o, "ref") else {
            r.skipped(&at, "`ref` is missing or not a string");
            continue;
        };
        v.pinned_inputs.push(PinnedInput {
            r#ref,
            reason: r.opt_str(o, "reason"),
            by: r.opt_str(o, "by"),
            date: r.opt_str(o, "date"),
        });
    }
    for (at, m) in r.objects(root, "candidate_runs") {
        if let Some(run) = read_run(&mut r, Obj { map: m, at: &at }) {
            v.candidate_runs.push(run);
        }
    }

    match map.get("hashes").filter(|h| !h.is_null()) {
        None => {}
        Some(Value::Object(hashes)) => {
            for (path, value) in hashes {
                match value.as_str().and_then(bare_sha256) {
                    Some(hex) => {
                        v.hashes.insert(path.clone(), hex.to_string());
                    }
                    None => r.warn(
                        format!("/hashes/{}", esc(path)),
                        "skipped: expected `sha256:<64 hex>` or `<64 hex>`",
                    ),
                }
            }
        }
        Some(_) => r.warn("/hashes".into(), "expected an object"),
    }
    v.locks = r.strings(root, "locks");

    for (at, m) in r.objects(root, "human_edits") {
        let o = Obj { map: m, at: &at };
        let path = Reader::req_str(o, "path");
        let sha256 = Reader::req_str(o, "sha256");
        let (Some(path), Some(sha256)) = (path, sha256.as_deref().and_then(bare_sha256)) else {
            r.skipped(&at, "`path` and a valid `sha256` are required");
            continue;
        };
        let sha256 = sha256.to_string();
        v.human_edits.push(HumanEdit {
            path,
            sha256,
            previous_sha256: r.opt_sha(o, "previous_sha256"),
            date: r.opt_str(o, "date"),
        });
    }
    for (at, m) in r.objects(root, "external") {
        let o = Obj { map: m, at: &at };
        let path = Reader::req_str(o, "path");
        let sha256 = Reader::req_str(o, "sha256");
        let (Some(path), Some(sha256)) = (path, sha256.as_deref().and_then(bare_sha256)) else {
            r.skipped(&at, "`path` and a valid `sha256` are required");
            continue;
        };
        v.external.push(ExternalFile {
            path,
            sha256: sha256.to_string(),
        });
    }
    v.has_working_doc = r.opt_bool(root, "has_working_doc");
    (v, r.warnings)
}

/// One `candidate_runs` entry; `None` (with a warning) when it has no `run_id`.
fn read_run(r: &mut Reader, o: Obj<'_>) -> Option<CandidateRun> {
    let Some(run_id) = Reader::req_str(o, "run_id") else {
        r.skipped(o.at, "`run_id` is missing or not a string");
        return None;
    };
    let status = r.opt_str(o, "status");
    if let Some(s) = &status
        && !matches!(s.as_str(), "open" | "selected" | "rejected")
    {
        r.warn(
            Reader::ptr(o, "status"),
            format!("`{s}` is not a run status (open, selected or rejected)"),
        );
    }
    let mut files = Vec::new();
    for (at, m) in r.objects(o, "files") {
        let f = Obj { map: m, at: &at };
        let (Some(path), Some(role)) = (Reader::req_str(f, "path"), Reader::req_str(f, "role"))
        else {
            r.skipped(&at, "`path` and `role` are required strings");
            continue;
        };
        files.push(CandidateFile {
            path,
            seed: r.opt_i64(f, "seed"),
            role,
            from: r.opt_str(f, "from"),
            sha256: r.opt_sha(f, "sha256"),
        });
    }
    let signed_off = match Reader::field(o, "signed_off") {
        None => None,
        Some(Value::Object(m)) => {
            let at = Reader::ptr(o, "signed_off");
            let s = Obj { map: m, at: &at };
            let version = Reader::req_str(s, "version").and_then(|v| v.parse().ok());
            match (
                Reader::req_str(s, "by"),
                Reader::req_str(s, "date"),
                version,
            ) {
                (Some(by), Some(date), Some(version)) => Some(SignedOff { by, date, version }),
                _ => {
                    r.warn(
                        at,
                        "ignored: `by`, `date` and a valid `version` are required",
                    );
                    None
                }
            }
        }
        Some(_) => {
            r.warn(Reader::ptr(o, "signed_off"), "expected an object");
            None
        }
    };
    Some(CandidateRun {
        run_id,
        path: r.opt_str(o, "path"),
        status,
        created: r.opt_str(o, "created"),
        target_version: r.opt_version(o, "target_version"),
        files,
        selected: r.strings(o, "selected"),
        signed_off,
        pinned: r.opt_bool(o, "pinned"),
        trashed_utc: r.opt_str(o, "trashed_utc"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CRANE: &str = include_str!(
        "../../ww-spec-tests/fixtures/sample-workspace/projects/sketchbook/assets/chr_paper_crane/chr_paper_crane.manifest.json"
    );
    const LAMP: &str = include_str!(
        "../../ww-spec-tests/fixtures/sample-workspace/projects/sketchbook/assets/prop_signal_lamp/prop_signal_lamp.manifest.json"
    );
    const HEX: &str = "aaae4dbdb0e697350dac691b26928d34a69061cf320b175c257c103fcaa32766";

    #[test] // spec: ARCH-078, CORE-047
    fn hashes_read_with_or_without_prefix_and_are_written_prefixed() {
        assert_eq!(bare_sha256(HEX), Some(HEX));
        assert_eq!(bare_sha256(&format!("sha256:{HEX}")), Some(HEX));
        assert_eq!(bare_sha256(&HEX[..63]), None);
        assert_eq!(bare_sha256(&format!("sha256:{}", &HEX[..63])), None);
        assert_eq!(bare_sha256(&format!("{HEX}0")), None);
        assert_eq!(bare_sha256(&format!("md5:{HEX}")), None);
        assert_eq!(bare_sha256(&format!("SHA256:{HEX}")), None);
        assert_eq!(bare_sha256(&format!("sha256: {HEX}")), None);
        assert_eq!(bare_sha256(&HEX.to_uppercase()), None);
        assert_eq!(bare_sha256(""), None);
        assert_eq!(hash_value(HEX), format!("sha256:{HEX}"));

        // The modern fixture writes `sha256:<hex>`, the legacy one bare hex: both read bare.
        let (crane, _) = Manifest::parse(CRANE).unwrap();
        let view = crane.view();
        assert_eq!(view.hashes.len(), 1);
        assert_eq!(view.hashes["v1.0.0/crane_b.png"], HEX);
        assert!(
            crane.get("hashes").unwrap()["v1.0.0/crane_b.png"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
        let (lamp, _) = Manifest::parse(LAMP).unwrap();
        let view = lamp.view();
        assert_eq!(view.hashes.len(), 5);
        for (path, hex) in &view.hashes {
            assert!(path.starts_with("v1.0.0/"), "{path}");
            assert_eq!(bare_sha256(hex), Some(hex.as_str()), "{path}");
        }
        // A bad hash value is a warning and left out of the view.
        let mut bad = crane.clone();
        bad.set(
            "hashes",
            json!({"v1.0.0/a.png": "sha256:xyz", "v1.0.0/b.png": HEX}),
        );
        let (reparsed, warnings) = Manifest::parse(&bad.to_text()).unwrap();
        assert_eq!(reparsed.view().hashes.len(), 1);
        assert!(
            warnings
                .iter()
                .any(|w| w.pointer == "/hashes/v1.0.0~1a.png"),
            "{warnings:?}"
        );
    }

    #[test] // spec: ARCH-076, CORE-148, CORE-047, CORE-048
    fn view_types_read_the_i5_run_shape() {
        let (mut m, warnings) = Manifest::parse(CRANE).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let v = m.view();
        assert_eq!(v.schema_version, Some(1));
        assert_eq!(v.asset_id, "chr_paper_crane");
        assert_eq!(v.project, "sketchbook");
        assert_eq!(v.asset_type.as_deref(), Some("image"));
        assert_eq!(v.status, Some(Status::Candidate));
        assert_eq!(v.version, Some(Version::new(1, 1, 0)));
        assert_eq!(v.approved_version, Some(Version::new(1, 0, 0)));
        assert_eq!(
            v.creation_uuid.as_deref(),
            Some("6f1c2a3e-8b4d-4c5e-9a7b-1d2e3f405162")
        );
        assert_eq!(
            v.inputs,
            [Input {
                r#ref: "proj:harbor/env_harbor_wall@1.0.0".into(),
                role: Some("reference".into()),
                sha256: Some(
                    "5a9db589b4907d4cfb2fb9a3a1da4efe5b2fa01d1b2aef2fed24ce2124a5a2a2".into()
                ),
            }]
        );
        assert_eq!(v.locks, ["prompts/crane.txt"]);
        assert!(!v.has_working_doc);
        assert!(v.external.is_empty());

        assert_eq!(v.candidate_runs.len(), 2);
        let sel = &v.candidate_runs[0];
        assert_eq!(sel.run_id, "r_20261002_0915");
        assert_eq!(sel.path.as_deref(), Some("candidates/r_20261002_0915/"));
        assert_eq!(sel.status.as_deref(), Some("selected"));
        assert_eq!(sel.created.as_deref(), Some("2026-10-02T09:15:00Z"));
        assert_eq!(sel.target_version, Some(Version::new(1, 0, 0)));
        assert_eq!(sel.selected, ["crane_b.png"]);
        assert_eq!(
            sel.signed_off,
            Some(SignedOff {
                by: "user".into(),
                date: "2026-10-02".into(),
                version: Version::new(1, 0, 0),
            })
        );
        assert!(!sel.pinned);
        assert_eq!(sel.trashed_utc, None);
        assert_eq!(sel.files[1].path, "candidates/r_20261002_0915/crane_b.png");
        assert_eq!(sel.files[1].seed, Some(12));
        assert_eq!(sel.files[1].role, "candidate");
        assert_eq!(sel.files[1].sha256.as_deref(), Some(HEX));
        let open = &v.candidate_runs[1];
        assert_eq!(open.status.as_deref(), Some("open"));
        assert_eq!(open.target_version, Some(Version::new(1, 1, 0)));
        assert!(open.selected.is_empty(), "selected: null reads as none");
        assert_eq!(open.signed_off, None);
        let derived = &open.files[1];
        assert_eq!(derived.role, "derived");
        assert_eq!(derived.seed, None);
        assert_eq!(derived.from.as_deref(), Some("crane_c.png"));
        assert!(derived.sha256.is_some());

        // pinned_inputs, human_edits in both shapes, external, a trashed run.
        m.set(
            "pinned_inputs",
            json!([{"ref": "proj:harbor/env_harbor_wall@1.0.0", "reason": "keeps the silhouette",
                    "by": "user", "date": "2026-10-04"}]),
        );
        m.set(
            "human_edits",
            json!([
                {"path": "v1.0.0/crane_b.png", "sha256": format!("sha256:{HEX}"),
                 "author": "sample-author", "date": "2026-10-04", "note": "hand-painted fold"},
                {"path": "prompts/crane.txt", "sha256": HEX, "previous_sha256": HEX,
                 "date": "2026-10-05"}
            ]),
        );
        m.set(
            "external",
            json!([{"path": "D:/elsewhere/x.png", "sha256": HEX}]),
        );
        m.set("has_working_doc", json!(true));
        m.runs_mut()[0].insert("trashed_utc".into(), json!("2026-10-06T00:00:00Z"));
        m.runs_mut()[0].insert("pinned".into(), json!(true));
        let (m2, warnings) = Manifest::parse(&m.to_text()).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let v = m2.view();
        assert_eq!(
            v.pinned_inputs,
            [PinnedInput {
                r#ref: "proj:harbor/env_harbor_wall@1.0.0".into(),
                reason: Some("keeps the silhouette".into()),
                by: Some("user".into()),
                date: Some("2026-10-04".into()),
            }]
        );
        assert_eq!(
            v.human_edits,
            [
                HumanEdit {
                    path: "v1.0.0/crane_b.png".into(),
                    sha256: HEX.into(),
                    previous_sha256: None,
                    date: Some("2026-10-04".into()),
                },
                HumanEdit {
                    path: "prompts/crane.txt".into(),
                    sha256: HEX.into(),
                    previous_sha256: Some(HEX.into()),
                    date: Some("2026-10-05".into()),
                },
            ]
        );
        assert_eq!(
            v.external,
            [ExternalFile {
                path: "D:/elsewhere/x.png".into(),
                sha256: HEX.into(),
            }]
        );
        assert!(v.has_working_doc);
        assert_eq!(
            v.candidate_runs[0].trashed_utc.as_deref(),
            Some("2026-10-06T00:00:00Z")
        );
        assert!(v.candidate_runs[0].pinned);

        // Wrong types are warnings and defaults, never errors.
        let mut odd = m2.clone();
        odd.set("status", json!("sideways"));
        // Ruling Q7: a leading zero is a bad version, read as a warning.
        odd.set("version", json!("01.0.0"));
        odd.set("approved_version", json!(3));
        odd.set("locks", json!("not a list"));
        odd.set("has_working_doc", json!("yes"));
        odd.set(
            "inputs",
            json!([{"role": "no ref"}, {"ref": "a@1.0.0", "sha256": 5}]),
        );
        odd.runs_mut()[1].insert("files".into(), json!([{"path": 1, "role": "candidate"}]));
        odd.runs_mut()[1].insert("signed_off".into(), json!({"by": "user"}));
        odd.runs_mut()[1].insert("pinned".into(), json!("no"));
        odd.set("asset_id", json!(7));
        let (odd, warnings) = Manifest::parse(&odd.to_text()).expect("still parses");
        let v = odd.view();
        assert_eq!(v.status, None);
        assert_eq!(v.version, None);
        assert_eq!(v.approved_version, None);
        assert!(v.locks.is_empty());
        assert!(!v.has_working_doc);
        assert_eq!(v.asset_id, "");
        assert_eq!(
            v.inputs,
            [Input {
                r#ref: "a@1.0.0".into(),
                role: None,
                sha256: None
            }]
        );
        assert!(v.candidate_runs[1].files.is_empty());
        assert_eq!(v.candidate_runs[1].signed_off, None);
        assert!(!v.candidate_runs[1].pinned);
        let pointers: Vec<&str> = warnings.iter().map(|w| w.pointer.as_str()).collect();
        for p in [
            "/status",
            "/version",
            "/approved_version",
            "/locks",
            "/has_working_doc",
            "/inputs/0",
            "/inputs/1/sha256",
            "/candidate_runs/1/files/0",
            "/candidate_runs/1/signed_off",
            "/candidate_runs/1/pinned",
            "/asset_id",
        ] {
            assert!(pointers.contains(&p), "{p} missing from {pointers:?}");
        }
        // Not a JSON object at all is the only parse error.
        assert!(Manifest::parse("[1, 2]").is_err());
        assert!(Manifest::parse("{\"a\": ").is_err());
    }

    #[test] // spec: ARCH-076, CORE-148
    fn push_refuses_a_value_that_is_not_a_list() {
        let (mut m, _) = Manifest::parse(LAMP).unwrap();
        let before = m.clone();
        // `notes` is a string, `legacy_tool` an object, `budget_usd` a number.
        for key in ["notes", "legacy_tool", "budget_usd", "commercial_use"] {
            assert_eq!(
                m.push(key, json!("x")),
                Err(EditError::NotAList { key: key.into() })
            );
        }
        assert_eq!(m, before, "a refused push changes nothing");
        // `null` becomes a one-item list in place; a missing key goes at the end.
        let at = m
            .as_map()
            .keys()
            .position(|k| k == "status_reason")
            .unwrap();
        m.push("status_reason", json!("why")).unwrap();
        assert_eq!(m.get("status_reason"), Some(&json!(["why"])));
        assert_eq!(
            m.as_map().keys().position(|k| k == "status_reason"),
            Some(at)
        );
        m.push("locks", json!("a")).unwrap();
        m.push("locks", json!("b")).unwrap();
        assert_eq!(m.get("locks"), Some(&json!(["a", "b"])));
        assert_eq!(
            m.as_map().keys().next_back().map(String::as_str),
            Some("locks")
        );
    }

    #[test] // spec: ARCH-076, CORE-148
    fn bom_is_kept_and_duplicate_keys_warn() {
        let with_bom = format!("\u{feff}{CRANE}");
        let (mut m, warnings) = Manifest::parse(&with_bom).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(m.to_text(), with_bom);
        assert_eq!(m.view().asset_id, "chr_paper_crane");
        m.set("status", json!("in_review"));
        assert!(m.to_text().starts_with("\u{feff}{\n"));
        let crlf = with_bom.replace('\n', "\r\n");
        let (m, _) = Manifest::parse(&crlf).unwrap();
        assert_eq!(m.eol(), "\r\n");
        assert_eq!(m.to_text(), crlf);

        let dup =
            "{\"a\": 1, \"b\": {\"x\": 1, \"x\": 2}, \"a\": 3, \"c\": [{\"k/y\": 1, \"k/y\": 2}]}";
        let (m, warnings) = Manifest::parse(dup).unwrap();
        let pointers: Vec<&str> = warnings
            .iter()
            .filter(|w| w.message.starts_with("duplicate key"))
            .map(|w| w.pointer.as_str())
            .collect();
        assert_eq!(pointers, ["/b/x", "/a", "/c/0/k~1y"]);
        // Kept once, at the first position, with the last value.
        assert_eq!(
            serde_json::to_string(m.as_map()).unwrap(),
            "{\"a\":3,\"b\":{\"x\":2},\"c\":[{\"k/y\":2}]}"
        );
    }

    #[test] // spec: CORE-047, CORE-048
    fn new_manifest_has_schema_version_status_and_version() {
        let m = Manifest::new(
            "sketchbook",
            "img_lighthouse",
            "image",
            "0b8e5c1a-2f3d-4e6a-8b9c-0d1e2f3a4b5c",
        );
        let (again, warnings) = Manifest::parse(&m.to_text()).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(again, m);
        let v = m.view();
        assert_eq!(v.schema_version, Some(1));
        assert_eq!(v.status, Some(Status::Planned));
        assert_eq!(v.version, Some(Version::new(1, 0, 0)));
        assert_eq!(v.approved_version, None);
        assert_eq!(v.asset_id, "img_lighthouse");
        assert_eq!(v.project, "sketchbook");
        assert_eq!(v.asset_type.as_deref(), Some("image"));
        assert_eq!(
            v.creation_uuid.as_deref(),
            Some("0b8e5c1a-2f3d-4e6a-8b9c-0d1e2f3a4b5c")
        );
        let keys: Vec<&str> = m.as_map().keys().map(String::as_str).collect();
        assert_eq!(keys[0], "schema_version");
        assert_eq!(m.eol(), "\n");
        assert!(m.to_text().ends_with("}\n"));
    }
}
