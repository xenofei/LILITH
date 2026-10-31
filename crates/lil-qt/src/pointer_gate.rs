//! Whether a click on Approve (or a cap prompt's Allow once or Raise) came from the owner's hand
//! (P4a-R10), Qt-free: the native side of the answer's gate. [`crate::answer_gate`] can't tell you
//! from a program running as you. This can tell, as far as Windows can.
//!
//! It stands on the Windows Hello window's rules ([`ClickGate`]), audited there and not repeated
//! here: only a press and a release that Windows reports as `IMO_HARDWARE` from a mouse or
//! touchpad, handled on the window's own thread, after a real rest on the button. Posted messages,
//! `SendInput` and a warped cursor all fail one of those. Raw input stays off (that window's own
//! rule without it): raw input goes to one window per process, and the shell has many. A still hand
//! whose rest Windows ended with a move of its own needs a small move, and the dialog says so.
//!
//! **What can't be told.** Microsoft documents `IMO_HARDWARE` as covering a UIAccess application's
//! injection too, so one (it needs an installed, signed binary) reads as a hand. What a posted
//! message's source reports isn't documented, so the source isn't the only factor: at a press the
//! physical left button must be down, and at a release up (`GetAsyncKeyState`, documented as the
//! physical button's state, which no message changes: [`Seen::physically_down`]). A click that went
//! by before the thread read it is refused, and says so.
//!
//! **How a click reaches it.** QML's event comes after the message, through Qt's queue, and by then
//! nothing can say where it came from. So the native event filter (`src/qt/pointer_filter.cpp`)
//! gives every pointer message to [`PointerGate::message`] before Qt sees it. A real press on a button
//! leaves a press token, which QML's own press takes ([`PointerGate::take_press`]). A real release on
//! the same button, after a press that counted, leaves a click token, which QML's own click takes
//! together with that press ([`PointerGate::take_click`]). Only both answer, for one button, once. A
//! release on another button than the press mints nothing. Tokens die at the next press and after
//! [`TOKEN_LIFETIME`]. A token isn't dropped at the next move: Qt can read more input before it
//! delivers the click, and a real click would be refused.
//!
//! **Messages that skip the filter.** Qt runs native filters on input messages only as they leave
//! the queue, so a *sent* mouse message reaches QML without passing the filter. A press or release
//! sent to this thread's windows is seen by a thread hook instead (`WH_CALLWNDPROC`) and drops every
//! token ([`PointerGate::sent_input`]), so it can't take one left by a real click, and a click with
//! no token is refused. If the hook can't be set, every click is refused ([`GateRefusal::NoGuard`]).
//!
//! **What this adds to the Hello window's rules**, since the buttons here aren't windows of their
//! own: where a button is comes from Qt at each message. A button that moved since the last message
//! (a card arrived above it, the list scrolled, its window moved), or that wasn't there, or whose
//! confirmation or amount changed ([`Target::serial`], a new one each time), starts again, and with
//! the cursor on it the pointer must leave and come back: nothing changes under a hand and keeps its
//! rest ([`GateRefusal::Reenter`]). A press that brought the window to the front (`WM_MOUSEACTIVATE`),
//! or with another window in front (`GetForegroundWindow`, which a program can't fake with a message),
//! is refused, and so is one where the window under the point isn't this one, or this one is
//! see-through, cloaked or minimized ([`GateRefusal::NotExposed`]).
//!
//! Touch and pen never count. Qt takes them as `WM_POINTER*`, which aren't read here, and a legacy
//! message that reports either device is refused too, so a tap on Approve says it needs the mouse or
//! touchpad.

use std::time::{Duration, Instant};

use lil_platform::hello::{
    ClickGate, InputDevice, InputOrigin, PointerMsg, Refusal, is_real_pointer,
};

/// How long a real press's or release's token waits for QML's event, which comes on the same thread
/// right after it. A stalled GUI thread can use this up: the click is refused, and says so.
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
    /// Which button, showing what: a new one each time what it stands for changes.
    pub serial: u64,
    /// `(left, top, right, bottom)` in the screen's physical pixels, clipped as it is drawn.
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
    /// The window under the cursor is the message's window, and it isn't see-through, cloaked or
    /// minimized.
    pub exposed: bool,
    /// The physical left button is down now.
    pub physically_down: bool,
    /// `TME_LEAVE` is armed on the message's window (moves only).
    pub tracked: bool,
    pub target: Option<Target>,
}

/// How many messages it has read, how many of them Windows reported as real, and how many mouse
/// button messages were sent past the filter: what a test and the app's log can check. No position,
/// no timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub seen: u64,
    pub real: u64,
    pub sent: u64,
}

/// Why a click didn't count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateRefusal {
    /// One of the Hello window's rules.
    Click(Refusal),
    /// What the button stands for changed, or it moved or appeared, under the cursor.
    Reenter,
    /// The physical button didn't agree with the message.
    ButtonState,
    /// The window under the point isn't this one, or this one can't be seen.
    NotExposed,
    /// The hook that sees sent messages couldn't be set.
    NoGuard,
}

impl GateRefusal {
    /// A stable name, for the log and for QML. The Hello window's `not_in_front` here means the click
    /// brought the window forward, not the answer gate's "not in front".
    pub fn name(self) -> &'static str {
        match self {
            GateRefusal::Click(Refusal::NotInFront) => "brought_forward",
            GateRefusal::Click(other) => other.name(),
            GateRefusal::Reenter => "reenter",
            GateRefusal::ButtonState => "button_state",
            GateRefusal::NotExposed => "not_exposed",
            GateRefusal::NoGuard => "no_guard",
        }
    }

    /// The words for a refusal's name, on the button labelled `label`.
    pub fn words(name: &str, label: &str) -> Option<String> {
        let click = |refusal: Refusal| Some(refusal.message(label));
        match name {
            "brought_forward" => click(Refusal::NotInFront),
            "not_pointer" => click(Refusal::NotPointer),
            "no_real_press" => click(Refusal::NoRealPress),
            "no_rest" => click(Refusal::NoRest),
            "no_move" => click(Refusal::NoMove),
            "reenter" => Some(format!(
                "What is shown here changed, or {label} moved. Move the pointer off it, check what it says, then come back and click it."
            )),
            "button_state" => Some(format!(
                "That click was too quick for LILITH to check. Hold the click a moment longer on {label}."
            )),
            "not_exposed" => Some(format!(
                "This window is covered, see-through, off the screen or hidden, so a click on {label} doesn't count. Something may have changed it: bring it back and look before you click."
            )),
            "no_guard" => Some(format!(
                "LILITH couldn't set up its check of where clicks come from, so {label} is off. Deny still works."
            )),
            _ => None,
        }
    }
}

/// See the module docs.
#[derive(Debug, Default)]
pub struct PointerGate {
    gate: ClickGate,
    /// The button the last move was on.
    current: Option<u64>,
    /// `WM_MOUSEACTIVATE` came: the next press brought the window to the front.
    activated: bool,
    /// The pointer must leave this button before it counts again.
    reenter: Option<u64>,
    /// Why the press that is down doesn't count, beyond what [`ClickGate`] judges.
    press_refused: Option<GateRefusal>,
    /// Which button the press that is down was on.
    press_serial: Option<u64>,
    /// A real release was the last thing handled, until the next message.
    releasing: bool,
    press_token: Option<(u64, Instant)>,
    /// QML's press on this button took a press token.
    qml_pressed: Option<u64>,
    token: Option<(u64, Instant)>,
    refused: Option<(u64, GateRefusal, Instant)>,
    no_guard: bool,
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
        let mut msg = s.msg;
        // Touch and pen never count, whatever the message.
        if matches!(msg.device, InputDevice::Touch | InputDevice::Pen) {
            msg.origin = InputOrigin::Unavailable;
        }
        self.counts.seen += 1;
        if msg.origin == InputOrigin::Hardware && !msg.sent {
            self.counts.real += 1;
        }
        let serial = s.target.map(|t| t.serial);
        if self.reenter.is_some() && serial != self.reenter {
            self.reenter = None;
        }
        if let Some(t) = s.target.filter(|t| t.changed) {
            self.gate.restart(msg.at);
            self.press_refused = None;
            self.press_serial = None;
            self.token = None;
            self.press_token = None;
            if contains(t.rect, msg.at) {
                self.reenter = Some(t.serial);
            }
        }
        // On the button, by where the cursor really is and where the message says it was.
        let inside = s
            .target
            .is_some_and(|t| contains(t.rect, msg.at) && contains(t.rect, msg.message_at));
        let must_reenter = serial.is_some() && serial == self.reenter;
        let real = is_real_pointer(msg.origin, msg.device, msg.sent);
        match s.kind {
            PointerKind::Move => {
                // From one button straight to another: the rest was on the first.
                if serial.is_some() && self.current.is_some() && serial != self.current {
                    self.gate.button_move(false, msg, now);
                }
                self.gate.button_move(inside && !must_reenter, msg, now);
                self.gate.button_tracking(s.tracked);
                self.current = serial;
            }
            PointerKind::Press => {
                self.token = None;
                self.refused = None;
                self.qml_pressed = None;
                let activated = std::mem::take(&mut self.activated);
                self.press_refused = if activated || !s.in_front {
                    Some(GateRefusal::Click(Refusal::NotInFront))
                } else if must_reenter {
                    Some(GateRefusal::Reenter)
                } else if !s.exposed {
                    Some(GateRefusal::NotExposed)
                } else if !s.physically_down {
                    Some(GateRefusal::ButtonState)
                } else {
                    None
                };
                self.gate.button_press(inside, msg, now);
                self.press_serial = serial.filter(|_| inside);
                self.press_token = self
                    .press_serial
                    .filter(|_| real && self.press_refused.is_none())
                    .map(|serial| (serial, now));
            }
            PointerKind::Release => {
                let (armed, refusal) = self.gate.button_release(inside, msg);
                self.releasing = true;
                let press_serial = self.press_serial.take();
                let press_refused = self.press_refused.take();
                let judged = if !real {
                    // The source first: this says so whatever else is wrong.
                    Err(GateRefusal::Click(Refusal::NotPointer))
                } else if let Some(refused) = press_refused {
                    Err(refused)
                } else if let Some(refusal) = refusal {
                    Err(GateRefusal::Click(refusal))
                } else if !armed || press_serial != serial {
                    // Pressed on another button, or off this one.
                    Err(GateRefusal::Click(Refusal::NoRealPress))
                } else if !s.exposed {
                    Err(GateRefusal::NotExposed)
                } else if s.physically_down {
                    Err(GateRefusal::ButtonState)
                } else {
                    Ok(())
                };
                match (serial, judged) {
                    (Some(serial), Ok(())) => self.token = Some((serial, now)),
                    (Some(serial), Err(refusal)) => {
                        self.refused = Some((serial, refusal, now));
                    }
                    (None, _) => {}
                }
            }
            PointerKind::Leave => {
                // It says nothing of `reenter`: any program can post this. A message with the cursor
                // really off the button clears that, above.
                self.gate.button_leave();
                self.current = None;
            }
            PointerKind::CaptureChanged => self.gate.capture_changed(),
            PointerKind::CancelMode => self.gate.cancel_mode(),
            PointerKind::MouseActivate => self.activated = true,
        }
    }

    /// A press or release was sent to one of this thread's windows, past the filter: everything
    /// waiting is dropped, so it can't use what a real click left.
    pub fn sent_input(&mut self) {
        self.counts.sent += 1;
        self.press_token = None;
        self.qml_pressed = None;
        self.token = None;
        self.refused = None;
    }

    /// QML's own press on button `serial`: it takes that button's press token, if there is one.
    pub fn take_press(&mut self, serial: u64, now: Instant) {
        self.qml_pressed = self
            .press_token
            .take()
            .filter(|&(s, at)| s == serial && fresh(at, now))
            .map(|(s, _)| s);
    }

    /// QML's own click on button `serial`: `Ok` when the hand made it, used up here, else which
    /// rule refused it. It needs QML's own press, with its token, on the same button.
    pub fn take_click(&mut self, serial: u64, now: Instant) -> Result<(), GateRefusal> {
        let token = self.token.take();
        let refused = self.refused.take();
        let pressed = self.qml_pressed.take();
        if self.no_guard {
            return Err(GateRefusal::NoGuard);
        }
        if pressed == Some(serial) && token.is_some_and(|(s, at)| s == serial && fresh(at, now)) {
            return Ok(());
        }
        match refused {
            Some((s, refusal, at)) if s == serial && fresh(at, now) => Err(refusal),
            _ => Err(GateRefusal::Click(Refusal::NotPointer)),
        }
    }

    /// Whether the hook that sees sent messages was set.
    pub fn guard_set(&mut self, ok: bool) {
        self.no_guard = !ok;
    }

    /// Forgets everything but the counts and the guard: nothing was watched until now, so what it
    /// knew is old.
    pub fn reset(&mut self) {
        *self = PointerGate {
            counts: self.counts,
            no_guard: self.no_guard,
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
    use lil_platform::hello::HOVER_BEFORE_PRESS;

    const RECT: (i32, i32, i32, i32) = (100, 100, 200, 130);
    const ON: (i32, i32) = (150, 115);
    const OFF: (i32, i32) = (50, 50);
    const NOT_POINTER: GateRefusal = GateRefusal::Click(Refusal::NotPointer);
    const NO_MOVE: GateRefusal = GateRefusal::Click(Refusal::NoMove);
    const NO_REAL_PRESS: GateRefusal = GateRefusal::Click(Refusal::NoRealPress);

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

        /// As a real hand's message reads: in front, exposed, the button down at a press only.
        fn seen_as(kind: PointerKind, m: PointerMsg, target: Option<Target>) -> Seen {
            Seen {
                kind,
                msg: m,
                in_front: true,
                exposed: true,
                physically_down: kind == PointerKind::Press,
                tracked: true,
                target,
            }
        }

        fn seen(&mut self, kind: PointerKind, m: PointerMsg, target: Option<Target>) {
            let s = Self::seen_as(kind, m, target);
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

        /// What Qt delivers for a whole click on button 1: its press, then its click.
        fn qml_click(&mut self) -> Result<(), GateRefusal> {
            self.gate.take_press(1, self.now);
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
        assert_eq!(r.qml_click(), Ok(()));
        assert_eq!(r.qml_click(), Err(NOT_POINTER), "the tokens are used up");
    }

    #[test] // spec: P4a-R10
    fn accepts_qmls_press_arriving_after_the_native_release() {
        // Qt can read the release before it delivers the press.
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.release(hardware(ON));
        r.gate.take_press(1, r.now);
        assert_eq!(r.gate.take_click(1, r.now), Ok(()));
    }

    #[test] // spec: P4a-R10
    fn accepts_a_move_between_the_release_and_its_click() {
        // Qt can read more input before it delivers the click: that doesn't refuse a real click.
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.release(hardware(ON));
        r.mv(hardware((151, 115)));
        assert_eq!(r.qml_click(), Ok(()));
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
        assert_eq!(r.qml_click(), Ok(()));
        r.press(hardware(ON));
        r.release(hardware(ON));
        assert_eq!(
            r.qml_click(),
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
        assert_eq!(r.qml_click(), Err(NOT_POINTER));
    }

    #[test] // spec: P4a-R10
    fn refuses_posted_messages_whatever_the_hand_did_before() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(posted(ON));
        r.release(posted(ON));
        assert_eq!(r.qml_click(), Err(NOT_POINTER));
        // A real press under a posted release.
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.release(posted(ON));
        assert_eq!(r.qml_click(), Err(NOT_POINTER));
    }

    /// The bypass the source alone would leave: a posted press and release that the source calls
    /// hardware, with a real rest and the cursor on the button. No message moves the physical button.
    #[test] // spec: P4a-R10
    fn refuses_a_posted_pair_that_reads_as_hardware_by_the_physical_button() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        let mut press = Replay::seen_as(PointerKind::Press, hardware(ON), Replay::target_at(ON));
        press.physically_down = false;
        r.gate.message(press, r.now);
        r.release(hardware(ON));
        assert_eq!(r.qml_click(), Err(GateRefusal::ButtonState));
        // A real press held down, under a release that reads as hardware.
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        let mut release =
            Replay::seen_as(PointerKind::Release, hardware(ON), Replay::target_at(ON));
        release.physically_down = true;
        r.gate.message(release, r.now);
        assert_eq!(r.qml_click(), Err(GateRefusal::ButtonState));
    }

    #[test] // spec: P4a-R10
    fn refuses_injected_input() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(injected(ON));
        r.release(injected(ON));
        assert_eq!(r.qml_click(), Err(NOT_POINTER));
        // An injected press under a real release.
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(injected(ON));
        r.release(hardware(ON));
        assert_eq!(r.qml_click(), Err(NO_REAL_PRESS));
        // A real press under an injected release.
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.release(injected(ON));
        assert_eq!(r.qml_click(), Err(NOT_POINTER));
    }

    #[test] // spec: P4a-R10
    fn refuses_touch_and_pen_whatever_the_message_reports() {
        for device in [InputDevice::Touch, InputDevice::Pen] {
            let mut r = Replay::new();
            r.rest_on_the_button();
            r.press(msg(InputOrigin::Hardware, device, false, ON));
            r.release(msg(InputOrigin::Hardware, device, false, ON));
            assert_eq!(r.qml_click(), Err(NOT_POINTER), "{device:?}");
        }
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
        assert_eq!(r.qml_click(), Err(NO_MOVE));
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
        assert_eq!(r.qml_click(), Err(GateRefusal::Click(Refusal::NoRest)));
    }

    #[test] // spec: P4a-R10
    fn refuses_a_button_that_slid_under_a_still_cursor_until_the_pointer_comes_back() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        // A card arrived above: the button moved, and the next message sees it.
        let moved = Target {
            serial: 1,
            rect: (100, 90, 200, 120),
            changed: true,
        };
        let same = Target {
            changed: false,
            ..moved
        };
        r.seen(PointerKind::Press, hardware(ON), Some(moved));
        r.seen(PointerKind::Release, hardware(ON), Some(same));
        assert_eq!(r.qml_click(), Err(GateRefusal::Reenter));
        // Moving on the button isn't enough, however long it rests.
        r.seen(PointerKind::Move, hardware((151, 115)), Some(same));
        r.seen(PointerKind::Move, hardware((152, 115)), Some(same));
        r.advance(HOVER_BEFORE_PRESS * 4);
        r.seen(PointerKind::Press, hardware((152, 115)), Some(same));
        r.seen(PointerKind::Release, hardware((152, 115)), Some(same));
        assert_eq!(r.qml_click(), Err(GateRefusal::Reenter));
        // Off it and back.
        r.seen(PointerKind::Move, hardware(OFF), None);
        r.seen(PointerKind::Move, hardware((110, 100)), Some(same));
        r.seen(PointerKind::Move, hardware((111, 101)), Some(same));
        r.advance(HOVER_BEFORE_PRESS);
        r.seen(PointerKind::Press, hardware((111, 101)), Some(same));
        r.seen(PointerKind::Release, hardware((111, 101)), Some(same));
        assert_eq!(r.qml_click(), Ok(()));
    }

    /// The buttons went off and on under a cursor that never moved, so the gate saw no message in
    /// between: `ApproveButton` watches again, and the next message finds a new button at that spot.
    #[test] // spec: P4a-R10
    fn refuses_a_rest_from_before_the_button_went_off_and_on() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        let back_on = Target {
            serial: 2,
            rect: RECT,
            changed: true,
        };
        let same = Target {
            changed: false,
            ..back_on
        };
        r.seen(PointerKind::Press, hardware(ON), Some(back_on));
        r.seen(PointerKind::Release, hardware(ON), Some(same));
        r.gate.take_press(2, r.now);
        assert_eq!(r.gate.take_click(2, r.now), Err(GateRefusal::Reenter));
        // Off it and back.
        r.seen(PointerKind::Move, hardware(OFF), None);
        r.seen(PointerKind::Move, hardware((101, 101)), Some(same));
        r.seen(PointerKind::Move, hardware(ON), Some(same));
        r.advance(HOVER_BEFORE_PRESS);
        r.seen(PointerKind::Press, hardware(ON), Some(same));
        r.advance(Duration::from_millis(80));
        r.seen(PointerKind::Release, hardware(ON), Some(same));
        r.gate.take_press(2, r.now);
        assert_eq!(r.gate.take_click(2, r.now), Ok(()));
    }

    #[test] // spec: P4a-R10
    fn refuses_a_moving_hand_on_a_button_that_changed_under_it() {
        // The swap the review found: the hand keeps moving and rests after the change.
        let mut r = Replay::new();
        r.rest_on_the_button();
        let new_card = Target {
            serial: 2,
            rect: RECT,
            changed: true,
        };
        let same = Target {
            changed: false,
            ..new_card
        };
        r.seen(PointerKind::Move, hardware((151, 116)), Some(new_card));
        r.seen(PointerKind::Move, hardware((152, 116)), Some(same));
        r.seen(PointerKind::Move, hardware((153, 116)), Some(same));
        r.advance(HOVER_BEFORE_PRESS * 4);
        r.seen(PointerKind::Press, hardware((153, 116)), Some(same));
        r.seen(PointerKind::Release, hardware((153, 116)), Some(same));
        r.gate.take_press(2, r.now);
        assert_eq!(r.gate.take_click(2, r.now), Err(GateRefusal::Reenter));
    }

    /// The review's replay: the amount changes under a resting hand, a program posts `WM_MOUSELEAVE`,
    /// and the hand's small moves on the button rebuild the rest.
    #[test] // spec: P4a-R10
    fn refuses_a_posted_leave_as_a_way_round_the_re_entry() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        let new_amount = Target {
            serial: 2,
            rect: RECT,
            changed: true,
        };
        let same = Target {
            changed: false,
            ..new_amount
        };
        r.seen(PointerKind::Move, hardware((151, 116)), Some(new_amount));
        r.seen(PointerKind::Leave, posted((151, 116)), Some(same));
        r.seen(PointerKind::Move, hardware((152, 116)), Some(same));
        r.seen(PointerKind::Move, hardware((153, 116)), Some(same));
        r.advance(HOVER_BEFORE_PRESS * 4);
        r.seen(PointerKind::Press, hardware((153, 116)), Some(same));
        r.seen(PointerKind::Release, hardware((153, 116)), Some(same));
        r.gate.take_press(2, r.now);
        assert_eq!(r.gate.take_click(2, r.now), Err(GateRefusal::Reenter));
        // A real leave, with the cursor off the button, does clear it.
        r.seen(PointerKind::Leave, hardware(OFF), None);
        r.seen(PointerKind::Move, hardware((110, 100)), Some(same));
        r.seen(PointerKind::Move, hardware((111, 101)), Some(same));
        r.advance(HOVER_BEFORE_PRESS);
        r.seen(PointerKind::Press, hardware((111, 101)), Some(same));
        r.seen(PointerKind::Release, hardware((111, 101)), Some(same));
        r.gate.take_press(2, r.now);
        assert_eq!(r.gate.take_click(2, r.now), Ok(()));
    }

    #[test] // spec: P4a-R10
    fn refuses_a_token_for_another_button() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.release(hardware(ON));
        r.gate.take_press(2, r.now);
        assert_eq!(r.gate.take_click(2, r.now), Err(NOT_POINTER));
        assert_eq!(r.qml_click(), Err(NOT_POINTER), "and it is gone");
    }

    #[test] // spec: P4a-R10
    fn refuses_a_press_on_one_button_and_a_release_on_another() {
        let other = Target {
            serial: 2,
            rect: (200, 100, 300, 130),
            changed: false,
        };
        let on_other = (250, 115);
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.seen(PointerKind::Move, hardware(on_other), Some(other));
        r.advance(HOVER_BEFORE_PRESS);
        r.seen(PointerKind::Release, hardware(on_other), Some(other));
        r.gate.take_press(2, r.now);
        assert_eq!(r.gate.take_click(2, r.now), Err(NO_REAL_PRESS));
        r.gate.take_press(1, r.now);
        assert_eq!(r.gate.take_click(1, r.now), Err(NOT_POINTER));
    }

    #[test] // spec: P4a-R10
    fn refuses_a_click_without_qmls_own_press() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.release(hardware(ON));
        assert_eq!(r.gate.take_click(1, r.now), Err(NOT_POINTER));
    }

    #[test] // spec: P4a-R10
    fn refuses_a_sent_pair_after_a_real_click_left_its_tokens() {
        // A real click that QML delivered elsewhere (something on top of the button), then a pair
        // sent past the filter: the hook sees it before it can use what is waiting.
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.release(hardware(ON));
        r.gate.sent_input();
        r.gate.take_press(1, r.now);
        r.gate.sent_input();
        assert_eq!(r.gate.take_click(1, r.now), Err(NOT_POINTER));
        assert_eq!(r.gate.counts().sent, 2);
    }

    #[test] // spec: P4a-R10
    fn refuses_every_click_when_the_hook_couldnt_be_set() {
        let mut r = Replay::new();
        r.gate.guard_set(false);
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.release(hardware(ON));
        assert_eq!(r.qml_click(), Err(GateRefusal::NoGuard));
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
        r.gate.take_press(2, r.now);
        assert_eq!(r.gate.take_click(2, r.now), Err(NO_MOVE));
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
        assert_eq!(r.qml_click(), Err(GateRefusal::Click(Refusal::NotInFront)));
        // The next one counts.
        r.press(hardware(ON));
        r.release(hardware(ON));
        assert_eq!(r.qml_click(), Ok(()));
    }

    #[test] // spec: P4a-R10
    fn refuses_a_press_with_another_window_in_front() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        let mut s = Replay::seen_as(PointerKind::Press, hardware(ON), Replay::target_at(ON));
        s.in_front = false;
        r.gate.message(s, r.now);
        r.release(hardware(ON));
        assert_eq!(r.qml_click(), Err(GateRefusal::Click(Refusal::NotInFront)));
    }

    #[test] // spec: P4a-R10
    fn refuses_a_click_where_the_window_cant_be_seen() {
        for kind in [PointerKind::Press, PointerKind::Release] {
            let mut r = Replay::new();
            r.rest_on_the_button();
            for step in [PointerKind::Press, PointerKind::Release] {
                let mut s = Replay::seen_as(step, hardware(ON), Replay::target_at(ON));
                s.exposed = step != kind;
                r.gate.message(s, r.now);
            }
            assert_eq!(r.qml_click(), Err(GateRefusal::NotExposed), "{kind:?}");
        }
    }

    #[test] // spec: P4a-R10
    fn refuses_a_token_past_its_lifetime_or_after_a_new_press() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.release(hardware(ON));
        r.advance(TOKEN_LIFETIME + Duration::from_millis(1));
        assert_eq!(r.qml_click(), Err(NOT_POINTER));
        // A release that left a token; a new press, posted, drops it.
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.release(hardware(ON));
        r.press(posted(ON));
        assert_eq!(r.qml_click(), Err(NOT_POINTER));
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
        assert_eq!(r.qml_click(), Err(NO_REAL_PRESS));
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
        assert_eq!(r.qml_click(), Err(NO_REAL_PRESS));
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.seen(PointerKind::CancelMode, posted(ON), Replay::target_at(ON));
        r.release(hardware(ON));
        assert_eq!(r.qml_click(), Err(NO_REAL_PRESS));
    }

    #[test] // spec: P4a-R10
    fn counts_what_windows_called_real() {
        let mut r = Replay::new();
        r.mv(hardware(OFF));
        r.mv(posted(OFF));
        r.mv(injected(OFF));
        r.gate.sent_input();
        assert_eq!(
            r.gate.counts(),
            Counts {
                seen: 3,
                real: 1,
                sent: 1
            }
        );
    }

    #[test] // spec: P4a-R10
    fn every_refusal_has_words() {
        for refusal in [
            GateRefusal::Click(Refusal::NotInFront),
            GateRefusal::Click(Refusal::NotPointer),
            GateRefusal::Click(Refusal::NoRealPress),
            GateRefusal::Click(Refusal::NoRest),
            GateRefusal::Click(Refusal::NoMove),
            GateRefusal::Reenter,
            GateRefusal::ButtonState,
            GateRefusal::NotExposed,
            GateRefusal::NoGuard,
        ] {
            let words = GateRefusal::words(refusal.name(), "Approve").unwrap();
            assert!(words.contains("Approve"), "{refusal:?}: {words}");
        }
    }
}
