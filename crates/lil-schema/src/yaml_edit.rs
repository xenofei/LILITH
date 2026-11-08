//! Targeted YAML edits that keep comments, order and formatting (ARCH-076): `lilith.yaml`,
//! `project.yaml` and `registry/*.yaml`.
//!
//! The edits are line edits on block-style YAML. Before and after each edit the text is parsed
//! with `yaml-rust2`, and the edit is accepted only when the new document equals the old one
//! with exactly that change, so an edit never changes anything else. Targets the line editor
//! can't change safely (anchors, aliases, tags, flow mappings, block scalars, multi-line
//! scalars) are refused with [`YamlEditError::Unsupported`] (`yaml.unsupported_edit`) and the
//! caller keeps its text. New lines use the file's line ending (that of its first line break).
//!
//! A missing top-level key is appended at the end of the document (`key: value`, or
//! `key: [item]` for [`YamlEdit::AppendToList`], or a block list of one mapping for
//! [`YamlEdit::AppendMappingToList`]); a missing nested key is `Unsupported`.
//!
//! A leading UTF-8 byte-order mark is set aside before the text is scanned and written back in
//! front of the result.

use yaml_rust2::{Yaml, YamlLoader};

use crate::manifest::BOM;

/// One edit. `path` is the chain of mapping keys from the document root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YamlEdit {
    /// Sets the key to the string `value`, written plain when that reads back as the same
    /// string and double-quoted otherwise. A trailing comment is kept in its column. The
    /// target may be a string or null; an existing integer, float or boolean is
    /// `Unsupported` (the edit writes strings only and never retypes a value).
    SetScalar { path: Vec<String>, value: String },
    /// Appends the string `item` to a block or flow list (`[]` included). A key whose value is
    /// null (empty, `~` or `null`) becomes `key: [item]`.
    AppendToList { path: Vec<String>, item: String },
    /// Replaces the value of the key (whatever it was) with a block mapping of these scalar
    /// entries, in this order. The key's block is rewritten whole, so comments inside it go; the
    /// comments and blank lines after it stay. A missing top-level key is appended at the end
    /// of the document. The machine-detected `hardware:` block (CORE-067) is written this way,
    /// because its values are numbers, which a scalar edit refuses to retype.
    SetMapping {
        path: Vec<String>,
        entries: Vec<(String, YamlValue)>,
    },
    /// Appends a mapping of these entries, in this order, to a list (`registry/redirects.yaml`
    /// and the other registry lists of records). In a block list the item is written as a
    /// block mapping after the list's last item, at the list's indent; in a flow list (`[]`
    /// included), or for a `~` or `null` value, as a flow mapping. A key with no value and
    /// nothing below it gets a block list below it; a missing top-level key is appended at the
    /// end of the document as a block list. The entries may not be empty.
    AppendMappingToList {
        path: Vec<String>,
        entries: Vec<(String, YamlValue)>,
    },
    /// Replaces item `index` (0-based) of the block list at `path` with a block mapping of these
    /// entries, at the list's indent (`registry/collections.yaml`). The item's own lines are
    /// rewritten (its first line to its last content line, so comments inside it go); the
    /// comments and blank lines before and after it stay. A flow list is `Unsupported`. The
    /// entries may not be empty.
    SetListItem {
        path: Vec<String>,
        index: usize,
        entries: Vec<(String, YamlValue)>,
    },
    /// Removes item `index` (0-based) of the block list at `path`: its first line to its last
    /// content line; the comments and blank lines around it stay. Removing the only item
    /// leaves `key: []`. A flow list is `Unsupported`.
    RemoveListItem { path: Vec<String>, index: usize },
}

/// A scalar [`YamlEdit::SetMapping`] writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YamlValue {
    /// A string, written plain when that reads back as the same string and double-quoted
    /// otherwise.
    Str(String),
    Int(i64),
    Bool(bool),
    /// A nested mapping of these entries, in this order (a block mapping below its key, or a
    /// flow mapping inside a flow item).
    Map(Vec<(String, YamlValue)>),
    /// A list of these values: a block list below its key (`[]` when empty), or a flow list
    /// inside a flow item or another list.
    List(Vec<YamlValue>),
}

/// Why an edit was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YamlEditError {
    /// The target is a form the line editor doesn't change (`yaml.unsupported_edit`).
    Unsupported(String),
    /// The text isn't valid YAML.
    Invalid(String),
}

impl std::fmt::Display for YamlEditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            YamlEditError::Unsupported(m) => write!(f, "unsupported YAML edit: {m}"),
            YamlEditError::Invalid(m) => write!(f, "invalid YAML: {m}"),
        }
    }
}

/// Applies `edits` in order and returns the new text.
pub fn apply(text: &str, edits: &[YamlEdit]) -> Result<String, YamlEditError> {
    match text.strip_prefix(BOM) {
        Some(body) => apply_body(body, edits).map(|out| format!("{BOM}{out}")),
        None => apply_body(text, edits),
    }
}

/// [`apply`] on text without a byte-order mark.
fn apply_body(text: &str, edits: &[YamlEdit]) -> Result<String, YamlEditError> {
    let mut doc = parse_doc(text).map_err(|e| match e {
        Parse::Invalid(m) => YamlEditError::Invalid(m),
        Parse::NotMapping(m) => YamlEditError::Unsupported(m),
    })?;
    let mut text = text.to_string();
    for edit in edits {
        let new_text = edit_text(&text, edit)?;
        let mut expected = doc.clone();
        edit_value(&mut expected, edit)?;
        let got = parse_doc(&new_text).map_err(|_| {
            unsupported(edit, "the line edit would not leave valid YAML".to_string())
        })?;
        if got != expected {
            return Err(unsupported(
                edit,
                "the line edit would change more than the target".to_string(),
            ));
        }
        text = new_text;
        doc = got;
    }
    Ok(text)
}

/// `value` as a YAML scalar: plain when it reads back as the same string, else double-quoted.
pub(crate) fn scalar(value: &str) -> String {
    if plain_ok(value) {
        value.to_string()
    } else {
        // JSON string escapes are all valid YAML double-quoted escapes.
        serde_json::to_string(value).expect("a string serialises")
    }
}

/// Whether `value` can be written plain, in block and in flow context, and read back as the
/// same string.
fn plain_ok(value: &str) -> bool {
    if value.is_empty()
        || value != value.trim()
        || value.chars().any(char::is_control)
        || value.contains(" #")
        || value.contains(": ")
        || value.ends_with(':')
        || value.contains([',', '[', ']', '{', '}'])
        || value.starts_with([
            '-', '?', ':', '#', '&', '*', '!', '|', '>', '\'', '"', '%', '@', '`',
        ])
    {
        return false;
    }
    let want = Yaml::String(value.to_string());
    let reads_back = |doc: &str, f: &dyn Fn(&Yaml) -> Option<Yaml>| {
        YamlLoader::load_from_str(doc)
            .ok()
            .and_then(|d| d.first().and_then(f))
            .is_some_and(|v| v == want)
    };
    let key = Yaml::String("k".into());
    reads_back(&format!("k: {value}\n"), &|d| {
        d.as_hash()?.get(&key).cloned()
    }) && reads_back(
        &format!("k: [{value}]\n"),
        &|d| match d.as_hash()?.get(&key)? {
            Yaml::Array(a) if a.len() == 1 => Some(a[0].clone()),
            _ => None,
        },
    )
}

fn unsupported(edit: &YamlEdit, why: String) -> YamlEditError {
    let path = match edit {
        YamlEdit::SetScalar { path, .. }
        | YamlEdit::AppendToList { path, .. }
        | YamlEdit::SetMapping { path, .. }
        | YamlEdit::AppendMappingToList { path, .. }
        | YamlEdit::SetListItem { path, .. }
        | YamlEdit::RemoveListItem { path, .. } => path.join("."),
    };
    YamlEditError::Unsupported(format!("`{path}`: {why}"))
}

enum Parse {
    Invalid(String),
    NotMapping(String),
}

/// The document's root mapping (an empty or comment-only document is an empty mapping).
fn parse_doc(text: &str) -> Result<Yaml, Parse> {
    let docs = YamlLoader::load_from_str(text).map_err(|e| Parse::Invalid(e.to_string()))?;
    match docs.as_slice() {
        [] | [Yaml::Null] => Ok(Yaml::Hash(Default::default())),
        [Yaml::Hash(_)] => Ok(docs.into_iter().next().expect("one document")),
        [_] => Err(Parse::NotMapping("the document is not a mapping".into())),
        _ => Err(Parse::NotMapping("the file holds several documents".into())),
    }
}

/// The edit applied to the parsed document: what the edited text must parse to.
fn edit_value(doc: &mut Yaml, edit: &YamlEdit) -> Result<(), YamlEditError> {
    let (path, new) = match edit {
        YamlEdit::SetScalar { path, value } => (path, Yaml::String(value.clone())),
        YamlEdit::AppendToList { path, item } => (path, Yaml::String(item.clone())),
        YamlEdit::SetMapping { path, entries }
        | YamlEdit::AppendMappingToList { path, entries } => (path, mapping_yaml(entries)),
        YamlEdit::SetListItem { path, index, .. } | YamlEdit::RemoveListItem { path, index } => {
            return edit_list_item(doc, edit, path, *index);
        }
    };
    let Some((last, parents)) = path.split_last() else {
        return Err(unsupported(edit, "an empty path".into()));
    };
    let mut cur = doc;
    for k in parents {
        cur = match cur {
            Yaml::Hash(h) => h
                .get_mut(&Yaml::String(k.clone()))
                .ok_or_else(|| unsupported(edit, format!("no key `{k}`")))?,
            _ => return Err(unsupported(edit, format!("`{k}` is not in a mapping"))),
        };
    }
    let Yaml::Hash(h) = cur else {
        return Err(unsupported(edit, "the parent is not a mapping".into()));
    };
    let key = Yaml::String(last.clone());
    match (edit, h.get_mut(&key)) {
        (YamlEdit::SetScalar { .. }, Some(Yaml::Integer(_) | Yaml::Real(_) | Yaml::Boolean(_))) => {
            return Err(unsupported(
                edit,
                "the value is a number or a boolean; a scalar edit writes strings only".into(),
            ));
        }
        (YamlEdit::SetScalar { .. } | YamlEdit::SetMapping { .. }, Some(v)) => *v = new,
        (
            YamlEdit::AppendToList { .. } | YamlEdit::AppendMappingToList { .. },
            Some(Yaml::Array(a)),
        ) => a.push(new),
        (
            YamlEdit::AppendToList { .. } | YamlEdit::AppendMappingToList { .. },
            Some(v @ Yaml::Null),
        ) => *v = Yaml::Array(vec![new]),
        (YamlEdit::AppendToList { .. } | YamlEdit::AppendMappingToList { .. }, Some(_)) => {
            return Err(unsupported(edit, "the value is not a list".into()));
        }
        (YamlEdit::SetScalar { .. } | YamlEdit::SetMapping { .. }, None) => {
            // `replace` keeps an existing key in place; here the key is new and goes last.
            h.replace(key, new);
        }
        (YamlEdit::AppendToList { .. } | YamlEdit::AppendMappingToList { .. }, None) => {
            h.replace(key, Yaml::Array(vec![new]));
        }
        (YamlEdit::SetListItem { .. } | YamlEdit::RemoveListItem { .. }, _) => {
            unreachable!("list item edits return above")
        }
    }
    Ok(())
}

/// [`YamlEdit::SetListItem`] and [`YamlEdit::RemoveListItem`] applied to the parsed document.
fn edit_list_item(
    doc: &mut Yaml,
    edit: &YamlEdit,
    path: &[String],
    index: usize,
) -> Result<(), YamlEditError> {
    let mut cur = doc;
    for k in path {
        cur = match cur {
            Yaml::Hash(h) => h
                .get_mut(&Yaml::String(k.clone()))
                .ok_or_else(|| unsupported(edit, format!("no key `{k}`")))?,
            _ => return Err(unsupported(edit, format!("`{k}` is not in a mapping"))),
        };
    }
    let Yaml::Array(items) = cur else {
        return Err(unsupported(edit, "the value is not a list".into()));
    };
    if index >= items.len() {
        return Err(unsupported(
            edit,
            format!("the list has no item {index} (0-based)"),
        ));
    }
    match edit {
        YamlEdit::SetListItem { entries, .. } => items[index] = mapping_yaml(entries),
        _ => {
            items.remove(index);
        }
    }
    Ok(())
}

/// The parsed form of `value`.
fn value_yaml(value: &YamlValue) -> Yaml {
    match value {
        YamlValue::Str(s) => Yaml::String(s.clone()),
        YamlValue::Int(n) => Yaml::Integer(*n),
        YamlValue::Bool(b) => Yaml::Boolean(*b),
        YamlValue::Map(inner) => mapping_yaml(inner),
        YamlValue::List(items) => Yaml::Array(items.iter().map(value_yaml).collect()),
    }
}

/// The parsed form of a mapping of `entries`.
fn mapping_yaml(entries: &[(String, YamlValue)]) -> Yaml {
    let mut mapping = yaml_rust2::yaml::Hash::new();
    for (key, value) in entries {
        mapping.insert(Yaml::String(key.clone()), value_yaml(value));
    }
    Yaml::Hash(mapping)
}

/// `entries` as the lines of a block mapping at indent `pad`, each ending with `eol`; a nested
/// mapping goes below its key, two spaces deeper (an empty one is `{}`), and a list below its
/// key as a block list two spaces deeper (an empty one is `[]`).
fn block_entries(entries: &[(String, YamlValue)], pad: usize, eol: &str) -> String {
    let indent = " ".repeat(pad);
    let mut out = String::new();
    for (key, value) in entries {
        let key = scalar(key);
        match value {
            YamlValue::Map(inner) if inner.is_empty() => {
                out.push_str(&format!("{indent}{key}: {{}}{eol}"));
            }
            YamlValue::Map(inner) => {
                out.push_str(&format!("{indent}{key}:{eol}"));
                out.push_str(&block_entries(inner, pad + 2, eol));
            }
            YamlValue::List(items) if items.is_empty() => {
                out.push_str(&format!("{indent}{key}: []{eol}"));
            }
            YamlValue::List(items) => {
                out.push_str(&format!("{indent}{key}:{eol}"));
                for item in items {
                    match item {
                        YamlValue::Map(inner) if !inner.is_empty() => {
                            out.push_str(&block_item(inner, pad + 2, eol));
                        }
                        other => out.push_str(&format!("{indent}  - {}{eol}", flow_value(other))),
                    }
                }
            }
            other => out.push_str(&format!("{indent}{key}: {}{eol}", flow_value(other))),
        }
    }
    out
}

/// `value` as it is written on one line: a scalar ([`scalar`] quotes a string holding a flow
/// indicator), or a flow mapping or list.
fn flow_value(value: &YamlValue) -> String {
    match value {
        YamlValue::Str(s) => scalar(s),
        YamlValue::Int(n) => n.to_string(),
        YamlValue::Bool(b) => b.to_string(),
        YamlValue::Map(inner) => flow_mapping(inner),
        YamlValue::List(items) => {
            let items: Vec<String> = items.iter().map(flow_value).collect();
            format!("[{}]", items.join(", "))
        }
    }
}

/// `entries` as a one-line flow mapping (`{a: b, c: {d: e}}`).
fn flow_mapping(entries: &[(String, YamlValue)]) -> String {
    let items: Vec<String> = entries
        .iter()
        .map(|(key, value)| format!("{}: {}", scalar(key), flow_value(value)))
        .collect();
    format!("{{{}}}", items.join(", "))
}

/// `entries` (not empty) as a block list item at indent `seq_indent`: `- ` and the first entry
/// on the item's line, the others below it, two spaces deeper than the dash.
fn block_item(entries: &[(String, YamlValue)], seq_indent: usize, eol: &str) -> String {
    let body = block_entries(entries, seq_indent + 2, eol);
    format!("{}- {}", " ".repeat(seq_indent), &body[seq_indent + 2..])
}

/// A line: `start..end` is its content without the line break, `next` the next line's start.
#[derive(Debug, Clone, Copy)]
struct Line {
    start: usize,
    end: usize,
    next: usize,
}

fn lines_of(text: &str) -> Vec<Line> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let (end, next) = match text[start..].find('\n') {
            Some(i) => {
                let nl = start + i;
                let end = if text[..nl].ends_with('\r') {
                    nl - 1
                } else {
                    nl
                };
                (end, nl + 1)
            }
            None => (text.len(), text.len()),
        };
        out.push(Line { start, end, next });
        start = next;
    }
    out
}

/// The file's line ending: that of its first line break, else `\n`.
fn eol_of(text: &str) -> &'static str {
    match text.find('\n') {
        Some(i) if text[..i].ends_with('\r') => "\r\n",
        _ => "\n",
    }
}

fn indent(text: &str, l: Line) -> usize {
    text[l.start..l.end]
        .bytes()
        .take_while(|&b| b == b' ')
        .count()
}

/// The line without its indent.
fn body(text: &str, l: Line) -> &str {
    &text[l.start + indent(text, l)..l.end]
}

/// A line that holds YAML content (not blank, a comment, or a document marker).
fn is_content(text: &str, l: Line) -> bool {
    let b = body(text, l).trim_end();
    !(b.is_empty() || b.starts_with('#') || b == "---" || b == "..." || b.starts_with('%'))
}

fn is_seq_item(text: &str, l: Line) -> bool {
    let b = body(text, l);
    b == "-" || b.starts_with("- ") || b.starts_with("-\t")
}

/// A mapping key on a line body: the key and the byte offset just after its `:`.
fn parse_key(b: &str) -> Option<(String, usize)> {
    let after_colon = |s: &str, at: usize| -> Option<usize> {
        let rest = &s[at..];
        let ws = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        let rest = &rest[ws..];
        let colon = rest.strip_prefix(':')?;
        (colon.is_empty() || colon.starts_with([' ', '\t'])).then_some(at + ws + 1)
    };
    let quote = b.chars().next()?;
    if quote == '"' || quote == '\'' {
        let close = closing_quote(b, quote)?;
        let raw = &b[..=close];
        let key = YamlLoader::load_from_str(raw)
            .ok()?
            .first()?
            .as_str()?
            .to_string();
        return Some((key, after_colon(b, close + 1)?));
    }
    if b.starts_with([
        '-', '?', '&', '*', '!', '[', '{', '|', '>', '#', '%', '@', '`',
    ]) {
        return None;
    }
    let bytes = b.as_bytes();
    let colon = (0..bytes.len()).find(|&i| {
        bytes[i] == b':' && matches!(bytes.get(i + 1), None | Some(b' ') | Some(b'\t'))
    })?;
    let key = b[..colon].trim_end();
    if key.is_empty() || key.contains(" #") {
        return None;
    }
    Some((key.to_string(), colon + 1))
}

/// The byte offset of the quote that closes the quoted scalar starting at `s[0]`.
fn closing_quote(s: &str, quote: char) -> Option<usize> {
    let b = s.as_bytes();
    let q = quote as u8;
    let mut i = 1;
    while i < b.len() {
        if q == b'"' && b[i] == b'\\' {
            i += 2;
            continue;
        }
        if b[i] == q {
            if q == b'\'' && b.get(i + 1) == Some(&b'\'') {
                i += 2;
                continue;
            }
            return Some(i);
        }
        i += 1;
    }
    None
}

/// A key found on a line.
#[derive(Debug, Clone, Copy)]
struct Hit {
    line: usize,
    indent: usize,
    /// Absolute offset just after the `:`.
    colon: usize,
    /// Absolute offset of the value (after the spaces), or of the comment, or the line end.
    value: usize,
    /// The line holds no value after the key (only spaces and maybe a comment).
    empty: bool,
}

/// Looks for `key` among the lines `from..to`, at the indent of the first content line there.
fn find_key(text: &str, lines: &[Line], from: usize, to: usize, key: &str) -> Option<Hit> {
    let level = (from..to)
        .find(|&i| is_content(text, lines[i]))
        .map(|i| indent(text, lines[i]))?;
    for (i, &l) in lines.iter().enumerate().take(to).skip(from) {
        if !is_content(text, l) || indent(text, l) != level {
            continue;
        }
        let Some((k, colon)) = parse_key(body(text, l)) else {
            continue;
        };
        if k != key {
            continue;
        }
        let colon = l.start + level + colon;
        let rest = &text[colon..l.end];
        let ws = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        let value = colon + ws;
        let empty = text[value..l.end].is_empty() || text[value..l.end].starts_with('#');
        return Some(Hit {
            line: i,
            indent: level,
            colon,
            value,
            empty,
        });
    }
    None
}

/// The first line after `hit` that is no longer inside its block.
fn block_end(text: &str, lines: &[Line], hit: Hit) -> usize {
    (hit.line + 1..lines.len())
        .find(|&i| is_content(text, lines[i]) && indent(text, lines[i]) <= hit.indent)
        .unwrap_or(lines.len())
}

fn has_content(text: &str, lines: &[Line], from: usize, to: usize) -> bool {
    (from..to).any(|i| is_content(text, lines[i]))
}

/// Why a value starting with `c` can't be edited as the requested kind, if it can't.
fn refuse_value(c: char) -> Option<&'static str> {
    match c {
        '&' => Some("the value has an anchor"),
        '*' => Some("the value is an alias"),
        '!' => Some("the value has a tag"),
        '{' => Some("the value is a flow mapping"),
        '|' | '>' => Some("the value is a block scalar"),
        '%' | '@' | '`' => Some("the value starts with a reserved character"),
        _ => None,
    }
}

/// Walks down the block mappings named by `parents` and returns the line range of the last
/// one's entries.
fn descend(
    text: &str,
    lines: &[Line],
    parents: &[String],
    fail: &dyn Fn(&str) -> YamlEditError,
) -> Result<(usize, usize), YamlEditError> {
    let (mut from, mut to) = (0, lines.len());
    for k in parents {
        let Some(hit) = find_key(text, lines, from, to, k) else {
            return Err(fail(&format!("no key `{k}` (a missing nested key)")));
        };
        if !hit.empty {
            let c = text[hit.value..].chars().next().unwrap_or(' ');
            return Err(fail(
                refuse_value(c).unwrap_or("a parent is not a block mapping"),
            ));
        }
        let end = block_end(text, lines, hit);
        let first = (hit.line + 1..end).find(|&i| is_content(text, lines[i]));
        match first {
            Some(i) if !is_seq_item(text, lines[i]) => {}
            Some(_) => return Err(fail("a parent is a list, not a mapping")),
            None => return Err(fail(&format!("`{k}` is empty (a missing nested key)"))),
        }
        (from, to) = (hit.line + 1, end);
    }
    Ok((from, to))
}

/// Which line edit [`edit_text`] makes.
#[derive(Clone, Copy)]
enum Kind {
    Scalar,
    Append,
}

/// The text with `edit` applied as a line edit.
fn edit_text(text: &str, edit: &YamlEdit) -> Result<String, YamlEditError> {
    let (path, new, kind) = match edit {
        YamlEdit::SetScalar { path, value } => (path, scalar(value), Kind::Scalar),
        YamlEdit::AppendToList { path, item } => (path, scalar(item), Kind::Append),
        YamlEdit::SetMapping { path, entries } => return edit_mapping(text, edit, path, entries),
        YamlEdit::AppendMappingToList { path, entries } => {
            return append_mapping(text, edit, path, entries);
        }
        YamlEdit::SetListItem { path, index, .. } | YamlEdit::RemoveListItem { path, index } => {
            return list_item(text, edit, path, *index);
        }
    };
    let fail = |why: &str| unsupported(edit, why.to_string());
    let Some((last, parents)) = path.split_last() else {
        return Err(fail("an empty path"));
    };
    let lines = lines_of(text);
    let eol = eol_of(text);
    let (from, to) = descend(text, &lines, parents, &fail)?;

    let Some(hit) = find_key(text, &lines, from, to, last) else {
        if !parents.is_empty() {
            return Err(fail("a missing nested key"));
        }
        // A missing top-level key goes at the end of the document.
        let mut out = text.to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(eol);
        }
        let value = match kind {
            Kind::Scalar => new,
            Kind::Append => format!("[{new}]"),
        };
        out.push_str(&format!("{}: {value}{eol}", scalar(last)));
        return Ok(out);
    };
    let line = lines[hit.line];
    let end = block_end(text, &lines, hit);
    let below = has_content(text, &lines, hit.line + 1, end);
    // A list may also sit at the key's own indent, right after it.
    let same_indent_seq = !below && end < lines.len() && {
        let l = lines[end];
        indent(text, l) == hit.indent && is_seq_item(text, l)
    };

    if hit.empty {
        if below || same_indent_seq {
            return match kind {
                Kind::Scalar => Err(fail("the value is a mapping or a list")),
                Kind::Append => {
                    let first = if below {
                        (hit.line + 1..end)
                            .find(|&i| is_content(text, lines[i]))
                            .expect("content below")
                    } else {
                        end
                    };
                    if !is_seq_item(text, lines[first]) {
                        return Err(fail("the value is a mapping, not a list"));
                    }
                    append_block_item(text, &lines, first, &new, eol)
                }
            };
        }
        // A null value: write it on the key's line, before any comment.
        let value = match kind {
            Kind::Scalar => new,
            Kind::Append => format!("[{new}]"),
        };
        let rest = &text[hit.colon..line.end];
        let mut out = String::with_capacity(text.len() + value.len() + 2);
        out.push_str(&text[..hit.colon]);
        out.push(' ');
        out.push_str(&value);
        if rest.trim_start().starts_with('#') {
            out.push_str(rest);
        }
        out.push_str(&text[line.end..]);
        return Ok(out);
    }

    let first = text[hit.value..].chars().next().expect("a value");
    if let Some(why) = refuse_value(first) {
        return Err(fail(why));
    }
    match kind {
        Kind::Scalar => {
            if first == '[' {
                return Err(fail("the value is a list"));
            }
            let vend = scalar_end(text, hit.value, line.end).ok_or_else(|| {
                fail("the value is a quoted scalar that continues on the next line")
            })?;
            Ok(replace_keeping_comment(text, line, hit.value, vend, &new))
        }
        Kind::Append => {
            if first != '[' {
                // A plain null becomes a one-item flow list. yaml-rust2 reads only `~` and
                // `null` as null (`Null` and `NULL` are strings to it).
                if let Some(vend) = scalar_end(text, hit.value, line.end)
                    && matches!(&text[hit.value..vend], "~" | "null")
                {
                    let list = format!("[{new}]");
                    return Ok(replace_keeping_comment(text, line, hit.value, vend, &list));
                }
                return Err(fail("the value is not a list"));
            }
            append_flow_item(text, hit.value, &new)
                .ok_or_else(|| fail("a flow list it can't follow"))
        }
    }
}

/// [`YamlEdit::SetMapping`] as a line edit: the key's whole block (its line and the lines
/// indented below it, not the comments and blank lines that trail it) is replaced by the new
/// block mapping; a missing top-level key is appended at the end of the document.
fn edit_mapping(
    text: &str,
    edit: &YamlEdit,
    path: &[String],
    entries: &[(String, YamlValue)],
) -> Result<String, YamlEditError> {
    let fail = |why: &str| unsupported(edit, why.to_string());
    let Some((last, parents)) = path.split_last() else {
        return Err(fail("an empty path"));
    };
    let lines = lines_of(text);
    let eol = eol_of(text);
    let (from, to) = descend(text, &lines, parents, &fail)?;
    let block = |pad: usize| {
        let mut out = format!("{}{}:{eol}", " ".repeat(pad), scalar(last));
        out.push_str(&block_entries(entries, pad + 2, eol));
        out
    };
    let Some(hit) = find_key(text, &lines, from, to, last) else {
        if !parents.is_empty() {
            return Err(fail("a missing nested key"));
        }
        let mut out = text.to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(eol);
        }
        out.push_str(&block(0));
        return Ok(out);
    };
    if !hit.empty {
        let first = text[hit.value..].chars().next().unwrap_or(' ');
        // A flow mapping on the key's own line (`hardware: {gpu: none}`) is replaced whole.
        if let Some(why) = refuse_value(first).filter(|_| first != '{') {
            return Err(fail(why));
        }
    }
    let mut end = block_end(text, &lines, hit);
    while end > hit.line + 1 && !is_content(text, lines[end - 1]) {
        end -= 1;
    }
    let (start, stop) = (lines[hit.line].start, lines[end - 1].next);
    let mut out = String::with_capacity(text.len() + 64);
    out.push_str(&text[..start]);
    out.push_str(&block(hit.indent));
    out.push_str(&text[stop..]);
    Ok(out)
}

/// [`YamlEdit::AppendMappingToList`] as a line edit (see the variant).
fn append_mapping(
    text: &str,
    edit: &YamlEdit,
    path: &[String],
    entries: &[(String, YamlValue)],
) -> Result<String, YamlEditError> {
    let fail = |why: &str| unsupported(edit, why.to_string());
    if entries.is_empty() {
        return Err(fail("an empty mapping"));
    }
    let Some((last, parents)) = path.split_last() else {
        return Err(fail("an empty path"));
    };
    let lines = lines_of(text);
    let eol = eol_of(text);
    let (from, to) = descend(text, &lines, parents, &fail)?;
    let Some(hit) = find_key(text, &lines, from, to, last) else {
        if !parents.is_empty() {
            return Err(fail("a missing nested key"));
        }
        let mut out = text.to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(eol);
        }
        out.push_str(&format!("{}:{eol}", scalar(last)));
        out.push_str(&block_item(entries, 2, eol));
        return Ok(out);
    };
    let line = lines[hit.line];
    let end = block_end(text, &lines, hit);
    let below = has_content(text, &lines, hit.line + 1, end);
    // A list may also sit at the key's own indent, right after it.
    let same_indent_seq = !below && end < lines.len() && {
        let l = lines[end];
        indent(text, l) == hit.indent && is_seq_item(text, l)
    };
    if hit.empty {
        if below || same_indent_seq {
            let first = if below {
                (hit.line + 1..end)
                    .find(|&i| is_content(text, lines[i]))
                    .expect("content below")
            } else {
                end
            };
            if !is_seq_item(text, lines[first]) {
                return Err(fail("the value is a mapping, not a list"));
            }
            let (seq_indent, after) = block_list_end(text, &lines, first);
            let item = block_item(entries, seq_indent, eol);
            return Ok(insert_after(text, after, &item, eol));
        }
        // No value and nothing below: a block list under the key, after its line.
        let item = block_item(entries, hit.indent + 2, eol);
        return Ok(insert_after(text, line, &item, eol));
    }
    let first = text[hit.value..].chars().next().expect("a value");
    if let Some(why) = refuse_value(first) {
        return Err(fail(why));
    }
    let item = flow_mapping(entries);
    if first != '[' {
        // yaml-rust2 reads only `~` and `null` as null.
        if let Some(vend) = scalar_end(text, hit.value, line.end)
            && matches!(&text[hit.value..vend], "~" | "null")
        {
            let list = format!("[{item}]");
            return Ok(replace_keeping_comment(text, line, hit.value, vend, &list));
        }
        return Err(fail("the value is not a list"));
    }
    append_flow_item(text, hit.value, &item).ok_or_else(|| fail("a flow list it can't follow"))
}

/// [`YamlEdit::SetListItem`] and [`YamlEdit::RemoveListItem`] as line edits (see the variants).
fn list_item(
    text: &str,
    edit: &YamlEdit,
    path: &[String],
    index: usize,
) -> Result<String, YamlEditError> {
    let fail = |why: &str| unsupported(edit, why.to_string());
    let Some((last, parents)) = path.split_last() else {
        return Err(fail("an empty path"));
    };
    let lines = lines_of(text);
    let eol = eol_of(text);
    let (from, to) = descend(text, &lines, parents, &fail)?;
    let Some(hit) = find_key(text, &lines, from, to, last) else {
        return Err(fail("no such key"));
    };
    if !hit.empty {
        return Err(fail("the value is not a block list"));
    }
    let end = block_end(text, &lines, hit);
    // The list's first item: below the key, or at the key's own indent right after it.
    let first = match (hit.line + 1..end).find(|&i| is_content(text, lines[i])) {
        Some(i) => i,
        None if end < lines.len()
            && indent(text, lines[end]) == hit.indent
            && is_seq_item(text, lines[end]) =>
        {
            end
        }
        None => return Err(fail("the value is not a block list")),
    };
    if !is_seq_item(text, lines[first]) {
        return Err(fail("the value is a mapping, not a list"));
    }
    let (seq_indent, last_line) = block_list_end(text, &lines, first);
    let list_end = lines
        .iter()
        .position(|l| l.start == last_line.start)
        .expect("the list's last line")
        + 1;
    // Each item: its first line, and its last content line before the next item.
    let starts: Vec<usize> = (first..list_end)
        .filter(|&i| {
            is_content(text, lines[i])
                && indent(text, lines[i]) == seq_indent
                && is_seq_item(text, lines[i])
        })
        .collect();
    let Some(&start) = starts.get(index) else {
        return Err(fail(&format!("the list has no item {index} (0-based)")));
    };
    let next = starts.get(index + 1).copied().unwrap_or(list_end);
    let stop = (start..next)
        .rev()
        .find(|&i| is_content(text, lines[i]))
        .expect("the item's own line");
    let (cut_from, cut_to) = (lines[start].start, lines[stop].next);
    let mut out = String::with_capacity(text.len() + 64);
    match edit {
        YamlEdit::SetListItem { entries, .. } => {
            if entries.is_empty() {
                return Err(fail("an empty mapping"));
            }
            out.push_str(&text[..cut_from]);
            out.push_str(&block_item(entries, seq_indent, eol));
            out.push_str(&text[cut_to..]);
        }
        _ if starts.len() == 1 => {
            // The only item: the key's line becomes `key: []`, before any comment there.
            let line = lines[hit.line];
            let rest = &text[hit.colon..line.end];
            out.push_str(&text[..hit.colon]);
            out.push_str(" []");
            if rest.trim_start().starts_with('#') {
                out.push_str(rest);
            }
            out.push_str(&text[line.end..cut_from]);
            out.push_str(&text[cut_to..]);
        }
        _ => {
            out.push_str(&text[..cut_from]);
            out.push_str(&text[cut_to..]);
        }
    }
    Ok(out)
}

/// `text` with `new` (whole lines) inserted after the line `l`, with a line break first when `l`
/// is the last line of a file without a final one.
fn insert_after(text: &str, l: Line, new: &str, eol: &str) -> String {
    let mut out = String::with_capacity(text.len() + new.len() + 2);
    out.push_str(&text[..l.next]);
    if l.next == l.end {
        out.push_str(eol);
    }
    out.push_str(new);
    out.push_str(&text[l.next..]);
    out
}

/// The indent of the block list whose first item is on line `first`, and its last line: the
/// last item at that indent, or a line indented deeper below it.
fn block_list_end(text: &str, lines: &[Line], first: usize) -> (usize, Line) {
    let seq_indent = indent(text, lines[first]);
    let mut last = first;
    for (i, &l) in lines.iter().enumerate().skip(first + 1) {
        if !is_content(text, l) {
            continue;
        }
        let ind = indent(text, l);
        if ind > seq_indent || (ind == seq_indent && is_seq_item(text, l)) {
            last = i;
        } else {
            break;
        }
    }
    (seq_indent, lines[last])
}

/// The end of the scalar at `start` on its line (before spaces and a comment), or `None` when a
/// quoted scalar isn't closed on the line or is followed by more than a comment.
fn scalar_end(text: &str, start: usize, line_end: usize) -> Option<usize> {
    let s = &text[start..line_end];
    let first = s.chars().next()?;
    let end = if first == '"' || first == '\'' {
        start + closing_quote(s, first)? + 1
    } else {
        let b = s.as_bytes();
        let comment = (1..b.len()).find(|&i| b[i] == b'#' && matches!(b[i - 1], b' ' | b'\t'));
        start + s[..comment.unwrap_or(b.len())].trim_end().len()
    };
    let after = text[end..line_end].trim_start();
    (after.is_empty() || after.starts_with('#')).then_some(end)
}

/// Replaces `vstart..vend` with `new`, keeping a following comment in its column when there is
/// room (and one space before it otherwise).
fn replace_keeping_comment(
    text: &str,
    line: Line,
    vstart: usize,
    vend: usize,
    new: &str,
) -> String {
    let rest = &text[vend..line.end];
    let ws = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    let mut out = String::with_capacity(text.len() + new.len());
    out.push_str(&text[..vstart]);
    out.push_str(new);
    if rest[ws..].starts_with('#') {
        let old_col = text[line.start..vend + ws].chars().count();
        let new_col = text[line.start..vstart].chars().count() + new.chars().count();
        let pad = old_col.saturating_sub(new_col).max(1);
        out.extend(std::iter::repeat_n(' ', pad));
        out.push_str(&text[vend + ws..]);
    } else {
        out.push_str(&text[vend..]);
    }
    out
}

/// Appends `item` to the block list whose first item is on line `first`.
fn append_block_item(
    text: &str,
    lines: &[Line],
    first: usize,
    item: &str,
    eol: &str,
) -> Result<String, YamlEditError> {
    let (seq_indent, l) = block_list_end(text, lines, first);
    let line = format!("{}- {item}{eol}", " ".repeat(seq_indent));
    Ok(insert_after(text, l, &line, eol))
}

/// Appends `item` to the flow list whose `[` is at `open`; `None` when the list can't be read
/// or more than a comment follows its `]` on that line.
fn append_flow_item(text: &str, open: usize, item: &str) -> Option<String> {
    let b = text.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    let mut last_content = open + 1;
    let mut empty = true;
    let mut has_comment = false;
    let close = loop {
        let c = *b.get(i)?;
        match c {
            b'"' | b'\'' => {
                let len = closing_quote(&text[i..], c as char)?;
                i += len + 1;
                last_content = i;
                empty = false;
                continue;
            }
            b'#' if i > 0 && matches!(b[i - 1], b' ' | b'\t' | b'\n' | b'\r') => {
                has_comment = true;
                i += text[i..].find('\n').unwrap_or(text.len() - i);
                continue;
            }
            b'[' | b'{' => {
                depth += 1;
                if depth > 1 {
                    empty = false;
                    last_content = i + 1;
                }
            }
            b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    break i;
                }
                last_content = i + 1;
            }
            b' ' | b'\t' | b'\r' | b'\n' => {}
            _ => {
                empty = false;
                last_content = i + 1;
            }
        }
        i += 1;
    };
    let line_end = text[close..]
        .find(['\r', '\n'])
        .map_or(text.len(), |n| close + n);
    let after = text[close + 1..line_end].trim_start();
    if !(after.is_empty() || after.starts_with('#')) {
        return None;
    }
    let mut out = String::with_capacity(text.len() + item.len() + 2);
    if empty {
        let inside = &text[open + 1..close];
        if !has_comment && !inside.contains('\n') {
            out.push_str(&text[..=open]);
            out.push_str(item);
            out.push_str(&text[close..]);
        } else {
            out.push_str(&text[..=open]);
            out.push_str(item);
            out.push_str(&text[open + 1..]);
        }
        return Some(out);
    }
    out.push_str(&text[..last_content]);
    out.push_str(if text[..last_content].ends_with(',') {
        " "
    } else {
        ", "
    });
    out.push_str(item);
    out.push_str(&text[last_content..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(path: &[&str], value: &str) -> YamlEdit {
        YamlEdit::SetScalar {
            path: path.iter().map(|s| s.to_string()).collect(),
            value: value.to_string(),
        }
    }

    fn append(path: &[&str], item: &str) -> YamlEdit {
        YamlEdit::AppendToList {
            path: path.iter().map(|s| s.to_string()).collect(),
            item: item.to_string(),
        }
    }

    /// The lines of `a` and `b` that differ, as `(line number, a line, b line)`; both texts
    /// must have the same number of lines.
    fn changed_lines<'a>(a: &'a str, b: &'a str) -> Vec<(usize, &'a str, &'a str)> {
        let (la, lb): (Vec<&str>, Vec<&str>) = (a.split('\n').collect(), b.split('\n').collect());
        assert_eq!(la.len(), lb.len(), "line count differs:\n{a}\n---\n{b}");
        la.iter()
            .zip(&lb)
            .enumerate()
            .filter(|(_, (x, y))| x != y)
            .map(|(i, (x, y))| (i, *x, *y))
            .collect()
    }

    const PROJECT: &str = "\
# Project settings (I4).
name: Ember Knight
slug: ember-knight
type: game               # game | asset-pack | vtuber | animation | audio | media | custom
status: active           # active | paused | archived
platforms: [pc]          # pc, console, mobile
modules: [direction, style, concept, 2d, world, 3d,
          audio, voice]
pipeline:
  skip_stages: []        # stage IDs
  # comment inside the block
  auto_approve:
    - icon
    - ui_sprite
  extra_reviews: []
links:
  - lib:pal_ember        # first link
  - proj:sketchbook
quoted: \"a value\"      # trailing
hardware: inherit
created: 2026-10-12
";

    #[test] // spec: ARCH-076
    fn yaml_set_scalar_keeps_comments_and_order() {
        let out = apply(PROJECT, &[set(&["status"], "paused")]).unwrap();
        assert_eq!(
            changed_lines(PROJECT, &out),
            [(
                4,
                "status: active           # active | paused | archived",
                "status: paused           # active | paused | archived"
            )]
        );
        // A longer value pushes the comment, a shorter one keeps its column.
        let out = apply(PROJECT, &[set(&["status"], "archived-for-now-and-later")]).unwrap();
        assert!(
            out.contains("status: archived-for-now-and-later # active | paused | archived\n"),
            "{out}"
        );
        let out = apply(PROJECT, &[set(&["type"], "media")]).unwrap();
        assert!(
            out.contains("type: media              # game | asset-pack"),
            "{out}"
        );

        // Nested keys, quoted values, and values that need quotes.
        let out = apply(
            PROJECT,
            &[
                set(&["quoted"], "b value"),
                set(&["hardware"], "true"),
                set(&["name"], "Ünïcödé space: two"),
            ],
        )
        .unwrap();
        let changed = changed_lines(PROJECT, &out);
        assert_eq!(changed.len(), 3, "{changed:?}");
        // `"a value"` is 9 characters, `b value` 7: the comment keeps its column.
        assert!(out.contains("quoted: b value        # trailing\n"), "{out}");
        assert!(out.contains("hardware: \"true\"\n"), "{out}");
        assert!(out.contains("name: \"Ünïcödé space: two\"\n"), "{out}");

        // A missing top-level key is appended at the end.
        let out = apply(PROJECT, &[set(&["engine"], "none")]).unwrap();
        assert_eq!(out, format!("{PROJECT}engine: none\n"));
        let no_eol = PROJECT.trim_end_matches('\n');
        assert_eq!(
            apply(no_eol, &[set(&["engine"], "none")]).unwrap(),
            format!("{PROJECT}engine: none\n")
        );
        // A missing nested key is unsupported; so is a scalar edit of a mapping or a list.
        for edit in [
            set(&["pipeline", "missing"], "x"),
            set(&["nope", "deeper"], "x"),
            set(&["pipeline"], "x"),
            set(&["links"], "x"),
            set(&["platforms"], "x"),
            set(&["status", "deeper"], "x"),
        ] {
            assert!(
                matches!(
                    apply(PROJECT, std::slice::from_ref(&edit)),
                    Err(YamlEditError::Unsupported(_))
                ),
                "{edit:?}"
            );
        }
        // Nested scalar in a block mapping.
        let ws = "defaults:      # inherited\n  vcs: git+lfs # how\n  models: {}\nname: x\n";
        let out = apply(ws, &[set(&["defaults", "vcs"], "none")]).unwrap();
        assert_eq!(
            out,
            "defaults:      # inherited\n  vcs: none    # how\n  models: {}\nname: x\n"
        );
        // A null value gets one.
        let out = apply("a:   # c\nb: 1\n", &[set(&["a"], "v")]).unwrap();
        assert_eq!(out, "a: v   # c\nb: 1\n");
        // Invalid YAML is refused as such.
        assert!(matches!(
            apply("a: [1, 2\n", &[set(&["a"], "x")]),
            Err(YamlEditError::Invalid(_))
        ));
    }

    #[test] // spec: ARCH-076, CORE-040
    fn yaml_edit_keeps_crlf_line_endings() {
        let crlf = PROJECT.replace('\n', "\r\n");
        let out = apply(
            &crlf,
            &[
                set(&["status"], "paused"),
                append(&["links"], "lib:new_item"),
                append(&["pipeline", "auto_approve"], "portrait"),
                append(&["platforms"], "web"),
                set(&["engine"], "none"),
                append(&["tags"], "t1"),
            ],
        )
        .unwrap();
        assert_eq!(
            out.matches('\n').count(),
            out.matches("\r\n").count(),
            "{out:?}"
        );
        assert!(out.contains("status: paused           # active | paused | archived\r\n"));
        assert!(out.contains("  - proj:sketchbook\r\n  - lib:new_item\r\n"));
        assert!(out.contains("    - ui_sprite\r\n    - portrait\r\n"));
        assert!(out.contains("platforms: [pc, web]"));
        assert!(out.ends_with("engine: none\r\ntags: [t1]\r\n"), "{out:?}");
        // Unchanged lines are byte for byte the same.
        let lf = out.replace("\r\n", "\n");
        let want = apply(
            PROJECT,
            &[
                set(&["status"], "paused"),
                append(&["links"], "lib:new_item"),
                append(&["pipeline", "auto_approve"], "portrait"),
                append(&["platforms"], "web"),
                set(&["engine"], "none"),
                append(&["tags"], "t1"),
            ],
        )
        .unwrap();
        assert_eq!(lf, want);
    }

    #[test] // spec: ARCH-076, CORE-019
    fn yaml_append_to_block_and_flow_lists() {
        // Block list, then flow lists (one line, empty, several lines).
        let out = apply(PROJECT, &[append(&["links"], "proj:harbor")]).unwrap();
        assert_eq!(
            out,
            PROJECT.replace(
                "  - proj:sketchbook\n",
                "  - proj:sketchbook\n  - proj:harbor\n"
            )
        );
        let out = apply(PROJECT, &[append(&["platforms"], "web")]).unwrap();
        assert_eq!(
            changed_lines(PROJECT, &out),
            [(
                5,
                "platforms: [pc]          # pc, console, mobile",
                "platforms: [pc, web]          # pc, console, mobile"
            )]
        );
        let out = apply(PROJECT, &[append(&["pipeline", "skip_stages"], "export")]).unwrap();
        assert!(
            out.contains("  skip_stages: [export]        # stage IDs\n"),
            "{out}"
        );
        let out = apply(PROJECT, &[append(&["modules"], "release")]).unwrap();
        assert!(
            out.contains("modules: [direction, style, concept, 2d, world, 3d,\n          audio, voice, release]\n"),
            "{out}"
        );
        let out = apply(PROJECT, &[append(&["pipeline", "auto_approve"], "a, b")]).unwrap();
        assert!(
            out.contains("    - ui_sprite\n    - \"a, b\"\n  extra_reviews"),
            "{out}"
        );

        // `links: []`, as new_project_yaml writes it.
        let fresh = "# c\nstatus: active\nlinks: []\ncreated: 2026-10-01\n";
        let out = apply(fresh, &[append(&["links"], "proj:harbor")]).unwrap();
        assert_eq!(
            out,
            "# c\nstatus: active\nlinks: [proj:harbor]\ncreated: 2026-10-01\n"
        );
        let out = apply(&out, &[append(&["links"], "lib:pal_ember")]).unwrap();
        assert!(
            out.contains("links: [proj:harbor, lib:pal_ember]\n"),
            "{out}"
        );

        // A missing top-level list is appended.
        let none = "# c\nstatus: active\n";
        let out = apply(none, &[append(&["links"], "proj:harbor")]).unwrap();
        assert_eq!(out, "# c\nstatus: active\nlinks: [proj:harbor]\n");
        // A null value becomes a one-item list; a list at the key's own indent works too.
        let out = apply("links:\nname: x\n", &[append(&["links"], "a")]).unwrap();
        assert_eq!(out, "links: [a]\nname: x\n");
        let out = apply("links:\n- a\n# end\nname: x\n", &[append(&["links"], "b")]).unwrap();
        assert_eq!(out, "links:\n- a\n- b\n# end\nname: x\n");
        // The last item of a document without a final line break.
        let out = apply("links:\n  - a", &[append(&["links"], "b")]).unwrap();
        assert_eq!(out, "links:\n  - a\n  - b\n");
        // Appending to a scalar or a mapping is unsupported, as is a missing nested list.
        for edit in [
            append(&["status"], "x"),
            append(&["pipeline"], "x"),
            append(&["pipeline", "nope"], "x"),
        ] {
            assert!(
                matches!(
                    apply(PROJECT, std::slice::from_ref(&edit)),
                    Err(YamlEditError::Unsupported(_))
                ),
                "{edit:?}"
            );
        }
    }

    #[test] // spec: ARCH-076
    fn yaml_edit_refuses_anchor_and_flow_mapping_targets() {
        let cases = [
            (
                "defaults: &d\n  vcs: git\nother: *d\n",
                set(&["defaults", "vcs"], "none"),
            ),
            ("x: &a 1\ny: *a\n", set(&["x"], "2")),
            ("x: &a 1\ny: *a\n", set(&["y"], "2")),
            ("list: &l [a]\nmore: *l\n", append(&["list"], "b")),
            (
                "hardware: {gpu: \"RTX\", vram_gb: 24}\n",
                set(&["hardware", "gpu"], "other"),
            ),
            ("hardware: {gpu: x}\n", set(&["hardware"], "none")),
            (
                "routing: {litellm_proxy: null}\n",
                append(&["routing"], "x"),
            ),
            ("tagged: !custom value\n", set(&["tagged"], "x")),
            ("text: |\n  line one\n  line two\n", set(&["text"], "x")),
            ("text: >\n  folded\n", set(&["text"], "x")),
            ("text: plain\n  continued\n", set(&["text"], "x")),
            ("text: \"open\n  quote\"\n", set(&["text"], "x")),
        ];
        for (text, edit) in cases {
            let got = apply(text, std::slice::from_ref(&edit));
            assert!(
                matches!(got, Err(YamlEditError::Unsupported(_))),
                "{text:?} {edit:?}: {got:?}"
            );
        }
        // An edit after a refused one never runs: the whole batch fails.
        assert!(
            apply(
                "a: 1\nh: {k: v}\n",
                &[set(&["a"], "2"), set(&["h", "k"], "w")]
            )
            .is_err()
        );
    }

    #[test] // spec: ARCH-076, CORE-019
    fn yaml_edit_keeps_a_bom_reads_null_lists_and_never_retypes() {
        // `~` and `null` are null: appending makes a one-item list, keeping the comment.
        for null in ["~", "null"] {
            let text = format!("links: {null}    # sources\nname: x\n");
            let out = apply(&text, &[append(&["links"], "proj:harbor")])
                .unwrap_or_else(|e| panic!("{null}: {e}"));
            // The list is longer than the old column allows: one space before the comment.
            assert_eq!(out, "links: [proj:harbor] # sources\nname: x\n", "{null}");
        }
        // yaml-rust2 reads `Null` as a string, so it is not a list to append to.
        assert!(matches!(
            apply("links: Null\n", &[append(&["links"], "a")]),
            Err(YamlEditError::Unsupported(_))
        ));
        let out = apply("links: ~\n", &[append(&["links"], "a")]).unwrap();
        assert_eq!(out, "links: [a]\n");
        let out = apply("links: null # c\n", &[set(&["links"], "x")]).unwrap();
        assert_eq!(out, "links: x    # c\n");

        // A number or boolean is never retyped into a string.
        for text in ["vram_gb: 24\n", "ratio: 0.5\n", "auto: true\n"] {
            let key = text.split(':').next().unwrap();
            assert!(
                matches!(
                    apply(text, &[set(&[key], "x")]),
                    Err(YamlEditError::Unsupported(_))
                ),
                "{text}"
            );
        }

        // A byte-order mark is set aside, and written back.
        let bom = format!("\u{feff}{PROJECT}");
        let out = apply(
            &bom,
            &[
                set(&["status"], "paused"),
                append(&["links"], "proj:harbor"),
            ],
        )
        .unwrap();
        assert!(out.starts_with("\u{feff}# Project settings"), "{out:?}");
        assert_eq!(out.matches('\u{feff}').count(), 1);
        assert_eq!(
            out.trim_start_matches('\u{feff}'),
            apply(
                PROJECT,
                &[
                    set(&["status"], "paused"),
                    append(&["links"], "proj:harbor")
                ]
            )
            .unwrap()
        );
        // A BOM in front of the first key still finds it.
        let out = apply("\u{feff}name: a\n", &[set(&["name"], "b")]).unwrap();
        assert_eq!(out, "\u{feff}name: b\n");
    }

    #[test] // spec: ARCH-076
    fn scalars_quote_only_when_needed() {
        for plain in [
            "active",
            "proj:harbor",
            "lib:pal_ember",
            "Ünïcödé space",
            "git+lfs",
            "say \"hi\"",
            "2026-10-01",
        ] {
            assert_eq!(scalar(plain), plain);
        }
        for (value, quoted) in [
            ("true", "\"true\""),
            ("123", "\"123\""),
            ("null", "\"null\""),
            ("", "\"\""),
            ("a: b", "\"a: b\""),
            ("a #b", "\"a #b\""),
            (" lead", "\" lead\""),
            ("- x", "\"- x\""),
            ("line\nbreak", "\"line\\nbreak\""),
            ("\"hi\"", "\"\\\"hi\\\"\""),
            ("'hi'", "\"'hi'\""),
            ("[x]", "\"[x]\""),
        ] {
            assert_eq!(scalar(value), quoted, "{value:?}");
        }
    }

    fn mapping(path: &[&str], entries: &[(&str, YamlValue)]) -> YamlEdit {
        YamlEdit::SetMapping {
            path: path.iter().map(|s| s.to_string()).collect(),
            entries: entries
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        }
    }

    fn hardware(gpu: &str, vram: i64) -> YamlEdit {
        mapping(
            &["hardware"],
            &[
                ("gpu", YamlValue::Str(gpu.into())),
                ("vram_gb", YamlValue::Int(vram)),
                ("detected", YamlValue::Str("2026-10-18".into())),
            ],
        )
    }

    /// A mapping edit rewrites the key's block (numbers included) and nothing around it; the
    /// comments and blank lines after the block stay; a flow mapping or a scalar value is
    /// replaced; a missing top-level key goes last; a missing nested key is refused.
    #[test] // spec: ARCH-076, CORE-067
    fn set_mapping_rewrites_the_block_and_only_the_block() {
        let text = "\
# top comment
name: Sample
hardware:           # detected
  gpu: Old GPU
  vram_gb: 8
  # inner comment
  ram_gb: 16

# before tools
tools: {}
";
        let out = apply(text, &[hardware("New GPU", 24)]).unwrap();
        assert_eq!(
            out,
            "\
# top comment
name: Sample
hardware:
  gpu: New GPU
  vram_gb: 24
  detected: 2026-10-18

# before tools
tools: {}
"
        );

        // Flow mapping, scalar value, CRLF files and the end of a file without a newline.
        for (old, new) in [
            (
                "a: 1\nhardware: {gpu: none}\nb: 2\n",
                "a: 1\nhardware:\n  gpu: X\n  vram_gb: 4\n  detected: 2026-10-18\nb: 2\n",
            ),
            (
                "hardware: none\n",
                "hardware:\n  gpu: X\n  vram_gb: 4\n  detected: 2026-10-18\n",
            ),
            (
                "a: 1\r\nhardware:\r\n  gpu: Y\r\n",
                "a: 1\r\nhardware:\r\n  gpu: X\r\n  vram_gb: 4\r\n  detected: 2026-10-18\r\n",
            ),
            (
                "a: 1\nhardware:\n  gpu: Y",
                "a: 1\nhardware:\n  gpu: X\n  vram_gb: 4\n  detected: 2026-10-18\n",
            ),
            // Missing: appended, after a final line without a newline too.
            (
                "a: 1",
                "a: 1\nhardware:\n  gpu: X\n  vram_gb: 4\n  detected: 2026-10-18\n",
            ),
        ] {
            assert_eq!(apply(old, &[hardware("X", 4)]).unwrap(), new, "{old:?}");
        }

        // A nested key that is missing, and a value the line editor won't touch.
        for refused in [
            "defaults:\n  vcs: git\n",
            "hardware: &h {gpu: none}\n",
            "hardware: |\n  text\n",
        ] {
            let edit = if refused.starts_with("defaults") {
                mapping(
                    &["defaults", "hardware"],
                    &[("gpu", YamlValue::Str("x".into()))],
                )
            } else {
                hardware("X", 4)
            };
            assert!(
                matches!(apply(refused, &[edit]), Err(YamlEditError::Unsupported(_))),
                "{refused:?}"
            );
        }
    }

    fn redirect(from: &str, to: &str, actor: bool) -> YamlEdit {
        let mut entries = vec![
            ("from".to_string(), YamlValue::Str(from.into())),
            ("to".to_string(), YamlValue::Str(to.into())),
            (
                "moved_utc".to_string(),
                YamlValue::Str("2026-11-05T10:00:00Z".into()),
            ),
            ("by".to_string(), YamlValue::Str("cli".into())),
        ];
        if actor {
            entries.push((
                "actor".to_string(),
                YamlValue::Map(vec![
                    ("kind".to_string(), YamlValue::Str("claude_code".into())),
                    ("session_id".to_string(), YamlValue::Str("s: 1".into())),
                ]),
            ));
        }
        YamlEdit::AppendMappingToList {
            path: vec!["redirects".to_string()],
            entries,
        }
    }

    #[test] // spec: ARCH-076, ARCH-125
    fn yaml_append_mapping_keeps_comments_and_order() {
        // A block list: the item goes after the last one, comments and the rest untouched.
        let text = "# Redirects (ARCH-125).\nredirects:\n  # oldest first\n  - from: a/x  # moved\n    to: a/y\n    moved_utc: 2026-11-01T10:00:00Z\n    by: cli\n# the end\nother: 1\n";
        let out = apply(text, &[redirect("a/y", "b/z", true)]).unwrap();
        assert_eq!(
            out,
            text.replace(
                "    by: cli\n# the end",
                "    by: cli\n  - from: a/y\n    to: b/z\n    moved_utc: 2026-11-05T10:00:00Z\n    by: cli\n    actor:\n      kind: claude_code\n      session_id: \"s: 1\"\n# the end"
            )
        );
        // A list at the key's own indent, CRLF, and no final line break.
        let text = "redirects:\r\n- from: a/x\r\n  to: a/y";
        let out = apply(text, &[redirect("a/y", "a/z", false)]).unwrap();
        assert_eq!(
            out,
            "redirects:\r\n- from: a/x\r\n  to: a/y\r\n- from: a/y\r\n  to: a/z\r\n  moved_utc: 2026-11-05T10:00:00Z\r\n  by: cli\r\n"
        );
        // A missing file or key: a block list at the end.
        let out = apply("", &[redirect("a/x", "a/y", false)]).unwrap();
        assert_eq!(
            out,
            "redirects:\n  - from: a/x\n    to: a/y\n    moved_utc: 2026-11-05T10:00:00Z\n    by: cli\n"
        );
        let out = apply("# c", &[redirect("a/x", "a/y", false)]).unwrap();
        assert!(out.starts_with("# c\nredirects:\n  - from: a/x\n"), "{out}");
        // A key with no value: the list goes under it; its comment stays.
        let out = apply(
            "redirects:   # none yet\nname: x\n",
            &[redirect("a/x", "a/y", false)],
        )
        .unwrap();
        assert_eq!(
            out,
            "redirects:   # none yet\n  - from: a/x\n    to: a/y\n    moved_utc: 2026-11-05T10:00:00Z\n    by: cli\nname: x\n"
        );
        // A flow list and a null: a flow mapping.
        let out = apply("redirects: []  # c\n", &[redirect("a/x", "a/y", true)]).unwrap();
        assert_eq!(
            out,
            "redirects: [{from: a/x, to: a/y, moved_utc: 2026-11-05T10:00:00Z, by: cli, actor: {kind: claude_code, session_id: \"s: 1\"}}]  # c\n"
        );
        let out = apply("redirects: ~\n", &[redirect("a/x", "a/y", false)]).unwrap();
        assert_eq!(
            out,
            "redirects: [{from: a/x, to: a/y, moved_utc: 2026-11-05T10:00:00Z, by: cli}]\n"
        );
        // Not a list, an anchor, an empty mapping: refused, the text untouched.
        for (text, edit) in [
            ("redirects: x\n", redirect("a/x", "a/y", false)),
            ("redirects:\n  a: b\n", redirect("a/x", "a/y", false)),
            ("redirects: &r []\n", redirect("a/x", "a/y", false)),
            (
                "redirects: []\n",
                YamlEdit::AppendMappingToList {
                    path: vec!["redirects".into()],
                    entries: Vec::new(),
                },
            ),
        ] {
            assert!(
                matches!(
                    apply(text, std::slice::from_ref(&edit)),
                    Err(YamlEditError::Unsupported(_))
                ),
                "{text:?}"
            );
        }
    }

    fn item(index: usize, entries: &[(&str, YamlValue)]) -> YamlEdit {
        YamlEdit::SetListItem {
            path: vec!["collections".into()],
            index,
            entries: entries
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        }
    }

    fn remove(index: usize) -> YamlEdit {
        YamlEdit::RemoveListItem {
            path: vec!["collections".into()],
            index,
        }
    }

    /// An item edit rewrites that item's own lines and a removal takes them out; comments and
    /// blank lines around them, the other items and their order stay; removing the only item
    /// leaves `key: []`; a flow list, a missing key or index, and an empty mapping are refused.
    /// Lists and booleans are written as block lists and plain `true`/`false`.
    #[test] // spec: ARCH-076, CORE-155
    fn list_items_are_set_and_removed_in_place() {
        let text = "\
# top
collections:   # entries
  # first
  - id: a
    name: A   # trailing
    refs:
      - p/x

  # second
  - {id: b, name: B}
  - id: c
    rule: {type: image}
# end
";
        let out = apply(
            text,
            &[item(
                1,
                &[
                    ("id", YamlValue::Str("b".into())),
                    ("name", YamlValue::Str("B, renamed".into())),
                    (
                        "rule",
                        YamlValue::Map(vec![
                            (
                                "type".into(),
                                YamlValue::List(vec![
                                    YamlValue::Str("image".into()),
                                    YamlValue::Str("true".into()),
                                ]),
                            ),
                            ("duplicates".into(), YamlValue::Bool(true)),
                            ("rating_min".into(), YamlValue::Int(3)),
                        ]),
                    ),
                    ("refs", YamlValue::List(Vec::new())),
                ],
            )],
        )
        .unwrap();
        assert_eq!(
            out,
            text.replace(
                "  - {id: b, name: B}\n",
                "  - id: b\n    name: \"B, renamed\"\n    rule:\n      type:\n        - image\n        - \"true\"\n      duplicates: true\n      rating_min: 3\n    refs: []\n"
            )
        );
        let out = apply(text, &[remove(0)]).unwrap();
        assert_eq!(
            out,
            text.replace(
                "  - id: a\n    name: A   # trailing\n    refs:\n      - p/x\n",
                ""
            )
        );
        let out = apply(text, &[remove(2)]).unwrap();
        assert_eq!(
            out,
            text.replace("  - id: c\n    rule: {type: image}\n", "")
        );
        let out = apply(text, &[remove(2), remove(1), remove(0)]).unwrap();
        assert_eq!(
            out,
            "# top\ncollections: []   # entries\n  # first\n\n  # second\n# end\n"
        );
        // A list at the key's own indent, CRLF line ends.
        let out = apply(
            "l:\r\n- id: a\r\n- id: b\r\n",
            &[remove(0)].map(|e| match e {
                YamlEdit::RemoveListItem { index, .. } => YamlEdit::RemoveListItem {
                    path: vec!["l".into()],
                    index,
                },
                e => e,
            }),
        )
        .unwrap();
        assert_eq!(out, "l:\r\n- id: b\r\n");
        for (text, edit) in [
            ("collections: [{id: a}]\n", remove(0)),
            ("collections:\n  - id: a\n", remove(1)),
            ("other: 1\n", remove(0)),
            ("collections:\n  a: b\n", remove(0)),
            ("collections:\n  - id: a\n", item(0, &[])),
            (
                "collections:\n  - id: a\n",
                item(3, &[("id", YamlValue::Str("a".into()))]),
            ),
        ] {
            assert!(
                matches!(
                    apply(text, std::slice::from_ref(&edit)),
                    Err(YamlEditError::Unsupported(_))
                ),
                "{text:?} {edit:?}"
            );
        }
    }
}
