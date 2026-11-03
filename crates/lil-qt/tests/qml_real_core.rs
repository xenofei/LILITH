//! The shell's QML against a real test-channel core (ledger TC-01: Task 20's review findings C1 and
//! I3, through the UI's own paths): runs `apps/lil-ui-kit/tests/tst_real_core.qml` in the QML test
//! runner, which CMake builds. CTest runs it as `tst_real_core` (label `qml`):
//! `cargo test --locked -p lil_qt --test qml_real_core -- --ignored`, with `LIL_QML_RUNNER` naming
//! the runner, `LIL_QML_REAL_CORE_TEST` the QML file and the Qt pair's `bin` on `PATH`. Without
//! them there is nothing to run, so the test is ignored by default.
//!
//! A real core grants `ui:image` only to `lil-image.exe` in the folder of its own executable
//! (ARCH-045, ARCH-046). So the launcher copies the test binaries' `lil-core.exe` and `lil.exe`
//! and the runner, named `lil-image.exe`, into a fresh folder, registers two workspaces on a fresh
//! test channel (the first the default), and runs the copy there with `LIL_QML_REAL_CORE=1`: its
//! `Core` connects as lil-image's does and starts that folder's core. Afterwards the channel's core
//! is stopped and the folder removed.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use lil_testkit::{TestEnv, ensure_built};

fn var(name: &str) -> PathBuf {
    PathBuf::from(
        std::env::var_os(name)
            .unwrap_or_else(|| panic!("{name} isn't set (CTest's tst_real_core sets it)")),
    )
}

#[test] // spec: APP-006, ARCH-030, ARCH-090, ARCH-100, APP-034
#[ignore = "CTest's tst_real_core runs it: it needs the QML test runner that CMake builds"]
fn the_shell_s_workspace_paths_against_a_real_core() {
    let runner = var("LIL_QML_RUNNER");
    let qml = var("LIL_QML_REAL_CORE_TEST");
    let env = TestEnv::new();
    let a = env.make_workspace("a");
    let b = env.make_workspace("b");
    env.register_workspace(&a, true);
    env.register_workspace(&b, false);

    let folder = tempfile::Builder::new()
        .prefix("lil-qml-real-core-")
        .tempdir()
        .expect("a folder for the copies");
    let bins = ensure_built();
    std::fs::copy(&bins.lil_core, folder.path().join("lil-core.exe")).expect("copy lil-core.exe");
    std::fs::copy(&bins.lil, folder.path().join("lil.exe")).expect("copy lil.exe");
    let app = folder.path().join("lil-image.exe");
    std::fs::copy(&runner, &app).expect("copy the runner as lil-image.exe");
    // The offscreen platform's 1280 x 800 screen, read from the working folder.
    let screen = qml.with_file_name("offscreen-screen.json");
    std::fs::copy(&screen, folder.path().join("offscreen-screen.json"))
        .expect("copy offscreen-screen.json");
    // The runner's own folder (KDDockWidgets' DLL and the shim's) goes first on PATH.
    let runner_dir = runner.parent().expect("the runner's folder").to_path_buf();
    let mut path =
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect::<Vec<_>>();
    path.insert(0, runner_dir);
    let path = std::env::join_paths(path).expect("PATH");

    let out = folder.path().join("runner.out");
    let mut cmd = Command::new(&app);
    cmd.arg("-input")
        .arg(&qml)
        .current_dir(folder.path())
        .envs(env.child_env())
        .env("PATH", path)
        .env("LIL_QML_REAL_CORE", "1")
        .env("LIL_QML_LIL", folder.path().join("lil.exe"))
        // Canonical, as the core lists them: TEMP can be an 8.3 short path (GitHub's runners), and
        // the QML compares the paths as text.
        .env(
            "LIL_QML_WS_A",
            std::fs::canonicalize(&a).expect("canonical a"),
        )
        .env(
            "LIL_QML_WS_B",
            std::fs::canonicalize(&b).expect("canonical b"),
        )
        .env(
            "QT_QPA_PLATFORM",
            "offscreen:configfile=offscreen-screen.json",
        )
        .env("QT_FORCE_STDERR_LOGGING", "1")
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(&out).expect("runner.out"))
        .stderr(std::fs::File::create(folder.path().join("runner.err")).expect("runner.err"));
    let mut child = cmd.spawn().expect("start the runner");
    let deadline = Instant::now() + Duration::from_secs(400);
    let status = loop {
        if let Some(status) = child.try_wait().expect("the runner's status") {
            break Some(status);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let read = |name: &str| std::fs::read_to_string(folder.path().join(name)).unwrap_or_default();
    let (stdout, stderr) = (read("runner.out"), read("runner.err"));
    println!("--- the runner's stdout\n{stdout}\n--- its stderr\n{stderr}");

    // The channel's core, started by the app's session: stopped through the channel's own `lil`.
    let stop = Command::new(folder.path().join("lil.exe"))
        .args(["--no-start", "core", "stop"])
        .current_dir(env.root())
        .envs(env.child_env())
        .output();
    if let Ok(stop) = &stop {
        println!("lil --no-start core stop: {:?}", stop.status.code());
    }
    std::thread::sleep(Duration::from_millis(500));

    assert!(
        status.is_some_and(|s| s.success()),
        "tst_real_core ended {status:?} (see its output above)"
    );
    assert!(
        stdout.contains("Totals: ") && stdout.contains(" 0 failed"),
        "tst_real_core's totals (see its output above)"
    );
    // A scanner may still hold a copy for a moment.
    for _ in 0..50 {
        if std::fs::remove_dir_all(folder.path()).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
