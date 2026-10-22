//! `winshell-app`: a tiny window with a single-instance pipe (S1.3, S1.4).
//!
//! Every start logs `start` with its raw `GetCommandLineW` to `<log dir>\events.jsonl`.
//!
//! - `winshell-app.exe "<url>"` (how the `lilspike` handler starts it): when an instance is
//!   already serving the pipe, this is B: it reads A's `{"app", "pid"}`, checks that A's image
//!   is its own exe, calls `AllowSetForegroundWindow(A)`, writes `{"args": [<url>]}`, reads
//!   `{"ok": true}` and exits 0. Otherwise it becomes A itself.
//! - `winshell-app.exe [--recipe basic|fallback] [--exit-after-secs N]`: A, with its window.
//!   On each hand-off A calls `SetForegroundWindow`; with `fallback` it first calls
//!   `AllowSetForegroundWindow` for itself and, if that fails, `SwitchToThisWindow`.
//! - `winshell-app.exe --blocker [--launch-url U --launch-after-ms N]`: another window that
//!   takes the front and serves no pipe; with `--launch-url` it then `ShellExecuteW`s the URL
//!   itself, as the foreground process (the Sandbox's foreground item).
//!
//! A window logs `front_check` 1 s after it asks for the front: whether
//! `GetForegroundWindow()` is it and whether it has keyboard focus.
#![windows_subsystem = "windows"]

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use windows::Win32::Foundation::{ERROR_PIPE_CONNECTED, HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{COLOR_WINDOW, HBRUSH};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeServerProcessId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DispatchMessageW,
    GetMessageW, KillTimer, MSG, PostMessageW, PostQuitMessage, RegisterClassW, SW_SHOW,
    SetForegroundWindow, SetTimer, ShowWindow, SwitchToThisWindow, TranslateMessage, WM_APP,
    WM_DESTROY, WM_TIMER, WNDCLASSW, WS_EX_LEFT, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};
use windows::core::{HSTRING, w};
use winshell::PIPE_NAME;
use winshell::args::Args;
use winshell::text::{split_first_arg, strip_one_quote_pair};
use winshell::win::{foreground_info, log_event, process_image, raw_command_line, shell_execute};

const WM_HANDOFF: u32 = WM_APP + 1;
const TIMER_CHECK: usize = 1;
const TIMER_EXIT: usize = 2;
const TIMER_LAUNCH: usize = 3;

static MAIN: AtomicIsize = AtomicIsize::new(0);
static FALLBACK: AtomicBool = AtomicBool::new(false);
static PHASE: Mutex<String> = Mutex::new(String::new());
static PENDING: Mutex<Vec<String>> = Mutex::new(Vec::new());
static LAUNCH_URL: Mutex<String> = Mutex::new(String::new());

fn main() {
    let cmdline = raw_command_line();
    let (_exe, rest) = split_first_arg(&cmdline);
    let rest = rest.to_string();
    log_event("start", json!({ "cmdline": cmdline, "rest": rest }));

    // A URL argument (anything not starting with `--`) is a hand-off candidate.
    let url = if rest.is_empty() || rest.starts_with("--") {
        None
    } else {
        Some(strip_one_quote_pair(&rest).to_string())
    };
    let args = if url.is_some() {
        Args::default()
    } else {
        Args::parse(std::env::args().skip(1))
    };

    if args.has("blocker") {
        *LAUNCH_URL.lock().expect("lock") = args.get_or("launch-url", "").to_string();
        run_window(
            "winshell blocker",
            "blocker",
            args.num("exit-after-secs", 300),
            args.num("launch-after-ms", 2000),
        );
        return;
    }

    if let Some(u) = &url {
        match hand_off(u) {
            Ok(v) => {
                log_event("handoff", v);
                return;
            }
            Err(why) => log_event("no_instance", json!({ "why": why })),
        }
    }

    FALLBACK.store(
        args.get_or("recipe", "basic") == "fallback",
        Ordering::SeqCst,
    );
    if !start_pipe_server() {
        log_event("pipe_taken", json!({}));
    }
    if let Some(u) = url {
        log_event("argument", json!({ "arg": u, "as": "own window" }));
    }
    run_window(
        &format!("winshell-app {}", std::process::id()),
        "start",
        args.num("exit-after-secs", 300),
        0,
    );
}

// ---- B: the second launch ----

fn hand_off(url: &str) -> Result<Value, String> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let file = loop {
        match OpenOptions::new().read(true).write(true).open(PIPE_NAME) {
            Ok(f) => break f,
            // ERROR_PIPE_BUSY: another client is being served; retry within 2 s.
            Err(e) if e.raw_os_error() == Some(231) && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("open: {e}")),
        }
    };
    let mut server_pid = 0u32;
    // SAFETY: the handle belongs to `file`, which outlives the call.
    unsafe {
        GetNamedPipeServerProcessId(HANDLE(file.as_raw_handle()), &mut server_pid)
            .map_err(|e| format!("GetNamedPipeServerProcessId: {e}"))?;
    }
    let mut reader = BufReader::new(&file);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("read hello: {e}"))?;
    let hello: Value = serde_json::from_str(line.trim()).map_err(|e| format!("hello: {e}"))?;
    let pid = hello["pid"].as_u64().unwrap_or(0) as u32;
    let own = std::env::current_exe().ok();
    let theirs = process_image(server_pid);
    let same_image = match (&own, &theirs) {
        (Some(a), Some(b)) => a.as_os_str().eq_ignore_ascii_case(b.as_os_str()),
        _ => false,
    };
    if pid != server_pid || !same_image {
        return Err(format!(
            "server check failed: hello pid {pid}, server pid {server_pid}, same image {same_image}"
        ));
    }
    // SAFETY: plain call.
    let allowed = unsafe { AllowSetForegroundWindow(pid) };
    (&file)
        .write_all(format!("{}\n", json!({ "args": [url] })).as_bytes())
        .map_err(|e| format!("write args: {e}"))?;
    line.clear();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("read ok: {e}"))?;
    Ok(json!({
        "to_pid": pid, "allow_set_foreground": allowed.is_ok(),
        "allow_error": allowed.err().map(|e| e.to_string()), "reply": line.trim(),
    }))
}

// ---- A: the pipe server ----

/// Creates the first pipe instance (fails if another A serves it), then serves clients on a
/// thread. Each client gets `{"app", "pid"}`, sends `{"args": [...]}`, gets `{"ok": true}`.
fn start_pipe_server() -> bool {
    let first = match create_instance(true) {
        Some(h) => h,
        None => return false,
    };
    thread::spawn(move || {
        let mut next = Some(first);
        loop {
            let file = match next.take().or_else(|| create_instance(false)) {
                Some(f) => f,
                None => return,
            };
            // SAFETY: a pipe handle owned by `file`; a client that connected first gives
            // ERROR_PIPE_CONNECTED, which is success.
            let connected = match unsafe { ConnectNamedPipe(HANDLE(file.as_raw_handle()), None) } {
                Ok(()) => true,
                Err(e) => e.code() == ERROR_PIPE_CONNECTED.to_hresult(),
            };
            if connected {
                // The next instance exists before this one closes, so a second launch never
                // finds no pipe while A runs.
                next = create_instance(false);
                serve(&file);
            }
        }
    });
    true
}

/// One pipe instance, owned by a `File` (which closes it).
fn create_instance(first: bool) -> Option<File> {
    let mut mode = PIPE_ACCESS_DUPLEX;
    if first {
        mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    // SAFETY: the name is NUL-terminated and outlives the call.
    let h = unsafe {
        CreateNamedPipeW(
            &HSTRING::from(PIPE_NAME),
            mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            65536,
            65536,
            0,
            None,
        )
    };
    if h.is_invalid() {
        None
    } else {
        // SAFETY: a fresh handle owned by nothing else.
        Some(unsafe { File::from_raw_handle(h.0) })
    }
}

fn serve(file: &File) {
    let hello = json!({ "app": "winshell-app", "pid": std::process::id() });
    if (&*file).write_all(format!("{hello}\n").as_bytes()).is_err() {
        return;
    }
    let mut line = String::new();
    if BufReader::new(file).read_line(&mut line).is_err() {
        return;
    }
    let msg: Value = serde_json::from_str(line.trim()).unwrap_or(Value::Null);
    let args: Vec<String> = msg["args"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let _ = (&*file).write_all(b"{\"ok\":true}\n");
    PENDING.lock().expect("lock").extend(args);
    let hwnd = HWND(MAIN.load(Ordering::SeqCst) as *mut _);
    // SAFETY: posts to our own window (a stale handle just fails).
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_HANDOFF, WPARAM(0), LPARAM(0));
    }
}

// ---- the window ----

fn ask_front(hwnd: HWND, phase: &str) {
    let fallback = FALLBACK.load(Ordering::SeqCst);
    // SAFETY: calls on our own window.
    let (asfw, sfw, switched) = unsafe {
        let asfw = if fallback {
            Some(AllowSetForegroundWindow(GetCurrentProcessId()).is_ok())
        } else {
            None
        };
        let sfw = SetForegroundWindow(hwnd).as_bool();
        let switched = if fallback && !sfw {
            SwitchToThisWindow(hwnd, true);
            true
        } else {
            false
        };
        SetTimer(Some(hwnd), TIMER_CHECK, 1000, None);
        (asfw, sfw, switched)
    };
    *PHASE.lock().expect("lock") = phase.to_string();
    log_event(
        "ask_front",
        json!({
            "phase": phase, "hwnd": hwnd.0 as isize, "fallback": fallback,
            "allow_self": asfw, "set_foreground": sfw, "switch_to_this_window": switched,
        }),
    );
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_HANDOFF => {
            let args: Vec<String> = std::mem::take(&mut *PENDING.lock().expect("lock"));
            log_event("activate", json!({ "args": args }));
            ask_front(hwnd, "activate");
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_CHECK => {
            // SAFETY: our own timer.
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_CHECK);
            }
            let fg = foreground_info();
            let me = hwnd.0 as isize;
            log_event(
                "front_check",
                json!({
                    "phase": *PHASE.lock().expect("lock"),
                    "hwnd": me,
                    "is_foreground": fg["fg_hwnd"].as_i64() == Some(me as i64),
                    "has_focus": fg["focus_hwnd"].as_i64() == Some(me as i64),
                    "foreground": fg,
                }),
            );
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_LAUNCH => {
            // SAFETY: our own timer.
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_LAUNCH);
            }
            let url = LAUNCH_URL.lock().expect("lock").clone();
            let code = shell_execute(&url);
            log_event(
                "blocker_launched",
                json!({ "url": url, "shell_execute": code, "foreground": foreground_info() }),
            );
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_EXIT => {
            // SAFETY: posts to our own thread's queue.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: as above.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: default handling for every other message.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn run_window(title: &str, phase: &str, exit_after_secs: u64, launch_after_ms: u64) {
    // SAFETY: standard window creation and message loop on this thread.
    unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: w!("winshell-app"),
            hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as usize as *mut _),
            ..Default::default()
        };
        RegisterClassW(&class);
        let hwnd = match CreateWindowExW(
            WS_EX_LEFT,
            w!("winshell-app"),
            &HSTRING::from(title),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            480,
            240,
            None,
            None,
            Some(instance.into()),
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                log_event("window_error", json!({ "error": e.to_string() }));
                return;
            }
        };
        MAIN.store(hwnd.0 as isize, Ordering::SeqCst);
        let _ = ShowWindow(hwnd, SW_SHOW);
        log_event(
            "window",
            json!({ "hwnd": hwnd.0 as isize, "title": title, "role": phase }),
        );
        ask_front(hwnd, phase);
        SetTimer(
            Some(hwnd),
            TIMER_EXIT,
            (exit_after_secs.max(1) * 1000) as u32,
            None,
        );
        if !LAUNCH_URL.lock().expect("lock").is_empty() {
            SetTimer(
                Some(hwnd),
                TIMER_LAUNCH,
                launch_after_ms.max(1) as u32,
                None,
            );
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    log_event("exit", json!({}));
}
