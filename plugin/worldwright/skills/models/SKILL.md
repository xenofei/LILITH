---
name: "models"
description: "Worldwright reference A: model registry snapshot (Claude, OpenAI, Google, xAI, Black Forest Labs, Chinese labs, open weights) with API IDs, prices, deprecations, licenses and the routing matrix. Load before choosing or calling any generation model or LLM, or when a model ID fails."
user-invocable: false
---

# Appendix A — Models and routing

Snapshot as of 2026-10-11. IDs, prices and deprecations change monthly; confirm against the live source before paid or irreversible steps, then update `registry/models.yaml`.

## A1. Claude

| Role | Model | API ID | Price per million tokens (input / output) | Notes |
|---|---|---|---|---|
| Orchestrator | Claude Opus 5.5 | `claude-opus-5-5` | $4 / $20 | 1M context, 128K output; default effort medium |
| Code, pipelines, rigging logic | Claude Sonnet 5.5 | `claude-sonnet-5-5` | $2 / $10 | |
| Bulk QA, tagging, triage | Claude Haiku 5.5 | `claude-haiku-5-5` | $0.10 / $0.50 up to 100K input; $0.50 / $2.50 above | Batch API is 50% off |
| Hardest long-horizon work | Claude Fable 5.1 | `claude-fable-5-1` | $10 / $50 | Only on request or after failed evals |
| Restricted | Claude Mythos 5.1 | — | — | Project Glasswing participants only |

- In Claude Code, subagents take the model aliases `opus`, `sonnet`, `haiku`, `fable` or `inherit`.
- Effort levels are `low`, `medium`, `high`, `xhigh` and `max`.

## A2. Image models

**Local:**

| Model | License | Use |
|---|---|---|
| Qwen-Image-2512 | Apache-2.0 | Text-to-image |
| Qwen-Image-Edit-2511 | Apache-2.0 | Multi-image edits, character consistency |
| Qwen-Image-Layered | Apache-2.0 | RGBA layer decomposition |
| Z-Image / Z-Image-Turbo (6B) | Apache-2.0 | Fast generation on consumer GPUs |
| FLUX.2 [klein] 4B | Apache-2.0 | About 8 GB of VRAM |
| LongCat-Image / Edit-Turbo | Apache-2.0 | |
| OmniGen2 | Apache-2.0 | |
| Step1X-Edit | Apache-2.0 | |
| Lumina-Image 2.0 | Apache-2.0 | |
| ERNIE-Image (8B) | Apache-2.0 | |
| GLM-Image | MIT | |
| HiDream-I1 / E1 | MIT, plus the Llama 3.1 license on its text encoder | |
| HunyuanImage 2.1 / 3.0 | Tencent community license | 3.0 needs 3–8 × 80 GB GPUs |

**Other local weights:**
- **FLUX.2 [dev] and [klein] 9B** (FLUX Non-Commercial License). Also available through BFL's API.
- **Ideogram 4 open weights** (non-commercial license).
- **Qwen-Image-2.1** (research and evaluation license).

**Hosted:**

| Provider | Model | ID | Notes |
|---|---|---|---|
| OpenAI | GPT Image 2.5 Sunburst / Flare | `gpt-image-2.5-sunburst`, `gpt-image-2.5-flare` | Sunburst for precise edits, Flare for speed. Transparent backgrounds are documented for `gpt-image-2`. `gpt-image-1` shuts down 2026-10-23. |
| Google | Nano Banana 2.1 | `gemini-nano-banana-2.1` | 1K/2K/4K output, up to 14 reference images, SynthID watermark. Also `gemini-3-pro-image` and `gemini-3.1-flash-lite-image`. The Imagen 4 GA models shut down 2026-08-17 (their previews shut down earlier); Nano Banana 2.1 replaces them. |
| xAI | Grok Imagine Image 2.0 | `grok-imagine-image-2.0` | Up to 2K, up to 5 reference images. `grok-imagine-image-quality` redirects here on 2026-11-02. |
| Black Forest Labs | FLUX 3 Image (preview); FLUX.2 [pro]/[flex] | `/v1/flux-3-image` | |
| ByteDance | Seedream 5.0 Pro / Lite / Flash | OpenRouter `bytedance-seed/seedream-5-0-pro` | |
| Alibaba | Qwen-Image 3.0 Pro, 2.0 Pro | `qwen-image-3.0-pro`, `qwen-image-2.0-pro` | API only |
| Tencent | Hy-Image 3.5 preview | OpenRouter `tencent/hy-image-v3.5-preview` | API only |
| Ideogram | 4.5 / 4.0 / 3.0 | Ideogram API | Strong typography; transparent output on 4.0 and 3.0 |
| Recraft | V4.1, V4.1 vector, V4 Pro | `recraftv4_1`, `recraftv4_1_vector`, `recraftv4_pro` | Native SVG output; good for icons and UI |
| Adobe | Firefly Image 5 | Firefly Services API | Also offers partner models |
| Luma / Runway | Uni-1; Gen-4 Image | `uni-1`; `gen4_image`, `gen4_image_turbo` | |
| Midjourney | V8.2 | No official API | Don't use third-party wrappers |

**Specialists:**
- **PixelLab:** pixel art via API and MCP server (`https://api.pixellab.ai/mcp`).
- **Retro Diffusion:** pixel art API.
- **Scenario:** custom model training, plus an MCP server.

## A3. Video (personal work and animation reference)

| Provider | Model | ID or route | Limits | Notes |
|---|---|---|---|---|
| Google | Gemini Omni Flash | `gemini-omni-1.1-flash` (Interactions API) | 10 s per step, extendable to 40 s; 720p native, 1080p/4K by upscaling; native audio | Replaces the Veo 3.1 previews, scheduled to shut down 2026-10-22 at the earliest |
| Kuaishou | Kling 3.0 / O3 | OpenRouter `kwaivgi/kling-v3.0-pro`; fal `kling-video/v3/...` | Audio | Kling 4.0 announced 2026-09-28 (API: verify) |
| ByteDance | Seedance 2.5 | fal `bytedance/seedance-2.5/...` | Up to 30 s, audio, reference sets | BytePlus is not offered in the US |
| Alibaba | Wan 3.0 | `wan3.0-video`; fal `alibaba/wan-3.0-...` | Up to 30 s, 1080p | API only |
| MiniMax | Hailuo H3 / H3 Max | OpenRouter `minimax/hailuo-3` | 4–15 s, 2K, audio | |
| xAI | Grok Imagine Video 1.5 / Lite | `grok-imagine-video-1.5`, `grok-imagine-video-1.5-lite` | Up to 15 s, up to 1080p, lip-synced speech | From $0.08/s; Lite from $0.02/s |
| Runway | Gen-4.5; Aleph 2.0 (edits); Act-Two (performance) | `gen4.5`, `aleph2`, `act_two` | | |
| Luma | Ray 3.2 | `ray-3.2` | 5 or 10 s, up to 1080p, HDR/EXR export | |
| Shengshu | Vidu Q4 | fal `fal-ai/vidu/q4/...` | 3–16 s, up to 4K, audio | |
| PixVerse | V6 | fal `fal-ai/pixverse/v6/...` | Up to 1080p, audio | |
| Black Forest Labs | FLUX 3 Video (preview) | `/v1/flux-3-video` | 5–20 s, up to 3840×2176, audio | |
| Lightricks | LTX-2.5 | `ltx-2-5-pro`, `ltx-2-5-fast`; also open weights | Up to 20 s, up to 4K, audio | |
| Pika | 2.5 | dev.pika.art | | Verify |
| OpenAI | none | — | — | Sora API shut down 2026-09-24; aggregator listings may be stale |
| Midjourney | Video | No API | | |

**Local, open:**

| Model | License | Notes |
|---|---|---|
| Wan 2.2 | Apache-2.0 | The TI2V-5B variant runs on 24 GB |
| Wan2.2-Animate-2-14B | Apache-2.0 | Character animation |
| Kandinsky 5 | MIT | |
| HunyuanVideo 1.5 | Tencent community license | |
| LTX-2.5 | LTX community license | |

**Post-processing:**
- Upscaling: SeedVR2, FlashVSR (both Apache-2.0).
- Frame interpolation: RIFE (MIT).
- Lip-sync: LatentSync (Apache-2.0), MuseTalk (MIT).

## A4. 3D generation and rigging

**Local:**

| Model | License | Notes |
|---|---|---|
| TRELLIS.2-4B | MIT | Textured PBR meshes; 24 GB or more. Its texture baking uses nvdiffrast; re-baking in Blender gives cleaner ORM maps. |
| Step1X-3D | Apache-2.0 | Watertight geometry plus texture |
| Direct3D-S2 | MIT | Shape only |
| TripoSG | MIT | Shape only |
| Hunyuan3D-2.1 | Tencent license | Shape needs 10 GB, paint 21 GB |
| Hunyuan3D-Omni, Hunyuan3D-Part | Tencent license | |
| SAM 3D Objects | SAM License | Outputs Gaussian splats |
| CharacterGen | Apache-2.0 | A-pose characters |
| StdGEN | Research-only checkpoints | Separates body, clothes and hair |

All of these output dense triangle meshes, so plan on retopology and UV unwrapping.

**Hosted:**

| Service | Version / route | Notes |
|---|---|---|
| Hunyuan3D 3.1 | fal `hunyuan-3d/v3.1/...`, Tencent Cloud | Tencent Cloud also offers UV, rigging and topology APIs |
| Tripo | H3.1, P2 | Quad meshes with PBR |
| Meshy | `meshy-7.1` | Textures up to 8K, humanoid rigging with walk/run clips, text-to-motion. API outputs are deleted after 3 days. |
| Rodin | Gen-2.5 | Quad or raw mesh, T- or A-pose; no auto-rigging |
| Seed3D 2.0 | Volcano Engine | |
| TRELLIS.2 | fal `trellis-2` | |

**Rigging:**

| Tool | License / terms | Notes |
|---|---|---|
| UniRig, SkinTokens | MIT | |
| Puppeteer | Apache-2.0 | |
| MagicArticulate | Apache-2.0 | |
| Make-It-Animatable | MIT code; weights license not stated (verify) | |
| Meshy rigging API | Hosted service | Humanoids only |
| Mixamo | Free, royalty-free | Humanoids only; no API |

## A5. Audio

**Music:**

| Model | Terms | Notes |
|---|---|---|
| ACE-Step 1.5 | MIT | 10 s to 10 min. The 2B model runs in under 4 GB; the XL 4B model needs 12 GB or more. Supports LoRA training. |
| DiffRhythm | Apache-2.0 | |
| Stable Audio 3.0 | Small, SFX and Medium models are open weights | Up to 6 min |
| Lyria 3.5 (`lyria-3.5`) | Hosted | 44.1 kHz stereo songs |
| ElevenLabs `music_v2_5` | Hosted; plan terms vary | |
| Suno v6 | No API | |
| MiniMax Music 3 | Open weights under a community license | Its API closed to new users on 2026-08-20 |
| YuE2 | Custom license | |
| Udio | Downloads are disabled | Not usable |

**Sound effects:**

| Model | Terms | Notes |
|---|---|---|
| MOSS-SoundEffect v2.0 | Apache-2.0 | 48 kHz, up to 30 s |
| ElevenLabs `eleven_text_to_sound_v2` | | Up to 30 s, loopable |
| Stable Audio Open | Stability Community License | |
| HunyuanVideo-Foley | Tencent license | Video-to-SFX |
| Seed Audio 1.0 | API | |
| jsfxr / ChipTone-style procedural | | Retro sounds |

MMAudio weights and ThinkSound/PrismAudio are non-commercial.

**Voice (TTS):**

*Local:*
| Model | License | Notes |
|---|---|---|
| Kokoro-82M | Apache-2.0 | |
| Fun-CosyVoice3-0.5B | Apache-2.0 | |
| Qwen3-TTS | Apache-2.0 | Clones a voice from 3 s of audio |
| MOSS-TTS v1.5 | Apache-2.0 | |
| Chatterbox | MIT | |
| Dia2 | Apache-2.0 | English only |
| Orpheus, Sesame CSM | Apache-2.0 plus the Llama license | |
| IndexTTS-2.5 | Bilibili IndexTTS license | |

*Hosted:*
| Model | ID | Notes |
|---|---|---|
| ElevenLabs | `eleven_v4`, `eleven_v4_turbo` | Verify plan terms |
| OpenAI | `gpt-4o-mini-tts` | |
| Gemini | `gemini-3.8-flash-tts` | |
| MiniMax | `speech-2.8-hd` | |
| Qwen | `qwen-audio-3.0-tts-plus` | |

*Other local (non-commercial weights):* F5-TTS, Fish Audio S2, Higgs Audio v3. VibeVoice TTS was withdrawn.

## A6. Non-Claude LLMs (for routed text work)

**US:**
- OpenAI: `gpt-6-astra` (flagship), `gpt-6.1-sol`, `gpt-6-luna` (low cost).
- Google: `gemini-3.8-flash`. Gemini 4 Argon is partner-only.
- xAI: `grok-4.7`.
- Meta: Muse Spark 1.1 / 1.2 (hosted only).

**Europe:**
- Mistral: Large 4 (open weights; verify IDs).

**China:**

| Lab | Model | License / notes |
|---|---|---|
| DeepSeek | `deepseek-v4-pro` | MIT; 1.6T total / 49B active |
| DeepSeek | `deepseek-flash` (V4.1 Flash) | MIT |
| Alibaba | `qwen3.8-max` | Cloud only |
| Alibaba | Qwen3.8-27B | Apache-2.0 |
| Alibaba | Qwen3.8-2.4T-A95B | Custom Qwen license |
| Alibaba | Qwen3.8-Flash-Next | Qwen Community License 1.0 |
| Moonshot | `kimi-k3` | Kimi K3 License |
| Z.ai | `glm-5.3` | MIT-like |
| Z.ai | `glm-5.3-flash` | MIT |
| MiniMax | `MiniMax-M3` | Community license |
| ByteDance | Seed 2.0 Pro, Seed 2.1 Turbo (the Doubao family) | Closed; OpenRouter `bytedance-seed/...` |
| Baidu | ERNIE 4.5 | Open weights (verify license) |
| Tencent | `hy3`, `hy4-preview` | Apache-2.0 |
| Xiaomi | `mimo-v2.6-pro` | MIT |
| StepFun | `step-5-preview` | API only |
| StepFun | Step-3.7-Flash | Apache-2.0 |
| Meituan | LongCat-2.0 | |

Native IDs and router IDs differ. For example, DeepSeek's own API uses `deepseek-flash`, while OpenRouter uses `deepseek/deepseek-v4.1-flash`. Record both in the registry. Direct-provider endpoints are listed in Appendix B5.

## A7. Routing matrix

Defaults when `defaults.models` in `worldwright.yaml` doesn't name a model for the task. Any hosted option above can be used when the user asks for it.

| Task | Primary (local, clean) | Fallback | Escalation (hosted) |
|---|---|---|---|
| Concept and key art | Qwen-Image-2512, Z-Image | FLUX.2 [klein] 4B | GPT Image 2.5 Sunburst, Nano Banana 2.1, Seedream 5.0 Pro, FLUX 3 |
| Consistent character and item edits | Qwen-Image-Edit-2511 | Step1X-Edit, LongCat-Image-Edit | GPT Image 2.5 Sunburst, Nano Banana 2.1 |
| Layered art for rigs and parallax | Qwen-Image-Layered | Manual PSD plus SAM 2.1 masks | — |
| Transparent sprites and UI | Generate, cut out with BiRefNet, bleed alpha | Ideogram 4.0 transparent | `gpt-image-2` transparent |
| Icons and vector UI | Raster, then VTracer | — | Recraft V4.1 vector |
| Pixel art | Qwen-Image-2512 pixel LoRA, then proper-pixel-art | 3D-to-2D renders (Appendix F7) | PixelLab, Retro Diffusion |
| Seamless textures and PBR maps | Z-Image or Qwen with seamless tiling, plus StableNormal | Material Maker (procedural) | — |
| Tilesets and autotiles | Image model with seamless tiling and a 47-tile blob or 16-tile Wang template; edge-match QA; export to Tiled, LDtk or Godot terrains | Assemble by hand from generated material swatches | PixelLab `create_tileset` (MCP) |
| Parallax backgrounds | Wide image split into depth layers with Qwen-Image-Layered (or a depth model), each layer tiling horizontally | — | Hosted image models with outpainting |
| 2D sprite animation | 3D-to-2D renders (F7); keyframes from Qwen-Image-Edit with references, in-betweens from RIFE | Video to sprites: Wan 2.2 or Wan-Animate, then BiRefNet cutout, then re-grid | PixelLab `animate_character` |
| VFX flipbooks | Blender simulations (Mantaflow smoke and fire, particles) rendered to flipbook sheets; procedural noise in Material Maker | Image-model frames plus RIFE, re-gridded and quantized for pixel styles | EmberGen (paid) |
| Fonts (display, pixel) | Customize an OFL font (rename it if it has a Reserved Font Name), or draw glyphs as vectors and assemble them with FontForge or fontmake | Pixel fonts drawn on a grid, exported as BMFont or TTF | Recraft vector glyph drafts, then a type designer |
| Props and environment pieces | TRELLIS.2 geometry, baked in Blender | Step1X-3D; Hunyuan3D-2.1 | Hunyuan3D 3.1, Tripo H3.1, Meshy 7.1, Rodin Gen-2.5 |
| 3D characters | CharacterGen, then retopology | TRELLIS.2 | Tripo H3.1, Meshy 7.1 (with rigging) |
| Rigging | UniRig/SkinTokens, Puppeteer | Make-It-Animatable | Meshy rigging; Mixamo (humanoids) |
| Motion | Hand-keyed and procedural; mocap packs | Meshy text-to-motion | Video-to-motion models (GVHMR, WHAM) |
| Animation reference video | Wan 2.2, Wan2.2-Animate | Kandinsky 5 | Kling 3.0, Seedance 2.5, Gemini Omni |
| Skyboxes and panoramas | — | HY-World panorama | Blockade Labs Skybox AI |
| Music | ACE-Step 1.5 | DiffRhythm | Lyria 3.5, ElevenLabs Music, Stable Audio 3.0 |
| Sound effects | MOSS-SoundEffect v2.0 | Procedural (jsfxr) | ElevenLabs SFX v2, Stable Audio |
| Foley from video | HunyuanVideo-Foley | — | — |
| Dialogue TTS | Kokoro, CosyVoice3, Qwen3-TTS, MOSS-TTS, Chatterbox | Dia2 | ElevenLabs v4, `gpt-4o-mini-tts`, Gemini TTS |
| Bulk text and localization drafts | — | DeepSeek V4.1 Flash, GLM-5.3-Flash, Qwen3.8 | Review by Claude Sonnet |
| Vision QA | Haiku 5.5 | Opus 5.5 | Gemini 3.8 Flash as a second opinion |
