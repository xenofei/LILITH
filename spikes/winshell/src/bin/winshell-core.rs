//! `winshell-core`: the windowless sender, started detached as `lil-client` starts the core.
//! Every command writes its result as JSON to `--out <file>` (there is no console).
//!
//! - `toast --tag T [--launch URL] [--button URL] [--suppress] [--title X] [--body Y]` (S1.2–S1.4)
//! - `progress [--secs 20] [--body B]`: the progress toast and its updates (S1.5; the script puts a nonce in B)
//! - `update [--seq N]`: one more update of the progress toast (S1.5)
//! - `remove-repost [--body B]`: `Remove(tag, group)`, then a new toast with that Tag (S1.5)
//! - `fullscreen [--secs 60] [--timing 100]`: the 2 s `SHQueryUserNotificationState` poll (S1.6)
//! - `power [--api request|ste|none] [--max-secs 7200] [--parent-pid P]`: the keep-awake request
//!   (S1.7); `none` holds nothing and only listens for power broadcasts (the sleep-and-wake check).
//!   With `--parent-pid` it clears the request and exits once that process has gone, so a closed
//!   console never leaves the PC held awake (automation review M1; `tests/core_parent.rs`)
#![windows_subsystem = "windows"]

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};
use windows::UI::Notifications::{
    NotificationData, NotificationUpdateResult, ToastNotificationManager,
};
use windows::Win32::Foundation::{
    CloseHandle, HANDLE, HWND, LPARAM, LRESULT, WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{
    ES_CONTINUOUS, ES_SYSTEM_REQUIRED, PowerClearRequest, PowerCreateRequest,
    PowerRequestSystemRequired, PowerSetRequest, SetThreadExecutionState,
};
use windows::Win32::System::Threading::{
    OpenProcess, POWER_REQUEST_CONTEXT_SIMPLE_STRING, PROCESS_SYNCHRONIZE, REASON_CONTEXT,
    REASON_CONTEXT_0, WaitForSingleObject,
};
use windows::Win32::System::WindowsProgramming::QueryUnbiasedInterruptTime;
use windows::Win32::UI::Shell::{
    QUERY_USER_NOTIFICATION_STATE, QUNS_ACCEPTS_NOTIFICATIONS, QUNS_APP, QUNS_BUSY,
    QUNS_NOT_PRESENT, QUNS_PRESENTATION_MODE, QUNS_QUIET_TIME, QUNS_RUNNING_D3D_FULL_SCREEN,
    SHQueryUserNotificationState,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, KillTimer, MSG,
    PostQuitMessage, RegisterClassW, SetTimer, TranslateMessage, WINDOW_EX_STYLE, WM_DESTROY,
    WM_POWERBROADCAST, WM_TIMER, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};
use windows::core::{HSTRING, PWSTR, w};
use winshell::args::Args;
use winshell::text::{ToastSpec, summary, toast_xml};
use winshell::win::{self, log_dir, make_toast, notifier, notifier_setting, now_ms, write_json};
use winshell::{PROGRESS_GROUP, PROGRESS_TAG};

fn main() {
    win::init_winrt();
    let args = Args::parse(std::env::args().skip(1));
    let out = PathBuf::from(args.get_or("out", "core-result.json"));
    let mut result = match args.command.as_deref() {
        Some("toast") => toast(&args),
        Some("progress") => progress(&args),
        Some("update") => update(&args),
        Some("remove-repost") => remove_repost(&args),
        Some("fullscreen") => fullscreen(&args),
        Some("power") => power(&args, &out),
        other => json!({ "error": format!("unknown command {other:?}") }),
    };
    // A sender that exits the instant `Show` returns is not what the core does; stay a moment.
    if matches!(
        args.command.as_deref(),
        Some("toast" | "progress" | "remove-repost")
    ) {
        thread::sleep(Duration::from_secs(1));
    }
    result["pid"] = json!(std::process::id());
    result["finished_ms"] = json!(now_ms() as u64);
    write_json(&out, &result);
}

fn err(e: windows::core::Error) -> Value {
    json!(format!("{e}"))
}

// ---- toasts (S1.2, S1.3, S1.4, S1.5's quiet toast) ----

fn toast(args: &Args) -> Value {
    let tag = args.get_or("tag", "spike");
    let mut actions = Vec::new();
    if let Some(url) = args.get("button").filter(|u| !u.is_empty()) {
        actions.push(("Open", url));
    }
    let title = args.get_or("title", "LILITH spike");
    let body = args.get_or("body", tag);
    let xml = toast_xml(&ToastSpec {
        launch: args.get("launch").filter(|u| !u.is_empty()),
        title,
        body,
        actions,
        progress: false,
    });
    let suppress = args.has("suppress");
    let (posted, events) = show_watched(&xml, tag, "spike", suppress);
    let watch = watch_events(args, events.as_ref());
    json!({
        "command": "toast", "tag": tag, "suppress": suppress, "xml": xml,
        "setting": notifier_setting(),
        "posted": posted.is_ok(), "error": posted.err().map(err),
        "shown_ms": now_ms() as u64, "toast_events": watch,
    })
}

/// Makes the toast, registers its event handlers (`win::watch_toast`) and shows it.
fn show_watched(
    xml: &str,
    tag: &str,
    group: &str,
    suppress: bool,
) -> (windows::core::Result<()>, Option<win::ToastEvents>) {
    match make_toast(xml, tag, group, suppress) {
        Ok(t) => {
            let events = win::watch_toast(&t).ok();
            (notifier().and_then(|n| n.Show(&t)), events)
        }
        Err(e) => (Err(e), None),
    }
}

/// With `--watch-secs N`, stays N s and returns the toast's Dismissed, Failed and Activated
/// events; without it, `null` (the toast is shown and left).
fn watch_events(args: &Args, events: Option<&win::ToastEvents>) -> Value {
    let secs = args.num("watch-secs", 0);
    let Some(events) = events else {
        return Value::Null;
    };
    if secs == 0 {
        return Value::Null;
    }
    thread::sleep(Duration::from_secs(secs));
    json!(win::toast_events(events))
}

fn update_name(r: NotificationUpdateResult) -> &'static str {
    match r {
        NotificationUpdateResult::Succeeded => "Succeeded",
        NotificationUpdateResult::Failed => "Failed",
        NotificationUpdateResult::NotificationNotFound => "NotificationNotFound",
        _ => "Unknown",
    }
}

fn progress_data(seq: u32, value: f64) -> windows::core::Result<NotificationData> {
    let data = NotificationData::new()?;
    let values = data.Values()?;
    values.Insert(
        &HSTRING::from("progressValue"),
        &HSTRING::from(format!("{value:.2}")),
    )?;
    values.Insert(
        &HSTRING::from("progressValueString"),
        &HSTRING::from(format!("{} %", (value * 100.0).round() as u32)),
    )?;
    values.Insert(&HSTRING::from("progressStatus"), &HSTRING::from("sampling"))?;
    data.SetSequenceNumber(seq)?;
    Ok(data)
}

fn post_progress(body: &str) -> (windows::core::Result<()>, Option<win::ToastEvents>) {
    let xml = toast_xml(&ToastSpec {
        title: "Spike job running",
        body,
        progress: true,
        ..Default::default()
    });
    let toast = match make_toast(&xml, PROGRESS_TAG, PROGRESS_GROUP, false) {
        Ok(t) => t,
        Err(e) => return (Err(e), None),
    };
    let events = win::watch_toast(&toast).ok();
    let shown = progress_data(1, 0.0)
        .and_then(|d| toast.SetData(&d))
        .and_then(|_| notifier()?.Show(&toast));
    (shown, events)
}

fn do_update(seq: u32, value: f64) -> String {
    match notifier().and_then(|n| {
        n.UpdateWithTagAndGroup(
            &progress_data(seq, value)?,
            &HSTRING::from(PROGRESS_TAG),
            &HSTRING::from(PROGRESS_GROUP),
        )
    }) {
        Ok(r) => update_name(r).to_string(),
        Err(e) => format!("error: {e}"),
    }
}

fn progress(args: &Args) -> Value {
    let secs = args.num("secs", 20) as u32;
    let (posted, events) = post_progress(args.get_or("body", "Progress toast (S1.5)"));
    let mut updates = Vec::new();
    if posted.is_ok() {
        for i in 1..=secs {
            thread::sleep(Duration::from_secs(1));
            let r = do_update(i + 1, f64::from(i) / f64::from(secs));
            updates.push(json!({ "seq": i + 1, "t_ms": now_ms() as u64, "result": r }));
        }
    }
    let all_succeeded = !updates.is_empty() && updates.iter().all(|u| u["result"] == "Succeeded");
    // The events since the post: one TimedOut per time the banner faded out. `--watch-secs`
    // adds a wait after the last update, so a re-pop late in the updates is still seen.
    let watch = watch_events(args, events.as_ref());
    json!({
        "command": "progress", "setting": notifier_setting(),
        "posted": posted.is_ok(), "error": posted.err().map(err),
        "updates": updates, "all_succeeded": all_succeeded, "toast_events": watch,
    })
}

fn update(args: &Args) -> Value {
    let seq = args.num("seq", 100) as u32;
    json!({ "command": "update", "seq": seq, "result": do_update(seq, 0.5) })
}

fn remove_repost(args: &Args) -> Value {
    let removed = ToastNotificationManager::History().and_then(|h| {
        h.RemoveGroupedTagWithId(
            &HSTRING::from(PROGRESS_TAG),
            &HSTRING::from(PROGRESS_GROUP),
            &HSTRING::from(winshell::AUMID),
        )
    });
    let xml = toast_xml(&ToastSpec {
        title: "Spike job finished",
        body: args.get_or("body", "Posted after Remove(tag, group) (S1.5)"),
        ..Default::default()
    });
    let (posted, events) = show_watched(&xml, PROGRESS_TAG, PROGRESS_GROUP, false);
    let watch = watch_events(args, events.as_ref());
    json!({
        "command": "remove-repost",
        "removed": removed.is_ok(), "remove_error": removed.err().map(err),
        "posted": posted.is_ok(), "post_error": posted.err().map(err),
        "toast_events": watch,
    })
}

// ---- the fullscreen poll (S1.6) ----

fn quns_name(s: QUERY_USER_NOTIFICATION_STATE) -> &'static str {
    match s {
        QUNS_NOT_PRESENT => "QUNS_NOT_PRESENT",
        QUNS_BUSY => "QUNS_BUSY",
        QUNS_RUNNING_D3D_FULL_SCREEN => "QUNS_RUNNING_D3D_FULL_SCREEN",
        QUNS_PRESENTATION_MODE => "QUNS_PRESENTATION_MODE",
        QUNS_ACCEPTS_NOTIFICATIONS => "QUNS_ACCEPTS_NOTIFICATIONS",
        QUNS_QUIET_TIME => "QUNS_QUIET_TIME",
        QUNS_APP => "QUNS_APP",
        _ => "unknown",
    }
}

fn query_state() -> String {
    // SAFETY: plain query.
    match unsafe { SHQueryUserNotificationState() } {
        Ok(s) => quns_name(s).to_string(),
        Err(e) => format!("error: {e}"),
    }
}

fn fullscreen(args: &Args) -> Value {
    let secs = args.num("secs", 60);
    let timing_n = args.num("timing", 0);
    let mut call_ms = Vec::new();
    for _ in 0..timing_n {
        let t = Instant::now();
        let _ = query_state();
        call_ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let start = Instant::now();
    let mut samples = Vec::new();
    while start.elapsed() < Duration::from_secs(secs) {
        samples.push(json!({
            "t_s": (start.elapsed().as_secs_f64() * 10.0).round() / 10.0,
            "state": query_state(),
        }));
        thread::sleep(Duration::from_secs(2));
    }
    json!({
        "command": "fullscreen", "interval_s": 2, "secs": secs,
        "samples": samples,
        "call_ms": if timing_n > 0 { summary(&call_ms) } else { Value::Null },
        "call_ms_raw": call_ms,
    })
}

// ---- keep-awake (S1.7) ----

struct PowerState {
    events: Vec<Value>,
    out: PathBuf,
    base: Value,
    cleared: bool,
    last_wall: SystemTime,
    last_unbiased: u64,
    last_instant: Instant,
}

static POWER: Mutex<Option<PowerState>> = Mutex::new(None);
static CLEAR_NOW: AtomicBool = AtomicBool::new(false);
/// A SYNCHRONIZE handle on the parent process (`--parent-pid`), or 0. Held from the start, so
/// a reused pid can't stand in for the parent.
static PARENT: AtomicIsize = AtomicIsize::new(0);
const TIMER_FLAGS: usize = 1;
const TIMER_CLOCK: usize = 2;

fn unbiased_100ns() -> u64 {
    let mut t = 0u64;
    // SAFETY: plain query into a local.
    let _ = unsafe { QueryUnbiasedInterruptTime(&mut t) };
    t
}

fn power_event(kind: &str, extra: Value) {
    if let Ok(mut guard) = POWER.lock()
        && let Some(st) = guard.as_mut()
    {
        let mut e = json!({ "t_ms": now_ms() as u64, "kind": kind });
        if let (Some(o), Value::Object(x)) = (e.as_object_mut(), extra) {
            o.extend(x);
        }
        st.events.push(e);
        let mut doc = st.base.clone();
        doc["events"] = json!(st.events);
        doc["cleared"] = json!(st.cleared);
        write_json(&st.out, &doc);
    }
}

fn pbt_name(w: usize) -> String {
    match w {
        0x4 => "PBT_APMSUSPEND".into(),
        0x7 => "PBT_APMRESUMESUSPEND".into(),
        0xA => "PBT_APMPOWERSTATUSCHANGE".into(),
        0x12 => "PBT_APMRESUMEAUTOMATIC".into(),
        0x8013 => "PBT_POWERSETTINGCHANGE".into(),
        other => format!("0x{other:x}"),
    }
}

extern "system" fn power_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_POWERBROADCAST => {
            power_event("power_broadcast", json!({ "pbt": pbt_name(wp.0) }));
            LRESULT(1)
        }
        WM_TIMER if wp.0 == TIMER_FLAGS => {
            let dir = log_dir();
            if dir.join("power-clear.flag").exists() && !CLEAR_NOW.swap(true, Ordering::SeqCst) {
                power_event("clear_flag_seen", json!({}));
            }
            let parent = PARENT.load(Ordering::SeqCst);
            // SAFETY: a handle this process opened and keeps open until exit.
            if parent != 0
                && unsafe { WaitForSingleObject(HANDLE(parent as *mut _), 0) } == WAIT_OBJECT_0
            {
                power_event("parent_gone", json!({}));
                // SAFETY: our own window and timer.
                unsafe {
                    let _ = KillTimer(Some(hwnd), TIMER_FLAGS);
                    PostQuitMessage(0);
                }
                return LRESULT(0);
            }
            if dir.join("power-quit.flag").exists() {
                // SAFETY: our own window and timer.
                unsafe {
                    let _ = KillTimer(Some(hwnd), TIMER_FLAGS);
                    PostQuitMessage(0);
                }
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_CLOCK => {
            let mut sample = None;
            if let Ok(mut guard) = POWER.lock()
                && let Some(st) = guard.as_mut()
            {
                let wall = SystemTime::now()
                    .duration_since(st.last_wall)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(-1.0);
                let unbiased_now = unbiased_100ns();
                let unbiased = (unbiased_now - st.last_unbiased) as f64 / 1e7;
                let instant = st.last_instant.elapsed().as_secs_f64();
                st.last_wall = SystemTime::now();
                st.last_unbiased = unbiased_now;
                st.last_instant = Instant::now();
                sample = Some(json!({
                    "wall_s": wall, "unbiased_s": unbiased, "instant_s": instant,
                    "sleep_suspected": wall - unbiased > 60.0,
                }));
            }
            if let Some(s) = sample {
                if s["sleep_suspected"] == true {
                    power_event("clock_jump", s);
                } else if let Ok(mut guard) = POWER.lock()
                    && let Some(st) = guard.as_mut()
                {
                    // Plain ticks are kept in memory only; the last one is written with
                    // the next event.
                    st.base["last_tick"] = s;
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: posts to our own thread's queue.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: default handling for every other message.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// Holds the request until `power-clear.flag` appears in the log folder, then clears it and
/// keeps listening for power broadcasts until `power-quit.flag` or `--max-secs`.
fn power(args: &Args, out: &Path) -> Value {
    let api = args.get_or("api", "request").to_string();
    let max_secs = args.num("max-secs", 7200);
    let dir = log_dir();
    let _ = std::fs::remove_file(dir.join("power-clear.flag"));
    let _ = std::fs::remove_file(dir.join("power-quit.flag"));
    let base = json!({ "command": "power", "api": api, "reason": winshell::POWER_REASON });
    if let Some(pid) = args.get("parent-pid").and_then(|p| p.parse::<u32>().ok()) {
        // SAFETY: plain call; the handle is closed before exit.
        match unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
            Ok(h) => PARENT.store(h.0 as isize, Ordering::SeqCst),
            Err(e) => {
                return json!({ "command": "power", "api": api, "error": format!("parent process {pid} not found: {e}") });
            }
        }
    }
    *POWER.lock().expect("power lock") = Some(PowerState {
        events: Vec::new(),
        out: out.to_path_buf(),
        base,
        cleared: false,
        last_wall: SystemTime::now(),
        last_unbiased: unbiased_100ns(),
        last_instant: Instant::now(),
    });

    // The hidden top-level window: never shown, but it receives WM_POWERBROADCAST
    // (a message-only window would not).
    // SAFETY: standard window creation on this thread; the class and title are static.
    let hwnd = unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let class = WNDCLASSW {
            lpfnWndProc: Some(power_wndproc),
            hInstance: instance.into(),
            lpszClassName: w!("winshell-core-power"),
            ..Default::default()
        };
        RegisterClassW(&class);
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("winshell-core-power"),
            w!("winshell-core power"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            100,
            100,
            None,
            None,
            Some(instance.into()),
            None,
        )
    };
    let hwnd = match hwnd {
        Ok(h) => h,
        Err(e) => return json!({ "command": "power", "error": err(e) }),
    };
    power_event("window", json!({ "hwnd": hwnd.0 as isize }));

    // Hold the request: the PowerCreateRequest object, or SetThreadExecutionState from a
    // dedicated thread (the FAIL path).
    let mut reason: Vec<u16> = winshell::POWER_REASON.encode_utf16().chain([0]).collect();
    let mut request = HANDLE::default();
    let (ste_tx, ste_rx) = mpsc::channel::<()>();
    let mut ste_thread = None;
    if api == "none" {
        power_event("set", json!({ "api": "none", "ok": true }));
    } else if api == "ste" {
        let (ack_tx, ack_rx) = mpsc::channel::<u32>();
        ste_thread = Some(thread::spawn(move || {
            // SAFETY: plain calls on this thread; it lives until told to clear.
            let prev = unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) };
            let _ = ack_tx.send(prev.0);
            let _ = ste_rx.recv();
            // SAFETY: as above.
            unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
        }));
        let prev = ack_rx.recv().unwrap_or(0);
        power_event(
            "set",
            json!({ "api": "ste", "ok": prev != 0, "previous_state": prev }),
        );
    } else {
        let context = REASON_CONTEXT {
            Version: 0, // POWER_REQUEST_CONTEXT_VERSION
            Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
            Reason: REASON_CONTEXT_0 {
                SimpleReasonString: PWSTR(reason.as_mut_ptr()),
            },
        };
        // SAFETY: the context and its string outlive the call.
        match unsafe { PowerCreateRequest(&context) } {
            Ok(h) => {
                request = h;
                // SAFETY: a request handle we own.
                let set = unsafe { PowerSetRequest(request, PowerRequestSystemRequired) };
                power_event(
                    "set",
                    json!({ "api": "request", "ok": set.is_ok(), "error": set.err().map(err) }),
                );
            }
            Err(e) => power_event(
                "set",
                json!({ "api": "request", "ok": false, "error": err(e) }),
            ),
        }
    }

    // SAFETY: timers on our own window.
    unsafe {
        SetTimer(Some(hwnd), TIMER_FLAGS, 500, None);
        SetTimer(Some(hwnd), TIMER_CLOCK, 30_000, None);
    }
    let started = Instant::now();
    let mut msg = MSG::default();
    let mut cleared = false;
    // SAFETY: the standard message loop on this thread.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            if !cleared && CLEAR_NOW.load(Ordering::SeqCst) {
                cleared = true;
                clear(&api, request, &ste_tx);
            }
            if started.elapsed() > Duration::from_secs(max_secs) {
                power_event("max_secs", json!({}));
                break;
            }
        }
    }
    if !cleared {
        clear(&api, request, &ste_tx);
    }
    if let Some(t) = ste_thread {
        let _ = t.join();
    }
    let parent = PARENT.swap(0, Ordering::SeqCst);
    if parent != 0 {
        // SAFETY: the handle opened above, closed once.
        unsafe {
            let _ = CloseHandle(HANDLE(parent as *mut _));
        }
    }
    power_event("exit", json!({}));
    let guard = POWER.lock().expect("power lock");
    let st = guard.as_ref().expect("power state");
    let mut doc = st.base.clone();
    doc["events"] = json!(st.events);
    doc["cleared"] = json!(st.cleared);
    doc
}

fn clear(api: &str, request: HANDLE, ste_tx: &mpsc::Sender<()>) {
    let result = if api == "none" {
        json!({ "api": "none", "ok": true })
    } else if api == "ste" {
        json!({ "api": "ste", "ok": ste_tx.send(()).is_ok() })
    } else {
        // SAFETY: a request handle we own (a null handle just fails).
        let r = unsafe { PowerClearRequest(request, PowerRequestSystemRequired) };
        json!({ "api": "request", "ok": r.is_ok(), "error": r.err().map(err) })
    };
    if let Ok(mut guard) = POWER.lock()
        && let Some(st) = guard.as_mut()
    {
        st.cleared = true;
    }
    power_event("cleared", result);
}
