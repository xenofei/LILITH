//! The shown-text normaliser for notification text (Appendix G "Toast XML").

/// `s` with C0 and C1 controls removed, the bidirectional and zero-width characters Appendix G
/// lists shown as `⟨U+XXXX⟩` (a U+200D between two `Extended_Pictographic` code points is kept),
/// and at most `max_chars` `char`s over the whole text, ending `…` when cut.
pub fn shown_text(s: &str, max_chars: usize) -> String {
    let _ = (s, max_chars);
    todo!("plan 4a Task 4")
}
