# Image Studio UX Research: Invoke canvas, ComfyUI graph formats, embedding ComfyUI in QtWebEngine

Research date: 2026-10-12. Sources are primary (official docs, source repos, release notes, npm/PyPI registries) unless marked otherwise.

Markers used in this report:
- **[V]** Verified: read directly in source code or official docs on 2026-10-12.
- **[U]** Unverified: inference, recollection or my own assessment. Check it before relying on it.
- **[R]** Recommendation for the Worldwright design. These are opinions.

---

## 0. Version snapshot (as of 2026-10-12)

| Thing | Version | Source |
|---|---|---|
| InvokeAI stable | **v6.14.2** (2026-09-27) | https://github.com/invoke-ai/InvokeAI/releases/tag/v6.14.2 |
| InvokeAI pre-release | **v7.0.0-alpha.2** (2026-10-11). The canvas engine was rewritten. | https://github.com/invoke-ai/InvokeAI/releases/tag/v7.0.0-alpha.2 |
| Invoke project status | Hosted Invoke.ai was shut down when the founding team joined Adobe. The project is now community-maintained, stewarded by lstein and blessedcoolant. | https://invoke.ai/ |
| ComfyUI latest release | **v0.39.0** (2026-10-05). It pins `comfyui-frontend-package==1.53.10`. | https://github.com/Comfy-Org/ComfyUI/blob/v0.39.0/requirements.txt |
| ComfyUI `master` | Pins `comfyui-frontend-package==1.55.14` and `comfyui-workflow-templates==0.11.78`. | https://github.com/Comfy-Org/ComfyUI/blob/master/requirements.txt |
| ComfyUI_frontend | `main` = 1.57.0. Maintenance branches are `core/1.54` and `core/1.55` (latest 1.55.16). PyPI latest is 1.55.16. | https://github.com/Comfy-Org/ComfyUI_frontend/releases, https://pypi.org/project/comfyui-frontend-package/ |
| ComfyUI repo location | `comfyanonymous/ComfyUI` now redirects to **`Comfy-Org/ComfyUI`**. | GitHub API redirect [V] |

Invoke's v7 alpha is important here. It replaces the v6 canvas, which was a Konva/Redux "inpainting canvas", with a real image editor engine: groups, 16 blend modes, non-destructive adjustments, text, gradients, a history panel and PSD export. The inventory below describes **v7 as the current design** and notes v6 behaviour where it differs.

---

# Part 1: Invoke canvas feature inventory

Primary docs for v7:
- Canvas introduction: https://invoke-ai.github.io/InvokeAI-7/users-guide/canvas/introduction/
- Canvas guide (tools and menus): https://invoke-ai.github.io/InvokeAI-7/users-guide/canvas/canvas-guide/
- Layers panel: https://invoke-ai.github.io/InvokeAI-7/users-guide/canvas/layers-panel/
- Generate panel: https://invoke-ai.github.io/InvokeAI-7/users-guide/image-generation/generate-panel/
- Gallery: https://invoke-ai.github.io/InvokeAI-7/users-guide/gallery/introduction/
- Workflows: https://invoke-ai.github.io/InvokeAI-7/users-guide/workflows/introduction/
- v7 canvas engine architecture (excellent for a native re-implementation): https://github.com/invoke-ai/InvokeAI/blob/main/invokeai/frontend/webv2/src/workbench/canvas-engine/ARCHITECTURE.md
- v7 alpha.1 release notes: https://github.com/invoke-ai/InvokeAI/releases/tag/v7.0.0-alpha.1

The repo `main` now has two frontends. `invokeai/frontend/webv1` is the v6 UI, still reachable with `--web-legacy`. `invokeai/frontend/webv2` is the v7 workbench. [V]

## 1.1 Unified canvas and the generation frame (bbox)

- **Concept [V].** The canvas is an infinite document of layers. A dashed rectangle, the **generation frame** (bbox), defines what gets generated. Only enabled raster content inside the frame is composited into the starting image, and the result is placed back at the same spot. Control layers and regional guidance are also read only inside the frame.
- **The frame is not a mask [V].** The docs say: "The frame is not a mask: everything inside it can change in image-to-image."
- **Tool and snapping [V].** The frame has its own tool (`C`). Shift keeps proportions, Ctrl resizes from the centre, and Alt disables grid snapping. It snaps to sizes the selected model supports and has an aspect lock. Header shortcuts: **Fit frame to layers** (Shift+N) and **Fit frame to masks** (Shift+B). Settings: show bbox overlay, show frame, clip strokes to bbox.
- **Scale before processing [V].** Values are Auto, Manual or None. Generation runs at a model-optimal resolution and is then resized back into the frame. This is how a small frame still gets a ~1 MP generation.
- **Size coupling [V].** The frame size is the Generate panel's *Size*.

## 1.2 Layer types (four "stacks")

The Layers panel has four stacks: **Inpaint Masks, Regional Guidance, Control Layers, Raster Layers**. Layers cannot move between stacks except through **Convert to**. Within a stack, higher layers draw over lower ones. [V]

The v7 document contract is `CanvasDocumentContractV3`. It has four forests (`stacks.raster`, `stacks.control`, `stacks.regional_guidance`, `stacks.inpaint_mask`) of leaves plus pass-through groups. Groups have no opacity, blend or transform. Limits are a nesting depth of 10 and 10,000 nodes. Above 2,000 nodes the panel degrades: it drops thumbnails and drag-reorder. [V] (ARCHITECTURE.md)

Each row has three separate states [V]:
- an **enable dot**, which controls whether the layer contributes to generation;
- an **eye**, which only hides the layer visually (a hidden mask still affects generation);
- a **lock**.

Shift+H hides all masks, guidance and control layers.

| Layer type | What it is | Key properties [V] |
|---|---|---|
| **Raster layer** | Pixels, plus text, shape and gradient layers that can be rasterized. Visible raster content in the frame is the init image. | Opacity, **16 blend modes**, lock transparency. Non-destructive **adjustments** as child rows: Brightness/Contrast, Exposure, Levels, Curves, Saturation, Hue, Invert. A **"regenerate region"** child is new in v7. Boolean ops with the layer below (intersect, cut out, cut away, exclude). |
| **Control layer** | The layer's own pixels are the guide image for a ControlNet, T2I-Adapter, Control LoRA or Z-Image ControlNet. The adapter type follows the model. | Weight 0–2 (default 0.75 for ControlNet). Begin/end step range. Control mode (ControlNet only): Balanced, More prompt, More control, Unbalanced. "Transparent preview". Can be converted to a raster layer. |
| **Inpaint mask** | Marks the area to regenerate. Everything outside is kept. | Child rows: **Add noise** (default 25%) and **Denoise limit** (default 80%), which caps how hard the masked area is repainted. Invert mask. Extract masked area. Mask display colour is cosmetic only. |
| **Regional guidance** | A painted region with its own positive and negative prompt. | **Auto-negative**: the region's positive prompt is used as a negative outside the region. Child **regional reference images**: IP-Adapter for SD1.5/SDXL, FLUX Redux for FLUX.1. Skipped if the model lacks regional support. |
| **Reference images (global)** | Live in the Generate panel's Guidance section, not on the canvas. | Most models take up to 5, and order matters for some. SD1.5/SDXL use IP-Adapter: Mode (Style, Composition, Both), Weight, an advanced style variant, and an active-step range. FLUX.1 uses IP-Adapter or Redux ("Image influence"). FLUX Kontext, FLUX.2 and Qwen-Image-Edit read references natively. You can create a reference from the frame contents with *Create From BBox*. |

Model-support caveat: Invoke's support docs said FLUX regional guidance was beta and supported only positive prompts (no negative, reference images or auto-negative). Release notes since then add regional prompting for FLUX.2 Klein, Z-Image, Krea 2 and Anima, so support varies by model. [V for the PRs; U for the current FLUX.1 matrix] (https://support.invoke.ai/support/solutions/articles/151000096682, PRs #9255, #9407, #8672)

## 1.3 Staging area (candidate review)

All behaviour below is [V] from the Canvas guide.
- When the Canvas is the destination, results appear in a **staging bar** at the bottom of the canvas. Step through candidates with arrows or ←/→, and show or hide a thumbnail strip.
- **Accept to Layer** adds the current candidate as a new raster layer and closes staging. Its split-arrow menu offers **Keep as Hidden Layer**, which keeps staging open.
- Other actions: **Save to gallery**, **Discard** (Delete), **Discard All**, and hide the current preview to A/B against the canvas.
- **Auto-switch** options: Newest, In progress, or Off. Cancel stops the running generation. Right-click a thumbnail to recall its settings.
- **While staging or generating, only the View tool works.** The canvas is locked. This is a deliberate, much-discussed constraint; see PR #8044 "Relax Canvas Entity Restrictions during Staging".
- v5.4.3 added *New Layer from Image* in the staging toolbar so you can take one candidate without dismissing the rest. This fixed earlier friction. [V] (https://github.com/invoke-ai/InvokeAI/pull/7389)
- **Routing [V].** A routing control beside Invoke chooses whether results go to the Canvas (staging) or the Gallery. Editing the canvas re-routes to Canvas automatically unless routing is locked.

## 1.4 Canvas tools and hotkeys

All tools below are [V] from the Canvas guide.

| Tool | Key | Notes |
|---|---|---|
| View / pan | H, hold Space | Space pans from any tool. |
| Move | V | Shift locks the axis. Enter places lifted pixels. Arrow keys nudge (Shift for 10 px). |
| Transform | Ctrl+T | Enter or Apply commits; Esc cancels. The Transform pane edits the layer transform numerically without the tool. |
| Generation frame | C | See 1.1. |
| Brush | B | Pressure-sensitive (new in v7). Size, opacity and hardness. `[` and `]` or Ctrl+scroll change size. |
| Eraser | E | |
| Shape | R | Press again to cycle shapes. On masks and regions, colour is ignored. |
| Gradient | G | Linear and radial (added in v6, PR #8774). |
| Text | T | Custom fonts. Ctrl+Enter finishes. |
| Marquee | U | Press again for ellipse. |
| Lasso | L | Press again for polygon. |
| Colour picker | Alt+click with a paint tool | X swaps foreground and background; D resets to black and white. |
| Misc | | Delete erases the selection or deletes the layer. Shift+C clears the selected mask. Ctrl+E merges down. Ctrl+J duplicates. Ctrl+G and Ctrl+Shift+G group and ungroup. |

Ctrl+V pastes as a new raster layer at the frame. [V]

## 1.5 Select Object (segmentation)

- Available from a raster or control layer menu. **Click to add include points, Shift-click for exclude points, or describe the object in text**. Then **Process**, then **Apply**. The result can also be saved as a selection or as a new layer. [V] (Canvas guide)
- Backend: Segment Anything v1 and v2 pipelines (`invokeai/backend/image_util/segment_anything/segment_anything_pipeline.py` and `segment_anything_2_pipeline.py`). Text prompts go through a grounding step. [V that the SAM/SAM2 files exist; U on the exact text-grounding model, which was Grounding DINO in v5.x]
- In v7 it integrates with the new selection system (marquee and lasso) and keeps its own history (`selection/selectionHistory.ts`). [V]

## 1.6 Filters (control-image extraction and image ops)

- Pick a filter, click **Process** (or enable *Process automatically*) to preview, then **Apply**. **Apply as** can write the output to a *new* raster or control layer instead of replacing the current one. Choosing a control model on a control layer auto-starts its recommended filter. [V]
- Filter list [V]:
  - **Edges:** Canny, HED, Lineart, Lineart Anime, MLSD, PiDiNet, Scribble.
  - **Depth and normals:** Depth Anything, Normal map.
  - **Pose and face:** DW Openpose, Mediapipe Face.
  - **Colour and tone:** Color map, Adjust image.
  - **Blur and noise:** Content shuffle, Blur, Noise.
  - **Upscale:** an upscaling model.
- **Run workflow on a layer** (since PR #8665, April 2026). From a raster or control layer menu, choose a workflow, its image input and image output, and a destination: Canvas staging, Replace layer, Copy as raster layer, or Gallery. This generalises filters to any node graph. [V]

## 1.7 Merge, flatten, export and save

- **Merge down** (Ctrl+E), **Merge visible** (Raster Layers header), and **Merge selected**. There is **no dedicated "flatten" command** in the docs. [V]
- **Rasterize** for text, shape and gradient layers. **Crop layer to bbox**. **Fit to bbox**. [V]
- **Save Canvas to Gallery** saves the visible composite. **Save BBox to Gallery** saves only the frame contents. **Save layer to uploads** saves one layer. [V]
- **Export to PSD** is new in v7, from the Raster Layers header. [V]
- **Projects (v7) [V].** A project holds canvas, settings, workflows, queue history and its own boards. Projects open as tabs and autosave (browser first, then server sync). They export as `.invk` files. This resolves the long-standing request in #6984, "Save and Load Canvas As Project".

## 1.8 Outpainting infill methods

- Backend methods in `invokeai/app/invocations/infill.py` [V]: **PatchMatch** (`infill_patchmatch`, only if pypatchmatch is available), **LaMa** (`infill_lama`), **CV2** (`infill_cv2`, OpenCV Telea), **Tile** (`infill_tile`, tile_size default 32), and **Solid Color** (`infill_rgba`). **Mosaic** exists in code but is commented out ("TODO: add mosaic back").
- **Pipeline [V].** For outpainting, the empty (transparent) part of the frame is first filled with the chosen infill. Then the transparent area plus any inpaint masks become the denoise mask, and generation blends the result in.
- **Compositing settings [V]:** Infill method, **Coherence pass mode** (`Gaussian Blur`, `Box Blur`, `Staged`), **Coherence edge size** (default 16), **Coherence min denoise**, and **Mask blur**.
- **Inpaint graph (v6 `addInpaint.ts`) [V].** Resize to the scaled bbox, then optional `img_noise` (from mask "Add noise"), then `create_gradient_mask` (coherence mode, edge radius, minimum denoise), then denoise with the gradient mask, then `expand_mask_with_fade` (fade = mask blur), then `invokeai_img_blend` / `apply_mask_to_image` paste-back. Outpaint adds `tomask` and `mask_combine` of the alpha with the inpaint masks. This ordering maps well onto ComfyUI nodes. Sources: https://github.com/invoke-ai/InvokeAI/blob/main/invokeai/frontend/webv1/src/features/nodes/util/graph/generation/addInpaint.ts and `addOutpaint.ts`.

## 1.9 Denoise strength and img2img

- **Denoising strength** (default 75%) controls how much img2img, inpaint and outpaint may change existing content. Per-mask **Denoise limit** further caps a specific inpaint mask. [V]
- Internally, the frontend converts strength to `denoising_start` / `denoising_end` (`getDenoisingStartAndEnd`) on Invoke's denoise node. In ComfyUI terms this is the KSampler `denoise` parameter, or KSamplerAdvanced start/end steps. [V for the code; the ComfyUI mapping is my translation]

## 1.10 Automatic generation-mode selection

Verbatim logic from v6 `CanvasCompositorModule.getGenerationMode()`, which the v7 docs also describe [V]:

```
composite = visible raster layers clipped to bbox
mask      = visible inpaint masks clipped to bbox
if composite is FULLY_TRANSPARENT        -> txt2img
elif composite is PARTIALLY_TRANSPARENT  -> outpaint   (infill + mask of the transparent area ∪ inpaint masks)
elif mask is FULLY_TRANSPARENT           -> img2img
else                                     -> inpaint
```

- The result is cached by a stable hash of (bbox rect, composite raster hash, composite mask hash). [V]
- Source: https://github.com/invoke-ai/InvokeAI/blob/main/invokeai/frontend/webv1/src/features/controlLayers/konva/CanvasCompositorModule.ts (around line 678).
- Consequence: visibility (the eye) matters for raster layers in this computation. "Visible adapters" are used, so a hidden raster layer is excluded from the init image. [V from code]

## 1.11 Undo and history

- **v6 [V].** `redux-undo` on the canvas slice, `limit: 64`. Snapshot restore and project load are filtered out of history.
- **v7 [V].** An engine-owned history (`canvas-engine/history/history.ts`).
  - Limits are **64 entries** and a **256 MB byte budget** across undo and redo.
  - Edits are *admitted* (memory reserved) before they mutate anything.
  - Replay is asynchronous and failure-atomic. Entries are labelled, for example "Brush stroke", and there is a **scrubbable History pane**.
  - Pixel undo uses image patches and structural undo uses document patches.
  - Undo entries hold references to gallery assets so cleanup doesn't delete images an undo could restore.
  - Ctrl+Z undoes; Ctrl+Shift+Z or Ctrl+Y redoes. *New canvas* is not undoable.

## 1.12 Gallery and boards

All items below are [V] from the Gallery docs and the alpha.2 notes.
- **Boards:** *Uncategorized* (always present), user boards, and project boards (v7). Optional read-only *By Date* boards. *Archived* group. alpha.2 adds multiple boards per project and a *Library* tier.
- **Auto-add board.** New generations, canvas saves, pastes and workflow nodes set to "Auto" go to the chosen board.
- **Starred** items are pinned in a section; a board's cover is its newest starred item. **Media vs Uploads** tabs. Drag-to-board *moves* (never copies).
- **Delete** is permanent across all boards and from disk. It does **not** check whether the canvas still uses the item.
- **Search:** metadata text with date tokens (`from:`, `to:`, `2w`). **Semantic search** by text or by image (DFN2B-CLIP index). **Image Map**: a zoomable similarity map with auto-named clusters, which stays fast at 170k+ images.
- **Recall:** Recall All, Remix, Use Prompt, Use Seed and Use Size from the context menu. v7 also restores LoRAs. Issue #9779 reports that v7 lacks the legacy UI's quick recall buttons.

## 1.13 Workflow editor integration

- Invoke's node editor is the **execution model for everything**. The Canvas and Generate tabs build graphs programmatically, with one builder per model family (`buildSDXLGraph.ts`, `buildFLUXGraph.ts`, `buildZImageGraph.ts` and so on, plus `addInpaint`, `addOutpaint`, `addRegions`, `addControlAdapters`, `addIPAdapters`). [V] (webv1/src/features/nodes/util/graph/generation/)
- **Linear "form" view [V].** Pin any unconnected input with "Expose in Linear UI form", and design the form with headings, text, dividers and row/column containers. View mode shows the form; Edit mode builds it. The form is saved with the workflow.
- **Workflow library [V].** Card grid with model badges and one-click install of missing models. Opening a library workflow copies it into the project. *Save to library* publishes it back.
- **Canvas ↔ workflow [V].** "Run workflow" on a layer (1.6). v7 adds `For`/`ForReturn` loops and media inputs that accept files or gallery items directly.
- **Design direction (RFC #8988) [V].** It proposes that *each tab is a graph-backed workspace*, with Canvas, Viewer and Workflow as views of the same graph. The stated motive is UX: "Hidden or surprising user workflow influence" and "Unclear sources of truth" when pages share state implicitly. lstein closed it as "the original plan for v7". https://github.com/invoke-ai/InvokeAI/issues/8988

## 1.14 Most-used and loved features vs. pain points

**Most loved [U, my assessment; no usage telemetry is public].** The evidence is Invoke's own positioning, issue reactions and the v7 notes.
1. **Unified canvas inpaint/outpaint with automatic mode selection.** The v7 notes say: "The old canvas was great at what it was built for, which was inpainting". The homepage leads with layer-based canvas editing (https://invoke.ai/).
2. **Staging area with candidate cycling.** You review N variants in place and accept one as a layer.
3. **Regional guidance and control layers with built-in filters.** These give one-click Canny or depth from any layer, so you never leave the canvas.
4. **Select Object (SAM)** for fast masks.
5. **Boards plus metadata recall** for iteration.

Historically highest-reaction canvas requests: "Canvas Layers" #4466 (17 reactions, done in canvas v2) and "Save and Load Canvas as Project" #6984 (done in v7 Projects).

**Known UX pain points [V, links to GitHub].**

| Issue | What it shows |
|---|---|
| [#9617](https://github.com/invoke-ai/InvokeAI/issues/9617) (v7, open) | Regional-guidance and control-layer settings render *below* the tool settings in the Properties pane. Users think the layer can't be configured. **Lesson: layer config must not be buried under tool config.** |
| [#9616](https://github.com/invoke-ai/InvokeAI/issues/9616) (v7, open) | The Shape tool creates a raster layer instead of filling the selected mask or region. **Lesson: tools must respect the selected layer type.** |
| [#9779](https://github.com/invoke-ai/InvokeAI/issues/9779) (v7, open) | The prompt, seed and parameter recall buttons from the legacy UI are missing. |
| [#8039](https://github.com/invoke-ai/InvokeAI/issues/8039) | Zoomed viewport aliasing. Canvas render quality matters. |
| [#7764](https://github.com/invoke-ai/InvokeAI/issues/7764) | No hotkey for "new raster layer". |
| [#6313](https://github.com/invoke-ai/InvokeAI/issues/6313) | Regional guidance doesn't rescale when the resolution changes. |
| [#7439](https://github.com/invoke-ai/InvokeAI/issues/7439) | Requests LoRAs per region. |
| [#8471](https://github.com/invoke-ai/InvokeAI/issues/8471) | Requests a start/end step range for regional guidance. |
| [#4208](https://github.com/invoke-ai/InvokeAI/issues/4208) | "Erase bounding box" vs "erase masked content" confusion. |
| [#6219](https://github.com/invoke-ai/InvokeAI/issues/6219) | The bbox defaulted to full image size, causing performance problems. |
| [#7327](https://github.com/invoke-ai/InvokeAI/issues/7327), [#6065](https://github.com/invoke-ai/InvokeAI/issues/6065) | Want finer manual control of the auto-scaled bbox. |
| Staging lock | Only View works while staging. PRs [#8044](https://github.com/invoke-ai/InvokeAI/pull/8044) and [#7389](https://github.com/invoke-ai/InvokeAI/pull/7389) show repeated relaxation of this. |
| [RFC #8988](https://github.com/invoke-ai/InvokeAI/issues/8988) | Implicit cross-page state leakage between Generate, Canvas and Workflow. |
| v7 alpha | A one-way DB migration (v7 DB is unreadable by v6), Python 3.12 required, and "This is alpha software". |

**[R] Takeaways for Worldwright's Image studio**
- Copy the four-stack model, the enable-vs-visible split, the auto mode table (1.10), and staging with Accept-as-layer / Keep-hidden / Discard-all.
- Keep layer properties (prompt, control model, weight) **visible and adjacent to the layer**, not under tool settings.
- Allow non-destructive edits during staging where safe.
- Make each canvas a graph-backed document (Invoke's v7 direction), so the node editor and the canvas are two views of one graph. Without this, state leaks implicitly between them.
- The v7 `ARCHITECTURE.md` is a near-ready blueprint for a native engine. It covers reducer-owned serializable document vs engine-owned pixels, byte-budgeted history, a 10k-node index, and capability-scoped engine handles.

---

# Part 2: ComfyUI workflow and graph formats (current)

Primary sources:
- Zod workflow schema (authoritative in practice): https://github.com/Comfy-Org/ComfyUI_frontend/blob/main/src/platform/workflow/validation/schemas/workflowSchema.ts
- UI→API conversion: https://github.com/Comfy-Org/ComfyUI_frontend/blob/main/src/utils/executionUtil.ts and https://github.com/Comfy-Org/ComfyUI_frontend/blob/main/src/lib/litegraph/src/subgraph/ExecutableNodeDTO.ts
- Node serialization: https://github.com/Comfy-Org/ComfyUI_frontend/blob/main/src/lib/litegraph/src/LGraphNode.ts and `types/serialisation.ts`
- Node-def (object_info) Zod schema: https://github.com/Comfy-Org/ComfyUI_frontend/blob/main/packages/object-info-parser/src/schemas/nodeDefSchema.ts
- Server: https://github.com/Comfy-Org/ComfyUI/blob/master/server.py
- V3 node schema: https://github.com/Comfy-Org/ComfyUI/blob/master/comfy_api/latest/_io.py
- Official docs (they lag the code): https://docs.comfy.org/specs/workflow_json and https://docs.comfy.org/development/comfyui-server/comms_routes

## 2.1 UI workflow JSON

### Two schema versions, and which one the frontend writes

- `validateComfyWorkflow()` dispatches on `version`. If `version === 1` it uses `zComfyWorkflow1`. **Anything else is treated as 0.4** (`zComfyWorkflow`). [V]
- **The frontend still *saves* 0.4 at the root.** `LGraph.serialize()` (marked `@deprecated` in favour of `asSerialisable()`, but still used by `graphToPrompt` and `toJSON`) emits `version: LiteGraph.VERSION`, and `VERSION = 0.4`. **Subgraph definitions** inside `definitions.subgraphs` use the v1 shape, with object links and `state`. [V]
  - Practical result: a saved workflow is a **0.4 root (tuple links) plus v1-shaped subgraph definitions**. Your parser must accept both link encodings.
- docs.comfy.org documents v1.0 as "latest" and doesn't mention subgraphs or `definitions`. **Trust the Zod file over the docs.** [V]

### Root (0.4) fields [V]

| Field | Notes |
|---|---|
| `id` | Optional GUID. |
| `revision` | Optional. |
| `last_node_id`, `last_link_id` | Required. |
| `nodes[]` | See the node table below. |
| `links[]` | **Tuples** `[link_id, origin_node_id, origin_slot, target_node_id, target_slot, type]`. `type` may be a string, a string[] or a number. |
| `floatingLinks[]` | Object links not attached at one end (reroute stubs). |
| `groups[]` | `{id?, title, bounding:[x,y,w,h], color?, font_size?, locked?}`. |
| `config` | `{links_ontop?, align_to_grid?}` or null. |
| `extra` | See below. Passthrough. |
| `version` | Required number. |
| `models[]` | `{name, url, hash?, hash_type?, directory}`. Used to prompt the download of missing models. |
| `definitions.subgraphs[]` | Subgraph definitions. |
| `subgraphs[]` | Optional instance appearance data. |

The root object and almost every sub-object are `.passthrough()`, so **unknown keys are preserved by validation**.

### v1 root (also the subgraph-definition base) [V]

- `version: 1`, `state: {lastGroupId, lastNodeId, lastLinkId, lastRerouteId}`.
- Links are **objects**: `{id, origin_id, origin_slot, target_id, target_slot, type, parentId?, hidden?, label?}`.
- Top-level `reroutes[]`: `{id, parentId?, pos, linkIds?, floating?: {slotType}}`.

### Node object [V]

| Field | Notes |
|---|---|
| `id` | Number. A string is allowed for legacy group-node ids. |
| `type` | The node class, or a **subgraph GUID** for subgraph instances. |
| `pos`, `size` | `[x,y]` or `{0:x, 1:y}`. |
| `flags` | `{collapsed?, pinned?, allow_interaction?, horizontal?, skip_repeated_outputs?}`. |
| `order` | |
| `mode` | `0` ALWAYS, `1` ON_EVENT, `2` **NEVER (muted)**, `3` ON_TRIGGER, `4` **BYPASS**. From `LGraphEventMode` in `globalEnums.ts`. |
| `inputs[]` | `{name, type, link, slot_index?, widget?: {name}, label?, localized_name?…}`. |
| `outputs[]` | `{name, type, links[], slot_index?}`. |
| `properties` | Known keys: `"Node name for S&R"`, `cnr_id` (Comfy Registry pack id), `aux_id` (`github-user/repo` for non-registry packs), `ver` (semver or git hash), `models[]`. Anything else is allowed via catchall. |
| `widgets_values` | See next section. |
| `widgets_values_named` | See next section. |
| `title`, `color`, `bgcolor`, `shape`, `boxcolor`, `showAdvanced` | |
| `extensions` | Namespaced extension payload. |

### `widgets_values` vs `widgets_values_named` (new in 2026)

- `widgets_values` is **positional**, in widget order. It includes frontend-only widgets such as the `control_after_generate` combo after a seed. For example, KSampler's `[seed, "randomize", steps, cfg, sampler, scheduler, denoise]`. [V for the mechanism; U for the exact KSampler example, which is from experience]
- Some custom nodes, for example VideoHelperSuite, replace it with an **object** that has a `length` property. The schema allows array **or** record. [V]
- Since PR #10392 (merged 2026-07-30, "Implement widgets_values_named for serialization"), the frontend writes **both**: positional `widgets_values` and a name→value dict `widgets_values_named`. Restore prefers the named form. It falls back to positional, or to the node-def `fallbackWidgetsValuesNames` list. [V] (`LGraphNode.ts` `serialiseWidgetValues`/`createWidgetRestorationState`)
- **[R]** A third-party editor should *read* named values when present and *write* both forms.

### Widgets vs inputs, and "converted widgets"

- Modern frontends give every widget an optional input socket. You no longer need to "Convert to input". In JSON, a widget-backed input appears in `inputs[]` with `widget: {name}`. [V for the `widget` field and compression; U for the release in which socket-on-every-widget landed]
- On save, `compressWidgetInputSlots()` **drops unconnected widget input slots** (`widget && link === null && !label`) and rewrites link target-slot indices. This makes the output match the "legacy widget conversion" format. A connected widget input stays in `inputs[]`, and its value in `widgets_values` is ignored at execution. [V]
- In `object_info`, `forceInput: true` makes an input socket-only. `defaultInput` is the legacy hint. `socketless` makes it widget-only. [V]

### Primitive nodes, reroutes and notes (frontend-only "virtual" nodes) [V]

| Node | Behaviour |
|---|---|
| `PrimitiveNode` (in `widgetInputs.ts`) | `isVirtualNode = true`. Before queueing, `applyToGraph()` pushes its value into the target widgets. It is never sent to the backend. Property `"Run widget replace on values"`. *Not* the same as backend primitive nodes such as `PrimitiveInt` and `PrimitiveString`, which are real executable nodes. |
| `Reroute` node (in `rerouteNode.ts`) | A legacy virtual node, resolved through during conversion. |
| Native reroutes | Link mid-points stored in `extra.reroutes` (0.4) or `reroutes` (v1). Parent chains go in `extra.linkExtensions: [{id, parentId}]` because 0.4 tuple links can't hold `parentId`. |
| `Note`, `MarkdownNote` | Virtual. |

### `extra` [V]

| Key | Meaning |
|---|---|
| `ds: {scale, offset:[x,y]}` | Canvas pan/zoom state. |
| `frontendVersion` | Stamped by `graphToPrompt`. |
| `linkExtensions` | Reroute parent chains (see above). |
| `linkPresentation` | Per-link `{hidden?, label?}`. Added Sept 2026. |
| `reroutes` | Native reroutes in 0.4 files. |
| `workflowRendererVersion` | `'LG'`, `'Vue'` or `'Vue-corrected'`. The renderer the workflow was laid out in, because Vue nodes have a different size and geometry. |
| `BlueprintDescription`, `BlueprintSearchAliases` | Blueprint metadata. |
| `linearMode`, `linearData: {inputs: [[nodeId, widgetName, {height?}]…], outputs: [nodeId…]}` | The "App / linear mode" form definition, conceptually like Invoke's linear form. |
| `groupNodes` | Legacy group nodes; see below. |

The docs' v1 spec also lists `extra.info` (name, author, …) as optional. [V docs]

### Subgraphs (2026 status)

- **Shipped and stable.**
  - Docs: subgraphs need frontend ≥ 1.24.3; *Unpack subgraph* is available; blueprints ("Add Subgraph to Library") need ≥ 1.27.7; there is a parameters panel for promoted widgets. [V] https://docs.comfy.org/interface/features/subgraph
  - The doc page still carries an older note saying "converting back… will be supported in the future", which conflicts with the documented Unpack feature. Treat Unpack as available.
- **JSON shape [V].** A subgraph **instance** is a node whose `type` is the subgraph GUID. It has `inputs[]` and `outputs[]` whose slots carry `id: GUID` and `linkIds`, and it **has no `properties` field** in `zSubgraphInstance`.
- The **definition** lives in `definitions.subgraphs[]`: `{id (GUID), revision, name, description?, category?, essentials_category?, inputNode:{id,bounding,pinned?}, outputNode:{…}, inputs[], outputs[], widgets:[{id,name}], nodes, links (objects), groups, reroutes, state, extra, definitions (nested)}`. Nesting is recursive.
- **Global blueprints [V].**
  - `GET /global_subgraphs` and `GET /global_subgraphs/{id}` serve JSON from `<custom_node>/subgraphs/*.json` and from ComfyUI's own `blueprints/` dir (`app/subgraph_manager.py`).
  - Docs: https://docs.comfy.org/custom-nodes/subgraph_blueprints
- **Group nodes are removed.** PR **#12931** (merged 2026-06-18) is titled "remove deprecated group nodes, auto-convert to subgraphs on load". `groupNode.ts` is now a "Migration-only adapter for deprecated group nodes". On load, `extra.groupNodes` definitions are synthesized as temporary types (`workflow>Name`, or legacy `workflow/Name`), unpacked, and **repackaged as subgraphs**. A failed conversion *removes the node* ("accepted lossiness"). [V]
  - **[R]** Your editor must support subgraphs. It can treat group nodes as import-only.

### V3 dynamic inputs (matter for graph editing) [V]

V3 nodes (`comfy_api/latest`) publish special input type strings in `object_info`:
- `COMFY_AUTOGROW_V3`: N inputs from a template; API names use dotted paths such as `group.name`.
- `COMFY_DYNAMICCOMBO_V3`: the selected option reveals nested inputs.
- `COMFY_MATCHTYPE_V3`: the output type follows the input type, with an `output_matchtypes` entry.
- `COMFY_DYNAMICGROUP_V3`: repeatable field groups, max 20, and field names may not contain dots.
- **MultiType**: inputs advertise comma-joined types, for example `"IMAGE,MASK"`.

A naive editor that only knows static `INPUT_TYPES` will mis-render these nodes.

## 2.2 API (prompt) format and UI→API conversion

- **Shape [V]:**

  ```
  { "<node_id>": { "class_type": "...", "inputs": { name: value | [origin_id, origin_slot] }, "_meta": {title, cnr_id?, aux_id?, ver?} } }
  ```

  The backend ignores `_meta`. It now carries pack identity so a re-imported prompt can offer missing-pack installs.
- **POST `/prompt` body [V]** (server.py and frontend `api.ts`):

  ```
  { "client_id", "prompt", "prompt_id"?, "number"?, "front"?, "partial_execution_targets"?, "extra_data": {...} }
  ```

  - `prompt_id` is an optional client-supplied UUID. It must be canonical lowercase.
  - `extra_data` holds `extra_pnginfo: {workflow: <UI JSON>}`, `auth_token_comfy_org`, `api_key_comfy_org`, `comfy_usage_source` and `preview_method`.
  - **`extra_pnginfo.workflow` is how the UI JSON gets embedded into output PNGs and job metadata. Always send it** if you want round-trip from outputs.
  - The server strips the sensitive keys `auth_token_comfy_org` and `api_key_comfy_org` before storing history. [V] (`execution.SENSITIVE_EXTRA_DATA_KEYS`)

**What the official frontend does in `graphToPrompt()`** [V, executionUtil.ts plus ExecutableNodeDTO.ts]:

1. For every node in execution order, call `applyToGraph()` on **virtual nodes** (PrimitiveNode, Reroute and others) to push values into their targets.
2. Serialize the UI workflow. Strip `localized_name` from slots, run `compressWidgetInputSlots`, and stamp `extra.frontendVersion`.
3. Build `ExecutableNodeDTO`s. **Muted (mode 2) and bypassed (mode 4) nodes are skipped**, and so are their inner nodes when the node is a subgraph.
4. **Subgraph flattening.** `dto.getInnerNodes()` recursively expands subgraph instances. Execution ids are the path joined with `:`. For example, inner node 7 inside subgraph instance 12 inside instance 3 becomes **`"3:12:7"`**. Links into and out of a subgraph are resolved through the subgraph IO nodes to real producers. An unconnected subgraph input whose slot carries a promoted widget resolves to that **widget's value**.
5. For each remaining non-virtual node:
   - **Widgets.** Each widget with a name and `options.serialize !== false` emits `inputs[widget.name]` using `serializeValue()` if defined. Array values are wrapped as `{__value__: [...]}` so they aren't mistaken for links; curve widgets use `{__type__:'CURVE', __value__}`. The `control_after_generate` combo is excluded because its `serialize` flag is false. [U on the exact flag location, but it is consistent with the observed API output]
   - **Links.** `resolveInput(i)` walks back through reroutes, virtual nodes, **bypassed nodes** and subgraph boundaries:
     - Bypass picks the matching input on the bypassed node. It prefers the same slot index if types are compatible, then the first exact type match, then the first compatible type. A `*` type falls back to the same slot or slot 0.
     - Bypass throws if nothing matches.
     - Circular resolution throws `RecursionError`.
   - **`_meta`** carries the title plus pack ids from `properties`.
6. **Remove dangling inputs** that point at nodes not in the output, for example because they were muted.

Seed "control after generate" (randomize, increment and so on) is **applied by the frontend** after or before queueing, according to a setting. The backend never sees it. A third-party runner must replicate it. [U for the exact setting name]

**[R]** For fidelity, implement conversion in Rust to mirror steps 1–6 exactly, and conformance-test it against the official frontend's "Export (API)" output on a corpus of templates (`comfyui-workflow-templates`). The comfyui MCP's `validate_workflow` can serve as an oracle.

## 2.3 `/object_info` schema

- `GET /object_info` returns all nodes; `GET /object_info/{class}` returns one. Both are also under `/api/...`. [V]
- Per node (server.py `node_info()`, or V3 `get_v1_info()`) [V]:

| Key | Notes |
|---|---|
| `input: {required, optional, hidden}` | Each entry is `name: [TYPE]` or `[TYPE, options]`. |
| `input_order: {required: [...], optional: [...]}` | **Use this for widget order.** JSON object key order is not guaranteed. |
| `is_input_list` | |
| `output: [TYPE…]` | |
| `output_is_list` | |
| `output_name` | |
| `output_tooltips` | |
| `output_matchtypes?` | V3 MatchType. |
| `name`, `display_name`, `description` | |
| `python_module` | `nodes`, `comfy_extras.*`, `custom_nodes.<pack>`, or `comfy_api_nodes.*`. |
| `category` | A slash path, for example `sampling/custom_sampling`. Default `'sd'`. |
| `output_node: bool` | Marks sinks the executor starts from. A prompt with no output node fails validation. |
| `has_intermediate_output` | |
| `deprecated?`, `experimental?`, `dev_only?` | |
| `api_node?` | Partner node; needs a Comfy account. |
| `search_aliases[]` | |
| `essentials_category?` | |

  The frontend schema also accepts `help`, `main_category`, `price_badge` (JSONata for API-node pricing) and `fallbackWidgetsValuesNames`.

- **Input types [V]** (nodeDefSchema.ts):
  - `INT` and `FLOAT`: `min`, `max`, `step`, `round` (FLOAT), `display: slider|number|knob|gradientslider|color(INT)`, `control_after_generate` (INT).
  - `BOOLEAN`: `label_on`, `label_off`.
  - `STRING`: `multiline`, `dynamicPrompts`, `placeholder`.
  - **COMBO**, in two syntaxes:
    - *Legacy (v1):* `[["a","b",…], {…}]`, where the first element is the options array.
    - *V2:* `["COMBO", {options:[…], …}]`. This is what V3 nodes emit.
    - COMBO options: `image_upload`, `video_upload`, `audio_upload`, `mesh_upload`, `animated_image_upload`, `image_folder: input|output|temp`, `allow_batch`, `upload_subfolder`, `control_after_generate`, `multi_select`, and **`remote`**. `remote` is lazy options fetched from a `route` with refresh, `response_key`, `query_params` and timeouts.
  - Any other string is a link-only custom type (`MODEL`, `CLIP`, `LATENT`, `IMAGE`, `MASK`, `CONDITIONING`, `*`, …).
  - Common options: `default`, `tooltip`, `forceInput`, `defaultInput`, `socketless`, `hidden`, `advanced`, `widgetType`, `display_name`, `lazy`, `rawLink`.
- **`hidden` [V]** is `{UNIQUE_ID, PROMPT, EXTRA_PNGINFO, DYNPROMPT, AUTH_TOKEN_COMFY_ORG, API_KEY_COMFY_ORG, COMFY_USAGE_SOURCE}`. The server injects these, so **don't render or send them**. API nodes automatically get the auth hidden inputs.
- Output types can be a COMBO list, for example a node that outputs one of a set of sampler names.

## 2.4 Safe "view + tweak + run" with round-trip fidelity

What the official frontend preserves:
- The Zod schemas are `.passthrough()` almost everywhere, and node `properties` has a catchall.
- `extensionPersistence.ts` (2026-09) explicitly round-trips **non-canonical node and graph fields** (as "legacy" fields) and a namespaced **`extensions`** object. It tolerates non-cloneable values (#18252). [V]
- These changes are on `main` (Sept 2026). Whether they are in the 1.55.x line ComfyUI master pins is [U]. Check `core/1.55` before relying on them.

**[R] Rules for Worldwright's native node editor**

1. **Treat the JSON as the document and edit it surgically.** Keep the original `serde_json::Value`, apply edits as patches (widget value, pos, link add/remove, mode), and re-emit. Unknown keys on root, nodes, links, groups, `extra`, `properties` and `extensions` stay intact byte-for-byte, modulo number formatting.
2. **Never renumber ids.** Bump `last_node_id` / `last_link_id` (0.4) or `state.*` (v1) on add. Keep 0.4 tuple links at the root and v1 object links inside `definitions.subgraphs`. Write `linkExtensions` when you change reroute parents.
3. **Widget values.** Map by name using `input_order` plus `widgets_values_named`, but **write both** `widgets_values` (positional, including frontend-only extra entries such as `control_after_generate`) and `widgets_values_named`.
   - If a node's widget list can't be reconstructed confidently (custom JS widgets, VHS object-style values, dynamic V3 inputs), mark it **read-only in the native editor** and offer "Open in ComfyUI".
4. **Frontend extensions.** Many custom nodes add JS widgets, hidden values or `serializeValue` hooks (for example rgthree, VHS, Impact) that the native editor can't reproduce. Detect `python_module` starting with `custom_nodes.`, and check `/extensions` for a JS file belonging to that pack. Then either:
   - (a) run the *official* conversion by executing `app.graphToPrompt()` in a hidden QtWebEngine page; or
   - (b) degrade that node to read-only.
   Option (a) gives exact parity at the cost of a headless web view.
5. **`extra.ds`.** Preserve it on save unless the user moved the view in the native editor. Never drop `workflowRendererVersion`; it changes node geometry when ComfyUI reopens the file.
6. **Run path.** Use your own Rust UI→API conversion (2.2), or delegate to (4a). Then `POST /prompt` with `client_id` and `extra_data.extra_pnginfo.workflow`. Track progress over `/ws?clientId=…` and fetch outputs via `/history/{id}` or `/api/jobs/{id}`, then `/view`.
7. **Validation.** `POST /prompt` returns `{error, node_errors}` on 400. Surface `node_errors[node_id]` on the native node. Execution ids with `:` map back to subgraph-inner nodes.
8. **Subgraphs.** Render instances as collapsible nodes with breadcrumb navigation, the same model as ComfyUI. Promoted widgets are `definition.widgets[{id,name}]`. Editing a definition affects every instance with that GUID.

**WebSocket events [V].**
- Text events: `status` (with `sid`), `execution_start`, `execution_cached`, `executing`, `progress`, `progress_state`, `executed`, `execution_success`, `execution_error`, `execution_interrupted`, `feature_flags`.
- Binary frames are a 4-byte big-endian type followed by the payload. Types: `1` PREVIEW_IMAGE, `2` UNENCODED_PREVIEW_IMAGE, `3` TEXT, `4` PREVIEW_IMAGE_WITH_METADATA (`protocol.py`).
- Also send a first message `{"type":"feature_flags","data":{…}}` to negotiate.

**Prior art for an external AI canvas on ComfyUI [V].**
- **Krita AI Diffusion** (Acly/krita-ai-diffusion, about 10.7k stars). It builds API-format graphs programmatically for a layer canvas.
- **comfyui-tooling-nodes** provides Load Image/Mask (Base64), Send Image (WebSocket), Load/Save Image to Cache, and Regions (attention masking). These avoid filesystem round-trips.
- ComfyUI also ships `custom_nodes/websocket_image_save.py` and `script_examples/websockets_api_example.py`.
- https://github.com/Acly/krita-ai-diffusion, https://github.com/Acly/comfyui-tooling-nodes

## 2.5 `comfyui-frontend` repo and schema packages

- Repo: **Comfy-Org/ComfyUI_frontend** (GPL-3.0, Vue 3 + TypeScript, vendored litegraph under `src/lib/litegraph`). The separate `@comfyorg/litegraph` npm package is **deprecated** ("Package no longer supported"; last version 0.17.2, 2025-08). [V]
- **Release cadence [V].** Roughly 2-week dev plus 2-week freeze per minor, daily patch releases in freeze, nightlies. `core/1.xx` maintenance branches receive backports. Pinned by ComfyUI through the pip package `comfyui-frontend-package`.
- **`@comfyorg/comfyui-frontend-types` (npm) [V].**
  - It still publishes in lock-step (latest 1.55.16, 2026-10-08). Its `package.json` says types `./index.d.ts` with peer deps `vue` and `zod`.
  - **However, since about 1.48.4 (2026-07-23) the tarballs contain only `LICENSE` and `package.json`, about 35 KB, with no `.d.ts`.** Earlier versions, for example 1.47.12, were about 1.76 MB with 3 files.
  - Cause unknown [U]: it is either a publishing regression or intentional.
  - **It is types-only** even when populated, so there is no runtime Zod.
- **Zod schemas** are not published as a package.
  - `packages/object-info-parser` (`@comfyorg/object-info-parser`, "Shared object_info schemas and helpers") is a private workspace package and **not on npm**. [V]
  - The workflow schema is in `src/platform/workflow/validation/schemas/workflowSchema.ts`.
  - **[R]** Vendor-copy both Zod files at a pinned frontend tag, convert them to JSON Schema (for example with `zod-to-json-schema`), and generate Rust types (`typify` or `schemars`). Re-sync at each frontend bump, and keep a GPL-3.0 licence note for the copied schema source.
- The official JSON Schema for v1 is on docs.comfy.org (`/specs/workflow_json`). Schema RFCs live at https://github.com/comfy-org/rfcs. [V]

## 2.6 Workflow templates "API"

There is no single REST "templates API". Templates come from these sources [V]:

| Endpoint | What it serves | Source |
|---|---|---|
| `GET /templates/index.json` (and `index.{locale}.json`), `GET /templates/{name}.json`, thumbnails | The **core template gallery**, from the pip package `comfyui-workflow-templates` (repo Comfy-Org/workflow_templates). Each index entry has `name`, `title`, `description`, `mediaType`, `tags`, `models`, `minComfyUIVersion`, `openSource`, `io.outputs[]`, `thumbnail` and so on. | `server.py`, `app/frontend_management.py`, frontend `api.ts` |
| `GET /api/workflow_templates` | A map of `custom_node_module → [workflow names]`, from each pack's `example_workflows/` folder. Files are served at `/api/workflow_templates/{module}/{name}.json`. | `app/custom_node_manager.py` |
| `GET /global_subgraphs[/{id}]` | Subgraph blueprints. | `app/subgraph_manager.py` |
| Frontend URL `/?template=<name>[&source=<module>][&mode=linear]` | Opens a template directly (see Part 3). | `useTemplateUrlLoader.ts` |

---

# Part 3: Embedding ComfyUI's web UI in QtWebEngine

## 3.1 Auth

- **The local ComfyUI server has no authentication [V].** `--multi-user` only selects a per-user storage namespace through the `Comfy-User` header (`api.ts` `addHeaderEntry(headers,'Comfy-User', …)`). It is not a security boundary [U on the "not a security boundary" wording; that is my reading]. Bind to `127.0.0.1` (the default).
- **Comfy account (partner/API nodes) [V/U].**
  - The frontend signs in with **Firebase**, using `signInWithPopup` in `packages/account-core/src/firebase/`.
  - Docs say browser login works only from `127.0.0.1` or `localhost`. Otherwise, use a **Comfy API key** from Settings → User. https://docs.comfy.org/tutorials/api-nodes/overview
  - In QtWebEngine:
    - (a) Popups need `QWebEnginePage::createWindow()` implemented, or `window.open` fails.
    - (b) Google OAuth inside embedded webviews is commonly blocked by Google's embedded-user-agent policy [U for QtWebEngine specifically].
  - **[R]** Default to API-key login for embedded use, or route partner-node auth through your native app. The frontend also sends `api_key_comfy_org` in `extra_data`, so a native runner can supply the key itself.
- `--offline` sets a strict CSP and disables partner nodes. [V]

## 3.2 CORS and origin checks

- **Default mode [V]** (`create_origin_only_middleware`):
  - **403** if `Sec-Fetch-Site: cross-site`.
  - **403** if the `Host` is loopback and `Origin`'s host:port ≠ `Host`.
  - **Implication:** load the UI from **the same origin as the server** (`http://127.0.0.1:8188/`), and make native→server calls without an `Origin` header (QNetworkAccessManager, reqwest).
  - If you serve your own page or use a custom URL scheme (`qrc:`, `app://`) and call ComfyUI from JS, you'll hit 403. Watch for `localhost` vs `127.0.0.1` mismatches too.
- **`--enable-cors-header [ORIGIN]` [V].** It *replaces* the origin-only middleware and adds `Access-Control-Allow-Origin: <ORIGIN or *>`, methods, `Content-Type, Authorization` and credentials headers. Use a specific origin, never `*`, on a desktop machine.
- **Reverse proxy [V].** `api_base` is derived from `location.pathname`, so the UI works under a sub-path. Every route is also served under `/api/...`.

## 3.3 WebSockets

- The UI connects to `ws(s)://<location.host><api_base>/ws?clientId=<sessionStorage clientId>`. The server issues `sid` in the first `status` message. The frontend stores it in `sessionStorage` and in `window.name` to keep duplicated tabs apart. [V] (`api.ts`, `server.py`)
- With a reconnect using the same clientId, the server re-sends the current `executing` node. [V]
- Events go to the socket whose `client_id` queued the prompt; broadcast events go to all. **[R]** Your native runner should use its **own clientId**. The embedded UI will then not show your native-queued jobs' previews (they are targeted), although the queue and history are shared. That may be desirable, or not.
- QtWebEngine supports WebSockets natively. The origin check above applies to the WS upgrade because it passes through the same middleware chain. [V for the middleware chain; U that the WS handshake carries a mismatching Origin in your setup]

## 3.4 Frontend version pinned by the pip package

- `requirements.txt` pins `comfyui-frontend-package==X` (v0.39.0 → 1.53.10; master → 1.55.14). At startup ComfyUI compares installed vs required `comfy*` packages and warns if outdated (`check_comfy_packages_versions`). [V]
- `--front-end-version owner/repo@vX.Y.Z|latest|prerelease` downloads a release `dist.zip` from GitHub into `web_custom_versions/`. `--front-end-root <path>` serves any local build. The default string is `comfyanonymous/ComfyUI@latest`, which means "use the pip package". [V] (`app/frontend_management.py`, `comfy/cli_args.py`)
- Templates (`comfyui-workflow-templates`) and embedded docs (`comfyui-embedded-docs`) are separate pip packages, also pinned. [V]
- **[R]**
  - Pin ComfyUI by tag *and* record the frontend version in your manifest.
  - Run your round-trip conformance tests against that exact frontend.
  - Read `extra.frontendVersion` from saved workflows to warn on newer-than-pinned files.
- **Browser engine floor [U].** The frontend is built with `target: 'es2022'` (vite.config.mts), but runtime APIs aren't polyfilled.
  - Use a recent Qt (6.8 LTS or later) and check `qWebEngineChromiumVersion()` at runtime.
  - Qt WebEngine tracks "the latest stable Chrome at feature freeze" per the Qt docs. I could not confirm a minimum Chromium for ComfyUI_frontend.

## 3.5 Opening a specific workflow via URL or API

- **URL parameters supported by the frontend [V]:**
  - `?template=<name>&source=<module|default>&mode=linear` opens a template from `/templates` or a custom-node `example_workflows` (`useTemplateUrlLoader.ts`; params must match `^[a-zA-Z0-9_.-]+$`).
  - `?share=<id>` opens Comfy-cloud shared workflows. It is not useful offline.
  - Other namespaces include `settings`, `assets`, `invite`, `workspace` and `desktop_login` (`preservedQueryNamespaces.ts`).
  - **There is no URL parameter to open an arbitrary local workflow file or JSON.**
- **Options [R]:**
  1. **JS bridge (recommended).** Call `QWebEnginePage::runJavaScript("window.comfyAPI.app.app.loadGraphData(<json>, true, true, '<name>')")`. In production builds every export under `src/scripts/*` is mirrored to `window.comfyAPI.<module>.<export>` by `comfyAPIPlugin` (docs/extensions/development.md). The signature is `loadGraphData(graphData, clean=true, restore_view=true, workflow=null, options)`. [V]
     - Pair with **QWebChannel** for events back to Rust/Qt, such as "workflow changed" or "user saved".
     - `app.graphToPrompt()` gives you the official API conversion on demand (2.4, rule 4a).
  2. **Tiny companion custom node** with a `WEB_DIRECTORY` JS extension (`app.registerExtension`). It can listen on QWebChannel or a custom WS message and call `loadGraphData`. This is more robust to frontend refactors than poking globals, because the extension API is a supported surface.
  3. **Userdata file.** `POST /userdata/workflows%2F<name>.json` writes into the user's workflow library (`user/default/workflows/`). The user (or option 1) then opens it from the sidebar. Persistence of open tabs uses browser storage (`useWorkflowPersistenceV2`), so the **QWebEngineProfile must be persistent**.
  4. Drag-and-drop of `.json` or PNG files onto the canvas works (`app.handleFile`). PNG, WebP and other formats with embedded workflows are parsed. API-format JSON is also accepted (`loadApiJson`).
- **QtWebEngine specifics [V Qt docs, https://doc-snapshots.qt.io/qtwebengine/qwebengineprofile.html]:**
  - The **default profile is off-the-record**. Use a named `QWebEngineProfile("comfyui")` so `localStorage` and IndexedDB persist (ComfyUI settings, open tabs, the clientId fallback). Delete the profile before app exit so it flushes.
  - **Downloads** ("Export workflow", "Save image") need `QWebEngineProfile::downloadRequested` handled with `accept()`. Otherwise they are silently cancelled.
  - **Permissions** (clipboard read for paste, notifications) persist by default on disk profiles (`persistentPermissionsPolicy`, Qt 6.8+). Clipboard paste of nodes and images also needs `QWebEngineSettings::JavascriptCanPaste` / `JavascriptCanAccessClipboard` [U on which flags the frontend needs].
  - **Popups** need `createWindow()` (see 3.1).
  - On Windows, the default UA reports "Windows NT 6.2" unless your app manifest declares newer OS support (cosmetic).
- **Known "embed" issues I could verify:** none specific to QtWebEngine. A 2023 issue "Failed to run within colab iframe" (https://github.com/Comfy-Org/ComfyUI/issues/479) relates to the origin check above. The frontend team tests iOS WKWebView (issue #13976), which suggests webview embedding is a supported scenario [U].

---

## Source index (primary)

- Invoke releases: https://github.com/invoke-ai/InvokeAI/releases
- Invoke v7 docs: https://invoke-ai.github.io/InvokeAI-7/ (canvas: `/users-guide/canvas/{introduction,canvas-guide,layers-panel}/`)
- Invoke v7 canvas architecture: https://github.com/invoke-ai/InvokeAI/blob/main/invokeai/frontend/webv2/src/workbench/canvas-engine/ARCHITECTURE.md
- Invoke v7 history: https://github.com/invoke-ai/InvokeAI/blob/main/invokeai/frontend/webv2/src/workbench/canvas-engine/history/history.ts
- Invoke generation mode: https://github.com/invoke-ai/InvokeAI/blob/main/invokeai/frontend/webv1/src/features/controlLayers/konva/CanvasCompositorModule.ts
- Invoke infill: https://github.com/invoke-ai/InvokeAI/blob/main/invokeai/app/invocations/infill.py
- Invoke gradient mask: https://github.com/invoke-ai/InvokeAI/blob/main/invokeai/app/invocations/sd/create_gradient_mask.py
- Invoke project status: https://invoke.ai/
- Invoke support (regional guidance, FLUX limits): https://support.invoke.ai/support/solutions/articles/151000096682
- ComfyUI_frontend: https://github.com/Comfy-Org/ComfyUI_frontend. Files: `src/platform/workflow/validation/schemas/workflowSchema.ts`, `src/utils/executionUtil.ts`, `src/utils/litegraphUtil.ts`, `src/lib/litegraph/src/{LGraph.ts,LGraphNode.ts,LiteGraphGlobal.ts,extensionPersistence.ts,types/serialisation.ts,types/globalEnums.ts,subgraph/ExecutableNodeDTO.ts}`, `src/extensions/core/{groupNode.ts,widgetInputs.ts,noteNode.ts,rerouteNode.ts}`, `packages/object-info-parser/src/schemas/nodeDefSchema.ts`, `src/platform/workflow/templates/composables/useTemplateUrlLoader.ts`, `src/scripts/{api.ts,app.ts}`, `docs/extensions/development.md`
- Group-node removal PR: https://github.com/Comfy-Org/ComfyUI_frontend/pull/12931
- ComfyUI server: https://github.com/Comfy-Org/ComfyUI/blob/master/server.py, `app/frontend_management.py`, `app/subgraph_manager.py`, `app/custom_node_manager.py`, `comfy/cli_args.py`, `comfy_api/latest/_io.py`, `protocol.py`, `execution.py`
- Docs: https://docs.comfy.org/specs/workflow_json, https://docs.comfy.org/development/comfyui-server/comms_routes, https://docs.comfy.org/interface/features/subgraph, https://docs.comfy.org/custom-nodes/subgraph_blueprints, https://docs.comfy.org/tutorials/api-nodes/overview
- Templates: https://github.com/Comfy-Org/workflow_templates
- npm: https://www.npmjs.com/package/@comfyorg/comfyui-frontend-types (tarball contents checked via registry), https://www.npmjs.com/package/@comfyorg/litegraph
- Qt WebEngine profile docs: https://doc-snapshots.qt.io/qtwebengine/qwebengineprofile.html
- Prior art: https://github.com/Acly/krita-ai-diffusion, https://github.com/Acly/comfyui-tooling-nodes
