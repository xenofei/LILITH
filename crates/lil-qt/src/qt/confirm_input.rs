//! `ConfirmInput`, a QML singleton of module `Lilith.Core`: what a confirm dialog's buttons ask
//! before they answer (P4a-R10).
//!
//! - `nowMs()` is a monotonic clock, and `gateRefusal(kind, focused, visibleSinceMs, inFront,
//!   nowMs)` is [`crate::answer_gate`]'s verdict: `kind` is `pointer`, `space` or `enter`,
//!   `visibleSinceMs` below 0 means not up, and the result is empty, or the refusal's name.
//! - `watch(item)`, `unwatch(item)` and `takeClick(item)` are [`crate::pointer_gate`]'s, for the
//!   buttons that need a real pointer: a button is watched for as long as it exists, and watched
//!   again whenever its confirmation changes, so it counts as a new button. `takeClick` is empty
//!   when the hand made the click that just reached QML, or else names the refusal. The native
//!   event filter behind them is `src/qt/pointer_filter.cpp`, installed at the first `watch`.
//! - `refusalText(name, label)` is what the dialog shows next to its buttons, and
//!   `logRefusal(name)` the one log line, with nothing of the input.
//! - `seenMessages()` and `realMessages()` count what the gate has read (a test, a diagnostic).
//!
//! Everything runs on the GUI thread, and the gate's state lives in that thread's local, out of
//! reach of anything but this process's own code.

use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::Instant;

use cxx_qt_lib::{QString, QVariant};
use lil_platform::hello::Refusal;
use lil_platform::input_source;

use crate::answer_gate::{AnswerGate, AnswerRefusal, InputKind};
use crate::pointer_gate::{PointerGate, PointerKind, Seen, Target};

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
        fn take_click(self: &ConfirmInput, item: &QVariant) -> QString;

        #[qinvokable]
        fn seen_messages(self: &ConfirmInput) -> f64;

        #[qinvokable]
        fn real_messages(self: &ConfirmInput) -> f64;
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
        );

        /// Nothing was watched until now: what the gate knew is old.
        fn pointer_gate_reset();
    }
}

use qobject::{PointerMessage, pointer_serial, pointer_unwatch, pointer_watch};

/// `ConfirmInput`'s Rust side: it holds nothing of its own.
#[derive(Default)]
pub struct ConfirmInputRust;

thread_local! {
    static GATE: RefCell<PointerGate> = RefCell::new(PointerGate::new());
}

static START: OnceLock<Instant> = OnceLock::new();

/// A pointer gate refusal's name here: the click that brought the window forward isn't the same
/// thing as the answer gate's "not in front".
fn pointer_name(refusal: Refusal) -> &'static str {
    match refusal {
        Refusal::NotInFront => BROUGHT_FORWARD,
        other => other.name(),
    }
}

const BROUGHT_FORWARD: &str = "brought_forward";

/// A key on a button that takes only a pointer (`ApproveButton`).
const KEY: &str = "key";

fn refusal_words(name: &str, label: &str) -> String {
    let pointer = |refusal: Refusal| refusal.message(label);
    match name {
        BROUGHT_FORWARD => pointer(Refusal::NotInFront),
        "not_pointer" => pointer(Refusal::NotPointer),
        "no_real_press" => pointer(Refusal::NoRealPress),
        "no_rest" => pointer(Refusal::NoRest),
        "no_move" => pointer(Refusal::NoMove),
        KEY => format!(
            "No key answers {label}: click it with your mouse or touchpad. Deny works from the keyboard."
        ),
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
        START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
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
        tracing::info!(refusal = %name, "an answer to a confirmation didn't count");
    }

    fn watch(&self, item: &QVariant) {
        pointer_watch(item);
    }

    fn unwatch(&self, item: &QVariant) {
        pointer_unwatch(item);
    }

    fn take_click(&self, item: &QVariant) -> QString {
        let serial = pointer_serial(item);
        let taken = GATE.with(|g| g.borrow_mut().take_click(serial, Instant::now()));
        QString::from(taken.err().map_or("", pointer_name))
    }

    fn seen_messages(&self) -> f64 {
        GATE.with(|g| g.borrow().counts().seen as f64)
    }

    fn real_messages(&self) -> f64 {
        GATE.with(|g| g.borrow().counts().real as f64)
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
) {
    let kind = match kind {
        PointerMessage::Move => PointerKind::Move,
        PointerMessage::Press => PointerKind::Press,
        PointerMessage::Release => PointerKind::Release,
        PointerMessage::Leave => PointerKind::Leave,
        PointerMessage::CaptureChanged => PointerKind::CaptureChanged,
        PointerMessage::CancelMode => PointerKind::CancelMode,
        PointerMessage::MouseActivate => PointerKind::MouseActivate,
        _ => return,
    };
    // Armed on every move, so leaving always ends the rest and forgets where the cursor was.
    let tracked = kind == PointerKind::Move && input_source::track_leave(window);
    let seen = Seen {
        kind,
        msg: input_source::pointer_msg(window, lparam),
        in_front: input_source::in_front(window),
        tracked,
        target: (serial != 0).then_some(Target {
            serial,
            rect: (left, top, right, bottom),
            changed,
        }),
    };
    GATE.with(|g| g.borrow_mut().message(seen, Instant::now()));
}

fn pointer_gate_reset() {
    GATE.with(|g| g.borrow_mut().reset());
}
