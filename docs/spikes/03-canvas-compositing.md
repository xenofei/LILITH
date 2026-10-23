# 03 Canvas compositing on D3D11 and D3D12

Date: 2026-10-12

GPU: a 16 GB consumer GPU (name and driver version removed). Qt 6.12.0 (Qt Quick on Direct3D 11 by default; Direct3D 12 via `QSG_RHI_BACKEND=d3d12`). ComfyUI is not involved in this spike.

## 1. Question

- **IMG-152:** does compute compositing work on Qt 6.12's D3D11 backend with the design the spec fixes, and on D3D12 too? The design is store-only writes, multi-pass isolated groups, separate mip textures and no sRGB flag.
- **IMG-030:** layer tiles are only sampled, and every projection write is store-only (no dispatch reads and writes the same image).
- **IMG-031:** isolated groups use an intermediate texture that a second pass samples. Layer tiles are found through a storage-buffer slot table, and curves are bound as a lookup texture.
- **IMG-032:** each mip level is a separate texture.
- **IMG-035:** no sRGB flag, so blending happens in encoded sRGB.
- **IMG-036:** does a Rust CPU compositor with the same formulas match the GPU?

## 2. Setup

| Item | Version |
|---|---|
| OS | Windows 11 Pro 10.0.26200 |
| GPU / driver | 16 GB consumer GPU |
| Graphics Tools (debug layers) | installed: `d3d11_3SDKLayers.dll` and `d3d12SDKLayers.dll` 10.0.26100.9549 in System32. Nothing was installed for this spike. |
| Rust | 1.99.0 |
| CMake / Ninja | 4.4.4 / 1.13.2 |
| MSVC | cl 19.44.35227 (VS 2022 Build Tools), Windows SDK 10.0.26100 |
| Qt | 6.12.0 `msvc2022_64` |
| cxx-qt / cxx | 0.10.0 (pinned `=0.10.0`) / 1.0.205; `Cargo.lock` committed |
| KDDockWidgets | 2.4.1 (FetchContent), Qt Quick frontend |
| Shaders | `qt_add_shaders(PRECOMPILE GLSL 430 HLSL 50 MSL 12)`: HLSL is compiled to DXBC (SM 5.0) by `fxc`. No DXC, and no `OPTIMIZED` (no spirv-opt). |
| Spike code | `spikes/canvas/` in the commit that adds this document |

## 3. Method

### What was built (`spikes/canvas/`)

The app is the Task 2 skeleton with the Canvas dock only: a KDDockWidgets window, a shim DLL and a cxx-qt crate. The Web and Counter docks are gone.

- **`shim/CanvasDocument`** generates the test documents in code from a fixed seed, so no golden images are committed.
  - The **golden scene** (1024×1024) is, bottom to top:
    1. paper: normal, 100 %, opaque;
    2. blobs: normal, 70 %, sparse;
    3. an **isolated group** (normal, 80 %) containing ink (multiply, 100 %, left three quarters only) and glow (screen, 60 %, sparse);
    4. grain: overlay, 90 %;
    5. a **curves** adjustment at 75 % (different S, gamma and linear curves per channel);
    6. veil: normal, 50 %, with every third tile empty.

    That is 6 pixel layers, 4 blend modes, 316 non-empty layer tiles and 64 output tiles.
  - **Procedural documents** come from `--layers N --doc WxH`. Each layer has a pool of 8 tile variants and 75 % tile coverage. There is an isolated group of layers 1–2 (unless `--no-group`) and curves after layer N/2.
- **`shim/CompositeRenderer`** is the `QQuickRhiItem` and its renderer.
  - **Tile cache:** RGBA8 2D-array textures of 128×128 slices, created without the sRGB flag and bound only as sampled textures (`tiles0..3`, up to 4 arrays of 2048 slices). A storage buffer `slots[row * layers + layer] = (array << 16) | slice`, or −1 for an empty tile, maps (layer, tile) → slice. Missing tiles are uploaded under a 24 MiB-per-frame budget, nearest-to-view first, with LRU eviction.
  - **Pass 1:** `composite_group.comp.qsb`. For each output tile, every isolated group is composited onto transparent black into its own 128×128 region of an **RGBA32F intermediate** (premultiplied, unclamped).
  - **Pass 2:** `composite_main.comp.qsb`. One dispatch per output tile (8×8 groups of 16×16). Each invocation loops over the parent stack bottom to top in registers. It samples each group's intermediate region as a single layer, applies normal, multiply, screen or overlay plus opacity and visibility, applies the curves from a 256×N RGBA8 lookup texture (two `texelFetch` and a linear mix), and writes the **RGBA8 projection** once with `imageStore`. Nothing in the pass reads the projection.
  - **Mips:** level k is a separate RGBA8 texture. `downsample.comp` samples level k−1 and stores level k, one compute pass per level, dispatched over the dirty region only.
  - **Display:** `display.frag` picks the two levels around log2(1/zoom), samples each bilinearly (`textureLod`) and mixes them. The mips are separate textures, so the shader selects them with a branch over 8 bindings.
  - **Disjointness assertion:** every SRB records the textures it binds for sampling and for image store. Each compute pipeline creation and each dispatch asserts that the two sets are disjoint and counts the checks.
  - **Debug-layer capture:** each frame, the D3D11 or D3D12 `InfoQueue` (queried from the QRhi native device) is drained into the log. Messages of severity warning or worse go to the log one by one.
  - **Read-back:** `QRhiReadbackResult` on the projection and on every mip texture.
- **`rust/src/cpu_composite.rs`** (crate `canvas_bridge`, cxx-qt bridge) is the CPU reference. It uses the same formulas and order of operations in `f32`: straight-alpha source × opacity, premultiplied W3C separable blend formulas, an `f32` group buffer, curves on un-premultiplied colour, and `round(clamp(x) × 255)`. It exposes:
  - `cpu_composite_tile(layers: &[LayerTile], out: &mut [u8; 65536])`, where `LayerTile` is `{kind, blend, opacity, visible, pixels: &[u8]}` and the stack is flat, with GROUP_BEGIN/END entries;
  - `cpu_downsample`;
  - `cpu_max_abs_diff`.
- **`CanvasController`** (in the exe):
  - counts frames (`frameSwapped`, timestamped on the render thread);
  - recreates the canvas item through a QML `Loader`, which destroys and recreates the node and renderer;
  - watches the trim flag;
  - records frame times and 4 ms heartbeat lateness;
  - builds the CPU golden at start-up, compares every read-back with Rust and saves the raw files.

### Switches (Task 4 uses these)

| Switch | Effect |
|---|---|
| `--layers N`, `--doc WxH`, `--no-group` | procedural document (without them, and with `--golden`, the golden scene is used) |
| `--backend d3d11\|d3d12` | sets `QSG_RHI_BACKEND` before Qt starts |
| `--golden <dir>` | golden scene; writes `cpu_L0.rgba`, `meta.json`, `gpu_first_L<k>.rgba`, `gpu_final_L<k>.rgba` and PNG previews to `<dir>` |
| `--frames N` | stops after N frames (in golden mode, after the final read-back that follows frame N) |
| `--frametimes <json>` | frame-to-frame times from `frameSwapped`, plus 4 ms `PreciseTimer` heartbeat lateness (a heartbeat more than 4 ms late is a stall). Each series reports n, p50, p95, p99, max and the raw samples. Also records per-frame GPU time (with `--gpu-timestamps`), tiles composited per frame and the cache size. |
| `--recreate-every N` | every N frames, deactivates and reactivates the canvas `Loader`, which destroys and recreates the item, node, renderer and every GPU resource |
| `--auto-pan` | pans at 1000 px/s (800 px/s in x and 600 px/s in y), bouncing at the document edges |
| `--trim-flag <path>` | when the file's mtime changes, the tile cache is trimmed to the visible tiles. New arrays are sized for the visible slices, each slice is copied on the GPU (`copyTexture`, no re-upload), and the old arrays are released. |
| `--zoom Z`, `--recomposite`, `--gpu-timestamps`, `--upload-mb N`, `--screenshot <png>`, `--quit-after <ms>`, `--log <file>` | view zoom; recomposite every visible tile every frame (timing); QRhi timestamps; upload budget; window grab; timeout; JSON-lines log |
| `--inject-hazard` | **negative control only:** the parent pass also samples the projection it writes |

### Re-run

```powershell
pwsh -File spikes/canvas/run-composite-check.ps1 -Backend d3d11 -Clean   # builds from scratch
pwsh -File spikes/canvas/run-composite-check.ps1 -Backend d3d12 -NoBuild
cargo test --manifest-path spikes/canvas/rust/Cargo.toml                 # after . spikes/toolchain/env.ps1
```

The script makes three runs per backend, all with `QSG_RHI_DEBUG_LAYER=1`:

- **Run A:** `--golden … --frames 120`.
- **Negative control:** `--inject-hazard --frames 30`.
- **Run B:** `--golden … --recreate-every 120 --frames 1200`.

It compares the raw RGBA8 files itself, in C# (independently of the exe's own Rust comparison, which is logged as `golden_compare`). It merges the results into `spikes/canvas/results/composite.json` under the backend's key.

Composite timing: `canvas.exe --layers 20 --doc 8192x8192 --backend <b> --frames 600 --gpu-timestamps --recomposite [--no-group] --frametimes …`. The view is at zoom 1 at the top-left of the document, so 60 visible output tiles are recomposited every frame. The `idle` run drops `--recomposite`.

## 4. Pass criteria

Copied from the plan (Task 3, Step 1) before the spike was run.

1. The GPU and CPU composites of the golden scene differ by at most 1/255 per channel on every pixel. The golden scene is 6 layers with 4 blend modes, one isolated group of 2 layers, one curves adjustment and partial opacity, on a 1024×1024 canvas.
2. No compute pipeline both reads and writes the same texture. Verify with the D3D11 debug layer enabled (`QSG_RHI_DEBUG_LAYER=1`): no errors or warnings about resource hazards.
3. Each mip level matches a CPU box-filter downsample of the level above to within 1/255.
4. With `--recreate-every 120 --frames 1200`, all 10 recreations rebuild the view (frame 1200 matches golden criterion 1), with no crash or device-removed error.
5. Criteria 1–4 are also run with `--backend d3d12`, and the results are recorded separately.

## 5. Measurements

### Correctness (criteria 1–4)

The two backends have separate columns (criterion 5).

| Metric | D3D11 | D3D12 |
|---|---|---|
| C1 projection max \|GPU − CPU\| per channel (1024×1024) | **1** | **1** |
| C1 channels off by 1 / off by ≥ 2 (of 4,194,304) | 101,443 (2.42 %) / **0** | 101,443 (2.42 %) / **0** |
| C1 D3D11 vs D3D12 projection | bit-identical | bit-identical |
| C3 mip max error, levels 1–10 (512² … 1²) vs box filter of the GPU level above | **1** on every level | **1** on every level |
| C2 debug layer active (InfoQueue present on the device) | yes | yes |
| C2 debug-layer hazard messages, clean run | **0** (0 warnings or errors in total) | **0**. The 9 warnings, all ID 971 (`ID3D12PipelineLibrary::Load*Pipeline: This library does not have a pipeline stored with name …`), are Qt's pipeline-cache misses, not hazards. |
| C2 disjointness assertion: SRBs checked (pipelines + dispatches) / violations | 141 / **0** | 141 / **0** |
| C2 negative control (`--inject-hazard`): debug-layer messages / assertion violations / projection error | 2 hazard warnings / 65 / 149 | 128 messages / 65 / 184 |
| C4 recreations / renderers created / renderers whose view matched (C1 and C3 ≤ 1) | **10 / 11 / 11** | **10 / 11 / 11** |
| C4 final read-back frame / max error / device-lost events / exit code | 1203 / **1** / 0 / 0 | 1202 / **1** / 0 / 0 |
| C4 run wall time (1200 frames, debug layer on) | 5.9 s | 6.2 s |

Debug-layer output of the negative control, as logged:

- **D3D11:**
  - `warning 2097354: ID3D11DeviceContext::CSSetUnorderedAccessViews: Resource being set to CS UnorderedAccessView slot 1 is still bound on input!`
  - `warning 2097316: ID3D11DeviceContext::CSSetUnorderedAccessViews: Forcing CS shader resource slot 5 to NULL.`
- **D3D12:**
  - `error 538: … SetComputeRootDescriptorTable: Resource state (0x8: D3D12_RESOURCE_STATE_UNORDERED_ACCESS) … is invalid for use as a NON_PIXEL_SHADER_RESOURCE`
  - `warning 1008: ID3D12CommandList::ResourceBarrier: Called on the same subresource(s) … in separate Barrier Descs`

In the clean runs neither message appears. That shows the debug layer, the log capture and the script's filter all work, so the zero in the clean runs is meaningful.

**Where the ±1 comes from.** Every one of the 101,443 differing channels has the GPU *lower*, spread evenly over all tiles and over R, G and B (alpha never differs). Two observations narrow it down:

- In mip level 1, all 196,740 exact ties (4-texel sums ≡ 2 mod 4) were stored rounded **down** by the GPU, while the integer reference `(sum + 2) / 4` rounds them up.
- An experiment that moved the CPU rounding threshold explained about half of the projection mismatches, but never all of them. The minimum was about 50 k mismatches of both signs at a threshold of +0.03 LSB.

So the GPU path comes out a few hundredths of an LSB lower than the `f32` CPU path: partly UNORM rounding, partly arithmetic. It never reaches a whole step (no channel is off by 2). The experiment was reverted, and the reference keeps plain round-to-nearest.

### Composite timing (20 layers, 8192×8192, one isolated group, curves)

GPU time per frame from `QRhiCommandBuffer::lastCompletedGpuTime()`. It covers all passes of the frame: group pass, parent pass, 13 mip passes over the dirty region, and display. 60 output tiles (the 1278×768 item at zoom 1) are recomposited every frame. n ≈ 600 frames per row.

| Backend, scene | n | p50 | p95 | p99 | max | Unit |
|---|---|---|---|---|---|---|
| D3D11, 60 tiles/frame, with group | 598 | 1.573 | 1.756 | 1.945 | 11.786 | ms GPU |
| D3D11, 60 tiles/frame, `--no-group` | 599 | 1.376 | 1.535 | 1.665 | 11.538 | ms GPU |
| D3D11, idle (display only) | 599 | 0.018 | 0.020 | 0.036 | 10.889 | ms GPU |
| D3D12, 60 tiles/frame, with group | 598 | 1.492 | 1.650 | 1.745 | 1.800 | ms GPU |
| D3D12, 60 tiles/frame, `--no-group` | 598 | 1.342 | 1.488 | 1.513 | 1.657 | ms GPU |
| D3D12, idle (display only) | 598 | 0.020 | 0.022 | 0.023 | 0.975 | ms GPU |

- The composite costs about **26 µs per 128×128 output tile with 20 layers on D3D11**, and **25 µs on D3D12**: (p50 − idle) / 60.
- Recompositing a whole 8K document (4,096 tiles) would therefore take about **106 ms on D3D11** and **100 ms on D3D12** of GPU time, extrapolated. This happens once, spread over frames by the 64-tile batch cap.
- The isolated-group pass costs 0.20 ms (D3D11) and 0.15 ms (D3D12), about 10–13 % of the frame. This is an approximation of what removing the intermediate would save: `--no-group` flattens the group, it is not an in-place read-write composite.
- The single max outliers of about 11 ms on D3D11 are the first sample of each run (first frame: uploads and first use of the pipelines).

Raw data: (raw output kept privately).

### Task 4 switches, smoke run (informational; the real measurement is Task 4)

`--layers 20 --doc 8192x8192 --backend d3d11 --auto-pan --frames 1500 --frametimes … --trim-flag …`, with the flag touched twice, no ComfyUI and no debug layer:

| Metric | n | p50 | p95 | p99 | max | Unit |
|---|---|---|---|---|---|---|
| frame-to-frame time | 1499 | 4.16 | 5.56 | 10.26 | 26.83 | ms |
| GUI heartbeat stalls (> 4 ms late) | 99 | 6.03 | 8.40 | 20.59 | 20.59 | ms |

The display refreshes at about 240 Hz. On each trim, the cache went from 8192 slices (512 MiB) to 1216 slices (76 MiB), 1153 visible slices copied on the GPU, in 1 frame. As panning continued it grew back to 204–332 MiB.

Raw data: (raw output kept privately).

Raw correctness data: (raw output kept privately). The previews `spikes/canvas/results/golden-{cpu-preview,d3d11-gpu-L2,d3d12-gpu-L2}.png` are committed. These are 256×256 and contain level 2 of the projection.

## 6. Verdict

**PASS on D3D11 (criteria 1–4); D3D12 also passes criteria 1–4 (criterion 5, recorded separately).**

1. The GPU composite of the golden scene matches the Rust CPU reference to within 1/255 on every channel, on both backends. The scene has 6 layers, 4 blend modes, an isolated group composited in a separate pass through an RGBA32F intermediate, curves through a lookup texture, partial opacity and sparse tiles found through the slot table.
2. With the debug layer on, no hazard message appeared, and the disjointness assertion found no violation in 141 checks. The negative control proves both detectors fire when a dispatch does read and write the same texture.
3. Every separately stored mip level is within 1/255 of a box-filter downsample of the level above.
4. Ten renderer recreations each rebuilt the whole view from a fresh GPU cache, and each matched the golden. The final read-back after frame 1200 matched, with no device loss and exit code 0.

Both backends used the same DXBC SM 5.0 shaders.

## 7. Decision

- **IMG-152 (D3D11 part):** the store-only compute compositing design works on Qt 6.12 D3D11 as specified. That design is sampled-only layer tiles through a slot table, multi-pass isolated groups via an intermediate, separate mip textures, and no sRGB flag (IMG-030, IMG-031, IMG-032, IMG-035). A Rust CPU compositor with the same formulas matches it to within 1/255 (IMG-036).
- **IMG-033 (D3D12):** not decided here. Task 4 makes the decision. Input from this spike:
  - D3D12 passes criteria 1–4 with the same SM 5.0 DXBC shaders; no DXC was needed, because the SM 6.6 path of IMG-033 was not built.
  - D3D12 composite time is about 5 % lower than D3D11.
  - Dropping the group intermediate saves about 10–13 % of the 20-layer frame by the flattening approximation, well under the 25 % in Task 4's rule. A true in-place variant was not built.

## 8. Follow-ups

1. **IMG-036 golden tests need a ±1 tolerance.** Bit-exact CPU/GPU parity is not available on this GPU: about 2.4 % of channels differ by 1, and exact ties round down on the GPU but up in the integer box filter. The spec's golden-test wording should say "within 1/255", which is what this spike measured.
2. **One dispatch per output tile** costs about 26 µs per 20-layer tile, so a full 8K recomposite is about 100 ms. M1 should try one dispatch per batch, with `gl_WorkGroupID.z` = tile, to cut dispatch overhead. That would change the plan's wording "one compute dispatch per tile".
3. **Nested groups:** the GPU path here supports one nesting level and up to 4 groups, while the CPU reference supports any depth. M1 needs a recursive pass schedule for IMG-031's "non-trivial nesting".
4. **Memory at 8K, for Task 4's < 1 GB trim check.**
   - The projection plus 13 separate mip textures is 341 MiB.
   - The group intermediate is 16 MiB.
   - The tile cache is capped at 4 × 2048 slices (512 MiB). After a trim it shrinks to the visible slices (76 MiB here) and regrows as the view pans.
   - The spike has no projection paging (one projection texture per document), so "trim to visible pages" applies to the tile cache only.
5. **D3D12 debug layer noise:** ID 971 (pipeline-library name not found) comes from Qt's pipeline cache on the first run. M1 tooling should filter it.
6. **GUI-heartbeat stalls were seen even idle** (p99 20.6 ms in a 6 s run at 240 Hz). Task 4 should check whether these are scene-graph sync blocks before it attributes stalls to ComfyUI load.
7. The **about 145 s first-launch stall** reported in Task 2 did not occur: every fresh-build launch here reached its first frame in under 1 s and finished its run in about 1.3–1.5 s.
