//! What Windows says of the pointer message this thread is handling, read the way the Windows
//! Hello window reads it ([`crate::hello`], whose own readers are private to it), for a window
//! whose buttons aren't windows of their own: `lil_qt`'s pointer gate (P4a-R10).
//!
//! Nothing here decides anything: [`crate::hello::ClickGate`] does.

use windows::Win32::Foundation::{HWND, LPARAM, POINT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::Input::{
    GetCurrentInputMessageSource, IMDT_KEYBOARD, IMDT_MOUSE, IMDT_PEN, IMDT_TOUCH, IMDT_TOUCHPAD,
    IMO_HARDWARE, IMO_INJECTED, IMO_SYSTEM, INPUT_MESSAGE_SOURCE,
};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetForegroundWindow, InSendMessage};

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
        WINDOW_EX_STYLE, WINDOW_STYLE, WM_LBUTTONUP, WM_USER, WNDCLASSW,
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

    /// A mouse message sent from another thread says so.
    #[test] // spec: P4a-R10
    fn a_release_sent_from_another_thread_says_so() {
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
