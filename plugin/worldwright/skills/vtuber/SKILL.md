---
name: "vtuber"
description: "Worldwright reference G: VRM, VRMA, Live2D, Inochi2D and PNGTuber formats, VRChat limits, automated 3D and 2D VTuber pipelines, avatar animation, streaming-app integration (VTube Studio, VMC, Warudo, VNyan), and reverse-engineering limits. Load for any avatar or VTuber request."
user-invocable: false
---

# Appendix G — VTuber pipeline

Snapshot as of 2026-10-11. No single open-source system produces a finished VRM with expressions and spring bones, or a finished Live2D rig. The pipelines below chain several tools, and you should tell the user that.

## G1. Formats

### VRM 1.0 (glTF-based)

**`meta` permission fields**

| Field | Values (default first) |
|---|---|
| `avatarPermission` | onlyAuthor, onlySeparatelyLicensedPerson, everyone |
| `commercialUsage` | personalNonProfit, personalProfit, corporation |
| `creditNotation` | required, unnecessary |
| `modification` | prohibited, allowModification, allowModificationRedistribution |
| `allowExcessivelyViolentUsage`, `allowExcessivelySexualUsage`, `allowPoliticalOrReligiousUsage`, `allowAntisocialOrHateUsage`, `allowRedistribution` | Booleans, default false |
| `licenseUrl` | Required: `https://vrm.dev/licenses/1.0/` |

- Fill these fields from the manifest and the user's choices.

**Expressions**
- Emotions: happy, angry, sad, relaxed, surprised and neutral.
- Visemes: aa, ih, ou, ee and oh.
- Blinks: blink, blinkLeft and blinkRight.
- Gaze: lookUp, lookDown, lookLeft and lookRight.
- Overrides: `overrideMouth`, `overrideBlink` and `overrideLookAt` each take none, block or blend.

**Extensions**

| Extension | What it does |
|---|---|
| VRMC_springBone | Spring physics for chains (hair, skirts, accessories) |
| VRMC_springBone_extended_collider | Sphere, capsule and plane colliders, with an "inside" option |
| VRMC_node_constraint | Roll, aim and rotation constraints |
| VRMC_materials_mtoon | Toon shading, rim light and UV scroll |
| First-person flags | Per mesh: auto, both, thirdPersonOnly or firstPersonOnly |

### VRM Animation (VRMC_vrm_animation 1.0, `.vrma`)

- A separate glTF file that holds animation only.
- Humanoid bones have no scale, and only Hips may have translation.
- An expression's weight is stored in the X translation of its node, clamped to 0–1.
- Gaze is driven through the lookAt node; the eyes and lookAt presets can't be animated directly.
- The rest pose is the VRM T-pose. 30 fps is suggested.
- The first animation in the file loads by default.

### Live2D standard parameters

Emit these exact IDs:

| Area | Parameter IDs |
|---|---|
| Head | ParamAngleX, ParamAngleY, ParamAngleZ (±30) |
| Eyes | ParamEyeLOpen, ParamEyeROpen (0–1, default 1); ParamEyeLSmile, ParamEyeRSmile; ParamEyeBallX, ParamEyeBallY (±1); ParamEyeBallForm |
| Brows | ParamBrowLY, ParamBrowRY, ParamBrowLX, ParamBrowRX, ParamBrowLAngle, ParamBrowRAngle, ParamBrowLForm, ParamBrowRForm |
| Mouth and cheeks | ParamMouthForm (±1), ParamMouthOpenY (0–1), ParamCheek |
| Body | ParamBodyAngleX, ParamBodyAngleY, ParamBodyAngleZ (±10); ParamBreath; ParamShoulderY; ParamArmLA, ParamArmRA, ParamArmLB, ParamArmRB; ParamHandL, ParamHandR; ParamBustX, ParamBustY; ParamBaseX, ParamBaseY |
| Hair | ParamHairFront, ParamHairSide, ParamHairBack, ParamHairFluffy |

The model bundle consists of `model3.json`, `moc3`, `physics3`, `exp3`, `motion3`, `cdi3` and `motionsync3`. Its schemas are published in Live2D's CubismSpecs repository.

### Inochi2D

- Open project and puppet formats: `.inx` and `.inp`.
- The SDK loads both.
- It's documented well enough to generate rigs programmatically.

### PNGTuber

- There is no shared standard; each app uses its own format.
- veadotube can convert avatars from HÖNK and PNGTuber Plus.

### VRChat performance ranks

| Limit | PC, Excellent → Poor | Quest/Android/iOS, Excellent → Poor |
|---|---|---|
| Triangles | 32k / 70k / 70k / 70k | 7.5k / 10k / 15k / 20k |
| Texture memory | 40 / 75 / 110 / 150 MB | 10 / 18 / 25 / 40 MB |
| Skinned meshes | 1 / 2 / 8 / 16 | — |
| Material slots | 4 / 8 / 16 / 32 | 1 / 1 / 2 / 4 |
| PhysBone components | 4 / 8 / 16 / 32 | 0 / 4 / 6 / 8 |
| Bones | 75 / 150 / 256 / 400 | — |

On mobile, "Very Poor" avatars are blocked by default.

**PhysBones rules**
- Never use a humanoid bone as a PhysBone root.
- A component can hold at most 256 transforms.
- Don't put a constraint and a PhysBone on the same GameObject.

## G2. Tools and licenses

| Tool | Version / license | Notes |
|---|---|---|
| VRoid Studio | v2.14, free, proprietary | Reverse engineering is prohibited. No AI features. |
| VRM Add-on for Blender | 4.10.1, GPL-3.0+ / MIT | Handles humanoid mapping, expressions, spring bones, MToon and export. Check which Blender versions it supports. |
| UniVRM | v0.131.3, MIT | Unity |
| three-vrm | v3.5.5, MIT | Web viewer; use it for QA renders |
| Avatar Toolkit | GPL-3.0 | Successor to the CATS Blender plugin; the GitHub repo is archived and development moved to Disroot |
| Modular Avatar | License: verify | VRChat outfit merging |
| lilToon | 2.3.4, MIT | Shader |
| Avatar Optimizer | v1.9.20, MIT | |
| VRCFury | Custom license | |
| Inochi Creator / Inochi Session | v0.8.6 / v0.8.7, BSD-2 | |
| nijigenerate / nijilive | BSD-2 | Forks derived from Inochi2D 0.8 |
| Live2D Cubism Editor | 5.3.04, paid (FREE edition available) | See below |
| Mixamo | Free, royalty-free | Humanoids |
| AccuRIG | Free | Exports FBX and USD |

**Live2D Cubism Editor**
- Built-in automation:
  - Automatic Mesh Generator
  - Auto Generation of Deformer
  - automatic facial motion
  - automatic sway motion
  - Motion-sync with animation bake
- FREE vs PRO: FREE is limited to 1 texture of up to 2048 px, 100 ArtMeshes, 30 parameters, 50 deformers and 30 parts. PRO removes the limits.

**Cubism External Application Integration API**
- A local WebSocket on port 22033. The user must approve each app.
- Methods: RegisterPlugin, GetParameters, GetParameterValues, SetParameterValues, GetDocuments, GetCurrentModelUID, SendCubismLog, plus notifications when moc3 or motion files are exported.
- It can read and write parameter values and record keys. It can't build meshes or deformers.
- Samples are in Live2D-Garage/CubismExternalAppPluginSamples.

## G3. Automated 3D VTuber pipeline (VRM)

1. **References.** From the character sheet or a text description, generate front, side and back views in A-pose. Use Qwen-Image-Edit-2511 or Nano Banana 2.1 with the sheet as reference.
2. **Mesh.**
   - CharacterGen (Apache-2.0): image to A-pose mesh, trained on VRM renders.
   - StdGEN: separates body, clothes and hair (research-only checkpoints).
   - TRELLIS.2.
   - Hosted: Tripo H3.1, Meshy 7.1 or Hunyuan3D 3.1.
3. **Clean up.**
   - Retopologize for deformation, with edge loops at the joints and face.
   - Separate hair, clothes and accessories.
   - Bring the triangle count within the target VRChat or app budget.
4. **Rig.**
   - UniRig or SkinTokens (MIT). UniRig ships a modified VRM add-on that exports `.vrm`.
   - Or Make-It-Animatable (MIT code), or Puppeteer (Apache-2.0).
   - Or Meshy's rigging API, for humanoids.
   - Then fix the weights (Appendix D2).
5. **Humanoid mapping.** In the VRM add-on, map every required bone.
6. **Face.**
   - Create the VRM expression presets.
   - For "Perfect Sync" tracking, create the 52 ARKit blendshapes by deformation transfer from a reference set, such as hinzka's free 52blendshapes-for-VRoid-face.
   - Test with MediaPipe Face Landmarker, which outputs 52 blendshape scores.
7. **Physics.**
   - Add spring bones for hair, skirts and accessories, with colliders on the head, neck, chest, upper arms and hips.
   - Set stiffness and drag per chain type.
   - Never root a spring bone on a humanoid bone.
8. **Materials.**
   - MToon with outlines, rim light and a shade color from the style bible.
   - Stay within texture-memory limits.
9. **Meta.** Fill in the permission fields from the manifest (see G1).
10. **Export.**
    - VRM 1.0.
    - VRM 0.x as well for VSeeFace, which only reads VRM 0.
    - For VRChat, a Unity package with PhysBones (Avatars 3.0). Use Modular Avatar for outfits.
11. **Animate.** See G5.
12. **Validate.**
    - Render with three-vrm.
    - Load in the user's streaming app (Warudo, VNyan, VSeeFace or VMagicMirror).
    - Check the VRChat performance rank if the avatar is for VRChat.
    - Run the QA gates.

## G4. Automated 2D VTuber pipelines

**Get layers**
- Use the artist's PSD if one exists. Otherwise decompose the image with Qwen-Image-Layered (Apache-2.0).
- Inpaint hidden areas: under the hair, closed eyes, mouth interiors, and behind the arms.
- Generate eye states (open and closed, smile) and mouth shapes (A, I, U, E, O, closed).
- Name parts consistently (Appendix F8).
- See-through (arXiv 2602.03749) is a research method for decomposing anime images into layers. Its code isn't released.

**Path A: Inochi2D (fully automatable, open)**
1. Generate the puppet programmatically: meshes per part, deformation, and parameters mirroring the Live2D standard set above.
2. Add physics for the hair and accessories.
3. Run it in Inochi Session, or in nijilive.

**Path B: Live2D (semi-automated)**
1. Prepare a Cubism-ready PSD, a parameter plan using the standard IDs, and a physics plan.
2. The user runs Cubism Editor's built-in automation: mesh generation, deformer generation, facial motion and sway.
3. Drive parameters and record keys through the External Application Integration API to author motions and expressions.
4. Export the model bundle.

**Template route: Textoon**
- Textoon (Apache-2.0 code) generates Live2D models from text, built on a fixed template.
- CartoonAlive (portrait to Live2D) has no released code; verify before planning around it.

**PNGTuber quick path**
- Make expression sprites (idle, talking, blinking, emotes) at a consistent size and anchor.
- Load them into veadotube or PNGTuber Plus (Unlicense).

## G5. Animation for VTubers

**Clip library**
- Idle, breathing, blinking (procedural with random intervals), head tilts, waves, nods, and emotes (laugh, shock, sad, angry, heart).
- Save clips as VRMA for 3D models, and as motion3 or Inochi2D animations for 2D.

**Motion sources**
- Hand-keyed or procedural animation.
- Motion capture the user owns.
- Motion packs.
- Text-to-motion models. Retarget onto VRM humanoid bones; the bone mapping is standard.
- XR Animator can capture from webcam and video (CC BY-NC-SA).

**Lip-sync**
- VRM: the aa, ih, ou, ee and oh visemes.
- Live2D: MotionSync, or ParamMouthOpenY and ParamMouthForm driven by audio.

**Expressions**
- Map hotkeys in the streaming app (VTube Studio, Warudo, VNyan).

## G6. Streaming apps and integration points

| App | Imports | License | Integration |
|---|---|---|---|
| VTube Studio | Live2D | Closed app; API docs MIT | WebSocket plugin API at `ws://localhost:8001` with token auth: inject parameters, custom parameters, hotkeys, expressions, items, physics overrides, events |
| VSeeFace 1.13.38 | VRM 0.x, VSFAvatar | Closed, free | Sends and receives VMC; Perfect Sync via iFacialMocap or MeowFace |
| Warudo | VRM, Unity mods | Closed | C# plugin and node SDK; Steam Workshop |
| VNyan | VRM 0/1, VSFAvatar | Free / pay what you want | SDK and node graphs; up to 4 VMC receivers |
| VMagicMirror 5.0 | VRM | MIT | |
| XR Animator 0.35 | VRM, MMD | CC BY-NC-SA | MediaPipe tracking; VMC output |
| Inochi Session / nijilive | Inochi2D | BSD-2 | |
| PNGTuber Plus | Its own format | Unlicense | |
| veadotube mini | Images, GIFs | Free | WebSocket control |
| OpenSeeFace 1.20 | — | BSD-2 | Face tracking (ONNX) |
| Kalidokit | — | MIT | Tracking-to-rig solver |
| obs-websocket 5.7 | — | GPL-2.0 | OBS automation |

**VMC protocol**
- OSC over UDP. Port 39539 carries performer-to-marionette traffic; port 39540 carries assistant-to-performer traffic.
- Main addresses: `/VMC/Ext/Root/Pos`, `/VMC/Ext/Bone/Pos`, `/VMC/Ext/Blend/Val`, `/VMC/Ext/Blend/Apply`, `/VMC/Ext/OK`, `/VMC/Ext/T`.

## G7. Reverse-engineering limits for VTuber apps

**Live2D**
- Proprietary Software License v2.1, §6.4: no reverse engineering, decompiling or disassembling.
- So:
  - Integrate through the official Editor, SDK and External API.
  - Never decompile Cubism Core or parse moc3 to convert models.
  - Treat third-party `.moc3` files as untrusted input (MOC3ingbird, CVE-2023-27566).

**VRoid Studio**
- Reverse engineering is prohibited.

**Interoperability**
- Use VMC, the VTube Studio API, the Warudo and VNyan SDKs, and VRM, rather than reverse-engineering closed apps (core reverse-engineering policy).

## G8. Avatar QA gates

**VRM**
- Passes validation on export.
- Meta fields complete.
- All required humanoid bones mapped.
- Expressions include visemes, blinks and lookAt.
- Spring bones have colliders and no jitter.
- MToon materials look right in three-vrm.
- Within budget for the target app or VRChat rank.

**Live2D and Inochi2D**
- Standard parameter IDs.
- No mesh tearing across the full parameter range.
- Physics settle within about 1 second.
- Expressions and motions play in the target app.

**Tracking**
- Under webcam tracking: blink, mouth and head ranges look natural.
- With Perfect Sync, all 52 shapes respond.

**Performance**
- Frame-time cost in the streaming app is acceptable.
- Texture memory is within limits.
