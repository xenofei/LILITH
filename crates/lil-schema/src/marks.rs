//! Asset marks and tags (Appendix H "Manifest fields" and "Organising tools"; CORE-152;
//! P4b-R26).
//!
//! The asset's `tags`, `rating` (0–5, absent is 0) and `label` (absent is none), and the
//! take-level [`Flag`]. A hand-written value outside these rules is read as written, with a
//! warning (ARCH-076): a rating above 5 reads as 5, an unknown label or flag as none, and tags
//! are normalised only when a mark next writes `tags`.

use std::fmt;
use std::str::FromStr;

use serde_json::Value;

use crate::manifest::{Obj, Reader};

/// The highest rating; 0 is unrated.
pub const MAX_RATING: u8 = 5;
/// The most tags an asset holds.
pub const MAX_TAGS: usize = 64;
/// The most characters in one tag.
pub const MAX_TAG_CHARS: usize = 64;

/// A colour label. Wire values are lowercase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Label {
    Red,
    Yellow,
    Green,
    Blue,
    Purple,
}

impl Label {
    pub const ALL: [Label; 5] = [
        Label::Red,
        Label::Yellow,
        Label::Green,
        Label::Blue,
        Label::Purple,
    ];

    /// The wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            Label::Red => "red",
            Label::Yellow => "yellow",
            Label::Green => "green",
            Label::Blue => "blue",
            Label::Purple => "purple",
        }
    }
}

impl FromStr for Label {
    type Err = String;

    fn from_str(s: &str) -> Result<Label, String> {
        Label::ALL
            .into_iter()
            .find(|l| l.as_str() == s)
            .ok_or_else(|| format!("`{s}` is not a label (red, yellow, green, blue or purple)"))
    }
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A take's flag: `pick` or `reject`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flag {
    Pick,
    Reject,
}

impl Flag {
    /// The wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            Flag::Pick => "pick",
            Flag::Reject => "reject",
        }
    }
}

impl FromStr for Flag {
    type Err = String;

    fn from_str(s: &str) -> Result<Flag, String> {
        match s {
            "pick" => Ok(Flag::Pick),
            "reject" => Ok(Flag::Reject),
            _ => Err(format!("`{s}` is not a flag (pick or reject)")),
        }
    }
}

impl fmt::Display for Flag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An asset's marks.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Marks {
    pub tags: Vec<String>,
    /// 0–5; 0 is unrated.
    pub rating: u8,
    pub label: Option<Label>,
}

/// The form tags are compared in, and the index's fold: Rust's full Unicode lowercase
/// (`str::to_lowercase`), not case folding (P4b-R26).
pub fn fold_tag(tag: &str) -> String {
    tag.to_lowercase()
}

/// Tags as a mark writes them (P4b-R26): each trimmed, 1–64 characters and without control
/// characters, else an error naming the tag; case-insensitive duplicates ([`fold_tag`]) are
/// dropped, the first spelling staying; at most 64 remain, else an error.
pub fn normalise_tags(tags: &[String]) -> Result<Vec<String>, String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for tag in tags {
        let t = tag.trim();
        let chars = t.chars().count();
        if chars == 0 {
            return Err("a tag can't be empty".into());
        }
        if chars > MAX_TAG_CHARS {
            return Err(format!(
                "the tag `{t}` is {chars} characters long; a tag has at most {MAX_TAG_CHARS}"
            ));
        }
        if t.chars().any(char::is_control) {
            return Err(format!("the tag {t:?} holds a control character"));
        }
        if seen.insert(fold_tag(t)) {
            out.push(t.to_string());
        }
    }
    if out.len() > MAX_TAGS {
        return Err(format!(
            "{} tags; an asset has at most {MAX_TAGS}",
            out.len()
        ));
    }
    Ok(out)
}

/// A rating at `key` of `o`: absent or `null` is 0; above 5 reads as 5 and anything that
/// isn't a whole number from 0 as 0, each with a warning.
pub(crate) fn read_rating(r: &mut Reader, o: Obj<'_>, key: &str) -> u8 {
    let Some(value) = Reader::field(o, key) else {
        return 0;
    };
    match value.as_u64() {
        Some(n) => match u8::try_from(n) {
            Ok(n) if n <= MAX_RATING => n,
            _ => {
                r.warn(Reader::ptr(o, key), "a rating is 0 to 5: read as 5");
                MAX_RATING
            }
        },
        None => {
            r.warn(
                Reader::ptr(o, key),
                "expected a whole number from 0 to 5: read as 0 (unrated)",
            );
            0
        }
    }
}

/// A flag at `key` of `o`: absent, `null` or unknown (with a warning) is none.
pub(crate) fn read_flag(r: &mut Reader, o: Obj<'_>, key: &str) -> Option<Flag> {
    let s = r.opt_str(o, key)?;
    match s.parse() {
        Ok(f) => Some(f),
        Err(e) => {
            r.warn(Reader::ptr(o, key), format!("{e}: read as none"));
            None
        }
    }
}

/// The asset's marks from the manifest root.
pub(crate) fn read_marks(r: &mut Reader, root: Obj<'_>) -> Marks {
    let tags = r.strings(root, "tags");
    match normalise_tags(&tags) {
        Ok(normal) if normal == tags => {}
        Ok(_) => r.warn(
            "/tags".into(),
            "read as written: it has untrimmed tags or case-insensitive duplicates, normalised \
             when a mark next writes `tags`",
        ),
        Err(e) => r.warn("/tags".into(), format!("read as written: {e}")),
    }
    let rating = read_rating(r, root, "rating");
    let label = match r.opt_str(root, "label") {
        None => None,
        Some(s) => match s.parse() {
            Ok(l) => Some(l),
            Err(e) => {
                r.warn("/label".into(), format!("{e}: read as none"));
                None
            }
        },
    };
    Marks {
        tags,
        rating,
        label,
    }
}

/// The JSON value of a rating, or `None` (the key is removed) for 0.
pub(crate) fn rating_value(rating: u8) -> Option<Value> {
    (rating > 0).then(|| Value::from(rating.min(MAX_RATING)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;
    use serde_json::json;

    fn s(tags: &[&str]) -> Vec<String> {
        tags.iter().map(|t| t.to_string()).collect()
    }

    #[test] // spec: CORE-152
    fn tags_normalise_trim_dedupe_and_refuse_controls() {
        // Trimmed; case-insensitive duplicates dropped, the first spelling staying (P4b-R26).
        assert_eq!(
            normalise_tags(&s(&[
                "  Harbor ",
                "harbor",
                "HARBOR",
                "Ärger",
                "äRGER",
                "night"
            ])),
            Ok(s(&["Harbor", "Ärger", "night"]))
        );
        assert_eq!(normalise_tags(&[]), Ok(Vec::new()));
        // Full lowercase (`str::to_lowercase`), not ASCII-only.
        assert_eq!(fold_tag("ÄRGER"), "ärger");
        assert_eq!(fold_tag("Harbor"), fold_tag("hARBOR"));

        for bad in ["", "   ", "a\u{7}b", "line\nbreak", "tab\there", "x\u{9f}"] {
            assert!(normalise_tags(&s(&[bad])).is_err(), "{bad:?}");
        }
        // 1–64 characters, counted as characters, not bytes.
        assert!(normalise_tags(&[("x".repeat(64))]).is_ok());
        assert!(normalise_tags(&[("é".repeat(64))]).is_ok());
        assert!(normalise_tags(&[("x".repeat(65))]).is_err());
        // At most 64 per asset, counted after duplicates are dropped.
        let mut many: Vec<String> = (0..64).map(|i| format!("t{i}")).collect();
        assert_eq!(normalise_tags(&many).map(|t| t.len()), Ok(64));
        many.push("T0".into());
        assert_eq!(normalise_tags(&many).map(|t| t.len()), Ok(64));
        many.push("t64".into());
        assert!(normalise_tags(&many).is_err());
    }

    #[test] // spec: CORE-152, ARCH-076
    fn out_of_range_marks_read_as_warnings() {
        let text = json!({
            "schema_version": 1, "asset_id": "img_dock", "project": "sketchbook",
            "tags": ["Sky", " sky"], "rating": 7, "label": "orange",
            "candidate_runs": [{"run_id": "r_20261101_0900", "files": [
                {"path": "candidates/r_20261101_0900/a.png", "role": "candidate",
                 "rating": 9, "flag": "maybe"},
                {"path": "candidates/r_20261101_0900/b.png", "role": "candidate",
                 "rating": -1, "flag": "reject"}
            ]}]
        })
        .to_string();
        let (m, warnings) = Manifest::parse(&text).expect("schema problems are warnings");
        let marks = m.marks();
        assert_eq!(marks.rating, 5, "7 reads as 5");
        assert_eq!(marks.label, None, "an unknown label reads as none");
        assert_eq!(marks.tags, s(&["Sky", " sky"]), "read as written");
        let files = &m.view().candidate_runs[0].files;
        assert_eq!((files[0].rating, files[0].flag), (5, None));
        assert_eq!((files[1].rating, files[1].flag), (0, Some(Flag::Reject)));
        let pointers: Vec<&str> = warnings.iter().map(|w| w.pointer.as_str()).collect();
        for p in [
            "/rating",
            "/label",
            "/tags",
            "/candidate_runs/0/files/0/rating",
            "/candidate_runs/0/files/0/flag",
            "/candidate_runs/0/files/1/rating",
        ] {
            assert!(pointers.contains(&p), "{p} missing from {pointers:?}");
        }

        // In range: no warning, and every label reads.
        for label in Label::ALL {
            let text = json!({"schema_version": 1, "asset_id": "img_dock", "project": "sketchbook",
                              "tags": ["Sky"], "rating": 3, "label": label.as_str()})
            .to_string();
            let (m, warnings) = Manifest::parse(&text).unwrap();
            assert!(warnings.is_empty(), "{warnings:?}");
            assert_eq!(
                m.marks(),
                Marks {
                    tags: s(&["Sky"]),
                    rating: 3,
                    label: Some(label)
                }
            );
        }
        let wire: Vec<&str> = Label::ALL.iter().map(|l| l.as_str()).collect();
        assert_eq!(wire, ["red", "yellow", "green", "blue", "purple"]);
        // Absent marks: no tags, unrated, no label, no warning.
        let (m, _) = Manifest::parse(
            &json!({"schema_version": 1, "asset_id": "a", "project": "p"}).to_string(),
        )
        .unwrap();
        assert_eq!(m.marks(), Marks::default());
    }
}
