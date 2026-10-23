# Local image models for a 16 GB canvas studio (October 2026)

**Target machine:** a 16 GB GPU that supports NVFP4, large system RAM (well over 64 GB), Windows 11, ComfyUI 0.39.2 (released 2026-10-08, the newest stable release).
**Already installed:** Z-Image-Turbo bf16 (12.3 GB file, about 12.6 GB VRAM in use), BiRefNet, Real-ESRGAN.
**Research date:** 2026-10-12.
**Sizes:** taken from Hugging Face file listings, in decimal GB.
**Tags:**
- `[verified]` means the claim was checked against a primary source (model card, Comfy-Org repo, ComfyUI docs or changelog).
- `[secondary]` means it comes only from a third-party article or forum.
- `[unverified]` means it is my inference or something I could not confirm.

---

## 0. TL;DR: what changed since the brief was written

1. **Qwen-Image-2.1 (released 2026-09-20) is the most important new model for this machine.**
   - It is a single 7B DiT that does text-to-image, instruction editing, up to 10 reference images, mask or annotation-guided local edits, and native RGBA output with transparency. [verified: model card]
   - ComfyUI has supported it natively since day 0 (v0.37.0). [verified: changelog]
   - Comfy-Org ships int8 "convrot" files: the DiT is 7.26 GB and the Qwen3-VL-8B text encoder is 9.35 GB (int8) or 6.31 GB (w4a8). It fits on 16 GB without offload. [verified: HF listing; secondary: a 4060 Ti 16 GB user reports about 15 GB peak, about 20 s per 1024² at 25 steps, and about 60 s per edit]
   - A Turbo checkpoint (8 steps, CFG 1) followed on 2026-10-09. There is also a Turbo LoRA (0.91 GB) and an alibaba-pai 4-step Acc LoRA. [verified]
   - A Fun ControlNet Union (8 control types plus inpainting) was merged into core around 2026-09-28. [verified: changelog v0.38.0 and the alibaba-pai card]
   - **License:** Qwen Research License, which is non-commercial. That is fine for this user.
   - In practice it **replaces Qwen-Image-2512 and Qwen-Image-Edit-2511 on a 16 GB card**. Both of those are 20B and their fp8 files are 20.5 GB.
2. **Z-Image-Edit has not been released.** Tongyi-MAI on HF has only `Z-Image` (base, 2026-01-28) and `Z-Image-Turbo`. [verified: HF org listing]
   - Z-Image inpainting therefore goes through the **Z-Image-Turbo-Fun-ControlNet-Union 2.1** inpaint mode, which has native ComfyUI node support, or through Differential Diffusion.
3. **New native ComfyUI features in 2026 that cover several operations:**
   - **SAM 3.1:** text-prompt segmentation.
   - **Depth Anything 3** (v0.25.0).
   - **SeedVR2** image and video upscaling (v0.28.0).
   - **Marigold V2** (v0.36.0). It is built on Qwen-Image-Edit-2509 int8 (20.5 GB), which makes it too heavy.
   - **MoGe 3.**
   - **Qwen-Image-2.1 tiny VAE** for previews (v0.38.0).
   - **Dynamic VRAM** ("aimdo"), the default on NVIDIA since about March 2026.
4. **Big new releases that are API-only, so ignore them for local use:** FLUX 3 Image (v0.38.2 partner node), HY Image 3.5 Preview, Qwen Image 3.0, Ideogram 4.5 and MAI Image. [secondary/verified via changelog wording]

---

## 1. Text-to-image

| Model | Params / files | ComfyUI | Fits 16 GB? | Speed (4090 class / test machine) | Notes |
|---|---|---|---|---|---|
| **Z-Image-Turbo** (installed) | 6B. bf16 12.3 GB, **int8_convrot 6.2 GB**, **nvfp4 4.5 GB**; TE qwen_3_4b 8.0 GB (fp8_mixed 5.6 / fp4_mixed 3.5). Also a 0.16 GB distill-patch LoRA | Native | Yes. bf16 fits, and int8 or nvfp4 frees about 6–8 GB for a ControlNet | about 2.3 s at 1024², 8 steps on a 4090 [secondary, one repeated claim]. No figure for the test machine's GPU found | Photoreal and strong for its size. Apache-2.0. NVFP4 is the fast path on GPUs that support it; its quality loss is [unverified] |
| **Qwen-Image-2.1 / 2.1-Turbo** | 7B DiT. bf16 14.2 GB, **int8 7.26 GB**; GGUF Q4_K_M 4.2 GB, Q8_0 7.6 GB (unsloth). TE Qwen3-VL-8B: bf16 17.5 / int8 9.35 / w4a8 6.31 GB. VAE 0.68 GB (RGBA, 64 ch) | Native (v0.37.0) | Yes (int8 DiT and int8 or w4a8 TE) | 4060 Ti 16 GB: about 20 s at 1024² and 25 steps [secondary]. Turbo uses 8 steps, so expect about 3× faster [unverified]. Native output is 2048² | Best typography and editing in this tier. RGBA output. Qwen Research License (non-commercial). **int8 needs PyTorch cu130.** On cu128 one user measured int8 at 4× slower than the Q8 GGUF; it was fixed after updating (GitHub #16470) [verified issue] |
| **FLUX.2 [klein] 4B** (distilled and base) | 4B. bf16 7.75 GB, **fp8 4.07 GB**; TE qwen_3_4b; VAE flux2-vae 0.34 GB | Native | Easily | About 1.2 s (distilled, 4 steps) and about 17 s (base, 50 steps) on a 5090, with 8.4 / 9.2 GB VRAM [verified: Comfy blog] | Apache-2.0. Generation and editing in one model. Very fast, so good for live previews |
| **FLUX.2 [klein] 9B** (plus a 9B-KV variant) | 9B. bf16 18.2 GB, **fp8 9.43 GB**, kv-fp8 9.82 GB; TE qwen_3_8b_fp8mixed | Native | fp8 yes. bf16 needs about 19.6 GB [verified] | About 2 s distilled on a 5090 [verified] | FLUX non-commercial license. Better than 4B. The KV variant is faster for multi-reference work [secondary: BFL docs] |
| **HiDream-O1-Image / -Dev / -Dev-2604** | 8B pixel-space "unified transformer" with no VAE. bf16 16.4 GB, **fp8_scaled 8.07 GB**, mxfp8 8.9 GB | Native (Comfy-Org repackage plus templates) | fp8 yes | Not reported. Full model uses 50 steps, Dev uses 28 | MIT license. Generation, editing and subject personalization up to 2048². Dev-2604 placed #8 in the AA arena per its card. ComfyUI VRAM and speed are [unverified] |
| **Krea 2 RAW / Turbo** | 12.9B. bf16 26.3 GB, fp8 13.1 GB, int8 13.5 GB, **turbo nvfp4 7.67 GB**; TE qwen3vl_4b fp8 5.2 GB; Qwen-Image VAE | Native (v0.26.0) | nvfp4 comfortably. fp8 is tight and will partly offload | Turbo uses 8 steps. No timing found | Strong aesthetics. Has style LoRAs, a style-reference LoRA, a depth ControlNet and a regional node pack. License sources conflict [secondary] |
| Qwen-Image-2512 | 20B. fp8 20.4 GB; GGUF Q4_K_M about 13 GB | Native | Only with offload or GGUF | Lightning LoRA available (lightx2v) | Superseded on 16 GB by Qwen-Image-2.1 |
| FLUX.2 [dev] | 32B. fp8mixed 35.5 GB plus Mistral-3-Small TE (18 GB fp8 / 12.3 GB fp4) | Native | No (heavy offload, slow) | Turbo LoRA exists (2.76 GB) | Skip on 16 GB |
| Others noted | Boogu-Image (v0.26), PID 1.5 PixelDiT (v0.28), Ming Image 0.1 Design (6B, RGBA, v0.38), SenseNova U1.5 (v0.35), JoyImageEdit (v0.29), LongCat-Image (6B) | Native per changelog | Not evaluated | – | [unverified] quality and fit |

**Recommendation:**
- Keep Z-Image-Turbo as the fast photoreal default.
- Add Qwen-Image-2.1 (int8, plus the Turbo LoRA) as the "smart" model for text, layout, editing and alpha.
- Add FLUX.2 klein 4B fp8 as the sub-second preview and editing model.
- HiDream-O1 Dev fp8 is the best optional fourth model.

---

## 2. Inpainting and outpainting

**Z-Image-Turbo has no dedicated inpaint checkpoint.** There is no Z-Image-Edit or Z-Image inpaint variant on HF. [verified] There are three options:

1. **Z-Image-Turbo-Fun-ControlNet-Union-2.1 inpaint mode.** This is the recommended route.
   - **Files** [verified: HF listing]:
     - Full: 6.71 GB (`...-Union-2.1-2602-8steps`).
     - Lite: 2.02 GB (`...-lite-2602-8steps`; control on 3 blocks instead of 15).
   - **ComfyUI** [verified]:
     - Native `ZImageFunControlnet` node with `image` / `inpaint_image` / `mask` inputs. All three connected means inpainting.
     - Inpaint support has been in core since v0.5.0 (Dec 2025).
     - The node inverts the mask internally for Z-Image patches.
   - **Model card advice** [verified]:
     - Use a larger `control_context_scale` for continuity. The good range is 0.65–1.0.
     - The 2601 release added more diverse masks and multi-resolution training.
     - Update 2602 added Gray control.
   - **Reliability:** earlier versions had mask bleed and bright artifacts, fixed in 2.1 updates. [secondary]
2. **Differential Diffusion** (native `DifferentialDiffusion` node) with `SetLatentNoiseMask` or `InpaintModelConditioning`, a soft mask, and denoise around 0.6–0.8. [secondary: tutorial]
   - It works with any model but can lose context at high denoise.
   - Good for touch-ups. It is weaker for adding large new objects.
3. **LanPaint.** A training-free inpaint sampler custom node that works across models; it appears in Krea 2 inpaint workflows. [secondary]

**Better inpaint-capable models on 16 GB:**
- **Qwen-Image-2.1 Fun ControlNet Union.**
  - Files: bf16 7.55 GB, **int8 3.78 GB**. [verified]
  - Its single control branch covers Canny, Depth, Grayscale, HED, Lineart, MLSD, Pose and Scribble **plus masked inpainting**.
  - Control and inpaint can be combined. The mask is white where content is regenerated. CFG is 1.0.
  - Native since v0.38.0. The `ZImageFunControlnet` doc says the node also takes Qwen 2.1 Fun patches without inverting the mask; comfyui-wiki instead names the "Apply Qwen Image DiffSynth ControlNet" node. Test both. [verified docs + secondary]
- **Qwen-Image-2.1 instruction editing with a mask or annotation.** The model card says local edits can be specified "via circles, painted annotations, or separate masks". It is the best "change this region" UX. The exact ComfyUI wiring for a separate mask is [unverified].
- **FLUX.2 klein** does object replace and remove by instruction. ComfyUI docs don't mention mask inpainting. [verified absence]

**Outpainting:** use native `ImagePadForOutpaint` together with any of the inpaint routes above. Qwen-Image-2.1 or FLUX.2 klein with an "extend the scene" instruction is an alternative. [unverified quality]

---

## 3. Instruction editing and multi-reference

| Model | Size / 16 GB files | ComfyUI | Refs | Fits 16 GB | Notes |
|---|---|---|---|---|---|
| **Qwen-Image-2.1 (+Turbo)** | 7B. int8 7.26 GB + TE int8 9.35 / w4a8 6.31 | Native | **Up to 10** | **Yes** | About 60 s per edit at 25 steps on a 4060 Ti [secondary]. Turbo uses 8 steps, and the 4-step Acc LoRA (PDD) also covers editing. Built-in subject extraction and background removal (template `image_qwen_image_2_1_background_removal`). Identity preservation |
| **FLUX.2 klein 4B / 9B** | fp8 4.07 / 9.43 GB | Native | Multi-ref (templates use 2 images; the exact maximum is [unverified]) | Yes | Fastest editor. 4B is Apache-2.0, 9B is non-commercial |
| HiDream-O1-Image (full) | fp8 8.07 GB | Native | Subject-driven personalization | Yes | The card recommends the **full** model for editing. ComfyUI edit quality is [unverified] |
| Qwen-Image-Edit-2511 | 20B. fp8mixed 20.5 GB, int8 20.5 GB; GGUF Q4_K_M 13.2 GB, Q3_K_M 9.9 GB; community Nunchaku int4 (wazimondo, QuantFunc) | Native (v0.5.0); GGUF via ComfyUI-GGUF; Nunchaku via ComfyUI-nunchaku | 1–3 [unverified] | Only GGUF Q4 or with Dynamic-VRAM streaming (slow) | Lightning 4/8-step LoRAs (0.85 GB bf16) [verified]. Nunchaku paths may not apply LoRAs [secondary]. Now superseded by 2.1 for 16 GB |
| LongCat-Image-Edit / -Turbo | 6B. bf16 12.5 GB | Native (Comfy-Org repackage) | 1 | Yes with bf16 (tight) | Turbo uses 8 steps. A vLLM recipe says ≥40 GB, but that is a serving setup, not ComfyUI [secondary] |
| OmniGen2 | about 7.9 GB fp16 DiT + 7.5 GB Qwen2.5-VL TE | Native | Multi | Yes, with TE offload | Older (2025). Quality is behind the models above [unverified] |
| Step1X-Edit v1.2 | Large (FLUX-based, about 12B+) [unverified] | Custom node (ComfyUI_Step1X-Edit) | 1 | Needs quantization | Not recommended; no 2026 activity |

**Pick:** Qwen-Image-2.1 is the primary editor, FLUX.2 klein 4B the fast editor, and HiDream-O1 optional.

---

## 4. Control (ControlNet / union) and preprocessors

| Base | Control model | Size | Controls | ComfyUI |
|---|---|---|---|---|
| Z-Image-Turbo | **Z-Image-Turbo-Fun-ControlNet-Union-2.1** (2602, 8-step) | 6.71 GB full / 2.02 GB lite | Canny, HED, Depth, Pose, MLSD, Scribble (2601+), Gray (2602), inpaint | Native: `Load Model Patch` → `ZImageFunControlnet`, files in `models/model_patches` [verified] |
| Z-Image-Turbo | **Z-Image-Turbo-Fun-ControlNet-Tile-2.1** | 6.71 GB / lite 2.02 GB | Tile / super-resolution | Same node [verified] |
| Qwen-Image-2.1 | **Qwen-Image-2.1-Fun-ControlNet-Union** | 7.55 GB bf16 / **3.78 GB int8** | Canny, Depth, Gray, HED, Lineart, MLSD, Pose (DWPose), Scribble, inpaint | Native since v0.38.0 [verified] |
| Qwen-Image (2512/Edit) | DiffSynth and InstantX ControlNets, Fun Union 2512 | various | – | Native [verified: Comfy-Org repos] |
| FLUX.2 klein | No union ControlNet found [unverified]. Use reference images or edit instructions | – | – | – |
| Krea 2 | Krea 2 Depth ControlNet [secondary] | – | Depth | Native per comfyui-wiki |

Use control_context_scale (strength) 0.65–1.0 for Z-Image [verified card] and 1.0 for Qwen 2.1 [verified card].

**Preprocessors:**
- **Depth:**
  - **Depth Anything 3** is native (v0.25.0). Files: small 0.14 GB, base 0.54 GB, mono-large / metric-large 1.34 GB. [verified]
  - Depth Anything V2 is available via `comfyui_controlnet_aux` (v1.1.5, Apr 2026) or `kijai/ComfyUI-DepthAnythingV2`.
  - **Lotus:**
    - Comfy-Org repackage: `lotus-depth-d-v1-1.safetensors`, 1.74 GB.
    - The built-in `LotusConditioning` node exists. Some sources say the actual depth workflow uses kijai's ComfyUI-Lotus [secondary; whether it is fully native is unverified].
  - **Marigold V2** is native (v0.36.0) but runs on a Qwen-Image-Edit-2509 int8 base (20.5 GB). Too heavy as a preprocessor here.
  - **MoGe 3** (v0.37.0) for geometry.
- **Pose:** DWPose (`DWPreprocessor`) in `comfyui_controlnet_aux`. The models download on first use, about 0.35 GB [unverified]. The Qwen 2.1 union was trained on DWPose skeletons. [verified card]
- **Edges / lines:** the native `Canny` node; HED, Lineart, MLSD, Scribble (PiDi) and others from `comfyui_controlnet_aux`.

---

## 5. Reference and style transfer

- **No IP-Adapter exists for Z-Image or Qwen-Image.** [secondary: none found] SD1.5/SDXL IP-Adapters don't transfer.
- **Redux is a FLUX.1-only adapter.** [unverified for FLUX.2; FLUX.2 uses native reference latents instead]
- What works on modern DiTs is **reference-image conditioning inside the unified models**:
  - **Qwen-Image-2.1:** up to 10 references, encoded by the Qwen3-VL TE and inserted as VAE latents via the `Text Encode Qwen Image 2.1` node. Best for "use the style or character of image 1". [verified: Comfy blog]
  - **FLUX.2 klein:** multi-reference editing and composition (style transfer is a listed use). [verified]
  - **HiDream-O1:** subject-driven personalization (identity and IP). [verified card]
  - **Krea 2:** `krea2_style_reference.safetensors` LoRA (0.46 GB) in Comfy-Org/Krea-2. Changelog v0.29.0 also mentions "Krea 2 regular and timestep-zero reference images". [verified listing]
  - **Qwen-Image-2.1 community edit LoRAs** (Consistency, Outfit-Swap, Multiple-Angles, Next-Scene). [verified on HF, quality unverified]
- **For Z-Image style:** train a LoRA (ai-toolkit supports Z-Image Turbo), or use low-denoise img2img plus the Gray or Tile ControlNet.

---

## 6. Regional prompting

- **Native mask conditioning:**
  - `ConditioningSetMask` / `ConditioningSetArea` + `ConditioningCombine` are core nodes. ComfyUI applies them in the sampler by computing a prediction per conditioning and blending them by mask, so they work mechanically with any model, Z-Image and Qwen included. [unverified]
  - Cost: roughly one extra forward pass per region.
  - With distilled CFG-1 models (Z-Image-Turbo, Qwen 2.1 Turbo), how well regions stay separated is **untested** [unverified].
- **Attention-couple / attention-mask regional methods:**
  - **No ComfyUI node was found that explicitly supports Z-Image or Qwen-Image.** [secondary search]
  - Existing ones: `ComfyUI-Krea2-Regional` (single-pass joint attention mask plus per-region LoRA), `ComfyUI-FluxRegionAttention` (FLUX.1) and an Anima regional node.
  - The A1111/Forge Regional Prompter (2026-09-04 update) lists Z-Image latent mode as supported and attention mode as partial. That is WebUI, not ComfyUI. [secondary]
  - These nodes conflict with `--use-flash-attention` / sage attention patches. [secondary]
- **Practical alternatives:**
  1. Qwen-Image-2.1 mask- or circle-guided local edits.
  2. Generate the base, then inpaint each region.
  3. Krea 2 plus Krea2-Regional if true single-pass regional generation is needed.

---

## 7. Segmentation and "select object"

| Tool | Size | ComfyUI | Notes |
|---|---|---|---|
| **SAM 3.1 (Multiplex)** | `sam3.1_multiplex_fp16.safetensors` 1.75 GB → `models/checkpoints` | **Native** (PR #13408; templates `utility_image_segment_sam3` / video) [verified] | Text prompts up to 32 tokens. Comma-separated subjects, with `:N` to cap detections (e.g. `eye:2`). Returns masks plus an RGBA preview. A v0.35.0 fix addressed empty text-prompt masks. SAM license |
| SAM 3.1 / SAM 3 (custom) | same weights | `ComfyUI-RMBG` v3.2.0 (2026-09-30) adds a SAM 3.1 Multiplex node, point/dot prompts and an `unload_model` toggle. `PozzettiAndrea/ComfyUI-SAM3` (text, multiprompt, point/box collectors) | Use when point and box clicking is needed. [secondary] |
| SAM 2.1 | about 0.9 GB for hiera-large [unverified] | `kijai/ComfyUI-segment-anything-2`, or SAM2 in ComfyUI-RMBG | Point and box click selection. SAM 3.1 supersedes it for text |
| **BiRefNet** (installed) | – | ComfyUI-RMBG or BiRefNet nodes | Best for whole-subject matting and background removal. Pair with SAM 3.1 for "select object by name" |
| Qwen-Image-2.1 background removal | (uses the main model) | Native template | Produces RGBA directly; slower than BiRefNet but semantic |

---

## 8. Layer decomposition

- **Qwen-Image-Layered**, Dec 2025, 20B on the Qwen-Image base:
  - **Native in ComfyUI since v0.6.0**: templates plus the `qwen_image_layered_vae`. [verified]
  - **Files** [verified]:
    - bf16 40.9 GB.
    - **fp8mixed 20.5 GB**.
    - GGUF (unsloth) Q4_K_M 13.2 GB, Q3_K_M 9.9 GB, Q5_K_M 15.0 GB.
    - The VAE is 0.25 GB.
    - The TE is qwen_2.5_vl_7b_fp8_scaled, 9.4 GB.
  - **VRAM / speed** [secondary: comfyui-wiki]:
    - Peaks around 45 GB on an RTX Pro 6000.
    - About 120 s at 1024px.
    - A 4090 is "almost fully utilized".
    - 640px is recommended. Use 50 steps at CFG 4.
  - **On 16 GB:**
    - The GGUF Q4_K_M or fp8 with Dynamic-VRAM streaming should run but slowly. Expect several minutes per decomposition [unverified estimate].
    - Variable number of layers; recursive splitting is possible.
- **Lighter partial alternatives:**
  - Qwen-Image-2.1 native RGBA ("extract subjects from photographs", "edit transparent layers").
  - SAM 3.1 masks + BiRefNet + inpaint-the-hole gives a manual 2–3 layer split.
- Put Qwen-Image-Layered in **M2**, not M1.

---

## 9. Upscaling and detail

- **SeedVR2**, native since v0.28.0 [verified]. Comfy-Org/SeedVR2 (Apache-2.0) files:
  - 3B: fp16 6.78, **fp8 3.39**, int8 3.46, mxfp8 3.56, **nvfp4 2.0 GB**.
  - 7B: fp16 16.5, **fp8 8.24**, int8 8.33, nvfp4 4.76 GB.
  - "7B sharp" in the same precisions.
  - VAE `ema_vae_fp16` 0.50 GB.
  - Templates: `utility_seedvr2_7b_int8_upscale_image`, `..._3b_int8_upscale_image`.
  - One-step diffusion restoration and upscale. Speed and VRAM at 4K are [unverified]; tile or batch settings live in the template.
- **Tile-based diffusion upscale:**
  - Z-Image-Turbo + **Fun-ControlNet-Tile-2.1** (lite 2.02 GB), with `Ultimate SD Upscale` (custom node) or a native tiled KSampler approach.
  - Or Qwen-Image-2.1 + the Restore LoRA (community).
  - Real-ESRGAN (installed) handles the pre-upscale pass.
- **Suggested chain:** Real-ESRGAN ×2 → SeedVR2 3B/7B for restoration, or Z-Image Tile for creative detail.

---

## 10. Fast previews

- Set `--preview-method taesd` (default is `none`; `auto` and `latent2rgb` also exist) and `--preview-size` (default 512). [verified: startup flags]
  - Tiny decoders go in `models/vae_approx`.
- **Z-Image:** uses the FLUX.1 16-channel VAE (`ae.safetensors`), so **TAEF1** (`madebyollin/taef1`, 9.8 MB) applies [unverified mapping].
- **Qwen-Image-2.1:**
  - **Native tiny VAE added in v0.38.0** [verified].
  - `madebyollin/taeqi2_1` (30.6 MB, RGBA, 64 ch, MIT).
- **FLUX.2:** `madebyollin/taef2` (10.7 MB), or `fal/FLUX.2-Tiny-AutoEncoder`. Whether ComfyUI's preview picks taef2 automatically is [unverified].
- **Qwen-Image 2512 / Edit / Layered** (Wan-2.1-style VAE): TAESD-family support is [unverified].
- **Few-step models and LoRAs** [verified unless noted]:
  - Z-Image-Turbo: 8 steps.
  - FLUX.2 klein distilled: 4 steps.
  - **Qwen-Image-2.1-Turbo**: 8 steps, CFG 1. Full model, or the Turbo LoRA (0.91 GB) on the base.
  - **Qwen-Image-2.1-Fun-Acc 4-step LoRA**: PDD, covers T2I and edit.
  - Viggle Qwen-2.1 turbo.
  - Qwen-Image-2512-Lightning and Qwen-Image-Edit-2511-Lightning (4/8 steps).
  - Krea 2 Turbo (8 steps) and its Turbo LoRA.
  - FLUX.2 dev Turbo LoRA.
- **Interactive-canvas strategy:**
  - FLUX.2 klein 4B fp8 (about 1 s) or Z-Image-Turbo nvfp4 at 512–768px for live drafts.
  - Re-render the final with Qwen-Image-2.1 or Z-Image bf16.

---

## 11. Model swapping on 16 GB VRAM with large system RAM

**How ComfyUI 0.39 behaves:**
- **Dynamic VRAM** is the default on NVIDIA Windows/Linux. [verified: Comfy blog + startup flags]
  - Loading a model maps the safetensors file into file-backed memory and reserves GPU *virtual* address space. Physical VRAM is committed only as layers fault in.
  - The most recently loaded model has priority and evicts older weights when space runs out.
  - Models are **no longer copied VRAM→RAM on unload**. VRAM is freed and the model returns to an "uncommitted" state that persists across runs.
  - Weights stay cached in RAM while memory allows, never go to the pagefile, and may be re-read from disk if the OS reclaims them.
  - If a weight doesn't fit, it is streamed per layer into a temporary GPU buffer, so it runs slower instead of failing with OOM.
- **Execution cache:** the default mode is `--cache-ram` (RAM-pressure caching). [verified]
  - Default active threshold: 10% of RAM, clamped to 2–10 GB.
  - Default inactive/pin threshold: 100% of RAM, max 128 GB.
  - So with well over 64 GB of RAM, essentially all models used in a session can stay RAM-resident.
- **Related changes:**
  - v0.30.0 added MRU weight pinning to process RAM.
  - v0.36–0.38 added "fast disk" detection: disk-backed loading on fast NVMe. Override with `--disable-fast-disk` or `--fast-disk`.
  - v0.37.0: "Dynamic VRAM keeps the text encoder on GPU".

**Switching Z-Image ↔ Qwen-Image-2.1 (or Qwen-Edit) in one session:**
- **Expected behaviour:**
  - The first use of each model loads from NVMe.
  - After that, switching is a RAM→VRAM transfer over PCIe 5.0 of roughly 7–13 GB per model (int8 DiT, plus the TE if it was evicted), which is about 1–3 s.
  - There is no re-read from disk while RAM isn't under pressure.
  - These timings are estimates [unverified]; no published measurement for a 16 GB consumer GPU was found.
- **Z-Image bf16 at about 12.6 GB and Qwen-2.1 int8 (7.3 + 9.4 GB TE) cannot co-reside.** Expect eviction on every switch.
  - Using **Z-Image int8 (6.2 GB) or nvfp4 (4.5 GB)** with the **Qwen3-VL-8B w4a8 TE (6.3 GB)** reduces thrash.
  - Batching edits by model also helps.
- **Known issues to watch:**
  - Models always reloading under Dynamic VRAM (GitHub #13139, desktop #1741).
  - Symlinked model folders causing a reload on every prompt (#14276).
  - Fixes reported: `--cache-ram 8 20`-style explicit thresholds, avoiding symlinks, or `--disable-dynamic-vram` as a fallback.
  - Some 5090 users reported Dynamic VRAM being 50–60% slower in March; compare total wall time both ways. [secondary]
- **int8_convrot models need PyTorch cu130.** On cu128 they were 3–4× slower. [verified: issue #16470, kijai]
- **Suggested launch flags to test:**
  - Default (Dynamic VRAM on) plus `--preview-method taesd`.
  - Optionally `--cache-ram 16 100`.
  - Keep `--reserve-vram` around 1 GB for the Windows desktop.
  - Avoid `--highvram` and `--gpu-only`.

---

## 12. Recommended M1 model set (16 GB)

Files are named as in the Comfy-Org repos. Sizes are download sizes.

### Core (covers all 11 operations)

| # | Purpose | File(s) | Download |
|---|---|---|---|
| – | T2I fast (have) | `z_image_turbo_bf16` + `qwen_3_4b` + `ae` | (installed) |
| 1 | Z-Image control + **inpaint** | `Z-Image-Turbo-Fun-Controlnet-Union-2.1-2602-8steps.safetensors` (full; or lite 2.02 GB) | 6.71 GB |
| 2 | Z-Image tile upscale | `Z-Image-Turbo-Fun-Controlnet-Tile-2.1-lite-2601-8steps.safetensors` | 2.02 GB |
| 3 | Smart T2I, **edit**, multi-ref, RGBA, mask edits | `qwen_image_2.1_int8_convrot.safetensors` | 7.26 GB |
| 4 | Qwen 2.1 speed | `qwen_image_2.1_turbo_lora_avg_rank_178_bf16.safetensors` (or the 4-step Fun-Acc LoRA) | 0.91 GB |
| 5 | Qwen 2.1 text encoder | `qwen3vl_8b_int8_convrot.safetensors` (or `qwen3vl_8b_w4a8` 6.31 GB for headroom) | 9.35 GB |
| 6 | Qwen 2.1 VAE | `qwen_image_2.1_vae_bf16.safetensors` | 0.68 GB |
| 7 | Qwen 2.1 control + inpaint | `qwen_image_2.1_fun_controlnet_union_int8_convrot.safetensors` | 3.78 GB |
| 8 | Fast edit / live preview | `flux-2-klein-4b-fp8.safetensors` + `flux2-vae.safetensors` (TE `qwen_3_4b` is probably the same file Z-Image uses; check the hash [unverified]) | 4.07 + 0.34 GB |
| 9 | Select object (text) | `sam3.1_multiplex_fp16.safetensors` | 1.75 GB |
| 10 | Depth | `depth_anything_3_mono_large.safetensors` | 1.34 GB |
| 11 | Pose / edges | `comfyui_controlnet_aux` (DWPose, HED, Lineart and others; auto-download) | about 0.35 GB [unverified] |
| 12 | Upscale / restore | `seedvr2_3b_fp8_e4m3fn` (3.39) + `seedvr2_7b_fp8_e4m3fn` (8.24) + `seedvr2_ema_vae_fp16` (0.50) | 12.13 GB |
| 13 | Previews | `taef1` (0.01), `taeqi2_1` (0.03), `taef2` (0.01) | about 0.05 GB |
| | **Core total (new downloads)** | | **about 50.8 GB** (about 46.1 GB with the lite union ControlNet) |

Custom nodes needed for M1:
- `comfyui_controlnet_aux` (preprocessors).
- Optionally `ComfyUI-RMBG` (BiRefNet, SAM point/box prompts, unload toggle).
- Optionally `ComfyUI-GGUF` (only if GGUF is used).

Everything else is native in 0.39.2.

### Optional / M2

| Purpose | File | Size |
|---|---|---|
| Qwen 2.1 full Turbo (instead of the LoRA) | `qwen_image_2.1_turbo_int8_convrot` | 7.26 GB |
| Second high-quality T2I / editor | HiDream-O1-Image-Dev fp8 (`hidream_o1_image_dev_fp8_scaled`) | 8.07 GB |
| Better klein | `flux-2-klein-9b-fp8` + `qwen_3_8b_fp8mixed` | 9.43 GB + TE (size not checked) |
| Faster Z-Image on a GPU that supports NVFP4 / less swapping | `z_image_turbo_nvfp4` or `int8_convrot` | 4.5 / 6.2 GB |
| Layer decomposition | `qwen-image-layered-Q4_K_M.gguf` + `qwen_image_layered_vae` + `qwen_2.5_vl_7b_fp8_scaled` | 13.2 + 0.25 + 9.4 GB |
| Qwen-Edit-2511 (if 2.1 is lacking for some edits) | `qwen-image-edit-2511-Q4_K_M.gguf` + Lightning 4-step LoRA | 13.2 + 0.85 GB |
| Krea 2 aesthetics + regional | `krea2_turbo_nvfp4` + `qwen3vl_4b_fp8_scaled` + `qwen_image_vae` | 7.67 + 5.24 + 0.25 GB |
| Lotus depth | `lotus-depth-d-v1-1` | 1.74 GB |

---

## 13. Open questions and unverified items

- Measured speeds on the test machine's 16 GB GPU for any of these models. None was found; test locally.
- Whether ComfyUI's TAESD preview automatically picks taef1 for Z-Image and taef2 for FLUX.2.
- How a separate inpaint mask is wired for Qwen-Image-2.1 editing in ComfyUI. The card says masks are supported, but the node wiring wasn't seen because the template JSON fetch failed with a 503.
- Which node applies the Qwen 2.1 Fun ControlNet: `ZImageFunControlnet` (per the docs) or "Apply Qwen Image DiffSynth ControlNet" (per comfyui-wiki).
- How well regional masks work on distilled CFG-1 models.
- Krea 2 license terms (sources conflict).
- Quality loss of NVFP4 and int8 vs bf16. One user saw more anatomy distortion with int8 than with Q8 GGUF on Qwen 2.1.
- Qwen-Image-Edit-2511 maximum number of references.

---

## Sources

**Primary: model cards and repos**
- https://huggingface.co/Qwen/Qwen-Image-2.1 · https://huggingface.co/Qwen/Qwen-Image-2.1-Turbo · https://github.com/QwenLM/Qwen-Image-2.1
- https://huggingface.co/Comfy-Org/Qwen-Image-2.1 (file sizes, folders, templates)
- https://huggingface.co/alibaba-pai/Qwen-Image-2.1-Fun-Controlnet-Union · https://huggingface.co/alibaba-pai/Qwen-Image-2.1-Fun-Acc-LoRAs
- https://huggingface.co/Tongyi-MAI (no Z-Image-Edit) · https://huggingface.co/Comfy-Org/z_image_turbo
- https://huggingface.co/alibaba-pai/Z-Image-Turbo-Fun-Controlnet-Union-2.1
- https://huggingface.co/black-forest-labs/FLUX.2-klein-4B · https://huggingface.co/black-forest-labs/FLUX.2-klein-4b-fp8 · https://huggingface.co/black-forest-labs/FLUX.2-klein-9b-fp8 · https://huggingface.co/black-forest-labs/FLUX.2-klein-9b-kv-fp8
- https://huggingface.co/Comfy-Org/flux2-dev
- https://huggingface.co/HiDream-ai/HiDream-O1-Image · https://huggingface.co/Comfy-Org/HiDream-O1-Image
- https://huggingface.co/Comfy-Org/Krea-2
- https://huggingface.co/Comfy-Org/Qwen-Image_ComfyUI · https://huggingface.co/Comfy-Org/Qwen-Image-Edit_ComfyUI · https://huggingface.co/Comfy-Org/Qwen-Image-Layered_ComfyUI
- https://huggingface.co/unsloth/Qwen-Image-Edit-2511-GGUF · https://huggingface.co/unsloth/Qwen-Image-Layered-GGUF · https://huggingface.co/unsloth/Qwen-Image-2.1-GGUF
- https://huggingface.co/lightx2v/Qwen-Image-Edit-2511-Lightning · https://huggingface.co/lightx2v/Qwen-Image-2512-Lightning
- https://huggingface.co/wazimondo/Qwen_image_edit_2511_int4 · https://huggingface.co/QuantFunc/Nunchaku-Qwen-Image-EDIT-2511 (community Nunchaku)
- https://huggingface.co/Comfy-Org/LongCat-Image · https://huggingface.co/Comfy-Org/Omnigen2_ComfyUI_repackaged · https://huggingface.co/stepfun-ai/Step1X-Edit-v1p2
- https://huggingface.co/Comfy-Org/SeedVR2 · https://huggingface.co/Comfy-Org/sam3.1 · https://huggingface.co/Comfy-Org/Depth-Anything-3 · https://huggingface.co/Comfy-Org/marigold-v2-0 · https://huggingface.co/Comfy-Org/lotus
- https://huggingface.co/madebyollin/taef1 · https://huggingface.co/madebyollin/taef2 · https://huggingface.co/madebyollin/taeqi2_1

**Primary: ComfyUI docs, blog and changelog**
- https://docs.comfy.org/changelog (v0.3.x → v0.39.2)
- https://docs.comfy.org/development/comfyui-server/startup-flags
- https://blog.comfy.org/p/dynamic-vram-in-comfyui-saving-local
- https://blog.comfy.org/p/qwen-image-21-in-comfyui-open-weight
- https://blog.comfy.org/p/flux2-klein-4b-fast-local-image-editing · https://docs.comfy.org/tutorials/flux/flux-2-klein
- https://docs.comfy.org/built-in-nodes/ZImageFunControlnet
- https://docs.comfy.org/tutorials/utility/video-segment-sam3
- https://docs.comfy.org/built-in-nodes/LotusConditioning
- https://github.com/Comfy-Org/ComfyUI/issues/16470 (int8 needs cu130) · https://github.com/Comfy-Org/ComfyUI/issues/13139 · https://github.com/Comfy-Org/ComfyUI/issues/14276 · https://github.com/Comfy-Org/desktop/issues/1741
- https://github.com/Comfy-Org/comfy-aimdo

**Secondary**
- https://huggingface.co/Qwen/Qwen-Image-2.1/discussions/35 (4060 Ti 16 GB test)
- https://comfyui-wiki.com/en/news/2026-09-24-qwen-image-2-1-fun-controlnet-union
- https://comfyui-wiki.com/en/tutorial/advanced/image/qwen/qwen-image-layered
- https://comfyui-wiki.com/en/news/2026-01-28-alibaba-z-image-base-release
- https://comfyui-wiki.com/en/news/2026-07-15-comfyui-v0-28-0
- https://comfyui-wiki.com/en/news/2026-06-22-krea-2-open-source-text-to-image
- https://comfyui-wiki.com/en/news/2026-10-01-comfyui-rmbg-v3-2-0 · https://github.com/1038lab/ComfyUI-RMBG
- https://runaihome.com/blog/z-image-turbo-16gb-vram-home-lab-image-gen-2026/
- https://apatero.com/blog/z-image-turbo-inpainting-comfyui-guide-2025
- https://github.com/januspluto/ComfyUI-Krea2-Regional · https://github.com/hako-mikan/sd-webui-regional-prompter
- https://www.runcomfy.com/comfyui-nodes/ComfyUI-SAM3
- https://gigazine.net/gsc_news/en/20260401-comfyui-dynamic-vram
- FLUX 3 Image / HY Image 3.5 API-only: https://openrouter.ai/black-forest-labs/flux-3-image · https://docs.comfy.org/tutorials/partner-nodes/tencent/hy-image-3-5
