//! `ConfirmInput`, a QML singleton of module `Lilith.Core`: what a confirm dialog's buttons ask
//! before they answer (P4a-R10), and what its card shows (APP-115).
//!
//! - `nowMs()` is a monotonic clock, and `gateRefusal(kind, focused, visibleSinceMs, inFront,
//!   nowMs)` is [`crate::answer_gate`]'s verdict: `kind` is `pointer`, `space` or `enter`,
//!   `visibleSinceMs` below 0 means not up, and the result is empty, or the refusal's name.
//! - `watch(item)`, `unwatch(item)`, `notePress(item)` and `takeClick(item)` are
//!   [`crate::pointer_gate`]'s, for the buttons that need a real pointer: a button is watched for as
//!   long as it exists, and watched again whenever what it stands for changes, so it counts as a new
//!   button. A button's press calls `notePress`, and its click calls `takeClick`, which is empty when
//!   the hand made that click, or else names the refusal. The native side behind them is
//!   `src/qt/pointer_filter.cpp`, installed at the first `watch`.
//! - `refusalText(name, label)` is what the dialog shows next to its buttons, and
//!   `logRefusal(name)` the one log line, with nothing of the input.
//! - `shownActions(json)` and `shownLine(text, max)` are [`crate::confirm_text`]'s.
//! - `seenMessages()`, `realMessages()` and `sentMessages()` count what the gate has read. Each press
//!   or release on a watched button writes one log line saying where Windows says it came from, the
//!   input's tick and `since_front_ms`, and the counts; each front change one line with the window
//!   and its tick, at most one a second per kind and path, so a burst logs only some of them:
//!   `tools/sandbox/confirm-check.ps1` reads them. No position and no text.
//! - `windowInFront(item)` and `frontGainedMs(item)` are Windows' own front for the item's window,
//!   which the answer bar decides by on Windows.
//!
//! Everything runs on the GUI thread, and the gate's state lives in that thread's local, out of
//! reach of anything but this process's own code.

use std::cell::{Cell, RefCell};
use std::sync::OnceLock;
use std::thread::LocalKey;
use std::time::Instant;

use cxx_qt_lib::{QString, QVariant};
use lil_platform::hello::is_real_pointer;
use lil_platform::input_source;

use crate::answer_gate::{AnswerGate, AnswerRefusal, InputKind};
use crate::confirm_text;
use crate::pointer_gate::{GateRefusal, LogLimit, PointerGate, PointerKind, Seen, Target};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        type ConfirmInput = super::ConfirmInputRust;

        #[qinvokable]
        fn now_ms(self: &ConfirmInput) -> f64;

        #[qinvokable]
        fn gate_refusal(
            self: &ConfirmInput,
            kind: &QString,
            focused: bool,
            visible_since_ms: f64,
            in_front: bool,
            now_ms: f64,
        ) -> QString;

        #[qinvokable]
        fn refusal_text(self: &ConfirmInput, name: &QString, label: &QString) -> QString;

        #[qinvokable]
        fn log_refusal(self: &ConfirmInput, name: &QString);

        #[qinvokable]
        fn watch(self: &ConfirmInput, item: &QVariant);

        #[qinvokable]
        fn unwatch(self: &ConfirmInput, item: &QVariant);

        #[qinvokable]
        fn note_press(self: &ConfirmInput, item: &QVariant);

        #[qinvokable]
        fn take_click(self: &ConfirmInput, item: &QVariant) -> QString;

        #[qinvokable]
        fn shown_actions(self: &ConfirmInput, json: &QString) -> QString;

        #[qinvokable]
        fn shown_line(self: &ConfirmInput, text: &QString, max_chars: i32) -> QString;

        #[qinvokable]
        fn raise_refusal(
            self: &ConfirmInput,
            amount: &QString,
            limit: &QString,
            currency: &QString,
        ) -> QString;

        #[qinvokable]
        fn seen_messages(self: &ConfirmInput) -> f64;

        #[qinvokable]
        fn real_messages(self: &ConfirmInput) -> f64;

        #[qinvokable]
        fn sent_messages(self: &ConfirmInput) -> f64;

        /// Whether the item's window is the foreground window, by Windows itself.
        #[qinvokable]
        fn window_in_front(self: &ConfirmInput, item: &QVariant) -> bool;

        /// When the item's top-level window last came to the front, by Windows itself, on
        /// now_ms()'s clock; -1 before that.
        #[qinvokable]
        fn front_gained_ms(self: &ConfirmInput, item: &QVariant) -> f64;
    }

    /// Which pointer message the filter read.
    #[namespace = "lil_qt"]
    enum PointerMessage {
        Move,
        Press,
        Release,
        Leave,
        CaptureChanged,
        CancelMode,
        MouseActivate,
        FrontGained,
        FrontClicked,
        FrontLost,
    }

    #[namespace = "lil_qt"]
    unsafe extern "C++" {
        include!("lil_qt/src/qt/pointer_filter.h");

        /// Watches the item in `item` (a new serial if it was watched already).
        #[cxx_name = "pointerWatch"]
        fn pointer_watch(item: &QVariant);

        #[cxx_name = "pointerUnwatch"]
        fn pointer_unwatch(item: &QVariant);

        /// The item's serial, or 0 when it isn't watched.
        #[cxx_name = "pointerSerial"]
        fn pointer_serial(item: &QVariant) -> u64;

        /// Whether the item's window is the foreground window (always, off Windows).
        #[cxx_name = "pointerWindowInFront"]
        fn pointer_window_in_front(item: &QVariant) -> bool;

        /// The item's top-level window (`GA_ROOT`), or 0.
        #[cxx_name = "pointerWindowRoot"]
        fn pointer_window_root(item: &QVariant) -> isize;
    }

    #[namespace = "lil_qt"]
    extern "Rust" {
        /// One pointer message to `window`, before Qt sees it. `serial` is the watched button
        /// under the cursor (0: none), with its rectangle on the screen and whether it moved.
        #[allow(clippy::too_many_arguments)]
        fn pointer_message(
            kind: PointerMessage,
            window: isize,
            lparam: isize,
            serial: u64,
            left: i32,
            top: i32,
            right: i32,
            bottom: i32,
            changed: bool,
            via_hook: bool,
            root: isize,
            time: u32,
        );

        /// A mouse button message was sent to a window of this thread, past the filter.
        fn pointer_sent();

        /// Whether the hook that sees sent messages is set.
        fn pointer_guard(ok: bool);

        /// Nothing was watched until now: what the gate knew is old.
        fn pointer_gate_reset();
    }
}

use qobject::{
    PointerMessage, pointer_serial, pointer_unwatch, pointer_watch, pointer_window_in_front,
    pointer_window_root,
};

/// `ConfirmInput`'s Rust side: it holds nothing of its own.
#[derive(Default)]
pub struct ConfirmInputRust;

thread_local! {
    static GATE: RefCell<PointerGate> = RefCell::new(PointerGate::new());
    /// The button the last move was on, for the log.
    static LAST_ON: Cell<u64> = const { Cell::new(0) };
    static SENT_LINES: Cell<LogLimit> = const { Cell::new(LogLimit::new()) };
    static PRESS_LINES: Cell<LogLimit> = const { Cell::new(LogLimit::new()) };
    static RELEASE_LINES: Cell<LogLimit> = const { Cell::new(LogLimit::new()) };
    static ARRIVAL_LINES: Cell<LogLimit> = const { Cell::new(LogLimit::new()) };
    static REFUSAL_LINES: Cell<LogLimit> = const { Cell::new(LogLimit::new()) };
    /// Front lines, by kind and by which path saw the message (filter, hook).
    static FRONT_LINES: Cell<[LogLimit; 8]> = const { Cell::new([LogLimit::new(); 8]) };
    /// When each top-level window of the app last came to the front, in now_ms()'s milliseconds.
    static FRONT_GAINED_AT: RefCell<Vec<(isize, f64)>> = const { RefCell::new(Vec::new()) };
}

fn log_due(limit: &'static LocalKey<Cell<LogLimit>>) -> bool {
    limit.with(|cell| {
        let mut limit = cell.get();
        let due = limit.due(Instant::now());
        cell.set(limit);
        due
    })
}

static START: OnceLock<Instant> = OnceLock::new();

fn ms_since_start() -> f64 {
    START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

fn refusal_words(name: &str, label: &str) -> String {
    if let Some(words) = GateRefusal::words(name, label) {
        return words;
    }
    match name {
        // A key on a button that takes only a pointer (`ApproveButton`).
        "key" => format!(
            "No key answers {label}: click it with your mouse or touchpad. Deny works from the keyboard."
        ),
        // What the button says changed between its press and its release (`ApproveButton`).
        "changed" => format!(
            "What {label} says changed while it was being clicked, so the click didn't count. Check it, then click again."
        ),
        // The amount field (`CapPrompt`).
        "not_a_number" => "Enter the new limit as a number, like 25 or 12.50.".to_string(),
        "too_many_decimals" => {
            "That has more decimals than this currency uses. Enter it as the button should say it."
                .to_string()
        }
        "too_large" => "That amount is too large.".to_string(),
        "not_higher" => {
            "That isn't higher than the limit now, so it wouldn't be a raise. Enter a higher one, or use Allow once or Cancel."
                .to_string()
        }
        "not_in_front" => AnswerRefusal::NotInFront.message().to_string(),
        "too_soon" => AnswerRefusal::TooSoon.message().to_string(),
        "not_focused" => AnswerRefusal::NotFocused.message().to_string(),
        _ => format!("That didn't count. Click {label} again."),
    }
}

fn input_kind(kind: &str) -> InputKind {
    match kind {
        "space" => InputKind::Space,
        "enter" => InputKind::Enter,
        _ => InputKind::PointerRelease,
    }
}

impl qobject::ConfirmInput {
    fn now_ms(&self) -> f64 {
        ms_since_start()
    }

    fn gate_refusal(
        &self,
        kind: &QString,
        focused: bool,
        visible_since_ms: f64,
        in_front: bool,
        now_ms: f64,
    ) -> QString {
        let gate = AnswerGate {
            visible_since_ms: (visible_since_ms >= 0.0).then_some(visible_since_ms as u64),
            in_front,
        };
        let refusal = gate.refusal(input_kind(&kind.to_string()), focused, now_ms as u64);
        QString::from(refusal.map_or("", AnswerRefusal::name))
    }

    fn refusal_text(&self, name: &QString, label: &QString) -> QString {
        QString::from(refusal_words(&name.to_string(), &label.to_string()).as_str())
    }

    fn log_refusal(&self, name: &QString) {
        if !log_due(&REFUSAL_LINES) {
            return;
        }
        let counts = GATE.with(|g| g.borrow().counts());
        tracing::info!(
            refusal = %name,
            seen = counts.seen,
            real = counts.real,
            sent = counts.sent,
            "an answer to a confirmation didn't count"
        );
    }

    fn watch(&self, item: &QVariant) {
        pointer_watch(item);
    }

    fn unwatch(&self, item: &QVariant) {
        pointer_unwatch(item);
    }

    fn note_press(&self, item: &QVariant) {
        let serial = pointer_serial(item);
        GATE.with(|g| g.borrow_mut().take_press(serial, Instant::now()));
    }

    fn take_click(&self, item: &QVariant) -> QString {
        let serial = pointer_serial(item);
        let taken = GATE.with(|g| g.borrow_mut().take_click(serial, Instant::now()));
        if taken.is_ok() {
            let counts = GATE.with(|g| g.borrow().counts());
            tracing::info!(
                seen = counts.seen,
                real = counts.real,
                sent = counts.sent,
                "a real click on a confirmation's button"
            );
        }
        QString::from(taken.err().map_or("", GateRefusal::name))
    }

    fn shown_actions(&self, json: &QString) -> QString {
        QString::from(confirm_text::shown_actions_json(&json.to_string()).as_str())
    }

    fn shown_line(&self, text: &QString, max_chars: i32) -> QString {
        let max = usize::try_from(max_chars).unwrap_or(0);
        QString::from(confirm_text::shown_line(&text.to_string(), max).as_str())
    }

    fn raise_refusal(&self, amount: &QString, limit: &QString, currency: &QString) -> QString {
        let refusal = confirm_text::raise_refusal(
            &amount.to_string(),
            &limit.to_string(),
            &currency.to_string(),
        );
        QString::from(refusal.unwrap_or(""))
    }

    fn seen_messages(&self) -> f64 {
        GATE.with(|g| g.borrow().counts().seen as f64)
    }

    fn real_messages(&self) -> f64 {
        GATE.with(|g| g.borrow().counts().real as f64)
    }

    fn sent_messages(&self) -> f64 {
        GATE.with(|g| g.borrow().counts().sent as f64)
    }

    fn window_in_front(&self, item: &QVariant) -> bool {
        pointer_window_in_front(item)
    }

    fn front_gained_ms(&self, item: &QVariant) -> f64 {
        let root = pointer_window_root(item);
        FRONT_GAINED_AT.with(|fronts| {
            fronts
                .borrow()
                .iter()
                .find(|&&(w, _)| w == root)
                .map_or(-1.0, |&(_, at)| at)
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn pointer_message(
    kind: PointerMessage,
    window: isize,
    lparam: isize,
    serial: u64,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    changed: bool,
    via_hook: bool,
    root: isize,
    time: u32,
) {
    let kind = match kind {
        PointerMessage::Move => PointerKind::Move,
        PointerMessage::Press => PointerKind::Press,
        PointerMessage::Release => PointerKind::Release,
        PointerMessage::Leave => PointerKind::Leave,
        PointerMessage::CaptureChanged => PointerKind::CaptureChanged,
        PointerMessage::CancelMode => PointerKind::CancelMode,
        PointerMessage::MouseActivate => PointerKind::MouseActivate,
        PointerMessage::FrontGained => PointerKind::FrontGained,
        PointerMessage::FrontClicked => PointerKind::FrontClicked,
        PointerMessage::FrontLost => PointerKind::FrontLost,
        _ => return,
    };
    // The source first of what is read here. The filter has already read the cursor and where the
    // buttons are, and none of that takes a message, so this is still the source of this one.
    let msg = input_source::pointer_msg(window, lparam);
    let real = is_real_pointer(msg.origin, msg.device, msg.sent);
    // Only a press and a release are judged by these, so a move doesn't pay for them.
    let judged = matches!(kind, PointerKind::Press | PointerKind::Release);
    let physically_down = judged && input_source::left_button_physically_down();
    // Armed on every move, so leaving always ends the rest and forgets where the cursor was.
    let tracked = kind == PointerKind::Move && input_source::track_leave(window);
    let exposed = judged && input_source::exposed_at(window, msg.at);
    let in_front = input_source::in_front(window);
    let seen = Seen {
        kind,
        msg,
        in_front,
        exposed,
        physically_down,
        tracked,
        target: (serial != 0).then_some(Target {
            serial,
            rect: (left, top, right, bottom),
            changed,
        }),
        window: root,
        time,
    };
    let (counts, came_forward, since_front) = GATE.with(|g| {
        let mut gate = g.borrow_mut();
        // Before it is judged: whether this press would be the one that brought the window forward
        // by what came before it, and how long after its window last came to the front it happened.
        let came_forward = gate.came_forward();
        let since_front = gate.ms_since_front(root, time, Instant::now());
        gate.message(seen, Instant::now());
        (gate.counts(), came_forward, since_front)
    });
    // Coming to the front, by Windows itself: the answer gate's clock starts again from here for
    // that window, and each change gets a line, so a real run shows what the gate was told, when,
    // and by which path.
    let gained = matches!(kind, PointerKind::FrontGained | PointerKind::FrontClicked);
    if gained || matches!(kind, PointerKind::FrontLost | PointerKind::MouseActivate) {
        if gained {
            let at = ms_since_start();
            FRONT_GAINED_AT.with(|fronts| {
                let mut fronts = fronts.borrow_mut();
                fronts.retain(|&(w, _)| w != root);
                fronts.push((root, at));
            });
        }
        let index = match kind {
            PointerKind::FrontGained => 0,
            PointerKind::FrontClicked => 1,
            PointerKind::FrontLost => 2,
            _ => 3,
        } + if via_hook { 4 } else { 0 };
        let due = FRONT_LINES.with(|cell| {
            let mut limits = cell.get();
            let due = limits[index].due(Instant::now());
            cell.set(limits);
            due
        });
        if due {
            let what = match kind {
                PointerKind::FrontGained => "a window of the app came to the front",
                PointerKind::FrontClicked => "a click brought a window of the app to the front",
                PointerKind::FrontLost => "a window of the app left the front",
                _ => "a click is bringing a window of the app to the front",
            };
            tracing::info!(
                via = if via_hook { "hook" } else { "filter" },
                // Which top-level window, as Windows numbers it for this session: nothing personal.
                window = format!("{root:#x}"),
                tick = time,
                on_button = serial != 0,
                in_front,
                in_send = msg.sent,
                seen = counts.seen,
                "{what}"
            );
        }
    }
    // One line each time the pointer arrives on a button, whatever brought it (the script's rest).
    if kind == PointerKind::Move {
        let arrived = serial != 0 && LAST_ON.replace(serial) != serial;
        if serial == 0 {
            LAST_ON.set(0);
        }
        if arrived && (real || log_due(&ARRIVAL_LINES)) {
            tracing::info!(
                origin = ?msg.origin,
                device = ?msg.device,
                in_send = msg.sent,
                // No position: whether the message's own point is where the cursor is now.
                on_time = msg.message_at == msg.at,
                seen = counts.seen,
                real = counts.real,
                sent = counts.sent,
                "a move onto a confirmation's button"
            );
        }
    }
    // A hand's are always worth a line, and come at a hand's rate. A program's come as often as it
    // likes, a press and a release each limited on their own, so a pair still shows both.
    let line_due = serial != 0
        && judged
        && (real
            || log_due(if kind == PointerKind::Press {
                &PRESS_LINES
            } else {
                &RELEASE_LINES
            }));
    if line_due {
        tracing::info!(
            kind = ?kind,
            origin = ?msg.origin,
            device = ?msg.device,
            in_send = msg.sent,
            physically_down,
            by_messages_down = input_source::left_button_down_by_messages(),
            in_front,
            exposed,
            came_forward,
            // By the input's own time, below 0 when the press came before the front. `front_known`
            // false: its window hasn't come to the front while watched, in the last minute.
            front_known = since_front.is_some(),
            since_front_ms = since_front.map_or(0, i64::from),
            // When the input happened, on GetTickCount's clock: what a script compares with its own.
            tick = time,
            seen = counts.seen,
            real = counts.real,
            sent = counts.sent,
            "a press or release on a confirmation's button"
        );
    }
}

fn pointer_sent() {
    let counts = GATE.with(|g| {
        let mut gate = g.borrow_mut();
        gate.sent_input();
        gate.counts()
    });
    if log_due(&SENT_LINES) {
        tracing::info!(
            seen = counts.seen,
            real = counts.real,
            sent = counts.sent,
            "a mouse button message was sent to a window of the app"
        );
    }
}

fn pointer_guard(ok: bool) {
    GATE.with(|g| g.borrow_mut().guard_set(ok));
    if !ok {
        tracing::error!("the hook that sees sent mouse messages couldn't be set: Approve is off");
    }
}

fn pointer_gate_reset() {
    GATE.with(|g| g.borrow_mut().reset());
    // What the windows' fronts were before anything was watched is old too.
    FRONT_GAINED_AT.with(|fronts| fronts.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::{confirm_text, refusal_words};

    // Each refusal of the amount field reads as its own words, never the catch-all "click again".
    #[test] // spec: APP-054
    fn each_raise_refusal_has_its_own_words() {
        for (amount, words) in [
            ("lots", "Enter the new limit as a number, like 25 or 12.50."),
            (
                "25.999",
                "That has more decimals than this currency uses. Enter it as the button should say it.",
            ),
            ("9223372036854.78", "That amount is too large."),
            (
                "1",
                "That isn't higher than the limit now, so it wouldn't be a raise. Enter a higher one, or use Allow once or Cancel.",
            ),
        ] {
            let name = confirm_text::raise_refusal(amount, "1", "USD").expect(amount);
            assert_eq!(refusal_words(name, "Raise the cap"), words, "{amount}");
        }
    }
}
