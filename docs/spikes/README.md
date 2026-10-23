# M0 spike results

Plan 0 (`docs/superpowers/plans/2026-10-12-m0-spikes.md`) ran on 2026-10-12 on the test machine: Windows 11 Pro, 16 GB consumer GPU, ComfyUI 0.39.2 with frontend 1.53.10. Each decision is recorded in the spec (`docs/superpowers/specs/2026-10-12-lilith-design.md`) as an "(M0 result, 2026-10-12: …)" note on the rule it settles. No rule was renumbered or deleted. IMG-033 is marked *withdrawn* and keeps its ID.

There is no spike 05. Pen input (IMG-046, IMG-154) is on hold by project decision, so that spike was not run.

## Summary

| Spike | Rules | Verdict | Decision | Results doc |
|---|---|---|---|---|
| 01 Toolchain | QA-077, QA-078, OPS-007 | PASS | Qt 6.12.0 + Qt WebEngine 6.140.0. CI installs with `naqt` (commit `43da90f1`) and `--extensions qtwebengine@6.140.0`; without the version naqt picks an older July build, so CI asserts 6.140.0. aqtinstall PR #1048 is still unmerged. git-lfs comes from Git for Windows ≥ 2.56.0.2 (git-lfs 3.8.0); winget lags at 2.55, so doctor needs a direct-download fallback. The test machine is now on Git 2.56.0 / git-lfs 3.8.0. | [01-toolchain.md](01-toolchain.md) |
| 02 Qt stack | ARCH-114, ARCH-115, ARCH-116, APP-137 | PASS (7/7) | M1 pins Qt 6.12.0, WebEngine 6.140.0, cxx-qt 0.10.0, KDDockWidgets 2.4.1. The 6.11 fallback is not needed. Floating docks recreate the canvas renderer cleanly. WebEngine stays off the startup path with the `Loader` pattern. | [02-qt-stack.md](02-qt-stack.md) |
| 03 Canvas compositing | IMG-152, IMG-030–032, IMG-035, IMG-036 | PASS on D3D11 (and on D3D12) | Store-only compute compositing works as specified on D3D11. The GPU matches the CPU reference within 1/255, but not bit for bit, so golden tests use a ±1/255 tolerance (IMG-036). | [03-canvas-compositing.md](03-canvas-compositing.md) |
| 04 Frame times, D3D12 rule | IMG-153, APP-135, IMG-040, IMG-033 | Criteria 1, 2, 4 PASS; criterion 3 PASS on D3D11, FAIL on D3D12 (by 3.9 MB). Every final run had user input. | **IMG-033 withdrawn:** D3D12 is not built, because condition 3 failed (12.4 % DXIL, 6.2 % DXBC against 25 %) and condition 2 was inconclusive. On D3D11 the load stall p99 was 6.3 ms. A GUI-thread-driven pan fails APP-135 under load (17.8 ms), so canvas input must avoid per-frame GUI-thread syncs. The tile cache regrows after a trim, so it needs a cap while a ComfyUI job runs. | [04-frametimes.md](04-frametimes.md) |
| 06 WebEngine ↔ ComfyUI | IMG-159, IMG-124, IMG-127, IMG-129, APP-137 | PASS (7/7) | Open in ComfyUI is `app.loadGraphData(graph, true, true, null, {openSource})`, with a base64 literal decoded in the page through a strictly anchored allowlist, pinned to frontend 1.53.10. The hidden off-the-record page is safe, so IMG-124 needs no fallback; a first-ever frontend boot writes 3 settings keys. Update source compares a normalised graph (the frontend adds `proxyWidgetErrorQuarantine`). WebEngine must live in a lazily loaded library. | [06-webengine-comfyui.md](06-webengine-comfyui.md) |
| 07 Clipboard and drag-out | APP-068, APP-069, APP-071 | PASS (manual checks 3, 4, 5b, 6 from a one-line summary) | The `QMimeData` recipe is confirmed. The exact PNG bytes need `setData("application/x-qt-windows-mime;value=\"PNG\"")`. Qt's extra URL and `image/*` formats are tolerated, with a native `IDataObject` as the fallback. Drags are copy-only. | [07-clipboard.md](07-clipboard.md) |
| 08 ComfyUI measurements | IMG-155, IMG-157, IMG-158, IMG-160 → IMG-111, IMG-107, IMG-045 | PARTIAL: the fit rule was inconsistent across repeats; LoRA, SAM, previews and the negative check passed | Z-Image default: int8_convrot + lite ControlNet on cu130, by the IMG-111 default, because bf16 was not shown to fit reliably (decided during M0 review). The canvas + WebEngine took ≈ 2.2 GB untrimmed (with IMG-040's trim/cap the canvas is ≈ 0.43–0.76 GB; the fit was not measured in that state). Peak and log criteria don't discriminate under dynamic VRAM. int8 LoRA is `supported`. Select object takes ≈ 0.5 s warm and 1.2–1.3 s after a generation, and SAM evicts Z-Image (+2–3.5 s). Previews use TAEF1 only with `preview_method: "taesd"`; the file is `taef1_decoder.pth` from GitHub. The test machine's ComfyUI is now on cu130. | [08-comfy-measurements.md](08-comfy-measurements.md) |
| 09 Scheduled task | OPS-078, OPS-079 | PARTIAL: criteria 1, 2, 3, 5 PASS; criterion 4 (no console flash) FAIL | A non-elevated shell can register the per-user task (a separate standard account was not tested). The console flash confirms that OPS-078's windowless launcher is necessary. The task action must be the launcher's absolute path, because a bare `pwsh` fails with 0x80070002. | [09-scheduled-task.md](09-scheduled-task.md) |
| 10 App shell (plan 4a) | APP-005, APP-135, APP-136, APP-137, APP-156, ARCH-114, ARCH-116, IMG-126 | PASS, with recorded deviations (S0.6 GPU level 90 %, S0.7 by UI Automation, S0.5 (a) on an idle machine) | KDDockWidgets 2.5.0. P4a-R1's feature route works with the Qt crate named `lil_qt` (cxx-qt-cmake 0.10.0 fails on a dashed name). WebEngine in a lazily loaded QML plugin, with a prototype-then-instance profile. Trimmed deploy 332 MiB (soft target 300; two cuts reach about 296). Cold start about 0.4–0.6 s. Under ComfyUI load, heartbeat p99 2.8 ms and frame p99 8.5 ms (isolated maxima 0.42 s). Shell VRAM with the Web dock 126 MiB peak (P4a-R15). Offscreen tests need only `QT_QPA_PLATFORM=offscreen`, and CI with Qt takes 26 min, so no split. | [10-app-shell.md](10-app-shell.md) |

## Resolved by existing rules — for review

No spike result needs a new project decision. Each failed or partial result below was resolved by an existing rule and is listed here for review. Implementation choices left to the plans are listed separately, under "Open technical choices for plan authors".

- **09, criterion 4 FAIL (console flash).** `pwsh -WindowStyle Hidden` flashed a console. OPS-078 already requires a windowless launcher, so the FAIL confirms the launcher is needed. Plan 7+ builds it and registers it by absolute path.
- **04, criterion 3 FAIL on D3D12 (by 3.9 MB).** This concerns a path that won't be built. The pre-registered D3D12 rule (IMG-152) failed condition 3, so IMG-033 is withdrawn and D3D11 stays the only path.
- **08 PARTIAL: IMG-111 is decided as int8.** The fit rule gave different answers on identical repeats. The default is int8 + lite ControlNet because that is the spec's default unless bf16 is shown to fit, and bf16 was not shown to fit reliably (decided during M0 review).
- **OPS-079: standard-account gap.** The task was registered from a non-elevated token of an admin-group account. A separate standard (non-admin) account was not tested.

## Open technical choices for plan authors

These are implementation choices for the plan named. They aren't project decisions, but are listed for visibility.

- **Plan 6:** which ComfyUI memory mode the suite ships with: dynamic VRAM (the default) or `--disable-dynamic-vram`, and the matching fit signal (08 follow-up 1). Also whether to fight SAM evicting Z-Image (`--cache-lru`, `--cache-ram`) or accept it and show "Loading model…" (08 follow-up 4).
- **Plan 4:** whether to add the Vulkan SDK (`spirv-opt`) so `qt_add_shaders` can use `OPTIMIZED`, or ship unoptimised SPIR-V (02 follow-up 1). This only matters for non-D3D backends.
- **Plans 5–6:** whether to set ComfyUI's `--reserve-vram` to the canvas budget, since Windows evicts canvas VRAM under ComfyUI load (08 follow-up 3).

## Carry-forward

`docs/superpowers/plans/2026-10-12-roadmap.md` ("M0 results that change later plans") lists the items plans 4–7 must pick up.
