//! The shown-text normaliser (APP-022, P4a-R8; Appendix G "Toast XML").

use lil_parse::shown_text;

fn marker(c: char) -> String {
    format!("\u{27e8}U+{:04X}\u{27e9}", u32::from(c))
}

#[test] // spec: APP-022
fn controls_are_removed() {
    let controls: Vec<char> = (0..=0x1f_u32)
        .chain(0x7f..=0x9f)
        .map(|n| char::from_u32(n).unwrap())
        .collect();
    assert_eq!(controls.len(), 32 + 33);
    let mut s = String::from("a");
    for c in &controls {
        s.push(*c);
        s.push('b');
    }
    assert_eq!(
        shown_text(&s, 1000),
        format!("a{}", "b".repeat(controls.len()))
    );
    assert_eq!(
        shown_text("line one\nline two\ttab\r", 200),
        "line oneline twotab"
    );
    // Neighbours of the ranges stay.
    assert_eq!(shown_text(" ~\u{a0}\u{a1}", 200), " ~\u{a0}\u{a1}");
}

#[test] // spec: APP-022
fn bidi_and_zero_width_are_shown() {
    let listed = [
        '\u{200e}', '\u{200f}', '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}',
        '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}', '\u{200b}', '\u{200c}', '\u{200d}',
        '\u{2060}', '\u{feff}',
    ];
    for c in listed {
        assert_eq!(
            shown_text(&format!("x{c}y"), 200),
            format!("x{}y", marker(c)),
            "U+{:04X}",
            u32::from(c)
        );
        assert_eq!(shown_text(&c.to_string(), 200), marker(c));
    }
    assert_eq!(marker('\u{202e}'), "\u{27e8}U+202E\u{27e9}");
    // Neighbours of the listed code points stay.
    for c in [
        '\u{2010}', '\u{2029}', '\u{202f}', '\u{2065}', '\u{206a}', '\u{2061}', '\u{fefe}',
    ] {
        assert_eq!(shown_text(&c.to_string(), 200), c.to_string());
    }
}

#[test] // spec: APP-022
fn emoji_sequences_and_rtl_text_survive() {
    // UTS #51 emoji ZWJ sequences: each element is an Extended_Pictographic code point,
    // optionally followed by one Emoji_Modifier or U+FE0F.
    let kept = [
        // Man, woman, girl, boy.
        "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}",
        // Woman technologist: medium skin tone (an Emoji_Modifier) before the joiner.
        "\u{1f469}\u{1f3fd}\u{200d}\u{1f4bb}",
        // Heart on fire: U+FE0F before the joiner.
        "\u{2764}\u{fe0f}\u{200d}\u{1f525}",
        // Rainbow flag: U+FE0F before the joiner.
        "\u{1f3f3}\u{fe0f}\u{200d}\u{1f308}",
        // Without the variation selector.
        "\u{1f3f3}\u{200d}\u{1f308}",
        // Family with skin tones on every element.
        "\u{1f468}\u{1f3fb}\u{200d}\u{1f469}\u{1f3ff}\u{200d}\u{1f467}",
    ];
    for emoji in kept {
        assert_eq!(shown_text(emoji, 200), emoji, "{emoji:?}");
    }
    let hebrew = "שלום עולם, זה טקסט בעברית";
    assert_eq!(shown_text(hebrew, 200), hebrew);
    let arabic = "مرحبا بالعالم";
    assert_eq!(shown_text(arabic, 200), arabic);
    // A ZWJ at either end of the text has no pictograph on that side.
    assert_eq!(
        shown_text("\u{200d}\u{1f468}\u{200d}", 200),
        format!("{}\u{1f468}{}", marker('\u{200d}'), marker('\u{200d}'))
    );
}

#[test] // spec: APP-022
fn other_joiners_are_shown() {
    assert_eq!(
        shown_text("a\u{200d}\u{1f468}", 200),
        format!("a{}\u{1f468}", marker('\u{200d}'))
    );
    assert_eq!(
        shown_text("\u{1f468}\u{200d}b", 200),
        format!("\u{1f468}{}b", marker('\u{200d}'))
    );
    // Persian "I want" with its zero-width non-joiner.
    assert_eq!(
        shown_text("می\u{200c}خواهم", 200),
        format!("می{}خواهم", marker('\u{200c}'))
    );
    // A lone joiner between letters.
    assert_eq!(
        shown_text("a\u{200d}b", 200),
        format!("a{}b", marker('\u{200d}'))
    );
    // Two joiners in a row: neither is between two pictographs.
    assert_eq!(
        shown_text("\u{1f468}\u{200d}\u{200d}\u{1f469}", 200),
        format!("\u{1f468}{0}{0}\u{1f469}", marker('\u{200d}'))
    );
    // A modifier or U+FE0F counts only right after a pictograph, and only once.
    for (before, after) in [
        ("\u{1f3fd}", "\u{1f4bb}"),
        ("a\u{fe0f}", "\u{1f525}"),
        ("a\u{1f3fd}", "\u{1f4bb}"),
        ("\u{1f469}\u{1f3fd}\u{1f3fd}", "\u{1f4bb}"),
        ("\u{2764}\u{fe0f}\u{fe0f}", "\u{1f525}"),
        ("\u{2764}\u{fe0f}", "b"),
    ] {
        assert_eq!(
            shown_text(&format!("{before}\u{200d}{after}"), 200),
            format!("{before}{}{after}", marker('\u{200d}')),
            "{before:?} ZWJ {after:?}"
        );
    }
}

#[test] // spec: APP-022
fn cut_at_max_with_an_ellipsis() {
    let s = "x".repeat(201);
    let shown = shown_text(&s, 200);
    assert_eq!(shown.chars().count(), 200);
    assert_eq!(shown, format!("{}…", "x".repeat(199)));
    assert_eq!(shown_text(&"x".repeat(200), 200), "x".repeat(200));
    assert_eq!(shown_text(&"漢".repeat(300), 80).chars().count(), 80);

    // Markers count over the whole text: 190 letters and 2 markers of 8 characters are 206.
    let s = format!("{}\u{202e}\u{202e}", "x".repeat(190));
    let shown = shown_text(&s, 200);
    assert!(shown.chars().count() <= 200, "{shown}");
    assert!(shown.ends_with('…'), "{shown}");
    assert!(shown.starts_with(&format!("{}{}", "x".repeat(190), marker('\u{202e}'))));
    // A marker is never cut in half.
    assert!(!shown.contains("\u{27e8}U+202E…"), "{shown}");
    assert_eq!(
        shown_text(&format!("{}\u{202e}", "x".repeat(190)), 197)
            .chars()
            .count(),
        191
    );
    assert_eq!(
        shown_text(&format!("{}\u{202e}", "x".repeat(190)), 198)
            .chars()
            .count(),
        198
    );

    // A kept joiner cut from the pictograph after it is dropped before the `…`.
    let s = format!("{}\u{1f469}\u{200d}\u{1f4bb}y", "x".repeat(197));
    assert_eq!(
        shown_text(&s, 200),
        format!("{}\u{1f469}…", "x".repeat(197))
    );

    // Removed controls don't count.
    let s = format!("{}{}", "\u{7}".repeat(50), "x".repeat(200));
    assert_eq!(shown_text(&s, 200), "x".repeat(200));
    assert_eq!(shown_text("abc", 0), "");
    assert_eq!(shown_text("abc", 1), "…");
}
