//! `hello-probe`: plan 4a Task 28's route step (Step 0, P4a-R38).
//!
//! Question: owned by a real window of this process that is brought to the front, does the
//! Windows Hello prompt come to the front of every other application, a fullscreen one
//! included, within 2 s and with no input since the call? Asked for the KEK route's calls
//! (`RequestCreateForWindowAsync`, `RequestSignForWindowAsync`, bound by hand in
//! `lil_platform::hello`) and for the DPAPI route's gate (`RequestVerificationForWindowAsync`).
//!
//! Modes:
//!
//! - `dry` (no window, no prompt, nothing on screen): the Windows build, whether Windows Hello
//!   is set up, the TPM, whether the window-owned bindings resolve on this build, that the
//!   probe's throwaway credential doesn't exist, a DPAPI round trip, the fullscreen state and
//!   the foreground lock timeout. Anyone may run it.
//! - `run` (the owner's check; only at a real terminal, after Enter): the hands-off pop-up, then
//!   **phase 1**, hands off: four prompts, each owned by the probe's window while another
//!   application (a test window) is in front, first an ordinary window and then a fullscreen
//!   one: create, create, consent, consent. Each is cancelled by the probe as soon as its
//!   placement is read (at most 6 s), so nothing is created (unless Windows Hello Face confirms
//!   one by itself first). Then **phase 2**: three prompts the owner confirms (face, fingerprint
//!   or PIN) once each is in front, each after its own 10 s warning: create the throwaway
//!   credential, sign with it with the ordinary window in front, sign with the fullscreen window
//!   in front. Then the credential is deleted and checked gone. Every 150 ms of every call,
//!   `GetForegroundWindow` and the last-input time are recorded.
//!
//!   The credential is removed on every way out: a leftover from an earlier run first, then at
//!   the end, on an error or a panic (a drop guard), and on Ctrl+C, Ctrl+Break or a closed
//!   terminal (a console handler). If that removal ever fails, the run prints the `cleanup`
//!   command.
//! - `cleanup` (no prompt): deletes the throwaway credential, if a stopped run left it.
//! - `blocker [--fullscreen]` and `owner --op create|sign|consent --mode placement|gesture`: the
//!   two child processes `run` starts. The owner child is started by this background process,
//!   as `lil` is by an agent's shell, so Windows gives it no special right to the foreground.
//!
//! A step passes when the prompt (`Credential Dialog Xaml Host`, or a window of
//! `CredentialUIBroker.exe`) is the foreground window within 2 s of the call and no input
//! (keyboard or mouse, a person's or synthetic) happened from the call until then. The test
//! window takes the front before the call by any means it has, a synthetic Alt tap included
//! when Windows refuses it otherwise; that is before the call, so it is allowed, and it is
//! recorded.
//!
//! Results go to `--out <file>` (default `%TEMP%\lilith-hello-probe\results-<unix>.json`), with
//! the route the evidence gives: KEK when every create and sign step passed, the signatures
//! matched and the delete worked; else DPAPI with the Windows Hello gate when both consent
//! steps passed; else passphrase only.
//!
//! The throwaway credential is [`CREDENTIAL`], never the reviewer's. Nothing else is written
//! but the results file.

use std::io::{BufRead, BufReader, IsTerminal, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lil_platform::hello::{self, OwnerWindow};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The probe's throwaway key credential.
const CREDENTIAL: &str = "LILITH-hello-probe";
/// The fixed 32 bytes the probe signs.
const CHALLENGE: [u8; 32] = *b"LILITH hello probe challenge v1!";
/// The bar: the prompt in front within this long after the call.
const IN_FRONT_WITHIN_MS: u64 = 2000;
/// How often the foreground and the last input are read.
const SAMPLE_MS: u64 = 150;
/// A placement step cancels its prompt by this time at the latest.
const PLACEMENT_MAX_MS: u64 = 6000;
/// A gesture step waits this long for the owner at most.
const GESTURE_MAX_MS: u64 = 90_000;
/// The test window must hold the front by this time, or the step is invalid.
const BLOCKER_FRONT_MS: u64 = 3000;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |f: &str| args.iter().any(|a| a == f);
    let value = |f: &str| {
        args.iter()
            .position(|a| a == f)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let code = match args.first().map(String::as_str) {
        Some("dry") => {
            let out = out_path(value("--out"));
            let record = dry();
            write_results(&out, &record);
            println!(
                "{}",
                serde_json::to_string_pretty(&record).unwrap_or_default()
            );
            println!("Results: {}", out.display());
            0
        }
        Some("run") => run(out_path(value("--out"))),
        Some("cleanup") => match hello::kek_remove(CREDENTIAL) {
            Ok(()) => {
                println!("The probe credential {CREDENTIAL} is gone (or never existed).");
                0
            }
            Err(e) => {
                eprintln!("couldn't remove {CREDENTIAL}: {e}");
                1
            }
        },
        Some("blocker") => blocker(has("--fullscreen")),
        Some("owner") => owner(
            value("--op").as_deref().unwrap_or(""),
            value("--mode").as_deref() == Some("gesture"),
        ),
        _ => {
            eprintln!("usage: hello-probe dry | run | cleanup [--out <file>]");
            2
        }
    };
    std::process::exit(code);
}

fn out_path(given: Option<String>) -> PathBuf {
    given.map(PathBuf::from).unwrap_or_else(|| {
        std::env::temp_dir()
            .join("lilith-hello-probe")
            .join(format!("results-{}.json", unix_secs()))
    })
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn utc_now() -> String {
    // Seconds since the epoch, as text: the records only need ordering and a rough time.
    format!("unix:{}", unix_secs())
}

fn write_results(path: &std::path::Path, record: &Value) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let text = serde_json::to_string_pretty(record).unwrap_or_default();
    if let Err(e) = std::fs::write(path, text) {
        eprintln!("couldn't write {}: {e}", path.display());
    }
}

// ---------------------------------------------------------------------------------------------
// dry: nothing on screen.
// ---------------------------------------------------------------------------------------------

fn dry() -> Value {
    let round_trip = hello::dpapi_protect(b"probe")
        .and_then(|sealed| hello::dpapi_unprotect(&sealed))
        .map(|plain| &plain[..] == b"probe");
    json!({
        "mode": "dry",
        "time": utc_now(),
        "windows_build": win::build(),
        "elevated": win::elevated(),
        "hello_supported": hello::supported(),
        "tpm_2_0": hello::tpm_present(),
        "window_bindings_available": hello::window_bindings_available(),
        "probe_credential_exists": hello::credential_exists(CREDENTIAL).map_err(|e| e.to_string()),
        "dpapi_round_trip": round_trip.map_err(|e| e.to_string()),
        "fullscreen_state": win::notification_state(),
        "foreground_lock_timeout_ms": win::foreground_lock_timeout(),
    })
}

// ---------------------------------------------------------------------------------------------
// run: the owner's check.
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Step {
    op: &'static str,
    fullscreen: bool,
    gesture: bool,
}

const PHASE_1: [Step; 4] = [
    Step {
        op: "create",
        fullscreen: false,
        gesture: false,
    },
    Step {
        op: "create",
        fullscreen: true,
        gesture: false,
    },
    Step {
        op: "consent",
        fullscreen: false,
        gesture: false,
    },
    Step {
        op: "consent",
        fullscreen: true,
        gesture: false,
    },
];

const PHASE_2: [Step; 3] = [
    Step {
        op: "create",
        fullscreen: false,
        gesture: true,
    },
    Step {
        op: "sign",
        fullscreen: false,
        gesture: true,
    },
    Step {
        op: "sign",
        fullscreen: true,
        gesture: true,
    },
];

/// A rough length of a phase-1 step, for the countdown.
const PHASE_1_STEP_S: u64 = 10;
/// A rough length of a phase-2 step, for the countdown.
const PHASE_2_STEP_S: u64 = 14;

fn run(out: PathBuf) -> i32 {
    if !std::io::stdin().is_terminal() {
        eprintln!(
            "hello-probe run only runs at a real terminal (it raises Windows Hello prompts); nothing was done"
        );
        return 2;
    }
    if win::elevated() {
        eprintln!(
            "hello-probe run refuses to run elevated: the suite never does. Use a normal terminal."
        );
        return 2;
    }
    let pre = dry();
    if pre["hello_supported"] != json!(true) || pre["window_bindings_available"] != json!(true) {
        eprintln!(
            "Windows Hello isn't set up, or this Windows build lacks the window-owned calls:"
        );
        eprintln!("{}", serde_json::to_string_pretty(&pre).unwrap_or_default());
        write_results(
            &out,
            &json!({ "mode": "run", "preflight": pre, "route": "passphrase", "why": "preflight" }),
        );
        return 1;
    }
    // The throwaway credential goes on every way out: a normal end, an error or a panic (the
    // guard's drop), and Ctrl+C, Ctrl+Break or a closed terminal (the console handler).
    let mut guard = CredentialGuard { finished: false };
    if !cleanup::install_console_handler() {
        eprintln!("couldn't install the Ctrl+C handler; nothing was done");
        return 1;
    }
    // A leftover from an earlier run is removed first.
    let leftover = if pre["probe_credential_exists"]["Ok"] == json!(true) {
        Some(hello::kek_remove(CREDENTIAL).map_err(|e| e.to_string()))
    } else {
        None
    };
    println!(
        "This takes about 2 minutes. A pop-up at the top of the screen says when to keep your"
    );
    println!("hands off and when to confirm a Windows Hello prompt.");
    println!(
        "If you stop it part-way (Ctrl+C), it removes its test key itself; if that ever fails, run:"
    );
    println!("  {CLEANUP_COMMAND}");
    println!("Press Enter to start.");
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);

    let popup = popup::Popup::start();
    let mut steps = Vec::new();

    let phase_1_s = PHASE_1.len() as u64 * PHASE_1_STEP_S;
    popup.warn(
        10,
        "opens test windows and Windows Hello prompts and closes them itself",
        phase_1_s,
    );
    for step in PHASE_1 {
        steps.push(run_step(step, &popup));
    }

    let phase_2_s = PHASE_2.len() as u64 * PHASE_2_STEP_S;
    popup.warn_owner(
        10,
        "Next: 3 Windows Hello prompts. Don't touch anything until each one is in front,\nthen confirm it (face, fingerprint or PIN).",
        phase_2_s,
    );
    for (i, step) in PHASE_2.into_iter().enumerate() {
        // At least 10 s of warning before each prompt's hands-off stretch, so the tail of the
        // last confirmation (a PIN's keys, a mouse move) can't land in the next call.
        if i > 0 {
            popup.warn_owner(
                10,
                "Next prompt in a moment: hands off until it is in front, then confirm it.",
                (PHASE_2.len() - i) as u64 * PHASE_2_STEP_S,
            );
        }
        let record = run_step(step, &popup);
        popup.resume();
        let failed = record["result"]["ok"] != json!(true);
        steps.push(record);
        if failed && step.op == "create" {
            break;
        }
    }
    let deleted = hello::kek_remove(CREDENTIAL).map_err(|e| e.to_string());
    let gone = hello::credential_exists(CREDENTIAL).map_err(|e| e.to_string());
    guard.finished = gone == Ok(false);
    popup.done();

    let verdict = route_verdict(&steps, gone == Ok(false));
    let record = json!({
        "mode": "run",
        "time": utc_now(),
        "preflight": pre,
        "steps": steps,
        "cleanup": { "leftover_at_start": leftover, "delete": deleted, "exists_after": gone },
        "verdict": verdict,
    });
    write_results(&out, &record);
    std::thread::sleep(Duration::from_secs(5));
    popup.close();
    println!(
        "Route: {}",
        record["verdict"]["route"].as_str().unwrap_or("?")
    );
    println!("Results: {}", out.display());
    0
}

/// The one command that removes the throwaway credential by hand (no prompt), from any folder.
const CLEANUP_COMMAND: &str = r#"cargo run -q --offline --manifest-path "$env:USERPROFILE\LILITH-work\p4a-t28\Cargo.toml" -p lil-platform --example hello-probe -- cleanup"#;

/// Removes the throwaway credential when `run` ends any other way than cleanly: an early
/// return, an error or a panic (drop runs while unwinding). Says what to run if that fails.
struct CredentialGuard {
    /// Set once the credential was deleted and checked gone.
    finished: bool,
}

impl Drop for CredentialGuard {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        cleanup::remove_and_report("the run ended early");
    }
}

/// The abnormal-end cleanup, shared by the drop guard and the console handler.
mod cleanup {
    use super::{CLEANUP_COMMAND, CREDENTIAL, hello};
    use windows::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
        SetConsoleCtrlHandler,
    };
    use windows::core::BOOL;

    /// Deletes the credential (no prompt) and says so; on failure, prints the exact command.
    pub fn remove_and_report(why: &str) {
        match hello::kek_remove(CREDENTIAL).and_then(|()| hello::credential_exists(CREDENTIAL)) {
            Ok(false) => eprintln!("hello-probe: {why}; its test key {CREDENTIAL} is removed."),
            Ok(true) => eprintln!(
                "hello-probe: {why}, and its test key {CREDENTIAL} is still there. Remove it with:\n  {CLEANUP_COMMAND}"
            ),
            Err(e) => eprintln!(
                "hello-probe: {why}, and removing its test key {CREDENTIAL} failed ({e}). Remove it with:\n  {CLEANUP_COMMAND}"
            ),
        }
    }

    extern "system" fn on_console_event(event: u32) -> BOOL {
        match event {
            e if e == CTRL_C_EVENT
                || e == CTRL_BREAK_EVENT
                || e == CTRL_CLOSE_EVENT
                || e == CTRL_LOGOFF_EVENT
                || e == CTRL_SHUTDOWN_EVENT =>
            {
                remove_and_report("stopped");
                // Not handled: Windows' default handler then ends the process. The test window
                // and the owner child share this console and get the same event.
                BOOL(0)
            }
            _ => BOOL(0),
        }
    }

    /// Installs [`on_console_event`] for this process. `false` when Windows refused.
    pub fn install_console_handler() -> bool {
        // SAFETY: a plain `extern "system"` handler with the documented signature, for the
        // life of the process.
        unsafe { SetConsoleCtrlHandler(Some(on_console_event), true) }.is_ok()
    }
}

/// One step: the test window takes the front, then the owner child makes its call.
fn run_step(step: Step, popup: &popup::Popup) -> Value {
    let mut blocker = match spawn_self(&if step.fullscreen {
        vec!["blocker", "--fullscreen"]
    } else {
        vec!["blocker"]
    }) {
        Ok(c) => c,
        Err(e) => {
            return json!({ "step": step_name(step), "error": format!("starting the test window: {e}") });
        }
    };
    let blocker_line = read_json_line(&mut blocker);
    let blocker_hwnd = blocker_line["hwnd"].as_i64().unwrap_or(0) as isize;
    let deadline = Instant::now() + Duration::from_millis(BLOCKER_FRONT_MS);
    while win::foreground() != blocker_hwnd && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let blocker_in_front = win::foreground() == blocker_hwnd;

    let mode = if step.gesture { "gesture" } else { "placement" };
    let mut owner = match spawn_self(&["owner", "--op", step.op, "--mode", mode]) {
        Ok(c) => c,
        Err(e) => {
            let _ = blocker.kill();
            return json!({ "step": step_name(step), "error": format!("starting the owner: {e}") });
        }
    };
    let mut events = Vec::new();
    let mut result = Value::Null;
    if let Some(stdout) = owner.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let v: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
            match v["event"].as_str() {
                Some("result") => result = v,
                Some("not_in_front") if step.gesture => {
                    popup.owner_text("The Windows Hello prompt did not come to the front by itself.\nFind it (taskbar or Alt+Tab) and confirm it, or press Esc in it.");
                    events.push(v);
                }
                Some(_) => events.push(v),
                None => {}
            }
        }
    }
    let _ = owner.wait();
    let _ = blocker.kill();
    let _ = blocker.wait();
    json!({
        "step": step_name(step),
        "blocker": blocker_line,
        "blocker_in_front_before_call": blocker_in_front,
        "events": events,
        "result": result,
    })
}

fn step_name(step: Step) -> String {
    format!(
        "{} with {} in front ({})",
        step.op,
        if step.fullscreen {
            "a fullscreen window"
        } else {
            "another window"
        },
        if step.gesture {
            "confirmed by the owner"
        } else {
            "cancelled by the probe"
        }
    )
}

fn spawn_self(args: &[&str]) -> std::io::Result<Child> {
    Command::new(std::env::current_exe()?)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
}

fn read_json_line(child: &mut Child) -> Value {
    let Some(stdout) = child.stdout.as_mut() else {
        return Value::Null;
    };
    let mut line = String::new();
    let _ = BufReader::new(stdout).read_line(&mut line);
    serde_json::from_str(&line).unwrap_or(Value::Null)
}

/// The route the steps give (P4a-R38's order): KEK, else DPAPI with the gate, else passphrase.
fn route_verdict(steps: &[Value], credential_gone: bool) -> Value {
    let passed = |op: &str| {
        let mine: Vec<&Value> = steps
            .iter()
            .filter(|s| s["result"]["op"] == json!(op))
            .collect();
        !mine.is_empty() && mine.iter().all(|s| step_passed(s))
    };
    let signatures: Vec<&str> = steps
        .iter()
        .filter_map(|s| s["result"]["signature_sha256"].as_str())
        .collect();
    let signatures_match = signatures.len() == 2 && signatures[0] == signatures[1];
    let kek = passed("create") && passed("sign") && signatures_match && credential_gone;
    let dpapi = passed("consent");
    json!({
        "create_steps_pass": passed("create"),
        "sign_steps_pass": passed("sign"),
        "signatures_match": signatures_match,
        "credential_gone": credential_gone,
        "consent_steps_pass": dpapi,
        "route": if kek { "kek" } else if dpapi { "dpapi" } else { "passphrase" },
    })
}

fn step_passed(step: &Value) -> bool {
    step["blocker_in_front_before_call"] == json!(true)
        && step["result"]["placement"]["pass"] == json!(true)
        && (step["result"]["mode"] != json!("gesture") || step["result"]["ok"] == json!(true))
}

// ---------------------------------------------------------------------------------------------
// The child processes.
// ---------------------------------------------------------------------------------------------

/// A test window that takes the front and stays until its parent kills it.
fn blocker(fullscreen: bool) -> i32 {
    let window = win::TestWindow::open(fullscreen);
    let recipe = window.take_front();
    println!(
        "{}",
        json!({ "hwnd": window.hwnd(), "fullscreen": fullscreen, "front_recipe": recipe,
                "in_front": win::foreground() == window.hwnd(),
                "fullscreen_state": win::notification_state() })
    );
    let _ = std::io::stdout().flush();
    // Wait until the parent closes stdin or kills this process.
    let mut sink = String::new();
    let _ = std::io::stdin().read_line(&mut sink);
    0
}

fn emit(v: Value) {
    println!("{v}");
    let _ = std::io::stdout().flush();
}

/// The owner side of one step: its window, brought to the front with the product's recipe,
/// then the Windows Hello call it owns, sampled every [`SAMPLE_MS`].
fn owner(op: &str, gesture: bool) -> i32 {
    let body = format!(
        "LILITH hello-probe (plan 4a Task 28, Step 0).\n\nThis window owns a Windows Hello {op} prompt.\n{}",
        if gesture {
            "Confirm the prompt once it is in front."
        } else {
            "The probe cancels the prompt itself."
        }
    );
    let window = match OwnerWindow::open("LILITH hello-probe", &body) {
        Ok(w) => w,
        Err(e) => {
            emit(json!({ "event": "result", "op": op, "ok": false, "error": e.to_string() }));
            return 1;
        }
    };
    let raise = window.bring_to_front();
    let owner_in_front = win::foreground() == window.hwnd();
    let fullscreen_state = win::notification_state();
    let call_tick = win::tick();
    let started = Instant::now();
    let message = "LILITH hello-probe: confirm it is you (a test; nothing is approved)";
    let op_handle = match op {
        "create" => hello::start_create_for_window(window.hwnd(), CREDENTIAL).map(Op::Create),
        "sign" => hello::start_sign_for_window(window.hwnd(), CREDENTIAL, &CHALLENGE).map(Op::Sign),
        "consent" => hello::start_consent_for_window(window.hwnd(), message).map(Op::Consent),
        other => Err(std::io::Error::other(format!("unknown op {other}"))),
    };
    let op_handle = match op_handle {
        Ok(h) => h,
        Err(e) => {
            emit(
                json!({ "event": "result", "op": op, "ok": false, "raise": raise.name(), "error": e.to_string() }),
            );
            return 1;
        }
    };

    let mut samples: Vec<Sample> = Vec::new();
    let max_ms = if gesture {
        GESTURE_MAX_MS
    } else {
        PLACEMENT_MAX_MS
    };
    let mut told_not_in_front = false;
    loop {
        let s = win::sample(started, call_tick);
        let in_front = s.is_prompt();
        samples.push(s);
        let elapsed = started.elapsed().as_millis() as u64;
        if !gesture && in_front && elapsed >= 300 {
            break;
        }
        if gesture
            && !told_not_in_front
            && !in_front
            && elapsed > 10_000
            && !samples.iter().any(Sample::is_prompt)
        {
            emit(json!({ "event": "not_in_front", "after_ms": elapsed }));
            told_not_in_front = true;
        }
        if op_handle.finished() || elapsed >= max_ms {
            break;
        }
        std::thread::sleep(Duration::from_millis(SAMPLE_MS));
    }
    let placement = judge(&samples);
    let mut result = json!({
        "event": "result", "op": op, "mode": if gesture { "gesture" } else { "placement" },
        "raise": raise.name(), "owner_in_front_before_call": owner_in_front,
        "fullscreen_state_at_call": fullscreen_state,
        "placement": placement,
        "samples": samples.iter().map(Sample::to_json).collect::<Vec<_>>(),
    });
    if !gesture {
        op_handle.cancel();
        let closed = wait_closed();
        result["cancelled"] = json!(true);
        result["prompt_closed_after_cancel"] = json!(closed);
        result["ok"] = json!(true);
        emit(result);
        return 0;
    }
    match op_handle.finish() {
        Ok(Finished::Created) => result["ok"] = json!(true),
        Ok(Finished::Signed(sig)) => {
            result["ok"] = json!(true);
            // Only a hash of the signature is recorded, to compare the two signs.
            result["signature_sha256"] = json!(hex(&Sha256::digest(&sig[..])));
            result["signature_len"] = json!(sig.len());
        }
        Ok(Finished::Consented(v)) => result["ok"] = json!(v),
        Err(e) => {
            result["ok"] = json!(false);
            result["error"] = json!(e.to_string());
        }
    }
    emit(result);
    0
}

/// Up to 3 s for the prompt window to go after a cancel.
fn wait_closed() -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if win::window_of_class(hello::PROMPT_WINDOW_CLASS) == 0 {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

enum Op {
    Create(hello::PendingCreate),
    Sign(hello::PendingSign),
    Consent(hello::PendingConsent),
}

enum Finished {
    Created,
    Signed(zeroize::Zeroizing<Vec<u8>>),
    Consented(bool),
}

impl Op {
    fn finished(&self) -> bool {
        match self {
            Op::Create(p) => p.finished(),
            Op::Sign(p) => p.finished(),
            Op::Consent(p) => p.finished(),
        }
    }
    fn cancel(&self) {
        match self {
            Op::Create(p) => p.cancel(),
            Op::Sign(p) => p.cancel(),
            Op::Consent(p) => p.cancel(),
        }
    }
    fn finish(self) -> std::io::Result<Finished> {
        match self {
            Op::Create(p) => hello::finish_create(p).map(|()| Finished::Created),
            Op::Sign(p) => hello::finish_sign(p).map(Finished::Signed),
            Op::Consent(p) => hello::finish_consent(p).map(Finished::Consented),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// One reading, [`SAMPLE_MS`] apart.
#[derive(Debug, Clone, PartialEq)]
struct Sample {
    t_ms: u64,
    fg_class: String,
    fg_exe: String,
    /// The last input, in ms after the call (negative: before it).
    last_input_ms: i64,
}

impl Sample {
    fn is_prompt(&self) -> bool {
        self.fg_class == hello::PROMPT_WINDOW_CLASS
            || self.fg_exe.eq_ignore_ascii_case("CredentialUIBroker.exe")
    }

    fn to_json(&self) -> Value {
        json!({ "t_ms": self.t_ms, "fg_class": self.fg_class, "fg_exe": self.fg_exe,
                "last_input_ms": self.last_input_ms })
    }
}

/// The bar for one call: the prompt is the foreground window within [`IN_FRONT_WITHIN_MS`], and
/// no input happened from the call until it was.
fn judge(samples: &[Sample]) -> Value {
    let first = samples.iter().position(Sample::is_prompt);
    let in_front_ms = first.map(|i| samples[i].t_ms);
    let upto = first.map_or(samples.len(), |i| i + 1);
    let input_before = samples[..upto].iter().any(|s| s.last_input_ms >= 0);
    let pass = matches!(in_front_ms, Some(ms) if ms <= IN_FRONT_WITHIN_MS) && !input_before;
    json!({
        "prompt_in_front_ms": in_front_ms,
        "input_before_prompt_in_front": input_before,
        "pass": pass,
    })
}

// ---------------------------------------------------------------------------------------------
// Windows helpers (probe only).
// ---------------------------------------------------------------------------------------------

mod win {
    use std::ffi::c_void;
    use std::time::Instant;

    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, COLOR_WINDOW, DT_CENTER, DT_WORDBREAK, DrawTextW, EndPaint, FillRect, HBRUSH,
        PAINTSTRUCT, SetBkMode, TRANSPARENT,
    };
    use windows::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::System::Threading::{
        AttachThreadInput, GetCurrentProcess, GetCurrentThreadId, OpenProcess, OpenProcessToken,
        PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetLastInputInfo, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
        LASTINPUTINFO, SendInput, VK_MENU,
    };
    use windows::Win32::UI::Shell::SHQueryUserNotificationState;
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DispatchMessageW,
        EnumWindows, GetClassNameW, GetClientRect, GetForegroundWindow, GetMessageW,
        GetSystemMetrics, GetWindowThreadProcessId, IsWindowVisible, MSG, PostQuitMessage,
        RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SPI_GETFOREGROUNDLOCKTIMEOUT, SW_SHOW,
        SetForegroundWindow, ShowWindow, SystemParametersInfoW, TranslateMessage, WM_DESTROY,
        WM_PAINT, WNDCLASSW, WS_EX_APPWINDOW, WS_OVERLAPPEDWINDOW, WS_POPUP, WS_VISIBLE,
    };
    use windows::core::{BOOL, HSTRING, PWSTR, w};

    use super::Sample;

    pub fn build() -> String {
        std::process::Command::new("cmd")
            .args(["/c", "ver"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    }

    pub fn elevated() -> bool {
        // SAFETY: a token query on this process; the handle is closed.
        unsafe {
            let mut token = Default::default();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
                return false;
            }
            let mut elevation = TOKEN_ELEVATION::default();
            let mut len = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                Some((&raw mut elevation).cast()),
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut len,
            )
            .is_ok();
            let _ = CloseHandle(token);
            ok && elevation.TokenIsElevated != 0
        }
    }

    pub fn notification_state() -> i32 {
        // SAFETY: a plain query.
        unsafe { SHQueryUserNotificationState() }.map_or(-1, |s| s.0)
    }

    pub fn foreground_lock_timeout() -> i64 {
        let mut ms = 0u32;
        // SAFETY: SPI_GETFOREGROUNDLOCKTIMEOUT writes one u32 to the pointer; read only.
        match unsafe {
            SystemParametersInfoW(
                SPI_GETFOREGROUNDLOCKTIMEOUT,
                0,
                Some((&raw mut ms).cast()),
                Default::default(),
            )
        } {
            Ok(()) => i64::from(ms),
            Err(_) => -1,
        }
    }

    pub fn foreground() -> isize {
        // SAFETY: a plain query.
        unsafe { GetForegroundWindow() }.0 as isize
    }

    pub fn tick() -> u32 {
        // SAFETY: a plain query.
        unsafe { GetTickCount() }
    }

    fn last_input_tick() -> u32 {
        let mut info = LASTINPUTINFO {
            cbSize: size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        // SAFETY: a sized structure owned by this frame.
        let _ = unsafe { GetLastInputInfo(&mut info) };
        info.dwTime
    }

    fn class_of(hwnd: HWND) -> String {
        let mut buf = [0u16; 256];
        // SAFETY: a plain query into a local buffer.
        let n = unsafe { GetClassNameW(hwnd, &mut buf) };
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }

    fn exe_of(hwnd: HWND) -> String {
        // SAFETY: plain queries; the process handle is closed.
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return String::new();
            };
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
            .is_ok();
            let _ = CloseHandle(process);
            if !ok {
                return String::new();
            }
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            path.rsplit('\\').next().unwrap_or("").to_string()
        }
    }

    pub fn sample(started: Instant, call_tick: u32) -> Sample {
        // SAFETY: a plain query.
        let fg = unsafe { GetForegroundWindow() };
        let last = last_input_tick();
        Sample {
            t_ms: started.elapsed().as_millis() as u64,
            fg_class: class_of(fg),
            fg_exe: exe_of(fg),
            // Wrapping tick arithmetic: the difference as a signed 32-bit value.
            last_input_ms: i64::from(last.wrapping_sub(call_tick) as i32),
        }
    }

    pub fn window_of_class(class_name: &str) -> isize {
        struct Ctx {
            want: String,
            found: isize,
        }
        extern "system" fn each(hwnd: HWND, lp: LPARAM) -> BOOL {
            // SAFETY: lp is the &mut Ctx below, live for the enumeration.
            let ctx = unsafe { &mut *(lp.0 as *mut Ctx) };
            // SAFETY: a plain query on a window handle from the enumeration.
            if class_of(hwnd) == ctx.want && unsafe { IsWindowVisible(hwnd) }.as_bool() {
                ctx.found = hwnd.0 as isize;
            }
            BOOL(1)
        }
        let mut ctx = Ctx {
            want: class_name.to_string(),
            found: 0,
        };
        // SAFETY: EnumWindows calls `each` with &mut ctx for the duration of the call.
        let _ = unsafe { EnumWindows(Some(each), LPARAM((&raw mut ctx) as isize)) };
        ctx.found
    }

    /// "Another application": an ordinary or a fullscreen window on its own thread.
    pub struct TestWindow {
        hwnd: isize,
    }

    impl TestWindow {
        pub fn open(fullscreen: bool) -> Self {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                // SAFETY: window creation and a message loop on this thread.
                unsafe {
                    let class = WNDCLASSW {
                        lpfnWndProc: Some(test_wndproc),
                        lpszClassName: w!("LILITH-probe-test-window"),
                        hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as usize as *mut c_void),
                        ..Default::default()
                    };
                    RegisterClassW(&class);
                    let (style, x, y, cx, cy) = if fullscreen {
                        (
                            WS_POPUP | WS_VISIBLE,
                            0,
                            0,
                            GetSystemMetrics(SM_CXSCREEN),
                            GetSystemMetrics(SM_CYSCREEN),
                        )
                    } else {
                        (
                            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                            CW_USEDEFAULT,
                            CW_USEDEFAULT,
                            900,
                            560,
                        )
                    };
                    let title = HSTRING::from(if fullscreen {
                        "Probe test window (fullscreen): another application"
                    } else {
                        "Probe test window: another application"
                    });
                    let hwnd = CreateWindowExW(
                        WS_EX_APPWINDOW,
                        w!("LILITH-probe-test-window"),
                        &title,
                        style,
                        x,
                        y,
                        cx,
                        cy,
                        None,
                        None,
                        None,
                        None,
                    )
                    .unwrap_or_default();
                    let _ = ShowWindow(hwnd, SW_SHOW);
                    let _ = tx.send(hwnd.0 as isize);
                    let mut msg = MSG::default();
                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            });
            Self {
                hwnd: rx.recv().unwrap_or(0),
            }
        }

        pub fn hwnd(&self) -> isize {
            self.hwnd
        }

        /// Takes the front by any means before the call: `SetForegroundWindow`, then with the
        /// input attached, then after a synthetic Alt tap. Returns the means that worked.
        pub fn take_front(&self) -> &'static str {
            let hwnd = HWND(self.hwnd as *mut c_void);
            // SAFETY: window calls on this process's own window; a synthetic Alt down and up.
            unsafe {
                if SetForegroundWindow(hwnd).as_bool() && GetForegroundWindow() == hwnd {
                    return "set_foreground";
                }
                let front_thread = GetWindowThreadProcessId(GetForegroundWindow(), None);
                let mine = GetCurrentThreadId();
                if front_thread != 0 && front_thread != mine {
                    let attached = AttachThreadInput(mine, front_thread, true).as_bool();
                    let _ = BringWindowToTop(hwnd);
                    let _ = SetForegroundWindow(hwnd);
                    if attached {
                        let _ = AttachThreadInput(mine, front_thread, false);
                    }
                    if GetForegroundWindow() == hwnd {
                        return "attached_input";
                    }
                }
                let key = |flags| INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: VK_MENU,
                            dwFlags: flags,
                            ..Default::default()
                        },
                    },
                };
                let inputs = [key(Default::default()), key(KEYEVENTF_KEYUP)];
                SendInput(&inputs, size_of::<INPUT>() as i32);
                let _ = SetForegroundWindow(hwnd);
                if GetForegroundWindow() == hwnd {
                    return "alt_tap";
                }
            }
            "failed"
        }
    }

    extern "system" fn test_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        // SAFETY: the window procedure of the probe's own class.
        unsafe {
            match msg {
                WM_PAINT => {
                    let mut ps = PAINTSTRUCT::default();
                    let hdc = BeginPaint(hwnd, &mut ps);
                    let mut rect = Default::default();
                    let _ = GetClientRect(hwnd, &mut rect);
                    let _ = FillRect(
                        hdc,
                        &rect,
                        HBRUSH((COLOR_WINDOW.0 + 1) as usize as *mut c_void),
                    );
                    SetBkMode(hdc, TRANSPARENT);
                    rect.top += 40;
                    let mut text: Vec<u16> =
                        "LILITH hello-probe: a test window standing in for another application.\nThe probe closes it itself."
                            .encode_utf16()
                            .collect();
                    DrawTextW(hdc, &mut text, &mut rect, DT_CENTER | DT_WORDBREAK);
                    let _ = EndPaint(hwnd, &ps);
                    LRESULT(0)
                }
                WM_DESTROY => {
                    PostQuitMessage(0);
                    LRESULT(0)
                }
                _ => DefWindowProcW(hwnd, msg, wp, lp),
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The hands-off pop-up (the owner's rule: at least 10 s of warning, then a countdown).
// ---------------------------------------------------------------------------------------------

mod popup {
    use super::*;
    use std::ffi::c_void;

    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush,
        DEFAULT_CHARSET, DT_CENTER, DT_WORDBREAK, DeleteObject, DrawTextW, EndPaint, FillRect,
        InvalidateRect, OUT_DEFAULT_PRECIS, PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor,
        TRANSPARENT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
        GetSystemMetrics, HWND_TOPMOST, LWA_ALPHA, MSG, PostMessageW, PostQuitMessage,
        RegisterClassW, SM_CXSCREEN, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        SWP_SHOWWINDOW, SetLayeredWindowAttributes, SetTimer, SetWindowPos, ShowWindow,
        TranslateMessage, WM_CLOSE, WM_DESTROY, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED,
        WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
    };
    use windows::core::w;

    #[derive(Clone)]
    enum State {
        /// "In N s: hands off", then a countdown of `run_s` from `until`.
        Warn {
            until: Instant,
            what: String,
            run_s: u64,
            owner: bool,
        },
        Owner {
            text: String,
            end: Instant,
        },
        Done,
    }

    static STATE: Mutex<Option<State>> = Mutex::new(None);
    /// The last countdown, which [`Popup::resume`] shows again after a one-off message.
    static LAST_WARN: Mutex<Option<State>> = Mutex::new(None);

    pub struct Popup {
        hwnd: Arc<Mutex<isize>>,
    }

    impl Popup {
        pub fn start() -> Self {
            let hwnd = Arc::new(Mutex::new(0isize));
            let shared = hwnd.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let h = create();
                *shared.lock().unwrap() = h;
                let _ = tx.send(());
                pump();
            });
            let _ = rx.recv();
            Self { hwnd }
        }

        fn set(&self, state: State) {
            if matches!(state, State::Warn { .. }) {
                *LAST_WARN.lock().unwrap() = Some(state.clone());
            }
            *STATE.lock().unwrap() = Some(state);
        }

        /// Shows the last countdown again (after [`Popup::owner_text`]).
        pub fn resume(&self) {
            let last = LAST_WARN.lock().unwrap().clone();
            if let Some(state) = last {
                *STATE.lock().unwrap() = Some(state);
            }
        }

        /// At least `warn_s` of "In N s: hands off", then a countdown of `run_s`. Blocks for
        /// the warning.
        pub fn warn(&self, warn_s: u64, what: &str, run_s: u64) {
            let until = Instant::now() + Duration::from_secs(warn_s);
            self.set(State::Warn {
                until,
                what: what.to_string(),
                run_s,
                owner: false,
            });
            std::thread::sleep(Duration::from_secs(warn_s));
        }

        /// Like [`Popup::warn`], for a stretch in which the owner confirms prompts.
        pub fn warn_owner(&self, warn_s: u64, what: &str, run_s: u64) {
            let until = Instant::now() + Duration::from_secs(warn_s);
            self.set(State::Warn {
                until,
                what: what.to_string(),
                run_s,
                owner: true,
            });
            std::thread::sleep(Duration::from_secs(warn_s));
        }

        pub fn owner_text(&self, text: &str) {
            self.set(State::Owner {
                text: text.to_string(),
                end: Instant::now() + Duration::from_secs(60),
            });
        }

        pub fn done(&self) {
            self.set(State::Done);
        }

        pub fn close(&self) {
            let h = *self.hwnd.lock().unwrap();
            // SAFETY: posting to this process's own window.
            unsafe {
                let _ = PostMessageW(Some(HWND(h as *mut c_void)), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }

    fn text_and_color() -> (String, u32) {
        let now = Instant::now();
        match STATE.lock().unwrap().clone() {
            None => (String::new(), 0x00303030),
            Some(State::Warn {
                until,
                what,
                run_s,
                owner,
            }) => {
                if now < until {
                    let n = (until - now).as_secs() + 1;
                    let text = if owner {
                        format!("In {n} s: {what}")
                    } else {
                        format!("In {n} s: HANDS OFF the mouse and keyboard.\nThe probe {what}.")
                    };
                    (text, 0x00005a96) // amber (BGR)
                } else {
                    let left = (until + Duration::from_secs(run_s))
                        .saturating_duration_since(now)
                        .as_secs();
                    let tail = if left > 0 {
                        format!(
                            "You can use the PC again in about {}:{:02}.",
                            left / 60,
                            left % 60
                        )
                    } else {
                        "Almost done...".to_string()
                    };
                    if owner {
                        (format!("{what}\n{tail}"), 0x00803010) // blue
                    } else {
                        (format!("HANDS OFF: the probe {what}.\n{tail}"), 0x001e1e8c) // red
                    }
                }
            }
            Some(State::Owner { text, end }) => {
                let left = end.saturating_duration_since(now).as_secs();
                (format!("{text}\n({left} s)"), 0x00803010)
            }
            Some(State::Done) => ("Done: you can use the PC again.".to_string(), 0x00286e14), // green
        }
    }

    fn create() -> isize {
        // SAFETY: window creation on this thread; the class and title are static.
        unsafe {
            let class = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                lpszClassName: w!("LILITH-probe-popup"),
                ..Default::default()
            };
            RegisterClassW(&class);
            let width = 760;
            let x = (GetSystemMetrics(SM_CXSCREEN) - width) / 2;
            let Ok(hwnd) = CreateWindowExW(
                WS_EX_NOACTIVATE
                    | WS_EX_TOOLWINDOW
                    | WS_EX_TRANSPARENT
                    | WS_EX_LAYERED
                    | WS_EX_TOPMOST,
                w!("LILITH-probe-popup"),
                w!("LILITH hello-probe"),
                WS_POPUP,
                x,
                24,
                width,
                150,
                None,
                None,
                None,
                None,
            ) else {
                return 0;
            };
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 238, LWA_ALPHA);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
            );
            SetTimer(Some(hwnd), 1, 250, None);
            hwnd.0 as isize
        }
    }

    fn pump() {
        // SAFETY: the message loop of this thread's window.
        unsafe {
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        // SAFETY: the window procedure of the pop-up's own class; GDI objects are deleted.
        unsafe {
            match msg {
                WM_TIMER => {
                    let _ = InvalidateRect(Some(hwnd), None, true);
                    LRESULT(0)
                }
                WM_PAINT => {
                    let (text, color) = text_and_color();
                    let mut ps = PAINTSTRUCT::default();
                    let hdc = BeginPaint(hwnd, &mut ps);
                    let mut rect = RECT {
                        left: 0,
                        top: 0,
                        right: 760,
                        bottom: 150,
                    };
                    let brush = CreateSolidBrush(COLORREF(color));
                    let _ = FillRect(hdc, &rect, brush);
                    let _ = DeleteObject(brush.into());
                    let font = CreateFontW(
                        26,
                        0,
                        0,
                        0,
                        400,
                        0,
                        0,
                        0,
                        DEFAULT_CHARSET,
                        OUT_DEFAULT_PRECIS,
                        CLIP_DEFAULT_PRECIS,
                        CLEARTYPE_QUALITY,
                        0,
                        w!("Segoe UI"),
                    );
                    let old = SelectObject(hdc, font.into());
                    SetBkMode(hdc, TRANSPARENT);
                    SetTextColor(hdc, COLORREF(0x00ffffff));
                    rect.left += 16;
                    rect.right -= 16;
                    rect.top += 18;
                    let mut wide: Vec<u16> = text.encode_utf16().collect();
                    DrawTextW(hdc, &mut wide, &mut rect, DT_CENTER | DT_WORDBREAK);
                    SelectObject(hdc, old);
                    let _ = DeleteObject(font.into());
                    let _ = EndPaint(hwnd, &ps);
                    LRESULT(0)
                }
                WM_CLOSE => {
                    let _ = DestroyWindow(hwnd);
                    LRESULT(0)
                }
                WM_DESTROY => {
                    PostQuitMessage(0);
                    LRESULT(0)
                }
                _ => DefWindowProcW(hwnd, msg, wp, lp),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t_ms: u64, class: &str, last_input_ms: i64) -> Sample {
        Sample {
            t_ms,
            fg_class: class.into(),
            fg_exe: String::new(),
            last_input_ms,
        }
    }

    const P: &str = hello::PROMPT_WINDOW_CLASS;

    #[test]
    fn a_prompt_in_front_in_time_with_no_input_passes() {
        let v = judge(&[
            s(0, "Other", -5000),
            s(150, "Other", -5000),
            s(1950, P, -5000),
        ]);
        assert_eq!(v["pass"], json!(true));
        assert_eq!(v["prompt_in_front_ms"], json!(1950));
    }

    #[test]
    fn late_never_or_after_input_fails() {
        assert_eq!(
            judge(&[s(0, "Other", -1), s(2100, P, -1)])["pass"],
            json!(false)
        );
        assert_eq!(
            judge(&[s(0, "Other", -1), s(150, "Other", -1)])["pass"],
            json!(false)
        );
        // Input after the call and before the prompt came forward: not "by itself".
        assert_eq!(
            judge(&[s(0, "Other", -1), s(150, "Other", 120), s(300, P, 120)])["pass"],
            json!(false)
        );
        // Input after the prompt was in front (the gesture) doesn't count.
        let v = judge(&[s(0, "Other", -1), s(300, P, -1), s(450, P, 400)]);
        assert_eq!(v["pass"], json!(true));
    }

    #[test]
    fn the_broker_process_counts_as_the_prompt() {
        let mut x = s(150, "SomeOtherHost", -1);
        x.fg_exe = "CredentialUIBroker.exe".into();
        assert!(x.is_prompt());
    }

    #[test]
    fn the_route_follows_p4a_r38s_order() {
        let step = |op: &str, pass: bool, sig: Option<&str>| {
            let mut r =
                json!({ "op": op, "mode": "gesture", "ok": true, "placement": { "pass": pass } });
            if let Some(sig) = sig {
                r["signature_sha256"] = json!(sig);
            }
            json!({ "blocker_in_front_before_call": true, "result": r })
        };
        let all = |create: bool, sign: bool, consent: bool, sig2: &str| {
            vec![
                step("create", create, None),
                step("consent", consent, None),
                step("sign", sign, Some("aa")),
                step("sign", sign, Some(sig2)),
            ]
        };
        assert_eq!(
            route_verdict(&all(true, true, true, "aa"), true)["route"],
            json!("kek")
        );
        assert_eq!(
            route_verdict(&all(true, true, true, "bb"), true)["route"],
            json!("dpapi")
        );
        assert_eq!(
            route_verdict(&all(true, true, true, "aa"), false)["route"],
            json!("dpapi")
        );
        assert_eq!(
            route_verdict(&all(false, true, true, "aa"), true)["route"],
            json!("dpapi")
        );
        assert_eq!(
            route_verdict(&all(false, false, false, "aa"), true)["route"],
            json!("passphrase")
        );
        // A step whose test window wasn't in front before the call is not a pass.
        let mut steps = all(true, true, true, "aa");
        steps[0]["blocker_in_front_before_call"] = json!(false);
        assert_eq!(route_verdict(&steps, true)["route"], json!("dpapi"));
    }
}
