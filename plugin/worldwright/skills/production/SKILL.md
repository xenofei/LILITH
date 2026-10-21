---
name: "production"
description: "Worldwright reference F (Direction and production modules): design doc to asset plan, store and marketing kit and trailers, level and world building, narrative and localization, engine placement and testing, protecting human edits, 3D-to-2D sprite renders, 2D rigs and visual novels, LoRA training, credits files, hardware-aware local runs and hardware stats. Load when planning a project or working on any of these modules."
user-invocable: false
---

# Appendix F — Production modules

Snapshot as of 2026-10-11. Engine versions referenced: Godot 4.7.2, Unity 6.3 LTS (docs for 6.6), Unreal 5.8, Blender 5.2 LTS.

## F1. From design doc to asset plan

This is the Direction module. Turn a design doc (Markdown, DOCX, PDF or exported Google Doc) into a production plan before generating anything in bulk. The same process works for other project types: a shot list or script for an `animation` project, a track and cue list for an `audio` project, a product list for an `asset-pack`.

**Outputs**

1. `plan/assets.csv`, one row per asset, with these columns:

   | Column | What it holds |
   |---|---|
   | `asset_id` | Stable, `<type>_<name>[_<variant>]`, e.g. `chr_knight`, `pal_ember`, `ico_potion_red` |
   | `name`, `type` | |
   | `module` | Module that produces it (IDs in Appendix I4) |
   | `priority` | vertical-slice, mvp or later |
   | `level_or_scene` | |
   | `engine_target` | |
   | Budgets | Triangles, texture size, frame count, duration, bones |
   | `depends_on` | Asset IDs or `lib:` items this asset needs, e.g. palette before tileset before tilemap. They become the manifest's `inputs`. |
   | `placeholder_status` | |
   | `owner` | ai or human |
   | `est_cost_usd`, `est_time` | |

2. `plan/schedule.md`: batches in dependency order, grouped by module where it helps parallel work, with a cost estimate for each batch.
3. `plan/open-questions.md`: decisions only the user can make.

**Process**

- Build greybox placeholders early. Give each its stable ID, final dimensions, pivot and name, so the final asset can replace it with no engine changes.
- Create a `planned` manifest for each row, so the asset graph and the status view cover work that hasn't started.
- When the design doc changes, diff it against the plan and update only the affected rows. Mark dropped rows `deprecated` instead of deleting them, and never delete signed-off assets.
- Share the plan and each batch's cost estimate before bulk runs, and check the estimates against any spending cap the user has set (Appendix I10).

## F2. Store and marketing kit

General rules:

- Build store art from signed-off assets plus a human-made layout.
- Render titles, logos and text with real fonts; never use AI-generated lettering.
- Keep screenshots honest: real gameplay only.

**Steam store assets** (Steamworks docs)

| Asset | Size | Rules |
|---|---|---|
| Header capsule | 920×430 | Required; no text other than the game title |
| Small capsule | 462×174 | Required; the logo should nearly fill it |
| Main capsule | 1232×706 | Required |
| Vertical capsule | 748×896 | Required |
| Screenshots | At least 1920×1080, 16:9 | At least 5; gameplay only; localize with a `_japanese`-style filename suffix |
| Page background | 1438×810 | Optional |
| Bundle header | 707×232 | Bundles only |

**Steam library assets**

| Asset | Size | Rules |
|---|---|---|
| Library capsule | 600×900 | |
| Library header | 920×430 | |
| Library hero | 3840×1240 | No text; keep key content inside the centered 860×380 safe area |
| Library logo | 1280 wide and/or 720 tall | Transparent PNG |

**Steam events and trailers**

- Event cover: 800×450. Event header: 1920×622.
- Trailers:
  - Up to 1920×1080, 30 or 60 fps, at least 5,000 kbps.
  - H.264 plus AAC, with audio at 44.1 or 48 kHz.
  - Must make sense with the sound off.
  - Some viewers watch for under 10 seconds, so lead with gameplay.

**Apple App Store**

- Screenshots, required sizes:
  - iPhone 6.1": 1179×2556 or 1206×2622.
  - iPad 13" (for iPad apps): 2064×2752 or 2048×2732.
- Screenshots, optional sizes: 6.9" (1320×2868) and 6.5" (1284×2778).
- Up to 10 screenshots per size, JPG or PNG, no alpha.
- App previews:
  - 15–30 s, up to 3 per size and language, 30 fps maximum.
  - H.264 at 10–12 Mbps.
  - 886×1920 for iPhone; 1200×1600 for iPad 13".
- App icon: 1024×1024 (verify the current Icon Composer requirements).

**Google Play**

- Icon: 512×512, 32-bit PNG.
- Feature graphic: 1024×500, no alpha.
- Screenshots: 2–8 per device type, sides 320–3840 px. Games should include at least three at 1920×1080.
- Video: a YouTube URL. Only the first 30 seconds autoplay, so show real gameplay in the first 10 seconds and add captions.
- TV banner: 1280×720.

**Other stores**

- Epic, itch.io and GOG asset specs: verify on the platform.
- Console store specs are under NDA, so use the platform holder's kit.

**Trailer pipeline**

1. **Capture** gameplay in the engine at 1920×1080 or higher, 60 fps. Use Godot Movie Maker, Unity Recorder or Unreal Movie Render Queue.
2. **Cut** for each store. Show gameplay within the first seconds: Steam viewers may watch less than 10 s, and Google Play autoplays only the first 30 s. A 30–90 s main trailer is common practice.
3. **Score** with music (Appendix A5). Keep dialogue and SFX levels consistent, and deliver a loudness-normalized mix; check each platform's target.
4. **Captions and titles.** Add captions, and render logo and title cards with real fonts.
5. **Export** to each platform's specs (above).

**Press kit**

- Build the press kit with presskit.html (MIT): fact sheet, description, screenshots, trailer, logo pack and credits.

## F3. Level and world building

**Modular kits**

- Snap pieces to a project grid (e.g. 1 m, 2 m, 4 m), with pivots at the grid corner or center.
- Share materials through trim sheets and decals.
- Name pieces `env_<set>_<piece>_<size>`.
- Check snapping in a test scene.

**Terrain**

| Engine | Heightmap format |
|---|---|
| Unreal | 16-bit grayscale PNG or RAW (RAW needs a JSON sidecar). Sizes follow components × quads + 1: 8129, 4033, 2017 … 127. |
| Unity | 16-bit or 8-bit RAW, with a byte-order option |
| Godot | Terrain3D 1.0.x (MIT) imports EXR or R16 |

- Gaea and World Machine: the free editions are resolution-capped.
- Blender's A.N.T. Landscape add-on.

**Foliage**

- Blender's Sapling Tree Gen (GPL) and Geometry Nodes.
- SpeedTree is a paid subscription.
- Produce LODs and billboard impostors.

**Skyboxes and panoramas**

- Blockade Labs Skybox AI: Model 3 makes 8K equirectangular images; Model 4 makes 4K (paid plans).
- HY-World 2.0's panorama model.
- Convert to HDR when you need image-based lighting.

**World generators**

| Generator | What it exports | Notes |
|---|---|---|
| World Labs Marble | SPZ or PLY splats (~2M or 500k points); a GLB collider mesh (100–200k triangles); a high-quality GLB mesh (~600k textured triangles, up to 1 h to build, 4 requests per hour) | |
| Tencent HY-World 2.0 | Meshes, 3D Gaussian splats, point clouds | Tencent license |
| Google Genie 3 | Nothing; it produces video only | |

**Gaussian splats**

- The Khronos `KHR_gaussian_splatting` glTF extension is ratified. SPZ compression is still only proposed.
- Convert formats with splat-transform (MIT). It can also write a voxel file for collision.
- Engine plugins:
  - Unity: aras-p (MIT).
  - Godot: GDGS 3.1 (MIT). It needs the Forward+ renderer and can generate collision.
  - Unreal: YaGS (Apache-2.0, now archived).

**Collision**

- Build convex decompositions with CoACD (MIT; it needs triangulated input) or V-HACD (BSD-3).
- Export collision meshes using the engine's naming convention.

**Photogrammetry**

- RealityScan.
- Meshroom (MPL-2.0).
- COLMAP 4.2 (BSD).

**Procedural layout**

- Wave Function Collapse with mxgmn WFC (MIT) or DeBroglie (MIT).

**Navigation**

- Recast/Detour (zlib).
- In Godot, only baked navigation meshes count. Match `cell_size` to the navigation map, and set `agent_radius`.

## F4. Narrative and localization

**Dialogue tools**

- Ink 1.2 (MIT), Yarn Spinner 3.2 (MIT), Dialogic 2 alpha for Godot 4.5+ (MIT).

**Engine localization formats**

| Engine | Formats and features |
|---|---|
| Godot | CSV or gettext PO/MO. The editor generates POT files with plurals and context. Automatic BiDi/RTL mirroring. Font fallback. Pseudolocalization options: accents, `double_vowels`, `fake_bidi`, `expansion_ratio` (0.3 covers most cases). |
| Unity | Localization package 1.5: XLIFF 1.2 and 2.0, CSV, Google Sheets; Smart Strings for plurals; built-in pseudo-localization |
| Unreal | PO export and import, String Tables, ICU plurals |

ICU MessageFormat 2 is stable for plurals and gender selection.

**Text expansion** (English to European languages; source: IBM via W3C)

| English length | Expected expansion |
|---|---|
| Up to 10 characters | 200–300% |
| 11–20 | 180–200% |
| 21–30 | 160–180% |
| 31–50 | 140–160% |
| 51–70 | 151–170% |
| Over 70 | 130% |

Design UI for these expansion ranges.

**Translation workflow**

1. **Glossary and style guide.** Build a glossary of character names, items and UI terms, with a do-not-translate list, and a style guide for each language.
2. **Export with context.** Export strings with screenshots, maximum lengths, speakers, and gender and plural variables.
3. **Draft.** Machine-translate drafts through the routing layer (Appendix A6), then have Claude review them against the glossary.
4. **Validate automatically.** Check that:
   - placeholders and ICU syntax are intact;
   - tags are balanced;
   - lengths fit the UI budget;
   - no strings are left untranslated;
   - glossary terms are used.
5. **Pseudo-localize in the engine** to catch overflow.
6. **Linguistic QA.** Have native speakers review before release, and feed their fixes back into the glossary.

**Fonts**

- Noto fonts (OFL) for CJK and other scripts, with HarfBuzz shaping.
- msdf-atlas-gen (MIT) for MSDF atlases, so text stays crisp at any scale.
- BMFont `.fnt` files for pixel games.

**Voice pipeline**

1. Script.
2. TTS (Appendix A5).
3. ASR round-trip check (word error rate ≤ 5%).
4. Montreal Forced Aligner timing.
5. Lip-sync data:
   - Rhubarb for 2D: mouth shapes A–F, plus G, H and X; outputs JSON or TSV.
   - Audio2Face-3D for 3D: the SDK is MIT; the models are under the NVIDIA Open Model License.
6. Engine viseme mapping.
7. Localized voice lines.

**Subtitles** (Game Accessibility Guidelines)

- Subtitle all important speech.
- Use at least 28 px at 1080p as a floor, and let players resize subtitles.
- Label speakers.
- In VR, place subtitles near the speaker.

## F5. Placing and testing assets in the engine

**Godot 4.7**

- `godot --headless --import` imports resources, then quits. Run scripts with `--script`.
- An `EditorImportPlugin` can store a source hash in `generator_parameters`. Bump `_get_format_version` to force reimports.
- `EditorScenePostImportPlugin` adds scene post-processing.
- For QA captures, use Movie Maker: `--write-movie out.png --fixed-fps 30 --resolution 1920x1080`. Whether it works headless: verify.

**Unity 6.3 LTS**

- Batch runs: `-batchmode -nographics -quit -executeMethod Namespace.Class.Method -logFile -`. Exit code 1 means failure.
- Use `AssetPostprocessor` callbacks (`OnPreprocessTexture`, `OnPreprocessModel`, `OnPostprocessAllAssets`) and folder Presets (`Preset.ApplyTo`).
- Save prefabs with `PrefabUtility.SaveAsPrefabAsset`.
- For captures, render a Camera to a RenderTexture.

**Unreal 5.8**

- Python editor scripting needs the Python Editor Script Plugin.
- Run scripts with `UnrealEditor-Cmd <project> -run=pythonscript -script=...`. This mode doesn't load levels; use `-ExecutePythonScript` in the full editor when you need them.
- Import with `AssetImportTask`, setting `automated`, `destination_path`, `filename`, `options`, `replace_existing` and `save`.
- Interchange imports glTF and MaterialX. FBX through Interchange is experimental.
- Capture with Movie Render Queue.

**Through MCP servers**

- Official Unity MCP (beta), CoplayDev unity-mcp, Epic's MCP plugin (experimental), or the Godot community MCP servers (Appendix C1).

**QA loop**

1. Import the asset.
2. Spawn it in a test scene with the project's lighting.
3. Capture turntables and gameplay-camera shots.
4. Compare them with the DCC render.
5. Check draw calls and memory against the budget (Appendix H5).

## F6. Protecting artists' manual edits

- Store a SHA256 for every generated output in the manifest. Before any regeneration, rehash the file. If the hash differs, treat the file as human-edited.
- Lock files with whatever version control the project uses:

  | System | How to lock |
  |---|---|
  | Git LFS 3.8 | `git lfs track --lockable`, then `git lfs lock`/`unlock` (the server must support locking) |
  | Perforce | `binary+l` typemap for exclusive checkout |
  | Unity Version Control | Smart Locks |
  | Diversion | Exclusive locks |

- To merge a regeneration with human work:
  - Regenerate only the unlocked layers, masks or regions.
  - Or create a sibling variant, and let the artist merge it.
- Record each edit under `human_edits`, with author, date, hash and note.

## F7. Rendering 3D models to 2D sprites

Use this for consistent characters across many frames and directions. Dead Cells is the reference case: characters were modelled in 3D and rendered very small with no anti-aliasing. Each frame produced a PNG and a normal map, and a toon shader added volume.

**Blender rig**

- An orthographic camera on a pivot, rotated in 8 directions (45° steps) or 16.
- A fixed pixels-per-unit scale.
- Anti-aliasing off (filter size 0).
- Toon or flat shading.
- Outlines with Line Art or Freestyle.
- An optional per-frame normal pass for 2D lighting.

**Steps**

1. Render each animation frame for each direction.
2. Quantize to the palette with no dithering.
3. Clean up orphan pixels.
4. Pack into a sheet with metadata: frame size, pivot, timing per direction.
5. Scale by integers only.

The old blender-spritesheets add-on (MIT) targets Blender 2.81, so prefer your own `bpy` script. SpriteStack and Pixel Composer are paid alternatives.

## F8. 2D rigs and visual novels

**Cutout and skeletal animation tools**

| Tool | License / terms | Notes |
|---|---|---|
| Spine 4.3 | Paid: Essential, Pro and Enterprise editions | |
| DragonBones | JS runtime is MIT | Maintenance status unclear |
| Godot Skeleton2D / Polygon2D | MIT | |
| Unity 2D Animation and PSD Importer | Unity | `.psb` files; blend modes are ignored |
| Inochi2D | BSD-2 | Open format; good for automation |
| Live2D Cubism | Paid (FREE edition available) | See Appendix G |

**Layered art**

- Get layers from the artist's PSD, or decompose a flat image with Qwen-Image-Layered (Apache-2.0).
- Inpaint whatever is hidden behind other layers.
- Name parts consistently, e.g. `body`, `head`, `hair_front`, `hair_back`, `eye_L_open`, `eye_L_closed`, `mouth_A`…

**Visual novel engines**

| Engine | Version / license | Asset conventions |
|---|---|---|
| Ren'Py | 8.5.3; MIT plus LGPL parts | `layeredimage` groups and attributes; files auto-named `name_group_attribute.png` (e.g. `eileen_face_happy.png`) |
| Naninovel (Unity) | Paid | Sprite, Layered, Diced, Live2D and Spine characters |
| Dialogic (Godot) | MIT | |
| TyranoBuilder | Paid | |

**Expression sheets**

- Produce a consistent set: neutral, happy, sad, angry, surprised, embarrassed and thinking, plus blink and talk frames.
- Use the same canvas size and anchor for every sprite.

## F9. Training LoRAs on signed-off assets

**Trainers**

| Trainer | License | Architectures and notes |
|---|---|---|
| ostris ai-toolkit | MIT | FLUX.1/FLUX.2 including klein, Qwen-Image and Edit, Z-Image, SDXL, SD1.5, HiDream, Lumina2, Wan 2.1/2.2, LTX-2, ACE-Step 1.5 (audio) |
| kohya sd-scripts 0.11.1 / kohya_ss | Apache-2.0 | SD1.x/2.x, SDXL, SD3/3.5, FLUX.1, Lumina, HunyuanImage 2.1 |
| musubi-tuner | Apache-2.0 | HunyuanVideo/1.5, Wan 2.1/2.2, FLUX.2, Qwen-Image, Z-Image, Kandinsky 5. Needs 12 GB or more for images and 24 GB or more for video. |
| Diffusers training scripts 0.41 | Apache-2.0 | FLUX.2, FLUX.2 klein, Qwen-Image, SDXL |
| SimpleTuner | AGPL-3.0 | |
| OneTrainer | AGPL-3.0 | |
| diffusion-pipe | GPL-3.0 | |

**Captioning models**

- Florence-2 (MIT).
- Qwen3-VL (Apache-2.0).
- JoyCaption (check the weights' license).

**Dataset rules**

- Build datasets from signed-off assets and the user's own references, and record a dataset manifest with sources and hashes.
- The dataset sizes below are community rules of thumb, not documented requirements. Tune them by evaluation.
- **Style LoRAs:** 30–150 varied subjects in a consistent style. Caption the content, not the style.
- **Character LoRAs:** 15–40 images covering varied poses, angles and expressions, with a consistent design and a unique trigger word.

**Evaluation**

- Render a fixed prompt grid with held-out prompts.
- Score similarity with CLIP or DINOv2 against the references. Single-prompt CLIP scores are unreliable.
- Check for overfitting: training images leaking through, and loss of prompt control.
- Compare the result against the base model.

**Register the LoRA**

- Store the trained LoRA in `library/loras/<item_id>/` with a manifest whose inputs are the dataset and the base model.
- In `registry/models.yaml`, record the base model, the trigger words, recommended weights and the evaluation results.

**Audio LoRAs**

- ACE-Step 1.5's built-in trainer: about 8 songs takes about 1 hour on a 3090.

## F10. Credits files

- Generate `CREDITS.md` from the manifests and registries: the people, tools and models that went into the project's signed-off assets, with Creative Commons attributions in TASL form (title, author, source, license) where an input asks for them.
- Regenerate it rather than editing it by hand, so it stays in step with the manifests.

## F11. Hardware-aware local runs

**Detect the hardware**

- NVIDIA: `nvidia-smi --query-gpu=name,memory.total,memory.free --format=csv,noheader,nounits` (confirm the field names with `--help-query-gpu`).
- AMD ROCm: PyTorch builds use `torch.cuda` and set `torch.version.hip`.
- Apple: MPS.
- ComfyUI's `/system_stats` reports VRAM.

**Choose variants that fit**

| Option | Notes |
|---|---|
| Nunchaku (SVDQuant) 1.2 | Apache-2.0. INT4 on RTX 20-series and newer; NVFP4 on RTX 50-series. Supports FLUX, Qwen-Image/Edit, Z-Image-Turbo and SANA. Qwen-Image runs in about 3 GiB with offloading. |
| ComfyUI-GGUF | Apache-2.0. Good for DiT/transformer models, a poor fit for conv UNets. LoRA support is experimental. |
| FP8 | ComfyUI flags `--fp8_e4m3fn-unet` and `--fp8_e4m3fn-text-enc` |
| TensorRT | Engines don't work with LoRA or ControlNet. FLUX needs more than 24 GB. |

**ComfyUI 0.39 memory flags**

- `--reserve-vram`, `--vram-headroom`, `--async-offload` (on by default for NVIDIA), `--novram`, `--highvram`, `--disable-smart-memory`, `--cpu-vae`.
- Cache modes: `--cache-ram` (default), `--cache-lru N`, `--cache-none`.
- `--lowvram` does nothing while dynamic VRAM is enabled.

**Hardware stats**

- GPU: `nvidia-smi --query-gpu=utilization.gpu,memory.used,memory.total,power.draw,temperature.gpu --format=csv,noheader,nounits`, or NVML through `pynvml` (`nvmlDeviceGetUtilizationRates`, `nvmlDeviceGetMemoryInfo`, `nvmlDeviceGetPowerUsage`, `nvmlDeviceGetTemperature`). Add `-l 5` to sample every 5 seconds during a long job.
- CPU and RAM: `psutil.cpu_percent()` and `psutil.virtual_memory()`, or `Get-Counter '\Processor(_Total)\% Processor Time'` and `Get-CimInstance Win32_OperatingSystem` on Windows.
- Report them before and during long local jobs, after an out-of-memory error, and when the user asks.

**Queue and disk**

- Queue jobs with `POST /prompt` and watch `/ws` for progress. Free memory with `POST /free`.
- Clean the Hugging Face cache with `hf cache ls`, `hf cache prune` and `hf cache rm`.
- Report model-folder sizes, and ask before deleting anything.
