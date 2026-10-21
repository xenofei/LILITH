---
name: "repair"
description: "Worldwright reference D: detection and automated repair of AI-generated defects in 3D meshes, rigs, animation, images, pixel art, sprites, alpha, textures, UI, fonts, audio, video and automation. Load whenever QA finds a defect or before running repairs."
user-invocable: false
---

# Appendix D — AI-defect repair playbooks

How to use these tables:
1. Detect with measurements first, then use vision.
2. Apply the automatic fix, then re-check the gates.
3. After two failed passes, regenerate only the affected scope.
4. If that also fails, escalate to the user with evidence.

Log every repair in the manifest's `repairs[]`, and add the repaired file to its run in `candidates/<run_id>/` with role `repair`. Tool details are in Appendix C.

## D1. 3D meshes (image-to-3D and text-to-3D output)

| Defect | Detect | Automatic fix | Escalate to |
|---|---|---|---|
| Duplicate vertices, slivers, degenerate faces | trimesh `is_watertight` and `is_winding_consistent`; count of zero-area faces | Blender Merge by Distance; delete degenerate faces | — |
| Non-manifold edges, holes | Edges with a face count other than 2; boundary loops; Euler characteristic | trimesh `fill_holes`; manifold3d union; voxel remesh; pymeshfix | Regenerate the part |
| Self-intersections, interpenetrating parts | 3D Print Toolbox "Check All"; CGAL or PyMeshLab self-intersection selection | manifold3d boolean union; voxel remesh; Make Manifold | Segment the mesh, then regenerate the part |
| Floating fragments | Connected components; flag pieces under 1–2% of the area | `trimesh.split` and keep the main parts | — |
| Flipped or inconsistent normals | Winding consistency; negative signed volume; rays from outside hitting back faces | Recalculate Outside; `trimesh.repair.fix_normals` | — |
| Triangle soup, poor edge flow, over budget | Face count against the budget; quad ratio; vertex valence histogram | QuadriFlow, Instant Meshes or QuadWild-BiMDF; meshopt simplify or `gltfpack -si 0.5` (target ratio) | DeepMesh (experimental) or manual retopology |
| Fused fingers or accessories, multi-face ("Janus") artifacts, invented back side | Render 6–8 views and run a vision check; Hausdorff distance against the mirrored mesh; part count | Segment (Hunyuan3D-Part, PartCrafter or OmniPart) and regenerate that part; Symmetrize for bilateral objects | New input views, then a human |
| Thin walls | Thickness check; inward ray casts | Solidify; voxel remesh at a finer size | Regenerate |
| Wrong scale, pivot or up-axis | Bounding box against a category prior (a human is about 1.6–1.9 m); is the minimum Z about 0? | Apply transforms; set the origin to bottom center; convert axes on export (Appendix H1) | — |
| Lighting or shadows baked into the albedo | Luminance correlated with an ambient-occlusion bake; extreme dark or bright areas | Delight the reference image before texturing (Marigold-IID), or re-texture | Re-texture |
| Blurry or misaligned texture projection, texture seams | Color difference across UV seams; reprojection error per view | Inpaint in UV space (dilation or LaMa); re-project with MV-Adapter | Human |
| Fragmented UV atlas | Chart count, packing efficiency, stretch | Re-unwrap with xatlas, or Smart UV Project plus Pack Islands; re-bake | — |
| Implausible PBR values | Dielectric albedo below sRGB 30–50 or above 240; metal reflectance outside 180–255; metallic values that aren't near 0 or 1 | Clamp or remap; snap metallic to 0 or 1 | Human |

## D2. Rigs, skinning and animation

| Defect | Detect | Automatic fix | Escalate to |
|---|---|---|---|
| Bad skin weights | Weights per vertex don't sum to 1; more influences than the engine allows (often 4); unweighted vertices | Normalize All; Limit Total; Smooth; Robust Skin Weights Transfer | Re-rig |
| Wrong skeleton (missing tail, wing or hair bones) | Bone count against a template; test poses show broken deformation | Add bones from a template; re-run the auto-rigger with a template hint | Manual rigging |
| Foot sliding | Foot speed while in contact (height below ε and speed below τ) | Lock the feet with IK during contacts | Human |
| Jitter | Jerk metric | One Euro, Butterworth or Savitzky–Golay filter | — |
| Loop seam | Distance between the first and last pose | Cyclic F-curves; crossfade | — |
| Interpenetration | Collision test on key poses | Adjust IK targets; add corrective shapes | Human |
| Root motion handled wrongly | Hips translation pattern | Extract root motion to the root bone, following the engine's convention | — |

## D3. 2D images and illustrations

| Defect | Detect | Automatic fix | Escalate to |
|---|---|---|---|
| Bad hands or faces, extra limbs | YOLO face and hand detectors; vision check | Crop, inpaint, paste back (Impact Pack FaceDetailer or DetailerForEach) | Manual inpaint mask, or a human |
| Garbled text | OCR confidence plus a dictionary check | Inpaint the text area, then render the text with a real font; never rely on generated text | — |
| Low resolution | Size against the target | Upscale (Real-ESRGAN, SeedVR2) | — |
| Duplicated motifs, near-duplicates | Perceptual-hash distance; CLIP embedding similarity | Deduplicate; regenerate with a new seed | — |
| Off-style output | Embedding similarity to the style reference set; vision rubric against the bible | Regenerate with reference images or the project LoRA | Train or update a style LoRA (Appendix F9) |

## D4. Pixel art

| Defect | Detect | Automatic fix | Escalate to |
|---|---|---|---|
| Fake pixel art (off-grid pixels, mixed pixel sizes, anti-aliasing) | Estimate the grid (Canny edges, then Hough lines, then median spacing); count colors per cell | Re-grid using the most common color per cell (`ppa in.png -c 16 -s 1 -t`; check `ppa --help`), or use unfake.js; then downscale to 1× | Regenerate with a native pixel-art generator |
| Too many colors | Count unique colors; cluster by ΔE | Quantize to the fixed palette with no dithering (Pillow, using FASTOCTREE for RGBA) | Hand-pick the palette |
| Orphan pixels, jagged lines | Single pixels via a 3×3 neighbor count; uneven staircase steps | Morphological cleanup (unfake.js) | Pixel artist |
| Semi-transparent edges | Alpha values other than 0 and 255 | Binarize the alpha | — |
| Needs true pixelization from an illustration | — | PixelOE (`--target_size`, `--patch_size`) | Pixel artist |

Scale pixel art by integers with nearest-neighbor sampling only.

## D5. Sprite sheets and 2D animation

| Defect | Detect | Automatic fix | Escalate to |
|---|---|---|---|
| Frame jitter or misregistration | ECC or phase correlation against a reference frame; bounding-box and centroid drift | Shift each frame by an integer offset (translation only for pixel art) | — |
| Inconsistent scale or pivot | Per-frame bounding-box height and foot-line position | Normalize to a reference frame; set a shared pivot | Regenerate with a reference image |
| Flicker, details drifting between frames | Per-frame luma variance; SSIM or embedding distance between neighboring frames | Deflicker (ffmpeg `deflicker`; check its options with `ffmpeg -h filter=deflicker`); propagate a keyframe's style (EbSynth) | Redo the frames from one keyframe |
| Missing in-between frames | — | RIFE 4.25 (handles anime well), then re-grid if the frames are pixel art | Animator |
| Atlas bleeding | Missing padding or extrusion | Pack with padding and extruded edges (crunch or rectpack) | — |

## D6. Cutouts and alpha

| Defect | Detect | Automatic fix | Escalate to |
|---|---|---|---|
| Halos, fringes, background residue | Partial alpha far from the edge; background color contaminating the edge band | BiRefNet with `refine_foreground` (HR-matting for hair) | SAM 2.1 trimap plus ViTMatte |
| Dark borders in the engine | Transparent pixels with RGB = 0; visible under bilinear filtering | Bleed edge colors into transparent pixels (alpha-bleeding, MIT) or premultiply. Godot's Fix Alpha Border is on by default; Unity's Alpha Is Transparency dilates colors. | — |

## D7. Textures and PBR maps (2D)

| Defect | Detect | Automatic fix | Escalate to |
|---|---|---|---|
| Visible tiling seams | Offset the image 50%, then measure edge energy along the center cross; render 2×2 tiles | Regenerate with seamless tiling (circular VAE padding), or inpaint the seam cross | Texture artist |
| Missing PBR maps | — | Normal maps: StableNormal (Apache-2.0) or DeepBump (GPL). Height maps from normals. Material maps: Marigold (RAIL++-M). | — |
| Color-space mistakes | Data maps (normal, roughness, AO) flagged as sRGB | Set the engine's import flags: linear for data maps, sRGB for color | — |
| Wrong normal-map convention | Mean of the normal map far from (0.5, 0.5, 1); green channel looks inverted | Flip the green channel to convert between DirectX and OpenGL (Appendix H) | — |

## D8. UI, icons and vectors

| Defect | Detect | Automatic fix | Escalate to |
|---|---|---|---|
| Raster icons needed as vectors | — | VTracer (`--mode polygon` or `spline`), then svgo or picosvg | Recraft vector output, or a designer |
| Bloated or invalid SVG | Validate | svgo, picosvg | — |
| Blurry icons at small sizes | Re-rasterize at each target size | Snap to the pixel grid; hint by hand at 16–32 px | Designer |
| Insufficient contrast | WCAG 2.2: 4.5:1 for text, 3:1 for large text and non-text | Shift lightness while keeping hue | Designer |
| Color-blind confusion | Machado simulation (colour-science or DaltonLens) | Add shape or icon cues; adjust hues | Designer |
| Text overflow | Render pseudo-localized text (+30% length) | Resize or rewrap the layout | Designer |
| Broken 9-slice | Variance of repeating rows and columns gives the border insets | Set the insets; regenerate a clean center | — |

## D9. Fonts

| Defect | Detect | Automatic fix | Escalate to |
|---|---|---|---|
| Overlapping contours, wrong winding direction | fontspector (universal profile) | skia-pathops overlap removal and direction fix | Type designer |
| Open paths, broken outlines | fontspector; FontForge validation | Rebuild the outlines; fontmake | Type designer |
| Inconsistent stroke weight, poor metrics or kerning | Measure stems across glyphs; check spacing | Normalize stems; auto-space; kern with test strings | Type designer |
| Missing glyphs for a locale | Coverage check against the locale's character set | Generate the missing glyphs, or fall back to Noto | — |
| Unhinted or blurry at small sizes | Render tests | ttfautohint | — |

## D10. Audio

| Defect | Detect | Automatic fix |
|---|---|---|
| DC offset | ffmpeg `astats` DC offset value | `highpass=f=20` (30 Hz for SFX) |
| Clipping | `astats` peak count and flat factor; samples at ±1.0 | `adeclip`, then lower the gain |
| Inter-sample peaks | `ebur128=peak=true` | Lower the gain; two-pass `loudnorm` with `TP=-1` |
| Clicks and pops | Spectral-flux spikes | `adeclick` (lower `t` is more aggressive) |
| Muffled, band-limited output | Spectral rolloff in librosa (little energy above 8–11 kHz) | AudioSR; Apollo for music; ClearerVoice super-resolution for speech |
| Codec shimmer, hiss | Spectral flatness in the high band; ASR confidence for speech | DeepFilterNet3 or Resemble Enhance for speech; Apollo for music (CC BY-SA) |
| Padded silence, abrupt start or end | Silence detection; non-zero first or last samples | `silenceremove`, then `afade` (5 ms in; 10–50 ms out). Skip this for loops. |
| Music or ambience that doesn't loop | PyMusicLooper score; RMS and spectral difference at the seam | PyMusicLooper `export-points`, snapped to beat_this downbeats; equal-power crossfade for ambiences |
| Tempo or structure drift | beat_this beat grid: variance of inter-beat intervals; requested BPM against the detected BPM | Cut at downbeats, or regenerate the section (ACE-Step repaint) |
| Unwanted vocals in an instrumental | RMS of the separated vocal stem above about −40 dBFS | Keep the instrumental stem from python-audio-separator (BS-RoFormer) |
| Stereo or phase problems | Inter-channel correlation below 0; loudness lost when folded to mono | Invert or collapse to mono; always mono for 3D sounds |
| Wrong sample rate | ffprobe | soxr VHQ resampling to 48 kHz |
| TTS mispronunciations or missing words | faster-whisper round-trip: WER or CER against the script | Regenerate the line; use Montreal Forced Aligner to locate the bad word; add phoneme or lexicon hints |
| Wrong emotion | No reliable open classifier; use vision/LLM review of the transcript plus a human listen | Regenerate with emotion controls |
| Lip-sync timing off | Word timestamps (MFA or WhisperX) against the animation keys | Rhubarb (2D mouth shapes A–H, X); Audio2Face-3D (3D) |

Notes on generators:
- MMAudio is trained on 8-second clips, and quality drops at very different lengths.
- ACE-Step 1.5 accepts BPM, key and time-signature controls; use them, then verify the result.

## D11. Video (personal work and animation reference)

| Defect | Detect | Fix |
|---|---|---|
| Flicker, morphing, identity drift | Embedding distance between frames; vision check | Regenerate the segment with reference images or first and last frames; deflicker |
| Low resolution | — | SeedVR2 or FlashVSR |
| Choppy motion | — | RIFE interpolation |
| Bad hands or faces in a few frames | Detectors per frame | Regenerate the shot; inpaint keyframes and propagate |
| Lip-sync mismatch | Speech-to-mouth offset | LatentSync or MuseTalk |
| Duration or aspect ratio wrong | ffprobe | Extend (Gemini Omni, Kling) or regenerate; never stretch |

## D12. Automation failures

| Failure | Detect | Fix |
|---|---|---|
| Hallucinated API calls (bpy, engine scripting) | The script errors in the sandbox, or the output doesn't change | Check against the installed version's API docs, then retry with a minimal script |
| Invalid JSON or YAML | Schema validation | Repair and retry with the schema in the prompt |
| Wrong or missing paths | Check that the file exists | Re-resolve the paths from the manifest |
| Stale model ID, 404 or deprecation notice | Provider error | Switch to the registry fallback; refresh the registry |
| Asynchronous job lost | No status after the timeout | Query the job status before resubmitting |
| Expired output URL | 403 or 404 when downloading | Re-request the job output if the host supports it; download immediately next time |
