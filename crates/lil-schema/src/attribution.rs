//! Who made a run, a take or an edit, and whether an outside file is trusted (Appendix H
//! "Attribution" and "Trust"; CORE-023, CORE-034, CORE-051, ARCH-060; P4b-R16, P4b-R28).
//!
//! [`Actor`] is the `actor` of `candidate_runs[]`, `candidate_runs[].files[]`, `human_edits[]`
//! and redirects. [`TrustRecord`] is a value of the manifest's top-level `trust` map, keyed by
//! asset-relative path; a file with no entry was made by the suite and counts as trusted.

use serde_json::{Map, Value};

use crate::manifest::{Obj, Reader, esc, set_opt};

/// `actor.kind`. The set is open on read: a kind this suite doesn't know is kept, unchanged,
/// as [`ActorKind::Other`] and written back as it was.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ActorKind {
    Owner,
    Tether,
    Director,
    ClaudeCode,
    Cli,
    Core,
    Other(String),
}

impl ActorKind {
    /// The wire value: `owner`, `tether`, `director`, `claude_code`, `cli`, `core`, or the
    /// unknown value as read.
    pub fn as_str(&self) -> &str {
        match self {
            ActorKind::Owner => "owner",
            ActorKind::Tether => "tether",
            ActorKind::Director => "director",
            ActorKind::ClaudeCode => "claude_code",
            ActorKind::Cli => "cli",
            ActorKind::Core => "core",
            ActorKind::Other(s) => s,
        }
    }

    /// Reads a wire value; never fails.
    pub fn from_wire(s: &str) -> ActorKind {
        match s {
            "owner" => ActorKind::Owner,
            "tether" => ActorKind::Tether,
            "director" => ActorKind::Director,
            "claude_code" => ActorKind::ClaudeCode,
            "cli" => ActorKind::Cli,
            "core" => ActorKind::Core,
            other => ActorKind::Other(other.to_string()),
        }
    }
}

/// `{kind, task_id?, conversation_id?, session_id?}` (P4b-R16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    pub kind: ActorKind,
    pub task_id: Option<String>,
    pub conversation_id: Option<String>,
    pub session_id: Option<String>,
}

impl Actor {
    /// The JSON form: `kind`, then the ids that are set.
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        self.write_into(&mut map);
        Value::Object(map)
    }

    /// Writes the actor into an existing object in place: each key keeps its position, an id
    /// that isn't set is removed by shifting, and keys this suite doesn't know stay.
    pub(crate) fn write_into(&self, map: &mut Map<String, Value>) {
        map.insert("kind".into(), Value::from(self.kind.as_str()));
        for (key, value) in [
            ("task_id", &self.task_id),
            ("conversation_id", &self.conversation_id),
            ("session_id", &self.session_id),
        ] {
            set_opt(map, key, value.as_deref().map(Value::from));
        }
    }

    /// The actor at `key` of `o`, leniently: absent or `null` is none, and an actor without a
    /// string `kind` is none with a warning.
    pub(crate) fn read(r: &mut Reader, o: Obj<'_>, key: &str) -> Option<Actor> {
        let at = Reader::ptr(o, key);
        match Reader::field(o, key)? {
            Value::Object(map) => {
                let a = Obj { map, at: &at };
                let Some(kind) = Reader::req_str(a, "kind") else {
                    r.warn(at.clone(), "ignored: `kind` is required");
                    return None;
                };
                Some(Actor {
                    kind: ActorKind::from_wire(&kind),
                    task_id: r.opt_str(a, "task_id"),
                    conversation_id: r.opt_str(a, "conversation_id"),
                    session_id: r.opt_str(a, "session_id"),
                })
            }
            _ => {
                r.warn(at, "expected an object");
                None
            }
        }
    }
}

/// `TrustRecord.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrustState {
    Trusted,
    Untrusted,
}

impl TrustState {
    /// The wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            TrustState::Trusted => "trusted",
            TrustState::Untrusted => "untrusted",
        }
    }
}

/// Where an untrusted file came from: `import`, `collect`, `inbox` or `derived`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrustSourceKind {
    Import,
    Collect,
    Inbox,
    Derived,
}

impl TrustSourceKind {
    pub const ALL: [TrustSourceKind; 4] = [
        TrustSourceKind::Import,
        TrustSourceKind::Collect,
        TrustSourceKind::Inbox,
        TrustSourceKind::Derived,
    ];

    /// The wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            TrustSourceKind::Import => "import",
            TrustSourceKind::Collect => "collect",
            TrustSourceKind::Inbox => "inbox",
            TrustSourceKind::Derived => "derived",
        }
    }
}

/// A `TrustRecord.sources` entry: `{kind, ref}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustSource {
    pub kind: TrustSourceKind,
    pub r#ref: String,
}

/// `{state, sources: [{kind, ref}], marked_trusted?: {utc}}` (ARCH-060; P4b-R28).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustRecord {
    pub state: TrustState,
    pub sources: Vec<TrustSource>,
    /// `marked_trusted.utc`: when you marked the file as trusted.
    pub marked_trusted: Option<String>,
}

impl TrustRecord {
    /// The JSON form.
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        self.write_into(&mut map);
        Value::Object(map)
    }

    /// Writes the record into an existing object in place (as [`Actor`]'s `write_into`).
    pub(crate) fn write_into(&self, map: &mut Map<String, Value>) {
        map.insert("state".into(), Value::from(self.state.as_str()));
        let sources = self
            .sources
            .iter()
            .map(|s| {
                let mut m = Map::new();
                m.insert("kind".into(), Value::from(s.kind.as_str()));
                m.insert("ref".into(), Value::from(s.r#ref.as_str()));
                Value::Object(m)
            })
            .collect();
        map.insert("sources".into(), Value::Array(sources));
        let marked = self.marked_trusted.as_deref().map(|utc| {
            let mut m = Map::new();
            m.insert("utc".into(), Value::from(utc));
            Value::Object(m)
        });
        set_opt(map, "marked_trusted", marked);
    }

    /// The record `value` at pointer `at`, leniently and on the safe side: a record that isn't
    /// an object, or whose `state` isn't `trusted` or `untrusted`, reads as untrusted with a
    /// warning. A source without a known `kind` and a string `ref` is skipped with a warning.
    pub(crate) fn read(r: &mut Reader, at: &str, value: &Value) -> TrustRecord {
        let mut rec = TrustRecord {
            state: TrustState::Untrusted,
            sources: Vec::new(),
            marked_trusted: None,
        };
        let Value::Object(map) = value else {
            r.warn(at.to_string(), "expected an object: read as untrusted");
            return rec;
        };
        let o = Obj { map, at };
        match Reader::field(o, "state").and_then(Value::as_str) {
            Some("trusted") => rec.state = TrustState::Trusted,
            Some("untrusted") => {}
            _ => r.warn(
                Reader::ptr(o, "state"),
                "expected `trusted` or `untrusted`: read as untrusted",
            ),
        }
        for (src_at, m) in r.objects(o, "sources") {
            let s = Obj {
                map: m,
                at: &src_at,
            };
            let kind = Reader::req_str(s, "kind")
                .and_then(|k| TrustSourceKind::ALL.into_iter().find(|t| t.as_str() == k));
            match (kind, Reader::req_str(s, "ref")) {
                (Some(kind), Some(r#ref)) => rec.sources.push(TrustSource { kind, r#ref }),
                _ => r.skipped(&src_at, "a known `kind` and a string `ref` are required"),
            }
        }
        rec.marked_trusted = match Reader::field(o, "marked_trusted") {
            None => None,
            Some(Value::Object(m)) => {
                let at = Reader::ptr(o, "marked_trusted");
                let utc = Reader::req_str(Obj { map: m, at: &at }, "utc");
                if utc.is_none() {
                    r.warn(at, "ignored: `utc` is required");
                }
                utc
            }
            Some(_) => {
                r.warn(Reader::ptr(o, "marked_trusted"), "expected an object");
                None
            }
        };
        rec
    }
}

/// The pointer of `path`'s record in the `trust` map.
pub(crate) fn trust_ptr(path: &str) -> String {
    format!("/trust/{}", esc(path))
}

/// Reads every record of the manifest's `trust` map, for their warnings.
pub(crate) fn read_trust(r: &mut Reader, root: Obj<'_>) {
    match Reader::field(root, "trust") {
        None => {}
        Some(Value::Object(map)) => {
            for (path, value) in map.iter().filter(|(_, v)| !v.is_null()) {
                TrustRecord::read(r, &trust_ptr(path), value);
            }
        }
        Some(_) => r.warn("/trust".into(), "expected an object"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;
    use serde_json::json;

    const HEX: &str = "aaae4dbdb0e697350dac691b26928d34a69061cf320b175c257c103fcaa32766";

    #[test] // spec: CORE-023, CORE-034
    fn actor_kind_reads_an_unknown_value_unchanged() {
        use ActorKind::*;
        for (wire, kind) in [
            ("owner", Owner),
            ("tether", Tether),
            ("director", Director),
            ("claude_code", ClaudeCode),
            ("cli", Cli),
            ("core", Core),
        ] {
            assert_eq!(ActorKind::from_wire(wire), kind);
            assert_eq!(kind.as_str(), wire);
        }
        // An open set on read: kept unchanged, and written back as it was.
        assert_eq!(
            ActorKind::from_wire("night_shift"),
            Other("night_shift".into())
        );
        assert_eq!(Other("night_shift".into()).as_str(), "night_shift");
        // The status log's `claude-code` is not an actor kind.
        assert_eq!(
            ActorKind::from_wire("claude-code"),
            Other("claude-code".into())
        );

        let text = json!({
            "schema_version": 1, "asset_id": "img_dock", "project": "sketchbook",
            "candidate_runs": [{"run_id": "r_20261101_0900",
                "actor": {"kind": "night_shift", "session_id": "s-1", "x_extra": 1},
                "files": [{"path": "candidates/r_20261101_0900/a.png", "role": "candidate",
                           "actor": {"kind": "claude_code", "task_id": "t-7"}}]}],
            "human_edits": [{"path": "work/a.psd", "sha256": HEX, "date": "2026-11-01"}]
        })
        .to_string();
        let (mut m, warnings) = Manifest::parse(&text).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let run = &m.view().candidate_runs[0];
        assert_eq!(
            run.actor,
            Some(Actor {
                kind: Other("night_shift".into()),
                task_id: None,
                conversation_id: None,
                session_id: Some("s-1".into()),
            })
        );
        assert_eq!(
            run.files[0]
                .actor
                .as_ref()
                .map(|a| (&a.kind, a.task_id.as_deref())),
            Some((&ClaudeCode, Some("t-7")))
        );

        let odd = Actor {
            kind: Other("night_shift".into()),
            task_id: None,
            conversation_id: Some("c-2".into()),
            session_id: None,
        };
        assert!(m.set_human_edit_actor("work/a.psd", HEX, &odd, "2026-11-02T08:00:00Z"));
        let (again, warnings) = Manifest::parse(&m.to_text()).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let edit = &again.view().human_edits[0];
        assert_eq!(edit.actor.as_ref(), Some(&odd));
        assert_eq!(edit.relabelled.as_deref(), Some("2026-11-02T08:00:00Z"));
        // No entry with that path and hash: nothing changes.
        let before = again.clone();
        let mut again = again;
        assert!(!again.set_human_edit_actor("work/b.psd", HEX, &odd, "2026-11-02T08:00:00Z"));
        assert!(!again.set_human_edit_actor("work/a.psd", &HEX[..63], &odd, "x"));
        assert_eq!(again, before);
        // An actor without a kind is a warning and no actor.
        let text = json!({"schema_version": 1, "asset_id": "a", "project": "p",
                          "candidate_runs": [{"run_id": "r_20261101_0900",
                                              "actor": {"session_id": "s"}}]})
        .to_string();
        let (m, warnings) = Manifest::parse(&text).unwrap();
        assert_eq!(m.view().candidate_runs[0].actor, None);
        assert!(
            warnings
                .iter()
                .any(|w| w.pointer == "/candidate_runs/0/actor"),
            "{warnings:?}"
        );
    }

    #[test] // spec: ARCH-060
    fn trust_records_read_and_write_and_a_bad_record_reads_untrusted() {
        let mut m = Manifest::new(
            "sketchbook",
            "img_dock",
            "image",
            "0b8e5c1a-2f3d-4e6a-8b9c-0d1e2f3a4b5c",
        );
        // No entry: made by the suite, so trusted (P4b-R28).
        assert_eq!(m.trust("candidates/r_20261101_0900/a.png"), None);
        let rec = TrustRecord {
            state: TrustState::Untrusted,
            sources: vec![TrustSource {
                kind: TrustSourceKind::Import,
                r#ref: "a.png".into(),
            }],
            marked_trusted: None,
        };
        m.set_trust("candidates/r_20261101_0900/a.png", Some(&rec));
        assert_eq!(
            m.as_map().keys().next_back().map(String::as_str),
            Some("trust")
        );
        let (again, warnings) = Manifest::parse(&m.to_text()).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            again.trust("candidates/r_20261101_0900/a.png"),
            Some(rec.clone())
        );
        assert_eq!(
            again.get("trust"),
            Some(&json!({"candidates/r_20261101_0900/a.png":
                {"state": "untrusted", "sources": [{"kind": "import", "ref": "a.png"}]}}))
        );
        // Removing the only record removes the map.
        m.set_trust("candidates/r_20261101_0900/a.png", None);
        assert_eq!(m.get("trust"), None);

        // A state this suite doesn't know, or a record that isn't an object, reads as untrusted.
        let text = json!({"schema_version": 1, "asset_id": "a", "project": "p", "trust": {
            "a.png": {"state": "maybe", "sources": [{"kind": "smuggled", "ref": "x"},
                                                   {"kind": "derived", "ref": "b.png"}]},
            "b.png": "trusted",
            "c.png": {"state": "trusted", "sources": [], "marked_trusted": {"utc": "2026-11-01T00:00:00Z"}}
        }})
        .to_string();
        let (m, warnings) = Manifest::parse(&text).unwrap();
        let a = m.trust("a.png").unwrap();
        assert_eq!(a.state, TrustState::Untrusted);
        assert_eq!(
            a.sources,
            [TrustSource {
                kind: TrustSourceKind::Derived,
                r#ref: "b.png".into()
            }]
        );
        assert_eq!(m.trust("b.png").unwrap().state, TrustState::Untrusted);
        let c = m.trust("c.png").unwrap();
        assert_eq!(c.state, TrustState::Trusted);
        assert_eq!(c.marked_trusted.as_deref(), Some("2026-11-01T00:00:00Z"));
        let pointers: Vec<&str> = warnings.iter().map(|w| w.pointer.as_str()).collect();
        for p in [
            "/trust/a.png/state",
            "/trust/a.png/sources/0",
            "/trust/b.png",
        ] {
            assert!(pointers.contains(&p), "{p} missing from {pointers:?}");
        }
    }
}
