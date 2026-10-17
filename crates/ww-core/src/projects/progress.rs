//! `progress.md` entries (CORE-039; Appendix E "`progress.md` entries").
//!
//! A new entry `- YYYY-MM-DD: <text>` goes on the line directly after `## Done`, newest first,
//! so the template's blank line stays between the last entry and the next heading, and every
//! other line stays byte for byte. A file without `## Done` gets the heading and the entry at
//! the end. The new lines use the file's line ending (that of its first line break; `\n` for
//! a file without one), so a CRLF file stays CRLF.

/// The `## Done` heading.
const DONE: &str = "## Done";

/// `text` with the entry `- <date>: <entry>` added under `## Done` (module docs).
///
/// # Errors
/// When `date` isn't `YYYY-MM-DD`, or `entry` is empty or holds a line break.
pub fn add_done_entry(text: &str, date: &str, entry: &str) -> Result<String, String> {
    let is_date = date.len() == 10
        && date.bytes().enumerate().all(|(i, b)| match i {
            4 | 7 => b == b'-',
            _ => b.is_ascii_digit(),
        });
    if !is_date {
        return Err(format!("{date:?} is not a YYYY-MM-DD date"));
    }
    if entry.trim().is_empty() || entry.contains(['\n', '\r']) {
        return Err(format!(
            "a progress entry is one non-empty line, not {entry:?}"
        ));
    }
    let eol = match text.find('\n') {
        Some(i) if i > 0 && text.as_bytes()[i - 1] == b'\r' => "\r\n",
        _ => "\n",
    };
    let line = format!("- {date}: {entry}{eol}");

    let mut at = 0;
    for piece in text.split_inclusive('\n') {
        let end = at + piece.len();
        if piece.trim_end_matches(['\n', '\r']).trim_end() == DONE {
            let mut out = String::with_capacity(text.len() + line.len() + eol.len());
            out.push_str(&text[..end]);
            if !piece.ends_with('\n') {
                out.push_str(eol);
            }
            out.push_str(&line);
            out.push_str(&text[end..]);
            return Ok(out);
        }
        at = end;
    }

    let mut out = text.to_string();
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push_str(eol);
        }
        if !out.ends_with(&format!("{eol}{eol}")) {
            out.push_str(eol);
        }
    }
    out.push_str(DONE);
    out.push_str(eol);
    out.push_str(&line);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: CORE-039
    fn entries_go_first_under_done() {
        let t = "# P\n\n## Done\n\n## Next\n";
        let one = add_done_entry(t, "2026-10-01", "a").unwrap();
        assert_eq!(one, "# P\n\n## Done\n- 2026-10-01: a\n\n## Next\n");
        let two = add_done_entry(&one, "2026-10-02", "b").unwrap();
        assert_eq!(
            two,
            "# P\n\n## Done\n- 2026-10-02: b\n- 2026-10-01: a\n\n## Next\n"
        );
        // A heading at the very end, without a line break.
        assert_eq!(
            add_done_entry("x\r\n## Done", "2026-10-01", "a").unwrap(),
            "x\r\n## Done\r\n- 2026-10-01: a\r\n"
        );
        // Trailing spaces after the heading are fine; `### Done` and `## Done now` aren't it.
        assert_eq!(
            add_done_entry("## Done  \n", "2026-10-01", "a").unwrap(),
            "## Done  \n- 2026-10-01: a\n"
        );
        assert_eq!(
            add_done_entry("### Done\n## Done now\n", "2026-10-01", "a").unwrap(),
            "### Done\n## Done now\n\n## Done\n- 2026-10-01: a\n"
        );
    }

    #[test] // spec: CORE-039
    fn a_file_without_done_gets_it_at_the_end() {
        assert_eq!(
            add_done_entry("", "2026-10-01", "a").unwrap(),
            "## Done\n- 2026-10-01: a\n"
        );
        assert_eq!(
            add_done_entry("# P\r\n\r\nnotes", "2026-10-01", "a").unwrap(),
            "# P\r\n\r\nnotes\r\n\r\n## Done\r\n- 2026-10-01: a\r\n"
        );
        assert_eq!(
            add_done_entry("# P\n\n", "2026-10-01", "a").unwrap(),
            "# P\n\n## Done\n- 2026-10-01: a\n"
        );
    }

    #[test] // spec: CORE-039
    fn bad_dates_and_entries_are_refused() {
        for (date, entry) in [
            ("2026-1-01", "a"),
            ("2026/10/01", "a"),
            ("2026-10-01", ""),
            ("2026-10-01", "a\nb"),
            ("2026-10-01", "a\r"),
        ] {
            assert!(
                add_done_entry("## Done\n", date, entry).is_err(),
                "{date} {entry:?}"
            );
        }
    }
}
