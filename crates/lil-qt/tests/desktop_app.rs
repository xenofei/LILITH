//! The deployed app on the real desktop (QA-042; P4a-R39's host set): tests named `desktop_*`,
//! marked `#[ignore = "desktop"]`, which run only through `lil dev desktop-tests --host`, under
//! the hands-off pop-up, with `LIL_DEPLOY_DIR` naming the deploy folder (`build\dev\deploy`).
//! Each starts that folder's `lil-image.exe` on a test channel of its own, with a core started
//! from the same folder (which takes it for `ui:image`), and leaves nothing running.

use std::ffi::c_void;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use lil_platform::guard::require_desktop_group;
use lil_platform::hello::{OwnerWindow, Raise};
use lil_testkit::{CoreFixture, TestEnv};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT, SendInput,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GA_ROOT, GetAncestor, GetClassNameW, GetForegroundWindow, GetSystemMetrics,
    GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, HWND_NOTOPMOST, HWND_TOPMOST,
    IsIconic, IsWindowVisible, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SetWindowPos,
    SwitchToThisWindow, WindowFromPoint,
};
use windows::core::BOOL;

/// A failure deadline for each wait, never a measurement.
const DEADLINE: Duration = Duration::from_secs(60);

fn deployed_app() -> PathBuf {
    let dir = std::env::var_os("LIL_DEPLOY_DIR").expect("LIL_DEPLOY_DIR names the deploy folder");
    let exe = PathBuf::from(dir).join("lil-image.exe");
    assert!(exe.is_file(), "no {}", exe.display());
    exe
}

/// A process that is killed when this is dropped.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_app(env: &TestEnv, args: &[&str]) -> Child {
    Command::new(deployed_app())
        .args(args)
        .envs(env.child_env())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start lil-image.exe")
}

/// The pid of the foreground window's process.
fn foreground_pid() -> u32 {
    let mut pid = 0u32;
    // SAFETY: plain queries; a null window gives pid 0.
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid)) };
    pid
}

/// A visible top-level window of `pid` whose title starts "LILITH".
fn app_window(pid: u32) -> Option<HWND> {
    struct Find {
        pid: u32,
        found: Option<HWND>,
    }
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` is the `Find` passed to EnumWindows, alive for the call.
        let find = unsafe { &mut *(lparam.0 as *mut Find) };
        let mut pid = 0u32;
        // SAFETY: plain window queries on a window EnumWindows gave.
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == find.pid && IsWindowVisible(hwnd).as_bool() {
                let mut title = [0u16; 128];
                let n = GetWindowTextW(hwnd, &mut title);
                let title = String::from_utf16_lossy(&title[..usize::try_from(n).unwrap_or(0)]);
                if title.starts_with("LILITH") {
                    find.found = Some(hwnd);
                    return false.into();
                }
            }
        }
        true.into()
    }
    let mut find = Find { pid, found: None };
    // SAFETY: `each` reads `find` only during the call.
    let _ = unsafe { EnumWindows(Some(each), LPARAM((&raw mut find) as isize)) };
    find.found
}

/// The window class of `hwnd`.
fn class_of(hwnd: HWND) -> String {
    let mut name = [0u16; 256];
    // SAFETY: a plain query into a local buffer.
    let n = unsafe { GetClassNameW(hwnd, &mut name) };
    String::from_utf16_lossy(&name[..usize::try_from(n).unwrap_or(0)])
}

/// One synthesized click on a visible point of `hwnd`, never its title bar (Task 1's S1.4
/// blocker recipe), which gives its process the foreground. The window is made topmost for the
/// click, so nothing covers it, and is normal again after. Coordinates are physical: the test
/// is per-monitor DPI aware.
fn click(hwnd: isize) {
    let h = HWND(hwnd as *mut c_void);
    let raise = |after: HWND| {
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW;
        // SAFETY: a plain call on a live window of this process.
        unsafe { SetWindowPos(h, Some(after), 0, 0, 0, 0, flags) }.expect("SetWindowPos");
    };
    raise(HWND_TOPMOST);
    std::thread::sleep(Duration::from_millis(200));
    let mut rect = RECT::default();
    // SAFETY: plain queries; `rect` is a valid out-pointer.
    unsafe { GetWindowRect(h, &mut rect) }.expect("the blocker's rectangle");
    let root_at = |p: POINT| {
        // SAFETY: plain queries.
        unsafe { GetAncestor(WindowFromPoint(p), GA_ROOT) }
    };
    let top = rect.top + 48;
    let point = (1..=5)
        .flat_map(|j| (1..=5).map(move |i| (i, j)))
        .map(|(i, j)| POINT {
            x: rect.left + (rect.right - rect.left) * i / 6,
            y: top + (rect.bottom - top) * j / 6,
        })
        .find(|&p| root_at(p) == h)
        .unwrap_or_else(|| {
            let centre = POINT {
                x: (rect.left + rect.right) / 2,
                y: (rect.top + rect.bottom) / 2,
            };
            let there = root_at(centre);
            let mut pid = 0u32;
            // SAFETY: plain queries.
            let (visible, iconic) = unsafe {
                GetWindowThreadProcessId(there, Some(&mut pid));
                (IsWindowVisible(h).as_bool(), IsIconic(h).as_bool())
            };
            let class = class_of(there);
            // The lock screen's frame covers every window while the PC is locked.
            let locked = if class == "LockScreenBackstopFrame" {
                " The PC is locked: run the host set while it is unlocked."
            } else {
                ""
            };
            panic!(
                "no point of the blocker {rect:?} is visible (visible {visible}, minimized                  {iconic}); pid {pid}'s window of class {class} covers its centre.{locked}"
            )
        });
    // SAFETY: plain queries and one SendInput of two mouse inputs.
    unsafe {
        let (vx, vy) = (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
        );
        let (vw, vh) = (
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2),
            GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2),
        );
        let dx = (point.x - vx) * 65535 / (vw - 1);
        let dy = (point.y - vy) * 65535 / (vh - 1);
        let at = MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK;
        let input = |flags| INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: 0,
                    dwFlags: at | flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let inputs = [input(MOUSEEVENTF_LEFTDOWN), input(MOUSEEVENTF_LEFTUP)];
        assert_eq!(
            SendInput(&inputs, i32::try_from(size_of::<INPUT>()).unwrap()),
            2,
            "SendInput"
        );
    }
    std::thread::sleep(Duration::from_millis(200));
    raise(HWND_NOTOPMOST);
}

fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let until = Instant::now() + DEADLINE;
    while !ready() {
        assert!(Instant::now() < until, "{what} within {DEADLINE:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test] // spec: APP-013, APP-014, QA-042
#[ignore = "desktop"]
fn desktop_second_launch_comes_to_front() {
    require_desktop_group(true);
    // Physical coordinates for the blocker's click (WindowFromPoint and SendInput agree).
    // SAFETY: a plain call, before this process makes any window.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let env = TestEnv::new();
    let deploy = deployed_app().parent().unwrap().to_path_buf();
    let _core = CoreFixture::start_exe(&env, &deploy.join("lil-core.exe"));

    // Another window takes the front first: the blocker, raised, else clicked.
    let blocker = OwnerWindow::open(
        "LILITH desktop test",
        "A second launch of LILITH must bring its first window in front of this one.",
    )
    .expect("the blocker window");
    // Task 1's recipes in turn: SetForegroundWindow (with the input attached), then the
    // fallback recipe's SwitchToThisWindow, then a synthesized click on the blocker.
    let raised = blocker.bring_to_front();
    if raised == Raise::NotInFront && !blocker.wait_front(Duration::from_millis(500)) {
        // SAFETY: a plain call on a live window of this process.
        unsafe { SwitchToThisWindow(HWND(blocker.hwnd() as *mut c_void), true) };
        if !blocker.wait_front(Duration::from_millis(500)) {
            click(blocker.hwnd());
        }
    }
    assert!(
        blocker.wait_front(Duration::from_secs(5)),
        "the blocker holds the front before the app starts ({}; the front is pid {})",
        raised.name(),
        foreground_pid()
    );

    // A, the first instance, shown behind it without the focus.
    let first = Running(start_app(&env, &["--no-activate"]));
    let a = first.0.id();
    wait_until("the first instance's window", || app_window(a).is_some());
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        blocker.wait_front(Duration::from_secs(2)),
        "the blocker still holds the front (the front is pid {}, the app {a})",
        foreground_pid()
    );

    // B, started by the process in front, as a toast click starts it: it hands over and exits.
    let started = Instant::now();
    let mut second = start_app(&env, &[]);
    let code = loop {
        if let Some(status) = second.try_wait().unwrap() {
            break status.code();
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the second launch didn't exit"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let took = started.elapsed();
    assert_eq!(code, Some(0), "the second launch exits 0");
    // GetForegroundWindow is the first instance's window.
    wait_until("the first instance in front", || foreground_pid() == a);
    // SAFETY: a plain query.
    let front = unsafe { GetForegroundWindow() };
    assert_eq!(app_window(a), Some(front), "its own window is in front");
    eprintln!("the second launch exited after {took:?}; the first instance came to the front");
    assert!(
        app_window(second.id()).is_none(),
        "the second launch showed no window"
    );
    drop(blocker);
}
