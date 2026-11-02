//! Whether a click on Approve (or a cap prompt's Allow once or Raise) came from the owner's hand
//! (P4a-R10), Qt-free: the native side of the answer's gate. [`crate::answer_gate`] can't tell you
//! from a program running as you. This can tell, as far as Windows can.
//!
//! It stands on the Windows Hello window's rules ([`ClickGate`]), audited there and not repeated
//! here: only a press and a release that Windows reports as `IMO_HARDWARE` from a mouse or
//! touchpad, handled on the window's own thread, after a real rest on the button. Posted messages
//! and `SendInput` fail one of those. Raw input stays off (that window's own rule without it): raw
//! input goes to one window per process, and the shell has many. A still hand whose rest Windows
//! ended with a move of its own needs a small move, and the dialog says so.
//!
//! **A warped cursor.** The Hello rules hold it off only for a still hand: under a moving hand, the
//! hand's next real moves would rebuild the rest. So a move that isn't real, to a place other than
//! where the cursor last was, on a button, means the pointer must really leave that button first
//! ([`GateRefusal::Reenter`]), and only a real message ends that. A real move read late, whose own
//! point isn't where the cursor is now, doesn't count as a place and ends nothing: a warp can land
//! while it waits in the queue. What can't be told apart: Windows' own move (it reports `IMO_SYSTEM`,
//! as for a warp) while the hand happens to be moving, when something changes the windows under it,
//! reads as a warp; the cost is a move off and back, with words. This is a heuristic over what
//! messages report, with residuals. A warp that lands just off a button, with the hand carrying on
//! onto it, arrives by the hand's own real moves, and this gate doesn't tell that from an arrival with
//! no warp. A warp merged into the hand's next move, if Windows reports that one move as a real one,
//! isn't seen at all: unmeasured until `tools/sandbox/confirm-check.ps1`'s step (l) reports it. And a
//! late real move with no move of Windows' own after it reads the same as that.
//!
//! **What can't be told.** Microsoft documents `IMO_HARDWARE` as covering a UIAccess application's
//! injection too, so one reads as a hand. It documents that such an application must be signed, and
//! installed in a protected folder unless a local policy turns the folder rule off. A public technique
//! may reuse a signed process's token instead; that is unmeasured here. What a posted
//! message's source reports isn't documented, so the source isn't the only factor: at a press the
//! left button must be down, and at a release up (`GetAsyncKeyState`: [`Seen::physically_down`]). That
//! state follows real and injected input alike, measured: 100 injected presses, none read as up. So it
//! proves no more than that a press or release really went through Windows' input, which a posted
//! message alone doesn't; against `SendInput`, or a posted press alongside one, it's the source that
//! refuses. A click that went by before the
//! thread read it is refused, and says so.
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
//! confirmation or amount changed, or that came back on ([`Target::serial`], a new one each time),
//! starts again, and with
//! the cursor on it the pointer must leave and come back: nothing changes under a hand and keeps its
//! rest ([`GateRefusal::Reenter`]). A press that brought the window to the front (`WM_MOUSEACTIVATE`),
//! or with another window in front (`GetForegroundWindow`, which a program can't fake with a message),
//! is refused, and so is one where the window under the point isn't this one, or this one is
//! see-through, cloaked or minimized ([`GateRefusal::NotExposed`]).
//!
//! **Coming to the front is read from Windows**, never from Qt: `WM_ACTIVATE` and `WM_MOUSEACTIVATE`,
//! sent messages, reach the gate through both the thread hook and the filter (Qt runs its dispatcher's
//! filters for sent messages that aren't input), before the click that caused them is read. Qt's own
//! activation reaches QML after that click, so nothing here waits on `Window.active`. On coming to the
//! front, per top-level window, the rest starts again, nothing half-done stands, and a button under
//! the cursor needs the pointer to leave it and come back ([`PointerKind::FrontGained`]). The press is
//! then judged by its own time (`MSG.time`, which a program posting a message can't choose) against
//! the tick its window came to the front: no later than that, it brought the window forward; within
//! [`FRONT_SETTLE_MS`], too soon. The premise: Windows delivers `WM_ACTIVATE` to this thread before
//! any input it reads while the window is in front, so the gate knows the front before it judges a
//! press. A thread stalled before it reads the activation stamps that front late, after the input
//! time of a press made in the meantime. That press then reads as having brought the window forward,
//! or as too soon: stricter, and the refusal may name the wrong cause.
//! `WA_CLICKACTIVE` and `WM_MOUSEACTIVATE` mark the activating click too. That ends only when the hand
//! moves somewhere: Windows' own move, or a real one in place, doesn't end it.
//!
//! A residual: any program can send this thread `WM_ACTIVATE` or `WM_MOUSEACTIVATE`. A forged one
//! can only refuse an answer (a click as too soon, brought forward or needing the pointer off and
//! back, and a keyboard Deny or Cancel as too soon, through the answer bar's clock) and change the
//! words it gives: it never approves anything.
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
    /// `WM_ACTIVATE` with `WA_ACTIVE`: the window came to the front, not by a click.
    FrontGained,
    /// `WM_ACTIVATE` with `WA_CLICKACTIVE`: a click brought it to the front.
    FrontClicked,
    /// `WM_ACTIVATE` with `WA_INACTIVE`: it left the front.
    FrontLost,
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
    /// `GetAsyncKeyState` has the left button down now: what Windows' input has done, real or injected,
    /// which no message changes.
    pub physically_down: bool,
    /// `TME_LEAVE` is armed on the message's window (moves only).
    pub tracked: bool,
    pub target: Option<Target>,
    /// The message's top-level window (`GA_ROOT`).
    pub window: isize,
    /// When the input happened, on `GetTickCount`'s clock: `MSG.time` for what left the queue, which
    /// a program posting a message can't choose; the tick when a sent message (activation) arrived.
    pub time: u32,
}

/// How long after its window comes to the front a press is too soon, by the input's own time: the
/// answer gate's half second.
pub const FRONT_SETTLE_MS: i32 = 500;

/// `GetTickCount` moves in steps of about 16 ms, and an activating click's own time comes a little
/// before its activation's tick: a press up to this long after the tick still brought it forward.
pub const TICK_SLACK_MS: i32 = 20;

/// How long a window's front is kept to compare a press with: far past the half second, so a stalled
/// thread's press still finds it, and far short of the clock's wrap.
pub const FRONT_KEPT: Duration = Duration::from_secs(60);

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
    /// What the button stands for changed, or it moved, appeared or came on, under the cursor.
    Reenter,
    /// The button's state didn't agree with the message.
    ButtonState,
    /// QML's events came past [`TOKEN_LIFETIME`]: too late to tell.
    Stalled,
    /// The press came within [`FRONT_SETTLE_MS`] of its window coming to the front, by the input's
    /// own time: the answer gate's half second, whatever order Qt reads things in. A thread stalled
    /// before it reads the activation makes this stricter, not laxer (the module docs).
    TooSoon,
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
            GateRefusal::Stalled => "stalled",
            GateRefusal::TooSoon => "too_soon",
            GateRefusal::NotExposed => "not_exposed",
            GateRefusal::NoGuard => "no_guard",
        }
    }

    /// The words for a refusal's name, on the button labelled `label`.
    pub fn words(name: &str, label: &str) -> Option<String> {
        let click = |refusal: Refusal| Some(refusal.message(label));
        match name {
            // Not the Hello window's words: coming to the front makes a new button here, so a small
            // move on it isn't enough.
            "brought_forward" => Some(format!(
                "That click brought this window to the front. Move the pointer off {label}, then come back and click it."
            )),
            "not_pointer" => click(Refusal::NotPointer),
            "no_real_press" => click(Refusal::NoRealPress),
            "no_rest" => click(Refusal::NoRest),
            "no_move" => click(Refusal::NoMove),
            "reenter" => Some(format!(
                "{label} just came on, or what it stands for changed or moved, or the pointer jumped. Move the pointer off it, check what it says, then come back and click it."
            )),
            "button_state" => Some(format!(
                "That click was too quick for LILITH to check. Hold the click a moment longer on {label}."
            )),
            // The answer gate's name too: one set of words for both.
            "too_soon" => Some(format!(
                "Wait a moment: {label} works half a second after this window comes to the front."
            )),
            "stalled" => Some(format!(
                "LILITH was too busy to check that click in time, so it didn't count. Click {label} again."
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

/// At most one line a second of one kind, for what a program can make happen as often as it likes:
/// it can't use up the day's log and push out later evidence. The counts are in the next line.
#[derive(Debug, Default, Clone, Copy)]
pub struct LogLimit {
    last: Option<Instant>,
}

impl LogLimit {
    pub const fn new() -> LogLimit {
        LogLimit { last: None }
    }

    /// Whether a line may go now.
    pub fn due(&mut self, now: Instant) -> bool {
        let due = self
            .last
            .is_none_or(|at| now.duration_since(at) >= Duration::from_secs(1));
        if due {
            self.last = Some(now);
        }
        due
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
    /// When each top-level window last came to the front, on the input's clock, and when that was
    /// read: an entry past [`FRONT_KEPT`] says nothing any more, and could wrap.
    fronts: Vec<(isize, u32, Instant)>,
    /// The pointer must really leave this button before it counts again.
    reenter: Option<u64>,
    /// Where the cursor was at the last message.
    last_at: Option<(i32, i32)>,
    /// Why the press that is down doesn't count, beyond what [`ClickGate`] judges.
    press_refused: Option<GateRefusal>,
    /// Which button the press that is down was on.
    press_serial: Option<u64>,
    /// A real release was the last thing handled, until the next message.
    releasing: bool,
    press_token: Option<(u64, Instant)>,
    /// QML's press on this button took a press token.
    qml_pressed: Option<u64>,
    /// QML's press on this button came after its token had run out.
    qml_press_late: Option<u64>,
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
        let real = is_real_pointer(msg.origin, msg.device, msg.sent);
        let serial = s.target.map(|t| t.serial);
        // Where the message says the cursor was is where it is now. A hand's move still in the queue
        // when a warp lands reads as real, from the old place, with the cursor at the new one.
        let on_time = msg.message_at == msg.at;
        // Whether a front change or an activation: they say nothing of where a hand put the cursor.
        let front = matches!(
            s.kind,
            PointerKind::FrontGained
                | PointerKind::FrontClicked
                | PointerKind::FrontLost
                | PointerKind::MouseActivate
        );
        // The hand put the cursor somewhere it wasn't.
        let moved_on = s.kind == PointerKind::Move && real && on_time && msg.at != self.last_at;
        // Only a real message, on time: a program can warp the cursor off and back, or post a move.
        if real && on_time && !front && self.reenter.is_some() && serial != self.reenter {
            self.reenter = None;
        }
        // A real move ends a rest's small moves near where it was; a warp lands anywhere. If it lands
        // on a button, the rest is the program's, whatever the hand does there next.
        let input = !msg.sent && msg.origin != InputOrigin::Unavailable;
        if s.kind == PointerKind::Move
            && !real
            && input
            && msg.at != self.last_at
            && let Some(t) = s.target.filter(|t| contains(t.rect, msg.at))
        {
            self.reenter = Some(t.serial);
        }
        // Only from a move of input, on time. A posted message leaves the queue before input, and the
        // filter reads the cursor for each message, so anything else could record a warp's place before
        // Windows' own move says so.
        if s.kind == PointerKind::Move && input && on_time && msg.at.is_some() {
            self.last_at = msg.at;
        }
        if let Some(t) = s.target.filter(|t| t.changed) {
            self.gate.restart(msg.at);
            // A press that brought the window forward stays that, whatever coming forward did here.
            self.press_refused = self
                .press_refused
                .filter(|&refused| refused == GateRefusal::Click(Refusal::NotInFront));
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
        match s.kind {
            PointerKind::Move => {
                // A hand moving somewhere: an activation with no press after it (a title bar) is
                // over. Windows' own move, or a real one in place, can come between an activation and
                // its press, and doesn't count.
                if moved_on {
                    self.activated = false;
                }
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
                self.qml_press_late = None;
                let activated = std::mem::take(&mut self.activated);
                // By the input's own time against its window's last coming to the front: the click
                // that caused the activation came no later than it.
                let since_front = self.ms_since_front(s.window, s.time, now);
                let brought_forward = since_front.is_some_and(|ms| ms <= TICK_SLACK_MS);
                let too_soon = since_front.is_some_and(|ms| ms < FRONT_SETTLE_MS);
                self.press_refused = if activated || brought_forward || !s.in_front {
                    Some(GateRefusal::Click(Refusal::NotInFront))
                } else if must_reenter {
                    Some(GateRefusal::Reenter)
                } else if too_soon {
                    Some(GateRefusal::TooSoon)
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
                } else if must_reenter {
                    Err(GateRefusal::Reenter)
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
            // The window came to the front, or left it, whatever QML's `Window.active` says: the
            // rest starts again, nothing half-done stands, and with the cursor on a button the
            // pointer must leave it and come back. A click meant for the window underneath, or a
            // window raised under a moving hand, is refused that way.
            PointerKind::FrontGained | PointerKind::FrontClicked | PointerKind::FrontLost => {
                self.gate.restart(msg.at);
                self.press_refused = None;
                self.press_serial = None;
                self.token = None;
                self.press_token = None;
                if s.kind != PointerKind::FrontLost {
                    self.fronts.retain(|&(w, _, _)| w != s.window);
                    self.fronts.push((s.window, s.time, now));
                    if let Some(t) = s.target.filter(|t| contains(t.rect, msg.at)) {
                        self.reenter = Some(t.serial);
                    }
                }
                if s.kind == PointerKind::FrontClicked {
                    self.activated = true;
                }
            }
        }
    }

    /// Milliseconds from `window` last coming to the front to `time`, both on the input's clock, or
    /// none if it never did while watched, or did more than [`FRONT_KEPT`] before `now`: the clock
    /// wraps every 49.7 days, and a front that old would read as after the press.
    pub fn ms_since_front(&self, window: isize, time: u32, now: Instant) -> Option<i32> {
        self.fronts
            .iter()
            .find(|&&(w, _, read)| w == window && now.saturating_duration_since(read) < FRONT_KEPT)
            .map(|&(_, at, _)| time.wrapping_sub(at) as i32)
    }

    /// A press or release was sent to one of this thread's windows, past the filter: everything
    /// waiting is dropped, so it can't use what a real click left.
    pub fn sent_input(&mut self) {
        self.counts.sent += 1;
        self.press_token = None;
        self.qml_pressed = None;
        self.qml_press_late = None;
        self.token = None;
        self.refused = None;
    }

    /// QML's own press on button `serial`: it takes that button's press token, if there is one.
    pub fn take_press(&mut self, serial: u64, now: Instant) {
        let token = self.press_token.take().filter(|&(s, _)| s == serial);
        self.qml_pressed = token.filter(|&(_, at)| fresh(at, now)).map(|(s, _)| s);
        self.qml_press_late = token.filter(|&(_, at)| !fresh(at, now)).map(|(s, _)| s);
    }

    /// QML's own click on button `serial`: `Ok` when the hand made it, used up here, else which
    /// rule refused it. It needs QML's own press, with its token, on the same button.
    pub fn take_click(&mut self, serial: u64, now: Instant) -> Result<(), GateRefusal> {
        let token = self.token.take();
        let refused = self.refused.take();
        let pressed = self.qml_pressed.take();
        let press_late = self.qml_press_late.take() == Some(serial);
        if self.no_guard {
            return Err(GateRefusal::NoGuard);
        }
        if pressed == Some(serial) && token.is_some_and(|(s, at)| s == serial && fresh(at, now)) {
            return Ok(());
        }
        // The hand's click, or a refusal already made, that only QML's events are late for.
        let token_late = token.is_some_and(|(s, at)| s == serial && !fresh(at, now));
        match refused {
            Some((s, refusal, at)) if s == serial && fresh(at, now) => Err(refusal),
            Some((s, _, _)) if s == serial => Err(GateRefusal::Stalled),
            _ if token_late || press_late => Err(GateRefusal::Stalled),
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

    /// A press now would be the one that brought the window forward.
    pub fn came_forward(&self) -> bool {
        self.activated
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
        start: Instant,
    }

    /// The replays' top-level window.
    const WINDOW: isize = 0x1234;
    /// `GetTickCount` when a replay starts: near its wrap, so a comparison that forgot it fails.
    const TICK_START: u32 = u32::MAX - 2_000;

    impl Replay {
        fn new() -> Replay {
            let now = Instant::now();
            Replay {
                gate: PointerGate::new(),
                now,
                start: now,
            }
        }

        fn advance(&mut self, by: Duration) {
            self.now += by;
        }

        /// `GetTickCount` now.
        fn tick(&self) -> u32 {
            TICK_START.wrapping_add((self.now - self.start).as_millis() as u32)
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
                window: WINDOW,
                time: TICK_START,
            }
        }

        fn seen(&mut self, kind: PointerKind, m: PointerMsg, target: Option<Target>) {
            let time = self.tick();
            self.seen_at(kind, m, target, time);
        }

        /// A message whose input happened at `time`, read now.
        fn seen_at(&mut self, kind: PointerKind, m: PointerMsg, target: Option<Target>, time: u32) {
            let s = Seen {
                time,
                ..Self::seen_as(kind, m, target)
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
        r.mv(system(ON));
        r.advance(Duration::from_secs(1));
        r.press(hardware(ON));
        r.release(hardware(ON));
        assert_eq!(r.qml_click(), Err(GateRefusal::Reenter));
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

    fn system(at: (i32, i32)) -> PointerMsg {
        msg(InputOrigin::System, InputDevice::Mouse, false, at)
    }

    /// A click on button 2, with QML's events.
    fn click_on_two(r: &mut Replay, at: (i32, i32), same: Target) -> Result<(), GateRefusal> {
        r.seen(PointerKind::Press, hardware(at), Some(same));
        r.advance(Duration::from_millis(60));
        r.seen(PointerKind::Release, hardware(at), Some(same));
        r.gate.take_press(2, r.now);
        r.gate.take_click(2, r.now)
    }

    /// The audit's replay, and its posted move: only a real message with the cursor off the button
    /// ends the rule, not a program moving the cursor off and back.
    #[test] // spec: P4a-R10
    fn refuses_a_programs_warp_as_a_way_round_the_re_entry() {
        let new_amount = Target {
            serial: 2,
            rect: RECT,
            changed: true,
        };
        let same = Target {
            changed: false,
            ..new_amount
        };
        let off: [fn((i32, i32)) -> PointerMsg; 3] = [system, posted, injected];
        for away in off {
            // With and without Windows' own move as the cursor comes back.
            for back in [true, false] {
                let mut r = Replay::new();
                r.rest_on_the_button();
                r.seen(PointerKind::Move, hardware((151, 116)), Some(new_amount));
                r.seen(PointerKind::Move, away(OFF), None);
                if back {
                    r.seen(PointerKind::Move, system((151, 116)), Some(same));
                }
                r.seen(PointerKind::Move, hardware((152, 116)), Some(same));
                r.seen(PointerKind::Move, hardware((153, 116)), Some(same));
                r.advance(HOVER_BEFORE_PRESS);
                assert_eq!(
                    click_on_two(&mut r, (153, 116), same),
                    Err(GateRefusal::Reenter)
                );
                // The hand really off and back does end it.
                r.seen(PointerKind::Move, hardware(OFF), None);
                r.seen(PointerKind::Move, hardware((101, 101)), Some(same));
                r.seen(PointerKind::Move, hardware(ON), Some(same));
                r.advance(HOVER_BEFORE_PRESS);
                assert_eq!(click_on_two(&mut r, ON, same), Ok(()));
            }
        }
    }

    /// A program with a low-level hook could time a warp for a hand heading to Deny, next door.
    #[test] // spec: P4a-R10
    fn refuses_a_warp_under_a_moving_hand() {
        let warps: [fn((i32, i32)) -> PointerMsg; 2] = [system, injected];
        for warp in warps {
            let mut r = Replay::new();
            r.mv(hardware(OFF));
            r.mv(hardware((60, 60)));
            r.mv(warp(ON));
            r.mv(hardware((151, 115)));
            r.mv(hardware((152, 115)));
            r.advance(HOVER_BEFORE_PRESS);
            r.press(hardware((152, 115)));
            r.advance(Duration::from_millis(60));
            r.release(hardware((152, 115)));
            assert_eq!(r.qml_click(), Err(GateRefusal::Reenter));
            r.rest_on_the_button();
            r.press(hardware(ON));
            r.advance(Duration::from_millis(60));
            r.release(hardware(ON));
            assert_eq!(r.qml_click(), Ok(()), "off and back ends it");
        }
    }

    /// Posted messages leave the queue before input, and the filter reads the cursor for each: a
    /// program warps, then posts anything the filter reads, so that Windows' own move isn't new.
    #[test] // spec: P4a-R10
    fn a_posted_message_cant_hide_a_warp() {
        type Warp = fn((i32, i32)) -> PointerMsg;
        let warps: [Warp; 2] = [system, injected];
        for warp in warps {
            for post in [
                PointerKind::Move,
                PointerKind::CancelMode,
                PointerKind::CaptureChanged,
                PointerKind::Leave,
            ] {
                let mut r = Replay::new();
                r.mv(hardware(OFF));
                r.mv(hardware((60, 60)));
                r.seen(post, posted(ON), Replay::target_at(ON));
                r.mv(warp(ON));
                r.mv(hardware((151, 115)));
                r.mv(hardware((152, 115)));
                r.advance(HOVER_BEFORE_PRESS);
                r.press(hardware((152, 115)));
                r.advance(Duration::from_millis(60));
                r.release(hardware((152, 115)));
                assert_eq!(r.qml_click(), Err(GateRefusal::Reenter), "{post:?}");
            }
        }
    }

    /// The same behind a new amount: the rule ends at the hand's move off, and a warp back behind a
    /// post starts it again.
    #[test] // spec: P4a-R10
    fn a_warp_back_behind_a_post_starts_the_re_entry_again() {
        let new_amount = Target {
            serial: 2,
            rect: RECT,
            changed: true,
        };
        let same = Target {
            changed: false,
            ..new_amount
        };
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.seen(PointerKind::Move, hardware((151, 116)), Some(new_amount));
        r.seen(PointerKind::Move, system(OFF), None);
        r.seen(PointerKind::Move, hardware((51, 50)), None);
        r.seen(PointerKind::Move, posted((151, 116)), Some(same));
        r.seen(PointerKind::Move, system((151, 116)), Some(same));
        r.seen(PointerKind::Move, hardware((152, 116)), Some(same));
        r.seen(PointerKind::Move, hardware((153, 116)), Some(same));
        r.advance(HOVER_BEFORE_PRESS);
        assert_eq!(
            click_on_two(&mut r, (153, 116), same),
            Err(GateRefusal::Reenter)
        );
    }

    /// A hand's move read after the warp landed: its own point is the old place, and the cursor reads
    /// the new one.
    fn late(at: (i32, i32), message_at: (i32, i32)) -> PointerMsg {
        PointerMsg {
            message_at: Some(message_at),
            ..hardware(at)
        }
    }

    fn click_on_one(r: &mut Replay, at: (i32, i32)) -> Result<(), GateRefusal> {
        r.advance(HOVER_BEFORE_PRESS);
        r.press(hardware(at));
        r.advance(Duration::from_millis(60));
        r.release(hardware(at));
        r.qml_click()
    }

    #[test] // spec: P4a-R10
    fn a_real_move_read_after_a_warp_doesnt_count_as_a_place() {
        let mut r = Replay::new();
        r.mv(hardware(OFF));
        r.mv(hardware((60, 60)));
        r.seen(PointerKind::Move, late(ON, (61, 60)), Replay::target_at(ON));
        r.mv(system(ON));
        r.mv(hardware((151, 115)));
        r.mv(hardware((152, 115)));
        assert_eq!(click_on_one(&mut r, (152, 115)), Err(GateRefusal::Reenter));
    }

    /// Both warps hidden by a hand's moves read late: the one with the cursor off clears nothing.
    #[test] // spec: P4a-R10
    fn a_new_amount_with_both_warps_hidden_by_late_moves_still_needs_the_re_entry() {
        let new_amount = Target {
            serial: 2,
            rect: RECT,
            changed: true,
        };
        let same = Target {
            changed: false,
            ..new_amount
        };
        for windows_own_moves in [true, false] {
            let mut r = Replay::new();
            r.rest_on_the_button();
            r.seen(PointerKind::Move, hardware((151, 116)), Some(new_amount));
            r.seen(PointerKind::Move, late(OFF, (152, 116)), None);
            if windows_own_moves {
                r.seen(PointerKind::Move, system(OFF), None);
            }
            r.seen(PointerKind::Move, late((153, 116), (51, 50)), Some(same));
            if windows_own_moves {
                r.seen(PointerKind::Move, system((153, 116)), Some(same));
            }
            r.seen(PointerKind::Move, hardware((154, 116)), Some(same));
            r.seen(PointerKind::Move, hardware((155, 116)), Some(same));
            r.advance(HOVER_BEFORE_PRESS);
            assert_eq!(
                click_on_two(&mut r, (155, 116), same),
                Err(GateRefusal::Reenter)
            );
        }
    }

    /// What isn't a move says nothing of where a hand put the cursor, whatever its origin reads.
    #[test] // spec: P4a-R10
    fn only_a_move_counts_as_a_place() {
        type Origin = fn((i32, i32)) -> PointerMsg;
        let cases: [(PointerKind, Origin); 6] = [
            (PointerKind::Leave, system),
            (PointerKind::CaptureChanged, system),
            (PointerKind::CancelMode, system),
            (PointerKind::MouseActivate, system),
            (PointerKind::Press, injected),
            (PointerKind::Release, injected),
        ];
        for (kind, origin) in cases {
            let mut r = Replay::new();
            r.mv(hardware(OFF));
            r.mv(hardware((60, 60)));
            r.seen(kind, origin(ON), Replay::target_at(ON));
            r.mv(system(ON));
            r.mv(hardware((151, 115)));
            r.mv(hardware((152, 115)));
            assert_eq!(
                click_on_one(&mut r, (152, 115)),
                Err(GateRefusal::Reenter),
                "{kind:?}"
            );
        }
    }

    /// A fast hand's moves all read a moment late, with no warp: nothing to refuse.
    #[test] // spec: P4a-R10
    fn a_hand_whose_moves_read_late_needs_no_re_entry() {
        let mut r = Replay::new();
        r.mv(hardware(OFF));
        r.seen(PointerKind::Move, late((80, 80), (60, 60)), None);
        r.seen(PointerKind::Move, late((90, 90), (80, 80)), None);
        r.seen(
            PointerKind::Move,
            late((120, 110), (90, 90)),
            Replay::target_at((120, 110)),
        );
        r.seen(
            PointerKind::Move,
            late(ON, (120, 110)),
            Replay::target_at(ON),
        );
        r.mv(hardware(ON));
        r.mv(hardware((151, 115)));
        assert_eq!(click_on_one(&mut r, (151, 115)), Ok(()));
    }

    /// The real run at 95e260f: a click straight on Approve in a window not in front counted. Coming
    /// to the front is now the gate's own: `WM_ACTIVATE` through the hook and the filter, whatever
    /// QML's `Window.active` does. Here the hand rested on Approve while the window was behind.
    #[test] // spec: P4a-R10
    fn refuses_a_click_that_brought_the_window_forward_after_a_rest_behind() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        // The click: Windows sends WM_MOUSEACTIVATE and WM_ACTIVATE, then gives the press.
        r.seen(
            PointerKind::MouseActivate,
            hardware(ON),
            Replay::target_at(ON),
        );
        r.seen(
            PointerKind::FrontClicked,
            hardware(ON),
            Replay::target_at(ON),
        );
        r.press(hardware(ON));
        r.advance(Duration::from_millis(60));
        r.release(hardware(ON));
        assert_eq!(r.qml_click(), Err(GateRefusal::Click(Refusal::NotInFront)));
        // Off and back, rest, click: that counts, half a second on.
        r.advance(Duration::from_millis(500));
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.advance(Duration::from_millis(60));
        r.release(hardware(ON));
        assert_eq!(r.qml_click(), Ok(()));
    }

    /// The same if only `WM_ACTIVATE` reaches the gate: the window came forward under the cursor.
    #[test] // spec: P4a-R10
    fn refuses_a_click_on_a_window_that_came_forward_under_the_cursor() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.seen(PointerKind::FrontLost, hardware(OFF), None);
        r.advance(Duration::from_secs(5));
        r.seen(
            PointerKind::FrontGained,
            hardware(ON),
            Replay::target_at(ON),
        );
        r.mv(hardware((151, 115)));
        r.mv(hardware((152, 115)));
        assert_eq!(click_on_one(&mut r, (152, 115)), Err(GateRefusal::Reenter));
        r.advance(Duration::from_millis(500));
        r.rest_on_the_button();
        assert_eq!(click_on_one(&mut r, ON), Ok(()));
    }

    /// The title bar, (j): the window comes forward with the cursor off every button, and the hand
    /// comes to Approve by moving. Its first click counts.
    #[test] // spec: P4a-R10
    fn accepts_the_first_click_after_the_title_bar_brought_the_window_forward() {
        let mut r = Replay::new();
        r.seen(PointerKind::FrontLost, hardware(OFF), None);
        r.seen(PointerKind::MouseActivate, hardware((60, 10)), None);
        r.seen(PointerKind::FrontClicked, hardware((60, 10)), None);
        // A hand takes a moment from the title bar to Approve.
        r.advance(Duration::from_millis(400));
        r.mv(hardware((70, 40)));
        r.mv(hardware((101, 101)));
        r.mv(hardware(ON));
        assert_eq!(click_on_one(&mut r, ON), Ok(()));
    }

    /// The audit's helper: one sent activation message as the hook then the filter give it, each
    /// stamped with `GetTickCount` on arrival.
    fn twice(r: &mut Replay, kind: PointerKind, at: (i32, i32), hook_tick: u32, filter_tick: u32) {
        let target = Replay::target_at(at);
        r.seen_at(kind, hardware(at), target, hook_tick);
        r.seen_at(kind, hardware(at), target, filter_tick);
    }

    /// The click on Approve in an inactive window as Windows sends it, each sent message through the
    /// hook and the filter, the second a tick later; the press's own time a few ms before.
    #[test] // spec: P4a-R10
    fn a_doubled_click_activation_is_refused_then_a_later_click_counts() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        let t = r.tick();
        twice(&mut r, PointerKind::MouseActivate, ON, t, t);
        twice(&mut r, PointerKind::FrontClicked, ON, t, t.wrapping_add(16));
        r.seen_at(
            PointerKind::Press,
            hardware(ON),
            Replay::target_at(ON),
            t.wrapping_sub(8),
        );
        r.advance(Duration::from_millis(60));
        r.release(hardware(ON));
        assert_eq!(r.qml_click(), Err(GateRefusal::Click(Refusal::NotInFront)));
        r.advance(Duration::from_millis(600));
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.advance(Duration::from_millis(60));
        r.release(hardware(ON));
        assert_eq!(r.qml_click(), Ok(()));
    }

    /// A front more than 2^31 ms old (24.8 days) would read, by the wrapping comparison, as after
    /// the press: it is dropped long before that, so a later click counts.
    #[test] // spec: P4a-R10
    fn a_front_from_long_ago_cant_wrap_into_brought_forward() {
        let mut r = Replay::new();
        r.seen(PointerKind::FrontGained, hardware(OFF), None);
        r.advance(Duration::from_millis((1u64 << 31) + 10_000));
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.advance(Duration::from_millis(60));
        r.release(hardware(ON));
        assert_eq!(r.qml_click(), Ok(()));
    }

    /// A press that happened before its window came to the front, by the input's own time, brought
    /// it forward, whatever the gate was told in between and whatever order Qt reads things in: no
    /// `WM_MOUSEACTIVATE` and no `WA_CLICKACTIVE` here, only the times.
    #[test] // spec: P4a-R10
    fn a_press_from_before_its_window_came_forward_brought_it_forward() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        let pressed_at = r.tick();
        r.advance(Duration::from_millis(30));
        r.seen(
            PointerKind::FrontGained,
            hardware(ON),
            Replay::target_at(ON),
        );
        // The press is read now, its input 30 ms before the activation.
        r.seen_at(
            PointerKind::Press,
            hardware(ON),
            Replay::target_at(ON),
            pressed_at,
        );
        r.advance(Duration::from_millis(60));
        r.release(hardware(ON));
        assert_eq!(r.qml_click(), Err(GateRefusal::Click(Refusal::NotInFront)));
    }

    /// Within half a second of its window coming to the front, by the input's own time: too soon,
    /// however late the thread read it. After that: it counts, however soon it is read.
    #[test] // spec: P4a-R10
    fn a_press_within_half_a_second_of_the_front_is_too_soon_by_its_own_time() {
        let mut r = Replay::new();
        r.seen(PointerKind::FrontGained, hardware(OFF), None);
        let came = r.tick();
        r.mv(hardware((101, 101)));
        r.mv(hardware(ON));
        r.advance(HOVER_BEFORE_PRESS + Duration::from_millis(100));
        // Pressed 250 ms after, read 2 s after: a stalled thread doesn't stretch the half second.
        r.advance(Duration::from_secs(2));
        r.seen_at(
            PointerKind::Press,
            hardware(ON),
            Replay::target_at(ON),
            came.wrapping_add(250),
        );
        r.release(hardware(ON));
        assert_eq!(r.qml_click(), Err(GateRefusal::TooSoon));
        // Pressed 600 ms after the front: it counts.
        r.seen_at(
            PointerKind::Press,
            hardware(ON),
            Replay::target_at(ON),
            came.wrapping_add(600),
        );
        r.release(hardware(ON));
        assert_eq!(r.qml_click(), Ok(()));
    }

    /// Another window of the app coming to the front (a floating panel) is that window's: a click in
    /// this one isn't too soon for it.
    #[test] // spec: P4a-R10
    fn another_windows_front_is_its_own() {
        let mut r = Replay::new();
        let other = Seen {
            window: WINDOW + 1,
            time: r.tick(),
            ..Replay::seen_as(PointerKind::FrontGained, hardware(OFF), None)
        };
        r.gate.message(other, r.now);
        assert_eq!(r.gate.ms_since_front(WINDOW, r.tick(), r.now), None);
        // The hand rests again, within half a second of the other window's front.
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.advance(Duration::from_millis(60));
        r.release(hardware(ON));
        assert_eq!(r.qml_click(), Ok(()));
    }

    /// Only a hand that moves somewhere ends the activation: a real move where the cursor already
    /// was isn't one.
    #[test] // spec: P4a-R10
    fn a_move_in_place_doesnt_end_the_activation() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.seen(
            PointerKind::MouseActivate,
            hardware(ON),
            Replay::target_at(ON),
        );
        r.mv(hardware(ON));
        r.press(hardware(ON));
        r.advance(Duration::from_millis(60));
        r.release(hardware(ON));
        assert_eq!(r.qml_click(), Err(GateRefusal::Click(Refusal::NotInFront)));
    }

    /// The window comes to the front between the activating click's press and its release, so the
    /// release finds a new button: the click still reads as what it was.
    #[test] // spec: P4a-R10
    fn the_activating_click_keeps_its_words_across_the_new_button() {
        let back = Target {
            serial: 2,
            rect: RECT,
            changed: true,
        };
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.seen(
            PointerKind::MouseActivate,
            posted(ON),
            Replay::target_at(ON),
        );
        r.press(hardware(ON));
        r.seen(PointerKind::Release, hardware(ON), Some(back));
        r.gate.take_press(2, r.now);
        assert_eq!(
            r.gate.take_click(2, r.now),
            Err(GateRefusal::Click(Refusal::NotInFront))
        );
    }

    /// The hand on its way back from another window, past what doesn't count as a place.
    #[test] // spec: P4a-R10
    fn a_hand_back_from_another_window_isnt_a_warp() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.seen(PointerKind::Leave, posted((199, 129)), None);
        r.advance(Duration::from_secs(5));
        r.seen(PointerKind::CaptureChanged, posted((120, 115)), None);
        r.mv(hardware((101, 101)));
        r.mv(hardware(ON));
        r.mv(system(ON));
        r.mv(hardware((151, 115)));
        r.advance(HOVER_BEFORE_PRESS);
        r.press(hardware((151, 115)));
        r.advance(Duration::from_millis(60));
        r.release(hardware((151, 115)));
        assert_eq!(r.qml_click(), Ok(()));
    }

    /// `ApproveButton` makes a new button as the window comes to the front, so a small move on it
    /// isn't enough, and the words can't say it is.
    #[test] // spec: P4a-R10
    fn the_activating_clicks_words_say_what_works_here() {
        let words = GateRefusal::words("brought_forward", "Approve").unwrap();
        assert!(words.contains("front"), "{words}");
        assert!(
            words.contains("off Approve"),
            "what gets it to count: {words}"
        );
        assert!(!words.contains("a little"), "{words}");
    }

    #[test] // spec: P4a-R10
    fn the_re_entry_words_cover_a_pointer_that_jumped() {
        let words = GateRefusal::words("reenter", "Approve").unwrap();
        assert!(words.contains("the pointer jumped"), "{words}");
    }

    /// What a hand does, with what Windows and other programs add around it.
    #[test] // spec: P4a-R10
    fn a_normal_hand_never_trips_the_warp_rule() {
        let mut r = Replay::new();
        r.mv(hardware(OFF));
        // Windows' own move off the button, somewhere new: a warp, maybe, but nowhere that matters.
        r.mv(system((60, 60)));
        for x in (70..150).step_by(7) {
            r.mv(hardware((x, 80)));
        }
        r.mv(hardware((101, 101)));
        r.mv(hardware(ON));
        // Windows' own move where the cursor already is.
        r.mv(system(ON));
        // A program's posted move: the message says elsewhere, the cursor is here.
        r.mv(PointerMsg {
            message_at: Some((160, 120)),
            ..posted(ON)
        });
        r.mv(hardware((151, 115)));
        r.advance(HOVER_BEFORE_PRESS);
        r.press(hardware((151, 115)));
        r.advance(Duration::from_millis(60));
        r.release(hardware((151, 115)));
        assert_eq!(r.qml_click(), Ok(()));
    }

    /// `ApproveButton` watches again when its window comes to the front, so the next message finds a
    /// new button. With the cursor on it: "a click meant for the window underneath".
    #[test] // spec: P4a-R10
    fn refuses_a_window_raised_under_a_moving_hand() {
        let raised = Target {
            serial: 2,
            rect: RECT,
            changed: true,
        };
        let same = Target {
            changed: false,
            ..raised
        };
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.seen(PointerKind::Leave, posted(ON), None);
        r.advance(Duration::from_secs(5));
        r.seen(PointerKind::Move, system(ON), Some(raised));
        r.seen(PointerKind::Move, hardware((151, 115)), Some(same));
        r.seen(PointerKind::Move, hardware((152, 115)), Some(same));
        r.advance(HOVER_BEFORE_PRESS);
        assert_eq!(
            click_on_two(&mut r, (152, 115), same),
            Err(GateRefusal::Reenter)
        );
    }

    /// A person's normal case: the title bar brings the window forward, and the hand goes to Approve.
    /// The first message after it finds the cursor off the button, so nothing needs a re-entry, and
    /// the title bar's activation says nothing of a click a minute later.
    #[test] // spec: P4a-R10
    fn accepts_a_click_after_a_title_bar_brought_the_window_forward() {
        let same = Target {
            serial: 2,
            rect: RECT,
            changed: false,
        };
        let mut r = Replay::new();
        r.seen(PointerKind::MouseActivate, posted(OFF), None);
        r.seen(PointerKind::Move, system(OFF), None);
        r.advance(Duration::from_secs(60));
        r.seen(PointerKind::Move, hardware((60, 60)), None);
        r.seen(PointerKind::Move, hardware((101, 101)), Some(same));
        r.seen(PointerKind::Move, hardware(ON), Some(same));
        r.advance(HOVER_BEFORE_PRESS);
        assert_eq!(click_on_two(&mut r, ON, same), Ok(()));
    }

    /// Windows' own move can come between the activation and its press: that's still that press.
    #[test] // spec: P4a-R10
    fn refuses_the_activating_click_past_windows_own_move() {
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.seen(
            PointerKind::MouseActivate,
            posted(ON),
            Replay::target_at(ON),
        );
        r.mv(system(ON));
        r.press(hardware(ON));
        r.release(hardware(ON));
        assert_eq!(r.qml_click(), Err(GateRefusal::Click(Refusal::NotInFront)));
    }

    /// A button that comes back between a press and its release: what to do, not a false alarm.
    #[test] // spec: P4a-R10
    fn says_to_come_back_when_the_button_came_back_during_the_click() {
        let back = Target {
            serial: 2,
            rect: RECT,
            changed: true,
        };
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.seen(PointerKind::Release, hardware(ON), Some(back));
        r.gate.take_press(2, r.now);
        assert_eq!(r.gate.take_click(2, r.now), Err(GateRefusal::Reenter));
    }

    /// QML's events more than a token's lifetime late: the words say that, not that it wasn't a hand.
    #[test] // spec: P4a-R10
    fn says_so_when_a_click_came_too_late_to_check() {
        // Both late.
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.release(hardware(ON));
        r.advance(TOKEN_LIFETIME + Duration::from_millis(200));
        assert_eq!(r.qml_click(), Err(GateRefusal::Stalled));
        // Only QML's press late: the release was just now.
        let mut r = Replay::new();
        r.rest_on_the_button();
        r.press(hardware(ON));
        r.advance(TOKEN_LIFETIME + Duration::from_millis(200));
        r.release(hardware(ON));
        assert_eq!(r.qml_click(), Err(GateRefusal::Stalled));
        // A refusal that came late is late too.
        let mut r = Replay::new();
        r.press(hardware(ON));
        r.release(hardware(ON));
        r.advance(TOKEN_LIFETIME + Duration::from_millis(200));
        assert_eq!(r.qml_click(), Err(GateRefusal::Stalled));
        // Nothing to be late: no press or release was seen at all.
        let mut r = Replay::new();
        r.advance(TOKEN_LIFETIME + Duration::from_millis(200));
        assert_eq!(r.qml_click(), Err(NOT_POINTER));
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
        assert_eq!(r.qml_click(), Err(GateRefusal::Stalled));
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
    fn the_log_limit_lets_one_line_a_second_through() {
        let mut limit = LogLimit::default();
        let start = Instant::now();
        assert!(limit.due(start));
        assert!(!limit.due(start));
        assert!(!limit.due(start + Duration::from_millis(999)));
        assert!(limit.due(start + Duration::from_secs(1)));
        assert!(!limit.due(start + Duration::from_millis(1500)));
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
            GateRefusal::Stalled,
            GateRefusal::TooSoon,
            GateRefusal::NotExposed,
            GateRefusal::NoGuard,
        ] {
            let words = GateRefusal::words(refusal.name(), "Approve").unwrap();
            assert!(words.contains("Approve"), "{refusal:?}: {words}");
        }
    }
}
