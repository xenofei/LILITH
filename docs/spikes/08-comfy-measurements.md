# 08 ComfyUI model measurements: VRAM fit, LoRA on int8, SAM 3.1 latency, preview decoders

Date: 2026-10-12

## 1. Question

IMG-155, IMG-157, IMG-158, IMG-160; they decide IMG-111, IMG-107, the IMG-158 status-bar copy and IMG-160.

- Does Z-Image Turbo bf16 + the lite Fun ControlNet fit on the 16 GB card next to the canvas and WebEngine, or must the default be int8?
- Do LoRAs apply correctly and quickly on the int8 (convrot) weights?
- How long does SAM 3.1 "select object" (text prompt) take through ComfyUI with Z-Image resident?
- Which preview decoder does ComfyUI use for Z-Image, and is TAEF1 picked automatically?

## 2. Setup

| Item | Version |
|---|---|
| OS | Windows 11 Pro 10.0.26200 |
| GPU / driver | NVIDIA GeForce RTX 5080 16 GB (16,303 MiB), driver 610.88; system RAM well above the model set (> 64 GB) |
| ComfyUI | 0.39.2 at `%USERPROFILE%\ComfyUI\ComfyUI`, started by the spike on `127.0.0.1:8188` only and stopped again. Default flags, so DynamicVRAM (comfy-aimdo 0.5.5, "NVML pressure") and the RAM-pressure node cache are on; comfy-kitchen 0.2.37 |
| ComfyUI frontend / templates | `comfyui-frontend-package` 1.53.10 / `comfyui-workflow-templates` 0.11.78 |
| Python / PyTorch | 3.12.11 / **2.11.0+cu128** for every measurement first, then **2.11.0+cu130** (step 7, kept) |
| Qt / Qt WebEngine (canvas, WebEngine stand-ins) | 6.12.0 `msvc2022_64` / 6.140.0 |
| Models | `z_image_turbo_bf16` (12.3 GB), `z_image_turbo_int8_convrot` (6.2 GB), `qwen_3_4b` TE, `ae` VAE, `Z-Image-Turbo-Fun-Controlnet-Union-2.1-lite-2602-8steps` (2.0 GB, `model_patches`), `sam3.1_multiplex_fp16` (1.75 GB), `taef1_decoder.pth` (4.9 MB, `vae_approx`), LoRA `pixel_art_style_z_image_turbo` (170 MB). Sources and licences in the table below; sizes and verified hashes (raw output kept privately) |
| Desktop load | A normal interactive desktop session: before ComfyUI loaded anything, about 2.4 GB of VRAM was in use by other processes (the desktop compositor, the shell and ordinary desktop applications) |
| Spike code | `spikes/comfy-measure/`, plus `--gpu-cache-mb` in `spikes/canvas/`, in the commit that adds this document (parent c4786ca), with the review fixes in the following commit |

Files downloaded for this spike (anonymous HTTPS, pinned to the listed revision, SHA-256 checked against the hub; no code from any download was run):

| File | Source | Revision | Licence |
|---|---|---|---|
| `z_image_turbo_int8_convrot.safetensors` | Hugging Face `Comfy-Org/z_image_turbo` | `6fc90a3b1b65` | Apache-2.0 |
| `Z-Image-Turbo-Fun-Controlnet-Union-2.1-lite-2602-8steps.safetensors` | Hugging Face `alibaba-pai/Z-Image-Turbo-Fun-Controlnet-Union-2.1` | `5155fc56d178` | Apache-2.0 |
| `sam3.1_multiplex_fp16.safetensors` | Hugging Face `Comfy-Org/sam3.1` | `7bb8374780a7` | SAM License |
| `taef1_decoder.pth` | GitHub `madebyollin/taesd` | `899155b16ee7` | MIT |
| `pixel_art_style_z_image_turbo.safetensors` | Hugging Face `tarn59/pixel_art_style_lora_z_image_turbo` | `0a5092d16196` | Apache-2.0 |

## 3. Method

**Driver: `spikes/comfy-measure/measure.py`** (standard library only). Per run it:
- uploads `{{IMAGE}}`/`{{MASK}}` inputs to ComfyUI's temp folder under a run-unique name (outside the timed window), fills `{{MODEL}}`, `{{LORA}}`, `{{PROMPT}}`, sets each KSampler seed to `--seed + run index`, turns `SaveImage` into `PreviewImage`;
- opens the websocket, submits one prompt with `extra_data.preview_method` (default `taesd`) and waits for `execution_success`;
- samples every 100 ms: `nvidia-smi memory.used` (total dedicated) and, through `pdh.dll` (ctypes; `typeperf` cannot sample faster than 1 s), the PDH counters `\GPU Process Memory(pid_<pid>_<luid>*)\Dedicated Usage`, plus `Local Usage`, `Shared Usage` and `Total Committed`, for ComfyUI and each `--watch-proc` process, on the NVIDIA adapter's LUID only (the machine also has an AMD iGPU);
- records `execution_start` → last `executed` (wall), per-node times, preview frames (count, size, bytes), the ComfyUI log lines of the run (`/internal/logs/raw`) and those matching the OOM/offload pattern;
- writes `results/<workflow>-<model>-[lora|nolora]-<tag>-<stamp>-r<n>.json`.

`--free-first` posts `/free {unload_models, free_memory}` before run 1, so run 1 loads the models into VRAM (from the Windows file cache).

**Workflows** (`spikes/comfy-measure/workflows/`). The CN and SAM templates were converted with the frontend's own `app.graphToPrompt()` (what Export (API) writes), as in Task 4; the raw exports are in `workflows/exports/`. `build_workflows.py` applies the listed edits:
- `zimage_inpaint_cn_lite.json`: template `image_z_image_turbo_fun_union_controlnet`, Canny branch removed, lite 2602 8-step patch, `ZImageFunControlnet` (strength 1) with `inpaint_image` + `mask` (new `LoadImageMask`, red channel), latent size from the input, `{{MODEL}}`. Input `inputs/harbor_1024.jpg` (Task 4's Z-Image output), mask `inputs/harbor_mask_1024.png` (280×300 box over the pink house).
- `zimage_t2i_lora.json`: Task 4's `zimage_t2i.json` + `LoraLoaderModelOnly` (`{{LORA}}`, strength 1.0) before `ModelSamplingAuraFlow`; the LoRA's trigger "Pixel art style. " prepended to the prompt (used with and without the LoRA). Without `--lora`, measure.py splices the LoRA node out.
- `sam31_select.json`: template `utility_image_segment_sam3` (SAM 3.1 Multiplex, `SAM3_Detect`, threshold 0.5, 2 refine iterations), `{{IMAGE}}` and `{{PROMPT}}` ("house").
- `zimage_t2i.json` is Task 4's file, unchanged (comfy_load.py and Task 6 compare against it); its `UNETLoader` is the model slot.
- Validation: each graph was posted once (raw output kept privately): no `node_errors`, every run completed.

**Companions (agreed during M0 review): `spikes/comfy-measure/companions.ps1 -Start/-Stop`.**
- `canvas.exe --doc 8192x8192 --layers 20 --auto-pan --on-top --gpu-cache-mb 1536`. The canvas had no budget switch and its real tile cache is capped at 512 MiB by its four bound texture arrays, so `--gpu-cache-mb N` now also holds (N − 512) MiB of unbound RGBA8 texture arrays, each slice written once, for the renderer's lifetime (`cache_reserve` log event). The switch defaults to 512 (= the cap, no reserve), so Task 3's and Task 4's runners, which don't pass it, keep their old behaviour; `companions.ps1` passes 1536 explicitly (as amended during M0 review).
- `webengine.exe --origin http://127.0.0.1:8188` (Task 6's app with no `--phase`: it just shows the ComfyUI frontend).
- Both are started fresh, with ComfyUI's models unloaded, 30 s before each fit series, and stopped after it. measure.py samples them throughout (`--watch-proc canvas.exe,webengine.exe,qtwebengineprocess.exe`).

**Series run (all commands from the repo root):**
```
python spikes/comfy-measure/measure.py --workflow zimage_t2i --runs 1                       # negative check, ComfyUI stopped
pwsh spikes/comfy-measure/companions.ps1 -Start -Tag cu128-bf16
python spikes/comfy-measure/measure.py --workflow zimage_inpaint_cn_lite --model bf16 --runs 5 --tag fit-cu128 \
  --image spikes/comfy-measure/inputs/harbor_1024.jpg --mask spikes/comfy-measure/inputs/harbor_mask_1024.png \
  --free-first --watch-proc canvas.exe,webengine.exe,qtwebengineprocess.exe --save-outputs
pwsh spikes/comfy-measure/companions.ps1 -Stop          # then the same with --model int8 (fresh companions)
#   repeated as --tag fit-repeat-cu128 (cu128) and --tag fit-cu130 (after step 7); without companions as --tag nocomp-<torch>
python spikes/comfy-measure/measure.py --workflow zimage_t2i_lora --model {bf16|int8} [--lora pixel_art_style_z_image_turbo.safetensors] \
  --runs 3 --seed 1000 --tag {cu128|cu130} --free-first --save-outputs
python spikes/comfy-measure/measure.py --workflow zimage_t2i --model bf16 --runs 1 --tag sam-prep-cu128 --free-first   # Z-Image resident
python spikes/comfy-measure/measure.py --workflow sam31_select --runs 11 --tag cu128 --image spikes/comfy-measure/inputs/harbor_1024.jpg --text house
#   + 5 x (zimage_t2i, then sam31_select --runs 1), --tag interleave-<torch>
python spikes/comfy-measure/measure.py --workflow zimage_t2i --model bf16 --preview {taesd|auto|none} --save-previews --tag preview-<method>-cu128
#   + one taesd run with taef1_decoder.pth hidden (cu128 by hand, cu130 with the helper below), and int8 warm runs on cu130
python spikes/comfy-measure/preview_nodecoder.py --workflow zimage_t2i --model int8 --runs 1 --preview taesd --save-previews --tag preview-taesd-nodecoder-cu130
#   (renames models/vae_approx/taef1_decoder.* to *.ww-hidden, runs measure.py, renames back in a finally block)
python spikes/comfy-measure/measure.py --workflow birefnet_rmbg --runs 2 --tag cu130 --image spikes/comfy-measure/inputs/harbor_1024.jpg --save-outputs
<ComfyUI venv python> spikes/comfy-measure/summary.py --fit --tag fit-cu128 | --lora --sam --previews --tag cu128 | --torch
```
All tables below are `summary.py` output (raw output kept privately).

## 4. Pass criteria

Copied from the plan (Task 8, step 2) before any measurement. Not edited after the run.

- **"Fits" (IMG-155/111):** across 5 inpaint runs with the canvas and WebEngine running, a configuration fits when all of these hold:
  - peak total dedicated VRAM ≤ 15,500 MiB;
  - no log line matches the OOM/offload pattern;
  - run time ≤ 3 × the median of runs 2–5 (so no system-memory fallback, per ARCH-072).

  **Decision:** the Z-Image default is **bf16 + lite ControlNet** if bf16 fits. Otherwise it is **int8 + lite ControlNet**, provided int8 fits. If neither fits, record FAIL and propose nvfp4 as the follow-up.
- **LoRA on int8 (IMG-157):** with the same seed and prompt, compare bf16+LoRA against int8+LoRA.
  - The LoRA must visibly change the output against the no-LoRA run, measured as SSIM(no-LoRA, LoRA) < 0.95 on both precisions.
  - The two precisions must agree with each other: SSIM(bf16+LoRA, int8+LoRA) ≥ 0.80.
  - The LoRA's time overhead must be ≤ 20 % over the same precision without it.

  If all three hold, the int8 profile's LoRA flag is `supported`; otherwise `unsupported`.
- **SAM 3.1 (IMG-158):** 1 cold and 10 warm "select object" runs (text prompt) on a 1024² image, with Z-Image resident. Record p50 and max for cold and warm. Target: warm p50 ≤ 1,500 ms (informational; it sets the status-bar copy, not a pass/fail).
- **Previews (IMG-160):** with `preview_method: "taesd"`, record whether preview frames arrive, their size (≤ 512 px, IMG-045), and from ComfyUI's log or the latent-format code which decoder was used for Z-Image (expected: TAEF1, which is unverified so far).
- **Negative check:** with ComfyUI stopped, `measure.py` prints `ComfyUI unreachable at 127.0.0.1:8188` and exits with code 2.

**How the criteria are applied (fixed before the run).**
- "Peak total dedicated VRAM" is the maximum `nvidia-smi memory.used` sample (100 ms) between `execution_start` and the end of the prompt, for each of the 5 runs.
- "OOM/offload pattern" is `out of memory|OOM|lowvram|offload|partially loaded`, matched against every ComfyUI log line written during the run. Two line formats match it only because they describe configuration, not an event, and they are printed on every text-encoder or VAE load whatever the VRAM state: `... load device: cuda:0, offload device: cpu, ...` (comfy/sd.py) and `Using async weight offloading with N streams` (startup). They are listed in the raw data but not counted. Every other match counts, including ComfyUI's `loaded partially; ... offloaded ...` and `Unloaded partially ... lowvram patches` lines.
- "Run time" is `execution_start` → last `executed` message; the 3× check applies to all 5 runs, including run 1. Before run 1 of each configuration ComfyUI's models are unloaded (`/free`), so run 1 includes loading the models into VRAM.
- Each inpaint run uploads the same 1024² image and mask under a new name and uses the next seed, so every run re-executes the image load, the ControlNet's VAE encode and the sampler, as a fresh request from the canvas would.
- LoRA runs: 3 runs per configuration (bf16/int8 × LoRA/no LoRA), seeds 1000, 1001, 1002 in every configuration; SSIM is computed per seed on the luminance of the 1024² outputs (Gaussian window 11, σ 1.5) and the criterion uses the minimum (for "agree") or maximum (for "visibly changes") over the 3 seeds. Time overhead uses the median wall time of runs 2–3 (run 1 loads the models).

## 5. Measurements

### 5.1 Negative check

ComfyUI stopped: `measure.py --workflow zimage_t2i --runs 1` printed `ComfyUI unreachable at 127.0.0.1:8188` and exited **2** in 2.42 s (raw output kept privately; repeated after the final stop, same result). **PASS.**

### 5.2 Canvas and WebEngine VRAM (the IMG-111 overhead, measured)

PDH on the NVIDIA adapter, 30 s after start, ComfyUI idle (raw output kept privately; 6 starts, all within ±5 MiB for the canvas):

| Process | Dedicated Usage | Local Usage (resident) | Total Committed |
|---|---|---|---|
| canvas.exe (8192², 20 layers, 1536 MiB cache budget) | 1,968 MiB | 1,968 MiB | 1,976–1,980 MiB |
| webengine.exe (shows ComfyUI) | 816–1,006 MiB (see note) | 212–218 MiB | 228–237 MiB |
| QtWebEngineProcess.exe | 0 | 0 | 0 |
| Everything else on the desktop (compositor, shell, other applications) | | ≈ 2,430 MiB | |

- Total dedicated with both stand-ins running and ComfyUI idle: **4,961 MiB** (nvidia-smi). The canvas + WebEngine alone are **≈ 2.2 GB untrimmed** (spec: ~1.5 GB including the compositor); with this desktop's other GPU users the non-ComfyUI load is ≈ 4.6 GB. *Untrimmed* means `companions.ps1` started the canvas with `--gpu-cache-mb 1536` (the 512 MiB tile cache plus a 1,024 MiB synthetic reserve) and no `--trim-flag`. IMG-040 forbids that state during jobs. With IMG-040's trim and cap, spike 04 measured the canvas process at ≈ 0.43–0.76 GB (04 §5). The fit was not measured in that state.
- **Under load the OS evicts the canvas.** In every fit series, during run 1 (model load) the canvas's resident VRAM fell from 1,968 to **416 MiB**, and stayed at 688–944 MiB afterwards, while its committed size stayed ~1,980 MiB. WDDM demotes the canvas's allocations to make room for ComfyUI; nothing stops it. (The 1,024 MiB reserve is never touched after creation, so it goes first; the 512 MiB of real tile arrays were also partly evicted.)
- Note: `Dedicated Usage` for webengine.exe is not a residency figure. It grows by ~3 GB per ComfyUI job (to 23 GB after 5 int8 runs, more than the card) while Local Usage stays ~115 MiB and Total Committed ~240 MiB; dwm.exe reads 22 GB the same way. The tables therefore use Local Usage / Total Committed for the stand-ins.

### 5.3 Fit (IMG-155): 5 inpaint runs with canvas + WebEngine

Primary series (step 4, cu128):

| Config | Wall ms runs 1–5 | Median 2–5 | Max ratio | Peak total MiB runs 1–5 | OOM/offload event lines | ComfyUI resident peak | ComfyUI shared (RAM) | Fits |
|---|---|---|---|---|---|---|---|---|
| bf16 + lite CN | 14,090 / 10,662 / 10,457 / 10,295 / 10,263 | 10,376 | 1.36 | 15,416 / **15,494** / 15,368 / 15,344 / 14,633 | 0 | 11,354–12,422 MiB | **6,048 MiB** | **YES** |
| int8 + lite CN | 17,360 / 14,709 / 14,695 / 14,693 / 14,698 | 14,697 | 1.18 | 15,378 / 14,036 / 14,018 / 13,983 / 14,036 | 0 | 10,234–11,562 MiB | 280 MiB | **YES** |

Same procedure, repeated (fresh stand-ins each time):

| Series | bf16 peaks | bf16 fits | int8 peaks | int8 fits | Medians 2–5 bf16 / int8 |
|---|---|---|---|---|---|
| cu128, repeat | run 1 **15,517**, others ≤ 15,390 | NO (peak) | run 1 **15,558**, others ≤ 13,965 | NO (peak) | 10,262 / 14,773 ms |
| cu130 (after step 7) | **15,578 / 15,567** / 15,497 / 15,322 / **15,544** | NO (peak) | 15,490 / 14,239 / 14,331 / 14,269 / 14,267 | **YES** | 9,663 / 5,882 ms |

No run in any series had an OOM/offload event line or exceeded 1.44 × its median. Without the stand-ins (`nocomp`, informational): bf16 peaks ≤ 15,470 MiB but run times 9.5–21.4 s on cu128; int8 13.4 s (cu128) and 5.3 s (cu130), flat.

What the numbers show beyond the rule:
- **The peak criterion is set by ComfyUI, not by the model.** DynamicVRAM fills the card to its own NVML-pressure headroom, so run 1 (text encoder, DiT and ControlNet loading together) peaks at 15.4–15.6 GB for *both* precisions; the run-1 peak falls in the first second of the KSampler in every series. Whether it lands under 15,500 is a ±60 MiB coin toss.
- **The log check cannot see offload under DynamicVRAM.** ComfyUI 0.39.2 logs `Model … prepared for dynamic VRAM loading. N MB Staged` whatever happens next; it never printed a `loaded partially` or `offload` event. Every OOM/offload pattern match in all runs was one of the two configuration lines excluded in §4.
- **bf16 does not fit resident; int8 does.** bf16 DiT (11.7 GB) + lite CN (1.9 GB) need ≈ 13.8 GB, but ComfyUI held at most 12,422 MiB (cu128) and 13,240 MiB (cu130) of VRAM with the stand-ins running, and kept **5.8–6.0 GB in pinned system memory mapped to the GPU** (PDH Shared Usage) in every bf16 run; int8's stayed at 280 MiB. bf16 streams weights from RAM every step. It shows as run-to-run variance: an 8-step bf16 t2i took 6.8–28.6 s (LoRA series, §5.4), where int8 varies by < 1 %.

### 5.4 LoRA on int8 (IMG-157)

LoRA `tarn59/pixel_art_style_lora_z_image_turbo` (strength 1.0), seeds 1000–1002, same prompt with and without the LoRA:

| Seeds 1000 / 1001 / 1002 | SSIM(no-LoRA, LoRA) bf16 | SSIM(no-LoRA, LoRA) int8 | SSIM(bf16+LoRA, int8+LoRA) | SSIM(bf16, int8) without LoRA |
|---|---|---|---|---|
| cu128 | 0.572 / 0.586 / 0.583 | 0.564 / 0.585 / 0.571 | 0.879 / 0.891 / **0.866** | 0.906 / 0.925 / 0.922 |
| cu130 | 0.573 / 0.590 / 0.583 | 0.562 / 0.575 / 0.568 | 0.878 / 0.882 / 0.895 | 0.919 / 0.897 / 0.918 |

| Torch | int8 LoRA, median runs 2–3 | int8 no LoRA | int8 overhead | bf16 LoRA | bf16 no LoRA | bf16 overhead |
|---|---|---|---|---|---|---|
| cu128 | 11,677 ms | 11,679 ms | **−0.0 %** | 10,285 ms | 23,379 ms | −56 % (noise: bf16 runs 7.5–27.9 s) |
| cu130 | 3,862 ms | 3,685 ms | **+4.8 %** | 7,618 ms | 17,702 ms | −57 % (noise: 6.8–28.6 s) |

All three criteria hold on both torch builds: max SSIM(no-LoRA, LoRA) 0.590 < 0.95; min SSIM(bf16+LoRA, int8+LoRA) 0.866 ≥ 0.80; overhead ≤ 20 % (int8 0–4.8 %; bf16 negative). Sample sheet: `results/samples/lora-seeds1000-1002-cu128.jpg`.

### 5.5 SAM 3.1 select object (IMG-158)

Text prompt "house" on the 1024² harbor image (`results/samples/sam-and-inpaint-cu128.jpg`):

| Series | cu128 p50 / max ms | cu130 p50 / max ms | SAM3_Detect p50 cu128 / cu130 | Z-Image resident when submitted |
|---|---|---|---|---|
| cold (run 1 of 11, after a Z-Image run) | 1,317 / 1,317 | 2,197 / 2,197 | 616 / 1,591 ms | yes |
| warm (runs 2–11) | **529 / 540** | **511 / 516** | 446 / 431 ms | **no** (see below) |
| interleaved: Z-Image t2i, then SAM, × 5 | **1,281 / 1,329** | **1,221 / 1,255** | 628 / 630 ms | yes |

- ComfyUI dropped Z-Image (from VRAM and from its node cache) as soon as the first SAM prompt ran: VRAM in use before the warm runs was 3.8–4.2 GB. So "warm with Z-Image resident" does not happen with ComfyUI's default cache. In the realistic generate → select → generate loop every SAM call reloads SAM (~1.25 s in total) and the next Z-Image run reloads Z-Image: +2.3–2.6 s (bf16, cu128: 9.8–9.9 vs 7.3 s) and +2.2–3.5 s (int8, cu130: 5.9–7.2 vs 3.7 s).
- Target warm p50 ≤ 1,500 ms: met in every variant.

### 5.6 Previews (IMG-160)

| preview_method | Frames per 8-step run | Frame size | Decoder | KSampler ms (int8, cu130, warm, runs 2–4 / 1–4) |
|---|---|---|---|---|
| `taesd` | 8 | 512 × 512 JPEG (≤ 512 ✓) | **TAEF1** (`models/vae_approx/taef1_decoder.pth`) | 3,328–3,351 (+0.46 s, +16 %) |
| `auto` | 8 | 512 × 512 JPEG | Latent2RGB | 2,875–2,887 (±0) |
| `none` | 0 | — | — | 2,860–2,896 |
| `taesd` with the taef1 file hidden (cu128 bf16 by hand; cu130 int8 via `preview_nodecoder.py`) | 8 | 512 × 512 JPEG | Latent2RGB fallback | log, both runs: `Warning: TAESD previews enabled, but could not find models/vae_approx/taef1_decoder` |

- Which decoder: Z-Image is `supported_models.ZImage` → `Lumina2` → `latent_format = latent_formats.Flux`, whose `taesd_decoder_name` is `"taef1_decoder"`; `latent_preview.get_previewer` takes the first `vae_approx` file starting with that name. The run with the file removed confirms it from ComfyUI's own log. Visual comparison: `results/samples/previews-taesd-vs-auto.jpg`.
- Is TAEF1 picked automatically? **Only with `preview_method: "taesd"`** (the file is then chosen from the latent format). `auto` maps to Latent2RGB in `get_previewer` and never uses TAEF1.
- Every frame is 512 px because the server scales each preview to `--preview-size` (512) with `ImageOps.contain`, including Latent2RGB's 128² image, so frame size does not tell the decoders apart.
- Not measured: `preview_method: "latent2rgb"` (by the code it is the same previewer that `auto` selects) and `"default"` (it resets to ComfyUI's command-line `--preview-method`, which is `none` here unless set).
- The Hugging Face repo `madebyollin/taef1` holds only diffusers-format weights, which ComfyUI's previewer cannot load; the file that works is `taef1_decoder.pth` from `github.com/madebyollin/taesd` (same author, MIT).

### 5.7 cu128 vs cu130 (step 7)

| Series | cu128 median 2–5 | cu130 median 2–5 | Speed-up |
|---|---|---|---|
| int8 inpaint + lite CN, with canvas + WebEngine | 14,697 ms | 5,882 ms | **2.50×** |
| int8 inpaint + lite CN, alone | 13,444 ms | 5,274 ms | **2.55×** |
| bf16 inpaint + lite CN, with canvas + WebEngine | 10,376 ms | 9,663 ms | 1.07× |
| bf16 inpaint + lite CN, alone | 11,698 ms | 9,867 ms | 1.19× |
| int8 t2i 1024², 8 steps (LoRA series without LoRA, median runs 2–3) | 11,679 ms | 3,685 ms | 3.17× |

On cu128 ComfyUI logs "You need pytorch with cu130 or higher to use optimized CUDA operations" and comfy-kitchen's `cuda` backend is `disabled: True`; on cu130 it is enabled (startup logs; raw output kept privately).

**Upgrade record (approved step 7; cu130 kept).**
- Before: `torch==2.11.0+cu128`, `torchvision==0.26.0+cu128`, no torchaudio; installed by uv (no `direct_url.json`), index `https://download.pytorch.org/whl/cu128` (from the `+cu128` local version). cu128 wheel sha256 (cp312, win_amd64, from the index): torch `7c78215c3af4f62e63f2b2e360f1722fc719b0853c7ac22666483d9810613a4c`, torchvision `8c0d1c4fbb2c9a4d5d41d0aaa87da20e525bcb2a154ce405725b0be59456804b`.
- A `pip freeze` of ComfyUI's venv was taken before the upgrade as the rollback reference, and another after it (raw output kept privately); the two differ only in the `torch` and `torchvision` lines.
- Install command, following ComfyUI's README cu130 line, with the installed versions pinned (torchaudio left out: it was not installed and is not in ComfyUI's requirements):
  ```
  %USERPROFILE%\ComfyUI\.venv\Scripts\python.exe -m pip install "torch==2.11.0+cu130" "torchvision==0.26.0+cu130" --extra-index-url https://download.pytorch.org/whl/cu130
  ```
  cu130 wheel sha256 (pip checks the index's hash fragment): torch `ef8beae16d781c3244ef28dc7bee6d8871c26bbde65d5bf66e902cb61972c4ab` (1,915 MB), torchvision `a3578f7c8e8a2724306c68c56873a1675fa7ce45471e18235c720a2ed242fe44`.
- Rollback command (not needed, not run):
  ```
  %USERPROFILE%\ComfyUI\.venv\Scripts\python.exe -m pip install "torch==2.11.0+cu128" "torchvision==0.26.0+cu128" --extra-index-url https://download.pytorch.org/whl/cu128
  ```
  then compare `pip freeze` with the pre-upgrade freeze.
- Verified on cu130: ComfyUI starts; `/system_stats` reports `pytorch_version: 2.11.0+cu130`; no custom-node import failure in the startup log, `comfyui_birefnet_ll` imported in 2.1 s and its 5 nodes are in `/object_info`; Z-Image t2i completes (bf16 11.8 s, int8 4.5 s, first runs). **BiRefNet runs on cu130:** `workflows/birefnet_rmbg.json` (`LoadRembgByBiRefNetModel` with `General.safetensors`, device AUTO, fp32 → `RembgByBiRefNet`) on `inputs/harbor_1024.jpg` completed twice (2,459 ms cold, 586 ms warm, no errors) and produced a 1024² RGBA cut-out and a 1024² mask covering the woman (51 % of the pixels > 0.5; raw output kept privately; sheet `results/samples/birefnet-cu130.jpg`). No rollback is needed.

Raw data: one JSON per run (raw output kept privately). A first bf16 fit series, run before Local/Committed sampling was added, gave the same picture (peaks ≤ 15,423 MiB, 10.3–14.0 s). The workflow validation runs were kept as well.

## 6. Verdict

**PARTIAL.**
- Negative check: PASS.
- Fit: the pre-registered rule gives different answers on an identical procedure, so it cannot decide IMG-111 on its own (see §7). Primary cu128 series: both fit (bf16 by 6 MiB). cu128 repeat: neither fits (run-1 peaks 15,517 and 15,558 MiB). cu130: bf16 fails (3 of 5 runs over 15,500), int8 fits (by 10 MiB). The peak criterion does not discriminate under DynamicVRAM (§5.3), and the log criterion is blind to its weight streaming; only the time criterion worked as intended, and it never tripped. The measurements outside the rule are unambiguous: bf16 + lite CN does not fit resident next to the canvas and WebEngine (6 GB of weights in pinned RAM in every run, 2–4× run-time swings); int8 + lite CN does (280 MiB shared, < 1 % variance).
- LoRA on int8: PASS on cu128 and cu130.
- SAM 3.1: warm p50 511–529 ms, but with Z-Image *not* resident, because ComfyUI unloads it; with Z-Image resident (interleaved) p50 1,221–1,281 ms. Both under the 1,500 ms target.
- Previews: frames arrive, 512 px; the decoder is TAEF1 with `taesd`; it is not picked by `auto`.

## 7. Decision

- **IMG-111 (Z-Image default): `int8_convrot` + lite ControlNet (2602 8-step), on PyTorch cu130 — by the IMG-111 default, because bf16 was not shown to fit.** IMG-111 makes int8 the default "unless the M0 VRAM measurement (IMG-155) shows bf16 fits". This is a **documented override of the one verbatim bf16 "fits" result**, confirmed during M0 review (2026-10-12: int8 is the spec default and bf16 was not shown to fit reliably). The evidence:
  - The verbatim rule's outcome per series (identical procedure each time): first cu128 series, bf16 YES (peak 15,494 MiB, 6 MiB under the limit) and int8 YES; cu128 repeat, neither (run-1 peaks 15,517 and 15,558 MiB); cu130, int8 only (bf16 15,578 MiB; int8 15,490 MiB, 10 MiB under). int8's own pass on cu130 is no stronger than bf16's on cu128, so neither "fits" result is the basis of the decision; the IMG-111 default is.
  - **The peak and log criteria don't discriminate under ComfyUI's DynamicVRAM** (on by default in 0.39.2), a finding for the spec: ComfyUI fills the card to its own headroom, so run-1 peaks of both precisions land within about ±60 MiB of 15,500, and it never logs offload, so the log grep cannot see weight streaming (§5.3, follow-up 1).
  - Arithmetic: bf16 DiT 11,738 MiB + lite CN 1,923 MiB + VAE 159 MiB (ComfyUI's "Staged" sizes) = 13,820 MiB of weights alone; plus the measured canvas + WebEngine, untrimmed (1,968 + ~230 MiB) = ~16,020 MiB. With IMG-040's trim and cap the canvas is ≈ 0.43–0.76 GB instead; the fit was not measured in that state. That is over the 15,500 MiB budget and leaves ~280 MiB of the 16,303 MiB card for the CUDA context, the 1024² activations, the compositor and every other app (≈ 2.4 GB on this desktop). bf16 cannot be resident next to the untrimmed canvas.
  - What ComfyUI did instead: bf16 kept **5.8–6.0 GB of weights in pinned system memory** mapped to the GPU (PDH Shared Usage) in every run with the stand-ins, against 280 MiB for int8, and bf16 run times swung 6.8–28.6 s for the same 8-step job where int8 varied by < 1 %.
  - int8 is also faster on the shipped torch build: 5.9 vs 9.7 s per inpaint on cu130.
- **IMG-107/157 (LoRA flag): Z-Image `int8_convrot` profile `supported`** (bf16 `supported` too). w4a8 was not measured: there is no Z-Image w4a8 file.
- **IMG-158 (status-bar copy):** select object takes ≈ 0.5 s when SAM is already loaded, ≈ 1.2–1.3 s right after a generation (SAM reloads), ≈ 1.3–2.2 s on first use. Suggested copy: "Selecting '<prompt>'…" without a time estimate (or "~1 s"). The next generation after a selection takes ~2–3 s longer (Z-Image reload); show it as "Loading model…".
- **IMG-160 (preview decoder):** Z-Image uses **TAEF1** (`taef1_decoder.pth` in `models/vae_approx`), and only when the suite sends `preview_method: "taesd"`; `auto` gives Latent2RGB. IMG-045's prompts must send `"taesd"`, and the download set must name `taef1_decoder.pth` from `madebyollin/taesd`, not the `madebyollin/taef1` Hugging Face repo.

## 8. Follow-ups

1. **IMG-155/ARCH-072 need a DynamicVRAM-aware fit signal.** Replace "peak total ≤ 15,500" and the log grep with "ComfyUI's PDH Shared Usage stays at its idle level (no weights staged in RAM)" plus the time check; or run fit measurements with `--disable-dynamic-vram` and keep the log grep. Decide which ComfyUI memory mode the suite ships with.
2. **The IMG-111 overhead, measured untrimmed, is ≈ 2.2 GB for the canvas + WebEngine** (canvas 1.97 GB including the 1,024 MiB synthetic reserve, WebEngine 0.23 GB), against the ~1.5 GB assumed, before the compositor and the user's other apps (≈ 2.4 GB on this desktop). With IMG-040's trim and cap the canvas is ≈ 0.43–0.76 GB (04 §5); the fit was not measured in that state. The measured figure is recorded in the IMG-111 M0 note; the rule's budget text is unchanged.
3. **WDDM evicts the canvas's VRAM under ComfyUI load** (1,968 → 416 MiB resident). Plans 5/6 must expect evicted tile textures (re-paging stalls on first touch after a job) and may want ComfyUI's `--reserve-vram` / `--vram-headroom` set to the canvas budget; re-measure APP-135 stalls with the cache evicted.
4. **ComfyUI drops Z-Image after any other prompt** (default RAM-pressure cache): select object and other utility graphs cost a ~2–3.5 s Z-Image reload on the next generation. Try `--cache-lru N` or the `--cache-ram` thresholds, or accept it and show it in the UI.
5. **Ship PyTorch cu130 with ComfyUI** (int8 2.5× faster; ComfyUI warns without it). Add a torch build check to setup.
6. TAEF1 previews cost ~0.46 s per 8-step 1024² run (+16 %), because ComfyUI decodes the full 1024² image and then shrinks it to 512 px. Acceptable; a cheaper decode would be an upstream change.
7. `canvas.exe --gpu-cache-mb` defaults to 512 (the bound arrays' cap, no reserve), so Task 3's and Task 4's runners reproduce their old behaviour; only Task 8's `companions.ps1` passes 1536. Checked after the change: a default start logs `cache_reserve` with `gpu_cache_mib 512, reserve_mib 0`; `--gpu-cache-mb 1536` logs `reserve_mib 1024`.
8. PDH `Dedicated Usage` is unusable for WebEngine and dwm (it grows past the card's size); per-process budgets in the suite should read Local Usage / Total Committed.
