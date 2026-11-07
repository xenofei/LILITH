//! `registry/redirects.yaml` (ARCH-125; Appendix H "`registry/redirects.yaml`"): one entry per
//! rename or move of an asset, `redirects: [{from, to, moved_utc, by, actor?}]`, names
//! `<project>/<asset_id>`.
//!
//! [`read_redirects`] reads the file strictly: a file that isn't YAML, whose `redirects` isn't
//! a list, or with an entry whose `from` or `to` isn't a `<project>/<asset_id>` name, whose
//! `moved_utc` is missing or not a scalar (any scalar is kept as its text), whose `by` isn't a
//! string, or whose `actor` isn't `{kind, …}`, is refused as a
//! whole, never read as "no redirects". No file is an empty list. [`append_redirect`] adds one
//! entry with the targeted YAML edit (ARCH-076), so comments, key order and the other entries
//! stay byte for byte; it creates the file when there is none. Who writes a redirect (Task 8's
//! accept-move, Task 11's restore to a new name) computes `by` and `actor` in `tools` and
//! passes them in: this module never names `pipeline`.

use lil_schema::workspace_yaml::read_yaml_mapping;
use lil_schema::{Actor, ActorKind, YamlEdit, YamlValue, is_asset_id, is_slug};
use serde_json::{Map, Value};

use crate::store::{Store, StoreError, Written, WsPath};

/// The redirects file, workspace-relative.
pub const REDIRECTS_YAML: &str = "registry/redirects.yaml";

/// One entry of `registry/redirects.yaml`. `from` and `to` are `(project, asset_id)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirect {
    pub from: (String, String),
    pub to: (String, String),
    /// RFC 3339 UTC, as written.
    pub moved_utc: String,
    /// As a sign-off's `signed_off.by` (Appendix E): `user`, `assistant`, `claude-code`, `cli`,
    /// …
    pub by: String,
    pub actor: Option<Actor>,
}

/// `<project>/<asset_id>`.
pub fn name_of(name: &(String, String)) -> String {
    format!("{}/{}", name.0, name.1)
}

/// `text` as `(project, asset_id)` when it is `<project>/<asset_id>` with a valid slug and id.
pub fn parse_name(text: &str) -> Option<(String, String)> {
    let (project, asset_id) = text.split_once('/')?;
    (is_slug(project) && is_asset_id(asset_id)).then(|| (project.to_string(), asset_id.to_string()))
}

/// The redirects file's path.
pub fn redirects_path() -> WsPath {
    WsPath::parse(REDIRECTS_YAML).expect("registry/redirects.yaml is a workspace path")
}

/// Parses the text of `registry/redirects.yaml` (module docs), entries in file order.
///
/// # Errors
/// Why the file can't be read as redirects, naming the entry (1-based) at fault.
pub fn parse_redirects(text: &str) -> Result<Vec<Redirect>, String> {
    let root = read_yaml_mapping(text, REDIRECTS_YAML)?;
    let list = match root.get("redirects") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(list)) => list,
        Some(_) => return Err("`redirects` is not a list".to_string()),
    };
    list.iter()
        .enumerate()
        .map(|(i, entry)| parse_entry(entry).map_err(|why| format!("entry {}: {why}", i + 1)))
        .collect()
}

fn parse_entry(entry: &Value) -> Result<Redirect, String> {
    let Value::Object(map) = entry else {
        return Err("not a mapping".to_string());
    };
    let text = |key: &str| -> Result<String, String> {
        match map.get(key) {
            Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
            Some(_) => Err(format!("`{key}` is not a non-empty string")),
            None => Err(format!("`{key}` is missing")),
        }
    };
    let name = |key: &str| -> Result<(String, String), String> {
        let value = text(key)?;
        parse_name(&value).ok_or_else(|| {
            format!("`{key}` is {value:?}, not `<project>/<asset_id>` (lowercase, as on disk)")
        })
    };
    let actor = match map.get("actor") {
        None | Some(Value::Null) => None,
        Some(Value::Object(a)) => Some(parse_actor(a)?),
        Some(_) => return Err("`actor` is not a mapping".to_string()),
    };
    Ok(Redirect {
        from: name("from")?,
        to: name("to")?,
        moved_utc: moved_utc(map)?,
        by: text("by")?,
        actor,
    })
}

/// `moved_utc` as written: any scalar is kept as its text (a value that isn't an RFC 3339 UTC
/// time sorts before any that is, so it never refuses the file); a missing key or a list or
/// mapping is refused.
fn moved_utc(map: &Map<String, Value>) -> Result<String, String> {
    match map.get("moved_utc") {
        None => Err("`moved_utc` is missing".to_string()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(Value::Null) => Ok(String::new()),
        Some(v @ (Value::Number(_) | Value::Bool(_))) => Ok(v.to_string()),
        Some(_) => Err("`moved_utc` is a list or a mapping, not a time".to_string()),
    }
}

fn parse_actor(a: &Map<String, Value>) -> Result<Actor, String> {
    let Some(Value::String(kind)) = a.get("kind") else {
        return Err("`actor.kind` is not a string".to_string());
    };
    let id = |key: &str| -> Result<Option<String>, String> {
        match a.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(format!("`actor.{key}` is not a string")),
        }
    };
    Ok(Actor {
        kind: ActorKind::from_wire(kind),
        task_id: id("task_id")?,
        conversation_id: id("conversation_id")?,
        session_id: id("session_id")?,
    })
}

/// Reads `registry/redirects.yaml`: its entries in file order, none when there is no file.
///
/// # Errors
/// `yaml.unsupported_edit` naming the file and why when it isn't redirects (module docs), as
/// any registry file that doesn't parse is; the store's read error.
pub async fn read_redirects(store: &Store) -> Result<Vec<Redirect>, StoreError> {
    let Some(bytes) = store.read(&redirects_path()).await? else {
        return Ok(Vec::new());
    };
    std::str::from_utf8(&bytes)
        .map_err(|_| "it is not UTF-8".to_string())
        .and_then(parse_redirects)
        .map_err(unreadable)
}

/// The error of a redirects file that doesn't parse.
pub(crate) fn unreadable(why: String) -> StoreError {
    StoreError::YamlUnsupported(format!(
        "{REDIRECTS_YAML} doesn't parse ({why}); fix it by hand first"
    ))
}

/// Appends `r` to `registry/redirects.yaml` with the targeted YAML edit (module docs), creating
/// the file when there is none. Inside the store's hash-checked edit, the file as it is must
/// read as redirects, and the edited text must read back with the reader's own rules
/// ([`parse_redirects`]), so the call never writes an entry that would make every redirect
/// unreadable. Nothing is checked against the other entries: a redirect never blocks anything
/// (ARCH-125).
///
/// # Errors
/// `params.invalid` naming the entry and field when `r` wouldn't read back (a name that isn't
/// `<project>/<asset_id>`, an empty `by`, …), with nothing written; `yaml.unsupported_edit`
/// when the file isn't UTF-8, doesn't parse as redirects, or the edit can't be made safely;
/// the store's read and write errors.
pub async fn append_redirect(store: &Store, r: &Redirect) -> Result<Written, StoreError> {
    let mut entries = vec![
        ("from".to_string(), YamlValue::Str(name_of(&r.from))),
        ("to".to_string(), YamlValue::Str(name_of(&r.to))),
        ("moved_utc".to_string(), YamlValue::Str(r.moved_utc.clone())),
        ("by".to_string(), YamlValue::Str(r.by.clone())),
    ];
    if let Some(actor) = &r.actor {
        let mut fields = vec![(
            "kind".to_string(),
            YamlValue::Str(actor.kind.as_str().to_string()),
        )];
        for (key, value) in [
            ("task_id", &actor.task_id),
            ("conversation_id", &actor.conversation_id),
            ("session_id", &actor.session_id),
        ] {
            if let Some(v) = value {
                fields.push((key.to_string(), YamlValue::Str(v.clone())));
            }
        }
        entries.push(("actor".to_string(), YamlValue::Map(fields)));
    }
    // A file that isn't UTF-8 or doesn't parse is `yaml.unsupported_edit` (as `edit_yaml` gave
    // it), never the store's transient `store.io` for text it can't decode. Checked again on the
    // text the edit reads, below.
    read_redirects(store).await?;
    let edits = [YamlEdit::AppendMappingToList {
        path: vec!["redirects".to_string()],
        entries,
    }];
    store
        .edit_text(&redirects_path(), &|text: &str| {
            // A file that doesn't parse is never edited (the edit would keep what's wrong).
            let before = parse_redirects(text).map_err(unreadable)?.len();
            let after = lil_schema::yaml_edit::apply(text, &edits)
                .map_err(|e| StoreError::YamlUnsupported(e.to_string()))?;
            match parse_redirects(&after) {
                Ok(read) if read.len() == before + 1 => Ok(after),
                Ok(_) => Err(StoreError::YamlUnsupported(format!(
                    "{REDIRECTS_YAML}: the appended entry doesn't read back as one redirect"
                ))),
                Err(why) => Err(StoreError::Invalid(format!(
                    "This redirect can't be written: {REDIRECTS_YAML} would no longer read \
                     ({why})."
                ))),
            }
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: ARCH-125
    fn entries_are_read_strictly() {
        let ok = "# c\nredirects:\n  - from: sketchbook/a\n    to: harbor/b\n    moved_utc: 2026-11-01T10:00:00Z\n    by: cli\n    actor: {kind: claude_code, session_id: s1}\n";
        let read = parse_redirects(ok).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].from, ("sketchbook".into(), "a".into()));
        assert_eq!(read[0].to, ("harbor".into(), "b".into()));
        assert_eq!(read[0].moved_utc, "2026-11-01T10:00:00Z");
        assert_eq!(read[0].actor.as_ref().unwrap().kind, ActorKind::ClaudeCode);
        // Any scalar `moved_utc` is kept as its text.
        let odd = "redirects:\n  - {from: s/a, to: s/b, moved_utc: 20261101, by: cli}\n  - {from: s/a, to: s/c, moved_utc: ~, by: cli}\n";
        let read = parse_redirects(odd).unwrap();
        assert_eq!(read[0].moved_utc, "20261101");
        assert_eq!(read[1].moved_utc, "");
        for empty in ["", "# only a comment\n", "redirects:\n", "redirects: []\n"] {
            assert_eq!(parse_redirects(empty).unwrap(), Vec::new(), "{empty:?}");
        }
        for bad in [
            "redirects: [\n",
            "- a\n",
            "redirects: x\n",
            "redirects:\n  - x\n",
            "redirects:\n  - {from: Sketchbook/a, to: s/b, moved_utc: t, by: cli}\n",
            "redirects:\n  - {from: s/a, to: s, moved_utc: t, by: cli}\n",
            "redirects:\n  - {from: s/a, to: s/b/c, moved_utc: t, by: cli}\n",
            "redirects:\n  - {from: s/a, to: s/b, by: cli}\n",
            "redirects:\n  - {from: s/a, to: s/b, moved_utc: [1], by: cli}\n",
            "redirects:\n  - {from: s/a, to: s/b, moved_utc: t, by: 1}\n",
            "redirects:\n  - {from: s/a, to: s/b, moved_utc: t, by: cli, actor: x}\n",
            "redirects:\n  - {from: s/a, to: s/b, moved_utc: t, by: cli, actor: {task_id: x}}\n",
        ] {
            assert!(parse_redirects(bad).is_err(), "{bad:?}");
        }
    }
}
