# 04 Frame times and GUI stalls under ComfyUI load; the D3D12 decision

Date: 2026-10-12

## 1. Question

- **IMG-153:** does the canvas (Task 3's compositor, 20 layers, 8192×8192) stay smooth while ComfyUI saturates the GPU?
- **APP-135:** are GUI-thread stalls under 16 ms at the 99th percentile while ComfyUI saturates the GPU?
- **IMG-040:** does the canvas trim its GPU tile cache to the visible pages before each ComfyUI prompt, so that the canvas process stays under 1 GB of GPU memory?
- **IMG-033 / IMG-152:** is the Direct3D 12 path worth building in M1b?

## 2. Setup

| Item | Version |
|---|---|
| OS | Windows 11 Pro 10.0.26200, display at 240 Hz (vsync 4.17 ms) |
| GPU / driver | 16 GB consumer GPU |
| Toolchain | Qt 6.12.0 `msvc2022_64` (threaded render loop, confirmed with `QSG_INFO=1`), Rust 1.99.0, CMake 4.4.4, Ninja 1.13.2, MSVC 19.44.35227, Windows SDK 10.0.26100 |
| Shader compilers | fxc (SDK 10.0.26100) for DXBC SM 5.0; dxc 1.8.2502.11 with dxil.dll 1.8 (same SDK) for DXIL SM 6.6. Both were already installed; nothing new was installed. |
| ComfyUI | 0.39.2, frontend 1.53.10, workflow templates 0.11.78, PyTorch 2.11.0+cu128 (venv at `%USERPROFILE%\ComfyUI\.venv`). Started for this spike on `--listen 127.0.0.1 --port 8188` and stopped afterwards. |
| Model | Z-Image Turbo bf16 (`z_image_turbo_bf16.safetensors`), `qwen_3_4b` text encoder, `ae` VAE |
| Spike code | `spikes/canvas/` in the commit that adds this document (Task 3's 747207c plus the Task 4 changes) |

## 3. Method

### What was built

- **`spikes/comfy-measure/workflows/zimage_t2i.json`** was produced as follows:
  1. Fetch the built-in template `image_z_image_turbo.json` from ComfyUI's `/templates/` route (package `comfyui_workflow_templates` 0.11.78).
  2. Load it into the running ComfyUI frontend (1.53.10) with `app.loadGraphData()`.
  3. Convert it with the frontend's own `app.graphToPrompt()`, the function behind Workflow → Export (API). This flattens the template's subgraph into nodes `57:*`.
  4. Store the `output` as is: 1024×1024, 8 steps, `res_multistep`/`simple`, cfg 1, seed 0.

  It validated:
  - `POST /prompt` returned empty `node_errors`;
  - the run completed in 12.9 s, cold, with a 1024×1024 PNG;
  - three further runs as `comfy_load.py` submits them took 6.6–6.9 s each.

  Raw data: (raw output kept privately).
- **`spikes/canvas/load/comfy_load.py`** uses only the Python standard library and ComfyUI's HTTP API.
  - It checks `/system_stats` with a 2 s timeout. If ComfyUI does not answer, it prints `ComfyUI unreachable at 127.0.0.1:8188` and exits 2.
  - `--unload-first` posts `/free` first.
  - Each prompt then runs in turn. The script touches `--trim-flag`, sets a fresh random `seed`, submits with `POST /prompt` and polls `/history/<id>` every 100 ms until the prompt finishes (one pending prompt, IMG-091). It repeats until `--seconds` have passed and then waits for the last prompt.
  - It writes a JSON summary with times, durations and trim-touch times.
  - By default it switches `SaveImage` to `PreviewImage` before submitting. The GPU work is the same, but the images go to ComfyUI's temp folder instead of filling `output/`. `--save-outputs` keeps `SaveImage`.
  - It never cancels or touches other prompts. The run script refuses to start a load run if ComfyUI's queue is not empty.
- **`spikes/canvas/load/sample-gpu.ps1`** runs once a second for the life of the canvas process. Each sample records:
  - PDH `\GPU Process Memory(pid_<pid>_*)\{Dedicated Usage, Shared Usage, Total Committed}`, summed over the process's adapter instances;
  - `nvidia-smi` GPU utilization and memory used;
  - `GetLastInputInfo` idle time;
  - whether the canvas window is minimized or in the foreground.
- **`spikes/canvas/run-frametimes.ps1 -Backend d3d11|d3d12`** builds the app, then runs the idle run and the load run. It writes `results/frametimes-<backend>-<idle|load>.json`, evaluates criteria 1–3 and merges everything, including the PDH samples, into `results/frametimes-summary.json`. Other modes:
  - `-NegativeCheck` (criterion 4) writes `results/negative-check.json`;
  - `-GuiPan` runs the GUI-driven pan diagnostic and writes to `results/diag/`;
  - `-Diagnostics` runs the controls (`--no-canvas`, and the window not on top).
- **`spikes/canvas/run-variant-bench.ps1`** covers D3D12 rule condition 3 and writes `results/composite-variants.json`. Section 5 has the details.
- **`canvas.exe`, new switches:**
  - `--frametimes`, extended:
    - all times share one clock;
    - `--measure-after <ms>` splits off the warm-up;
    - each stall is stored with its time;
    - **sync-wait instrumentation** records when the GUI thread is blocked in the threaded render loop's sync (`afterAnimating` on the GUI thread → `afterSynchronizing` on the render thread), and every stall is attributed to it by overlap;
    - the trims are listed with wall-clock time and cache size before and after.
  - `--composite-variant twopass[-sm66] | single[-sm66] | inplace | inplace-sm50`.
  - `--no-canvas` (control run).
  - `--on-top`.
  - `--gui-pan` (diagnostic).
- **Shaders:** `composite.comp` gained a `COMPOSITE_FLAT` path in which the whole tree, with the group as `GROUP_BEGIN`…`GROUP_END` nodes, is one program, plus `COMPOSITE_INPLACE`. `CMakeLists.txt` builds six more `.qsb` packs from it:
  - `composite_single`, `composite_single_sm66`;
  - `composite_inplace` (DXIL SM 6.6), `composite_inplace_sm50` (DXBC);
  - `composite_main_sm66`, `composite_group_sm66`.

  The SM 6.6 packs contain DXIL only. A pipeline created from them on D3D12 therefore runs dxc output; there is no fallback.

### How to re-run

```powershell
# ComfyUI on loopback, models installed; machine otherwise idle
pwsh -File spikes/canvas/run-frametimes.ps1 -Backend d3d11            # criteria 1-3 (builds first)
pwsh -File spikes/canvas/run-frametimes.ps1 -Backend d3d12 -NoBuild
pwsh -File spikes/canvas/run-frametimes.ps1 -Backend d3d11 -NoBuild -GuiPan -Attempts 1   # diagnostic
pwsh -File spikes/canvas/run-frametimes.ps1 -Backend d3d12 -NoBuild -GuiPan -Attempts 1
pwsh -File spikes/canvas/run-frametimes.ps1 -Backend d3d11 -NoBuild -Diagnostics -Seconds 60
pwsh -File spikes/canvas/run-variant-bench.ps1 -Rounds 3              # D3D12 rule, condition 3
# ComfyUI stopped:
pwsh -File spikes/canvas/run-frametimes.ps1 -NegativeCheck            # criterion 4
```

## 4. Pass criteria

Copied from the plan before running. Do not edit after the run.

1. **Idle, D3D11:** a 20-layer 8192×8192 document, panning continuously (`--auto-pan`), frame-time p99 ≤ 16.7 ms.
2. **Under load, D3D11:** the same scene while `comfy_load.py` runs for 120 s. The GUI-thread stall is late heartbeat time, which counts as a stall when it is over 4 ms late. Stall p99 must be under 16 ms (APP-135). Frame-time p99 is recorded; the target is ≤ 33 ms, and missing it is reported, not a hard fail.
3. **The tile cache trim rule (IMG-040)** is respected. `comfy_load.py --trim-flag %TEMP%\ww-spike-trim` and `canvas.exe --trim-flag %TEMP%\ww-spike-trim` share the flag file, so the canvas trims its GPU cache to the visible pages before each load prompt. Check this with the per-process "GPU Process Memory" PDH counter: after the trim, the canvas process uses less than 1 GB.
4. **Negative check:** with ComfyUI stopped, `comfy_load.py` prints `ComfyUI unreachable at 127.0.0.1:8188` and exits 2 within 5 s.

**D3D12 decision rule:** build the D3D12 path in M1b (IMG-033) only if:
- D3D12 passes Task 3's criteria 1–4;
- its load-run stall p99 and frame-time p99 are no worse than D3D11's (within 10 %);
- **and** removing the isolated-group intermediate pass on D3D12 (an in-place read-write composite) cuts the 20-layer composite time by at least 25 %.

Otherwise D3D11 stays the only path.

### How the criteria are measured (fixed before the run)

These are the definitions the scripts use. They were written down before any measurement was taken.

- **Scene:** `canvas.exe --layers 20 --doc 8192x8192 --auto-pan` at zoom 1 in the 1280×800 window. The pan speed is 1000 px/s, bouncing at the edges. There is no debug layer and no GPU timestamp queries, so neither adds work to the frames being measured.
- **Window:** every run lasts 125 s. The first 5 s are start-up (QML, pipelines and the first view's tile uploads); they are recorded separately as `warmup` and are not part of the criteria. The measured series covers the following 120 s.
- **Frame time:** the interval between consecutive `QQuickWindow::frameSwapped` signals.
- **Stall:** a 4 ms `Qt::PreciseTimer` heartbeat on the GUI thread. Each beat's lateness is (interval − 4 ms). A beat that is more than 4 ms late is a stall. "Stall p99" is the 99th percentile (nearest rank) of the stall series, the same definition Task 3 implemented. The lateness of all beats is reported too.
- **Load:** `comfy_load.py --seconds 120 --trim-flag %TEMP%\ww-spike-trim --unload-first` starts 3 s after the canvas.
  - `--unload-first` calls ComfyUI's `/free` (unload models, free memory) before the first prompt. Every load run therefore includes a cold model load, and D3D11 and D3D12 see the same load.
  - Prompts are submitted one at a time. Each waits for the previous one to finish (IMG-091) and gets a fresh random seed.
  - The workflow is `spikes/comfy-measure/workflows/zimage_t2i.json` (Z-Image Turbo, 1024², 8 steps).
- **Criterion 3:** a separate process samples `\GPU Process Memory(pid_<canvas pid>_*)\Dedicated Usage` once a second during the load run, summed over the process's adapter instances. `Shared Usage` and `Total Committed` are recorded alongside it.
  - "After the trim" means the first sample taken at least 1 s after each touch of the flag file.
  - Each such sample must be below 1 GB, taken as 1,000,000,000 bytes (the stricter reading).
  - The maximum over the whole run is reported as well.
- **Added after trial runs, before the final runs (measurement conditions; the criteria themselves are unchanged):**
  - **The window is kept on top (`--on-top`).** Windows 11 drops a fully occluded or minimized window's process to the default 15.6 ms timer resolution. In trial runs with the window behind another one, every 4 ms heartbeat fired about 11.5 ms late (lateness p50 11.5 ms, every beat a "stall") while the GUI thread was idle. That is an instrument artefact, not a stall. With the window visible, lateness p50 is 0.004–0.02 ms.
  - **A run is valid only if nobody touches the machine.** The GPU sampler also records the session's last-input time (`GetLastInputInfo`) and whether the window is minimized. A run with keyboard or mouse input, or with the window minimized, during the measured window is repeated, up to 3 attempts. Every attempt is recorded. In trial runs, input during a run produced 0.1–4.5 s frame gaps and stalls of up to 0.6 s.
  - **Diagnostic, not a criterion: `--gui-pan`.** `--auto-pan` (Task 3) computes the pan on the render thread from the clock, and the render thread repaints without syncing with the GUI thread. Steady-state panning therefore never blocks the GUI thread. `--gui-pan` drives the same pan from a QML animation on the GUI thread, so every frame needs a scene-graph sync, as a pan driven by mouse or tablet events would. It is run idle and under load and reported next to the criteria.
- **Criterion 4:** run with ComfyUI stopped. The script checks the exact message, exit code 2, and a wall time under 5 s.
- **D3D12 rule, condition 2:** "no worse than D3D11's (within 10 %)" means D3D12 load-run p99 ≤ 1.10 × D3D11 load-run p99, for the stall series and for frame time separately.
- **D3D12 rule, condition 3:** the benchmark is Task 3's: `--recomposite --gpu-timestamps`, 60 output tiles per frame, GPU time per frame from `lastCompletedGpuTime()`.
  - Composite time is the p50 of that GPU time minus the p50 of the idle (display-only) run, the same derivation as Task 3's "26 µs per tile". It still includes the 13 mip passes over the dirty region, which are the same in both variants.
  - The in-place variant passes if its composite time is ≤ 0.75 × the two-pass composite time.
  - All the runs for this condition use D3D12, back to back in the same session.
  - The full-frame GPU p50 is reported too.
  - **Added after a preliminary run, before the final one.** The in-place variant needs SM 6.x on D3D12 (built with the SDK's dxc, as agreed during M0 review), and a first run showed that the shader compiler alone (dxc DXIL vs fxc DXBC) moves the composite time by tens of percent.
    - So the two-pass version it is compared against is **the same two-pass shaders built as DXIL SM 6.6** (`twopass-sm66`). Only the pass structure then differs.
    - The ratio against Task 3's DXBC two-pass is reported as well.
    - Every variant runs in 3 interleaved rounds, and the median is used.

## 5. Measurements

### Measurement conditions (read this first)

- **Someone used the machine during every final run.** `GetLastInputInfo` showed keyboard or mouse input in 17–75 of the 120 one-second samples of each run, so none met the "no input" validity rule.
  - The window was never minimized, and it stayed on top.
  - D3D11 used all 3 attempts per run. Its attempts gave similar frame times (idle frame p99 6.97 / 7.04 / 6.78 ms; load frame p99 12.50 / 12.40 / 11.37 ms), so the input had no visible effect on frame times. The load-run stall maximum was not the same across attempts: 5.3 / 8.9 / 6.3 ms. With only a few stalls per run, whether input caused any of them cannot be told.
  - The remaining runs were then made with `-Attempts 1`.
  - The tables report the last attempt. Every attempt, with the times of its input samples, was recorded in `frametimes-summary.json` (`attempts`, `conditions`) (raw output kept privately).
- **Load:** each load run completed 17 Z-Image Turbo prompts with no failures: p50 7.0–7.1 s, the first about 10 s (a cold load after `/free`).
  - The GPU was busy: `nvidia-smi` read 97–98 % in steady state and a mean of 89–91 % over the run.
  - About 15 GB of the 16 GB was in use.

### Criteria runs (`--auto-pan`, 120 s measured, 5 s warm-up excluded)

| Backend, run | Metric | n | p50 | p95 | p99 | max | Unit |
|---|---|---|---|---|---|---|---|
| D3D11 idle | frame time | 28717 | 4.163 | 4.363 | **6.784** | 17.366 | ms |
| D3D11 idle | stalls (> 4 ms late) | 0 | – | – | – | – | ms |
| D3D11 idle | heartbeat lateness, all beats | 29999 | 0.013 | 0.724 | 1.088 | 2.238 | ms |
| D3D11 load | frame time | 22612 | 4.243 | 8.432 | **11.369** | 50.757 | ms |
| D3D11 load | stalls | 3 | 4.347 | 6.301 | **6.301** | 6.301 | ms |
| D3D11 load | heartbeat lateness, all beats | 29994 | 0.019 | 0.769 | 1.146 | 6.301 | ms |
| D3D12 idle | frame time | 28777 | 4.164 | 4.450 | **5.822** | 19.057 | ms |
| D3D12 idle | stalls | 0 | – | – | – | – | ms |
| D3D12 idle | heartbeat lateness, all beats | 29997 | 0.016 | 0.832 | 1.150 | 1.977 | ms |
| D3D12 load | frame time | 28308 | 4.166 | 7.923 | **8.648** | 29.634 | ms |
| D3D12 load | stalls | 6 | 4.486 | 7.475 | **7.475** | 7.475 | ms |
| D3D12 load | heartbeat lateness, all beats | 29990 | 0.014 | 0.749 | 1.110 | 7.475 | ms |

- **Warm-up (first 5 s, not in the criteria).** Stalls: n 82, p99 19.9 ms (D3D11 idle); n 75, p99 16.1 ms (D3D11 load); about 40, p99 25–28 ms (D3D12). Section 5.3 explains them.
- **Under load, D3D11 presents fewer frames than D3D12** (22,612 vs 28,308 in 120 s), but its frame-time p99 stays well inside 33 ms.

Raw data: (raw output kept privately). It holds every frame interval, each stall with its time and sync overlap, each trim, and the summary (criteria, conditions, PDH samples, ComfyUI prompt times).

### Criterion 3: GPU memory of the canvas process (PDH Dedicated Usage)

| | D3D11 | D3D12 |
|---|---|---|
| Trims (one per prompt) | 17 / 17 touches, each 6–62 ms after the touch | 17 / 17, 7–61 ms |
| Tile cache after each trim | ≤ 76 MiB (from 128–512 MiB) | ≤ 76 MiB |
| **First sample ≥ 1 s after each trim, max** | **764.3 MiB = 801,435,648 B → PASS** | **957.4 MiB = 1,003,888,640 B → FAIL** (3.9 MB over 1e9 B, after the first trim) |
| First sample ≥ 1 s after each trim, min | 432.3 MiB | 553.4 MiB |
| Process with the cache empty (projection + 13 mips + intermediate + swap chain) | 432 MiB | 553 MiB |
| Whole run, max (the cache at its 512 MiB cap, before the first trim) | 944.3 MiB = 990,183,424 B | 1137.4 MiB = 1,192,628,224 B |

- **The trim itself works, but the cache regrows within the same second.** While the view pans, `allocSlot()` adds a whole 2048-slice array (128 MiB) at a time. One second after the first trim the D3D11 process was already back at 764 MiB, while the cache had been at 76 MiB just after the trim.
- **The 1 GB limit is met on D3D11 and missed narrowly on D3D12.** D3D12 holds about 120–190 MiB more for the same resources.
- **The D3D11 pass does not discriminate.** D3D11's whole-run maximum (944 MiB = 990,183,424 B, with the cache at its 512 MiB cap) is already under 1e9 B without any trim. The "after trim" sample mostly measures how fast the cache regrows.
- **The D3D12 miss is at the first trim, during start-up.** It is the first sample after the first trim (prompt 0, t ≈ 5 s), which falls during the cold model load. That the cache had regrown by then is **inferred** from the timing and from the regrowth seen after every other trim; the cache size at that sample was not measured directly.

### Criterion 4: negative check

`python spikes/canvas/load/comfy_load.py --seconds 10` with ComfyUI stopped printed `ComfyUI unreachable at 127.0.0.1:8188` and exited 2 in **2.12 s** (raw output kept privately).
- It was also the first thing run, before ComfyUI was started: same output, exit 2, 2.42 s.
- Nothing listens on the port, but Windows retries the refused loopback connection for about 2 s, which is why it takes 2 s.

### 5.3 Stall analysis: spike artefacts vs the design

1. **Instrument artefact: timer resolution.** When the canvas window was behind another window, Windows 11 gave the process the default 15.6 ms timer resolution. The 4 ms heartbeat then fired every ~15.5 ms, and every beat counted as an ~11.5 ms "stall" while the GUI thread did nothing.
   - In an 8 s trial: lateness p50 11.17 ms with the window behind, 0.006 ms with it on top.
   - Raw data: (raw output kept privately).
   - Fixed by `--on-top`.
2. **Task 3's "idle p99 20.6 ms" was start-up, not the steady state.** For the first ~3.6 s after launch the window's UI requests scene-graph syncs at about 75 Hz.
   - Without the canvas (`--no-canvas`) each sync costs the GUI thread 0.03 ms.
   - With the canvas repainting continuously, each one waits for the render thread's frame in flight, 4.5–8 ms: one to two vsyncs.
   - Task 3's 6 s series included that start-up. Here it falls in the 5 s warm-up (stall p99 16–28 ms).
   - In steady-state `--auto-pan` the GUI thread never syncs, because the render thread computes the pan and repaints via `QQuickRhiItemRenderer::update()` without a sync. Both idle runs had **0 stalls in 120 s**.
3. **The spike's own GUI-thread work is not a stall source in the steady state.**
   - Logging (the JSON-lines log, flushed per event), the 50 ms trim-flag poll and the per-frame `onFrame` hop produced no stall in the idle runs; logging was on in every run.
   - Read-backs and tile preparation only happen in golden or composite mode, or on the render thread.
   - The control without the canvas had 1 stall in 60 s (7.3 ms).
4. **The real design effect: the sync couples the GUI thread to the GPU.** Under load, the stalls in the criteria runs come from the trims. Each trim is an `update()` from the GUI thread, so it needs a sync, and the sync waits for the render thread, which is waiting on the GPU.
   - Every load run had 17 syncs: p50 4.2 ms, max 8.4 ms (D3D11) and 6.1 ms (D3D12).
   - 99 % (D3D11) and 61 % (D3D12) of the stall time fell inside those sync waits.
   - `--auto-pan` hides this, because the steady state needs no sync. A pan driven by input events needs one every frame. That is the `--gui-pan` diagnostic:

| `--gui-pan` (diagnostic) | n | p50 | p95 | p99 | max | sync waits (n / p99 / max) | stall time in sync waits |
|---|---|---|---|---|---|---|---|
| D3D11 idle, frame time | 28700 | 4.165 | 4.444 | 6.452 | 15.364 | | |
| D3D11 idle, stalls | 9 | 4.866 | 8.974 | 8.974 | 8.974 | 28701 / 0.61 / 8.5 ms | 80 % |
| D3D11 load, frame time | 20900 | 4.451 | 8.441 | 11.319 | 276.671 | | |
| **D3D11 load, stalls** | 924 | 4.319 | 8.705 | **17.760** | 272.176 | 20901 / 5.46 / 273 ms | **95.5 %** |
| D3D12 idle, frame time | 28753 | 4.162 | 4.568 | 5.863 | 16.577 | | |
| D3D12 idle, stalls | 18 | 6.206 | 11.704 | 11.704 | 11.704 | 28755 / 0.22 / 12.2 ms | 95 % |
| D3D12 load, frame time | 28028 | 4.164 | 4.926 | 8.453 | 187.276 | | |
| **D3D12 load, stalls** | 149 | 5.136 | 21.683 | **41.122** | 182.921 | 28029 / 4.21 / 183 ms | **97.8 %** |

   - With the GUI driving each frame, APP-135 fails under ComfyUI load on both backends: stall p99 17.8 ms (D3D11) and 41.1 ms (D3D12), against < 16 ms.
   - Almost all of that stall time is the GUI thread blocked in the threaded render loop's sync while the render thread waits on a GPU that ComfyUI is saturating.
   - A trial run before input tracking gave 17.3 ms and 29.7 ms, the same picture.
   - Some of the largest stalls (> 16 ms) fall within 1.2 s of a recorded input sample: 7 of 11 on D3D11, 5 of 10 on D3D12. With 1 s sampling, input cannot be ruled out for those particular outliers. The p99 is set by the many sync waits, not by them.
   - Raw data: (raw output kept privately). The per-frame sync list there is trimmed to waits > 4 ms.

A trial D3D12 load run (before the final runs) had one ComfyUI prompt that took 73.9 s instead of ~7 s, at 13–22 s/it for 4 steps, while the canvas held 757–821 MiB. It did not recur in the final runs (max 10.2 s), so its cause is not known. Its timing fits IMG-040's concern: the cache regrows during the prompt, after ComfyUI has sized its model loading.

### D3D12 rule, condition 3: composite variants

Task 3's benchmark: 60 tiles per frame recomposited, 20 layers, 8K. The table gives the GPU composite time per frame (p50 − idle p50), as the median of 3 interleaved rounds. The `golden` column is max |GPU − CPU| on Task 3's golden scene, with the debug layer on.

| Variant | Passes | Compiler | D3D12 | D3D11 | golden |
|---|---|---|---|---|---|
| `twopass` (Task 3, IMG-031) | group → RGBA32F intermediate, then parent | fxc DXBC 5.0 | 1.103 ms | 1.186 ms | ±1 |
| `twopass-sm66` | same | dxc DXIL 6.6 | **0.754 ms** | – | ±1 |
| `single` | one pass, the group in registers | fxc DXBC 5.0 | 1.062 ms | 1.070 ms | ±1 |
| `single-sm66` | same | dxc DXIL 6.6 | 0.662 ms | – | ±1 |
| **`inplace`** | one pass, projection read-write (RGBA8 typed UAV load) | dxc DXIL 6.6 | **0.661 ms** | – | ±2 (24 px > 1) |
| `inplace-sm50` | same | fxc DXBC 5.0 | 1.035 ms | 1.050 ms | ±2 |

- **The in-place read-write composite cuts the composite time by 12.4 % against the same two-pass shaders built for SM 6.6** (0.661 vs 0.754 ms). That is **below the 25 % required.**
  - Rounding: 12.4 % comes from the unrounded medians in `composite-variants.json` (0.660736 vs 0.754144 ms, a 12.39 % cut). Computed from the rounded table values (0.661 vs 0.754 ms) it is 12.3 %. Either way it is far below 25 %.
  - Against Task 3's DXBC two-pass it cuts 40.1 %. But single-sm66 vs twopass-sm66 shows that removing the pass is worth about 12 %. The rest is the compiler: dxc DXIL vs fxc DXBC for the same two-pass shaders is 0.754 vs 1.103 ms (−32 %).
  - A one-pass composite with the group in registers (`single`, no read-write) is as fast as the in-place one and stays within ±1. The in-place one is ±2, because it parks the backdrop as RGBA8 in the projection.
- **fxc accepts the RGBA8 typed UAV load in SM 5.0.** It flags the shader "Typed UAV Load Additional Formats", and `inplace-sm50` runs on D3D11 on this GPU (±2, 0 debug-layer messages).
  - IMG-030's "D3D11 (SM 5.0) can't read images in place" is therefore a hardware-capability limit, not an API limit. The test machine's GPU reports `TypedUAVLoadAdditionalFormats`.
  - Half-float images were not tested.
- First rounds after ComfyUI's `/free` were slower (twopass 1.47 ms in round 1, 1.10 ms in rounds 2 and 3), which is why the medians are used.

### D3D12 rule, condition 1: Task 3's criteria on the final build

`run-composite-check.ps1 -Backend d3d11|d3d12 -NoBuild` on this commit printed **ALL CRITERIA PASSED** on both backends: max diff 1, no hazards, mips ±1, 10 recreations. The output is raw output kept privately; Task 3's output was left untouched.

## 6. Verdict

**PASS on criteria 1, 2 and 4 on both backends; criterion 3 PASS on D3D11 and FAIL by 3.9 MB on D3D12. A finding outside the criteria: APP-135 fails for a pan driven from the GUI thread.**

**Input caveat for criteria 1–3.** Every final run had keyboard or mouse input during its measured window (17–75 of the 120 one-second samples, §5), so none met the "no input" validity condition set before the run. The verdicts below are on those runs. In trial runs input only added frame gaps and stalls, so it can make a criterion harder to pass, not easier, and the passes stand. Small differences between runs or between backends, however, may be input rather than the canvas.

1. **Criterion 1 (idle) passes** (input present). Frame-time p99 is 6.78 ms on D3D11 and 5.82 ms on D3D12, against 16.7 ms, with 0 GUI stalls in 120 s.
2. **Criterion 2 (load) passes** (input present). Stall p99 is 6.30 ms on D3D11 (n = 3) and 7.48 ms on D3D12 (n = 6), against < 16 ms. Frame-time p99 is 11.37 and 8.65 ms, against the 33 ms target.
   - Caveat: with `--auto-pan` the GUI thread syncs only at the 17 trims. The pass therefore shows that the render thread and the composite design keep up under load. It does not show that the GUI thread is protected.
   - The `--gui-pan` diagnostic, which is what an input-driven pan does, has a stall p99 of 17.8 ms on D3D11 and 41.1 ms on D3D12 under the same load. 95–98 % of that stall time is GUI-thread sync waits on a render thread held up by the GPU.
3. **Criterion 3 (trim) passes on D3D11 and fails on D3D12.** The D3D11 pass does not discriminate: even untrimmed, at its 512 MiB cache cap, the D3D11 process stays under 1e9 B.
   - The trim works, within 62 ms of each touch, down to ≤ 76 MiB of cache.
   - D3D11: the first sample ≥ 1 s after each trim is at most 764 MiB.
   - D3D12: 957.4 MiB = 1,003,888,640 bytes after the first trim, over the 1,000,000,000-byte line.
   - On both backends the cache grows back by 128 MiB arrays within a second while the view pans.
   - The D3D12 FAIL stands as the criterion was pre-registered (1,000,000,000 bytes, first sample ≥ 1 s after each trim), although it is by 3.9 MB and the D3D11 pass does not discriminate.
4. **Criterion 4 passes.** Exact message, exit 2, 2.1 s.

## 7. Decision

- **IMG-033 (D3D12): not built in M1b. D3D11 stays the only path.**
  - **The decision rests on condition 3**, which fails on its own in both same-compiler comparisons.
  - Condition 2 is inconclusive and does not contribute to the decision.

  Rule conditions:
  1. **Met.** D3D12 passes Task 3's criteria 1–4, re-run on this build.
  2. **Inconclusive (too few events, input contamination).** On paper, D3D12's load-run stall p99 of 7.475 ms is +18.6 % over D3D11's 6.301 ms. Its frame-time p99 is better: 8.648 vs 11.369 ms. The stall comparison cannot carry a verdict:
     - Each p99 is the maximum of a tiny series: n = 6 (D3D12) and n = 3 (D3D11).
     - The same D3D11 load run, repeated 3 times, gave a stall maximum of 5.3, 8.9 and 6.3 ms. That spread alone covers the gap.
     - **The 7.475 ms D3D12 stall is not a sync wait and may be input.** It happened at t = 72.267 s with no sync-wait overlap. There was user input within ~1 s of it: `user_idle_ms` was 0, 110 and 47 at the one-second samples at 70.7, 71.7 and 72.8 s, and the canvas window was not in the foreground at those samples.

     The `--gui-pan` diagnostic (stall p99 41.1 vs 17.8 ms; D3D12 has fewer stalls, 149 vs 924, but a longer tail, p95 21.7 vs 8.7 ms) leans the same way. But it is not the rule's measurement, and it too was taken with input present.

     *Note, 2026-10-12 (after review): condition 2 was first written up as "Not met" (+18.6 % stall p99, over the +10 % allowed). Review re-read it as "Inconclusive" for the reasons above. The decision did not change, because condition 3 fails on its own.*
  3. **Not met, in both same-compiler comparisons.** Removing the pass with the in-place read-write composite cuts the composite time:
     - **12.4 %** with the DXIL SM 6.6 build: 0.661 vs 0.754 ms (12.4 % from the unrounded medians; 12.3 % from the rounded values shown);
     - **6.2 %** with the DXBC SM 5.0 build: `inplace-sm50` 1.035 vs `twopass` 1.103 ms.

     Both are short of the 25 % required.
- **M1b roadmap entry:** "IMG-033 Direct3D 12 path — not scheduled. Decided in M0 spike 04: D3D11 only."
  - Reopen only if one of these makes D3D12 the only route to something needed:
    - the dxc compiler advantage (below);
    - a feature D3D11 truly lacks.
- **IMG-153:** the store-only canvas stays smooth while ComfyUI saturates the GPU. At 240 Hz, frame-time p99 is 6.8 ms idle and 11.4 ms under load on D3D11, and 5.8 and 8.6 ms on D3D12.
- **APP-135:** the M0 measurement passes with the render-thread pan. It does **not** pass when every frame needs a GUI sync. M1 must keep per-frame canvas view changes off the GUI-thread sync path (follow-up 1).

## 8. Follow-ups

1. **APP-135, canvas input path (M1).** Do not drive pan, zoom or stroke preview through QML properties and `update()` on the GUI thread. Every such frame is a threaded-render-loop sync, and under ComfyUI load the GUI thread then waits on the GPU (stall p99 17.8–41 ms here).
   - Instead, hand view changes to the canvas renderer through a thread-safe queue, as `--auto-pan` effectively does, and let it repaint from the render thread with `QQuickRhiItemRenderer::update()`, which needs no sync.
   - Re-measure APP-135 with an input-driven pan in M1.
2. **IMG-040, trim must also hold (M1a).** The trim drops the cache to the visible slices, but it grows back by 128 MiB arrays within a second while the view moves. For as long as a ComfyUI job is loading or running:
   - cap the cache at the trimmed size plus a small margin;
   - allocate small arrays.

   Otherwise the canvas reclaims the VRAM ComfyUI sized its model loading against. That is the very case IMG-040 guards against, and a candidate for the 74 s prompt seen in a trial.
3. **Canvas VRAM budget.** The fixed part (one 8K projection, 13 separate mip textures, a 16 MiB intermediate, the swap chain) is 432 MiB on D3D11 and 553 MiB on D3D12, before any tile cache. Paging the projection (IMG-040's "visible pages") would be needed to make "a few hundred MB" true at 8K.
4. **IMG-030 wording.** SM 5.0 typed UAV loads of RGBA8 work when the GPU reports `TypedUAVLoadAdditionalFormats`, which fxc flags. "D3D11 can't" should become "not guaranteed on D3D11; store-only by design".
   - The one-pass composite with the group in registers (`single`) needs no read-write at all.
   - Its saving over the two-pass composite, with the same compiler, is:
     - 12.2 % with D3D12 DXIL (0.662 vs 0.754 ms), the same as in-place;
     - 3.7 % with D3D12 DXBC (1.062 vs 1.103 ms);
     - 9.8 % with D3D11 DXBC (1.070 vs 1.186 ms).

     Unlike in-place, it stays within ±1.
   - It handles any nesting depth that fits a fixed register stack.
   - M1 could use it instead of IMG-031's intermediate pass.
5. **Shader compiler.** dxc DXIL SM 6.6 runs the same composite 32 % faster than fxc DXBC on D3D12. If composite cost ever matters more than the extra D3D12 stall risk, that is the argument for reopening IMG-033, not the in-place composite.
6. **Measurement hygiene.** Frame-time spikes need:
   - the window visible and on top (the Windows 11 timer-resolution rule);
   - no user input.

   Future runs should check both, as `sample-gpu.ps1` now does. This run could not get an input-free window.
