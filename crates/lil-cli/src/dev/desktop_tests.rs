//! `lil dev desktop-tests`: the desktop group (QA-042, QA-073; P4a-R29, P4a-R31, P4a-R35,
//! P4a-R39).
//!
//! The desktop group is the Rust tests named `desktop_*` and marked `#[ignore = "desktop"]`. Each
//! starts with `lil_platform::guard::require_desktop_group`, so it runs only where this command
//! puts it:
//!
//! - **In Windows Sandbox** (the default, `--suite desktop`): the command builds a bundle in
//!   `build\sandbox\in` (a nextest archive of the desktop packages, the tracked source files for
//!   `--workspace-remap`, the test kit's binaries, the host's `cargo-nextest.exe`,
//!   `tools/sandbox/nextest-sandbox.toml`, the three VC++ runtime DLLs and
//!   `tools/sandbox/run-inside.ps1`), writes `build\sandbox\lilith-desktop.wsb` from
//!   `tools/sandbox/lilith-desktop.wsb.in`, starts the Sandbox, waits for `started.json` and
//!   `done.json` in `build\sandbox\out`, reads the JUnit files, copies them to
//!   `build\sandbox\results` and closes the Sandbox and its viewer. The bundle and the deploy
//!   folder are mapped read-only and only the results folder is writable, so nothing in the
//!   Sandbox can write to the source or build tree. The host set (below) is left out there:
//!   every token in the Sandbox is elevated (Task 1's S1.9), and the suite never runs elevated.
//! - **`--suite qml` / `--suite grabs`** (P4a-R35, only if Task 0's S0.9 failed): the CTest
//!   commands of that preset, exported with `ctest --show-only=json-v1` and run on the
//!   Sandbox's own screen, with the build tree and the Qt pair's folder mapped read-only at
//!   their host paths.
//! - **`--host`** (P4a-R39): the host set, P4a-R39's four toast and foreground tests, on this
//!   PC: refused when elevated; announced by the hands-off pop-up (`tools/sandbox/hands-off.ps1`:
//!   topmost, click-through, never takes the focus, at least 10 s of warning, then a countdown
//!   and "done"); run with `LIL_DESKTOP_HOST=1`; then every desktop test channel's shortcut,
//!   registry keys, CLSID and toasts are checked gone, and swept by their fixed name prefix
//!   (`lil_platform::desktop::DESKTOP_TEST_ID_PREFIX`) if not.
//!
//! Exit codes: 0 every test passed, 1 a test failed (or something was left behind), 2 the run
//! couldn't happen. Everything that touches the PC goes through [`SandboxLauncher`] and
//! [`HostOps`], and every wait through [`Clock`], so the unit tests below use fakes, with no
//! Sandbox and no real time.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::spec_tests::parse_junit;

/// P4a-R39's host set: the toast and foreground tests, which run on the PC (`--host`) and never
/// in the Sandbox.
pub const HOST_SET: [&str; 4] = [
    "desktop_toast_posts_under_the_shortcut_aumid",
    "desktop_toasts_state_is_ready_on_this_pc",
    "desktop_second_launch_comes_to_front",
    "desktop_toast_url_brings_the_confirm_window_to_front",
];

/// Where the bundle is mapped inside the Sandbox (read-only).
pub const SANDBOX_IN: &str = r"C:\lil\in";
/// Where the deploy folder is mapped inside the Sandbox (read-only).
pub const SANDBOX_DEPLOY: &str = r"C:\lil\deploy";
/// Where the results folder is mapped inside the Sandbox (the only writable mapping).
pub const SANDBOX_OUT: &str = r"C:\lil\out";

/// How long the Sandbox has to write `started.json` (S1.9 saw 11–12 s).
pub const START_TIMEOUT: Duration = Duration::from_secs(120);
/// `--timeout-mins`' default: the whole run, from the start to `done.json`.
pub const DEFAULT_TIMEOUT_MINS: u64 = 20;
/// How often the results folder is looked at.
const POLL: Duration = Duration::from_secs(2);
/// The hands-off pop-up's warning before the host run starts (owner direction: at least 10 s).
pub const HANDS_OFF_WARNING: Duration = Duration::from_secs(10);
/// The pop-up's countdown: this much per host test, plus [`HOST_OVERHEAD_SECS`]. An estimate;
/// the pop-up says "almost done" once it has passed.
const HOST_SECS_PER_TEST: u64 = 30;
const HOST_OVERHEAD_SECS: u64 = 30;
/// The worst case per test: the desktop profile's `slow-timeout` (60 s × `terminate-after = 5`).
/// The pop-up's cap is this per test, plus [`HOST_CAP_OVERHEAD_SECS`].
const HOST_WORST_SECS_PER_TEST: u64 = 300;
const HOST_CAP_OVERHEAD_SECS: u64 = 120;
/// What the pop-up says is happening.
const HANDS_OFF_WHAT: &str = "runs its toast and foreground tests";

/// The template, the script inside, the desktop profile and the pop-up, as committed under
/// `tools/sandbox/`.
const WSB_TEMPLATE: &str = include_str!("../../../../tools/sandbox/lilith-desktop.wsb.in");
const RUN_INSIDE: &str = include_str!("../../../../tools/sandbox/run-inside.ps1");
const NEXTEST_SANDBOX: &str = include_str!("../../../../tools/sandbox/nextest-sandbox.toml");
const HANDS_OFF_SCRIPT: &str = include_str!("../../../../tools/sandbox/hands-off.ps1");
/// The desktop profile's file, relative to the repository (the host run's `--config-file`).
const NEXTEST_SANDBOX_PATH: &str = r"tools\sandbox\nextest-sandbox.toml";

/// The packages whose tests make up the desktop group. `lil-qt` joins when it exists (Task 15).
const DESKTOP_PACKAGES: [&str; 2] = ["lil-platform", "lil-qt"];

/// The CTest test that runs `cargo test` and isn't a window test: never bundled.
const CTEST_E2E: &str = "lil_qt_e2e";

/// The VC++ runtime DLLs the Rust test binaries need (OPS-094).
const VC_RUNTIME: [&str; 3] = ["vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll"];

/// The owner's one-time step when there is no Windows Sandbox (QA-042).
pub const OWNER_STEP: &str = "Windows Sandbox isn't turned on. It's a one-time step for you: \
    Settings > System > Optional features > More Windows features > Windows Sandbox (it needs \
    virtualization on in the firmware), then restart, and run this again.";
/// Why a run that Ctrl+C stopped ends.
pub const INTERRUPTED: &str = "interrupted (Ctrl+C)";
/// The refusal while another Sandbox runs.
pub const ONE_AT_A_TIME: &str =
    "Only one Windows Sandbox can run at a time; close the open one, then run this again.";

// ---------------------------------------------------------------------------------------------
// The filters.
// ---------------------------------------------------------------------------------------------

/// The host set as a nextest filterset: `test(=a) | test(=b) | …`.
pub fn host_filter() -> String {
    HOST_SET
        .iter()
        .map(|t| format!("test(={t})"))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// What runs in the Sandbox: every `desktop_*` test but the host set. run-inside.ps1 holds it
/// as a literal; the unit tests check that it is this.
#[cfg(test)]
fn sandbox_filter() -> String {
    format!("test(/^desktop_/) - ({})", host_filter())
}

// ---------------------------------------------------------------------------------------------
// The interfaces.
// ---------------------------------------------------------------------------------------------

/// Whether Windows Sandbox can start here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// The Store version: `wsb.exe` answers `wsb list`.
    Cli,
    /// The inbox version: `WindowsSandbox.exe` is present.
    Inbox,
    Missing,
}

/// A Sandbox this run started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxHandle {
    pub via: Availability,
    /// The id `wsb start` printed ([`Availability::Cli`]).
    pub id: Option<String>,
    /// The pids of the Sandbox processes that ran before the start: never this run's.
    pub pids_before: Vec<u32>,
    /// The processes this run started itself (`wsb connect`, or `WindowsSandbox.exe`), pinned at
    /// their start.
    pub started: Vec<u32>,
}

/// Starts and stops Windows Sandbox.
pub trait SandboxLauncher {
    fn availability(&self) -> Availability;
    /// Whether a Sandbox runs: `wsb list`, else a process with one of the names S1.9 recorded.
    /// The run's own lingering `wsb connect` client never counts.
    fn running(&self) -> bool;
    /// `wsb start --config "<the .wsb's XML text>"` and `wsb connect` (the logon command needs a
    /// user session), else `WindowsSandbox.exe <file>`.
    fn start(&self, wsb: &Path) -> io::Result<SandboxHandle>;
    /// `wsb stop --id` (else ends the started processes) unless the Sandbox has shut itself
    /// down; then closes the run's own viewer client (`WindowsSandboxRemoteSession`), which stays
    /// open after the VM stops (S1.9).
    fn stop(&self, h: &SandboxHandle) -> io::Result<()>;
}

/// The two clocks of a run: how long it has waited, and the wait itself.
pub trait Clock {
    fn now(&self) -> Duration;
    fn sleep(&self, d: Duration);
}

/// The real clock.
#[derive(Debug)]
pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    pub fn new() -> SystemClock {
        SystemClock {
            start: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        SystemClock::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.start.elapsed()
    }
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// Which tests run, and where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suite {
    /// The Rust desktop group, in the Sandbox.
    Desktop,
    /// CTest's `qml` preset, in the Sandbox (P4a-R35).
    Qml,
    /// CTest's `grabs` preset, in the Sandbox (P4a-R35).
    Grabs,
    /// The toast and foreground tests on your PC, no Sandbox (P4a-R39).
    Host,
}

impl Suite {
    fn name(self) -> &'static str {
        match self {
            Suite::Desktop => "desktop",
            Suite::Qml => "qml",
            Suite::Grabs => "grabs",
            Suite::Host => "host",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunConfig {
    pub suite: Suite,
    /// The whole run, from the start to `done.json`.
    pub timeout: Duration,
    /// For `started.json` ([`START_TIMEOUT`]).
    pub start_timeout: Duration,
    /// Leave the Sandbox open afterwards (run-inside.ps1 doesn't shut it down either).
    pub keep_open: bool,
}

/// One test's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestResult {
    /// `<classname>::<name>`, as the JUnit file names it.
    pub name: String,
    pub passed: bool,
}

/// What a run found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub tests: Vec<TestResult>,
    /// Anything else that fails the run: a non-zero exit with no failed test, a test channel's
    /// leftovers.
    pub problems: Vec<String>,
    /// The `cargo-nextest` that ran the tests.
    pub nextest_version: Option<String>,
    /// Where the results were copied.
    pub results: Option<PathBuf>,
}

impl Summary {
    pub fn failed(&self) -> usize {
        self.tests.iter().filter(|t| !t.passed).count()
    }

    pub fn passed(&self) -> usize {
        self.tests.len() - self.failed()
    }

    /// 0 when every test passed and nothing else went wrong, else 1.
    pub fn exit_code(&self) -> i32 {
        if self.failed() == 0 && self.problems.is_empty() && !self.tests.is_empty() {
            0
        } else {
            1
        }
    }
}

/// Why the run couldn't happen (exit 2), in plain words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunError(pub String);

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn fail<T>(message: impl Into<String>) -> Result<T, RunError> {
    Err(RunError(message.into()))
}

/// The process exit code of a run: 0, 1 or 2.
pub fn exit_code(result: &Result<Summary, RunError>) -> i32 {
    match result {
        Ok(s) => s.exit_code(),
        Err(_) => 2,
    }
}

/// A command the run starts on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub env_remove: Vec<String>,
}

impl Cmd {
    fn new(program: impl Into<PathBuf>, cwd: &Path, args: &[&str]) -> Cmd {
        Cmd {
            program: program.into(),
            args: args.iter().map(|a| (*a).to_string()).collect(),
            cwd: cwd.to_path_buf(),
            env: Vec::new(),
            env_remove: Vec::new(),
        }
    }

    fn arg(mut self, a: impl Into<String>) -> Cmd {
        self.args.push(a.into());
        self
    }

    /// The program's file name without `.exe`, for matching.
    #[cfg(test)]
    fn program_name(&self) -> String {
        let name = self
            .program
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        name.strip_suffix(".exe").unwrap_or(&name).to_string()
    }

    /// The command line as it would be typed, for messages and tests.
    pub fn line(&self) -> String {
        std::iter::once(self.program.display().to_string())
            .chain(self.args.iter().map(|a| {
                if a.contains(' ') || a.is_empty() {
                    format!("'{a}'")
                } else {
                    a.clone()
                }
            }))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// One `<MappedFolder>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapped {
    pub host: PathBuf,
    pub sandbox: String,
    pub read_only: bool,
}

/// What the host sweep removed, and what is still there.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Swept {
    pub removed: Vec<String>,
    pub remaining: Vec<String>,
}

/// The hands-off pop-up, on screen.
pub trait HandsOff {
    /// Shows "done" and lets the pop-up close.
    fn done(&self);
    /// `Ok` while the pop-up is still on screen; why it isn't, otherwise.
    fn check(&self) -> Result<(), String>;
}

/// Everything else the run does on the host.
pub trait HostOps {
    /// Runs `cmd` with its output on the console; its exit code.
    fn run(&self, cmd: &Cmd) -> io::Result<i32>;
    /// Runs `cmd` and captures its standard output (its errors go to the console).
    fn output(&self, cmd: &Cmd) -> io::Result<(i32, String)>;
    /// A program on `PATH` (`cargo-nextest`).
    fn find_program(&self, name: &str) -> Option<PathBuf>;
    /// The three VC++ runtime DLLs ([`VC_RUNTIME`]), from the MSVC redist folder.
    fn vc_runtime(&self) -> Result<Vec<PathBuf>, String>;
    /// The host folders that SYSTEM can't read (or, when mapped writable, can't change): the
    /// Sandbox's shares run as SYSTEM.
    fn system_cannot_use(&self, mapped: &[Mapped]) -> io::Result<Vec<PathBuf>>;
    /// Whether this process runs elevated.
    fn elevated(&self) -> bool;
    /// Shows the hands-off pop-up and returns once it is on screen: `warning` of "in N s", then
    /// a countdown of `run`; it stays up at most `cap` after the countdown's start.
    fn show_hands_off(
        &self,
        what: &str,
        warning: Duration,
        run: Duration,
        cap: Duration,
    ) -> io::Result<Box<dyn HandsOff>>;
    /// Whether the process `pid` runs (the host-run lock's owner).
    fn process_alive(&self, pid: u32) -> bool;
    /// What desktop test channels left on this PC: shortcuts, registry keys, CLSIDs, toasts.
    fn desktop_leftovers(&self) -> io::Result<Vec<String>>;
    /// Removes what [`HostOps::desktop_leftovers`] finds, through the test-only removers.
    fn sweep_desktop_leftovers(&self) -> io::Result<Swept>;
    /// `LIL_QT_DIR`: the Qt pair's folder.
    fn qt_dir(&self) -> Option<PathBuf>;
    /// Whether Ctrl+C or Ctrl+Break came: the run then stops its Sandbox, or cleans up after
    /// the host run, before it exits.
    fn interrupted(&self) -> bool;
}

// ---------------------------------------------------------------------------------------------
// The folders.
// ---------------------------------------------------------------------------------------------

/// The run's folders, all under the repository's ignored `build\`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub repo: PathBuf,
    /// `build\sandbox`
    pub root: PathBuf,
    /// `build\sandbox\in`: the bundle.
    pub in_dir: PathBuf,
    /// `build\sandbox\out`: the Sandbox's results.
    pub out_dir: PathBuf,
    /// `build\sandbox\results`: the results kept after the run.
    pub results: PathBuf,
    /// `build\sandbox\lilith-desktop.wsb`: it holds absolute local paths, so it is never
    /// committed.
    pub wsb: PathBuf,
    /// `build\dev`: the CMake build tree (Task 15).
    pub build_tree: PathBuf,
    /// `build\dev\deploy`: the deploy folder (Task 15's `cmake --build --preset dev --target
    /// deploy`).
    pub deploy: PathBuf,
}

impl Layout {
    pub fn new(repo: &Path) -> Layout {
        let root = repo.join("build").join("sandbox");
        let build_tree = repo.join("build").join("dev");
        Layout {
            repo: repo.to_path_buf(),
            in_dir: root.join("in"),
            out_dir: root.join("out"),
            results: root.join("results"),
            wsb: root.join("lilith-desktop.wsb"),
            deploy: build_tree.join("deploy"),
            build_tree,
            root,
        }
    }
}

/// Removes `dir` and makes it again, empty.
fn reset_dir(dir: &Path) -> Result<(), RunError> {
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            return fail(format!(
                "couldn't empty {} (is a file in it open?): {e}",
                dir.display()
            ));
        }
        _ => {}
    }
    std::fs::create_dir_all(dir)
        .or_else(|e| fail(format!("couldn't create {}: {e}", dir.display())))
}

fn write(path: &Path, text: &str) -> Result<(), RunError> {
    std::fs::write(path, text).or_else(|e| fail(format!("couldn't write {}: {e}", path.display())))
}

fn copy(from: &Path, to: &Path) -> Result<(), RunError> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .or_else(|e| fail(format!("couldn't create {}: {e}", parent.display())))?;
    }
    std::fs::copy(from, to)
        .map(|_| ())
        .or_else(|e| fail(format!("couldn't copy {}: {e}", from.display())))
}

/// Copies everything below `from` into `to`.
fn copy_tree(from: &Path, to: &Path) -> Result<(), RunError> {
    let entries = std::fs::read_dir(from)
        .or_else(|e| fail(format!("couldn't read {}: {e}", from.display())))?;
    for entry in entries {
        let entry = entry.or_else(|e| fail(format!("couldn't read {}: {e}", from.display())))?;
        let target = to.join(entry.file_name());
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            copy_tree(&entry.path(), &target)?;
        } else {
            copy(&entry.path(), &target)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The .wsb.
// ---------------------------------------------------------------------------------------------

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// The `.wsb` from the template, with `mapped` as its folders.
pub fn wsb_xml(mapped: &[Mapped]) -> String {
    let folders: String = mapped
        .iter()
        .map(|m| {
            format!(
                "    <MappedFolder>\r\n      <HostFolder>{}</HostFolder>\r\n      \
                 <SandboxFolder>{}</SandboxFolder>\r\n      <ReadOnly>{}</ReadOnly>\r\n    \
                 </MappedFolder>",
                xml_escape(&m.host.display().to_string()),
                xml_escape(&m.sandbox),
                m.read_only
            )
        })
        .collect::<Vec<_>>()
        .join("\r\n");
    WSB_TEMPLATE
        .replace("\r\n", "\n")
        .replace('\n', "\r\n")
        .replace("@MAPPED_FOLDERS@", &folders)
}

/// The `.wsb`'s text on one line, for `wsb start --config`: the line breaks and the indentation
/// between the elements go (no text in it spans lines).
pub fn wsb_one_line(xml: &str) -> String {
    xml.lines().map(str::trim).collect()
}

// ---------------------------------------------------------------------------------------------
// The run.
// ---------------------------------------------------------------------------------------------

/// Runs `cfg.suite` for the checkout at `repo` on this PC.
pub fn run(
    repo: &Path,
    cfg: &RunConfig,
    launcher: &dyn SandboxLauncher,
    clock: &dyn Clock,
) -> Result<Summary, RunError> {
    run_with(repo, cfg, &RealHost::new(repo), launcher, clock)
}

/// [`run`] with the host's operations given.
pub fn run_with(
    repo: &Path,
    cfg: &RunConfig,
    host: &dyn HostOps,
    launcher: &dyn SandboxLauncher,
    clock: &dyn Clock,
) -> Result<Summary, RunError> {
    let layout = Layout::new(repo);
    match cfg.suite {
        Suite::Host => run_host(&layout, host, clock),
        _ => run_sandbox(&layout, cfg, host, launcher, clock),
    }
}

/// The desktop packages that exist in this checkout.
fn desktop_packages(repo: &Path) -> Vec<&'static str> {
    DESKTOP_PACKAGES
        .into_iter()
        .filter(|p| repo.join("crates").join(p).join("Cargo.toml").is_file())
        .collect()
}

fn package_args(packages: &[&str]) -> Vec<String> {
    packages
        .iter()
        .flat_map(|p| ["-p".to_string(), (*p).to_string()])
        .collect()
}

/// `cargo nextest archive -p <each desktop package> --archive-file <file>`. Default features:
/// the desktop tests drive the deployed app and need no `qt` feature. The archive's `-E` takes
/// binary-level filtersets only, so it filters nothing: the run inside does.
pub fn archive_command(repo: &Path, archive: &Path) -> Cmd {
    let mut cmd = Cmd::new("cargo", repo, &["nextest", "archive"]);
    cmd.args.extend(package_args(&desktop_packages(repo)));
    cmd.arg("--archive-file").arg(archive.display().to_string())
}

/// The nextest setup script's build of the test binaries (`.config/nextest.toml`,
/// `build-testbins`).
pub fn testbins_command(repo: &Path) -> Cmd {
    Cmd::new(
        "cargo",
        repo,
        &[
            "build",
            "--locked",
            "--offline",
            "--bins",
            "-p",
            "lil-cli",
            "-p",
            "lil-core",
            "-p",
            "lil-mcp",
            "-p",
            "lil-testkit",
            "--target-dir",
            "target/testbins",
        ],
    )
}

/// The host run's nextest command after `verb` (`list` or `run`): the host set from the desktop
/// packages, ignored tests only, the desktop profile from `tools/sandbox/nextest-sandbox.toml`.
fn host_nextest(repo: &Path, verb: &str) -> Cmd {
    let mut cmd = Cmd::new("cargo", repo, &["nextest", verb]);
    cmd.args.extend(package_args(&desktop_packages(repo)));
    for a in [
        "--run-ignored",
        "only",
        "--config-file",
        NEXTEST_SANDBOX_PATH,
        "--profile",
        "desktop",
    ] {
        cmd.args.push(a.to_string());
    }
    cmd.arg("-E").arg(host_filter())
}

fn run_sandbox(
    l: &Layout,
    cfg: &RunConfig,
    host: &dyn HostOps,
    launcher: &dyn SandboxLauncher,
    clock: &dyn Clock,
) -> Result<Summary, RunError> {
    if launcher.availability() == Availability::Missing {
        return fail(OWNER_STEP);
    }
    if launcher.running() {
        return fail(ONE_AT_A_TIME);
    }
    let mapped = build_bundle(l, cfg, host)?;
    // The results folder must exist before the start, and nothing an earlier run left in it may
    // be read as this run's.
    reset_dir(&l.out_dir)?;
    let refused = host
        .system_cannot_use(&mapped)
        .or_else(|e| fail(format!("couldn't read the folders' permissions: {e}")))?;
    if !refused.is_empty() {
        let names: Vec<String> = refused.iter().map(|p| p.display().to_string()).collect();
        return fail(format!(
            "Windows Sandbox's shared folders are read as SYSTEM, which can't use {}. Give SYSTEM \
             read access to them ({} also needs write), then run this again.",
            names.join(", "),
            l.out_dir.display()
        ));
    }
    write(&l.wsb, &wsb_xml(&mapped))?;
    if host.interrupted() {
        return fail(INTERRUPTED);
    }
    println!("Starting Windows Sandbox ({}).", l.wsb.display());
    println!(
        "Keep the PC unlocked until the run is done; the Sandbox's window may stay in the \
         background."
    );
    let handle = launcher
        .start(&l.wsb)
        .or_else(|e| fail(format!("couldn't start Windows Sandbox: {e}")))?;
    // From here every way out, a panic included, closes the Sandbox and its viewer, except
    // --keep-open without Ctrl+C.
    let mut guard = StopOnDrop {
        launcher,
        handle,
        armed: true,
    };
    let outcome = wait_and_collect(l, cfg, host, clock);
    if cfg.keep_open && !host.interrupted() {
        guard.armed = false;
        println!("--keep-open: the Sandbox stays open; close its window when you are done.");
    }
    drop(guard);
    outcome
}

/// Stops the Sandbox it holds when dropped, unless disarmed.
struct StopOnDrop<'a> {
    launcher: &'a dyn SandboxLauncher,
    handle: SandboxHandle,
    armed: bool,
}

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        if self.armed
            && let Err(e) = self.launcher.stop(&self.handle)
        {
            eprintln!(
                "warning: couldn't close the Sandbox or its window ({e}); close the Windows \
                 Sandbox window yourself."
            );
        }
    }
}

/// The bundle in `build\sandbox\in`, and the folders the `.wsb` maps.
fn build_bundle(l: &Layout, cfg: &RunConfig, host: &dyn HostOps) -> Result<Vec<Mapped>, RunError> {
    reset_dir(&l.in_dir)?;
    std::fs::create_dir_all(&l.root)
        .or_else(|e| fail(format!("couldn't create {}: {e}", l.root.display())))?;
    let dir = &l.in_dir;
    write(
        &dir.join("in-sandbox.marker"),
        "LILITH desktop-tests bundle: run-inside.ps1 runs only where this is mapped.\r\n",
    )?;
    write(&dir.join("run-inside.ps1"), RUN_INSIDE)?;
    let dlls = host
        .vc_runtime()
        .or_else(|e| fail(format!("couldn't find the VC++ runtime DLLs: {e}")))?;
    for dll in &dlls {
        let name = dll.file_name().unwrap_or_default();
        copy(dll, &dir.join("vcrt").join(name))?;
    }
    let mut qt_dir = None;
    match cfg.suite {
        Suite::Desktop | Suite::Host => bundle_desktop(l, host)?,
        Suite::Qml | Suite::Grabs => qt_dir = Some(bundle_ctest(l, cfg.suite, host)?),
    }
    let config = serde_json::json!({
        "suite": cfg.suite.name(),
        "keep_open": cfg.keep_open,
        "qt_dir": qt_dir.as_ref().map(|q: &PathBuf| q.display().to_string()),
    });
    write(&dir.join("config.json"), &config.to_string())?;

    let mut mapped = vec![Mapped {
        host: l.in_dir.clone(),
        sandbox: SANDBOX_IN.to_string(),
        read_only: true,
    }];
    if l.deploy.is_dir() {
        mapped.push(Mapped {
            host: l.deploy.clone(),
            sandbox: SANDBOX_DEPLOY.to_string(),
            read_only: true,
        });
    } else {
        println!(
            "note: no deploy folder at {} (Task 15's `cmake --build --preset dev --target \
             deploy`), so nothing is mapped at {SANDBOX_DEPLOY}; tests that start the app fail.",
            l.deploy.display()
        );
    }
    if let Some(qt) = qt_dir {
        for folder in [l.build_tree.clone(), qt] {
            mapped.push(Mapped {
                sandbox: folder.display().to_string(),
                host: folder,
                read_only: true,
            });
        }
    }
    mapped.push(Mapped {
        host: l.out_dir.clone(),
        sandbox: SANDBOX_OUT.to_string(),
        read_only: false,
    });
    Ok(mapped)
}

fn bundle_desktop(l: &Layout, host: &dyn HostOps) -> Result<(), RunError> {
    let dir = &l.in_dir;
    println!(
        "Building the bundle in {} (about 10 minutes the first time, 1 to 2 after).",
        dir.display()
    );
    let archive = archive_command(&l.repo, &dir.join("desktop.tar.zst"));
    expect_success(host, &archive, "building the nextest archive")?;
    let testbins = testbins_command(&l.repo);
    expect_success(host, &testbins, "building the test binaries")?;
    let built = l.repo.join("target").join("testbins").join("debug");
    let entries = std::fs::read_dir(&built)
        .or_else(|e| fail(format!("couldn't read {}: {e}", built.display())))?;
    // The programs only: their .pdb files would make the bundle about 800 MB.
    for entry in entries.flatten() {
        let path = entry.path();
        let program = path
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("exe") || x.eq_ignore_ascii_case("dll"));
        if program && entry.file_type().is_ok_and(|t| t.is_file()) {
            copy(&entry.path(), &dir.join("testbins").join(entry.file_name()))?;
        }
    }
    // The same cargo-nextest builds the archive and runs it inside.
    let Some(nextest) = host.find_program("cargo-nextest") else {
        return fail("cargo-nextest isn't on PATH (cargo install cargo-nextest --locked)");
    };
    copy(&nextest, &dir.join("cargo-nextest.exe"))?;
    write(&dir.join("nextest-sandbox.toml"), NEXTEST_SANDBOX)?;
    // The tracked files only, for --workspace-remap.
    let ls = Cmd::new("git", &l.repo, &["ls-files", "-z"]);
    let (code, files) = host
        .output(&ls)
        .or_else(|e| fail(format!("couldn't run git ls-files: {e}")))?;
    if code != 0 {
        return fail(format!("git ls-files exited {code}"));
    }
    let src = dir.join("src");
    for file in files.split('\0').filter(|f| !f.is_empty()) {
        let from = l.repo.join(file);
        // A tracked file deleted in the working tree is left out.
        if from.is_file() {
            copy(&from, &src.join(file))?;
        }
    }
    Ok(())
}

/// The CTest commands of `suite`'s preset, without [`CTEST_E2E`]; the Qt pair's folder.
fn bundle_ctest(l: &Layout, suite: Suite, host: &dyn HostOps) -> Result<PathBuf, RunError> {
    let Some(qt) = host.qt_dir() else {
        return fail("LIL_QT_DIR isn't set: it names the Qt pair's folder (Task 15).");
    };
    if !l.build_tree.is_dir() {
        return fail(format!(
            "no CMake build tree at {}: build it first (cmake --preset dev, then cmake --build \
             --preset dev).",
            l.build_tree.display()
        ));
    }
    let preset = suite.name();
    let cmd = Cmd::new(
        "ctest",
        &l.repo,
        &["--preset", preset, "--show-only=json-v1"],
    );
    let (code, json) = host
        .output(&cmd)
        .or_else(|e| fail(format!("couldn't run ctest: {e}")))?;
    if code != 0 {
        return fail(format!("`{}` exited {code}", cmd.line()));
    }
    let filtered = without_e2e(&json).or_else(|e| fail(format!("ctest's JSON: {e}")))?;
    write(&l.in_dir.join("ctest.json"), &filtered)?;
    Ok(qt)
}

/// `ctest --show-only=json-v1`'s document without [`CTEST_E2E`].
pub fn without_e2e(json: &str) -> Result<String, String> {
    let mut doc: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let Some(tests) = doc.get_mut("tests").and_then(|t| t.as_array_mut()) else {
        return Err("no `tests` array".to_string());
    };
    tests.retain(|t| t.get("name").and_then(|n| n.as_str()) != Some(CTEST_E2E));
    Ok(doc.to_string())
}

fn expect_success(host: &dyn HostOps, cmd: &Cmd, what: &str) -> Result<(), RunError> {
    match host.run(cmd) {
        Ok(0) => Ok(()),
        Ok(code) => fail(format!("{what} failed: `{}` exited {code}", cmd.line())),
        Err(e) => fail(format!("{what} failed: couldn't run `{}`: {e}", cmd.line())),
    }
}

/// `done.json`, as run-inside.ps1 writes it.
#[derive(Debug, Default, Deserialize)]
struct Done {
    exit: Option<i64>,
    nextest_version: Option<String>,
    error: Option<String>,
}

fn wait_and_collect(
    l: &Layout,
    cfg: &RunConfig,
    host: &dyn HostOps,
    clock: &dyn Clock,
) -> Result<Summary, RunError> {
    let t0 = clock.now();
    let started = l.out_dir.join("started.json");
    while !started.is_file() {
        if host.interrupted() {
            return fail(INTERRUPTED);
        }
        if clock.now() - t0 >= cfg.start_timeout {
            return fail(format!(
                "Windows Sandbox didn't start: no started.json within {} s.",
                cfg.start_timeout.as_secs()
            ));
        }
        clock.sleep(POLL);
    }
    println!(
        "The Sandbox started after {} s; the tests run (up to {} minutes in all).",
        (clock.now() - t0).as_secs(),
        cfg.timeout.as_secs() / 60
    );
    let done = l.out_dir.join("done.json");
    while !done.is_file() {
        if host.interrupted() {
            return fail(INTERRUPTED);
        }
        if clock.now() - t0 >= cfg.timeout {
            return fail(format!(
                "the tests didn't finish within {} s (no done.json).",
                cfg.timeout.as_secs()
            ));
        }
        clock.sleep(POLL);
    }
    // done.json may be caught while it is being written.
    let mut parsed = None;
    for _ in 0..5 {
        if let Some(d) = std::fs::read_to_string(&done)
            .ok()
            .and_then(|t| serde_json::from_str::<Done>(t.trim_start_matches('\u{feff}')).ok())
        {
            parsed = Some(d);
            break;
        }
        clock.sleep(Duration::from_secs(1));
    }
    let Some(done) = parsed else {
        return fail(format!("{} isn't valid JSON", done.display()));
    };
    reset_dir(&l.results)?;
    copy_tree(&l.out_dir, &l.results)?;
    let mut summary = Summary {
        nextest_version: done.nextest_version,
        results: Some(l.results.clone()),
        ..Summary::default()
    };
    let entries = std::fs::read_dir(&l.out_dir)
        .or_else(|e| fail(format!("couldn't read {}: {e}", l.out_dir.display())))?;
    let mut junit: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("xml")))
        .collect();
    junit.sort();
    for file in &junit {
        let cases = std::fs::read_to_string(file)
            .map_err(|e| e.to_string())
            .and_then(|x| parse_junit(&x))
            .or_else(|e| fail(format!("{}: {e}", file.display())))?;
        summary.tests.extend(
            cases
                .into_iter()
                .filter(|c| !c.skipped)
                .map(|c| TestResult {
                    name: c.name,
                    passed: !c.failed,
                }),
        );
    }
    if let Some(e) = done.error {
        summary.problems.push(format!("run-inside.ps1 failed: {e}"));
    }
    if summary.tests.is_empty() {
        return fail(format!(
            "no test results came back (done.json: exit {:?}; see {}).",
            done.exit,
            l.results.join("run-inside.log").display()
        ));
    }
    if done.exit != Some(0) && summary.failed() == 0 {
        let exit = done
            .exit
            .map_or_else(|| "without an exit code".to_string(), |c| c.to_string());
        summary.problems.push(format!("the tests exited {exit}"));
    }
    Ok(summary)
}

fn run_host(l: &Layout, host: &dyn HostOps, clock: &dyn Clock) -> Result<Summary, RunError> {
    if host.elevated() {
        return fail(
            "--host runs the toast and foreground tests as the suite runs, never elevated: run it \
             from a terminal that isn't elevated (not \"Run as administrator\").",
        );
    }
    // One host run at a time: a second one's sweep would remove the first one's live keys.
    let _lock = HostLock::take(&l.root.join("host").join("host-run.lock"), host)?;
    // What a killed earlier run left behind.
    let swept = host.sweep_desktop_leftovers().or_else(|e| {
        fail(format!(
            "couldn't check for an earlier run's leftovers: {e}"
        ))
    })?;
    for name in &swept.removed {
        println!("removed what an earlier desktop run left: {name}");
    }
    if !swept.remaining.is_empty() {
        return fail(format!(
            "an earlier desktop run left these behind and they couldn't be removed: {}",
            swept.remaining.join(", ")
        ));
    }
    // Builds the desktop tests and lists the host set.
    let list = host_nextest(&l.repo, "list")
        .arg("--message-format")
        .arg("oneline");
    let (code, listed) = host
        .output(&list)
        .or_else(|e| fail(format!("couldn't run `{}`: {e}", list.line())))?;
    if code != 0 {
        return fail(format!(
            "building the desktop tests failed: `{}` exited {code}",
            list.line()
        ));
    }
    let count = listed.lines().filter(|x| !x.trim().is_empty()).count() as u64;
    if count == 0 {
        return fail("none of the host set's tests is in this checkout.");
    }
    let junit = l
        .repo
        .join("target")
        .join("nextest")
        .join("desktop")
        .join("junit.xml");
    let _ = std::fs::remove_file(&junit);
    let estimate = Duration::from_secs(count * HOST_SECS_PER_TEST + HOST_OVERHEAD_SECS);
    println!(
        "{count} host test(s). The hands-off pop-up warns you {} s ahead.",
        HANDS_OFF_WARNING.as_secs()
    );
    let cap = Duration::from_secs(count * HOST_WORST_SECS_PER_TEST + HOST_CAP_OVERHEAD_SECS);
    let popup = host
        .show_hands_off(HANDS_OFF_WHAT, HANDS_OFF_WARNING, estimate, cap)
        .or_else(|e| {
            fail(format!(
                "the hands-off pop-up didn't show, so nothing ran: {e}"
            ))
        })?;
    clock.sleep(HANDS_OFF_WARNING);
    // The tests start only while the pop-up is on screen.
    if let Err(e) = popup.check() {
        popup.done();
        return fail(format!(
            "the hands-off pop-up closed ({e}), so nothing ran."
        ));
    }
    let mut run = host_nextest(&l.repo, "run");
    // Ctrl+C during the warning: nothing runs, but the cleanup below still does.
    run.env.push(("LIL_DESKTOP_HOST".into(), "1".into()));
    run.env
        .push(("LIL_DEPLOY_DIR".into(), l.deploy.display().to_string()));
    run.env_remove.push("LIL_SANDBOX".into());
    let ran = if host.interrupted() {
        Err(io::Error::other(INTERRUPTED))
    } else {
        host.run(&run)
    };
    let mut summary = Summary::default();
    if ran.is_ok()
        && let Err(e) = popup.check()
    {
        summary
            .problems
            .push(format!("the hands-off pop-up closed during the run ({e})"));
    }
    match host.desktop_leftovers() {
        Ok(left) if left.is_empty() => {}
        Ok(left) => {
            summary
                .problems
                .push(format!("the tests left these behind: {}", left.join(", ")));
            match host.sweep_desktop_leftovers() {
                Ok(s) if s.remaining.is_empty() => {}
                Ok(s) => summary.problems.push(format!(
                    "and these couldn't be removed: {}",
                    s.remaining.join(", ")
                )),
                Err(e) => summary.problems.push(format!("removing them failed: {e}")),
            }
        }
        Err(e) => summary
            .problems
            .push(format!("couldn't check what the tests left behind: {e}")),
    }
    popup.done();
    // Ctrl+C reaches the tests too; their leftovers were checked and swept above.
    if host.interrupted() {
        let cleanup = if summary.problems.is_empty() {
            "nothing was left behind".to_string()
        } else {
            summary.problems.join("; ")
        };
        return fail(format!("{INTERRUPTED}; {cleanup}"));
    }
    let code = ran.or_else(|e| fail(format!("couldn't run `{}`: {e}", run.line())))?;
    let xml = std::fs::read_to_string(&junit)
        .or_else(|e| fail(format!("no JUnit file at {}: {e}", junit.display())))?;
    let cases = parse_junit(&xml).or_else(|e| fail(format!("{}: {e}", junit.display())))?;
    summary.tests = cases
        .into_iter()
        .filter(|c| !c.skipped)
        .map(|c| TestResult {
            name: c.name,
            passed: !c.failed,
        })
        .collect();
    if summary.tests.is_empty() {
        return fail("no host test ran: every case in the JUnit file was skipped.");
    }
    let kept = l.results.join("host");
    reset_dir(&kept)?;
    copy(&junit, &kept.join("junit.xml"))?;
    summary.results = Some(kept);
    if code != 0 && summary.failed() == 0 {
        summary
            .problems
            .push(format!("cargo nextest exited {code}"));
    }
    Ok(summary)
}

/// The host run's lock file, holding the owner's pid; removed when dropped.
struct HostLock(PathBuf);

impl HostLock {
    /// Takes the lock at `path`. A lock whose owner no longer runs is stale and is taken over;
    /// a live owner's refuses (exit 2).
    fn take(path: &Path, host: &dyn HostOps) -> Result<HostLock, RunError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .or_else(|e| fail(format!("couldn't create {}: {e}", dir.display())))?;
        }
        for _ in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(mut f) => {
                    use std::io::Write as _;
                    let _ = write!(f, "{}", std::process::id());
                    return Ok(HostLock(path.to_path_buf()));
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    let owner = std::fs::read_to_string(path)
                        .ok()
                        .and_then(|t| t.trim().parse::<u32>().ok());
                    if let Some(pid) = owner.filter(|p| host.process_alive(*p)) {
                        return fail(format!(
                            "another host run is in progress (process {pid}); let it finish, \
                             then run this again."
                        ));
                    }
                    let _ = std::fs::remove_file(path);
                }
                Err(e) => return fail(format!("couldn't take {}: {e}", path.display())),
            }
        }
        fail(format!("couldn't take {}", path.display()))
    }
}

impl Drop for HostLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// One line per test, the problems, then the counts.
pub fn print_summary(s: &Summary) {
    for t in &s.tests {
        println!("{} {}", if t.passed { "PASS" } else { "FAIL" }, t.name);
    }
    for p in &s.problems {
        println!("problem: {p}");
    }
    println!("{} passed, {} failed", s.passed(), s.failed());
    if let Some(v) = &s.nextest_version {
        println!("run by {v}");
    }
    if let Some(r) = &s.results {
        println!("results: {}", r.display());
    }
}

/// `lil dev desktop-tests`: the command's entry; the process exit code.
pub fn cli(suite: Suite, timeout: Duration, keep_open: bool) -> i32 {
    let repo = match super::root::source_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("desktop tests: {e}");
            return 2;
        }
    };
    let repo = std::fs::canonicalize(&repo).map_or(repo, |p| strip_verbatim(&p));
    let cfg = RunConfig {
        suite,
        timeout,
        start_timeout: START_TIMEOUT,
        keep_open,
    };
    if let Err(e) = lil_platform::interrupt::catch_interrupts() {
        eprintln!("warning: Ctrl+C can't be caught ({e}): it would leave the Sandbox open.");
    }
    let result = run(&repo, &cfg, &RealLauncher::new(), &SystemClock::new());
    match &result {
        Ok(s) => print_summary(s),
        Err(e) => eprintln!("desktop tests couldn't run: {e}"),
    }
    exit_code(&result)
}

/// `p` without the `\\?\` prefix `canonicalize` adds: the Sandbox and PowerShell take plain
/// paths.
fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => p.to_path_buf(),
    }
}

// ---------------------------------------------------------------------------------------------
// The real launcher.
// ---------------------------------------------------------------------------------------------

/// The names of a running Sandbox's processes (S1.9 recorded `vmmemWindowsSandbox` with the
/// Store version; `WindowsSandbox` and `WindowsSandboxClient` are the inbox version's).
/// `WindowsSandboxServer` runs whether or not a Sandbox does, so it doesn't count.
const VM_NAMES: [&str; 3] = [
    "WindowsSandbox",
    "WindowsSandboxClient",
    "vmmemWindowsSandbox",
];
/// The viewer client `wsb connect` opens. It stays open after the VM stops (S1.9), so it never
/// counts as a running Sandbox.
const CLIENT_NAMES: [&str; 1] = ["WindowsSandboxRemoteSession"];

/// How long [`RealLauncher::stop`] lets the Sandbox shut itself down, and then `wsb stop`.
const SELF_STOP: Duration = Duration::from_secs(60);
const WSB_STOP: Duration = Duration::from_secs(30);
/// How long a client has to close after its window was asked to.
const CLIENT_CLOSE: Duration = Duration::from_secs(10);

/// The system calls of [`RealLauncher`], so its command lines and its stop can be tested.
pub trait LauncherSys {
    /// `wsb.exe` on `PATH` (the Store version's app alias).
    fn wsb(&self) -> Option<PathBuf>;
    /// `%SystemRoot%\System32\WindowsSandbox.exe`.
    fn inbox_exe(&self) -> PathBuf;
    fn exists(&self, path: &Path) -> bool;
    /// Runs `program` and returns its exit code and its output (stdout, then stderr).
    fn output(&self, program: &Path, args: &[String]) -> io::Result<(i32, String)>;
    /// Starts `program` without waiting; its pid, pinned.
    fn spawn(&self, program: &Path, args: &[String]) -> io::Result<u32>;
    /// Every process: pid and name without `.exe`.
    fn processes(&self) -> io::Result<Vec<(u32, String)>>;
    /// Opens a handle to `pid`, held from now on, so the process can't be confused with a later
    /// one with the same pid. False when it can't be opened.
    fn pin(&self, pid: u32) -> bool;
    /// Whether the pinned process still runs.
    fn alive(&self, pid: u32) -> bool;
    /// Asks the pinned process's window to close.
    fn close(&self, pid: u32);
    /// Ends the pinned process.
    fn kill(&self, pid: u32);
    fn sleep(&self, d: Duration);
}

/// The real Windows Sandbox.
pub struct RealLauncher {
    sys: Box<dyn LauncherSys>,
}

impl RealLauncher {
    pub fn new() -> RealLauncher {
        RealLauncher::with_sys(Box::new(RealSys::default()))
    }

    pub fn with_sys(sys: Box<dyn LauncherSys>) -> RealLauncher {
        RealLauncher { sys }
    }

    /// The ids `wsb list` prints, or `None` when it can't be asked.
    fn listed(&self) -> Option<Vec<String>> {
        let wsb = self.sys.wsb()?;
        match self.sys.output(&wsb, &["list".to_string()]) {
            Ok((0, out)) => Some(guids(&out)),
            _ => None,
        }
    }

    fn sandbox_pids(&self, names: &[&str]) -> Vec<u32> {
        self.sys
            .processes()
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, n)| names.iter().any(|x| n.eq_ignore_ascii_case(x)))
            .map(|(pid, _)| pid)
            .collect()
    }

    /// The Sandbox processes that appeared after `h`'s start, pinned when first seen.
    fn own_new(&self, h: &SandboxHandle, names: &[&str]) -> Vec<u32> {
        self.sandbox_pids(names)
            .into_iter()
            .filter(|p| !h.pids_before.contains(p))
            .filter(|p| self.sys.pin(*p))
            .collect()
    }

    fn wait_until(&self, within: Duration, mut done: impl FnMut() -> bool) -> bool {
        let mut waited = Duration::ZERO;
        while !done() {
            if waited >= within {
                return false;
            }
            self.sys.sleep(POLL);
            waited += POLL;
        }
        true
    }
}

impl Default for RealLauncher {
    fn default() -> Self {
        RealLauncher::new()
    }
}

/// The GUIDs in `text`, in order.
fn guids(text: &str) -> Vec<String> {
    let is_guid = |w: &str| {
        let parts: Vec<&str> = w.split('-').collect();
        parts.iter().map(|p| p.len()).eq([8, 4, 4, 4, 12])
            && parts
                .iter()
                .all(|p| p.bytes().all(|b| b.is_ascii_hexdigit()))
    };
    text.split(|c: char| !(c.is_ascii_hexdigit() || c == '-'))
        .filter(|w| is_guid(w))
        .map(str::to_string)
        .collect()
}

impl SandboxLauncher for RealLauncher {
    fn availability(&self) -> Availability {
        if self.listed().is_some() {
            Availability::Cli
        } else if self.sys.exists(&self.sys.inbox_exe()) {
            Availability::Inbox
        } else {
            Availability::Missing
        }
    }

    fn running(&self) -> bool {
        match self.listed() {
            Some(ids) => !ids.is_empty(),
            None => !self.sandbox_pids(&VM_NAMES).is_empty(),
        }
    }

    fn start(&self, wsb: &Path) -> io::Result<SandboxHandle> {
        let all: Vec<&str> = VM_NAMES.iter().chain(&CLIENT_NAMES).copied().collect();
        let pids_before = self.sandbox_pids(&all);
        match self.sys.wsb().filter(|_| self.listed().is_some()) {
            Some(cli) => {
                let xml = std::fs::read_to_string(wsb)?;
                let args = vec![
                    "start".to_string(),
                    "--config".to_string(),
                    wsb_one_line(&xml),
                ];
                let (code, out) = self.sys.output(&cli, &args)?;
                let id = guids(&out).into_iter().next();
                let Some(id) = id.filter(|_| code == 0) else {
                    return Err(io::Error::other(format!(
                        "wsb start exited {code}: {}",
                        out.trim()
                    )));
                };
                // The logon command needs a user session: connect opens the Sandbox's window.
                let connect = match self.sys.spawn(
                    &cli,
                    &["connect".to_string(), "--id".to_string(), id.clone()],
                ) {
                    Ok(pid) => pid,
                    Err(e) => {
                        // The VM is up, but without a user session its logon command never
                        // runs, so it would never shut itself down.
                        let _ = self
                            .sys
                            .output(&cli, &["stop".to_string(), "--id".to_string(), id.clone()]);
                        return Err(e);
                    }
                };
                Ok(SandboxHandle {
                    via: Availability::Cli,
                    id: Some(id),
                    pids_before,
                    started: vec![connect],
                })
            }
            None => {
                let exe = self.sys.inbox_exe();
                let pid = self.sys.spawn(&exe, &[wsb.display().to_string()])?;
                Ok(SandboxHandle {
                    via: Availability::Inbox,
                    id: None,
                    pids_before,
                    started: vec![pid],
                })
            }
        }
    }

    fn stop(&self, h: &SandboxHandle) -> io::Result<()> {
        let mut problems = Vec::new();
        // The VM: run-inside.ps1 shuts it down itself; else wsb stop, else the own processes go.
        match (&h.id, self.sys.wsb()) {
            (Some(id), Some(cli)) => {
                let gone = |l: &RealLauncher| l.listed().is_some_and(|ids| !ids.contains(id));
                if !self.wait_until(SELF_STOP, || gone(self)) {
                    let _ = self
                        .sys
                        .output(&cli, &["stop".to_string(), "--id".to_string(), id.clone()]);
                    if !self.wait_until(WSB_STOP, || gone(self)) {
                        problems.push(format!("the Sandbox {id} is still running"));
                    }
                }
            }
            _ => {
                let mut vm: Vec<u32> = h.started.clone();
                vm.extend(self.own_new(h, &VM_NAMES));
                let alive = |l: &RealLauncher| vm.iter().any(|p| l.sys.alive(*p));
                if !self.wait_until(SELF_STOP, || !alive(self)) {
                    for p in &vm {
                        self.sys.kill(*p);
                    }
                    if alive(self) {
                        problems.push("the Sandbox's processes are still running".to_string());
                    }
                }
            }
        }
        // The viewer clients: the started `wsb connect` and the WindowsSandboxRemoteSession it
        // opened, both still open after the VM stopped.
        let mut clients: Vec<u32> = h.started.clone();
        clients.extend(self.own_new(h, &CLIENT_NAMES));
        for pid in clients {
            if !self.sys.alive(pid) {
                continue;
            }
            self.sys.close(pid);
            if !self.wait_until(CLIENT_CLOSE, || !self.sys.alive(pid)) {
                self.sys.kill(pid);
                if self.sys.alive(pid) {
                    problems.push(format!(
                        "the Sandbox's window (process {pid}) is still open"
                    ));
                }
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(io::Error::other(problems.join("; ")))
        }
    }
}

/// The real system calls.
#[derive(Default)]
struct RealSys {
    pinned: std::sync::Mutex<std::collections::HashMap<u32, lil_platform::process::ProcessHandle>>,
}

impl RealSys {
    fn pinned(
        &self,
    ) -> std::sync::MutexGuard<
        '_,
        std::collections::HashMap<u32, lil_platform::process::ProcessHandle>,
    > {
        self.pinned.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl LauncherSys for RealSys {
    fn wsb(&self) -> Option<PathBuf> {
        lil_platform::process::find_on_path("wsb")
    }

    fn inbox_exe(&self) -> PathBuf {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| OsString::from(r"C:\Windows"));
        PathBuf::from(root)
            .join("System32")
            .join("WindowsSandbox.exe")
    }

    fn exists(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn output(&self, program: &Path, args: &[String]) -> io::Result<(i32, String)> {
        let out = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .output()?;
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok((out.status.code().unwrap_or(-1), text))
    }

    fn spawn(&self, program: &Path, args: &[String]) -> io::Result<u32> {
        let child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let pid = child.id();
        self.pin(pid);
        Ok(pid)
    }

    fn processes(&self) -> io::Result<Vec<(u32, String)>> {
        let out = Command::new("tasklist")
            .args(["/FO", "CSV", "/NH"])
            .stdin(Stdio::null())
            .output()?;
        Ok(parse_tasklist(&String::from_utf8_lossy(&out.stdout)))
    }

    fn pin(&self, pid: u32) -> bool {
        let mut pinned = self.pinned();
        if pinned.contains_key(&pid) {
            return true;
        }
        match lil_platform::process::open(pid) {
            Ok(h) => {
                pinned.insert(pid, h);
                true
            }
            Err(_) => false,
        }
    }

    fn alive(&self, pid: u32) -> bool {
        self.pinned()
            .get(&pid)
            .is_some_and(|h| matches!(h.exit_code(), Ok(None)))
    }

    fn close(&self, pid: u32) {
        if self.alive(pid) {
            // Without /F, taskkill asks the process's windows to close.
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }

    fn kill(&self, pid: u32) {
        if let Some(h) = self.pinned().get(&pid) {
            let _ = h.kill();
        }
    }

    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// `tasklist /FO CSV /NH`'s lines as pid and name (without `.exe`).
fn parse_tasklist(text: &str) -> Vec<(u32, String)> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.trim().trim_matches('"').split("\",\"").collect();
            let name = fields.first()?;
            let pid = fields.get(1)?.parse().ok()?;
            let name = name
                .strip_suffix(".exe")
                .or_else(|| name.strip_suffix(".EXE"))
                .unwrap_or(name);
            Some((pid, name.to_string()))
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// The real host.
// ---------------------------------------------------------------------------------------------

/// The real host operations.
pub struct RealHost {
    layout: Layout,
}

impl RealHost {
    pub fn new(repo: &Path) -> RealHost {
        RealHost {
            layout: Layout::new(repo),
        }
    }
}

fn command(cmd: &Cmd) -> Command {
    let mut c = Command::new(&cmd.program);
    super::root::scrub_git_env(&mut c);
    c.args(&cmd.args).current_dir(&cmd.cwd);
    for name in &cmd.env_remove {
        c.env_remove(name);
    }
    for (k, v) in &cmd.env {
        c.env(k, v);
    }
    c
}

/// `%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe` (Windows PowerShell 5.1, for
/// WinForms and the ACL check).
fn windows_powershell() -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| OsString::from(r"C:\Windows"));
    PathBuf::from(root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
}

/// Prints each folder SYSTEM (`S-1-5-18`) has no allow entry for with the rights it needs:
/// read and execute, or modify for a `w|` folder. The folders come in `LIL_ACL_FOLDERS`, one per
/// line, each `r|<path>` or `w|<path>`.
const ACL_CHECK: &str = r#"
$ErrorActionPreference = 'Stop'
foreach ($line in ($env:LIL_ACL_FOLDERS -split "`n")) {
    if (-not $line) { continue }
    $write = $line.StartsWith('w|'); $path = $line.Substring(2)
    $need = if ($write) { [Security.AccessControl.FileSystemRights]::Modify } else { [Security.AccessControl.FileSystemRights]::ReadAndExecute }
    $ok = $false
    try {
        foreach ($rule in (Get-Acl -LiteralPath $path).Access) {
            if ($rule.AccessControlType -ne 'Allow') { continue }
            try { $sid = $rule.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value } catch { continue }
            if ($sid -eq 'S-1-5-18' -and (($rule.FileSystemRights -band $need) -eq $need)) { $ok = $true }
        }
    } catch { }
    if (-not $ok) { [Console]::Out.WriteLine($path) }
}
"#;

/// `powershell -EncodedCommand`'s argument: the script as base64 of UTF-16LE.
fn encoded_command(script: &str) -> String {
    use base64::Engine as _;
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

struct RealHandsOff {
    state: PathBuf,
    error: PathBuf,
    child: std::sync::Mutex<std::process::Child>,
}

impl HandsOff for RealHandsOff {
    fn done(&self) {
        let _ = std::fs::write(&self.state, "done");
    }

    fn check(&self) -> Result<(), String> {
        if let Ok(e) = std::fs::read_to_string(&self.error) {
            return Err(format!("hands-off.ps1 failed: {}", e.trim()));
        }
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        match child.try_wait() {
            Ok(None) => Ok(()),
            Ok(Some(status)) => Err(format!("it exited ({status})")),
            Err(e) => Err(format!("it can't be checked: {e}")),
        }
    }
}

impl Drop for RealHandsOff {
    fn drop(&mut self) {
        self.done();
    }
}

impl HostOps for RealHost {
    fn run(&self, cmd: &Cmd) -> io::Result<i32> {
        println!("> {}", cmd.line());
        Ok(command(cmd).status()?.code().unwrap_or(-1))
    }

    fn output(&self, cmd: &Cmd) -> io::Result<(i32, String)> {
        let out = command(cmd)
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output()?;
        Ok((
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        ))
    }

    fn find_program(&self, name: &str) -> Option<PathBuf> {
        lil_platform::process::find_on_path(name)
    }

    fn vc_runtime(&self) -> Result<Vec<PathBuf>, String> {
        let pf86 = std::env::var_os("ProgramFiles(x86)").ok_or("ProgramFiles(x86) isn't set")?;
        let vswhere = PathBuf::from(pf86).join(r"Microsoft Visual Studio\Installer\vswhere.exe");
        let out = Command::new(&vswhere)
            .args(["-latest", "-products", "*", "-property", "installationPath"])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| {
                format!(
                    "{}: {e} (Visual Studio Build Tools are needed)",
                    vswhere.display()
                )
            })?;
        let vs = String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        if vs.is_empty() {
            return Err("vswhere found no Visual Studio installation".to_string());
        }
        let redist = Path::new(&vs).join(r"VC\Redist\MSVC");
        let mut versions: Vec<(Vec<u32>, PathBuf)> = std::fs::read_dir(&redist)
            .map_err(|e| format!("{}: {e}", redist.display()))?
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let version: Option<Vec<u32>> = name.split('.').map(|p| p.parse().ok()).collect();
                version.map(|v| (v, e.path()))
            })
            .collect();
        versions.sort();
        for (_, dir) in versions.iter().rev() {
            let Ok(entries) = std::fs::read_dir(dir.join("x64")) else {
                continue;
            };
            for crt in entries.flatten() {
                let name = crt.file_name().to_string_lossy().into_owned();
                if name.starts_with("Microsoft.VC") && name.ends_with(".CRT") {
                    let dlls: Vec<PathBuf> =
                        VC_RUNTIME.iter().map(|d| crt.path().join(d)).collect();
                    if dlls.iter().all(|d| d.is_file()) {
                        return Ok(dlls);
                    }
                }
            }
        }
        Err(format!(
            "no x64 Microsoft.VC*.CRT folder with {} under {}",
            VC_RUNTIME.join(", "),
            redist.display()
        ))
    }

    fn system_cannot_use(&self, mapped: &[Mapped]) -> io::Result<Vec<PathBuf>> {
        let folders: Vec<String> = mapped
            .iter()
            .map(|m| {
                format!(
                    "{}|{}",
                    if m.read_only { "r" } else { "w" },
                    m.host.display()
                )
            })
            .collect();
        let out = Command::new(windows_powershell())
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-EncodedCommand",
                &encoded_command(ACL_CHECK),
            ])
            .env("LIL_ACL_FOLDERS", folders.join("\n"))
            .stdin(Stdio::null())
            .output()?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "the permission check exited {:?}: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect())
    }

    fn elevated(&self) -> bool {
        lil_platform::desktop::is_elevated()
    }

    fn show_hands_off(
        &self,
        what: &str,
        warning: Duration,
        run: Duration,
        cap: Duration,
    ) -> io::Result<Box<dyn HandsOff>> {
        let dir = self.layout.root.join("host");
        std::fs::create_dir_all(&dir)?;
        let script = dir.join("hands-off.ps1");
        std::fs::write(&script, HANDS_OFF_SCRIPT)?;
        let state = dir.join("hands-off.state");
        let shown = dir.join("hands-off.state.shown");
        let error = dir.join("hands-off.state.error");
        for f in [&state, &shown, &error] {
            let _ = std::fs::remove_file(f);
        }
        let args: Vec<String> = [
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .chain([
            script.display().to_string(),
            "-What".to_string(),
            what.to_string(),
            "-WarnSec".to_string(),
            warning.as_secs().to_string(),
            "-RunSec".to_string(),
            run.as_secs().to_string(),
            "-CapSec".to_string(),
            cap.as_secs().to_string(),
            "-StateFile".to_string(),
            state.display().to_string(),
            "-ParentPid".to_string(),
            std::process::id().to_string(),
        ])
        .collect();
        // No console window (CREATE_NO_WINDOW); the form itself never takes the focus.
        let child = {
            use std::os::windows::process::CommandExt as _;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            Command::new(windows_powershell())
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()?
        };
        let popup = RealHandsOff {
            state,
            error: error.clone(),
            child: std::sync::Mutex::new(child),
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        while !shown.is_file() {
            if let Ok(e) = std::fs::read_to_string(&error) {
                return Err(io::Error::other(format!(
                    "hands-off.ps1 failed: {}",
                    e.trim()
                )));
            }
            if Instant::now() >= deadline {
                return Err(io::Error::other("it wasn't on screen within 30 s"));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        Ok(Box::new(popup))
    }

    fn desktop_leftovers(&self) -> io::Result<Vec<String>> {
        let programs = lil_platform::known_folders::programs().map_err(io::Error::other)?;
        lil_platform::desktop::desktop_test_leftovers(&programs)
    }

    fn sweep_desktop_leftovers(&self) -> io::Result<Swept> {
        let programs = lil_platform::known_folders::programs().map_err(io::Error::other)?;
        let sweep = lil_platform::desktop::sweep_desktop_tests(&programs)?;
        Ok(Swept {
            removed: sweep.removed,
            remaining: sweep.remaining,
        })
    }

    fn interrupted(&self) -> bool {
        lil_platform::interrupt::interrupted()
    }

    fn process_alive(&self, pid: u32) -> bool {
        lil_platform::process::open(pid).is_ok_and(|h| matches!(h.exit_code(), Ok(None)))
    }

    fn qt_dir(&self) -> Option<PathBuf> {
        std::env::var_os("LIL_QT_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::HashSet;
    use std::rc::Rc;

    // -----------------------------------------------------------------------------------------
    // Fakes: no Sandbox, no real time, no real host.
    // -----------------------------------------------------------------------------------------

    type Log = Rc<RefCell<Vec<String>>>;

    /// Time moves only when the run sleeps.
    struct FakeClock {
        now: Cell<Duration>,
        log: Log,
        /// The wait panics (a bug in the run) at its first sleep.
        panics: Cell<bool>,
    }

    impl Clock for FakeClock {
        fn now(&self) -> Duration {
            self.now.get()
        }
        fn sleep(&self, d: Duration) {
            assert!(!self.panics.get(), "a bug in the wait");
            self.now.set(self.now.get() + d);
            self.log.borrow_mut().push(format!("sleep {}", d.as_secs()));
        }
    }

    const TESTBINS: [&str; 7] = [
        "lil.exe",
        "lil-core.exe",
        "lil-mcp.exe",
        "lil-test-app.exe",
        "lil-fake-comfyui.exe",
        "lil-fake-comfy-cli.exe",
        "claude.exe",
    ];

    /// The tracked files of the fixture repository.
    const TRACKED: [&str; 4] = [
        "Cargo.toml",
        "README.md",
        "crates/lil-platform/Cargo.toml",
        "crates/lil-platform/src/lib.rs",
    ];

    struct FakeHost {
        repo: PathBuf,
        tools: PathBuf,
        log: Log,
        cmds: RefCell<Vec<Cmd>>,
        mapped: RefCell<Vec<Mapped>>,
        refuse: Vec<PathBuf>,
        elevated: bool,
        leftovers: RefCell<Vec<String>>,
        popups: RefCell<Vec<(Duration, Duration, Duration)>>,
        /// The pop-up has gone away.
        popup_closed: Rc<Cell<bool>>,
        /// Processes that run (the host-run lock's owner).
        alive_pids: Vec<u32>,
        /// What the host's `cargo nextest run` exits with, and whether it writes the JUnit file.
        host_exit: i32,
        host_writes_junit: bool,
        ctest_json: String,
        qt: Option<PathBuf>,
        host_junit: String,
        /// Ctrl+C has come.
        interrupt: Rc<Cell<bool>>,
        /// Ctrl+C comes while the host tests run, and they leave this behind.
        run_interrupted_leaving: Option<String>,
    }

    struct FakePopup {
        log: Log,
        closed: Rc<Cell<bool>>,
    }

    impl HandsOff for FakePopup {
        fn done(&self) {
            self.log.borrow_mut().push("popup done".to_string());
        }
        fn check(&self) -> Result<(), String> {
            if self.closed.get() {
                Err("it exited".to_string())
            } else {
                Ok(())
            }
        }
    }

    impl HostOps for FakeHost {
        fn run(&self, cmd: &Cmd) -> io::Result<i32> {
            self.log.borrow_mut().push(format!("run {}", cmd.line()));
            self.cmds.borrow_mut().push(cmd.clone());
            let args: Vec<&str> = cmd.args.iter().map(String::as_str).collect();
            match (cmd.program_name().as_str(), &args[..]) {
                ("cargo", ["nextest", "archive", ..]) => {
                    let i = args.iter().position(|a| *a == "--archive-file").unwrap();
                    std::fs::write(args[i + 1], b"archive").unwrap();
                }
                ("cargo", ["build", ..]) => {
                    let dir = self.repo.join(r"target\testbins\debug");
                    std::fs::create_dir_all(dir.join("deps")).unwrap();
                    for b in TESTBINS {
                        std::fs::write(dir.join(b), b"exe").unwrap();
                    }
                    std::fs::write(dir.join("lil.pdb"), b"pdb").unwrap();
                }
                ("cargo", ["nextest", "run", ..]) => {
                    if let Some(left) = &self.run_interrupted_leaving {
                        self.interrupt.set(true);
                        self.leftovers.borrow_mut().push(left.clone());
                        return Ok(1);
                    }
                    if self.host_writes_junit {
                        let dir = self.repo.join(r"target\nextest\desktop");
                        std::fs::create_dir_all(&dir).unwrap();
                        std::fs::write(dir.join("junit.xml"), &self.host_junit).unwrap();
                    }
                    return Ok(self.host_exit);
                }
                _ => {}
            }
            Ok(0)
        }

        fn output(&self, cmd: &Cmd) -> io::Result<(i32, String)> {
            self.log.borrow_mut().push(format!("output {}", cmd.line()));
            self.cmds.borrow_mut().push(cmd.clone());
            let out = match cmd.program_name().as_str() {
                "git" => TRACKED.join("\0") + "\0",
                "ctest" => self.ctest_json.clone(),
                "cargo" => HOST_SET[..2]
                    .iter()
                    .map(|t| format!("lil-platform::desktop {t}\n"))
                    .collect(),
                _ => String::new(),
            };
            Ok((0, out))
        }

        fn find_program(&self, name: &str) -> Option<PathBuf> {
            (name == "cargo-nextest").then(|| self.tools.join("cargo-nextest.exe"))
        }

        fn vc_runtime(&self) -> Result<Vec<PathBuf>, String> {
            Ok(VC_RUNTIME.iter().map(|d| self.tools.join(d)).collect())
        }

        fn system_cannot_use(&self, mapped: &[Mapped]) -> io::Result<Vec<PathBuf>> {
            self.mapped.borrow_mut().extend(mapped.iter().cloned());
            Ok(self.refuse.clone())
        }

        fn elevated(&self) -> bool {
            self.elevated
        }

        fn show_hands_off(
            &self,
            _what: &str,
            warning: Duration,
            run: Duration,
            cap: Duration,
        ) -> io::Result<Box<dyn HandsOff>> {
            self.log.borrow_mut().push("popup".to_string());
            self.popups.borrow_mut().push((warning, run, cap));
            Ok(Box::new(FakePopup {
                log: self.log.clone(),
                closed: self.popup_closed.clone(),
            }))
        }

        fn process_alive(&self, pid: u32) -> bool {
            self.alive_pids.contains(&pid)
        }

        fn desktop_leftovers(&self) -> io::Result<Vec<String>> {
            self.log.borrow_mut().push("check leftovers".to_string());
            Ok(self.leftovers.borrow().clone())
        }

        fn sweep_desktop_leftovers(&self) -> io::Result<Swept> {
            self.log.borrow_mut().push("sweep".to_string());
            Ok(Swept {
                removed: std::mem::take(&mut *self.leftovers.borrow_mut()),
                remaining: Vec::new(),
            })
        }

        fn qt_dir(&self) -> Option<PathBuf> {
            self.qt.clone()
        }

        fn interrupted(&self) -> bool {
            self.interrupt.get()
        }
    }

    /// What the fake Sandbox writes to the results folder once it has started.
    type OnStart = Box<dyn Fn(&Path)>;

    struct FakeLauncher {
        availability: Availability,
        running: bool,
        out: PathBuf,
        on_start: OnStart,
        /// At the start: the results folder existed, it was empty, the `.wsb`'s text.
        started: RefCell<Option<(bool, bool, String)>>,
        stops: Cell<usize>,
    }

    impl SandboxLauncher for FakeLauncher {
        fn availability(&self) -> Availability {
            self.availability
        }
        fn running(&self) -> bool {
            self.running
        }
        fn start(&self, wsb: &Path) -> io::Result<SandboxHandle> {
            let exists = self.out.is_dir();
            let empty = exists && std::fs::read_dir(&self.out).unwrap().next().is_none();
            *self.started.borrow_mut() =
                Some((exists, empty, std::fs::read_to_string(wsb).unwrap()));
            (self.on_start)(&self.out);
            Ok(SandboxHandle {
                via: self.availability,
                id: None,
                pids_before: Vec::new(),
                started: Vec::new(),
            })
        }
        fn stop(&self, _h: &SandboxHandle) -> io::Result<()> {
            self.stops.set(self.stops.get() + 1);
            Ok(())
        }
    }

    fn junit(cases: &[(&str, bool)]) -> String {
        let mut x = String::from(r#"<?xml version="1.0"?><testsuites><testsuite name="s">"#);
        for (name, passed) in cases {
            x.push_str(&format!(
                r#"<testcase name="{name}" classname="lil-platform::desktop">{}</testcase>"#,
                if *passed {
                    ""
                } else {
                    r#"<failure message="boom">boom</failure>"#
                }
            ));
        }
        x.push_str("</testsuite></testsuites>");
        x
    }

    /// A Sandbox run that finishes: started.json, one passing JUnit file, done.json.
    fn finishes() -> OnStart {
        Box::new(|out: &Path| {
            std::fs::write(out.join("started.json"), "{}").unwrap();
            std::fs::write(
                out.join("junit.xml"),
                junit(&[("desktop_power_request_sets_and_clears", true)]),
            )
            .unwrap();
            std::fs::write(
                out.join("done.json"),
                r#"{"suite":"desktop","exit":0,"tests":1,"passed":1,"failed":0,"nextest_version":"cargo-nextest 0.9.148"}"#,
            )
            .unwrap();
        })
    }

    struct Fx {
        _tmp: tempfile::TempDir,
        repo: PathBuf,
        host: FakeHost,
        clock: FakeClock,
        log: Log,
    }

    impl Fx {
        fn new() -> Fx {
            let tmp = tempfile::tempdir().unwrap();
            let repo = tmp.path().join("repo");
            for f in TRACKED {
                let p = repo.join(f);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, f).unwrap();
            }
            std::fs::write(repo.join("untracked.txt"), "x").unwrap();
            let tools = tmp.path().join("tools");
            std::fs::create_dir_all(&tools).unwrap();
            for f in VC_RUNTIME.iter().chain(&["cargo-nextest.exe"]) {
                std::fs::write(tools.join(f), f).unwrap();
            }
            let log: Log = Rc::default();
            Fx {
                host: FakeHost {
                    repo: repo.clone(),
                    tools,
                    log: log.clone(),
                    cmds: RefCell::default(),
                    mapped: RefCell::default(),
                    refuse: Vec::new(),
                    elevated: false,
                    leftovers: RefCell::default(),
                    popups: RefCell::default(),
                    ctest_json: String::new(),
                    qt: None,
                    host_junit: junit(&[(HOST_SET[0], true), (HOST_SET[1], true)]),
                    interrupt: Rc::default(),
                    run_interrupted_leaving: None,
                    popup_closed: Rc::default(),
                    alive_pids: Vec::new(),
                    host_exit: 0,
                    host_writes_junit: true,
                },
                clock: FakeClock {
                    now: Cell::new(Duration::ZERO),
                    log: log.clone(),
                    panics: Cell::new(false),
                },
                repo,
                log,
                _tmp: tmp,
            }
        }

        fn layout(&self) -> Layout {
            Layout::new(&self.repo)
        }

        fn launcher(&self, on_start: OnStart) -> FakeLauncher {
            FakeLauncher {
                availability: Availability::Cli,
                running: false,
                out: self.layout().out_dir,
                on_start,
                started: RefCell::default(),
                stops: Cell::new(0),
            }
        }

        fn run(&self, cfg: &RunConfig, launcher: &FakeLauncher) -> Result<Summary, RunError> {
            run_with(&self.repo, cfg, &self.host, launcher, &self.clock)
        }

        fn cmd(&self, program: &str, first: &str) -> Cmd {
            self.host
                .cmds
                .borrow()
                .iter()
                .find(|c| c.program_name() == program && c.args.iter().any(|a| a == first))
                .cloned()
                .unwrap_or_else(|| panic!("no `{program} … {first}` ran"))
        }
    }

    fn cfg(suite: Suite) -> RunConfig {
        RunConfig {
            suite,
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_MINS * 60),
            start_timeout: START_TIMEOUT,
            keep_open: false,
        }
    }

    /// The `<MappedFolder>`s of a `.wsb`: (host, sandbox, read-only).
    fn mapped_of(xml: &str) -> Vec<(String, String, bool)> {
        let doc = roxmltree::Document::parse(xml).expect("the .wsb parses as XML");
        doc.descendants()
            .filter(|n| n.has_tag_name("MappedFolder"))
            .map(|m| {
                let text = |tag: &str| {
                    m.children()
                        .find(|c| c.has_tag_name(tag))
                        .and_then(|c| c.text())
                        .unwrap_or_default()
                        .to_string()
                };
                (
                    text("HostFolder"),
                    text("SandboxFolder"),
                    text("ReadOnly") == "true",
                )
            })
            .collect()
    }

    fn element(xml: &str, tag: &str) -> String {
        let doc = roxmltree::Document::parse(xml).unwrap();
        doc.descendants()
            .find(|n| n.has_tag_name(tag))
            .and_then(|n| n.text())
            .unwrap_or_default()
            .to_string()
    }

    // -----------------------------------------------------------------------------------------
    // The .wsb and the bundle.
    // -----------------------------------------------------------------------------------------

    #[test] // spec: QA-042
    fn wsb_maps_the_bundle_read_only_and_the_results_writable() {
        let fx = Fx::new();
        let l = fx.layout();
        std::fs::create_dir_all(&l.deploy).unwrap();
        let launcher = fx.launcher(finishes());
        fx.run(&cfg(Suite::Desktop), &launcher).unwrap();
        let xml = std::fs::read_to_string(&l.wsb).unwrap();
        // The Sandbox was started with this very file.
        assert_eq!(launcher.started.borrow().as_ref().unwrap().2, xml);
        // Nothing but the bundle and the deploy folder (read-only) and the results (writable).
        assert_eq!(
            mapped_of(&xml),
            vec![
                (l.in_dir.display().to_string(), SANDBOX_IN.to_string(), true),
                (
                    l.deploy.display().to_string(),
                    SANDBOX_DEPLOY.to_string(),
                    true
                ),
                (
                    l.out_dir.display().to_string(),
                    SANDBOX_OUT.to_string(),
                    false
                ),
            ]
        );
        assert_eq!(element(&xml, "Networking"), "Disable");
        assert_eq!(element(&xml, "ClipboardRedirection"), "Disable");
        let logon = element(&xml, "Command");
        assert_eq!(
            logon,
            r"powershell.exe -ExecutionPolicy Bypass -File C:\lil\in\run-inside.ps1"
        );
        // Without a deploy folder there is nothing to map at C:\lil\deploy.
        let fx = Fx::new();
        fx.run(&cfg(Suite::Desktop), &fx.launcher(finishes()))
            .unwrap();
        let xml = std::fs::read_to_string(fx.layout().wsb).unwrap();
        let sandbox: Vec<String> = mapped_of(&xml).into_iter().map(|m| m.1).collect();
        assert_eq!(sandbox, [SANDBOX_IN, SANDBOX_OUT]);
        // A host path with XML's special characters is escaped.
        let x = wsb_xml(&[Mapped {
            host: PathBuf::from(r"C:\a&b<c>"),
            sandbox: SANDBOX_IN.into(),
            read_only: true,
        }]);
        assert_eq!(mapped_of(&x)[0].0, r"C:\a&b<c>");
        // `wsb start --config` gets it on one line.
        assert!(!wsb_one_line(&x).contains('\n'));
        assert_eq!(mapped_of(&wsb_one_line(&x)), mapped_of(&x));
    }

    #[test] // spec: QA-042
    fn qml_suite_maps_the_build_tree_and_qt_at_their_host_paths() {
        let mut fx = Fx::new();
        let l = fx.layout();
        std::fs::create_dir_all(&l.deploy).unwrap();
        let qt = fx._tmp.path().join("Qt").join("6.12.0");
        std::fs::create_dir_all(&qt).unwrap();
        fx.host.qt = Some(qt.clone());
        fx.host.ctest_json =
            r#"{"kind":"ctestInfo","tests":[{"name":"tst_boot","command":["x.exe"]}]}"#.into();
        fx.run(&cfg(Suite::Qml), &fx.launcher(finishes())).unwrap();
        let xml = std::fs::read_to_string(&l.wsb).unwrap();
        let tree = l.build_tree.display().to_string();
        let qt = qt.display().to_string();
        assert_eq!(
            mapped_of(&xml),
            vec![
                (l.in_dir.display().to_string(), SANDBOX_IN.to_string(), true),
                (
                    l.deploy.display().to_string(),
                    SANDBOX_DEPLOY.to_string(),
                    true
                ),
                (tree.clone(), tree, true),
                (qt.clone(), qt, true),
                (
                    l.out_dir.display().to_string(),
                    SANDBOX_OUT.to_string(),
                    false
                ),
            ]
        );
        assert_eq!(
            fx.cmd("ctest", "--preset").args,
            ["--preset", "qml", "--show-only=json-v1"]
        );
        // No Rust build for the CTest suites.
        assert!(
            fx.host
                .cmds
                .borrow()
                .iter()
                .all(|c| c.program_name() != "cargo")
        );
        // Without LIL_QT_DIR it can't run.
        let fx = Fx::new();
        std::fs::create_dir_all(fx.layout().build_tree).unwrap();
        let err = fx
            .run(&cfg(Suite::Grabs), &fx.launcher(finishes()))
            .unwrap_err();
        assert!(err.0.contains("LIL_QT_DIR"), "{err}");
    }

    #[test] // spec: QA-042
    fn the_inside_command_runs_ignored_desktop_tests_from_the_archive() {
        let fx = Fx::new();
        fx.run(&cfg(Suite::Desktop), &fx.launcher(finishes()))
            .unwrap();
        let script = std::fs::read_to_string(fx.layout().in_dir.join("run-inside.ps1")).unwrap();
        assert_eq!(script, RUN_INSIDE);
        for needed in [
            r"'--archive-file', (Join-Path $In 'desktop.tar.zst')",
            r"'--workspace-remap', 'C:\lil\work\src'",
            r"'--extract-to', 'C:\lil\work\bin'",
            r"'--config-file', (Join-Path $In 'nextest-sandbox.toml')",
            r"'--profile', 'desktop'",
            r"'--run-ignored', 'only'",
            r"'-E', $Filter",
            r"$Filter = 'test(/^desktop_/)",
            r"$env:LIL_TESTBINS_DIR = Join-Path $In 'testbins'",
            r"$env:LIL_SANDBOX = '1'",
            r"$env:LIL_DEPLOY_DIR = 'C:\lil\deploy'",
            r"Copy-Item -LiteralPath $junit -Destination $Out",
            "shutdown /s /t 0",
        ] {
            assert!(script.contains(needed), "run-inside.ps1 lacks {needed}");
        }
        // C:\lil\work\bin exists before nextest runs (--extract-to fails on a missing folder).
        let mkdir = script
            .find(r"New-Item -ItemType Directory -Force -Path (Join-Path $Work 'bin')")
            .expect("creates C:\\lil\\work\\bin");
        let nextest = script.find("'nextest', 'run'").unwrap();
        assert!(mkdir < nextest);
        // The guard comes before anything else runs.
        let guard = script
            .find("if ($env:USERNAME -ne 'WDAGUtilityAccount'")
            .unwrap();
        assert!(guard < script.find("Write-Json 'started.json'").unwrap());
        assert!(guard < script.find("{ shutdown /s /t 0 }").unwrap());
    }

    #[test] // spec: QA-042
    fn the_desktop_profile_is_serialised() {
        let fx = Fx::new();
        fx.run(&cfg(Suite::Desktop), &fx.launcher(finishes()))
            .unwrap();
        let text =
            std::fs::read_to_string(fx.layout().in_dir.join("nextest-sandbox.toml")).unwrap();
        let doc: toml::Table = text.parse().expect("nextest-sandbox.toml parses");
        let desktop = doc["profile"]["desktop"].as_table().unwrap();
        assert_eq!(desktop["test-threads"].as_integer(), Some(1));
        assert_eq!(desktop["retries"].as_integer(), Some(0));
        assert_eq!(desktop["slow-timeout"]["period"].as_str(), Some("60s"));
        assert_eq!(
            desktop["slow-timeout"]["terminate-after"].as_integer(),
            Some(5)
        );
        assert_eq!(desktop["junit"]["path"].as_str(), Some("junit.xml"));
        assert_eq!(doc["store"]["dir"].as_str(), Some("target/nextest"));
        // No setup scripts: there is no cargo in the Sandbox.
        assert!(!doc.contains_key("scripts"), "{text}");
        assert!(!desktop.contains_key("scripts"), "{text}");
        assert!(!doc.contains_key("experimental"), "{text}");
        // The host run reads the same file.
        assert_eq!(
            std::fs::read_to_string(super::super::root::built_root().join(NEXTEST_SANDBOX_PATH))
                .unwrap()
                .replace("\r\n", "\n"),
            NEXTEST_SANDBOX.replace("\r\n", "\n")
        );
    }

    #[test] // spec: QA-042, QA-056
    fn the_bundle_holds_the_testbins_and_the_vc_runtime() {
        let fx = Fx::new();
        let l = fx.layout();
        fx.run(&cfg(Suite::Desktop), &fx.launcher(finishes()))
            .unwrap();
        for b in TESTBINS {
            assert!(l.in_dir.join("testbins").join(b).is_file(), "{b}");
        }
        // Only the binaries, not cargo's folders beside them.
        assert!(!l.in_dir.join(r"testbins\deps").exists());
        assert!(!l.in_dir.join(r"testbins\lil.pdb").exists());
        for d in VC_RUNTIME {
            assert!(l.in_dir.join("vcrt").join(d).is_file(), "{d}");
        }
        for f in [
            "cargo-nextest.exe",
            "desktop.tar.zst",
            "nextest-sandbox.toml",
            "run-inside.ps1",
            "in-sandbox.marker",
        ] {
            assert!(l.in_dir.join(f).is_file(), "{f}");
        }
        // The tracked files only.
        for f in TRACKED {
            assert!(l.in_dir.join("src").join(f).is_file(), "{f}");
        }
        assert!(!l.in_dir.join(r"src\untracked.txt").exists());
        let config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(l.in_dir.join("config.json")).unwrap())
                .unwrap();
        assert_eq!(config["suite"], "desktop");
        assert_eq!(config["keep_open"], false);
        // The same build as the nextest setup script's.
        let build = fx.cmd("cargo", "build");
        assert_eq!(
            build.args.join(" "),
            "build --locked --offline --bins -p lil-cli -p lil-core -p lil-mcp -p lil-testkit \
             --target-dir target/testbins"
        );
        assert_eq!(build.cwd, fx.repo);
    }

    #[test] // spec: QA-042
    fn qml_suite_leaves_out_lil_qt_e2e_and_sets_the_grab_dir() {
        let mut fx = Fx::new();
        let l = fx.layout();
        std::fs::create_dir_all(&l.build_tree).unwrap();
        fx.host.qt = Some(fx._tmp.path().join("Qt"));
        fx.host.ctest_json = r#"{"kind":"ctestInfo","version":{"major":1,"minor":0},"tests":[
            {"name":"tst_boot","command":["C:/b/tst_boot.exe"],"properties":[{"name":"ENVIRONMENT","value":["QT_QPA_PLATFORM=offscreen"]}]},
            {"name":"lil_qt_e2e","command":["cargo","test"]},
            {"name":"lazy_web","command":["C:/b/lazy_web.exe"]}]}"#
            .into();
        fx.run(&cfg(Suite::Grabs), &fx.launcher(finishes()))
            .unwrap();
        let bundled: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(l.in_dir.join("ctest.json")).unwrap())
                .unwrap();
        let names: Vec<&str> = bundled["tests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["tst_boot", "lazy_web"]);
        assert_eq!(bundled["kind"], "ctestInfo");
        assert_eq!(
            fx.cmd("ctest", "--preset").args,
            ["--preset", "grabs", "--show-only=json-v1"]
        );
        let config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(l.in_dir.join("config.json")).unwrap())
                .unwrap();
        assert_eq!(config["suite"], "grabs");
        assert_eq!(
            config["qt_dir"],
            fx._tmp.path().join("Qt").display().to_string()
        );
        for needed in [
            r"$env:LIL_GRAB_DIR = Join-Path $Out 'grabs'",
            r"Remove-Item Env:\QT_QPA_PLATFORM",
            r"$_ -notmatch '^QT_QPA_PLATFORM='",
            r#"@('-o', "$xml,junitxml")"#,
            r"(Join-Path $config.qt_dir 'bin')",
        ] {
            assert!(RUN_INSIDE.contains(needed), "run-inside.ps1 lacks {needed}");
        }
    }

    #[test] // spec: QA-042
    fn a_host_folder_system_cannot_read_exits_2() {
        let mut fx = Fx::new();
        let l = fx.layout();
        fx.host.refuse = vec![l.in_dir.clone()];
        let launcher = fx.launcher(finishes());
        let result = fx.run(&cfg(Suite::Desktop), &launcher);
        assert_eq!(exit_code(&result), 2);
        let err = result.unwrap_err();
        assert!(err.0.contains(&l.in_dir.display().to_string()), "{err}");
        assert!(err.0.contains("SYSTEM"), "{err}");
        assert!(launcher.started.borrow().is_none(), "never started");
        // Every mapped folder was checked, the results folder as writable.
        let checked = fx.host.mapped.borrow();
        assert!(checked.iter().any(|m| m.host == l.in_dir && m.read_only));
        assert!(checked.iter().any(|m| m.host == l.out_dir && !m.read_only));
    }

    #[test] // spec: QA-042
    fn the_archive_command_has_no_test_predicate() {
        let fx = Fx::new();
        let l = fx.layout();
        fx.run(&cfg(Suite::Desktop), &fx.launcher(finishes()))
            .unwrap();
        let archive = fx.cmd("cargo", "archive");
        let line = archive.line();
        assert!(!line.contains("test("), "{line}");
        assert!(!line.contains("--run-ignored"), "{line}");
        assert!(!archive.args.iter().any(|a| a == "-E"), "{line}");
        let file = l.in_dir.join("desktop.tar.zst").display().to_string();
        assert_eq!(
            archive.args,
            [
                "nextest",
                "archive",
                "-p",
                "lil-platform",
                "--archive-file",
                file.as_str(),
            ]
        );
        // lil-qt joins once it exists.
        let q = fx.repo.join(r"crates\lil-qt");
        std::fs::create_dir_all(&q).unwrap();
        std::fs::write(q.join("Cargo.toml"), "").unwrap();
        let args = archive_command(&fx.repo, Path::new("a.tar.zst")).args;
        assert_eq!(
            args,
            [
                "nextest",
                "archive",
                "-p",
                "lil-platform",
                "-p",
                "lil-qt",
                "--archive-file",
                "a.tar.zst"
            ]
        );
    }

    #[test] // spec: QA-042, QA-086
    fn the_wsb_is_written_under_build_and_never_tracked() {
        let fx = Fx::new();
        let l = fx.layout();
        assert_eq!(l.wsb, fx.repo.join(r"build\sandbox\lilith-desktop.wsb"));
        fx.run(&cfg(Suite::Desktop), &fx.launcher(finishes()))
            .unwrap();
        assert!(l.wsb.is_file());
        // This checkout ignores it, and everything else the run writes under build\.
        for path in [
            "build/sandbox/lilith-desktop.wsb",
            "build/sandbox/in/config.json",
            "build/sandbox/out/done.json",
        ] {
            let status = super::super::root::git_command()
                .arg("-C")
                .arg(super::super::root::built_root())
                .args(["check-ignore", "-q", path])
                .status()
                .unwrap();
            assert!(status.success(), "{path} isn't ignored");
        }
    }

    #[test] // spec: QA-042
    fn the_out_folder_is_made_and_emptied_before_the_start() {
        let fx = Fx::new();
        let l = fx.layout();
        // An earlier run's files: a done.json that would end this run's wait at once.
        std::fs::create_dir_all(l.out_dir.join("grabs")).unwrap();
        for f in ["done.json", "started.json", "old.xml", r"grabs\a.png"] {
            std::fs::write(l.out_dir.join(f), "{}").unwrap();
        }
        let launcher = fx.launcher(finishes());
        fx.run(&cfg(Suite::Desktop), &launcher).unwrap();
        let (exists, empty, _) = launcher.started.borrow().clone().unwrap();
        assert!(exists && empty, "exists {exists}, empty {empty}");
        // And it is made when there is none.
        let fx = Fx::new();
        let launcher = fx.launcher(finishes());
        fx.run(&cfg(Suite::Desktop), &launcher).unwrap();
        let (exists, empty, _) = launcher.started.borrow().clone().unwrap();
        assert!(exists && empty);
    }

    // -----------------------------------------------------------------------------------------
    // The run's lifecycle.
    // -----------------------------------------------------------------------------------------

    #[test] // spec: QA-042
    fn refuses_when_the_sandbox_is_missing() {
        let fx = Fx::new();
        let mut launcher = fx.launcher(finishes());
        launcher.availability = Availability::Missing;
        let result = fx.run(&cfg(Suite::Desktop), &launcher);
        assert_eq!(exit_code(&result), 2);
        let err = result.unwrap_err();
        assert_eq!(err.0, OWNER_STEP);
        assert!(err.0.contains("Windows Sandbox") && err.0.contains("restart"));
        assert!(fx.host.cmds.borrow().is_empty(), "no bundle is built");
        assert!(launcher.started.borrow().is_none());
    }

    #[test] // spec: QA-042
    fn refuses_while_another_sandbox_runs() {
        let fx = Fx::new();
        let mut launcher = fx.launcher(finishes());
        launcher.running = true;
        let result = fx.run(&cfg(Suite::Desktop), &launcher);
        assert_eq!(exit_code(&result), 2);
        assert_eq!(
            result.unwrap_err().0,
            "Only one Windows Sandbox can run at a time; close the open one, then run this again."
        );
        assert!(launcher.started.borrow().is_none());
        assert_eq!(
            launcher.stops.get(),
            0,
            "never stops a Sandbox it didn't start"
        );
    }

    #[test] // spec: QA-042
    fn gives_up_when_the_sandbox_never_starts() {
        let fx = Fx::new();
        let launcher = fx.launcher(Box::new(|_: &Path| {}));
        let result = fx.run(&cfg(Suite::Desktop), &launcher);
        assert_eq!(exit_code(&result), 2);
        assert!(
            result
                .unwrap_err()
                .0
                .contains("Windows Sandbox didn't start"),
            "the message"
        );
        assert_eq!(launcher.stops.get(), 1);
        let waited = fx.clock.now();
        assert!(
            waited >= START_TIMEOUT && waited < START_TIMEOUT + Duration::from_secs(5),
            "{waited:?}"
        );
    }

    #[test] // spec: QA-042
    fn collects_results_and_reports_a_failure() {
        let fx = Fx::new();
        let l = fx.layout();
        let launcher = fx.launcher(Box::new(|out: &Path| {
            std::fs::write(out.join("started.json"), "{}").unwrap();
            std::fs::write(
                out.join("a.xml"),
                junit(&[("desktop_power_request_sets_and_clears", true)]),
            )
            .unwrap();
            std::fs::write(
                out.join("b.xml"),
                junit(&[(
                    "desktop_shortcut_and_handlers_round_trip_under_a_test_scheme",
                    false,
                )]),
            )
            .unwrap();
            std::fs::write(out.join("run-inside.log"), "log").unwrap();
            // As Windows PowerShell 5.1 writes it: UTF-8 with a BOM.
            std::fs::write(
                out.join("done.json"),
                "\u{feff}{\"suite\":\"desktop\",\"exit\":100,\"tests\":2,\"passed\":1,\"failed\":1,\
                 \"nextest_version\":\"cargo-nextest 0.9.148\"}",
            )
            .unwrap();
        }));
        let result = fx.run(&cfg(Suite::Desktop), &launcher);
        assert_eq!(exit_code(&result), 1);
        let summary = result.unwrap();
        assert_eq!(
            summary.tests,
            [
                TestResult {
                    name: "lil-platform::desktop::desktop_power_request_sets_and_clears".into(),
                    passed: true
                },
                TestResult {
                    name: "lil-platform::desktop::desktop_shortcut_and_handlers_round_trip_under_a_test_scheme".into(),
                    passed: false
                },
            ]
        );
        assert_eq!(
            summary.nextest_version.as_deref(),
            Some("cargo-nextest 0.9.148")
        );
        for f in ["a.xml", "b.xml", "done.json", "run-inside.log"] {
            assert!(l.results.join(f).is_file(), "{f} copied");
        }
        assert_eq!(summary.results.as_deref(), Some(l.results.as_path()));
        assert_eq!(launcher.stops.get(), 1);
        // All passed: exit 0; passed tests but a non-zero exit: 1.
        let fx = Fx::new();
        assert_eq!(
            exit_code(&fx.run(&cfg(Suite::Desktop), &fx.launcher(finishes()))),
            0
        );
        let fx = Fx::new();
        let launcher = fx.launcher(Box::new(|out: &Path| {
            finishes()(out);
            std::fs::write(out.join("done.json"), r#"{"exit":4}"#).unwrap();
        }));
        let s = fx.run(&cfg(Suite::Desktop), &launcher).unwrap();
        assert_eq!(s.exit_code(), 1);
        assert_eq!(s.problems, ["the tests exited 4"]);
    }

    #[test] // spec: QA-042
    fn times_out_and_stops_the_sandbox() {
        let fx = Fx::new();
        let launcher = fx.launcher(Box::new(|out: &Path| {
            std::fs::write(out.join("started.json"), "{}").unwrap();
        }));
        let mut c = cfg(Suite::Desktop);
        c.timeout = Duration::from_secs(5);
        let result = fx.run(&c, &launcher);
        assert_eq!(exit_code(&result), 2);
        assert!(result.unwrap_err().0.contains("didn't finish within 5 s"));
        assert_eq!(launcher.stops.get(), 1);
        assert!(fx.clock.now() >= Duration::from_secs(5));
    }

    #[test] // spec: QA-042
    fn keep_open_never_calls_stop() {
        let mut c = cfg(Suite::Desktop);
        c.keep_open = true;
        let fx = Fx::new();
        let launcher = fx.launcher(finishes());
        assert_eq!(exit_code(&fx.run(&c, &launcher)), 0);
        assert_eq!(launcher.stops.get(), 0);
        // run-inside.ps1 leaves it open too.
        let config: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(fx.layout().in_dir.join("config.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(config["keep_open"], true);
        assert!(RUN_INSIDE.contains("if (-not $config.keep_open) { shutdown /s /t 0 }"));
        // Not on a timeout either.
        let fx = Fx::new();
        let launcher = fx.launcher(Box::new(|_: &Path| {}));
        assert_eq!(exit_code(&fx.run(&c, &launcher)), 2);
        assert_eq!(launcher.stops.get(), 0);
    }

    // -----------------------------------------------------------------------------------------
    // The real launcher, on a fake system.
    // -----------------------------------------------------------------------------------------

    const ID: &str = "f185da8e-518e-4bc0-96c0-ca0937db2bdd";
    const CONNECT_PID: u32 = 900;

    #[derive(Default)]
    struct FakeSys {
        wsb: Option<PathBuf>,
        inbox: bool,
        list_works: bool,
        listed: RefCell<Vec<String>>,
        procs: RefCell<Vec<(u32, String)>>,
        /// The processes the Sandbox brings up when it starts.
        brings_up: Vec<(u32, String)>,
        alive: RefCell<HashSet<u32>>,
        /// Whether a client closes when asked.
        closes: bool,
        /// Sleeps until the VM shuts itself down (`None`: never).
        self_stop_after: Cell<Option<u32>>,
        /// `spawn` (`wsb connect`) fails.
        spawn_fails: bool,
        calls: RefCell<Vec<String>>,
    }

    impl FakeSys {
        fn store() -> FakeSys {
            FakeSys {
                wsb: Some(PathBuf::from(r"C:\apps\wsb.exe")),
                inbox: true,
                list_works: true,
                procs: RefCell::new(vec![(100, "WindowsSandboxServer".into())]),
                brings_up: vec![
                    (200, "WindowsSandboxRemoteSession".into()),
                    (300, "vmmemWindowsSandbox".into()),
                ],
                closes: true,
                ..FakeSys::default()
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }

        fn gone(&self, pid: u32) {
            self.alive.borrow_mut().remove(&pid);
            self.procs.borrow_mut().retain(|(p, _)| *p != pid);
        }
    }

    impl LauncherSys for Rc<FakeSys> {
        fn wsb(&self) -> Option<PathBuf> {
            self.wsb.clone()
        }
        fn inbox_exe(&self) -> PathBuf {
            PathBuf::from(r"C:\Windows\System32\WindowsSandbox.exe")
        }
        fn exists(&self, path: &Path) -> bool {
            self.inbox && path == self.inbox_exe()
        }
        fn output(&self, program: &Path, args: &[String]) -> io::Result<(i32, String)> {
            self.calls.borrow_mut().push(format!(
                "output {} {}",
                program.display(),
                args.join(" ")
            ));
            match args.first().map(String::as_str) {
                Some("list") if self.list_works => Ok((0, self.listed.borrow().join("\r\n"))),
                Some("list") => Ok((1, String::new())),
                Some("start") => {
                    self.listed.borrow_mut().push(ID.into());
                    for p in &self.brings_up {
                        self.procs.borrow_mut().push(p.clone());
                        self.alive.borrow_mut().insert(p.0);
                    }
                    Ok((
                        0,
                        format!("Windows Sandbox environment started successfully:\r\nId: {ID}"),
                    ))
                }
                Some("stop") => {
                    self.listed.borrow_mut().clear();
                    self.gone(300);
                    Ok((0, String::new()))
                }
                _ => Ok((1, String::new())),
            }
        }
        fn spawn(&self, program: &Path, args: &[String]) -> io::Result<u32> {
            self.calls
                .borrow_mut()
                .push(format!("spawn {} {}", program.display(), args.join(" ")));
            if self.spawn_fails {
                return Err(io::Error::other("no session"));
            }
            self.alive.borrow_mut().insert(CONNECT_PID);
            Ok(CONNECT_PID)
        }
        fn processes(&self) -> io::Result<Vec<(u32, String)>> {
            Ok(self.procs.borrow().clone())
        }
        fn pin(&self, pid: u32) -> bool {
            self.calls.borrow_mut().push(format!("pin {pid}"));
            self.alive.borrow().contains(&pid)
        }
        fn alive(&self, pid: u32) -> bool {
            self.alive.borrow().contains(&pid)
        }
        fn close(&self, pid: u32) {
            self.calls.borrow_mut().push(format!("close {pid}"));
            if self.closes {
                self.gone(pid);
            }
        }
        fn kill(&self, pid: u32) {
            self.calls.borrow_mut().push(format!("kill {pid}"));
            self.gone(pid);
        }
        fn sleep(&self, _d: Duration) {
            if let Some(n) = self.self_stop_after.get() {
                if n <= 1 {
                    // run-inside.ps1's shutdown: the VM goes, the viewer stays.
                    self.listed.borrow_mut().clear();
                    self.gone(300);
                    self.self_stop_after.set(None);
                } else {
                    self.self_stop_after.set(Some(n - 1));
                }
            }
        }
    }

    fn wsb_file(dir: &Path) -> PathBuf {
        let path = dir.join("lilith-desktop.wsb");
        std::fs::write(
            &path,
            wsb_xml(&[Mapped {
                host: PathBuf::from(r"C:\r\build\sandbox\in"),
                sandbox: SANDBOX_IN.into(),
                read_only: true,
            }]),
        )
        .unwrap();
        path
    }

    #[test] // spec: QA-042
    fn uses_wsb_when_the_cli_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let wsb = wsb_file(dir.path());
        let xml = std::fs::read_to_string(&wsb).unwrap();

        let sys = Rc::new(FakeSys::store());
        let launcher = RealLauncher::with_sys(Box::new(sys.clone()));
        assert_eq!(launcher.availability(), Availability::Cli);
        assert!(!launcher.running());
        let h = launcher.start(&wsb).unwrap();
        assert_eq!(h.via, Availability::Cli);
        assert_eq!(h.id.as_deref(), Some(ID));
        assert_eq!(h.started, [CONNECT_PID]);
        assert_eq!(
            h.pids_before,
            Vec::<u32>::new(),
            "WindowsSandboxServer isn't one"
        );
        let calls = sys.calls();
        let start = format!(
            r"output C:\apps\wsb.exe start --config {}",
            wsb_one_line(&xml)
        );
        assert!(calls.contains(&start), "{calls:?}");
        assert!(!wsb_one_line(&xml).contains(['\r', '\n']));
        assert!(
            calls.contains(&format!(r"spawn C:\apps\wsb.exe connect --id {ID}")),
            "{calls:?}"
        );
        assert!(launcher.running(), "wsb list names it");

        // The inbox version: the file's path, to WindowsSandbox.exe.
        let sys = Rc::new(FakeSys {
            inbox: true,
            ..FakeSys::default()
        });
        let launcher = RealLauncher::with_sys(Box::new(sys.clone()));
        assert_eq!(launcher.availability(), Availability::Inbox);
        let h = launcher.start(&wsb).unwrap();
        assert_eq!((h.via, h.id), (Availability::Inbox, None));
        assert_eq!(
            sys.calls()
                .iter()
                .filter(|c| c.starts_with("spawn") || c.starts_with("output"))
                .collect::<Vec<_>>(),
            [&format!(
                r"spawn C:\Windows\System32\WindowsSandbox.exe {}",
                wsb.display()
            )]
        );
        // A wsb that doesn't answer `wsb list` isn't the CLI.
        let sys = Rc::new(FakeSys {
            wsb: Some(PathBuf::from(r"C:\apps\wsb.exe")),
            inbox: true,
            ..FakeSys::default()
        });
        assert_eq!(
            RealLauncher::with_sys(Box::new(sys)).availability(),
            Availability::Inbox
        );
        // Neither.
        let sys = Rc::new(FakeSys::default());
        assert_eq!(
            RealLauncher::with_sys(Box::new(sys)).availability(),
            Availability::Missing
        );
    }

    #[test] // spec: QA-042
    fn stop_closes_the_runs_own_viewer_client() {
        let dir = tempfile::tempdir().unwrap();
        let wsb = wsb_file(dir.path());
        // The VM shuts itself down (run-inside.ps1); the viewer stays open until closed.
        let sys = Rc::new(FakeSys::store());
        sys.self_stop_after.set(Some(3));
        let launcher = RealLauncher::with_sys(Box::new(sys.clone()));
        let h = launcher.start(&wsb).unwrap();
        launcher.stop(&h).unwrap();
        let calls = sys.calls();
        assert!(calls.contains(&format!("close {CONNECT_PID}")), "{calls:?}");
        assert!(calls.contains(&"close 200".to_string()), "{calls:?}");
        assert!(!calls.iter().any(|c| c.contains("stop --id")), "{calls:?}");
        assert!(!calls.iter().any(|c| c.starts_with("kill")), "{calls:?}");
        assert!(
            !calls.iter().any(|c| c.ends_with(" 100")),
            "never touches the Sandbox server: {calls:?}"
        );
        assert!(!sys.alive(200) && !sys.alive(CONNECT_PID));

        // A VM that doesn't stop gets `wsb stop --id`; a viewer that doesn't close is ended.
        let sys = Rc::new(FakeSys {
            closes: false,
            ..FakeSys::store()
        });
        let launcher = RealLauncher::with_sys(Box::new(sys.clone()));
        let h = launcher.start(&wsb).unwrap();
        launcher.stop(&h).unwrap();
        let calls = sys.calls();
        assert!(
            calls.contains(&format!(r"output C:\apps\wsb.exe stop --id {ID}")),
            "{calls:?}"
        );
        assert!(calls.contains(&"close 200".to_string()), "{calls:?}");
        assert!(calls.contains(&"kill 200".to_string()), "{calls:?}");
        assert!(calls.contains(&format!("kill {CONNECT_PID}")), "{calls:?}");

        // running() ignores a lingering viewer client: with the CLI, and without it.
        let sys = Rc::new(FakeSys::store());
        sys.procs
            .borrow_mut()
            .push((200, "WindowsSandboxRemoteSession".into()));
        assert!(!RealLauncher::with_sys(Box::new(sys)).running());
        let sys = Rc::new(FakeSys {
            inbox: true,
            procs: RefCell::new(vec![
                (100, "WindowsSandboxServer".into()),
                (200, "WindowsSandboxRemoteSession".into()),
            ]),
            ..FakeSys::default()
        });
        let launcher = RealLauncher::with_sys(Box::new(sys.clone()));
        assert!(!launcher.running());
        sys.procs.borrow_mut().push((400, "WindowsSandbox".into()));
        assert!(launcher.running());
    }

    #[test]
    fn tasklist_lines_give_pids_and_names() {
        let text = "\"System Idle Process\",\"0\",\"Services\",\"0\",\"8 K\"\r\n\
                    \"wsb.exe\",\"900\",\"Console\",\"1\",\"10,000 K\"\r\n\
                    \"WindowsSandboxRemoteSession.EXE\",\"200\",\"Console\",\"1\",\"1 K\"\r\n\
                    INFO: nothing\r\n";
        assert_eq!(
            parse_tasklist(text),
            [
                (0, "System Idle Process".to_string()),
                (900, "wsb".to_string()),
                (200, "WindowsSandboxRemoteSession".to_string()),
            ]
        );
        assert_eq!(guids(&format!("Id: {ID}\r\nx-1234")), [ID]);
        assert!(guids("no sandboxes").is_empty());
    }

    // -----------------------------------------------------------------------------------------
    // The host run.
    // -----------------------------------------------------------------------------------------

    #[test] // spec: QA-042
    fn host_mode_refuses_when_elevated() {
        let mut fx = Fx::new();
        fx.host.elevated = true;
        let launcher = fx.launcher(finishes());
        let result = fx.run(&cfg(Suite::Host), &launcher);
        assert_eq!(exit_code(&result), 2);
        assert!(result.unwrap_err().0.contains("elevated"));
        assert!(
            fx.log.borrow().is_empty(),
            "nothing ran: {:?}",
            fx.log.borrow()
        );
        assert!(fx.host.popups.borrow().is_empty());
    }

    #[test] // spec: QA-042
    fn host_mode_shows_the_pop_up_before_running_only_the_host_set() {
        let fx = Fx::new();
        let launcher = fx.launcher(finishes());
        let summary = fx.run(&cfg(Suite::Host), &launcher).unwrap();
        assert_eq!(summary.exit_code(), 0, "{summary:?}");
        assert_eq!(summary.tests.len(), 2);
        assert!(launcher.started.borrow().is_none(), "no Sandbox");
        let log = fx.log.borrow().clone();
        let at = |prefix: &str| {
            log.iter()
                .position(|e| e.starts_with(prefix))
                .unwrap_or_else(|| panic!("no {prefix}: {log:?}"))
        };
        let (listed, popup, ran) = (
            at("output cargo nextest list"),
            at("popup"),
            at("run cargo nextest run"),
        );
        assert!(
            at("sweep") < listed && listed < popup && popup < ran,
            "{log:?}"
        );
        // At least 10 s of warning on screen before the tests start.
        let warned: u64 = log[popup..ran]
            .iter()
            .filter_map(|e| e.strip_prefix("sleep "))
            .map(|s| s.parse::<u64>().unwrap())
            .sum();
        assert!(warned >= 10, "{log:?}");
        let (warning, countdown, cap) = fx.host.popups.borrow()[0];
        assert!(warning >= Duration::from_secs(10));
        assert!(countdown > Duration::ZERO);
        // The pop-up outlasts the worst case: each of the 2 tests running until nextest ends it.
        assert!(cap >= Duration::from_secs(2 * 300), "{cap:?}");
        // Then the cleanup check, then "done".
        assert!(ran < at("check leftovers") && at("check leftovers") < at("popup done"));
        let run = fx.cmd("cargo", "run");
        let mut want: Vec<String> = [
            "nextest",
            "run",
            "-p",
            "lil-platform",
            "--run-ignored",
            "only",
        ]
        .map(String::from)
        .to_vec();
        want.extend(
            [
                "--config-file",
                NEXTEST_SANDBOX_PATH,
                "--profile",
                "desktop",
                "-E",
            ]
            .map(String::from),
        );
        want.push(host_filter());
        assert_eq!(run.args, want);
        assert!(run.env.contains(&("LIL_DESKTOP_HOST".into(), "1".into())));
        assert!(run.env.contains(&(
            "LIL_DEPLOY_DIR".into(),
            fx.layout().deploy.display().to_string()
        )));
        assert_eq!(run.env_remove, ["LIL_SANDBOX"]);
        assert!(fx.layout().results.join(r"host\junit.xml").is_file());
    }

    #[test] // spec: QA-042
    fn host_mode_checks_its_cleanup() {
        let fx = Fx::new();
        // An earlier killed run's leftovers are swept before anything runs.
        *fx.host.leftovers.borrow_mut() = vec![r"HKCU\Software\LILITH-test-desktop-1".into()];
        let launcher = fx.launcher(finishes());
        let summary = fx.run(&cfg(Suite::Host), &launcher).unwrap();
        assert_eq!(summary.exit_code(), 0, "{summary:?}");
        assert_eq!(fx.log.borrow()[0], "sweep");

        // This run's tests leave something: the run fails, names it and sweeps it.
        let fx = Fx::new();
        let leftover = r"HKCU\Software\Classes\lil-test-desktop-0123456789abcdef";
        let host = &fx.host;
        let log = fx.log.clone();
        let launcher = fx.launcher(finishes());
        // The leftover appears when the tests run.
        struct Leaving<'a>(&'a FakeHost, &'a str);
        impl HostOps for Leaving<'_> {
            fn run(&self, cmd: &Cmd) -> io::Result<i32> {
                let code = self.0.run(cmd)?;
                self.0.leftovers.borrow_mut().push(self.1.to_string());
                Ok(code)
            }
            fn output(&self, cmd: &Cmd) -> io::Result<(i32, String)> {
                self.0.output(cmd)
            }
            fn find_program(&self, name: &str) -> Option<PathBuf> {
                self.0.find_program(name)
            }
            fn vc_runtime(&self) -> Result<Vec<PathBuf>, String> {
                self.0.vc_runtime()
            }
            fn system_cannot_use(&self, mapped: &[Mapped]) -> io::Result<Vec<PathBuf>> {
                self.0.system_cannot_use(mapped)
            }
            fn elevated(&self) -> bool {
                false
            }
            fn show_hands_off(
                &self,
                what: &str,
                warning: Duration,
                run: Duration,
                cap: Duration,
            ) -> io::Result<Box<dyn HandsOff>> {
                self.0.show_hands_off(what, warning, run, cap)
            }
            fn process_alive(&self, pid: u32) -> bool {
                self.0.process_alive(pid)
            }
            fn desktop_leftovers(&self) -> io::Result<Vec<String>> {
                self.0.desktop_leftovers()
            }
            fn sweep_desktop_leftovers(&self) -> io::Result<Swept> {
                self.0.sweep_desktop_leftovers()
            }
            fn qt_dir(&self) -> Option<PathBuf> {
                None
            }
            fn interrupted(&self) -> bool {
                false
            }
        }
        let leaving = Leaving(host, leftover);
        let summary =
            run_with(&fx.repo, &cfg(Suite::Host), &leaving, &launcher, &fx.clock).unwrap();
        assert_eq!(summary.exit_code(), 1);
        assert_eq!(summary.failed(), 0);
        assert!(
            summary.problems.iter().any(|p| p.contains(leftover)),
            "{summary:?}"
        );
        let log = log.borrow();
        let check = log.iter().position(|e| e == "check leftovers").unwrap();
        assert_eq!(log[check + 1], "sweep", "{log:?}");
        assert!(host.leftovers.borrow().is_empty(), "swept");
        assert_eq!(log.last().unwrap(), "popup done");
    }

    #[test] // spec: QA-042
    fn ctrl_c_stops_the_sandbox_even_with_keep_open() {
        let fx = Fx::new();
        let interrupt = fx.host.interrupt.clone();
        let launcher = fx.launcher(Box::new(move |out: &Path| {
            std::fs::write(out.join("started.json"), "{}").unwrap();
            interrupt.set(true);
        }));
        let mut c = cfg(Suite::Desktop);
        c.keep_open = true;
        let result = fx.run(&c, &launcher);
        assert_eq!(exit_code(&result), 2);
        assert_eq!(result.unwrap_err().0, INTERRUPTED);
        assert_eq!(
            launcher.stops.get(),
            1,
            "the Sandbox and its viewer are closed"
        );
        // Ctrl+C while the Sandbox starts, before started.json: stopped too.
        let fx = Fx::new();
        let interrupt = fx.host.interrupt.clone();
        let launcher = fx.launcher(Box::new(move |_: &Path| interrupt.set(true)));
        assert_eq!(exit_code(&fx.run(&cfg(Suite::Desktop), &launcher)), 2);
        assert_eq!(launcher.stops.get(), 1);
    }

    #[test] // spec: QA-042
    fn ctrl_c_on_the_host_run_still_cleans_up() {
        let mut fx = Fx::new();
        let leftover = r"HKCU\Software\Classes\CLSID\{00000000-0000-5000-8000-000000000000}";
        fx.host.run_interrupted_leaving = Some(leftover.to_string());
        let launcher = fx.launcher(finishes());
        let result = fx.run(&cfg(Suite::Host), &launcher);
        assert_eq!(exit_code(&result), 2);
        let err = result.unwrap_err().0;
        assert!(err.starts_with(INTERRUPTED), "{err}");
        assert!(err.contains(leftover), "{err}");
        let log = fx.log.borrow();
        let ran = log
            .iter()
            .position(|e| e.starts_with("run cargo nextest run"))
            .unwrap();
        assert_eq!(
            log[ran + 1..],
            ["check leftovers", "sweep", "popup done"],
            "{log:?}"
        );
        assert!(fx.host.leftovers.borrow().is_empty(), "swept");
    }

    // -----------------------------------------------------------------------------------------
    // Fail-closed paths (review M2, M3, M5, M6, M7).
    // -----------------------------------------------------------------------------------------

    #[test] // spec: QA-042
    fn a_sandbox_run_without_junit_or_with_an_error_never_passes() {
        // exit 0, but no JUnit file came back: couldn't run.
        let fx = Fx::new();
        let launcher = fx.launcher(Box::new(|out: &Path| {
            std::fs::write(out.join("started.json"), "{}").unwrap();
            std::fs::write(out.join("done.json"), r#"{"exit":0,"tests":0}"#).unwrap();
        }));
        let result = fx.run(&cfg(Suite::Desktop), &launcher);
        assert_eq!(exit_code(&result), 2, "{result:?}");
        assert_eq!(launcher.stops.get(), 1);
        // Every test passed, but run-inside.ps1 reported an error: fails.
        let fx = Fx::new();
        let launcher = fx.launcher(Box::new(|out: &Path| {
            finishes()(out);
            std::fs::write(
                out.join("done.json"),
                r#"{"exit":0,"error":"Copy-Item failed"}"#,
            )
            .unwrap();
        }));
        let s = fx.run(&cfg(Suite::Desktop), &launcher).unwrap();
        assert_eq!(s.exit_code(), 1);
        assert_eq!(s.problems, ["run-inside.ps1 failed: Copy-Item failed"]);
        // No exit code at all (a crash before nextest returned): fails.
        let fx = Fx::new();
        let launcher = fx.launcher(Box::new(|out: &Path| {
            finishes()(out);
            std::fs::write(out.join("done.json"), r#"{"exit":null}"#).unwrap();
        }));
        assert_eq!(
            fx.run(&cfg(Suite::Desktop), &launcher).unwrap().exit_code(),
            1
        );
    }

    #[test] // spec: QA-042
    fn a_panic_in_the_wait_still_closes_the_sandbox() {
        let fx = Fx::new();
        fx.clock.panics.set(true);
        let launcher = fx.launcher(Box::new(|_: &Path| {}));
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            fx.run(&cfg(Suite::Desktop), &launcher)
        }));
        assert!(caught.is_err(), "the fake clock panics");
        assert_eq!(launcher.stops.get(), 1, "the guard stopped the Sandbox");
    }

    #[test] // spec: QA-042
    fn a_failed_connect_stops_the_started_vm() {
        let dir = tempfile::tempdir().unwrap();
        let wsb = wsb_file(dir.path());
        let sys = Rc::new(FakeSys {
            spawn_fails: true,
            ..FakeSys::store()
        });
        let launcher = RealLauncher::with_sys(Box::new(sys.clone()));
        assert!(launcher.start(&wsb).is_err());
        let calls = sys.calls();
        assert!(
            calls.contains(&format!(r"output C:\apps\wsb.exe stop --id {ID}")),
            "{calls:?}"
        );
        assert!(!launcher.running(), "nothing is left running");
    }

    #[test] // spec: QA-042
    fn a_host_run_without_junit_or_with_only_skipped_cases_exits_2() {
        for exit in [0, 1] {
            let mut fx = Fx::new();
            fx.host.host_writes_junit = false;
            fx.host.host_exit = exit;
            let result = fx.run(&cfg(Suite::Host), &fx.launcher(finishes()));
            assert_eq!(exit_code(&result), 2, "exit {exit}: {result:?}");
            assert!(result.unwrap_err().0.contains("no JUnit file"));
            // The cleanup ran all the same.
            let log = fx.log.borrow();
            assert!(log.contains(&"check leftovers".to_string()), "{log:?}");
            assert_eq!(log.last().unwrap(), "popup done");
        }
        let mut fx = Fx::new();
        fx.host.host_junit = r#"<testsuites><testsuite name="s"><testcase name="desktop_x" classname="c"><skipped/></testcase></testsuite></testsuites>"#.into();
        let result = fx.run(&cfg(Suite::Host), &fx.launcher(finishes()));
        assert_eq!(exit_code(&result), 2, "{result:?}");
        assert!(result.unwrap_err().0.contains("no host test ran"));
    }

    #[test] // spec: QA-042
    fn a_closed_pop_up_stops_the_host_run_before_the_tests() {
        let fx = Fx::new();
        fx.host.popup_closed.set(true);
        let result = fx.run(&cfg(Suite::Host), &fx.launcher(finishes()));
        assert_eq!(exit_code(&result), 2);
        assert!(result.unwrap_err().0.contains("pop-up closed"));
        let log = fx.log.borrow();
        assert!(
            !log.iter().any(|e| e.starts_with("run cargo nextest run")),
            "no test ran without the pop-up: {log:?}"
        );
        assert_eq!(log.last().unwrap(), "popup done");
    }

    #[test] // spec: QA-042
    fn one_host_run_at_a_time() {
        let mut fx = Fx::new();
        let lock = fx.layout().root.join(r"host\host-run.lock");
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        // A live run holds the lock: refused before anything is swept.
        std::fs::write(&lock, "4242").unwrap();
        fx.host.alive_pids = vec![4242];
        let result = fx.run(&cfg(Suite::Host), &fx.launcher(finishes()));
        assert_eq!(exit_code(&result), 2);
        assert!(
            result
                .unwrap_err()
                .0
                .contains("another host run is in progress")
        );
        assert!(fx.log.borrow().is_empty(), "{:?}", fx.log.borrow());
        assert!(lock.is_file(), "the live run's lock is left alone");
        // A dead owner's lock is stale: taken over, and gone after the run.
        fx.host.alive_pids.clear();
        let summary = fx.run(&cfg(Suite::Host), &fx.launcher(finishes())).unwrap();
        assert_eq!(summary.exit_code(), 0);
        assert!(!lock.exists());
    }

    #[test] // spec: QA-042
    fn the_sandbox_filter_leaves_out_the_host_set() {
        let filter = sandbox_filter();
        assert!(filter.starts_with("test(/^desktop_/) - ("), "{filter}");
        for t in HOST_SET {
            assert!(filter.contains(&format!("test(={t})")), "{t}");
            assert!(host_filter().contains(&format!("test(={t})")), "{t}");
        }
        assert_eq!(host_filter().matches("test(=").count(), 4);
        // P4a-R39's toast tests are in the host set; the rest of Task 5's group isn't.
        for t in [
            "desktop_toast_posts_under_the_shortcut_aumid",
            "desktop_toasts_state_is_ready_on_this_pc",
        ] {
            assert!(HOST_SET.contains(&t));
        }
        for t in [
            "desktop_shortcut_and_handlers_round_trip_under_a_test_scheme",
            "desktop_power_request_sets_and_clears",
        ] {
            assert!(!HOST_SET.contains(&t) && !host_filter().contains(t));
        }
        // run-inside.ps1 runs exactly this filter.
        assert!(
            RUN_INSIDE.contains(&format!("$Filter = '{filter}'")),
            "run-inside.ps1's filter isn't {filter}"
        );
    }
}
