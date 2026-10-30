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
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT, SendInput,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetSystemMetrics, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsWindowVisible, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
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

/// One synthesized click at the centre of `hwnd` (Task 1's S1.4 blocker recipe), which gives its
/// process the foreground.
fn click(hwnd: isize) {
    let mut rect = RECT::default();
    // SAFETY: plain queries; `rect` is a valid out-pointer.
    unsafe {
        GetWindowRect(HWND(hwnd as *mut c_void), &mut rect).expect("the blocker's rectangle");
        let (vx, vy) = (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
        );
        let (vw, vh) = (
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1),
            GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1),
        );
        let (x, y) = ((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2);
        let dx = (x - vx) * 65535 / vw;
        let dy = (y - vy) * 65535 / vh;
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
    let env = TestEnv::new();
    let deploy = deployed_app().parent().unwrap().to_path_buf();
    let _core = CoreFixture::start_exe(&env, &deploy.join("lil-core.exe"));

    // A, the first instance, with its window up.
    let first = Running(start_app(&env, &[]));
    let a = first.0.id();
    wait_until("the first instance's window", || app_window(a).is_some());

    // Another window takes the front: the blocker, raised, else clicked.
    let blocker = OwnerWindow::open(
        "LILITH desktop test",
        "A second launch of LILITH must bring its first window in front of this one.",
    )
    .expect("the blocker window");
    if blocker.bring_to_front() == Raise::NotInFront {
        click(blocker.hwnd());
    }
    assert!(
        blocker.wait_front(Duration::from_secs(5)),
        "the blocker holds the front before the second launch"
    );
    assert_ne!(foreground_pid(), a);

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
