//! Whether a click on Approve (or a cap prompt's Allow once or Raise) came from the owner's hand
//! (P4a-R10), Qt-free: the native side of the answer's gate. [`crate::answer_gate`] can't tell you
//! from a program running as you. This can tell, as far as Windows can.
//!
//! It stands on the Windows Hello window's rules ([`ClickGate`]), audited there and not repeated
//! here: only a press and a release that Windows reports as `IMO_HARDWARE` from a mouse or
//! touchpad, handled on the window's own thread, after a real rest on the button. Posted and sent
//! messages, `SendInput` and a warped cursor all fail one of those. Raw input stays off (that
//! window's own rule without it): raw input goes to one window per process, and the shell has many.
//! A still hand whose rest Windows ended with a move of its own needs a small move, and the dialog
//! says so.
//!
//! **How a click reaches it.** QML's event comes after the message, through Qt's queue, and by then
//! nothing can say where it came from. So the native event filter (`src/qt/pointer_filter.cpp`)
//! gives every pointer message to [`PointerGate::message`] before Qt sees it, and a real release on
//! a button leaves a token for that button. The button's `onClicked` takes it
//! ([`PointerGate::take_click`]), and only a token answers. A posted, sent or synthetic release
//! still reaches QML, finds no token, and is refused; UI Automation's Invoke and the keyboard
//! never ask for one. The token is used once, dies at the next press and after
//! [`TOKEN_LIFETIME`], and names one button, so it can only ever answer what the hand clicked.
//!
//! **What this adds to the Hello window's rules**, since the buttons here aren't windows of their
//! own: where a button is comes from Qt at each message. A button that moved since the last message
//! (a card arrived above it, the list scrolled, its window moved), or that wasn't there, starts
//! again: nothing can slide Approve under a still cursor. A button is new each time its
//! confirmation changes (`Target::serial`), so a click on the card that took another's place
//! doesn't count. A press that brought the window to the front (`WM_MOUSEACTIVATE`), or with
//! another window in front (`GetForegroundWindow`, which a program can't fake with a message), is
//! refused.
//!
//! Touch and pen never count: Qt takes them as `WM_POINTER*`, which aren't read here, so a tap on
//! Approve says it needs the mouse or touchpad.

use std::time::{Duration, Instant};

use lil_platform::hello::{ClickGate, PointerMsg, Refusal};

/// How long a real release's token waits for its `onClicked`, which comes on the same thread right
/// after it. A stalled GUI thread can use this up: the click is refused, and says so.
pub const TOKEN_LIFETIME: Duration = Duration::from_secs(1);

/// The kinds of message the gate reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerKind {
    /// `WM_MOUSEMOVE`.
    Move,
    /// `WM_LBUTTONDOWN`, `WM_LBUTTONDBLCLK`.
    Press,
    /// `WM_LBUTTONUP`.
    Release,
    /// `WM_MOUSELEAVE`.
    Leave,
    /// `WM_CAPTURECHANGED`.
    CaptureChanged,
    /// `WM_CANCELMODE`.
    CancelMode,
    /// `WM_MOUSEACTIVATE` for a left press.
    MouseActivate,
}

/// The button under the cursor, as Qt places it now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// Which button, for which confirmation: a new one each time its confirmation changes.
    pub serial: u64,
    /// `(left, top, right, bottom)` in the screen's physical pixels.
    pub rect: (i32, i32, i32, i32),
    /// It moved since the last message, or wasn't seen before.
    pub changed: bool,
}

/// One message, as the gate reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seen {
    pub kind: PointerKind,
    pub msg: PointerMsg,
    /// The message's window is the foreground window.
    pub in_front: bool,
    /// `TME_LEAVE` is armed on the message's window (moves only).
    pub tracked: bool,
    pub target: Option<Target>,
}

/// How many messages it has read, and how many of them Windows reported as real: what a test
/// and a diagnostic can check. No position, no timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub seen: u64,
    pub real: u64,
}

/// See the module docs.
#[derive(Debug, Default)]
pub struct PointerGate {
    gate: ClickGate,
    /// The button the last move was on.
    current: Option<u64>,
    /// `WM_MOUSEACTIVATE` came: the next press brought the window to the front.
    activated: bool,
    /// Why the press that is down doesn't count, beyond what [`ClickGate`] judges.
    press_refused: Option<Refusal>,
    /// A real release was the last thing handled, until the next message.
    releasing: bool,
    token: Option<(u64, Instant)>,
    refused: Option<(u64, Refusal, Instant)>,
    counts: Counts,
}

fn contains(rect: (i32, i32, i32, i32), point: Option<(i32, i32)>) -> bool {
    point.is_some_and(|(x, y)| x >= rect.0 && x < rect.2 && y >= rect.1 && y < rect.3)
}

fn fresh(at: Instant, now: Instant) -> bool {
    now.checked_duration_since(at)
        .is_some_and(|age| age <= TOKEN_LIFETIME)
}

impl PointerGate {
    pub fn new() -> PointerGate {
        PointerGate::default()
    }

    /// One pointer message, before Qt sees it.
    pub fn message(&mut self, s: Seen, now: Instant) {
        if self.releasing && s.kind != PointerKind::CaptureChanged {
            self.releasing = false;
            self.gate.release_done();
        }
        self.counts.seen += 1;
        if s.msg.origin == lil_platform::hello::InputOrigin::Hardware && !s.msg.sent {
            self.counts.real += 1;
        }
        if s.target.is_some_and(|t| t.changed) {
            self.gate.restart(s.msg.at);
            self.press_refused = None;
            self.token = None;
        }
        let serial = s.target.map(|t| t.serial);
        // On the button, by where the cursor really is and where the message says it was.
        let inside = s
            .target
            .is_some_and(|t| contains(t.rect, s.msg.at) && contains(t.rect, s.msg.message_at));
        match s.kind {
            PointerKind::Move => {
                // From one button straight to another: the rest was on the first.
                if serial.is_some() && self.current.is_some() && serial != self.current {
                    self.gate.button_move(false, s.msg, now);
                }
                self.gate.button_move(inside, s.msg, now);
                self.gate.button_tracking(s.tracked);
                self.current = serial;
            }
            PointerKind::Press => {
                self.token = None;
                self.refused = None;
                self.press_refused = (std::mem::take(&mut self.activated) || !s.in_front)
                    .then_some(Refusal::NotInFront);
                self.gate.button_press(inside, s.msg, now);
            }
            PointerKind::Release => {
                let (armed, refusal) = self.gate.button_release(inside, s.msg);
                self.releasing = true;
                let refusal = self.press_refused.take().or(refusal);
                match (serial, armed, refusal) {
                    (Some(serial), true, None) => self.token = Some((serial, now)),
                    (Some(serial), _, Some(refusal)) => {
                        self.refused = Some((serial, refusal, now));
                    }
                    _ => {}
                }
            }
            PointerKind::Leave => {
                self.gate.button_leave();
                self.current = None;
            }
            PointerKind::CaptureChanged => self.gate.capture_changed(),
            PointerKind::CancelMode => self.gate.cancel_mode(),
            PointerKind::MouseActivate => self.activated = true,
        }
    }

    /// A click on button `serial` reached QML: `Ok` when the hand made it, used up here, else
    /// which rule refused it.
    pub fn take_click(&mut self, serial: u64, now: Instant) -> Result<(), Refusal> {
        let token = self.token.take();
        let refused = self.refused.take();
        if token.is_some_and(|(s, at)| s == serial && fresh(at, now)) {
            return Ok(());
        }
        match refused {
            Some((s, refusal, at)) if s == serial && fresh(at, now) => Err(refusal),
            _ => Err(Refusal::NotPointer),
        }
    }

    /// Forgets everything but the counts: nothing was watched until now, so what it knew is old.
    pub fn reset(&mut self) {
        *self = PointerGate {
            counts: self.counts,
            ..PointerGate::default()
        };
    }

    pub fn counts(&self) -> Counts {
        self.counts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lil_platform::hello::{HOVER_BEFORE_PRESS, InputDevice, InputOrigin};

    const RECT: (i32, i32, i32, i32) = (100, 100, 200, 130);
    const ON: (i32, i32) = (150, 115);
    const OFF: (i32, i32) = (50, 50);

    fn msg(origin: InputOrigin, device: InputDevice, sent: bool, at: (i32, i32)) -> PointerMsg {
        PointerMsg {
            origin,
            device,
            sent,
            at: Some(at),
            message_at: Some(at),
        }
    }

    fn hardware(at: (i32, i32)) -> PointerMsg {
        msg(InputOrigin::Hardware, InputDevice::Mouse, false, at)
    }

    fn injected(at: (i32, i32)) -> PointerMsg {
        msg(InputOrigin::Injected, InputDevice::Mouse, false, at)
    }

    fn posted(at: (i32, i32)) -> PointerMsg {
        msg(
            InputOrigin::Unavailable,
            InputDevice::Unavailable,
            false,
            at,
        )
    }

    /// Replays a hand's messages at given times, as the filter would give them.
    struct Replay {
        gate: PointerGate,
        now: Instant,
    }

    impl Replay {
        fn new() -> Replay {
            Replay {
                gate: PointerGate::new(),
                now: Instant::now(),
            }
        }

        fn advance(&mut self, by: Duration) {
            self.now += by;
        }

        fn seen(&mut self, kind: PointerKind, m: PointerMsg, target: Option<Target>) {
            let s = Seen {
                kind,
                msg: m,
                in_front: true,
                tracked: true,
                target,
            };
            self.gate.message(s, self.now);
        }

        fn target_at(at: (i32, i32)) -> Option<Target> {
            contains(RECT, Some(at)).then_some(Target {
                serial: 1,
                rect: RECT,
                changed: false,
            })
        }

        fn mv(&mut self, m: PointerMsg) {
            let at = m.at.unwrap();
            self.seen(PointerKind::Move, m, Self::target_at(at));
        }

        fn press(&mut self, m: PointerMsg) {
            let at = m.at.unwrap();
            self.seen(PointerKind::Press, m, Self::target_at(at));
        }

        fn release(&mut self, m: PointerMsg) {
            let at = m.at.unwrap();
            self.seen(PointerKind::Release, m, Self::target_at(at));
        }

        /// The hand comes from outside, moves on the button and rests.
        fn rest_on_the_button(&mut self) {
            self.mv(hardware(OFF));
            self.advance(Duration::from_millis(16));
            self.mv(hardware((101, 101)));
            self.advance(Duration::from_millis(16));
            self.mv(hardware(ON));
            self.advance(HOVER_BEFORE_PRESS);
        }

        fn take(&mut self) -> Result<(), Refusal> {
            self.gate.take_click(1, self.now)
        }
    }

    #[test] // spec: P4a-R10
    fn accepts_a_real_click_after_a_real_rest_once() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.advance(Duration::from_millis(80));
        r.release(hardware(ON));
        assert_eq!(r.take(), Ok(()));
        assert_eq!(r.take(), Err(Refusal::NotPointer), "the token is used up");
    }

    #[test] // spec: P4a-R10
    fn accepts_a_second_click_with_a_still_hand_only_after_its_own_press() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.release(hardware(ON));
        // Qt's capture goes back within the release, and Windows' own move comes next.
        r.seen(
            PointerKind::CaptureChanged,
            posted(ON),
            Replay::target_at(ON),
        );
        assert_eq!(r.take(), Ok(()));
        r.press(hardware(ON));
        r.release(hardware(ON));
        assert_eq!(
            r.take(),
            Ok(()),
            "the rest stays over a click: {:?}",
            r.gate
        );
    }

    #[test] // spec: P4a-R10
    fn refuses_a_click_that_is_only_a_qt_event() {
        // UI Automation, the QML test runner's mouseClick, a sent message: no message came here.
        let mut r = Replay::new();
        r.rest_on_the_button();
        assert_eq!(r.take(), Err(Refusal::NotPointer));
    }

    #[test] // spec: P4a-R10
    fn refuses_posted_messages_whatever_the_hand_did_before() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(posted(ON));
        r.release(posted(ON));
        assert_eq!(r.take(), Err(Refusal::NotPointer));
        // A real press under a posted release.
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.release(posted(ON));
        assert_eq!(r.take(), Err(Refusal::NotPointer));
    }

    #[test] // spec: P4a-R10
    fn refuses_injected_input() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(injected(ON));
        r.release(injected(ON));
        assert_eq!(r.take(), Err(Refusal::NotPointer));
        // An injected press under a real release.
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(injected(ON));
        r.release(hardware(ON));
        assert_eq!(r.take(), Err(Refusal::NoRealPress));
        // A real press under an injected release.
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.release(injected(ON));
        assert_eq!(r.take(), Err(Refusal::NotPointer));
    }

    #[test] // spec: P4a-R10
    fn refuses_a_warp_onto_the_button() {
        // SetCursorPos: Windows reports its move as its own (measured on build 26300).
        let mut r = Replay::new();
        r.mv(hardware(OFF));
        r.advance(Duration::from_secs(1));
        r.mv(msg(InputOrigin::System, InputDevice::Mouse, false, ON));
        r.advance(Duration::from_secs(1));
        r.press(hardware(ON));
        r.release(hardware(ON));
        assert_eq!(r.take(), Err(Refusal::NoMove));
    }

    #[test] // spec: P4a-R10
    fn refuses_a_press_too_soon_after_arriving() {
        let mut r = Replay::new();
        r.mv(hardware(OFF));
        r.mv(hardware((101, 101)));
        r.mv(hardware(ON));
        r.advance(HOVER_BEFORE_PRESS - Duration::from_millis(1));
        r.press(hardware(ON));
        r.release(hardware(ON));
        assert_eq!(r.take(), Err(Refusal::NoRest));
    }

    #[test] // spec: P4a-R10
    fn refuses_a_button_that_slid_under_a_still_cursor() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        // A card arrived above: the button moved, and the next message sees it.
        let moved = Target {
            serial: 1,
            rect: (100, 90, 200, 120),
            changed: true,
        };
        r.seen(PointerKind::Press, hardware(ON), Some(moved));
        let same = Target {
            changed: false,
            ..moved
        };
        r.seen(PointerKind::Release, hardware(ON), Some(same));
        assert_eq!(r.take(), Err(Refusal::NoMove));
    }

    #[test] // spec: P4a-R10
    fn refuses_a_token_for_another_button() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.release(hardware(ON));
        assert_eq!(r.gate.take_click(2, r.now), Err(Refusal::NotPointer));
        assert_eq!(r.take(), Err(Refusal::NotPointer), "and it is gone");
    }

    #[test] // spec: P4a-R10
    fn refuses_a_new_card_in_the_old_cards_place() {
        // The same place, another confirmation's button: a new serial, seen as changed.
        let mut r = Replay::new();
        r.rest_on_the_button();
        let new_card = Target {
            serial: 2,
            rect: RECT,
            changed: true,
        };
        r.seen(PointerKind::Press, hardware(ON), Some(new_card));
        let same = Target {
            changed: false,
            ..new_card
        };
        r.seen(PointerKind::Release, hardware(ON), Some(same));
        assert_eq!(r.gate.take_click(2, r.now), Err(Refusal::NoMove));
    }

    #[test] // spec: P4a-R10
    fn refuses_a_rest_carried_from_one_button_to_the_next() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        let next = Target {
            serial: 2,
            rect: (200, 100, 300, 130),
            changed: false,
        };
        r.seen(PointerKind::Move, hardware((210, 115)), Some(next));
        r.seen(PointerKind::Press, hardware((210, 115)), Some(next));
        r.seen(PointerKind::Release, hardware((210, 115)), Some(next));
        assert_eq!(r.gate.take_click(2, r.now), Err(Refusal::NoMove));
    }

    #[test] // spec: P4a-R10
    fn refuses_the_click_that_brought_the_window_forward() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.seen(
            PointerKind::MouseActivate,
            posted(ON),
            Replay::target_at(ON),
        );
        r.press(hardware(ON));
        r.release(hardware(ON));
        assert_eq!(r.take(), Err(Refusal::NotInFront));
        // The next one counts.
        r.press(hardware(ON));
        r.release(hardware(ON));
        assert_eq!(r.take(), Ok(()));
    }

    #[test] // spec: P4a-R10
    fn refuses_a_press_with_another_window_in_front() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        let s = Seen {
            kind: PointerKind::Press,
            msg: hardware(ON),
            in_front: false,
            tracked: true,
            target: Replay::target_at(ON),
        };
        r.gate.message(s, r.now);
        r.release(hardware(ON));
        assert_eq!(r.take(), Err(Refusal::NotInFront));
    }

    #[test] // spec: P4a-R10
    fn refuses_a_token_past_its_lifetime_or_after_a_new_press() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.release(hardware(ON));
        r.advance(TOKEN_LIFETIME + Duration::from_millis(1));
        assert_eq!(r.take(), Err(Refusal::NotPointer));
        // A release that left a token; a new press, posted, drops it.
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.release(hardware(ON));
        r.press(posted(ON));
        assert_eq!(r.take(), Err(Refusal::NotPointer));
    }

    #[test] // spec: P4a-R10
    fn refuses_a_release_that_a_program_says_is_on_the_button_while_the_cursor_is_not() {
        // The message's own point is the poster's to choose; the cursor isn't.
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        let lying = PointerMsg {
            at: Some(OFF),
            message_at: Some(ON),
            ..hardware(ON)
        };
        r.seen(PointerKind::Release, lying, Replay::target_at(ON));
        assert_eq!(r.take(), Err(Refusal::NotPointer));
    }

    #[test] // spec: P4a-R10
    fn refuses_a_press_after_the_capture_was_taken_or_the_mode_cancelled() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.seen(
            PointerKind::CaptureChanged,
            posted(ON),
            Replay::target_at(ON),
        );
        r.release(hardware(ON));
        assert_eq!(r.take(), Err(Refusal::NoRealPress));
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.seen(PointerKind::CancelMode, posted(ON), Replay::target_at(ON));
        r.release(hardware(ON));
        assert_eq!(r.take(), Err(Refusal::NoRealPress));
    }

    #[test] // spec: P4a-R10
    fn counts_what_windows_called_real() {
        let mut r = Replay::new();
        r.mv(hardware(OFF));
        r.mv(posted(OFF));
        r.mv(injected(OFF));
        assert_eq!(r.gate.counts(), Counts { seen: 3, real: 1 });
    }
}
