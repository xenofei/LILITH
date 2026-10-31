//! A core pipe whose reported server isn't who holds it (ARCH-027; Appendix G "Connections").
//!
//! `GetNamedPipeServerProcessId` names the pipe's creator, whoever holds the handle now. Each
//! test here has a `creator` copy of this test binary make a test channel's core pipe, copy its
//! handle into a `holder` and close its own. The creator stays on, in a folder the client trusts
//! and as this user, so it passes every check of a pid, as a copy that took a dead core's pid
//! does. The holder serves the pipe as a hostile process would, writing what it reads to its out
//! file. A real core can't be used: nothing makes it hand its handle over.
//!
//! **The probe.** Both are this binary running its ignored `probe` test, with `PROBE_ROLE` set.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use lil_client::blocking::{BlockingClient, drain};
use lil_client::{ClientConfig, ClientError, StartPolicy};
use lil_protocol::CallerKind;
use lil_testkit::TestEnv;
use serde_json::{Value, json};
use windows::Win32::Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};

const PROBE_ROLE: &str = "LIL_CLIENT_PROBE_ROLE";
const PROBE_OUT: &str = "LIL_CLIENT_PROBE_OUT";
const PROBE_PIPE: &str = "LIL_CLIENT_PROBE_PIPE";
const PROBE_HOLDER: &str = "LIL_CLIENT_PROBE_HOLDER";
const PROBE_HANDED: &str = "LIL_CLIENT_PROBE_HANDED";
/// A failure deadline for each wait, never a measurement.
const DEADLINE: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------------------------
// The probe's side.
// ---------------------------------------------------------------------------------------------

fn write_out(line: &str) {
    let path = std::env::var_os(PROBE_OUT).expect("the probe's out file");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("the probe's out file");
    writeln!(f, "{line}").expect("write the probe's out file");
}

#[test]
#[ignore = "probe"]
fn probe() {
    match std::env::var(PROBE_ROLE).as_deref() {
        Ok("creator") => pipe_creator(),
        Ok("holder") => pipe_holder(),
        _ => {}
    }
}

/// `creator`: makes the pipe, copies its handle into the holder, closes its own and stays on.
fn pipe_creator() -> ! {
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
    use windows::Win32::System::Pipes::{
        CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE};
    use windows::core::HSTRING;
    let name = std::env::var(PROBE_PIPE).expect("the pipe's name");
    let holder: u32 = std::env::var(PROBE_HOLDER).unwrap().parse().unwrap();
    let user = lil_platform::process::current_user_sid().expect("this user's SID");
    let mut sd = PSECURITY_DESCRIPTOR::default();
    let mut copy = HANDLE::default();
    // SAFETY: a NUL-terminated SDDL and a valid out-pointer, its descriptor left to the process's
    // end (a test) and outliving the attributes that read it; every handle is this process's own
    // and closed once, and the copy belongs to the holder from here on.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &HSTRING::from(lil_platform::pipe::pipe_sddl(&user)),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
        .expect("the pipe's descriptor");
        let attributes = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap(),
            lpSecurityDescriptor: sd.0,
            bInheritHandle: false.into(),
        };
        let pipe = CreateNamedPipeW(
            &HSTRING::from(name),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            65536,
            65536,
            0,
            Some(&raw const attributes),
        );
        assert!(!pipe.is_invalid(), "the pipe can't be made");
        let target = OpenProcess(PROCESS_DUP_HANDLE, false, holder).expect("the holder");
        DuplicateHandle(
            GetCurrentProcess(),
            pipe,
            target,
            &raw mut copy,
            0,
            false,
            DUPLICATE_SAME_ACCESS,
        )
        .expect("a copy of the pipe's handle");
        let _ = CloseHandle(target);
        let _ = CloseHandle(pipe);
    }
    std::fs::write(
        std::env::var_os(PROBE_HANDED).unwrap(),
        format!("{}", copy.0 as usize),
    )
    .expect("the handed file");
    write_out("handed");
    loop {
        // A test's end kills it.
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// `holder`: waits for the creator's handle, then serves the pipe one client after another. It
/// writes `got <line>` for each line it reads, and to a `core/prove` it tries every way it has to
/// set the event (`proof-open <ok | code>`, and `proof-impersonated <ok | code>` under the
/// client's token), then says it did, whatever came of it; anything else is -32601. Before any
/// client comes, `relay-open <ok | code>` says whether it could reach the core's pipe by its name,
/// which is how it would have a real core set the event for it.
fn pipe_holder() -> ! {
    use std::io::{BufRead as _, BufReader};
    use std::mem::ManuallyDrop;
    use std::os::windows::io::FromRawHandle as _;
    use windows::Win32::Security::RevertToSelf;
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, DisconnectNamedPipe, ImpersonateNamedPipeClient,
    };
    use windows::Win32::System::Threading::{EVENT_MODIFY_STATE, OpenEventW, SetEvent};
    use windows::core::HSTRING;
    write_out("ready");
    let handed = PathBuf::from(std::env::var_os(PROBE_HANDED).unwrap());
    let until = Instant::now() + DEADLINE;
    let pipe = loop {
        if let Ok(text) = std::fs::read_to_string(&handed)
            && !text.is_empty()
        {
            break HANDLE(text.parse::<usize>().unwrap() as *mut _);
        }
        assert!(Instant::now() < until, "no handle was handed over");
        std::thread::sleep(Duration::from_millis(5));
    };
    // Before the test's client comes: the test waits for `listening`.
    let relay = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(std::env::var(PROBE_PIPE).unwrap());
    write_out(&format!(
        "relay-open {}",
        relay.map_or_else(
            |e| e.raw_os_error().unwrap_or_default().to_string(),
            |_| "ok".to_string()
        )
    ));
    write_out("listening");
    let code = |e: windows::core::Error| format!("{}", e.code().0 & 0xFFFF);
    let set = |name: &str| -> Result<(), windows::core::Error> {
        // SAFETY: the name outlives the call; the event is closed once.
        unsafe {
            let event = OpenEventW(EVENT_MODIFY_STATE, false, &HSTRING::from(name))?;
            let done = SetEvent(event);
            let _ = CloseHandle(event);
            done
        }
    };
    // SAFETY: the handle is this process's, and it outlives the file, which never closes it.
    let file = ManuallyDrop::new(unsafe { std::fs::File::from_raw_handle(pipe.0.cast::<_>()) });
    loop {
        // SAFETY: a synchronous call on an open pipe handle; "already connected" is no matter.
        let _ = unsafe { ConnectNamedPipe(pipe, None) };
        let mut reader = BufReader::new(&*file);
        let mut line = String::new();
        while matches!(reader.read_line(&mut line), Ok(n) if n > 0) {
            write_out(&format!("got {}", line.trim_end()));
            let message: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
            let id = message["id"].clone();
            let reply = if message["method"] == "core/prove" {
                let name = message["params"]["name"].as_str().unwrap_or_default();
                write_out(&format!(
                    "proof-open {}",
                    set(name).map_or_else(code, |()| "ok".to_string())
                ));
                // SAFETY: the pipe is connected; this thread reverts before anything else.
                let impersonated = unsafe { ImpersonateNamedPipeClient(pipe) }.and_then(|()| {
                    let set = set(name);
                    // SAFETY: as above.
                    unsafe { RevertToSelf() }?;
                    set
                });
                write_out(&format!(
                    "proof-impersonated {}",
                    impersonated.map_or_else(code, |()| "ok".to_string())
                ));
                json!({"jsonrpc": "2.0", "id": id, "result": {"status": "ok", "proved": true}})
            } else {
                json!({"jsonrpc": "2.0", "id": id,
                       "error": {"code": -32601, "message": "method not found"}})
            };
            let mut writer: &std::fs::File = &file;
            let _ = writeln!(writer, "{reply}");
            let _ = writer.flush();
            line.clear();
        }
        write_out("eof");
        // SAFETY: an open pipe handle.
        let _ = unsafe { DisconnectNamedPipe(pipe) };
    }
}

// ---------------------------------------------------------------------------------------------
// The test's side.
// ---------------------------------------------------------------------------------------------

fn read_lines(out: &Path) -> Vec<String> {
    std::fs::read_to_string(out)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// Waits until `lines()` has a line that starts with `prefix`.
fn wait_line(lines: impl Fn() -> Vec<String>, prefix: &str) {
    let until = Instant::now() + DEADLINE;
    while !lines().iter().any(|l| l.starts_with(prefix)) {
        assert!(Instant::now() < until, "no `{prefix}` line: {:?}", lines());
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Waits until `holder` has seen `clients` clients come and go.
fn wait_clients(lines: impl Fn() -> Vec<String>, clients: usize) -> Vec<String> {
    let until = Instant::now() + DEADLINE;
    loop {
        let seen = lines();
        if seen.iter().filter(|l| *l == "eof").count() >= clients {
            return seen;
        }
        assert!(Instant::now() < until, "{seen:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A probe at this process's level, killed when dropped.
struct Probe {
    child: Child,
    out: PathBuf,
}

impl Probe {
    fn start(role: &str, out: PathBuf, extra: &[(&str, &std::ffi::OsStr)]) -> Probe {
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "probe",
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(PROBE_ROLE, role)
            .env(PROBE_OUT, &out)
            .envs(extra.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start the probe");
        Probe { child, out }
    }

    fn lines(&self) -> Vec<String> {
        read_lines(&self.out)
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Labels `dir` and what it holds Low-integrity writable, so a Low process can write its out file
/// there. Never the folder of an exe the test starts at Medium: this test binary's own folder is
/// left alone.
fn label_low(dir: &Path) {
    let done = Command::new("icacls")
        .arg(dir)
        .args(["/setintegritylevel", "(OI)(CI)low"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("icacls");
    assert!(done.success(), "icacls {}", dir.display());
}

/// A `holder` probe at Low integrity, as a sandboxed helper of this user: this process's token,
/// duplicated with the Low label (S-1-16-4096), through `CreateProcessAsUserW`.
struct LowHolder {
    process: HANDLE,
    out: PathBuf,
}

impl LowHolder {
    fn start(out: PathBuf, pipe: &str, handed: &Path) -> LowHolder {
        use std::os::windows::ffi::OsStrExt as _;
        use windows::Win32::Foundation::{HLOCAL, LocalFree};
        use windows::Win32::Security::Authorization::ConvertStringSidToSidW;
        use windows::Win32::Security::{
            DuplicateTokenEx, PSID, SID_AND_ATTRIBUTES, SecurityImpersonation, SetTokenInformation,
            TOKEN_ACCESS_MASK, TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE,
            TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenIntegrityLevel, TokenPrimary,
        };
        use windows::Win32::System::Threading::{
            CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, GetCurrentProcess,
            OpenProcessToken, PROCESS_INFORMATION, STARTUPINFOW,
        };
        use windows::core::{HSTRING, PWSTR};
        let exe = std::env::current_exe().unwrap();
        let mut vars: Vec<(std::ffi::OsString, std::ffi::OsString)> = std::env::vars_os().collect();
        vars.push((PROBE_ROLE.into(), "holder".into()));
        vars.push((PROBE_OUT.into(), out.clone().into_os_string()));
        vars.push((PROBE_PIPE.into(), pipe.into()));
        vars.push((PROBE_HANDED.into(), handed.as_os_str().to_owned()));
        let mut block: Vec<u16> = Vec::new();
        for (k, v) in &vars {
            block.extend(k.encode_wide());
            block.push(u16::from(b'='));
            block.extend(v.encode_wide());
            block.push(0);
        }
        block.push(0);
        let mut command: Vec<u16> = format!(
            "\"{}\" probe --exact --ignored --nocapture --test-threads=1",
            exe.display()
        )
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
        // SAFETY: every handle is this process's own and closed once; every buffer outlives the
        // call that reads it; the SID from ConvertStringSidToSidW is freed once.
        unsafe {
            let mut token = HANDLE::default();
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ADJUST_DEFAULT | TOKEN_ASSIGN_PRIMARY,
                &mut token,
            )
            .expect("this process's token");
            let mut low = HANDLE::default();
            DuplicateTokenEx(
                token,
                TOKEN_ACCESS_MASK(0x0200_0000),
                None,
                SecurityImpersonation,
                TokenPrimary,
                &mut low,
            )
            .expect("a copy of the token");
            let mut sid = PSID::default();
            ConvertStringSidToSidW(&HSTRING::from("S-1-16-4096"), &mut sid).expect("the Low SID");
            let label = TOKEN_MANDATORY_LABEL {
                Label: SID_AND_ATTRIBUTES {
                    Sid: sid,
                    // SE_GROUP_INTEGRITY.
                    Attributes: 0x20,
                },
            };
            SetTokenInformation(
                low,
                TokenIntegrityLevel,
                (&raw const label).cast(),
                u32::try_from(size_of::<TOKEN_MANDATORY_LABEL>()).unwrap() + 12,
            )
            .expect("the Low label");
            let startup = STARTUPINFOW {
                cb: u32::try_from(size_of::<STARTUPINFOW>()).unwrap(),
                ..Default::default()
            };
            let mut info = PROCESS_INFORMATION::default();
            CreateProcessAsUserW(
                Some(low),
                &HSTRING::from(exe.as_os_str()),
                Some(PWSTR(command.as_mut_ptr())),
                None,
                None,
                false,
                CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
                Some(block.as_ptr().cast()),
                None,
                &startup,
                &mut info,
            )
            .expect("the Low process");
            let _ = CloseHandle(info.hThread);
            let _ = CloseHandle(low);
            let _ = CloseHandle(token);
            let _ = LocalFree(Some(HLOCAL(sid.0)));
            LowHolder {
                process: info.hProcess,
                out,
            }
        }
    }

    fn pid(&self) -> u32 {
        // SAFETY: the process handle is this struct's own and open.
        unsafe { windows::Win32::System::Threading::GetProcessId(self.process) }
    }

    fn lines(&self) -> Vec<String> {
        read_lines(&self.out)
    }
}

impl Drop for LowHolder {
    fn drop(&mut self) {
        use windows::Win32::System::Threading::{TerminateProcess, WaitForSingleObject};
        // SAFETY: the process handle is this struct's own, closed once.
        unsafe {
            let _ = TerminateProcess(self.process, 1);
            let _ = WaitForSingleObject(self.process, 5000);
            let _ = CloseHandle(self.process);
        }
    }
}

/// A `creator` probe, once its pipe's handle is the holder's.
fn hand_pipe(dir: &Path, pipe: &str, holder: u32, handed: &Path) -> Probe {
    let holder = holder.to_string();
    let creator = Probe::start(
        "creator",
        dir.join("creator.txt"),
        &[
            (PROBE_PIPE, std::ffi::OsStr::new(pipe)),
            (PROBE_HOLDER, std::ffi::OsStr::new(&holder)),
            (PROBE_HANDED, handed.as_os_str()),
        ],
    );
    wait_line(|| creator.lines(), "handed");
    creator
}

/// A client that trusts the folder of this test binary, the creator's, and starts no core.
fn client_config(env: &TestEnv) -> ClientConfig {
    let mut cfg = ClientConfig::for_env(env.env.clone(), CallerKind::Cli).unwrap();
    cfg.start = StartPolicy::NoStart;
    cfg.start_timeout = Duration::from_secs(5);
    cfg
}

fn got(lines: &[String]) -> Vec<Value> {
    lines
        .iter()
        .filter_map(|l| l.strip_prefix("got "))
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test] // spec: ARCH-027, ARCH-029
fn a_low_holder_of_the_core_pipe_fails_the_proof_and_reads_nothing_more() {
    let env = TestEnv::new();
    let low_dir = tempfile::tempdir().unwrap();
    label_low(low_dir.path());
    let handed = low_dir.path().join("handed");
    let pipe = env.env.pipe_name();
    let holder = LowHolder::start(low_dir.path().join("holder.txt"), &pipe, &handed);
    wait_line(|| holder.lines(), "ready");
    let _creator = hand_pipe(low_dir.path(), &pipe, holder.pid(), &handed);
    wait_line(|| holder.lines(), "listening");
    let cfg = client_config(&env);

    // `Client::connect` is `greet`, and `send_deny` goes through it with its token; a stop is
    // `drain_and_wait`.
    let greeted = BlockingClient::connect(&cfg);
    let stopped = drain(&cfg, Duration::from_secs(5));
    let lines = wait_clients(|| holder.lines(), 2);

    let requests = got(&lines);
    assert_eq!(
        requests.len(),
        2,
        "a proof from each, nothing else: {lines:?}"
    );
    for request in &requests {
        assert_eq!(request["method"], "core/prove", "{request}");
        // Nothing about the client in what it is sent: no workspace, no app name, no version.
        assert_eq!(request.as_object().unwrap().len(), 4, "{request}");
        assert_eq!(request["params"].as_object().unwrap().len(), 1, "{request}");
    }
    for result in [greeted.map(|_| ()), stopped.map(|_| ())] {
        match result {
            Err(ClientError::Untrusted { reason, .. }) => {
                assert!(reason.contains("prove"), "{reason}");
            }
            other => panic!("expected Untrusted, got {other:?}"),
        }
    }
    // It couldn't set the event, by either way it has: no write up (5: access denied), and the
    // client's token is lent to it for identifying only. Nor can it reach the core's pipe to have
    // a real core set it.
    let tries = |prefix: &str| -> Vec<String> {
        lines
            .iter()
            .filter_map(|l| l.strip_prefix(prefix).map(str::to_string))
            .collect()
    };
    assert_eq!(tries("proof-open "), ["5", "5"], "{lines:?}");
    let impersonated = tries("proof-impersonated ");
    assert_eq!(impersonated.len(), 2, "{lines:?}");
    assert!(!impersonated.iter().any(|t| t == "ok"), "{lines:?}");
    assert_eq!(tries("relay-open "), ["5"], "{lines:?}");
}

#[test] // spec: ARCH-027
fn a_holder_at_the_clients_own_level_passes_the_proof_outside_the_threat_model() {
    // The same run with the holder at Medium: it can set the event, so it gets the hello. ARCH-027
    // leaves a same-user process at this level out of the model; this also shows the Low run above
    // would see the hello if the proof didn't stop it.
    let env = TestEnv::new();
    let dir = tempfile::tempdir().unwrap();
    let handed = dir.path().join("handed");
    let pipe = env.env.pipe_name();
    let holder = Probe::start(
        "holder",
        dir.path().join("holder.txt"),
        &[
            (PROBE_PIPE, std::ffi::OsStr::new(&pipe)),
            (PROBE_HANDED, handed.as_os_str()),
        ],
    );
    wait_line(|| holder.lines(), "ready");
    let _creator = hand_pipe(dir.path(), &pipe, holder.child.id(), &handed);
    wait_line(|| holder.lines(), "listening");
    let cfg = client_config(&env);

    let greeted = BlockingClient::connect(&cfg);
    assert!(
        matches!(greeted, Err(ClientError::Protocol(_))),
        "{greeted:?}"
    );
    let stopped = drain(&cfg, Duration::from_secs(5));
    assert!(
        matches!(stopped, Err(ClientError::Protocol(_))),
        "{stopped:?}"
    );

    // At this level it reaches the pipe by its name too: that was its first client.
    let lines = wait_clients(|| holder.lines(), 3);
    assert!(lines.iter().any(|l| l == "relay-open ok"), "{lines:?}");
    assert!(lines.iter().any(|l| l == "proof-open ok"), "{lines:?}");
    let methods: Vec<Value> = got(&lines).iter().map(|r| r["method"].clone()).collect();
    assert_eq!(
        methods,
        ["core/prove", "core/hello", "core/prove", "core/hello"],
        "{lines:?}"
    );
}
