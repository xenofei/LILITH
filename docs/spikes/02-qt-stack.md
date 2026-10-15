# 02 Qt stack: cxx-qt + KDDockWidgets + QQuickRhiItem + WebEngine

Date: 2026-10-12

GPU: NVIDIA GeForce RTX 5080 16 GB, driver 610.88 (Windows driver 32.0.16.1088). Qt 6.12.0 + Qt WebEngine 6.140.0. ComfyUI 0.39.2 (frontend 1.53.10, per the plan; the run loaded `*Unsaved Workflow - ComfyUI`).

## 1. Question

- **ARCH-115:** do cxx-qt 0.10 + KDDockWidgets 2.4.1 (Qt Quick) + a `QQuickRhiItem` shim + Qt WebEngine + `windeployqt` work together on Qt 6.12 LTS with MSVC on Windows 11? The version that passes is the M1 Qt pin (fall back to 6.11 if 6.12 fails).
- **ARCH-116:** does a GPU canvas in a dock survive its renderer being destroyed and recreated when the dock floats into its own top-level window and comes back?
- **APP-137:** can WebEngine stay off the startup path (`Qt::AA_ShareOpenGLContexts` before the app object, the `QtWebEngine` import only in a `Loader`-created component, no WebEngine DLL linked by the exe or the shim)?

## 2. Setup

| Item | Version |
|---|---|
| OS | Windows 11 Pro 10.0.26200 |
| GPU / driver | NVIDIA GeForce RTX 5080 16 GB, 610.88 (also an AMD Radeon iGPU; Qt picked the RTX 5080) |
| Rust | rustc / cargo 1.99.0, `stable-x86_64-pc-windows-msvc` |
| CMake / Ninja | 4.4.4 / 1.13.2 |
| MSVC | cl 19.44.35227 (VS 2022 Build Tools, toolset 14.44.35207), Windows SDK 10.0.26100 |
| Qt | 6.12.0 `msvc2022_64` at `C:\Qt\6.12.0\msvc2022_64` |
| Qt WebEngine | 6.140.0 (Chromium 140 base) |
| cxx-qt | `cxx-qt`, `cxx-qt-lib`, `cxx-qt-build` = 0.10.0 (pinned `=0.10.0`), `cxx` 1.0.205; `Cargo.lock` committed |
| cxx-qt CMake | `KDAB/cxx-qt-cmake` tag `0.10.0` (FetchContent), which fetches Corrosion v0.5.2 |
| KDDockWidgets | 2.4.1 (FetchContent tag `v2.4.1`, commit c1d28d25), `KDDockWidgets_FRONTENDS=qtquick`, shared library `kddockwidgets-qt62.dll` |
| ComfyUI | 0.39.2 at `%USERPROFILE%\ComfyUI\ComfyUI`, started by the check script on `127.0.0.1:8188` only |
| Spike code | `spikes/qt-stack/` in the commit that adds this document |

## 3. Method

The app (`spikes/qt-stack/`):

- `main.cpp` sets `Qt::AA_ShareOpenGLContexts`, then creates `QGuiApplication`, calls `KDDockWidgets::initFrontend(QtQuick)`, registers the shim types and loads `qrc:/qml/Main.qml`.
- `qml/Main.qml`: an `ApplicationWindow` with a KDDockWidgets `DockingArea` and three docks:
  - **Canvas** holds a `SpikeCanvas`;
  - **Counter** holds a label bound to `SpikeCounter.count` and a button calling `increment()`;
  - **Web** is tabbed behind Counter. Its `Loader` sets `source: "WebDock.qml"` only once the dock's content has really been shown, and `qml/WebDock.qml` is the only file importing `QtWebEngine`.
- `shim/` is a DLL (`spike_shim.dll`):
  - `SpikeCanvas` is a `QQuickRhiItem`. Its renderer clears to a colour, dispatches a compute shader (16×16 groups) that writes a 256×256 `RGBA16F` storage texture, and draws a triangle that samples that texture. It logs `renderer_created`, `backend` (from `QRhi::backendName()`), `initialize` (process-wide count), `compute_dispatch` and `renderer_destroyed`.
  - `SpikeLog` writes the JSON-lines log.
- Shaders (`shaders/fill.comp`, `blit.vert`, `blit.frag`) are built with `qt_add_shaders(... BATCHABLE PRECOMPILE GLSL "430" HLSL 50 MSL 12 ...)`, so HLSL is precompiled to DXBC by `fxc`.
- `rust/` is the crate `spike_bridge`: QML module `SpikeBridge`, element `SpikeCounter` with `count: i32`, `increment()` and the signal `rendererRecreated()`. QML emits the signal whenever the canvas reports a renderer `initialize()`, and a `Connections` handler logs it as `renderer_recreated`.
- QML is compiled in as plain resources (`qt_add_resources`, no qmlcachegen), so the `QtWebEngine` import is resolved only at run time.

App switches:

| Switch | Effect |
|---|---|
| `--log <file>` | one JSON line per event: `{"t_ms":…, "event":…, …}`. Events: `start`, `qml_loaded`, `renderer_created`, `backend`, `initialize`, `compute_dispatch`, `renderer_recreated`, `window_shown`, `docks`, `float`, `web_dock_shown`, `webengine_loaded`, `web_load`, `web_title`, `web_probe`, `counter`, `heartbeat`, `screenshot`, `qt_msg`, `quit_requested`, `quit`, `exit`, `renderer_destroyed` |
| `--auto-float-canvas` | floats the Canvas dock at 2 s, re-docks it at 4 s, quits at 6 s |
| `--show-web <ms>` | makes the Web tab current after `<ms>` |
| `--auto-counter <ms>` | clicks the Counter button 3 times, 500 ms apart, logging the bound label after each click |
| `--quit-after <ms>` | quits after `<ms>` |
| `--screenshot <png>` | saves `QQuickWindow::grabWindow()` of the main window just before an automatic quit |
| `--heartbeat <ms>` | GUI-thread timer; gaps show event-loop stalls |

Re-run everything (it builds from scratch, deploys, starts and stops ComfyUI, and writes `spikes/qt-stack/results/checks.json`):

```powershell
pwsh -File spikes/qt-stack/run-checks.ps1 -Clean
```

What `run-checks.ps1` does, per criterion:

1. Configures and builds with `cmake -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo`, counts `warning` lines in the build log, and greps our CMake and Cargo files for any warning suppression.
2. Runs the app with `--auto-counter 1500 --quit-after 6000`. Pass: the `docks` event shows all three docks open, a `backend` event says `D3D11`, `quit` is logged and the exit code is 0.
3. Runs `--auto-float-canvas`. Pass: `initialize` ≥ 2, exit code 0 and `quit` logged. The script also requires ≥ 3 `renderer_created` events (start, float, re-dock).
4. `dumpbin /dependents` on the exe and `spike_shim.dll`. In the start run: no `webengine_loaded` event and `Qt6WebEngineCore.dll` not loaded in the process (checked with `GetModuleHandle`). In every Web run: `webengine_loaded` comes after `web_dock_shown`, and the DLL is absent when the dock is shown.
5. **ComfyUI down:** shows the Web tab at 2 s and clicks the counter at 8 s. Pass: a `web_load` failure is logged; the load reports `isErrorPage` and the page is `chrome-error://`; the counter label still updates after the failure; GUI heartbeats every 250 ms show no gap ≥ 1 s; `Process.Responding` is never false; exit code is 0.
   **ComfyUI up:** the script starts ComfyUI with `--listen 127.0.0.1 --port 8188`, waits for `/system_stats`, runs the app, then stops the process it started. Pass: the load succeeds, the page title contains `ComfyUI`, the page has its graph `<canvas>`, and the exit code is 0.
6. Runs `windeployqt --release --qmldir qml` into `build/deploy` (after copying the exe and our DLLs), then runs the checks for criteria 2 and 3 from there. Those runs use a PATH with every Qt, Visual Studio, MSVC and Windows Kits directory removed and `QTDIR`, `QT_PLUGIN_PATH`, `QML_IMPORT_PATH` and the VC variables unset. The script asserts that no remaining PATH directory contains `Qt6Core.dll` or `cl.exe`. It also does an informational WebEngine run from the deploy folder.
7. Uses the criterion 2 run: three `counter` events, ending at `count 3` and `"Count: 3"`, and every label text equals `"Count: " + count`.

## 4. Pass criteria

Copied verbatim from the plan (Task 2, step 1) before the spike was run:

1. Configure + build with Ninja and MSVC succeeds with no warnings treated as errors disabled.
2. The app starts and shows the three docks. The log contains `backend: D3D11`.
3. `--auto-float-canvas`: the log shows `initialize` at least twice (the renderer is recreated on float and on re-dock), no crash, and the exit code is 0.
4. `webengine_loaded` doesn't appear before the Web dock is shown. `dumpbin /dependents` on the app exe and the shim shows no `Qt6WebEngine*` DLL (APP-137).
5. With ComfyUI running, the Web dock shows ComfyUI's UI. With ComfyUI stopped, the Web dock shows the browser's error page, the app stays responsive, and the log records the load failure.
6. `windeployqt --qmldir qml` output starts and passes criteria 2–3 with a PATH stripped of every Qt and MSVC directory.
7. `SpikeCounter.increment()` from QML updates the bound text (the cxx-qt bridge works both ways).

Interpretation of criterion 1 (decided during M0 review, 2026-10-12): the wording is garbled; it means the build must succeed with the toolchain's default warning settings, and nobody may disable warnings or warnings-as-errors to get it to pass.

## 5. Measurements

Raw data: (raw output kept privately) (final run 2026-10-12 14:29). The window grabs `spikes/qt-stack/results/c2-start.png`, `c5-down.png` and `c5-up.png` are committed. Per-run JSON-lines logs are in `spikes/qt-stack/build/runs/` (git-ignored). The RED run before the project existed is also raw output kept privately.

| Criterion | Result | Evidence (final run) |
|---|---|---|
| 1 build | PASS | configure exit 0, build exit 0 (111 Ninja steps, from scratch), **0 warning lines**; no warning flags set in our files |
| 2 start | PASS | docks open: `canvas`, `counter`, `web`; `{"event":"backend","backend":"D3D11"}`, device "NVIDIA GeForce RTX 5080"; exit 0 |
| 3 float | PASS | `initialize` ×3 (893×768 → 885×760 floating → 893×768), renderers 1→2→3, each with a new QRhi (`rhi_changed: true`); `float` events show the canvas leave and return to the main window; exit 0, wall 6.4 s |
| 4 lazy WebEngine | PASS | exe deps: kddockwidgets-qt62, spike_shim, Qt6Quick/Gui/Qml/Core + CRT/system; shim deps: Qt6Quick/Gui/Core + CRT; **no Qt6WebEngine***. Start run: no `webengine_loaded`, `Qt6WebEngineCore.dll` not in process. Web runs: `web_dock_shown` 2294 ms → `webengine_loaded` 2511 ms (down), 2375 → 2597 ms (up); DLL absent at show time in both |
| 5 ComfyUI down | PASS | `web_load failed net::ERR_CONNECTION_REFUSED` (code -102), `isErrorPage: true`, page `chrome-error://chromewebdata/` "This site can't be reached"; counter 1→2→3 after the failure; 37 heartbeats after it, max gap 281 ms; 0 not-responding polls; exit 0 |
| 5 ComfyUI up | PASS | `web_load succeeded` for `http://127.0.0.1:8188/`, title `*Unsaved Workflow - ComfyUI`, graph canvas present (`results/c5-up.png` shows the node graph); exit 0; ComfyUI stopped afterwards |
| 6 deploy | PASS | `windeployqt` exit 0; 16 Qt/VS/MSVC/Kits PATH entries removed, no `Qt6Core.dll`/`cl.exe` left on PATH; deployed start: 3 docks, D3D11, exit 0; deployed float: initialize ×3, renderers ×3, exit 0. Informational deployed Web run: WebEngine loaded, `QtWebEngineProcess` ran, error page shown, no Qt warnings, exit 0 |
| 7 bridge | PASS | `counter` events: `Count: 1` @1884 ms, `Count: 2` @2384, `Count: 3` @2889 |

Timings (single runs, so n per series is small; all in ms from process start unless stated):

| Metric | n | p50 | p95 | p99 | max | Unit |
|---|---|---|---|---|---|---|
| Start → QML loaded (`qml_loaded`) | 7 | 355 | 415 | 415 | 415 | ms |
| QML loaded → first canvas `initialize` | 7 | 3 | 4 | 4 | 4 | ms |
| Float request (≈ t 2000 + QML load) → new renderer `initialize` | 2 | 77 | 78 | 78 | 78 | ms |
| Web dock shown → `webengine_loaded` (build tree) | 2 | 217 | 222 | 222 | 222 | ms |
| Web dock shown → `webengine_loaded` (deploy folder, stripped PATH) | 1 | 427 | 427 | 427 | 427 | ms |
| WebEngine load start → ComfyUI page `succeeded` | 1 | 155 | 155 | 155 | 155 | ms |
| WebEngine load start → `ERR_CONNECTION_REFUSED` | 2 | 2111 | 2118 | 2118 | 2118 | ms |
| GUI heartbeat gap after load failure (250 ms timer) | 36 | — | — | — | 281 | ms |

Float latencies are approximate: the float timer starts at component completion (≈ QML loaded), and the new renderer's `initialize` was logged 77 ms and 78 ms after that point plus 2000 ms. Sizes: `qt_stack_spike.exe` 3.6 MiB, `spike_shim.dll` 0.15 MiB, `kddockwidgets-qt62.dll` 3.3 MiB. The deploy folder is 414 MiB in 1,580 files, mostly WebEngine resources plus every Qt Quick Controls style.

## 6. Verdict

**PASS** on Qt 6.12.0 + Qt WebEngine 6.140.0. All seven criteria pass in one scripted from-scratch run, and the down and up halves of criterion 5 were both run in that same pass. cxx-qt 0.10 (`cxx_qt_import_crate` + `cxx_qt_import_qml_module`) and KDDockWidgets 2.4.1's Qt Quick frontend (FetchContent) build together with Ninja and MSVC, with zero warnings at default settings.

The `QQuickRhiItem` compute + sample shim runs on D3D11. Floating and re-docking the canvas destroys its renderer and builds a new one against the floating window's own QRhi, then against the main window's again. There is no crash, and the content is rebuilt each time (ARCH-116 holds for a shim that recreates its resources in `initialize()`).

WebEngine stays out of the process until the Web dock is first shown (APP-137). The exe and the shim do not link it. Both the ComfyUI UI and Chromium's error page render inside the dock, and the GUI thread stays live after a refused connection. The `windeployqt` output runs criteria 2–3 with Qt and MSVC off PATH. The fallback to Qt 6.11 (step 5) was not needed.

## 7. Decision

- **ARCH-115:**
  - Qt **6.12.0** (LTS), `msvc2022_64`;
  - Qt WebEngine **6.140.0**;
  - cxx-qt **0.10.0** with cxx-qt-cmake `0.10.0` (Corrosion v0.5.2);
  - KDDockWidgets **2.4.1** (Qt Quick frontend) confirmed.
- **Working CMake pattern (plan 4):**
  - `CMAKE_MSVC_RUNTIME_LIBRARY=MultiThreadedDLL`;
  - `find_package(Qt6 … Quick QuickControls2 ShaderTools WebEngineQuick)` plus a separate `find_package(Qt6 COMPONENTS GuiPrivate)` for `<rhi/qrhi.h>`;
  - `FetchContent` KDDockWidgets `v2.4.1` with `KDDockWidgets_QT6=ON` and `KDDockWidgets_FRONTENDS=qtquick`;
  - `FetchContent` cxx-qt-cmake `0.10.0`;
  - `cxx_qt_import_crate(MANIFEST_PATH rust/Cargo.toml CRATES spike_bridge LOCKED QT_MODULES Qt6::Core Qt6::Gui Qt6::Qml)` plus `cxx_qt_import_qml_module(spike_bridge_qml URI "SpikeBridge" SOURCE_CRATE spike_bridge)`;
  - the shim as a `SHARED` `qt_add_library` linking `Qt6::GuiPrivate` privately, with `qt_add_shaders(... BATCHABLE PRECOMPILE ...)`;
  - the exe linking `KDAB::kddockwidgets`, the shim and `spike_bridge_qml`, but never `Qt6::WebEngineQuick`;
  - QML through `qt_add_resources`;
  - `CMAKE_RUNTIME_OUTPUT_DIRECTORY` set so the exe and every DLL land in one `bin/`.
- **APP-137:** the pattern holds as specified (attribute before the app object, `Loader` + a separate `WebDock.qml`, no link).

## 8. Follow-ups

1. **`OPTIMIZED` in `qt_add_shaders` needs `spirv-opt`** (Vulkan SDK), which is not installed. The flag was dropped; shaders are unoptimised SPIR-V and precompiled DXBC. Either add the Vulkan SDK to the toolchain (QA-077 list) or note that shaders are not SPIR-V optimised.
2. **cxx-qt link trap:** when a bridge uses no `cxx-qt-lib` type, the crate needs `extern crate cxx_qt_lib;`. Without it the link fails with an unresolved `cxx_qt_init_crate_cxx_qt_lib`, because cxx-qt-build's generated initializer still calls it. Plan 4 should keep this line or use a `cxx-qt-lib` type.
3. **`<rhi/qrhi.h>` needs `Qt6::GuiPrivate`** (found explicitly) on Qt 6.12. Plan 4's shim must link it privately.
4. **KDDockWidgets builds as `kddockwidgets-qt62.dll`** (Qt 6.2+ ABI naming), which `windeployqt` does not copy. The deploy step must copy our own DLLs (shim, KDDW) next to the exe first, as `run-checks.ps1` does.
5. **The deploy folder is 414 MiB.** `windeployqt` ships every Qt Quick Controls style and the full WebEngine resources. The installer (plan 4) should use `--qmldir` plus `--skip-plugin-types` or a style filter to trim it, and should note that `vc_redist.x64.exe` is copied but not run.
6. **QQuickRhiItem `initialize()` runs on resizes too.** The renderer is genuinely recreated on float and re-dock (new `renderer_created` and a new QRhi), so canvas code must rebuild GPU state whenever `rhi()` changes, not just on first use. Task 3 builds on this.
7. **Unexplained 2.5-minute startup stall,** seen in early manual runs before the scripted checks. In the first three launches after the first build (14:10–14:14), the window appeared but QML creation and the first frame were delayed about 145 s (`qml_loaded` was not yet logged; the canvas's first `initialize` came at t = 143.7 s). Every later launch (7 manual launches and 22 launches across three full scripted runs) reached `qml_loaded` in 286–452 ms. It looks like a first-launch effect (Defender scanning the new binaries, or the first QML or pipeline cache build), but it was not proven. Plan 4's startup telemetry should log time-to-first-frame so this shows up if it recurs.
8. **Qt's caches** go to `%LOCALAPPDATA%\qt_stack_spike\cache` (QML and pipeline cache), keyed by the application name. The suite must set its application and organization names deliberately, so caches land under the suite's own folder. The default WebEngine profile in QML was off-the-record: no WebEngine data was written.
9. **CMake configure warning:** `Qt6QmlAssetDownloaderPrivate` needs `Qt6TaskTree`, which is not installed (also seen in Task 1). It is harmless here; add `qttasktree` to the Qt module list to silence it.
