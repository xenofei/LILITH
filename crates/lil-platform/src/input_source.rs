//! What Windows says of the pointer message this thread is handling, read the way the Windows
//! Hello window reads it ([`crate::hello`], whose own readers are private to it), for a window
//! whose buttons aren't windows of their own: `lil_qt`'s pointer gate (P4a-R10).
//!
//! Nothing here decides anything: [`crate::hello::ClickGate`] does.
//!
//! Microsoft documents `IMO_HARDWARE` as covering a UIAccess application's injection as well, and
//! documents nothing about what a posted message reports (`hello::InputOrigin`), so a "hardware"
//! source means no more than that. [`left_button_physically_down`] is the second factor.

use windows::Win32::Foundation::{HWND, LPARAM, POINT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyState, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_LBUTTON,
    VK_RBUTTON,
};
use windows::Win32::UI::Input::{
    GetCurrentInputMessageSource, IMDT_KEYBOARD, IMDT_MOUSE, IMDT_PEN, IMDT_TOUCH, IMDT_TOUCHPAD,
    IMO_HARDWARE, IMO_INJECTED, IMO_SYSTEM, INPUT_MESSAGE_SOURCE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GA_ROOT, GWL_EXSTYLE, GetAncestor, GetCursorPos, GetForegroundWindow,
    GetLayeredWindowAttributes, GetSystemMetrics, GetWindowLongPtrW, InSendMessage, IsIconic,
    IsWindowVisible, LAYERED_WINDOW_ATTRIBUTES_FLAGS, LWA_ALPHA, LWA_COLORKEY, SM_SWAPBUTTON,
    WS_EX_LAYERED, WS_EX_TRANSPARENT, WindowFromPoint,
};

use crate::hello::{InputDevice, InputOrigin, PointerMsg};

fn hwnd(window: isize) -> HWND {
    HWND(window as *mut core::ffi::c_void)
}

/// The origin and device of the input message this thread is handling. A failed query is
/// `Unavailable`, which never counts as real.
pub fn current_source() -> (InputOrigin, InputDevice) {
    let mut source = INPUT_MESSAGE_SOURCE::default();
    // SAFETY: a plain query into a local structure.
    if unsafe { GetCurrentInputMessageSource(&mut source) }.is_err() {
        return (InputOrigin::Unavailable, InputDevice::Unavailable);
    }
    let origin = match source.originId {
        IMO_HARDWARE => InputOrigin::Hardware,
        IMO_INJECTED => InputOrigin::Injected,
        IMO_SYSTEM => InputOrigin::System,
        _ => InputOrigin::Unavailable,
    };
    let device = match source.deviceType {
        IMDT_MOUSE => InputDevice::Mouse,
        IMDT_TOUCH => InputDevice::Touch,
        IMDT_PEN => InputDevice::Pen,
        IMDT_KEYBOARD => InputDevice::Keyboard,
        IMDT_TOUCHPAD => InputDevice::Touchpad,
        _ => InputDevice::Unavailable,
    };
    (origin, device)
}

/// The mouse message to `window` with `lparam` that this thread is handling: its source, where the
/// cursor really is (`GetCursorPos`, never the message's own point, which a program that posts it
/// chooses), and the message's own point in screen coordinates, only to compare with it.
pub fn pointer_msg(window: isize, lparam: isize) -> PointerMsg {
    let (origin, device) = current_source();
    let mut cursor = POINT::default();
    // SAFETY: a query into a local structure.
    let at = unsafe { GetCursorPos(&mut cursor) }
        .ok()
        .map(|()| (cursor.x, cursor.y));
    let lp = LPARAM(lparam);
    let mut point = POINT {
        x: i32::from((lp.0 & 0xffff) as u16 as i16),
        y: i32::from(((lp.0 >> 16) & 0xffff) as u16 as i16),
    };
    // SAFETY: a local point; a window that has gone fails the call.
    let message_at = unsafe { ClientToScreen(hwnd(window), &mut point) }
        .as_bool()
        .then_some((point.x, point.y));
    PointerMsg {
        origin,
        device,
        // SAFETY: a query about the message this thread is handling.
        sent: unsafe { InSendMessage() }.as_bool(),
        at,
        message_at,
    }
}

/// Whether `window` is the foreground window.
pub fn in_front(window: isize) -> bool {
    // SAFETY: a plain query.
    let front = unsafe { GetForegroundWindow() };
    front == hwnd(window)
}

/// Whether the left button is physically down now. `GetAsyncKeyState` is documented as reading the
/// physical button, so no message can change it; it reads the physical button that is mapped to the
/// logical left one (`SM_SWAPBUTTON`, as its documentation says to). A failed call reads as up.
pub fn left_button_physically_down() -> bool {
    // SAFETY: plain queries.
    unsafe {
        let key = if GetSystemMetrics(SM_SWAPBUTTON) != 0 {
            VK_RBUTTON
        } else {
            VK_LBUTTON
        };
        GetAsyncKeyState(i32::from(key.0)) < 0
    }
}

/// Whether this thread's own key state says the left button is down: the state "as a thread reads key
/// messages", kept for the log, which measures it next to the physical one.
pub fn left_button_down_by_messages() -> bool {
    // SAFETY: a plain query.
    unsafe { GetKeyState(i32::from(VK_LBUTTON.0)) < 0 }
}

/// What can make a window impossible to see while it still takes clicks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exposure {
    pub visible: bool,
    pub iconic: bool,
    pub cloaked: bool,
    /// `WS_EX_TRANSPARENT`.
    pub click_through: bool,
    /// `WS_EX_LAYERED`, with `GetLayeredWindowAttributes`'s `(alpha, uses alpha, uses a colour key)`,
    /// `None` inside when that call failed (a window drawn with `UpdateLayeredWindow`).
    pub layered: Option<Option<(u8, bool, bool)>>,
}

impl Exposure {
    /// Whether nothing here hides it (pure). A layered window counts only when fully opaque by its
    /// own attributes; one that can't be read doesn't.
    pub fn is_seen(self) -> bool {
        self.visible
            && !self.iconic
            && !self.cloaked
            && !self.click_through
            && match self.layered {
                None => true,
                Some(Some((alpha, uses_alpha, uses_key))) => {
                    (!uses_alpha || alpha == 255) && !uses_key
                }
                Some(None) => false,
            }
    }
}

/// What `window` says of itself now. Anything that can't be read counts against it.
pub fn exposure(window: isize) -> Exposure {
    let w = hwnd(window);
    // SAFETY: queries about a window, into local values; a window that has gone fails them.
    unsafe {
        let ex_style = GetWindowLongPtrW(w, GWL_EXSTYLE) as u32;
        let mut cloaked = 0u32;
        let cloak_read = DwmGetWindowAttribute(
            w,
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast(),
            size_of::<u32>() as u32,
        )
        .is_ok();
        let layered = (ex_style & WS_EX_LAYERED.0 != 0).then(|| {
            let (mut alpha, mut flags) = (0u8, LAYERED_WINDOW_ATTRIBUTES_FLAGS(0));
            GetLayeredWindowAttributes(w, None, Some(&mut alpha), Some(&mut flags))
                .ok()
                .map(|()| {
                    (
                        alpha,
                        flags.0 & LWA_ALPHA.0 != 0,
                        flags.0 & LWA_COLORKEY.0 != 0,
                    )
                })
        });
        Exposure {
            visible: IsWindowVisible(w).as_bool(),
            iconic: IsIconic(w).as_bool(),
            cloaked: !cloak_read || cloaked != 0,
            click_through: ex_style & WS_EX_TRANSPARENT.0 != 0,
            layered,
        }
    }
}

/// Whether the top-level window under `at` is `window`, and nothing about it hides it. It can't see a
/// program's own see-through window drawn over this one that lets the click fall through.
pub fn exposed_at(window: isize, at: Option<(i32, i32)>) -> bool {
    let Some((x, y)) = at else {
        return false;
    };
    // SAFETY: plain queries.
    let top = unsafe { GetAncestor(WindowFromPoint(POINT { x, y }), GA_ROOT) };
    top == hwnd(window) && exposure(window).is_seen()
}

/// Arms `TME_LEAVE` on `window`, this thread's own; whether it is armed.
pub fn track_leave(window: isize) -> bool {
    let mut track = TRACKMOUSEEVENT {
        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: hwnd(window),
        dwHoverTime: 0,
    };
    // SAFETY: a local structure.
    unsafe { TrackMouseEvent(&mut track) }.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use windows::Win32::Foundation::{LRESULT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, MSG,
        PM_REMOVE, PeekMessageW, PostMessageW, PostQuitMessage, RegisterClassW, SendMessageW,
        SetLayeredWindowAttributes, SetWindowLongPtrW, WINDOW_EX_STYLE, WINDOW_STYLE,
        WM_LBUTTONDOWN, WM_LBUTTONUP, WM_USER, WNDCLASSW,
    };
    use windows::core::w;

    static SENT_SEEN: AtomicBool = AtomicBool::new(false);

    extern "system" fn proc(window: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if msg == WM_LBUTTONUP && pointer_msg(window.0 as isize, lp.0).sent {
            SENT_SEEN.store(true, Ordering::SeqCst);
        }
        if msg == WM_USER {
            // SAFETY: this thread's own message loop.
            unsafe { PostQuitMessage(0) };
        }
        // SAFETY: the default procedure for this window's message.
        unsafe { DefWindowProcW(window, msg, wp, lp) }
    }

    /// A hidden window on this thread: nothing shows, and no input reaches it.
    fn hidden_window() -> HWND {
        let class = WNDCLASSW {
            lpfnWndProc: Some(proc),
            lpszClassName: w!("LILITH-input-source-test"),
            ..Default::default()
        };
        // SAFETY: standard calls; a second registration fails harmlessly.
        unsafe {
            RegisterClassW(&class);
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("LILITH-input-source-test"),
                w!(""),
                WINDOW_STYLE::default(),
                0,
                0,
                10,
                10,
                None,
                None,
                None,
                None,
            )
            .expect("a hidden window")
        }
    }

    /// A posted mouse message is never reported as a real device's.
    #[test] // spec: P4a-R10
    fn a_posted_release_is_not_hardware() {
        let window = hidden_window();
        // SAFETY: this thread's own hidden window and queue.
        unsafe {
            PostMessageW(Some(window), WM_LBUTTONUP, WPARAM(0), LPARAM(0)).unwrap();
            let mut got = MSG::default();
            assert!(
                PeekMessageW(
                    &mut got,
                    Some(window),
                    WM_LBUTTONUP,
                    WM_LBUTTONUP,
                    PM_REMOVE
                )
                .as_bool()
            );
            let m = pointer_msg(window.0 as isize, got.lParam.0);
            assert_ne!(m.origin, InputOrigin::Hardware, "{m:?}");
            assert!(!m.sent, "posted, not sent");
            let _ = DestroyWindow(window);
        }
    }

    /// A posted press doesn't move what the thread believes of the button, nor the physical button
    /// (Windows' documented second factor, measured here on the thread that read the message). The
    /// physical button is the person's: a press of theirs during the test only makes it try again.
    #[test] // spec: P4a-R10
    fn a_posted_press_moves_no_button_state() {
        let window = hidden_window();
        let mut clean = false;
        for _ in 0..20 {
            // SAFETY: this thread's own hidden window and queue.
            unsafe {
                assert!(
                    !left_button_down_by_messages(),
                    "no input read on this thread"
                );
                let before = left_button_physically_down();
                PostMessageW(Some(window), WM_LBUTTONDOWN, WPARAM(1), LPARAM(0)).unwrap();
                let mut got = MSG::default();
                assert!(
                    PeekMessageW(
                        &mut got,
                        Some(window),
                        WM_LBUTTONDOWN,
                        WM_LBUTTONDOWN,
                        PM_REMOVE
                    )
                    .as_bool()
                );
                DispatchMessageW(&got);
                assert!(
                    !left_button_down_by_messages(),
                    "a posted press changed the thread's key state"
                );
                if !before && !left_button_physically_down() {
                    clean = true;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(clean, "the physical button read as down throughout");
        // SAFETY: this thread's own window.
        let _ = unsafe { DestroyWindow(window) };
    }

    #[test] // spec: P4a-R10
    fn a_window_that_cant_be_seen_doesnt_count() {
        let seen = Exposure {
            visible: true,
            iconic: false,
            cloaked: false,
            click_through: false,
            layered: None,
        };
        assert!(seen.is_seen());
        assert!(
            !Exposure {
                visible: false,
                ..seen
            }
            .is_seen()
        );
        assert!(
            !Exposure {
                iconic: true,
                ..seen
            }
            .is_seen()
        );
        assert!(
            !Exposure {
                cloaked: true,
                ..seen
            }
            .is_seen()
        );
        assert!(
            !Exposure {
                click_through: true,
                ..seen
            }
            .is_seen()
        );
        let layered = |l| {
            Exposure {
                layered: Some(l),
                ..seen
            }
            .is_seen()
        };
        assert!(layered(Some((255, true, false))), "opaque");
        assert!(layered(Some((0, false, false))), "no alpha in use");
        assert!(!layered(Some((1, true, false))), "see-through");
        assert!(!layered(Some((254, true, false))), "see-through");
        assert!(!layered(Some((255, true, true))), "a colour key");
        assert!(!layered(None), "can't be read");
    }

    /// The real reads, on a hidden window (nothing shows): a program's `WS_EX_LAYERED` and alpha, and
    /// `WS_EX_TRANSPARENT`, read as they were set.
    #[test] // spec: P4a-R10
    fn exposure_reads_what_a_program_set_on_the_window() {
        let window = hidden_window();
        let id = window.0 as isize;
        let e = exposure(id);
        assert!(!e.visible, "it is hidden");
        assert!(!e.click_through && e.layered.is_none(), "{e:?}");
        assert!(
            !exposed_at(id, Some((0, 0))),
            "a hidden window is never under a point"
        );
        // SAFETY: this thread's own hidden window.
        unsafe {
            SetWindowLongPtrW(
                window,
                GWL_EXSTYLE,
                (WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0) as isize,
            );
            SetLayeredWindowAttributes(window, Default::default(), 1, LWA_ALPHA).unwrap();
        }
        let e = exposure(id);
        assert!(e.click_through, "{e:?}");
        assert_eq!(e.layered, Some(Some((1, true, false))), "{e:?}");
        // SAFETY: this thread's own window.
        let _ = unsafe { DestroyWindow(window) };
    }

    /// The reader says when a message was sent from another thread, on a plain window. Qt's path for
    /// a sent message skips its filter: `tst_confirmations` proves the hook sees it.
    #[test] // spec: P4a-R10
    fn the_reader_says_when_a_message_was_sent() {
        let (tx, rx) = mpsc::channel();
        let pump = std::thread::spawn(move || {
            let window = hidden_window();
            tx.send(window.0 as isize).unwrap();
            let mut got = MSG::default();
            // SAFETY: this thread's own message loop and window.
            unsafe {
                while GetMessageW(&mut got, None, 0, 0).as_bool() {
                    DispatchMessageW(&got);
                }
                let _ = DestroyWindow(window);
            }
        });
        let window = hwnd(rx.recv().unwrap());
        // SAFETY: a window that lives until the WM_USER below ends its loop.
        unsafe {
            SendMessageW(window, WM_LBUTTONUP, None, None);
            SendMessageW(window, WM_USER, None, None);
        }
        pump.join().unwrap();
        assert!(SENT_SEEN.load(Ordering::SeqCst));
    }
}
