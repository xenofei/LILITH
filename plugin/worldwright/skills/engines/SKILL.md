---
name: "engines"
description: "Worldwright reference H: axes and units, Godot, Unity, Unreal and web import and export notes, texture compression, audio delivery and starting performance budgets. Load before exporting to an engine, setting performance budgets, or checking units, axes or normal-map conventions."
user-invocable: false
---

# Appendix H — Engine export specs and performance budgets

## H1. Axes and units

| Tool or format | Up axis | Handedness | Units | Forward / front |
|---|---|---|---|---|
| Blender | Z | Right | Meters | Front faces −Y |
| glTF 2.0 | Y | Right | Meters | Front faces +Z |
| Godot 4 | Y | Right | Meters | Forward is −Z |
| Unity | Y | Left | Meters | Forward is +Z |
| Unreal 5 | Z | Left | Centimeters (1 uu = 1 cm) | Forward is +X |

- Record any axis conversion and scale factor in the manifest.
- Set pivots by asset type:
  - characters and props: bottom center;
  - modular pieces: grid corner or center, following the kit convention;
  - doors: on the hinge axis.

## H2. Per-engine notes

### Godot 4.7

**Models and import**
- Import `.glb` files and configure them in the scene import settings.
- Run `godot --headless --import` for batch import.

**Textures**
- Fix Alpha Border is on by default. Premultiplied alpha is available, with a matching blend mode.
- VRAM compression uses BPTC on desktop and ASTC on mobile.
- Normal maps use the OpenGL convention.

**Audio**
- WAV is for short, frequently played sounds. Compression defaults to Quite OK Audio (QOA); IMA ADPCM supports forward loops only.
- WAV loop mode can be read from the file or set in the import settings.
- Ogg Vorbis is for music, speech and long sounds. Set the loop, loop offset, BPM and beat count in the import settings, because Ogg files carry no loop metadata.

**2D and other content**
- TileSet terrains for autotiles.
- AnimatedSprite2D and SpriteFrames for animations.
- Skeleton2D for 2D rigs.
- Terrain3D for terrain.
- GDGS for Gaussian splats.
- Dialogic for dialogue.
- Translations via CSV or PO.

### Unity 6.3 LTS (docs for 6.6)

**Models and import**
- FBX, or glTF through glTFast.
- Humanoid avatars for retargeting.
- Folder presets applied with `AssetPostprocessor`.

**Textures**
- Turn sRGB off for data maps.
- Alpha Is Transparency dilates colors at alpha edges.
- Normal maps use the OpenGL convention.

**Audio**
- Imports MP3, AIFF, WAV, Ogg Vorbis and FLAC. No Opus.
- Compression: PCM, ADPCM (for noisy, frequently played sounds) or Vorbis.
- Load types: Decompress On Load, Compressed In Memory, or Streaming (about 200 KB of overhead).
- Force To Mono.

**2D and other content**
- Sprite Atlas.
- PSD Importer (`.psb`).
- 2D Animation.
- Rule Tiles.
- Localization package.

### Unreal 5.8

**Models and import**
- Interchange handles glTF and MaterialX. FBX through Interchange is experimental.
- Units are centimeters and the up axis is Z.
- Nanite is optional for high-poly meshes; keep LODs for everything else.

**Textures**
- Normal maps use the DirectX convention, so flip the green channel when converting from OpenGL.
- Pack occlusion, roughness and metallic into a single ORM texture.

**Landscape**
- Heightmaps in 16-bit PNG or RAW, sized by component count (Appendix F3).

**Audio**
- WAV. Cue points and loop regions come from the WAV file or the waveform editor.

**Automation**
- Python editor scripting and `AssetImportTask`.
- Movie Render Queue for captures.

### Web, Bevy and Defold

- glTF plus KTX2 textures: UASTC for quality, ETC1S for size.
- Meshopt compression (`gltfpack -cc`).
- Texture atlases with JSON metadata.

### VRChat and VTuber apps

See Appendix G.

## H3. Texture formats

**Block compression by platform**

| Platform | Color | Normals |
|---|---|---|
| Desktop and console | BC7 | BC5 |
| Mobile | ASTC | ASTC |
| Web and cross-platform | KTX2/Basis | KTX2/Basis |

**Size rules**
- Power-of-two sizes for mipmapped textures.
- Dimensions a multiple of 4 for block compression.
- UI textures may be non-power-of-two when they don't use mipmaps.

**Color space**
- sRGB for color maps.
- Linear for normal, roughness, metallic, AO and height maps.

**Pixel art**
- Point (nearest) filtering, no mipmaps, and lossless or uncompressed storage.

## H4. Audio delivery

**Masters**
- Keep masters as 48 kHz, 24-bit WAV.
- Encode per engine (H2).
- For middleware (FMOD, Wwise), deliver the WAV masters and let the middleware encode for each platform.

**Channels**
- Mono for 3D-positioned sounds.
- Stereo for music, UI sounds and ambient beds that aren't positioned in the world.

**Loops**
- Write loop points into the WAV `smpl` chunk for Godot and Unreal. Ogg files need Godot import settings instead.
- Render each loop twice and inspect the seam.

**Loudness**
- Use house reference levels per asset category. These are a convention, not a standard.
- Target the final game mix to ASWG-R001:
  - −24 LKFS ±2 on console and PC;
  - −18 on handheld and mobile (some guidance says −16);
  - true peak at or below −1 dBTP;
  - measured over 30 minutes or more of representative play.

## H5. Performance budgets

These are starting points based on common practice, not standards. Override them in `project.yaml` per project and platform.

| Platform | Hero character | Props | Textures | Notes |
|---|---|---|---|---|
| Mobile | 5–15k triangles; 1–2 materials; ≤ 60 bones | 0.3–3k | 512–1024 | ASTC; watch draw calls |
| Handheld (Switch-class) | 15–30k | 1–5k | 1024–2048 | |
| PC and console | 30–100k plus LODs | 2–20k | 2048–4096 | Nanite optional on Unreal |
| Standalone VR | Use VRChat's Quest ranks as a ceiling (7.5–20k) | 0.5–5k | 512–2048 | 72–90 fps targets |
| Web | 5–20k | 0.3–3k | 512–1024 with KTX2 | Budget the total download size |

**Every asset**
- One to two LOD steps of about 50% each.
- Shared atlases and trim sheets where possible.
- Measure draw calls and memory in the engine (Appendix F5).
