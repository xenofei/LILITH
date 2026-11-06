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

use crate::attribution::{self, Actor, TrustRecord};
use crate::marks::{self, Flag, Marks};
use crate::status::{self, STATUS_LOG_MAX, Status, StatusLogEntry, Via};
use crate::takes::{self, AutoPick, OverridesAutoPick};
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
#[derive(Debug, Clone, PartialEq, Default)]
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
    /// The version whose sign-off rejected the run (`rejected_at_version`): its 90 days
    /// start at that sign-off (CORE-032).
    pub rejected_at_version: Option<Version>,
    /// Who made the run (CORE-023).
    pub actor: Option<Actor>,
    /// The run's `inputs` (CORE-025).
    pub inputs: Vec<RunInput>,
    /// The version a restore as new version copied (`restored_from`; CORE-177).
    pub restored_from: Option<Version>,
}

/// A `candidate_runs[].inputs` entry: `{ref, role, sections?}`.
#[derive(Debug, Clone, PartialEq)]
pub struct RunInput {
    pub r#ref: String,
    pub role: Option<String>,
    /// Kept as written: its shape is reserved (CORE-025).
    pub sections: Option<Value>,
}

/// A `candidate_runs[].files` entry.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CandidateFile {
    /// Asset-relative (I5).
    pub path: String,
    pub seed: Option<i64>,
    /// `candidate`, `derived`, `repair` or `preview`.
    pub role: String,
    pub from: Option<String>,
    /// Bare hex; absent in pre-2b runs.
    pub sha256: Option<String>,
    /// The take the file belongs to (1-based); none in a run read by the legacy rule (P4b-R3).
    pub take: Option<u32>,
    /// `primary: true`: the file is its take's primary, which holds the take's flag and rating.
    pub primary: bool,
    /// The take's flag, on its primary file.
    pub flag: Option<Flag>,
    /// The take's rating (0–5, 0 unrated), on its primary file.
    pub rating: u8,
    /// Who made the file (CORE-023).
    pub actor: Option<Actor>,
}

/// `candidate_runs[].signed_off`.
#[derive(Debug, Clone, PartialEq)]
pub struct SignedOff {
    /// `user`, `assistant`, `claude-code`, `cli` or `auto`.
    pub by: String,
    pub date: String,
    pub version: Version,
    pub via: Option<Via>,
    /// The take numbers signed off (`takes`), when the sign-off named takes.
    pub takes: Vec<u32>,
    /// The names of the chosen takes' other files.
    pub companions: Vec<String>,
    pub auto_pick: Option<AutoPick>,
    pub overrides_auto_pick: Option<OverridesAutoPick>,
}

/// A `human_edits` entry. I5's `author` and `note` are read when present and otherwise
/// ignored; the core writes `path`, `sha256`, `previous_sha256` and `date`, and `actor` for an
/// agent edit (CORE-034).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HumanEdit {
    pub path: String,
    /// Bare hex.
    pub sha256: String,
    /// Bare hex.
    pub previous_sha256: Option<String>,
    pub date: Option<String>,
    pub actor: Option<Actor>,
    /// `relabelled.utc`: when you marked the entry as an AI edit.
    pub relabelled: Option<String>,
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

/// Sets a `{utc}` object at `key`: `utc` is set inside an existing object, which keeps its
/// other keys; otherwise `{utc}` is written in place (a new key is appended). `None` removes the
/// key by shifting.
pub(crate) fn set_utc_object(map: &mut Map<String, Value>, key: &str, utc: Option<&str>) {
    match (map.get_mut(key), utc) {
        (Some(Value::Object(existing)), Some(utc)) => {
            existing.insert("utc".into(), Value::from(utc));
        }
        (_, Some(utc)) => {
            let mut m = Map::new();
            m.insert("utc".into(), Value::from(utc));
            map.insert(key.to_string(), Value::Object(m));
        }
        (_, None) => {
            map.shift_remove(key);
        }
    }
}

/// Sets `key` in place (a new key is appended), or removes it by shifting when `value` is
/// `None`.
pub(crate) fn set_opt(map: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    match value {
        Some(v) => {
            map.insert(key.to_string(), v);
        }
        None => {
            map.shift_remove(key);
        }
    }
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

    fn root(&self) -> Obj<'_> {
        Obj {
            map: &self.map,
            at: "",
        }
    }

    /// The asset's `tags`, `rating` and `label`, read leniently (see [`marks`]).
    pub fn marks(&self) -> Marks {
        marks::read_marks(&mut Reader::new(), self.root())
    }

    /// Writes the marks whose value differs from [`Manifest::marks`] (the lenient read), each
    /// key in place; no tags, a rating of 0 and no label remove their key by shifting
    /// (Appendix E). A mark whose read value doesn't change is left as written, so a
    /// hand-written value that breaks the rules stays until that mark is edited (P4b-R26: tags
    /// are normalised only when a mark next writes `tags`). Tags are written as given: the
    /// caller normalises them first ([`marks::normalise_tags`]). A rating above 5 is an error
    /// and changes nothing.
    pub fn set_marks(&mut self, m: &Marks) -> Result<(), String> {
        check_rating(m.rating)?;
        let now = self.marks();
        if now.tags != m.tags {
            let tags = (!m.tags.is_empty()).then(|| Value::from(m.tags.clone()));
            set_opt(&mut self.map, "tags", tags);
        }
        if now.rating != m.rating {
            set_opt(&mut self.map, "rating", marks::rating_value(m.rating));
        }
        if now.label != m.label {
            let label = m.label.map(|l| Value::from(l.as_str()));
            set_opt(&mut self.map, "label", label);
        }
        Ok(())
    }

    /// The `status_log` entries the view can read, newest last.
    pub fn status_log(&self) -> Vec<StatusLogEntry> {
        status::read_status_log(&mut Reader::new(), self.root())
    }

    /// Appends `e` to `status_log` and drops the oldest entries beyond 50 (CORE-156). A
    /// missing key gets a one-entry list at the end; `null`, or a value that isn't a list
    /// (which the read warns of), becomes a one-entry list in place.
    pub fn push_status(&mut self, e: StatusLogEntry) {
        let entry = e.to_value();
        match self.map.get_mut("status_log") {
            Some(Value::Array(list)) => {
                list.push(entry);
                let over = list.len().saturating_sub(STATUS_LOG_MAX);
                list.drain(..over);
            }
            Some(other) => *other = Value::Array(vec![entry]),
            None => {
                self.map
                    .insert("status_log".into(), Value::Array(vec![entry]));
            }
        }
    }

    /// The trust record of the asset-relative `path`. `None` when it has none: the suite made
    /// the file and it counts as trusted (P4b-R28). A malformed record reads as untrusted.
    pub fn trust(&self, path: &str) -> Option<TrustRecord> {
        let Some(Value::Object(trust)) = self.map.get("trust") else {
            return None;
        };
        let value = trust.get(path).filter(|v| !v.is_null())?;
        Some(TrustRecord::read(
            &mut Reader::new(),
            &attribution::trust_ptr(path),
            value,
        ))
    }

    /// Sets the trust record of `path`: in place when it has one (its unknown keys stay), else
    /// appended to the `trust` map, which is made at the end when missing. `None` removes the
    /// record by shifting, and the `trust` map with it when that leaves it empty.
    pub fn set_trust(&mut self, path: &str, rec: Option<&TrustRecord>) {
        let Some(rec) = rec else {
            if let Some(Value::Object(trust)) = self.map.get_mut("trust")
                && trust.shift_remove(path).is_some()
                && trust.is_empty()
            {
                self.map.shift_remove("trust");
            }
            return;
        };
        let slot = self
            .map
            .entry("trust")
            .or_insert_with(|| Value::Object(Map::new()));
        if !slot.is_object() {
            // `null` or a malformed value (which the read warns of): replaced in place.
            *slot = Value::Object(Map::new());
        }
        let Value::Object(trust) = slot else {
            unreachable!("made an object above")
        };
        match trust.get_mut(path) {
            Some(Value::Object(existing)) => rec.write_into(existing),
            _ => {
                trust.insert(path.to_string(), rec.to_value());
            }
        }
    }

    /// Sets the flag and rating of take `take` of run `run_id` on the take's primary file
    /// entry (P4b-R3), each key in place: `flag: Some(None)` and `rating: Some(0)` remove the
    /// key by shifting, `None` leaves it. The entry written is the one the view read as the
    /// primary, found by its position among the entries the read keeps, never by its name. An
    /// unknown run or take, a rating above 5, or an entry that can't be identified exactly is
    /// an error and changes nothing. Whether the run is open is the caller's check.
    pub fn set_take_mark(
        &mut self,
        run_id: &str,
        take: u32,
        flag: Option<Option<Flag>>,
        rating: Option<u8>,
    ) -> Result<(), String> {
        if let Some(r) = rating {
            check_rating(r)?;
        }
        let view = self.view();
        // The view keeps every object run with a string `run_id`, in order, so the first such
        // run in the view and the first such raw object are the same entry.
        let run = view
            .candidate_runs
            .iter()
            .find(|r| r.run_id == run_id)
            .ok_or_else(|| format!("the asset has no run `{run_id}`"))?;
        let (t, kept_index) = takes::grouped(&run.files)
            .0
            .into_iter()
            .find(|(t, _)| t.n == take)
            .ok_or_else(|| format!("run `{run_id}` has no take {take}"))?;
        let want_path = run.files[kept_index].path.clone();
        let unclear = || {
            format!(
                "run `{run_id}`: the file entry of take {take}'s primary, `{}`, can't be found \
                 exactly; nothing was changed",
                t.primary
            )
        };
        let entry = self
            .runs_mut()
            .into_iter()
            .find(|r| r.get("run_id").and_then(Value::as_str) == Some(run_id))
            .and_then(|r| r.get_mut("files"))
            .and_then(Value::as_array_mut)
            .and_then(|files| {
                // The entries `read_run` keeps: objects with a string `path` and `role`.
                files
                    .iter_mut()
                    .filter_map(Value::as_object_mut)
                    .filter(|f| {
                        let s = |k: &str| f.get(k).is_some_and(Value::is_string);
                        s("path") && s("role")
                    })
                    .nth(kept_index)
            })
            .filter(|f| f.get("path").and_then(Value::as_str) == Some(want_path.as_str()))
            .ok_or_else(unclear)?;
        if let Some(flag) = flag {
            set_opt(entry, "flag", flag.map(|f| Value::from(f.as_str())));
        }
        if let Some(r) = rating {
            set_opt(entry, "rating", marks::rating_value(r));
        }
        Ok(())
    }

    /// Sets `actor` and `relabelled: {utc}` on the newest `human_edits` entry with this `path`
    /// and SHA-256 (with or without the `sha256:` prefix), each in place: an existing actor
    /// keeps its unknown keys. `false`, and nothing changes, when no entry matches.
    pub fn set_human_edit_actor(
        &mut self,
        path: &str,
        sha256: &str,
        actor: &Actor,
        utc: &str,
    ) -> bool {
        let Some(want) = bare_sha256(sha256) else {
            return false;
        };
        let Some(Value::Array(edits)) = self.map.get_mut("human_edits") else {
            return false;
        };
        let Some(entry) = edits
            .iter_mut()
            .rev()
            .filter_map(Value::as_object_mut)
            .find(|e| {
                e.get("path").and_then(Value::as_str) == Some(path)
                    && e.get("sha256")
                        .and_then(Value::as_str)
                        .and_then(bare_sha256)
                        == Some(want)
            })
        else {
            return false;
        };
        match entry.get_mut("actor") {
            Some(Value::Object(existing)) => actor.write_into(existing),
            _ => {
                entry.insert("actor".into(), actor.to_value());
            }
        }
        set_utc_object(entry, "relabelled", Some(utc));
        true
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
pub(crate) fn esc(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// Collects warnings while the view is read.
pub(crate) struct Reader {
    warnings: Vec<SchemaWarning>,
}

/// A JSON object and its pointer.
#[derive(Clone, Copy)]
pub(crate) struct Obj<'a> {
    pub(crate) map: &'a Map<String, Value>,
    pub(crate) at: &'a str,
}

impl Reader {
    pub(crate) fn new() -> Reader {
        Reader {
            warnings: Vec::new(),
        }
    }

    pub(crate) fn warn(&mut self, pointer: String, message: impl Into<String>) {
        self.warnings.push(SchemaWarning {
            pointer,
            message: message.into(),
        });
    }

    /// The value of `key`, treating `null` as absent.
    pub(crate) fn field<'a>(o: Obj<'a>, key: &str) -> Option<&'a Value> {
        o.map.get(key).filter(|v| !v.is_null())
    }

    pub(crate) fn ptr(o: Obj<'_>, key: &str) -> String {
        format!("{}/{}", o.at, esc(key))
    }

    pub(crate) fn opt_str(&mut self, o: Obj<'_>, key: &str) -> Option<String> {
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
    pub(crate) fn req_str(o: Obj<'_>, key: &str) -> Option<String> {
        Self::field(o, key)?.as_str().map(str::to_string)
    }

    pub(crate) fn opt_version(&mut self, o: Obj<'_>, key: &str) -> Option<Version> {
        let s = self.opt_str(o, key)?;
        match s.parse() {
            Ok(v) => Some(v),
            Err(e) => {
                self.warn(Self::ptr(o, key), e);
                None
            }
        }
    }

    pub(crate) fn opt_sha(&mut self, o: Obj<'_>, key: &str) -> Option<String> {
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

    pub(crate) fn opt_bool(&mut self, o: Obj<'_>, key: &str) -> bool {
        match Self::field(o, key) {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => {
                self.warn(Self::ptr(o, key), "expected true or false");
                false
            }
        }
    }

    pub(crate) fn opt_i64(&mut self, o: Obj<'_>, key: &str) -> Option<i64> {
        let n = Self::field(o, key)?.as_i64();
        if n.is_none() {
            self.warn(Self::ptr(o, key), "expected an integer");
        }
        n
    }

    /// The elements of the list `key` (absent or null: none).
    pub(crate) fn list<'a>(&mut self, o: Obj<'a>, key: &str) -> &'a [Value] {
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
    pub(crate) fn objects<'a>(
        &mut self,
        o: Obj<'a>,
        key: &str,
    ) -> Vec<(String, &'a Map<String, Value>)> {
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

    pub(crate) fn strings(&mut self, o: Obj<'_>, key: &str) -> Vec<String> {
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

    /// The `utc` of a `{utc}` object at `key` (`relabelled`, `marked_trusted`): absent or
    /// `null` is none; not an object, or an object without a string `utc`, is none with a
    /// warning.
    pub(crate) fn opt_utc_object(&mut self, o: Obj<'_>, key: &str) -> Option<String> {
        let at = Self::ptr(o, key);
        match Self::field(o, key)? {
            Value::Object(m) => {
                let utc = Self::req_str(Obj { map: m, at: &at }, "utc");
                if utc.is_none() {
                    self.warn(at, "ignored: `utc` is required");
                }
                utc
            }
            _ => {
                self.warn(at, "expected an object");
                None
            }
        }
    }

    pub(crate) fn skipped(&mut self, at: &str, needs: &str) {
        self.warn(at.to_string(), format!("skipped: {needs}"));
    }
}

/// Reads the typed view, leniently: every problem is a warning and the field's default.
fn read_view(map: &Map<String, Value>) -> (ManifestView, Vec<SchemaWarning>) {
    let mut r = Reader::new();
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
        let relabelled = r.opt_utc_object(o, "relabelled");
        v.human_edits.push(HumanEdit {
            path,
            sha256,
            previous_sha256: r.opt_sha(o, "previous_sha256"),
            date: r.opt_str(o, "date"),
            actor: Actor::read(&mut r, o, "actor"),
            relabelled,
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
    // Read here for their warnings; `Manifest::marks`, `status_log` and `trust` give the values.
    marks::read_marks(&mut r, root);
    status::read_status_log(&mut r, root);
    attribution::read_trust(&mut r, root);
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
        let take = match Reader::field(f, "take") {
            None => None,
            Some(n) => {
                let n = n
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|&n| n >= 1);
                if n.is_none() {
                    r.warn(
                        Reader::ptr(f, "take"),
                        "expected a take number from 1: read as no take",
                    );
                }
                n
            }
        };
        files.push(CandidateFile {
            path,
            seed: r.opt_i64(f, "seed"),
            role,
            from: r.opt_str(f, "from"),
            sha256: r.opt_sha(f, "sha256"),
            take,
            primary: r.opt_bool(f, "primary"),
            flag: marks::read_flag(r, f, "flag"),
            rating: marks::read_rating(r, f, "rating"),
            actor: Actor::read(r, f, "actor"),
        });
    }
    for message in takes::group(&files).1 {
        r.warn(Reader::ptr(o, "files"), message);
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
                (Some(by), Some(date), Some(version)) => Some(SignedOff {
                    by,
                    date,
                    version,
                    via: Via::read(r, s, "via"),
                    takes: read_take_numbers(r, s, "takes"),
                    companions: r.strings(s, "companions"),
                    auto_pick: takes::read_auto_pick(r, s),
                    overrides_auto_pick: takes::read_overrides(r, s),
                }),
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
        rejected_at_version: r.opt_version(o, "rejected_at_version"),
        actor: Actor::read(r, o, "actor"),
        inputs: read_run_inputs(r, o),
        restored_from: r.opt_version(o, "restored_from"),
    })
}

/// A rating a setter writes: 0 to 5, else an error.
fn check_rating(rating: u8) -> Result<(), String> {
    if rating > marks::MAX_RATING {
        return Err(format!("a rating is 0 to 5, not {rating}"));
    }
    Ok(())
}

/// A list of take numbers (each a whole number from 1); others are skipped with a warning.
fn read_take_numbers(r: &mut Reader, o: Obj<'_>, key: &str) -> Vec<u32> {
    let base = Reader::ptr(o, key);
    let mut out = Vec::new();
    for (i, item) in r.list(o, key).iter().enumerate() {
        match item.as_u64().and_then(|n| u32::try_from(n).ok()) {
            Some(n) if n >= 1 => out.push(n),
            _ => r.warn(
                format!("{base}/{i}"),
                "skipped: expected a take number from 1",
            ),
        }
    }
    out
}

/// A run's `inputs`: `{ref, role, sections?}`; an entry without a string `ref` is skipped.
fn read_run_inputs(r: &mut Reader, o: Obj<'_>) -> Vec<RunInput> {
    let mut out = Vec::new();
    for (at, m) in r.objects(o, "inputs") {
        let i = Obj { map: m, at: &at };
        let Some(r#ref) = Reader::req_str(i, "ref") else {
            r.skipped(&at, "`ref` is missing or not a string");
            continue;
        };
        out.push(RunInput {
            r#ref,
            role: r.opt_str(i, "role"),
            sections: Reader::field(i, "sections").cloned(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attribution::{ActorKind, TrustSource, TrustSourceKind, TrustState};
    use crate::marks::Label;
    use crate::status::StatusBy;
    use serde_json::json;

    const CRANE: &str = include_str!(
        "../../lil-spec-tests/fixtures/sample-workspace/projects/sketchbook/assets/chr_paper_crane/chr_paper_crane.manifest.json"
    );
    const LAMP: &str = include_str!(
        "../../lil-spec-tests/fixtures/sample-workspace/projects/sketchbook/assets/prop_signal_lamp/prop_signal_lamp.manifest.json"
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
                via: None,
                takes: Vec::new(),
                companions: Vec::new(),
                auto_pick: None,
                overrides_auto_pick: None,
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
                    ..Default::default()
                },
                HumanEdit {
                    path: "prompts/crane.txt".into(),
                    sha256: HEX.into(),
                    previous_sha256: Some(HEX.into()),
                    date: Some("2026-10-05".into()),
                    ..Default::default()
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

    fn log_entry(utc: &str) -> StatusLogEntry {
        StatusLogEntry {
            from: Some(Status::Candidate),
            to: Status::InReview,
            by: StatusBy::User,
            utc: utc.into(),
            via: None,
            reason: None,
        }
    }

    #[test] // spec: CORE-156
    fn status_log_keeps_the_newest_50() {
        let mut m = Manifest::new(
            "sketchbook",
            "img_dock",
            "image",
            "0b8e5c1a-2f3d-4e6a-8b9c-0d1e2f3a4b5c",
        );
        assert!(m.status_log().is_empty());
        for i in 0..60 {
            m.push_status(log_entry(&format!("2026-11-01T00:{i:02}:00Z")));
        }
        let log = m.status_log();
        assert_eq!(log.len(), 50);
        assert_eq!(log[0].utc, "2026-11-01T00:10:00Z", "the oldest drop");
        assert_eq!(log[49].utc, "2026-11-01T00:59:00Z", "newest last");
        assert_eq!(
            m.get("status_log").and_then(Value::as_array).map(Vec::len),
            Some(50)
        );

        // `null` becomes a list in place; an entry the view skips still counts and drops first.
        let text = json!({"schema_version": 1, "asset_id": "a", "project": "p",
                          "status_log": null, "x_after": 1})
        .to_string();
        let (mut m, _) = Manifest::parse(&text).unwrap();
        m.push_status(log_entry("2026-11-01T00:00:00Z"));
        let keys: Vec<&str> = m.as_map().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "schema_version",
                "asset_id",
                "project",
                "status_log",
                "x_after"
            ]
        );
        let mut raw = vec![json!("hand-written")];
        raw.extend((0..49).map(|i| log_entry(&format!("2026-11-01T01:{i:02}:00Z")).to_value()));
        m.set("status_log", Value::Array(raw));
        assert_eq!(m.status_log().len(), 49);
        m.push_status(log_entry("2026-11-01T02:00:00Z"));
        let list = m.get("status_log").and_then(Value::as_array).unwrap();
        assert_eq!(list.len(), 50);
        assert!(list[0].is_object(), "the hand-written entry was the oldest");
    }

    /// `before` and `after` as text, for an exact comparison that includes key order (the
    /// map's own `==` ignores order).
    fn text_of(map: &Map<String, Value>) -> String {
        serde_json::to_string_pretty(map).unwrap()
    }

    #[test] // spec: ARCH-076, CORE-148
    fn unknown_fields_round_trip_through_every_new_setter() {
        const RUN: &str = "r_20261101_0900";
        let a_png = format!("candidates/{RUN}/a.png");
        let b_png = format!("candidates/{RUN}/b.png");
        let base = json!({
            "schema_version": 1, "asset_id": "img_dock", "project": "sketchbook",
            "x_before_tags": "a", "tags": ["dock"], "x_after_tags": {"k": 1},
            "rating": 2, "x_after_rating": [1], "label": "blue", "x_after_label": true,
            "status_log": [{"x_entry": 1, "from": "planned", "to": "in_progress", "by": "user",
                            "utc": "2026-11-01T08:00:00Z"}],
            "x_after_log": null,
            "candidate_runs": [{"x_run": 1, "run_id": RUN, "status": "open", "files": [
                {"x_file_before": 1, "path": a_png, "role": "candidate", "flag": "pick",
                 "x_file_mid": 2, "rating": 3, "x_file_after": 3},
                {"path": b_png, "role": "candidate"}
            ], "x_run_after": 2}],
            "human_edits": [{"x_edit": 1, "path": "work/a.psd", "sha256": HEX,
                             "actor": {"kind": "owner", "x_actor": 1}, "x_edit_after": 2}],
            "x_before_trust": 1,
            "trust": {
                a_png.clone(): {"x_rec": 1, "state": "untrusted",
                                "sources": [{"kind": "import", "ref": "a.png", "x_src": 1},
                                            {"kind": "smuggled", "ref": "elsewhere"}],
                                "marked_trusted": {"x_mt": 1, "utc": "2026-11-01T00:00:00Z"},
                                "x_rec_after": 2},
                "work/a.psd": {"state": "untrusted", "sources": [{"kind": "collect", "ref": "a.psd"}]}
            },
            "x_last": "end"
        });
        let (m, warnings) = Manifest::parse(&base.to_string()).unwrap();
        let pointers: Vec<&str> = warnings.iter().map(|w| w.pointer.as_str()).collect();
        assert_eq!(
            pointers,
            ["/trust/candidates~1r_20261101_0900~1a.png/sources/1"],
            "only the source of a kind this suite doesn't know"
        );
        let edit = |f: &dyn Fn(&mut Map<String, Value>)| {
            let mut map = m.as_map().clone();
            f(&mut map);
            text_of(&map)
        };
        fn file_at(map: &mut Map<String, Value>, i: usize) -> &mut Map<String, Value> {
            map["candidate_runs"][0]["files"][i]
                .as_object_mut()
                .unwrap()
        }

        // set_marks: changed keys in place, an absent one removed by shifting.
        let mut got = m.clone();
        let mut marks = got.marks();
        marks.tags = vec!["dock".into(), "pier".into()];
        marks.rating = 4;
        marks.label = None;
        got.set_marks(&marks).unwrap();
        let want = edit(&|map| {
            map.insert("tags".into(), json!(["dock", "pier"]));
            map.insert("rating".into(), json!(4));
            map.shift_remove("label");
        });
        assert_eq!(text_of(got.as_map()), want);
        // A rating of 0 removes the key by shifting; the unchanged marks stay untouched.
        let mut got = m.clone();
        let mut marks = got.marks();
        marks.rating = 0;
        got.set_marks(&marks).unwrap();
        let want = edit(&|map| {
            map.shift_remove("rating");
        });
        assert_eq!(text_of(got.as_map()), want);
        // A rating above 5 is refused, and nothing changes.
        let mut same = m.clone();
        marks.rating = 6;
        marks.tags.clear();
        assert!(same.set_marks(&marks).is_err());
        assert_eq!(same, m, "a refused mark changes nothing");
        // A key that wasn't there is appended.
        let mut fresh = Manifest::new("sketchbook", "img_new", "image", "u");
        fresh
            .set_marks(&Marks {
                tags: vec!["Dock".into()],
                rating: 1,
                label: Some(Label::Red),
            })
            .unwrap();
        let keys: Vec<&str> = fresh.as_map().keys().map(String::as_str).collect();
        assert_eq!(keys[keys.len() - 3..], ["tags", "rating", "label"]);

        // push_status: one entry appended to the list in place.
        let mut got = m.clone();
        got.push_status(log_entry("2026-11-01T09:00:00Z"));
        let want = edit(&|map| {
            map["status_log"]
                .as_array_mut()
                .unwrap()
                .push(log_entry("2026-11-01T09:00:00Z").to_value());
        });
        assert_eq!(text_of(got.as_map()), want);

        // set_take_mark: the primary file's flag cleared by shifting, its rating in place.
        let mut got = m.clone();
        got.set_take_mark(RUN, 1, Some(None), Some(5)).unwrap();
        let want = edit(&|map| {
            let f = file_at(map, 0);
            f.shift_remove("flag");
            f.insert("rating".into(), json!(5));
        });
        assert_eq!(text_of(got.as_map()), want);
        // Take 2 (no keys yet): appended to its file entry; a rating of 0 removes the key.
        let mut got = m.clone();
        got.set_take_mark(RUN, 2, Some(Some(Flag::Reject)), Some(0))
            .unwrap();
        let want = edit(&|map| {
            let f = file_at(map, 1);
            f.insert("flag".into(), json!("reject"));
        });
        assert_eq!(text_of(got.as_map()), want);
        let mut same = m.clone();
        assert!(same.set_take_mark(RUN, 3, Some(None), None).is_err());
        assert!(
            same.set_take_mark("r_20261101_0901", 1, None, Some(1))
                .is_err()
        );
        assert!(same.set_take_mark(RUN, 1, None, Some(6)).is_err());
        assert_eq!(same, m, "a refused mark changes nothing");

        // set_human_edit_actor: the actor edited in place (its unknown key kept), `relabelled`
        // appended to the entry.
        let mut got = m.clone();
        let agent = Actor {
            kind: ActorKind::ClaudeCode,
            task_id: None,
            conversation_id: None,
            session_id: None,
        };
        assert!(got.set_human_edit_actor(
            "work/a.psd",
            &hash_value(HEX),
            &agent,
            "2026-11-02T00:00:00Z"
        ));
        let want = edit(&|map| {
            let e = map["human_edits"][0].as_object_mut().unwrap();
            e["actor"]["kind"] = json!("claude_code");
            e.insert("relabelled".into(), json!({"utc": "2026-11-02T00:00:00Z"}));
        });
        assert_eq!(text_of(got.as_map()), want);

        // set_trust: a record edited in place (the unknown keys of the record, of a source and
        // of `marked_trusted` kept, and the source the read skipped kept), a new one appended,
        // one removed by shifting.
        let mut got = m.clone();
        let mut rec = got.trust(&a_png).unwrap();
        rec.state = TrustState::Trusted;
        rec.marked_trusted = Some("2026-11-02T00:00:00Z".into());
        rec.sources.push(TrustSource {
            kind: TrustSourceKind::Derived,
            r#ref: "b.png".into(),
        });
        got.set_trust(&a_png, Some(&rec));
        let derived = TrustRecord {
            state: TrustState::Untrusted,
            sources: vec![TrustSource {
                kind: TrustSourceKind::Derived,
                r#ref: "a.png".into(),
            }],
            marked_trusted: None,
        };
        got.set_trust("candidates/r_20261101_0900/a_cut.png", Some(&derived));
        got.set_trust("work/a.psd", None);
        let want = edit(&|map| {
            let t = map["trust"].as_object_mut().unwrap();
            let r = t[a_png.as_str()].as_object_mut().unwrap();
            r["state"] = json!("trusted");
            r["sources"]
                .as_array_mut()
                .unwrap()
                .push(json!({"kind": "derived", "ref": "b.png"}));
            r["marked_trusted"]["utc"] = json!("2026-11-02T00:00:00Z");
            t.shift_remove("work/a.psd");
            t.insert(
                "candidates/r_20261101_0900/a_cut.png".into(),
                json!({"state": "untrusted", "sources": [{"kind": "derived", "ref": "a.png"}]}),
            );
        });
        assert_eq!(text_of(got.as_map()), want);
        // A source no longer in the record is removed by shifting; the skipped one stays.
        let mut got = m.clone();
        let mut rec = got.trust(&a_png).unwrap();
        rec.sources.clear();
        rec.marked_trusted = None;
        got.set_trust(&a_png, Some(&rec));
        let want = edit(&|map| {
            let r = map["trust"][a_png.as_str()].as_object_mut().unwrap();
            r["sources"].as_array_mut().unwrap().remove(0);
            r.shift_remove("marked_trusted");
        });
        assert_eq!(text_of(got.as_map()), want);
        // Unchanged, the record's text doesn't change.
        let mut got = m.clone();
        let rec = got.trust(&a_png).unwrap();
        got.set_trust(&a_png, Some(&rec));
        assert_eq!(text_of(got.as_map()), text_of(m.as_map()));

        // Every setter's result reads back with no warning, and the text keeps its line ending.
        let crlf = m.to_text().replace('\n', "\r\n");
        let (mut m, _) = Manifest::parse(&crlf).unwrap();
        m.set_marks(&Marks::default()).unwrap();
        m.push_status(log_entry("2026-11-01T09:00:00Z"));
        m.set_take_mark(RUN, 1, Some(Some(Flag::Reject)), Some(1))
            .unwrap();
        assert!(m.set_human_edit_actor("work/a.psd", HEX, &agent, "2026-11-02T00:00:00Z"));
        m.set_trust(&a_png, None);
        let text = m.to_text();
        assert!(!text.replace("\r\n", "").contains('\n'), "CRLF kept");
        let (_, warnings) = Manifest::parse(&text).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
    }
}
