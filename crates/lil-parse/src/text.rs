//! The shown-text normaliser for notification text (Appendix G "Toast XML"; P4a-R8).

/// `s` with C0 and C1 controls removed, the bidirectional and zero-width characters Appendix G
/// lists shown as `⟨U+XXXX⟩` (a U+200D inside a UTS #51 emoji ZWJ sequence is kept), and at most
/// `max_chars` `char`s over the whole text, ending `…` when cut.
///
/// The limit counts the output, markers included. A marker is never cut in half: when the next
/// piece doesn't fit before the `…`, the text ends there.
pub fn shown_text(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let fits = pieces(s)
        .try_fold(0_usize, |n, p| {
            Some(n + p.len()).filter(|n| *n <= max_chars)
        })
        .is_some();
    let budget = if fits { max_chars } else { max_chars - 1 };
    let mut out = String::new();
    let mut n = 0;
    for piece in pieces(s) {
        n += piece.len();
        if n > budget {
            break;
        }
        match piece {
            Piece::Char(c) => out.push(c),
            Piece::Marker(c) => out.push_str(&format!("\u{27e8}U+{:04X}\u{27e9}", u32::from(c))),
        }
    }
    if !fits {
        out.push('…');
    }
    out
}

/// One character of the output, or one marker.
enum Piece {
    Char(char),
    Marker(char),
}

impl Piece {
    /// Its length in `char`s: every listed code point is below U+10000, so a marker is 8.
    fn len(&self) -> usize {
        match self {
            Piece::Char(_) => 1,
            Piece::Marker(_) => 8,
        }
    }
}

/// The pieces of `s`, in order: controls dropped, listed characters as markers.
fn pieces(s: &str) -> impl Iterator<Item = Piece> + '_ {
    // The two input code points before the current one, nearest first.
    let mut before: [Option<char>; 2] = [None; 2];
    let mut rest = s.chars().peekable();
    std::iter::from_fn(move || {
        loop {
            let c = rest.next()?;
            let [prev, prev2] = before;
            before = [Some(c), prev];
            if c.is_control() {
                continue;
            }
            let joins_pictographs = c == '\u{200d}'
                && ends_an_emoji_element(prev, prev2)
                && rest.peek().copied().is_some_and(is_extended_pictographic);
            return Some(if is_shown_as_marker(c) && !joins_pictographs {
                Piece::Marker(c)
            } else {
                Piece::Char(c)
            });
        }
    })
}

/// Whether the input just before a U+200D ends an element of a UTS #51 emoji ZWJ sequence: an
/// `Extended_Pictographic` code point, optionally followed by one `Emoji_Modifier` or U+FE0F
/// (Appendix G "Toast XML").
fn ends_an_emoji_element(prev: Option<char>, prev2: Option<char>) -> bool {
    match prev {
        Some(c) if is_extended_pictographic(c) => true,
        Some(c) if is_emoji_modifier(c) || c == '\u{fe0f}' => {
            prev2.is_some_and(is_extended_pictographic)
        }
        _ => false,
    }
}

/// `Emoji_Modifier` in Unicode 16.0.0's `emoji-data.txt`: U+1F3FB–U+1F3FF, the skin tones.
fn is_emoji_modifier(c: char) -> bool {
    matches!(c, '\u{1f3fb}'..='\u{1f3ff}')
}

/// The bidirectional controls and zero-width characters of Appendix G: U+200E, U+200F,
/// U+202A–U+202E, U+2066–U+2069, U+200B–U+200D, U+2060 and U+FEFF.
fn is_shown_as_marker(c: char) -> bool {
    matches!(
        c,
        '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
            | '\u{2060}'
            | '\u{feff}'
    )
}

fn is_extended_pictographic(c: char) -> bool {
    EXTENDED_PICTOGRAPHIC
        .binary_search_by(|&(lo, hi)| {
            if hi < c {
                std::cmp::Ordering::Less
            } else if lo > c {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// `Extended_Pictographic`, generated once from Unicode 16.0.0's `emoji-data.txt`
/// (https://www.unicode.org/Public/16.0.0/ucd/emoji/emoji-data.txt), adjacent ranges merged.
const EXTENDED_PICTOGRAPHIC: &[(char, char)] = &[
    ('\u{a9}', '\u{a9}'),
    ('\u{ae}', '\u{ae}'),
    ('\u{203c}', '\u{203c}'),
    ('\u{2049}', '\u{2049}'),
    ('\u{2122}', '\u{2122}'),
    ('\u{2139}', '\u{2139}'),
    ('\u{2194}', '\u{2199}'),
    ('\u{21a9}', '\u{21aa}'),
    ('\u{231a}', '\u{231b}'),
    ('\u{2328}', '\u{2328}'),
    ('\u{2388}', '\u{2388}'),
    ('\u{23cf}', '\u{23cf}'),
    ('\u{23e9}', '\u{23f3}'),
    ('\u{23f8}', '\u{23fa}'),
    ('\u{24c2}', '\u{24c2}'),
    ('\u{25aa}', '\u{25ab}'),
    ('\u{25b6}', '\u{25b6}'),
    ('\u{25c0}', '\u{25c0}'),
    ('\u{25fb}', '\u{25fe}'),
    ('\u{2600}', '\u{2605}'),
    ('\u{2607}', '\u{2612}'),
    ('\u{2614}', '\u{2685}'),
    ('\u{2690}', '\u{2705}'),
    ('\u{2708}', '\u{2712}'),
    ('\u{2714}', '\u{2714}'),
    ('\u{2716}', '\u{2716}'),
    ('\u{271d}', '\u{271d}'),
    ('\u{2721}', '\u{2721}'),
    ('\u{2728}', '\u{2728}'),
    ('\u{2733}', '\u{2734}'),
    ('\u{2744}', '\u{2744}'),
    ('\u{2747}', '\u{2747}'),
    ('\u{274c}', '\u{274c}'),
    ('\u{274e}', '\u{274e}'),
    ('\u{2753}', '\u{2755}'),
    ('\u{2757}', '\u{2757}'),
    ('\u{2763}', '\u{2767}'),
    ('\u{2795}', '\u{2797}'),
    ('\u{27a1}', '\u{27a1}'),
    ('\u{27b0}', '\u{27b0}'),
    ('\u{27bf}', '\u{27bf}'),
    ('\u{2934}', '\u{2935}'),
    ('\u{2b05}', '\u{2b07}'),
    ('\u{2b1b}', '\u{2b1c}'),
    ('\u{2b50}', '\u{2b50}'),
    ('\u{2b55}', '\u{2b55}'),
    ('\u{3030}', '\u{3030}'),
    ('\u{303d}', '\u{303d}'),
    ('\u{3297}', '\u{3297}'),
    ('\u{3299}', '\u{3299}'),
    ('\u{1f000}', '\u{1f0ff}'),
    ('\u{1f10d}', '\u{1f10f}'),
    ('\u{1f12f}', '\u{1f12f}'),
    ('\u{1f16c}', '\u{1f171}'),
    ('\u{1f17e}', '\u{1f17f}'),
    ('\u{1f18e}', '\u{1f18e}'),
    ('\u{1f191}', '\u{1f19a}'),
    ('\u{1f1ad}', '\u{1f1e5}'),
    ('\u{1f201}', '\u{1f20f}'),
    ('\u{1f21a}', '\u{1f21a}'),
    ('\u{1f22f}', '\u{1f22f}'),
    ('\u{1f232}', '\u{1f23a}'),
    ('\u{1f23c}', '\u{1f23f}'),
    ('\u{1f249}', '\u{1f3fa}'),
    ('\u{1f400}', '\u{1f53d}'),
    ('\u{1f546}', '\u{1f64f}'),
    ('\u{1f680}', '\u{1f6ff}'),
    ('\u{1f774}', '\u{1f77f}'),
    ('\u{1f7d5}', '\u{1f7ff}'),
    ('\u{1f80c}', '\u{1f80f}'),
    ('\u{1f848}', '\u{1f84f}'),
    ('\u{1f85a}', '\u{1f85f}'),
    ('\u{1f888}', '\u{1f88f}'),
    ('\u{1f8ae}', '\u{1f8ff}'),
    ('\u{1f90c}', '\u{1f93a}'),
    ('\u{1f93c}', '\u{1f945}'),
    ('\u{1f947}', '\u{1faff}'),
    ('\u{1fc00}', '\u{1fffd}'),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: APP-022
    fn the_pictograph_table_is_sorted_and_disjoint() {
        assert!(
            EXTENDED_PICTOGRAPHIC
                .windows(2)
                .all(|w| w[0].0 <= w[0].1 && w[0].1 < w[1].0)
        );
        for c in ['\u{a9}', '\u{1f468}', '\u{1f600}', '\u{1fffd}', '\u{2764}'] {
            assert!(is_extended_pictographic(c), "{c:?}");
        }
        for c in [
            'a',
            '\u{a8}',
            '\u{1f3fb}',
            '\u{fe0f}',
            '\u{1fffe}',
            '\u{200d}',
        ] {
            assert!(!is_extended_pictographic(c), "{c:?}");
        }
    }
}
