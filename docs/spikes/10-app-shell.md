# 10 App shell: KDDockWidgets 2.5, the `lil-qt` route, lazy WebEngine, deploy, cold start, stalls, Narrator

Date: 2026-10-21

GPU: NVIDIA GeForce RTX 5080 16 GB, driver 617.42. Qt 6.12.0 + Qt WebEngine 6.140.0. ComfyUI 0.42.1 (used only by S0.6, which the owner starts).

## 1. Question

Plan 4a, Task 0. Before the shell is built (Tasks 15–29), does the Qt stack it will use hold up?

- **APP-005:** does KDDockWidgets **2.5.0** build cleanly on Qt 6.12.0 and save and restore a layout of several windows, including one window alone (`LayoutSaver::addWindowToSave()`)? Otherwise 2.4.1 stays.
- **The `lil-qt` route (P4a-R1):** can `lil-qt` be a cargo workspace member with its Qt parts behind a `qt` feature, built by CMake through `cxx_qt_import_crate(... FEATURES qt LOCKED)` against the workspace `Cargo.lock`, while the workspace still builds and lints with no Qt?
- **APP-137, IMG-126:** does WebEngine stay out of the process until its dock is shown when it lives in its own QML plugin DLL, with a named persistent profile from `WebEngineProfilePrototype` stored under the app's own local folder?
- **ARCH-114, OPS-094:** what do the `windeployqt` flags and the `--skip-plugin-types` list leave in the deploy folder, does the trimmed folder run with no Qt or MSVC on PATH, and how big is it?
- **APP-136:** is the deployed app interactive in under 2 s from process start, warm, after a reboot and after a Defender scan?
- **APP-135:** does the GUI thread stay under 16 ms p99 with 8 previews at 10 Hz, a paged 10,000-row list scrolling and 6 live charts, while ComfyUI saturates the GPU? How much dedicated VRAM does the shell itself use with a web page shown (P4a-R15)?
- **APP-156:** does Narrator read KDDockWidgets' title bars and tabs and the spike's own controls?
- **QA-068, P4a-R32, P4a-R35:** do Qt Quick, KDDockWidgets, Shapes, a WebEngine `Loader` and grabs work under `QT_QPA_PLATFORM=offscreen`, locally and on a `windows-2025` runner, and how long does the `pr-ready` job take with Qt in it?
- **Caches (ARCH-095):** with the organization and application names set, do Qt's QML and pipeline caches land under the app's own local folder?

## 2. Setup

| Item | Version |
|---|---|
| OS | Windows 11 Pro 10.0.26200 |
| GPU / driver | NVIDIA GeForce RTX 5080 16 GB, 617.42 |
| Rust | rustc / cargo 1.99.0, `stable-x86_64-pc-windows-msvc` |
| CMake / Ninja | 4.4.4 / 1.13.2 |
| MSVC | cl 19.44.35227 (VS 2022 Build Tools, toolset 14.44.35207), Windows SDK 10.0.26100; VC++ redist folder 14.44.35112 |
| Qt | 6.12.0 `msvc2022_64` at `C:\Qt\6.12.0\msvc2022_64` (`spikes/toolchain/env.ps1`) |
| Qt WebEngine | 6.140.0 (installed by `naqt`) |
| cxx-qt | `cxx-qt`, `cxx-qt-lib`, `cxx-qt-build` = 0.10.0 (pinned `=0.10.0`); `spikes/app-shell/rust/Cargo.lock` committed |
| cxx-qt CMake | `KDAB/cxx-qt-cmake` tag `0.10.0` (FetchContent), which fetches Corrosion v0.5.2 |
| KDDockWidgets | `v2.5.0` (FetchContent, commit 884b00a3), `KDDockWidgets_QT6=ON`, `KDDockWidgets_FRONTENDS=qtquick`; fallback `v2.4.1` (commit c1d28d25) |
| ComfyUI | 0.42.1 at `%USERPROFILE%\ComfyUI\ComfyUI`, started only by `comfy-load.ps1` on `127.0.0.1` |
| Spike code | `spikes/app-shell/` in the commit that adds the spike code (recorded in §5) |

## 3. Method

### The spike app (`spikes/app-shell/`)

- `main.cpp` sets the organization name `LILITH-spike` and the application name `image` and `Qt::AA_ShareOpenGLContexts` before the application object exists, calls `KDDockWidgets::initFrontend(QtQuick)`, installs the shim's dock factory, and loads `qml/Main.qml`.
- `qml/Main.qml`: two main windows, each a KDDockWidgets `DockingArea`:
  - `main-a` holds the Jobs dock (`JobsSim.qml`: 8 job tiles with a preview `Image` from the image provider and a progress bar) and the Grid dock (`GridSim.qml`: a `ListView` over the paged list model), with the Web dock (`WebPanel.qml`) tabbed behind Grid;
  - `main-b` holds the Charts dock (`ChartsSim.qml`: 6 Qt Quick Shapes with a `PathPolyline` each, P4a-R17; the chart tile is a delegate with `Accessible.role` and `Accessible.name`);
  - the Counter dock (a button bound to the Rust counter, with an accessible name, and an `Accessible.announce()` on each click) starts floating, so the layout has 3 windows: `main-a`, `main-b` and one floating window.
- `qml/WebPanel.qml` is the only file that imports `SpikeWeb`. Its `Loader` is activated the first time its dock is really shown.
- `web/` is the separate QML plugin `SpikeWeb` (its own DLL, built with `qt_add_qml_module`), the only code that imports `QtWebEngine`: `SpikeWebView.qml` is a `WebEngineView` whose profile is a named persistent profile made by `WebEngineProfilePrototype` (storage name `image`, storage and cache paths under `%LOCALAPPDATA%\LILITH-spike\image\`). It loads `about:blank` unless told otherwise.
- `shim/` is a DLL: the dock factory (a `KDDockWidgets::QtQuick::ViewFactory` subclass that themes the title bar through its own QML), the preview `QQuickImageProvider` (decodes the latest JPEG of each job), the 4 ms GUI-thread heartbeat logger (a `Qt::PreciseTimer` at 4 ms; lateness is the measured interval minus 4 ms), the frame-interval logger (`frameSwapped` of each window), the start-up timer (`first_frame` is the first `frameSwapped`, `interactive` the first `QAbstractEventDispatcher::aboutToBlock` after the QML root completes, both in ms since the process creation time from `GetProcessTimes`), the `LayoutSaver` wrapper and the JSON-lines log.
- `rust/` is the crate `spike_shell_bridge` (cxx-qt 0.10): a 10,000-row `QAbstractListModel` that exposes rows 200 at a time through `canFetchMore`/`fetchMore`; a feed of 8 fake jobs at 10 Hz, each tick a 512×512 JPEG frame and a progress value; 6 telemetry series at 1 Hz; and the counter.
- `tests/tst_shell.qml` with `tests/tst_main.cpp` is the S0.9 Qt Quick Test.
- `--no-activate` shows every window, including floating ones, without activating it (the `_q_showWithoutActivating` window property, set on each window's first show), so the scripted runs never take focus from the desktop. The harness also records the foreground window during the runs.

### The harness

`run-spike.ps1` builds from scratch, deploys, runs each check and writes `spikes/app-shell/results/checks.json` (one verdict per check, the raw samples kept in the JSON). Re-run it:

```powershell
pwsh -NoProfile -File spikes/app-shell/run-spike.ps1 -Clean            # every check that needs no owner
pwsh -NoProfile -File spikes/app-shell/run-spike.ps1 -KddwTag v2.4.1   # S0.1's fallback
```

The checks the owner must be present for (S0.5 (b) and (c), S0.6, S0.7, and the scratch CI run of S0.9 and S0.10) have their exact commands in `spikes/app-shell/OWNER-STEPS.md`. `comfy-load.ps1` starts ComfyUI on `127.0.0.1` with the spike's own GPU-saturating test workflow in a loop, samples NVML GPU utilisation, and stops ComfyUI afterwards; only the owner starts it.

Per check:

- **S0.1:** configure and build with `-G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo` from an empty build folder; warning lines are split into our own files, KDDockWidgets' and the toolchain's (by path), and our CMake and Cargo files are grepped for any warning flag. Layout: a save run moves the 3 windows to fixed, non-default geometries, saves the whole layout with `LayoutSaver` and logs a snapshot (each window's geometry; each dock's window, floating state, open state and rectangle in its window); a restore run starts with the default layout, restores the file and logs the same snapshot. Three save-and-restore cycles, each comparing the 3 windows within 2 px and every dock by unique name. The geometries fit a 1920×1080 primary screen at (0, 0), because KDDockWidgets moves a floating window that would restore off-screen back onto a screen, so a geometry on a monitor that is switched off fails the comparison for a reason that has nothing to do with the restore. The per-window check saves `main-b` alone with `addWindowToSave()`, then moves `main-b`, floats its dock and moves `main-a`, restores the file, and checks that `main-b` and its dock are back and that `main-a` and the floating window were left alone.
- **S0.2:** a detached scratch `git worktree` under `%TEMP%` gets a throwaway member (a counter with a QML round trip behind the `qt` feature; `cxx`, `cxx-qt`, `cxx-qt-lib` and `cxx-qt-build` added to `[workspace.dependencies]` and taken from there as optional), its entries in the workspace `Cargo.lock` (`cargo metadata`, nothing else updated), and a small CMake project that imports it with the exact `cxx_qt_import_crate` call of §4 and runs the counter under `QT_QPA_PLATFORM=offscreen` (no window). Then `cargo build --workspace --locked` and `cargo clippy -p <member> --all-targets --locked -- -D warnings` run with every Qt folder removed from PATH and `QMAKE`, `QTDIR` and `CMAKE_PREFIX_PATH` unset; the script also checks the CMake build left `Cargo.lock` unchanged. It runs twice, with the member named `lil-qt-probe` (the workspace's dashed convention) and `lil_qt_probe`, because cxx-qt-build names its exported files after `CARGO_PKG_NAME` while cxx-qt-cmake 0.10.0 uses Corrosion's target name, which has underscores. The worktree is removed afterwards. Re-run: `pwsh -NoProfile -File spikes/app-shell/s02-lil-qt-route.ps1 -Pkg <name> -Out <json>`.
- **S0.3:** `dumpbin /dependents` of the exe and the shim DLL; the bridge is a static library linked into the exe, so its linker directives and symbols are searched for WebEngine too. A run shows the Web dock at 3 s: a 100 ms probe of `GetModuleHandleW(L"Qt6WebEngineCore.dll")` runs from start until the dock is shown, then once after the `Loader` completes; the profile's storage and cache paths are logged and checked on disk; every Qt message is searched for "deprecat"; the `about:blank` load result is logged.
- **S0.4:** exe and our DLLs copied first, then `windeployqt` with the flags of §4 and `--qmldir` for every QML folder (`qml/`, `web/`, `shim/` and KDDockWidgets' `src/qtquick/views/qml`); the VC++ runtime DLLs copied from `%VCToolsRedistDir%x64\Microsoft.VC143.CRT`. Sizes per configuration (untrimmed: `--release --qmldir` only; trimmed: every flag; trimmed without WebEngine: every flag and no `web/` QML folder): `--dry-run --list source` is run and its exit and list recorded, and because it exits 1 whenever WebEngine is involved (windeployqt then reads `QtWebEngineProcess.exe` from the target folder, which a dry run never fills), each configuration is also deployed for real into a scratch folder, with `--list source`, and that folder is measured with our files and the VC++ DLLs. The deployed folder then runs S0.1's restore cycle and S0.3's lazy-load run with spike 02's stripped environment.
- **S0.5 (a):** one untimed start, then 10 starts of the deployed app with `--no-activate --quit-after 2000` and the stripped environment; `first_frame` and `interactive` from each log. Before each start the harness waits (up to 3 minutes) until the machine's CPU load is under 35 % and records the load; a result over 2,000 ms on a busy machine is INCONCLUSIVE, not FAIL.
- **Diagnostics:** if the GUI thread doesn't run its event loop for 5 s, or the process is still alive 8 s after quitting, the app writes every thread's stack beside its log (`<log>.stall-stacks.txt`, `<log>.stacks.txt`): first the instruction pointer and a stack scan for each thread, GUI thread first, then a symbolised walk with dbghelp.
- **S0.8:** `%LOCALAPPDATA%\LILITH-spike` is removed before the first deployed run, and `%LOCALAPPDATA%`'s top-level folders are listed before and after; afterwards the `qmlcache` and `qtpipelinecache-*` folders are looked for under `%LOCALAPPDATA%\LILITH-spike\image\`, and a folder named after the exe is looked for in `%LOCALAPPDATA%`.
- **S0.9 (local):** `tst_app_shell` (Qt Quick Test, `tests/tst_shell.qml`) under `QT_QPA_PLATFORM=offscreen`, first with no other variable, then with `QT_QUICK_BACKEND=software`; each test function runs on its own and its result is recorded per environment, and the grab test logs the scene graph's `GraphicsInfo.api`. The scratch CI run uses the same commands (`spikes/app-shell/ci/spike10-qt.yml`).

## 4. Pass criteria

Copied verbatim from the plan (Task 0) before anything was run:

- **S0.1 KDDockWidgets 2.5.0** (time box: half a day): `FetchContent` at tag `v2.5.0`; the shim's dock factory themed through ViewFactory QML; 3 docks in 2 main windows plus 1 floating; `LayoutSaver` save and restore; `LayoutSaver::addWindowToSave()` for one window. PASS = configure and build exit 0 with 0 warning lines from our own files at default settings (no warning flag added or removed in them; warnings from KDDockWidgets' own headers are recorded separately and don't fail the check); restore puts each of the 3 windows within 2 px of its saved geometry with the same docks in the same places (by unique name), 3 runs of 3; the per-window save round-trips one window alone; exit code 0. Any FAIL, or the time box running out → the same checks on `v2.4.1`, which then stays (APP-005).
- **S0.2 The `lil-qt` route** (P4a-R1): in a scratch worktree only, a throwaway workspace member with `cxx`, `cxx-qt` and `cxx-qt-lib` optional under a `qt` feature and `cxx-qt-build` an optional build-dependency. PASS = it builds through `cxx_qt_import_crate(MANIFEST_PATH <member>/Cargo.toml CRATES <member> LOCKED FEATURES qt QT_MODULES Qt6::Core Qt6::Gui Qt6::Qml)` against the workspace `Cargo.lock`, and its QML counter round-trips; and `cargo build --workspace --locked` and `cargo clippy -p <member> --all-targets -- -D warnings` pass with no Qt folder on PATH and `QMAKE` unset. FAIL → P4a-R1's fallback. The scratch worktree is deleted; only the result is recorded.
- **S0.3 Lazy WebEngine library** (APP-137, IMG-126): `SpikeWeb` is a separate QML plugin DLL imported only by `WebPanel.qml`, which a `Loader` loads when its dock is first shown; the named persistent WebEngine profile comes from `WebEngineProfilePrototype`. PASS = `dumpbin /dependents` of the exe, the shim DLL and the bridge library list no `Qt6WebEngine*`; `GetModuleHandleW(L"Qt6WebEngineCore.dll")` is null from start until the dock is shown and non-null after; the WebEngine profile's storage is under `%LOCALAPPDATA%\LILITH-spike\image\`; Qt logs no deprecation warning; `about:blank` loads. FAIL → stop; the result goes to the owner before Task 2, because APP-137 (an invariant) can't be met as specified.
- **S0.4 Deploy size** (ARCH-114, OPS-094): the exe and our DLLs (shim, KDDockWidgets, `SpikeWeb`) are copied first, then `windeployqt --release --qmldir <every QML folder> --no-translations --no-opengl-sw --no-system-dxc-compiler --no-ffmpeg --skip-plugin-types <list>`, and the VC++ runtime DLLs are copied app-local from the MSVC redist folder (no `vc_redist` is run). Record the `--dry-run --list` size and file count untrimmed, trimmed, and trimmed without the WebEngine module. PASS (hard) = the trimmed folder runs S0.1's restore and S0.3's lazy load with every Qt, Visual Studio, MSVC and Windows Kits folder removed from PATH and `QTDIR`, `QT_PLUGIN_PATH` and `QML_IMPORT_PATH` unset (spike 02's harness). Target (soft) = trimmed ≤ 300 MiB; above that is reported, not a failure.
- **S0.5 Cold start** (APP-136): the deployed app logs `first_frame` (the first `frameSwapped`) and `interactive` (the first idle of the event loop after the QML root completes), in ms from process start. PASS = `interactive` < 2,000 ms in each of: (a) 10 warm starts (p50 and max recorded); (b) the first start after a reboot (owner present); (c) the first start of a fresh copy of the folder after a Defender scan of it (owner present: `"%ProgramFiles%\Windows Defender\MpCmdRun.exe" -Scan -ScanType 3 -File <folder>`). A repeat of spike 02's 145 s stall goes to the owner at once, with its log. Other FAIL → the start is profiled (QML cache through `qmlcachegen`, the pipeline cache, deferred panel loading) and re-measured once; still over 2 s → the result goes to the owner before Task 2.
- **S0.6 GUI stalls under ComfyUI load** (APP-135): at once, 8 fake jobs each pushing a 512×512 JPEG preview and progress at 10 Hz through the image provider; the 10,000-row paged list scrolled 200 rows a second; 6 charts drawn with Qt Quick Shapes `PathPolyline`, 900 points each, a point a second (P4a-R17); while ComfyUI runs the test loop with NVML GPU utilisation ≥ 95 % throughout. Over 60 s, measure the 4 ms heartbeat's lateness and the frame interval. PASS = p99 < 16 ms for both (max recorded). Recorded, not a bar: the spike's dedicated VRAM (PDH `Local Usage`) with the web dock shown, for P4a-R15. FAIL → previews drop to 5 Hz and charts to one redraw a second, and the check runs again; still failing → the result goes to the owner before Task 2.
- **S0.7 Narrator smoke check** (APP-156; owner present): with Narrator on (Ctrl+Win+Enter), the owner tabs through the spike window and records yes or no for: a KDDockWidgets title bar, a dock tab, the counter button by its accessible name, the chart tile (a custom delegate with `Accessible.role` and `Accessible.name`), and an `Accessible.announce()` message. PASS = the spike's own controls are read (else the pattern is fixed before Task 18). The KDDockWidgets answers are recorded, and any "no" goes to the owner (APP-156).
- **S0.9 Offscreen GUI tests** (QA-068): under `QT_QPA_PLATFORM=offscreen`, a Qt Quick Test with a KDDockWidgets main window and one floating window, a `PathPolyline` Shape, a `Loader` of the `SpikeWeb` plugin, and a grab that isn't one flat colour; the backend environment it needs (for example `QT_QUICK_BACKEND=software`) is recorded. PASS = all of it works locally and in one scratch CI run on `windows-2025` (a workflow commit the owner pushes, P4a-R32). FAIL → P4a-R35 for whatever failed, recorded per item: those tests run in the Sandbox runner's `--suite qml` or `--suite grabs` mode (Task 30), on the Sandbox's own screen.
- **S0.10 CI time with Qt** (P4a-R32): the scratch CI run of S0.9 also builds the spike with Qt installed by `naqt` and times the whole `pr-ready` job's steps. PASS = the job, with Qt added, stays under 50 minutes. FAIL → Task 15 splits Qt into its own job with a Qt cache, as P4a-R32 says.
- **S0.8 Caches:** with `organizationName` `LILITH-spike` and `applicationName` `image`, PASS = Qt's QML and pipeline caches are under `%LOCALAPPDATA%\LILITH-spike\image\`, and no new folder named after the executable appears in `%LOCALAPPDATA%`. FAIL → the app sets `QML_DISK_CACHE_PATH` and the pipeline-cache path itself from `Env` (ARCH-095), and the check runs again.

## 5. Measurements

To be filled in after the run (Step 4).

## 6. Verdict

To be filled in after the run (Step 4).

## 7. Decision

To be filled in after the run (Step 4).

## 8. Follow-ups

To be filled in after the run (Step 4).
