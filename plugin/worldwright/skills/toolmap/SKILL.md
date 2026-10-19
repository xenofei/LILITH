---
name: "toolmap"
description: "Worldwright reference C: tools by asset category with versions and licenses, covering Blender, ComfyUI, game engines and their MCP servers, and 2D, 3D, font and audio processing. Load when choosing a tool or setting up an integration."
user-invocable: false
---

# Appendix C — Tool integration map

Versions and licenses as of 2026-10-11; the license names are reference information. Generation models are in Appendix A.

## C1. DCC apps, engines and their MCP servers

Nearly every creative-tool MCP server can execute arbitrary code (Python or Lua). Run these servers bound to localhost, save the user's work before any exec call, and ask for confirmation before exec tools touch the user's files.

**Blender 5.2 LTS** (GPL; `bpy` 5.2.2 is on PyPI)
- Headless use: `blender -b -P script.py -- <args>`.
- Official Blender Lab MCP v1.0.3: requires Blender 5.1+. It runs model-written Python without safeguards, so run it in a VM.
- Community `blender-mcp` 2.0.0 (MIT): has `execute_blender_code`, an unauthenticated socket, and telemetry that is on by default (set `DISABLE_TELEMETRY=true`).

**ComfyUI v0.39.2** (GPL-3.0)
- Start it with `--listen 127.0.0.1` and `--enable-manager`. `--disable-partner-nodes` turns off the hosted partner nodes.
- Official `comfy-mcp` 0.10.0 (AGPL-3.0 or a commercial license): can install nodes, so restrict it.
- Community alternatives: comfyui-mcp-server (Apache-2.0), comfy-mcp-server (MIT).
- `comfy-cli` handles node and model management.

**Game engines**

| Engine | Official MCP | Community MCP |
|---|---|---|
| Godot 4.7.2 (MIT) | None found | Coding-Solo/godot-mcp (MIT); bradypp/godot-mcp (MIT, has a read-only mode) |
| Unity 6.3 LTS | Unity MCP server, beta (Unity 6+; lists Claude Code) | CoplayDev/unity-mcp v10.3.0 (MIT); IvanMurzak/Unity-MCP (Apache-2.0) |
| Unreal 5.8 | Epic MCP plugin, experimental | chongdashu/unreal-mcp (MIT); flopperam/unreal-engine-mcp (license: verify; runs arbitrary Python) |

**2D and audio tools**
- **Aseprite:** EULA, not open source. Scriptable through its CLI and Lua.
  - diivi/aseprite-mcp (MIT) has 104 tools, including `run_lua_script`.
- **Pixelorama:** MIT.
- **LibreSprite:** GPL-2.0.
- **GIMP:** maorcc/gimp-mcp (GPL-3.0) can run arbitrary Python.
- **Krita and Inkscape:** no MCP server verified. The Inkscape CLI works for SVG conversion.
- **REAPER:** community MCP servers (MIT).
- **Material Maker:** MIT. Procedural materials.

**Proprietary apps** (Spine, Live2D Cubism, VRoid Studio): see Appendices F8 and G.

## C2. 2D image post-processing

**Background removal and matting**

| Tool | License / terms | Notes |
|---|---|---|
| BiRefNet | MIT | Use the HR-matting model for hair and fur, plus `refine_foreground` |
| transparent-background (InSPyReNet) | MIT | |
| BEN2 base | MIT | The enhanced hair-matting version is paid |
| ViTMatte | MIT | Needs a trimap |
| SAM 2.1 | Apache-2.0 | Masks and trimaps |
| SAM 3 | SAM License | Gated access |
| RMBG-2.0 | CC BY-NC | |

**Upscaling**
- Real-ESRGAN (BSD-3).
- spandrel core (MIT; it keeps non-commercial architectures separate).
- SeedVR2 (Apache-2.0).
- SUPIR, 4x-UltraSharp and 4x-Remacri (non-commercial licenses); Upscayl (AGPL desktop app).

**Detail fixing and checks**

| Task | Tools |
|---|---|
| Faces and hands | ComfyUI Impact Pack (GPL-3.0); Impact Subpack (AGPL, detector whitelist); ADetailer (AGPL) |
| OCR | PaddleOCR 3.7 (Apache-2.0), Tesseract 5 (Apache-2.0) |
| Duplicates and similarity | imagehash (BSD-2), open_clip (MIT), DINOv2 (Apache-2.0) |
| Image quality scores | pyiqa (PolyForm Noncommercial); OneAlign (MIT weights) |

**Pixel art**
- proper-pixel-art (MIT), unfake.js (MIT), PixelOE (Apache-2.0), pyxelate (MIT, old dependencies).
- Pillow `quantize`.
- pngquant/libimagequant: GPLv3 or a commercial license.

**Sprite tooling**

| Task | Tools |
|---|---|
| Packing | crunch (MIT), rectpack (Apache-2.0), free-tex-packer (MIT) |
| Frame alignment | OpenCV ECC / phase correlation |
| Frame interpolation | Practical-RIFE 4.25 (MIT); FILM (Apache-2.0 code; check the weights' license) |
| Style propagation | EbSynth (public-domain code) |

**Layers**
- Qwen-Image-Layered (Apache-2.0).

**Vectors**
- Tracing: VTracer (MIT), Potrace (GPL).
- Generation: StarVector (Apache-2.0; not for natural images), OmniSVG (Apache-2.0 code; check the weights' license).
- Cleanup: svgo (MIT), picosvg (Apache-2.0), scour (Apache-2.0).

**Color and contrast**
- DaltonLens (MIT) and colour-science (BSD-3) for color-vision-deficiency simulation.
- APCA is advisory only, under a restricted W3C license.

## C3. Fonts

**Building and fixing**
- fontTools 4.66 (MIT).
- skia-pathops 0.9.2 (BSD-3) to remove overlaps.
- fontmake (Apache-2.0).
- gftools 0.10 (Apache-2.0).
- ttfautohint 1.8.4 (FTL or GPLv2) for hinting.
- FontForge (GPL) for scripted edits.

**Checking**
- fontspector 1.8 (Apache-2.0), the Rust successor to Font Bakery.
- Font Bakery 1.1 (Apache-2.0).

**Game text**
- msdf-atlas-gen (MIT) for SDF/MSDF atlases.
- BMFont format for pixel fonts.

**Coverage and shaping**
- Noto fonts (OFL).
- HarfBuzz (MIT).

## C4. 3D processing

**Geometry**

| Tool | License | Notes |
|---|---|---|
| trimesh 5.1.1 | MIT | |
| manifold3d 3.5.4 | Apache-2.0 | |
| Open3D 0.20 | MIT | |
| libigl | MPL-2.0 | |
| CGAL | GPL/LGPL or commercial | |
| PyMeshLab 2025.7 | GPL | |
| pymeshfix | GPL or commercial | |
| Blender 3D Print Toolbox 1.4.1 | GPL | |

**Retopology**
- Use QuadriFlow (built into Blender), Instant Meshes (BSD-3) or QuadWild-BiMDF (GPL, CLI).
- DeepMesh 0.5B (Apache-2.0) is experimental.
- MeshAnything V2 and FastMesh are non-commercial (S-Lab license).
- BPT is under Tencent's license.

**Part segmentation**
- PartCrafter, SAMPart3D, OmniPart: MIT code; check the weights' licenses.
- Hunyuan3D-Part: Tencent's license.
- PartField and PartPacker are non-commercial (NVIDIA).

**UV unwrapping**
- xatlas (MIT).
- Blender's Smart UV Project and Pack Islands.
- PartUV (bundles PartField).

**Delighting and material estimation**
- Marigold-IID v1.1 (Apache-2.0 code, RAIL++-M weights) is a strong option.
- Hunyuan3D-Delight is under Tencent's license.
- StableDelight: check the license.
- RGB↔X is non-commercial (Adobe Research).
- IntrinsicAnything requires registration.

**Re-texturing**
- MV-Adapter: Apache-2.0 code; the base models' licenses also apply.
- Hunyuan3D-2.1 Paint: Tencent's license; needs 21 GB.
- TEXGen (no license file published).

**Optimization and validation**
- meshoptimizer / gltfpack 1.3 (MIT).
- glTF-Transform CLI 4.5.1 (MIT): `weld`, `dedup`, `prune`, `simplify`, `meshopt`, `etc1s`, `uastc`, `resize`, `center`, `validate`.
- Khronos glTF-Validator (Apache-2.0).
- OpenUSD 26.08 `usdchecker` (UsdValidation; usd-core is under TOST-1.0).

**Collision**
- CoACD (MIT).
- V-HACD (BSD-3).

**Photogrammetry**
- COLMAP 4.2 (BSD).
- Meshroom 2025.1 (MPL-2.0).
- RealityScan: free under $1M revenue.

**Gaussian splats**

| Tool | License | Notes |
|---|---|---|
| splat-transform | MIT | Converts PLY/SOG/SPZ/GLB |
| SuperSplat 3.4 | MIT | |
| SPZ | MIT | |
| Unity: aras-p UnityGaussianSplatting | MIT | |
| Godot: GDGS 3.1 | MIT | |
| Unreal: YaGS | Apache-2.0 | Archived |
| SuGaR | Non-commercial | Mesh extraction from splats |

## C5. Rigging and animation

**Auto-rigging**

| Tool | License / terms | Notes |
|---|---|---|
| UniRig, SkinTokens | MIT | |
| Puppeteer | Apache-2.0 | Exports FBX |
| MagicArticulate | Apache-2.0 | |
| Make-It-Animatable | MIT code; check the weights' license | |
| Rigify | Built into Blender | |
| Mixamo | Free, royalty-free | Humanoids only; no API |
| AccuRIG 2.0 | Free | Windows only; no CLI |

**Skin weights**
- Robust Skin Weights Transfer (MIT reference code).

**Retargeting**
- Rokoko Blender add-on (LGPL-3).
- Expy Kit (license: verify).

## C6. Audio processing

**Analysis and fixes**

| Tool | License | Notes |
|---|---|---|
| ffmpeg | LGPL/GPL builds | |
| SoX | | |
| pyloudnorm 0.2 | MIT | |
| soxr 1.1 | LGPL-2.1+ | |
| librosa 1.0 | ISC | |
| beat_this 1.1 | MIT | |
| PyMusicLooper 3.6 | MIT | Finds loop points |
| essentia | AGPL | |
| madmom | CC BY-NC-SA models | |

**Stem separation**
- python-audio-separator 0.47 (MIT). Each model's weights have their own license.
- Demucs 4.1 (MIT; no longer maintained).
- ZFTurbo MSST (MIT).

**Enhancement**
- DeepFilterNet (MIT/Apache-2.0).
- Resemble Enhance (MIT).
- VoiceFixer (MIT).
- AudioSR (MIT code, Apache-2.0 weights).
- ClearerVoice-Studio (Apache-2.0).
- Apollo (CC BY-SA 4.0).

**Speech QA and alignment**
- faster-whisper 1.2.1 (MIT).
- WhisperX 3.8.6 (BSD-2).
- Montreal Forced Aligner 3.4.3 (MIT).

**Lip-sync**
- Rhubarb Lip Sync (MIT).
- NVIDIA Audio2Face-3D: SDK MIT; models under the NVIDIA Open Model License.

**MIDI**
- pretty_midi and mido (MIT).
- music21 (BSD-3).

Audacity's mod-script-pipe is unsuitable for automation services. Use ffmpeg and Python instead.

## C7. Training, hubs and routing

- **LoRA training:** see Appendix F9.
- **Civitai:**
  - MCP server at `https://mcp.civitai.com/mcp`.
  - Official CLI `@civitai/cli`.
  - ComfyUI-Lora-Manager (GPL-3.0).
- **Hugging Face:**
  - MCP server at `https://huggingface.co/mcp`.
  - `huggingface_hub` 2.2 and the `hf` CLI.
- **Routers:** see Appendix B.
- **VTuber tools:** see Appendix G.
