//! When the confirm dialog takes an answer (P4a-R10), Qt-free: at least [`ANSWER_DELAY_MS`] after
//! it came up in front, while its window is in front, and for a key only on the focused button.
//! Why: a key already held down, or a click meant for the window underneath, must not answer.
//!
//! This is every answer's first rule, and it can't tell you from a program running as you. So
//! Approve, and a cap prompt's Allow once and Raise, also need a real pointer click
//! ([`crate::pointer_gate`]); Deny, and a cap prompt's Cancel, come from any source.
//!
//! [`AnswerGate::refusal`] names the reason, so a refused answer is never silent: the dialog shows
//! [`AnswerRefusal::message`] next to its buttons and logs [`AnswerRefusal::name`].

/// How long the dialog must have been up, in front, before it takes an answer.
pub const ANSWER_DELAY_MS: u64 = 500;

/// The input that asks to answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    PointerRelease,
    Space,
    Enter,
}

/// Why an answer wasn't taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerRefusal {
    /// The window isn't in front.
    NotInFront,
    /// The dialog came up less than [`ANSWER_DELAY_MS`] ago, or isn't up.
    TooSoon,
    /// A key, while another control has the keyboard focus.
    NotFocused,
}

impl AnswerRefusal {
    /// A stable name, for the log.
    pub fn name(self) -> &'static str {
        match self {
            AnswerRefusal::NotInFront => "not_in_front",
            AnswerRefusal::TooSoon => "too_soon",
            AnswerRefusal::NotFocused => "not_focused",
        }
    }

    /// What the dialog says, next to its buttons.
    pub fn message(self) -> &'static str {
        match self {
            AnswerRefusal::NotInFront => "Bring this window to the front first.",
            AnswerRefusal::TooSoon => {
                "Wait a moment: the buttons work half a second after this window comes to the front."
            }
            AnswerRefusal::NotFocused => "Move the keyboard focus to the button first.",
        }
    }
}

/// What the dialog knows when an input asks to answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnswerGate {
    /// When the dialog came up in front, since it last wasn't; `None` while it isn't.
    pub visible_since_ms: Option<u64>,
    /// QML's `Window.active`.
    pub in_front: bool,
}

impl AnswerGate {
    /// Whether this input may answer.
    pub fn accept(&self, kind: InputKind, focused_on_button: bool, now_ms: u64) -> bool {
        self.refusal(kind, focused_on_button, now_ms).is_none()
    }

    /// Why this input may not answer, if so. A window that isn't in front comes first: waiting
    /// wouldn't help.
    pub fn refusal(
        &self,
        kind: InputKind,
        focused_on_button: bool,
        now_ms: u64,
    ) -> Option<AnswerRefusal> {
        if !self.in_front {
            return Some(AnswerRefusal::NotInFront);
        }
        let up_long_enough = self
            .visible_since_ms
            .and_then(|since| now_ms.checked_sub(since))
            .is_some_and(|up| up >= ANSWER_DELAY_MS);
        if !up_long_enough {
            return Some(AnswerRefusal::TooSoon);
        }
        (kind != InputKind::PointerRelease && !focused_on_button)
            .then_some(AnswerRefusal::NotFocused)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [InputKind; 3] = [
        InputKind::PointerRelease,
        InputKind::Space,
        InputKind::Enter,
    ];

    fn gate(since: Option<u64>, in_front: bool) -> AnswerGate {
        AnswerGate {
            visible_since_ms: since,
            in_front,
        }
    }

    #[test] // spec: P4a-R10
    fn refuses_within_500_ms_of_showing() {
        for kind in KINDS {
            let up = gate(Some(1000), true);
            assert!(!up.accept(kind, true, 1000), "{kind:?}");
            assert!(!up.accept(kind, true, 1499), "{kind:?}");
            assert_eq!(up.refusal(kind, true, 1499), Some(AnswerRefusal::TooSoon));
            assert!(up.accept(kind, true, 1500), "{kind:?}");
        }
        // Not up at all, and a clock that reads before it came up.
        assert_eq!(
            gate(None, true).refusal(InputKind::PointerRelease, true, 9000),
            Some(AnswerRefusal::TooSoon)
        );
        assert!(!gate(Some(1000), true).accept(InputKind::PointerRelease, true, 10));
    }

    #[test] // spec: P4a-R10
    fn refuses_when_not_in_front() {
        for kind in KINDS {
            let behind = gate(Some(0), false);
            assert!(!behind.accept(kind, true, 60_000), "{kind:?}");
            assert_eq!(
                behind.refusal(kind, true, 60_000),
                Some(AnswerRefusal::NotInFront)
            );
        }
    }

    #[test] // spec: P4a-R10
    fn accepts_a_release_or_space_or_enter_on_the_focused_button_after_500_ms() {
        let up = gate(Some(0), true);
        assert!(up.accept(InputKind::PointerRelease, true, 500));
        assert!(up.accept(InputKind::Space, true, 500));
        assert!(up.accept(InputKind::Enter, true, 500));
        // A pointer needs no keyboard focus; a key does.
        assert!(up.accept(InputKind::PointerRelease, false, 500));
        for key in [InputKind::Space, InputKind::Enter] {
            assert_eq!(
                up.refusal(key, false, 500),
                Some(AnswerRefusal::NotFocused),
                "{key:?}"
            );
        }
    }

    #[test] // spec: P4a-R10
    fn answer_gate_names_each_refusal_in_words() {
        for (refusal, name, words) in [
            (AnswerRefusal::NotInFront, "not_in_front", "to the front"),
            (AnswerRefusal::TooSoon, "too_soon", "Wait a moment"),
            (AnswerRefusal::NotFocused, "not_focused", "keyboard focus"),
        ] {
            assert_eq!(refusal.name(), name);
            assert!(refusal.message().contains(words), "{refusal:?}");
        }
    }
}
