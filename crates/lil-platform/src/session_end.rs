//! The session-end window (ARCH-021, ARCH-005).
//!
//! Windows tells applications that the session is ending by sending `WM_QUERYENDSESSION` and
//! then `WM_ENDSESSION` to their top-level windows. A message-only window (`HWND_MESSAGE`)
//! does not get them, so the core owns one hidden top-level window of the class
//! `LilithCoreSessionEnd`. It is created without `WS_VISIBLE` and is never shown
//! (ARCH-005). It runs on its own thread with its own message loop.
//!
//! - `WM_QUERYENDSESSION` → [`SessionEnd::QueryEnd`] is reported and the window answers TRUE.
//!   Another application can still cancel the logoff, so the core only logs it.
//! - `WM_ENDSESSION` with wParam TRUE → [`SessionEnd::End`] is reported, then the window
//!   blocks until [`SessionEndWatcher::shutdown_complete`] or [`SESSION_END_BUDGET`],
//!   whichever comes first: Windows ends the process once the message returns.
//!
//! **Ownership.** Only the watcher destroys its window, in `Drop`, with a private message
//! (`WM_APP + 1` carrying a per-watcher random key). A `WM_CLOSE` from anywhere else is
//! ignored and reported as [`SessionEnd::CloseIgnored`], and a `WM_QUIT` posted from outside
//! does not end the message loop, so the window lives exactly as long as its watcher.
//!
//! The same window reports the sleep broadcasts (ARCH-127; Appendix G "Keep-awake"):
//! `WM_POWERBROADCAST` with `PBT_APMSUSPEND` → [`PowerEvent::Suspending`], with
//! `PBT_APMRESUMEAUTOMATIC` → [`PowerEvent::Resumed`], on the receiver
//! [`SessionEndWatcher::power_events`] hands out once. Task 1's S1.7 saw both arrive at a
//! hidden top-level window across a real sleep.
//!
//! [`SessionEndWatcher::simulate`] sends those messages to the watcher's own window, for
//! tests. What it sends is a [`SessionEndSim`], a type of its own: the [`SessionEnd`] events are
//! only what the window reports. Nothing here looks at or enumerates any other window.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::io;
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use windows::Win32::Foundation::{
    ERROR_CLASS_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, IsWindow,
    IsWindowVisible, MSG, PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND, PostMessageW, PostQuitMessage,
    RegisterClassW, SendMessageW, TranslateMessage, WINDOW_EX_STYLE, WM_APP, WM_CLOSE, WM_DESTROY,
    WM_ENDSESSION, WM_POWERBROADCAST, WM_QUERYENDSESSION, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::{PCWSTR, w};

use crate::process::io_error;

/// How long `WM_ENDSESSION` waits for the core's shutdown: under the roughly 5 s Windows
/// allows before it ends the process anyway.
pub const SESSION_END_BUDGET: Duration = Duration::from_millis(4500);

/// The window class.
const CLASS_NAME: PCWSTR = w!("LilithCoreSessionEnd");

/// The private message that destroys the window; its lParam must be the watcher's key.
const WM_DESTROY_WATCHER: u32 = WM_APP + 1;

/// What [`SessionEndWatcher::simulate`] sends to the watcher's own window (tests). It is not
/// a [`SessionEnd`]: those are what the window reports, and `Close` is reported as
/// [`SessionEnd::CloseIgnored`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEndSim {
    /// `WM_QUERYENDSESSION`.
    QueryEnd,
    /// `WM_ENDSESSION` with wParam TRUE.
    End,
    /// `WM_CLOSE`, as another program could send it.
    Close,
    /// `WM_POWERBROADCAST` with `PBT_APMSUSPEND`.
    Suspend,
    /// `WM_POWERBROADCAST` with `PBT_APMRESUMEAUTOMATIC`.
    Resume,
}

/// A sleep broadcast the window was told (ARCH-127).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerEvent {
    /// `PBT_APMSUSPEND`: the PC is going to sleep.
    Suspending,
    /// `PBT_APMRESUMEAUTOMATIC`: the PC woke up.
    Resumed,
}

/// What the window was told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEnd {
    /// `WM_QUERYENDSESSION`: the session may end; another application can still cancel it.
    QueryEnd,
    /// `WM_ENDSESSION` with wParam TRUE: the session ends.
    End,
    /// A `WM_CLOSE` (or a destroy message without the watcher's key) came from outside the
    /// watcher; it was ignored and the window lives on.
    CloseIgnored,
}

/// What the window thread and the watcher share.
#[derive(Debug)]
struct Shared {
    tx: UnboundedSender<SessionEnd>,
    power_tx: UnboundedSender<PowerEvent>,
    state: Mutex<Waiting>,
    changed: Condvar,
    /// The lParam of [`WM_DESTROY_WATCHER`]: random, so only the watcher's `Drop` sends it.
    destroy_key: isize,
}

#[derive(Debug, Default)]
struct Waiting {
    /// [`SessionEndWatcher::shutdown_complete`] was called.
    complete: bool,
    /// A `WM_ENDSESSION` handler is inside its wait.
    waiting: bool,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Waiting> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `WM_ENDSESSION`: waits until the shutdown is complete or the budget is spent.
    fn wait_for_shutdown(&self) {
        let deadline = Instant::now() + SESSION_END_BUDGET;
        let mut state = self.lock();
        state.waiting = true;
        while !state.complete {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        state.waiting = false;
    }
}

thread_local! {
    /// The window thread's [`Shared`], for the window procedure (which runs on that thread,
    /// also for messages sent from other threads).
    static SHARED: RefCell<Option<Arc<Shared>>> = const { RefCell::new(None) };
    /// The window thread got `WM_DESTROY`: the next `WM_QUIT` ends its message loop.
    static DESTROYED: Cell<bool> = const { Cell::new(false) };
}

/// The hidden session-end window and its thread. Dropping it destroys the window; nothing
/// else does (module docs).
#[derive(Debug)]
pub struct SessionEndWatcher {
    /// The window handle's value (an `HWND` is not `Send`).
    hwnd: isize,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    /// The sleep broadcasts' receiver until [`SessionEndWatcher::power_events`] takes it.
    power_rx: Mutex<Option<UnboundedReceiver<PowerEvent>>>,
}

impl SessionEndWatcher {
    /// Creates the hidden window on a new thread. The receiver gets every
    /// [`SessionEnd`] the window is told.
    ///
    /// # Errors
    /// If the window class cannot be registered, or the thread or window cannot be created.
    pub fn start() -> io::Result<(SessionEndWatcher, UnboundedReceiver<SessionEnd>)> {
        let (tx, rx) = unbounded_channel();
        let (power_tx, power_rx) = unbounded_channel();
        let mut key = [0u8; 8];
        getrandom::fill(&mut key).map_err(|e| io::Error::other(e.to_string()))?;
        let shared = Arc::new(Shared {
            tx,
            power_tx,
            state: Mutex::new(Waiting::default()),
            changed: Condvar::new(),
            // Never 0, so a message without an lParam never matches.
            destroy_key: isize::from_ne_bytes(key) | 1,
        });
        let (created_tx, created_rx) = std::sync::mpsc::channel::<io::Result<isize>>();
        let for_thread = shared.clone();
        let thread = std::thread::Builder::new()
            .name("lil-session-end".into())
            .spawn(move || window_thread(for_thread, &created_tx))?;
        let hwnd = match created_rx.recv() {
            Ok(Ok(hwnd)) => hwnd,
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => {
                let _ = thread.join();
                return Err(io::Error::other("the session-end window thread ended"));
            }
        };
        Ok((
            SessionEndWatcher {
                hwnd,
                shared,
                thread: Some(thread),
                power_rx: Mutex::new(Some(power_rx)),
            },
            rx,
        ))
    }

    /// The receiver of every [`PowerEvent`] the window is told. The first call takes it; later
    /// calls return `None`.
    pub fn power_events(&self) -> Option<UnboundedReceiver<PowerEvent>> {
        self.power_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// The core's shutdown is complete: a waiting `WM_ENDSESSION`, and any later one, returns.
    pub fn shutdown_complete(&self) {
        self.shared.lock().complete = true;
        self.shared.changed.notify_all();
    }

    /// Sends `WM_QUERYENDSESSION`, or `WM_ENDSESSION` with wParam TRUE, to this watcher's own
    /// window, and waits for the window to handle it. For `QueryEnd` the result is the
    /// window's answer (TRUE: the session may end); for `End`, whether the message was
    /// handled (it returns once the shutdown is complete or the budget is spent).
    ///
    /// `Close` sends `WM_CLOSE`, as another program could; the result is whether the window
    /// still answers afterwards.
    pub fn simulate(&self, msg: SessionEndSim) -> bool {
        let hwnd = self.hwnd();
        match msg {
            SessionEndSim::QueryEnd => {
                // SAFETY: `hwnd` is this watcher's live window: only `Drop` destroys it
                // (module docs). The message carries no pointers.
                let answer = unsafe { SendMessageW(hwnd, WM_QUERYENDSESSION, None, None) };
                answer.0 != 0
            }
            SessionEndSim::End => {
                // SAFETY: as above.
                unsafe { SendMessageW(hwnd, WM_ENDSESSION, Some(WPARAM(1)), Some(LPARAM(0))) };
                true
            }
            SessionEndSim::Close => {
                // SAFETY: as above; the window ignores the message.
                unsafe { SendMessageW(hwnd, WM_CLOSE, None, None) };
                self.is_alive()
            }
            SessionEndSim::Suspend | SessionEndSim::Resume => {
                let event = if msg == SessionEndSim::Suspend {
                    PBT_APMSUSPEND
                } else {
                    PBT_APMRESUMEAUTOMATIC
                };
                // SAFETY: as above. The broadcast's wParam is the event; lParam is unused.
                let answer = unsafe {
                    SendMessageW(
                        hwnd,
                        WM_POWERBROADCAST,
                        Some(WPARAM(event as usize)),
                        Some(LPARAM(0)),
                    )
                };
                // TRUE: the broadcast was handled.
                answer.0 != 0
            }
        }
    }

    /// Whether the window still exists. It does until the watcher is dropped.
    pub fn is_alive(&self) -> bool {
        // SAFETY: plain call on a handle value; a destroyed window reads as `false`.
        unsafe { IsWindow(Some(self.hwnd())) }.as_bool()
    }

    /// Whether the window is visible. It never is (ARCH-005).
    pub fn is_visible(&self) -> bool {
        // SAFETY: `hwnd` is this watcher's live window; the call only reads its style.
        unsafe { IsWindowVisible(self.hwnd()) }.as_bool()
    }

    /// Whether a `WM_ENDSESSION` handler is waiting for [`Self::shutdown_complete`].
    pub fn is_waiting_for_shutdown(&self) -> bool {
        self.shared.lock().waiting
    }

    fn hwnd(&self) -> HWND {
        HWND(self.hwnd as *mut c_void)
    }
}

impl Drop for SessionEndWatcher {
    /// Lets a waiting `WM_ENDSESSION` return, destroys the window (the private message with
    /// the key) and waits for its thread.
    fn drop(&mut self) {
        self.shutdown_complete();
        // SAFETY: `hwnd` is this watcher's window, alive until this message is handled;
        // posting carries no pointers. If posting fails anyway, the thread is not waited for.
        let posted = unsafe {
            PostMessageW(
                Some(self.hwnd()),
                WM_DESTROY_WATCHER,
                WPARAM(0),
                LPARAM(self.shared.destroy_key),
            )
        };
        if let Some(thread) = self.thread.take()
            && posted.is_ok()
            && thread.thread().id() != std::thread::current().id()
        {
            let _ = thread.join();
        }
    }
}

/// Registers the window class once per process.
fn register_class(instance: HINSTANCE) -> io::Result<()> {
    static REGISTERED: OnceLock<Result<(), i32>> = OnceLock::new();
    let registered = REGISTERED.get_or_init(|| {
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        // SAFETY: `class` is fully initialised and names a static class name and a window
        // procedure that lives for the whole process.
        if unsafe { RegisterClassW(&class) } != 0 {
            return Ok(());
        }
        // SAFETY: plain call; reads this thread's last error.
        let error = unsafe { GetLastError() };
        if error == ERROR_CLASS_ALREADY_EXISTS {
            Ok(())
        } else {
            Err(error.0 as i32)
        }
    });
    registered.map_err(io::Error::from_raw_os_error)
}

/// The window thread: creates the hidden window, reports its handle, then runs the message
/// loop until the window is destroyed.
fn window_thread(shared: Arc<Shared>, created: &std::sync::mpsc::Sender<io::Result<isize>>) {
    SHARED.with(|s| *s.borrow_mut() = Some(shared));
    let made = (|| -> io::Result<HWND> {
        // SAFETY: plain call; `None` asks for this executable's module handle, which is not
        // freed.
        let module = unsafe { GetModuleHandleW(None) }.map_err(|e| io_error(&e))?;
        let instance = HINSTANCE(module.0);
        register_class(instance)?;
        // SAFETY: the class is registered; the strings are static. No parent: a top-level
        // window (a message-only one gets no session messages). No `WS_VISIBLE` and no
        // `ShowWindow`: it is never shown (ARCH-005).
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS_NAME,
                w!("LILITH core"),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            )
        }
        .map_err(|e| io_error(&e))
    })();
    match made {
        Ok(hwnd) => {
            let _ = created.send(Ok(hwnd.0 as isize));
        }
        Err(e) => {
            let _ = created.send(Err(e));
            return;
        }
    }
    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` is a valid out-pointer; `None` takes messages for every window of
        // this thread (only the session-end window).
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        // -1: an error.
        if got.0 == -1 {
            break;
        }
        // 0: WM_QUIT. Only the one `WM_DESTROY` posts ends the loop; one posted from
        // outside is ignored.
        if got.0 == 0 {
            if DESTROYED.with(Cell::get) {
                break;
            }
            continue;
        }
        // SAFETY: `msg` was filled by `GetMessageW`.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    SHARED.with(|s| s.borrow_mut().take());
}

/// The window procedure. Runs on the window thread.
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let shared = || SHARED.with(|s| s.borrow().clone());
    match msg {
        WM_QUERYENDSESSION => {
            if let Some(shared) = shared() {
                let _ = shared.tx.send(SessionEnd::QueryEnd);
            }
            LRESULT(1)
        }
        WM_ENDSESSION => {
            if wparam.0 != 0
                && let Some(shared) = shared()
            {
                let _ = shared.tx.send(SessionEnd::End);
                shared.wait_for_shutdown();
            }
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            let event = match wparam.0 as u32 {
                PBT_APMSUSPEND => Some(PowerEvent::Suspending),
                PBT_APMRESUMEAUTOMATIC => Some(PowerEvent::Resumed),
                _ => None,
            };
            if let (Some(event), Some(shared)) = (event, shared()) {
                let _ = shared.power_tx.send(event);
            }
            // TRUE: handled (the broadcast's documented answer).
            LRESULT(1)
        }
        WM_DESTROY_WATCHER if shared().is_some_and(|s| lparam.0 == s.destroy_key) => {
            // SAFETY: `hwnd` is the window this procedure serves, on its own thread; the key
            // shows the watcher's `Drop` sent this.
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        // From outside the watcher: the window stays (module docs).
        WM_CLOSE | WM_DESTROY_WATCHER => {
            if let Some(shared) = shared() {
                let _ = shared.tx.send(SessionEnd::CloseIgnored);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            DESTROYED.with(|d| d.set(true));
            // SAFETY: plain call; ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: the default handling of a message this window does not handle, with the
        // arguments Windows passed in.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
