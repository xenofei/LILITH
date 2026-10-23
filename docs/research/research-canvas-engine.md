# Canvas engine research: Rust core + Qt 6.12 Quick (QQuickRhiItem) on Windows

Date: 2026-10-12. Target: native image studio, Rust core, Qt 6.12 Quick/QML via cxx-qt 0.10 plus a C++ shim, a 16 GB consumer GPU, layered documents stored as content-addressed layer files.

**Verification legend**
- **[V]** I checked this against a primary source: official docs, source code at `master`/`dev`, or a project's own README or blog.
- **[P]** Partly verified. The source was a search snippet, blocked, or secondary.
- **[U]** Unverified. This is my inference, general knowledge, or a design recommendation. Treat these as hypotheses to prototype.

Source code was read from the projects' default branches as of today. Line-level details may differ in the exact release you ship.

---

## 0. TL;DR: recommended architecture

```
                 GUI thread (QML)                        Render thread (Qt Quick, threaded loop on D3D11)
 ┌────────────────────────────────────────┐         ┌─────────────────────────────────────────────────┐
 │ QML UI (panels, tools)  ⇄ cxx-qt QObjects│        │ C++ CanvasRhiItemRenderer (QQuickRhiItemRenderer)│
 │ C++ CanvasRhiItem (QQuickRhiItem)        │ sync() │  • upload dirty tiles → texture-array slots      │
 │ C++ window event filter: QTabletEvent ───┼──┐ ───►│  • compute: composite dirty projection tiles     │
 └────────────────────────────────────────┘  │      │  • generate mips for touched projection pages    │
                                             │      │  • draw visible pages + display-LUT + overlays   │
            lock-free sample queue           ▼      └─────────────────────────────────────────────────┘
 ┌──────────────────────────────────────────────────────────────────────────────────────────────┐
 │ Rust core (no Qt types):                                                                     │
 │  TileStore: layers = persistent map<TileCoord, Arc<Tile>> (128×128), COW via Arc::make_mut    │
 │  Brush worker: stamp/dab engine → writes tiles → emits dirty-tile set                        │
 │  Undo: snapshots of tile maps (structural sharing), budgeted, spills to content-addressed CAS│
 │  CPU reference compositor (export, thumbnails, golden tests), selection ops, adjustments     │
 │  ComfyUI client: WS preview frames → decode → preview overlay texture                        │
 └──────────────────────────────────────────────────────────────────────────────────────────────┘
```

**Key decisions**
1. **Pixels on the CPU are authoritative.** RAM holds the tiles. Brush and edit ops run in Rust on the CPU. The GPU holds a budgeted cache of tiles and does all compositing for display.
2. **Storage tiles are 128×128.** Each tile is content-hashed (e.g. BLAKE3), so tiles double as the unit for undo, swap, and the content-addressed layer files.
3. **Layer storage formats:** RGBA8 sRGB-encoded, straight alpha, for M1 "8-bit documents". The tile store is generic over format, so RGBA16F linear "16/32-bit documents" can be added later. Masks and selections are R8.
4. **GPU compositing** uses QRhi compute shaders (or a fragment fallback) that read layer tiles from texture arrays. The result goes to an RGBA16F projection, stored as pages with mipmaps. Blend math follows the W3C Compositing and Blending formulas, done in sRGB-encoded space by default to match Photoshop and Krita 8-bit behaviour. A per-document "linear blending" flag can come later.
5. **Display colour management** is a 3D LUT built with moxcms, from the document space to the monitor ICC profile, and applied in the canvas fragment shader. **HDR/scRGB is not in M1.**
6. **Brush input:** QTabletEvent is captured in C++ through a QQuickWindow event filter with tablet compression OFF (the default). Samples are pushed to Rust. The M1 brush is a Rust stamp engine with pressure, spacing, flow, hardness, smoothing, and an eraser.
7. **Undo** uses tile-level copy-on-write (COW) snapshots: Krita's model, implemented with `Arc` plus a persistent map. It has a memory budget and spills to the CAS.
8. **Adjustments** in M1 are non-destructive adjustment layers, but only for point ops: levels, curves, hue/sat, brightness/contrast. The GPU runs the same shader code paths, with a CPU reference implementation. Neighbourhood filters (blur, sharpen) are destructive in M1.
9. **Not for M1:** wgpu interop, Vello, Skia, libmypaint. Section 2.4 covers why and when to revisit each.

---

## 1. Tiled layer storage and compositing

### 1.1 How existing apps do it

**Krita** ([source, invent.kde.org](https://invent.kde.org/graphics/krita))
- **Tiles [V].** `KisPaintDevice` data lives in a `KisTiledDataManager`. Tiles are **64×64**: `__TILE_DATA_WIDTH 64` in [`libs/image/tiles3/kis_tile_data_interface.h`](https://invent.kde.org/graphics/krita/-/blob/master/libs/image/tiles3/kis_tile_data_interface.h). The tile matrix grows on demand, can have negative indices (an unbounded canvas), and creates tiles lazily ([kis_tiled_data_manager.cc on LXR](https://lxr.kde.org/source/graphics/krita/libs/image/tiles3/kis_tiled_data_manager.cc)).
- **Undo = COW mementos [V].** `KisMementoManager::registerTileChange()` is called when a tile gets new tile-data through COW. After `commit()`, the memento item becomes a co-owner of the old tile data, which is "never changed since then"; each later write duplicates it ([kis_memento_manager.h](https://invent.kde.org/graphics/krita/-/blob/master/libs/image/tiles3/kis_memento_manager.h)). A `KisTileDataPooler` thread pre-duplicates tile data to make COW faster (kis_tile_data_interface.h). This is exactly the model recommended in §4.
- **Swap [V].** Tile data can be compressed and swapped (`tiles3/swap/`). The default memory hard limit is **50%** of RAM (`memoryHardLimitPercent` in [kis_image_config.cpp](https://invent.kde.org/graphics/krita/-/blob/master/libs/image/kis_image_config.cpp)). The default undo stack limit is **200** steps (`undoStackLimit` in [kis_config.cc](https://invent.kde.org/graphics/krita/-/blob/master/libs/ui/kis_config.cc)). The user docs describe "Swap Undo After" and a swap-file size limit ([Performance Settings](https://docs.krita.org/en/reference_manual/preferences/performance_settings.html)).
- **Projection [V].** `KisImage` keeps a projection (the merged image). Updates go through `KisUpdateScheduler` and walkers (`KisMergeWalker`, `KisRefreshSubtreeWalker`). The walkers compute the need-rect and change-rect per layer, and then `KisAsyncMerger` recomposites only the dirty rectangles ([kis_async_merger.cpp](https://invent.kde.org/graphics/krita/-/blob/master/libs/image/kis_async_merger.cpp)). All compositing runs on the CPU.
- **Display [V].**
  - The OpenGL canvas keeps its own texture tiles, **256×256** by default (`openGLTextureSize` default 256 in kis_config.cc). Each tile has an overlap border of `1 << numMipmapLevels` pixels to allow mipmapped sampling without seams.
  - Uploads go through pixel-unpack buffers (`KisOpenGLBufferCircularStorage`), and mipmaps are regenerated per tile with `glGenerateMipmap` ([kis_texture_tile.cpp](https://invent.kde.org/graphics/krita/-/blob/master/libs/ui/opengl/kis_texture_tile.cpp), [kis_opengl_image_textures.cpp](https://invent.kde.org/graphics/krita/-/blob/master/libs/ui/opengl/kis_opengl_image_textures.cpp)).
  - Conversion to the display colour space happens on the CPU while building update info, or via OCIO in the shader.
- **Instant Preview / LOD [V].** Krita draws feedback on "a smaller version of the canvas" while the full-resolution stroke is computed in the background. It applies by default to brushes over 100 px ([Instant Preview docs](https://docs.krita.org/en/reference_manual/instant_preview.html)).
- **Selections [V].** `KisPixelSelection` is a paint device in the **alpha8** colour space ([kis_pixel_selection.cpp](https://invent.kde.org/graphics/krita/-/blob/master/libs/image/kis_pixel_selection.cpp)). The marching-ants outline is a cached `QPainterPath` built by `KisOutlineGenerator`, which is "used e.g. for brushes and marching ants" ([kis_outline_generator.h](https://invent.kde.org/graphics/krita/-/blob/master/libs/image/kis_outline_generator.h)).

**GIMP / GEGL**
- **Tile size [V].** GEGL's built-in default is **128×128** (`tile-width`/`tile-height` default 128 in [gegl-config.c](https://gitlab.gnome.org/GNOME/gegl/-/blob/master/gegl/gegl-config.c)). This changed from 512 in 2014 ([commit mail](https://mail.gnome.org/archives/commits-list/2014-June/msg02934.html)). The env-var page still says 128x64, which is **stale** ([gegl.org/environment.html](https://gegl.org/environment.html)).
- **Swap and cache [V].** The tile cache size is configurable. With no `GEGL_SWAP` set, GEGL does not swap to disk (same page).
- **COW tiles [V].** `gegl_buffer_copy` shares tiles copy-on-write when the rectangles are tile-aligned. It has `cow_rect` logic in [gegl-buffer-access.c](https://gitlab.gnome.org/GNOME/gegl/-/blob/master/gegl/buffer/gegl-buffer-access.c) and `gegl_tile_dup` in gegl-tile.c.
- **Projection [V].** `GimpProjection` renders invalidated areas incrementally in **32×32 chunks** (`GIMP_PROJECTION_UPDATE_CHUNK_WIDTH 32`). It uses a *priority rect* (the visible area) so on-screen chunks render first ([gimpprojection.c](https://gitlab.gnome.org/GNOME/gimp/-/blob/master/app/core/gimpprojection.c)). The projection buffer keeps a mip pyramid; the code comment reads "The pyramid levels constitute a geometric sum with a ratio of 1/4".
- **Display [V].** The display shell renders scaled chunks with cairo on the CPU ([gimpdisplayshell-render.c](https://gitlab.gnome.org/GNOME/gimp/-/blob/master/app/display/gimpdisplayshell-render.c)).

**Photopea**
- **[V]** From version 1.3, "Pixel data of each layer are stored in WebGL memory". WebGL handles "blending (for all blend modes)" and masks. The blog's benchmark: a 2048×1152, 10-layer redraw takes 850 ms on CPU vs 55 ms with WebGL ([Photopea 1.3 blog](https://blog.photopea.com/photopea-1-3.html)).
- **[V]** Adjustment layers (Levels, Curves, Hue/Sat, and others) "are processed by the GPU in WebGL mode" ([1.4 blog](https://blog.photopea.com/photopea-1-4.html)).
- **[U]** No public detail on its tiling.

**Photoshop**
- **[P]** Adobe's "Memory & performance" help page says you can set **up to 8 cache levels** (pre-computed downsampled copies used for fast redraw at low zoom) and choose one of **four cache tile sizes** ([helpx: memory-performance](https://helpx.adobe.com/photoshop/using/memory-performance.html)). That page returned 403 to my fetch, so this is from search snippets only.
- **[U]** Internals are proprietary. Photoshop is a CPU tile engine with GPU-accelerated display and some filters.

**Takeaways**
1. Everyone tiles: 64 (Krita, libmypaint), 128 (GEGL), or a configurable size (Photoshop).
2. Everyone keeps a merged **projection** plus a **mip/LOD pyramid** for zoomed-out display.
3. Only dirty rects are recomposited, with visible regions prioritised.
4. Undo is tile COW (Krita explicitly).
5. Web-era editors (Photopea) moved compositing and adjustments to the GPU for 60 fps.

### 1.2 Recommendation

**Tile size: 128×128 for storage, undo, and hashing.**
- At 8 bit this is 64 KiB per RGBA8 tile, 16 KiB per R8 mask tile, and 128 KiB per RGBA16F tile.
- 128 is a good middle ground [U]:
  - 64 creates 4× more map entries and hashes. An 8K layer is about 8,100 tiles at 64 px vs about 2,040 at 128 px.
  - 256 makes COW and undo of small brush dabs copy 4× more bytes, and makes content-addressed dedupe coarser.
- Store **uniform tiles** (fully transparent, or a single colour) as a tag rather than pixels. Krita has the equivalent "default pixel" concept [U for details]. AI-generated layers placed on a big canvas are mostly empty, so this matters.
- Tile coordinates are `i32` pairs, which gives an unbounded or infinite canvas. The document "canvas rect" is just metadata; crop changes the rect, not the pixels.

**Pixel formats**

| Use | M1 format | Later |
|---|---|---|
| Layer pixels | **RGBA8**, sRGB-encoded, straight alpha on disk; premultiply on upload or in shader | RGBA16F linear (16/32-bit docs) |
| Masks, selections | **R8** (alpha8, like Krita) | R16 if needed |
| GPU projection (merged) | **RGBA16F**, premultiplied | same |
| GPU display intermediate | QQuickRhiItem colour buffer RGBA8 (default) or RGBA16F | RGBA16F for HDR |

Why 8-bit sRGB-encoded storage in M1 [U]:
- ComfyUI outputs and most inputs are 8-bit sRGB PNG/JPEG.
- It halves RAM and VRAM compared with half-float.
- Users expect Photoshop-style blend-mode results. Photoshop and Krita blend 8-bit documents in their gamma-encoded space; Photoshop's "blend RGB colors using gamma 1.0" is an opt-in setting [P].
- Compositing still happens in an **RGBA16F accumulator**, which avoids repeated quantisation and banding across 20 layers.

Plan the type system for multiple formats now: `enum TileFormat { Rgba8Srgb, Rgba16fLinear, R8 }`. Retro-fitting this later is the expensive part.

**GPU vs CPU compositing: composite on the GPU for display, and keep a CPU reference compositor in Rust.**
- The GPU path gives 60 fps with 20+ layers, blend modes, masks, adjustment layers and transform previews. Photopea measured roughly a 15× speedup by moving the same work to WebGL [V].
- The CPU path covers export, thumbnails, "merge down", headless batch work, and **golden tests** that check GPU output against CPU output within a tolerance.
- Blend-mode formulas: use the [W3C Compositing and Blending Level 1](https://www.w3.org/TR/compositing-1/) spec for separable modes (multiply, screen, overlay, darken, lighten, color-dodge/burn, hard/soft-light, difference, exclusion) and non-separable modes (hue, saturation, color, luminosity). That way the CPU and GPU implementations share one spec.

**Dirty tracking [U]**
- Each edit produces a `DirtySet`: a set of tile coordinates per layer. The compositor dirties the same tile coordinates in the projection, because all point-wise ops are 1:1.
- Layer transforms or moves dirty the union of the old and new bounds.
- Layer-property changes dirty the layer's whole extent: opacity, blend mode, visibility.
- Neighbourhood ops (blur, layer effects) expand by the kernel radius, which is Krita's "need rect" idea.
- The GPU side keeps a per-projection-page dirty bitmap. Each frame it recomposites dirty tiles, **visible ones first**, which is GIMP's priority rect. It caps work per frame and defers the rest.

**Memory budget: 8K × 20 layers (computed)**

| Item | 7680×4320 (33.2 MP) | 8192×8192 (67.1 MP) |
|---|---|---|
| 1 RGBA8 layer, full coverage | 133 MB | 268 MB |
| 20 RGBA8 layers, full coverage | **2.65 GB** | 5.37 GB |
| 20 RGBA16F layers, full coverage | 5.31 GB | 10.7 GB |
| 1 R8 mask | 33 MB | 67 MB |
| RGBA16F projection + mip chain (×4/3) | ~354 MB | ~716 MB |
| Tiles per full layer @128² | 2,040 | 4,096 |

Recommended defaults [U]:
- **RAM:** tile store budget is about 50% of physical RAM, matching Krita's default. Beyond that, LRU-evict *clean* tiles, which already live in the CAS on disk, and compress or spill undo tiles.
- **VRAM:** tile-cache budget of about **1.5–2 GB by default, configurable**, not "everything resident".
  - The test machine's GPU has 16 GB, but **ComfyUI runs on the same GPU** and SDXL/Flux-class models want most of it.
  - Add a hook that trims the canvas cache when a ComfyUI job starts. ComfyUI also has its own `free` endpoint (the MCP server exposes `free_memory`).
- Real documents are sparse, so the full-coverage numbers are worst cases.

---

## 2. GPU rendering in Qt Quick from Rust

### 2.1 QQuickRhiItem facts

- **[V]** Added in **Qt 6.7**. It is the QRhi-based replacement for QQuickFramebufferObject ([docs](https://doc.qt.io/qt-6/qquickrhiitem.html)).
- **[V]** Threading:
  - `QQuickRhiItemRenderer::synchronize()` is called "on the render thread while the GUI thread is blocked".
  - The item and the renderer must not share variables; communication goes through `synchronize()` or queued signals.
  - `initialize()` and `render()` run on the render thread.
- **[V]** The colour buffer is a texture that follows the item size by default. `colorBufferFormat` can be RGBA8 (default), RGBA16F, RGBA32F, or RGB10A2.
- **[V]** `alphaBlending` is false by default, and premultiplied alpha is expected.
- **[V]** `effectiveColorBufferSize` updates asynchronously, so use the render target's size instead.
- **[V]** It does not work with the software adaptation.
- **[V]** Windows defaults:
  - The default RHI backend is **Direct3D 11**, and the default render loop on Windows/D3D11 is **threaded** ([scenegraph.qdoc](https://github.com/qt/qtdeclarative/blob/dev/src/quick/doc/src/concepts/visualcanvas/scenegraph.qdoc)).
  - `QSG_RHI_BACKEND=d3d12` selects D3D12.
- **[V]** The QRhi API has *limited compatibility guarantees*: no source or binary compatibility across minor releases ([QRhi docs](https://doc.qt.io/qt-6/qrhi.html)). Pinning **Qt 6.12 LTS** (released 2026-09-30; standard support until 2031) suits this well ([Qt 6.12 blog](https://qt.io/blog/qt-6.12-released)).
- **[V]** QRhi supports compute shaders and image load/store where `QRhi::Compute` is reported, which includes D3D11, D3D12 and Vulkan ([QRhi docs](https://doc.qt.io/qt-6/qrhi.html)).
- **[V]** Shaders are written once in Vulkan-style GLSL and compiled by `qsb` / `qt_add_shaders()` to SPIR-V plus HLSL/MSL/GLSL. `.comp` compute shaders are supported ([qsb docs](https://doc.qt.io/qt-6/qtshadertools-qsb.html)).
- **[V]** Texture formats include RGBA8, R8, R16, RGBA16F, RGBA32F, R16F, R32F, and RGB10A2, plus integer formats in 6.9/6.10. Flags include `MipMapped`, `UsedWithGenerateMips`, `UsedWithLoadStore`, `TextureArray` (via `QRhi::newTextureArray`, gated by `QRhi::TextureArrays`), and `ThreeDimensional` ([QRhiTexture](https://doc.qt.io/qt-6/qrhitexture.html)).

### 2.2 Texture upload strategy

**What the docs say [V]** ([QRhiResourceUpdateBatch](https://doc.qt.io/qt-6/qrhiresourceupdatebatch.html), [QRhiTextureSubresourceUploadDescription](https://doc.qt.io/qt-6/qrhitexturesubresourceuploaddescription.html)):
- `uploadTexture(QRhiTexture*, QRhiTextureUploadDescription)` uploads any number of `(layer, mipLevel, subresource)` entries.
- A subresource takes raw bytes via `setData(QByteArray)`, with `setDestinationTopLeft` and `setSourceSize` for **sub-rect updates**.
- `setDataStride` sets the row pitch. It needs `QRhi::ImageDataStride`, which is available everywhere except GLES2.
- If the source data is not owned, it must stay valid until `endFrame()`.
- `hasOptimalCapacity()` tells you when to split batches.
- `generateMips()` needs `MipMapped|UsedWithGenerateMips`, and is not guaranteed for every format.
- Readback (`readBackTexture`) is asynchronous and returns one mip of one layer per call.

**Recommended scheme [U]:**
1. **Preallocate once** in `initialize()`. Creating textures per frame is a known stall source.
   - Layer tile pool: `newTextureArray(RGBA8, N_slots, 128×128)`. Allocate in chunks of, for example, 1024 slots (128 MiB each) up to the VRAM budget. Use an R8 array for masks.
   - Check `QRhi::TextureArrays` and the maximum array size at runtime. D3D11's limit is 2048 slices [U, general D3D knowledge].
   - Slot allocator: `HashMap<(LayerId, TileCoord), Slot>` with LRU eviction.
2. **Projection pages:** RGBA16F 2048×2048 textures with a full mip chain, flagged `UsedWithLoadStore | MipMapped | UsedWithGenerateMips`. Validate this flag combination on D3D11 at startup [U].
   - The page grid covers the canvas, which keeps the infinite canvas possible.
   - An 8K document needs 4×3 pages. A simpler alternative for documents up to 16384 px is a single big texture; QRhi reports `TextureSizeMax`, typically 4096–16384 [V].
3. **Per frame:**
   - In `synchronize()`, swap in a pre-built `RenderPacket` from Rust: dirty tile list, layer table, view transform, overlays. This is O(1); the GUI thread is blocked here [V], so do no copying.
   - In `render()`:
     1. Build one update batch. Add one `QRhiTextureUploadDescription` with an entry per dirty tile, targeting its array layer.
     2. Cap uploads per frame, e.g. 32–64 MB, visible tiles first.
     3. Run a compute pass that composites the dirty projection tiles.
     4. Call `generateMips` for touched pages.
     5. Run the render pass that draws visible pages.
   - Data handoff: wrap the Rust `Arc<[u8]>` with `QByteArray::fromRawData` and keep the Arcs alive in the renderer until the next frame. That keeps the "valid until endFrame" rule [V rule / U technique]. Otherwise accept one `memcpy`; 64 KiB tiles make that cheap.
4. **Brush bandwidth is small.** A 500 px brush stroke dirties roughly 4–16 tiles per frame, under 1 MB at RGBA8. PCIe bandwidth is not the bottleneck; draw-call and pass overhead are.
5. **Pan and zoom never recomposite.** They only change the view uniform used to sample projection pages. Use the mip chain plus trilinear sampling when zoomed out, and nearest-neighbour above 200% zoom so pixel artists see real pixels.
6. **Seams:** compositing is 1:1 texel-to-texel and needs no borders. Only the display sampling of projection pages does. Either add an apron to pages, as Krita does with a `1<<mipLevels` border [V], or sample across page edges in the shader by clamping to inner texels and accepting tiny seams at very low zoom.

**QQuickRhiItem vs QSGRenderNode [U].** QQuickRhiItem renders into its own texture, which Qt Quick then composites. That is one extra full-viewport copy, about 33 MP/s at 4K60, which is trivial on a 16 GB consumer GPU. QSGRenderNode draws directly into the window's pass and saves that copy, but its API is harder. Start with QQuickRhiItem.

**C++ shim boundary [U].**
- The cxx-qt 0.10 changelog (2026-08-24) covers JSON types, item-model enums and image-provider bindings (0.9). It mentions no QQuickItem or QQuickRhiItem subclass support ([releases](https://github.com/KDAB/cxx-qt/releases)) [V for changelog contents].
- `QQuickRhiItemRenderer` is a non-QObject class with pure virtuals, so write `CanvasRhiItem` and `CanvasRhiItemRenderer` in C++. They should hold a `rust::Box<CanvasHandle>` and call `extern "Rust"` functions through `cxx`.
- Keep QML-facing models (layer list, tool settings) as cxx-qt QObjects.

### 2.3 GPU vs CPU compositing

**Recommendation: composite on the GPU with QRhi `qsb` shaders.** CPU compositing then uploading the projection means every brush dab costs `layers × dirty tiles` CPU blend work, and every blend-mode or opacity slider tick recomposites the whole canvas on the CPU. That works for about 5 layers but not for 20 layers at 8K and 60 fps. Krita and GIMP get away with CPU compositing by recompositing dirty rects only; their slider interactions on big documents are slow.

**Compositor shape [U]:**
- Use **one compute dispatch per batch of dirty projection tiles**. Each thread loops bottom→top over the layer list for its tile:
  1. Read the layer's tile slot from the texture array; slot indices come from a storage buffer.
  2. Read the mask slot.
  3. Apply opacity, the blend mode (switch on a mode ID), and clipping.
  4. Write once to the projection page with `imageStore`.
- No ping-pong targets are needed, because blending happens in registers.
- **Isolated groups** composite into a temporary tile, which then acts as a layer. Pass-through groups simply inline their children.
- **Adjustment layers** are entries in the same loop: apply a point-op to the accumulator so far (§7).
- Keep a fragment-shader fallback that renders a full-tile quad with the same loop, in case some format or flag combination isn't load/store-capable.
- **Transform preview:** during a free transform, sample the transformed layer's tiles with the inverse matrix in the display pass. Commit with a high-quality CPU resample when the user releases.

### 2.4 Alternatives

**wgpu interop: possible on paper, not recommended for M1.**
- **[V]** wgpu 30.0.1 exposes `Device::as_hal::<A>()`, `create_texture_from_hal` and `create_buffer_from_hal` ([docs.rs](https://docs.rs/wgpu/latest/wgpu/struct.Device.html)).
- **[V]** wgpu-hal's dx12 `Device` exposes `raw_device()`, `raw_queue()`, `texture_from_raw()` and `buffer_from_raw()` ([wgpu-hal/src/dx12/device.rs](https://github.com/gfx-rs/wgpu/blob/trunk/wgpu-hal/src/dx12/device.rs)).
- **[V]** On the Qt side, `QRhiD3D12NativeHandles` can import an existing `ID3D12Device` *and* `ID3D12CommandQueue` ([docs](https://doc.qt.io/qt-6/qrhid3d12nativehandles.html)). `QQuickGraphicsDevice::fromRhi()` (6.6) hands such a QRhi to Qt Quick ([docs](https://doc.qt.io/qt-6/qquickgraphicsdevice.html)). `QRhiTexture::createFrom()` wraps native textures, and `setNativeLayout()` communicates the D3D12 resource state ([docs](https://doc.qt.io/qt-6/qrhitexture.html)).
- **[U]** So a wgpu-created D3D12 device and queue *could* be shared with Qt Quick running on D3D12, with zero-copy textures. Downsides:
  - D3D12 is not Qt's default backend.
  - You have to coordinate resource-state tracking between two engines on one queue.
  - The `windows` crate version must match wgpu-hal.
  - It's all `unsafe`.
  - I found no public example. A wgpu feature request for native shared textures was closed as not planned ([gfx-rs/wgpu#274](https://github.com/gfx-rs/wgpu/issues/274)).
- Revisit only if you adopt Vello, Graphite nodes, or rust-gpu shaders.

**Vello [V]** ([repo](https://github.com/linebender/vello); crates `vello` 0.11.0 and `vello_cpu` 0.3.0, both 2026-10-02):
- It's a 2D *vector* renderer. The README calls `vello_cpu` "the most mature choice". The GPU path is moving to a hybrid renderer, and the compute renderer is "experimental".
- Graphite's GSoC notes say Vello "is only well suited for composing vector elements and plain raster images", and lacks raster ops like hue/sat ([discussion #2658](https://github.com/GraphiteEditor/Graphite/discussions/2658)) [P].
- Use: possibly later for vector or text layers rasterised into tiles. Not as the raster compositor.

**Skia (`skia-safe` 0.153.3, Skia m155) [V]** ([rust-skia](https://github.com/rust-skia/rust-skia)):
- MIT licensed, with prebuilt Windows MSVC binaries, and `gl`, `vulkan`, `d3d` and `metal` features.
- Full blend-mode set and a fast CPU raster [U for perf claims].
- Heavy dependency. It would be a third GPU context next to Qt's QRhi.
- Use: optional for text and vector layers later. Not needed for M1.

**tiny-skia 0.12.0 [V]** ([repo](https://github.com/linebender/tiny-skia)):
- CPU only, RGBA8888 only, no text. The README says it is about 20–100% slower than Skia on x86-64. BSD-3.
- It has `Mask::fill_path` and `intersect_path` ([docs.rs](https://docs.rs/tiny-skia/latest/tiny_skia/struct.Mask.html)), which suits **rasterising lasso and rect selections with anti-aliasing**. Recommended for that.

**`image` 0.25.10 [V]:**
- `imageops` has only `overlay` and `Pixel::blend`, which is normal alpha-over done per pixel in float ([color.rs](https://github.com/image-rs/image/blob/main/src/color.rs)). There are no blend modes.
- Use it for decode/encode only. It already depends on **moxcms** [V, crates.io reverse deps].

**fast_image_resize 6.1.0 [V]** ([repo](https://github.com/Cykooz/fast_image_resize)):
- SIMD resize for U8x4, U16x4 and F32x4, with an AVX2 path, and dual MIT/Apache license.
- The author's benchmark: Lanczos3 4928×3279→852×567 at 13.3 ms vs 174 ms for `image`.
- Use it for CPU mip generation, thumbnails, transform-commit downscales, and scaling ComfyUI inputs.
- It does *not* linearise for you; it provides sRGB mappers.

**Qt Canvas Painter (6.12) [V]:**
- It left Technology Preview in 6.12 and is "an imperative painter-like rendering API ... that uses the GPU" via QRhi ([blog](https://qt.io/blog/qt-6.12-released), [what's new](https://doc.qt.io/qt-6/whatsnew612.html)).
- [U] Candidate for overlay UI drawn over the canvas: transform handles, rulers, guides. Not for raster compositing.

---

## 3. Brush engines

### 3.1 libmypaint [V]
- **License:** ISC (permissive; [COPYING](https://github.com/mypaint/libmypaint/blob/master/COPYING)).
- **Maintenance:** The last release is **v1.6.1 (2020-05-11)**. A 2.0.0 line exists only as alpha/beta pre-releases from 2020. Commits still land occasionally; the latest was 2026-04-14, a Windows configure fix ([GitHub API](https://github.com/mypaint/libmypaint)). In practice it is in **low-activity maintenance**.
- **Build:** autotools. json-c is required, and GLib/GObject-Introspection are optional only via `--without-glib --disable-introspection` ([README](https://github.com/mypaint/libmypaint)). There is no CMake build and no MSVC instructions, so you'd have to compile it with the `cc` crate yourself [U].
- **Data model:** `MYPAINT_TILE_SIZE 64` tiles of `uint16` RGBA, premultiplied, in 15-bit fixed point (`mypaint-config.h`, `mypaint-tiled-surface.h`). You can implement the `MyPaintSurface` `draw_dab` callback instead, which avoids the format mismatch [U].
- **From Rust:** feasible with bindgen and a `-sys` crate. I found no maintained `libmypaint-sys` crate [P]. Pure-Rust options:
  - **`hokusai`** 0.3.0 (MIT/Apache; [reearth/hokusai](https://github.com/reearth/hokusai)). It is very new: created 2026-05, 16 stars. It claims .myb compatibility, with 137 of 196 stock brushes matching libmypaint 1.6.1 [P, README via search].
  - `rustport`, an abandoned partial port.
- **Verdict:** Don't take libmypaint into M1. Revisit for "M2: natural media" either as a vendored C build behind a `draw_dab` adapter or via hokusai if it matures.

### 3.2 Tablet input in Qt 6 on Windows

- **[V]** Qt 6's Windows plugin uses **Pointer Input Messages (WM_POINTER, i.e. Windows Ink)** by default. `-platform windows:nowmpointer` switches to legacy mouse handling ([QGuiApplication docs](https://doc.qt.io/qt-6/qguiapplication.html)).
- **[V]** Pen pointers become `QTabletEvent`s (`translatePenEvent` → `QWindowSystemInterface::handleTabletEvent` in [qwindowspointerhandler.cpp](https://github.com/qt/qtbase/blob/dev/src/plugins/platforms/windows/qwindowspointerhandler.cpp)).
  - **Important:** when tablet compression is off, which is the default (`AA_CompressTabletEvents` defaults to false [V, qnamespace.qdoc]), Qt calls `GetPointerPenInfoHistory` and **dispatches every coalesced history frame**.
  - So you get the full device rate, e.g. **200 pps on Wacom Intuos Pro** ([Wacom device specs](https://developer-support.wacom.com/hc/en-us/articles/9354492149527-Wacom-Device-Specifications)) [P].
- **[V]** WinTab is still supported. It can be toggled at runtime through the private native interface `QWindowsApplication::setWinTabEnabled(bool)` ([qwindowsapplication.cpp](https://github.com/qt/qtbase/blob/dev/src/plugins/platforms/windows/qwindowsapplication.cpp)). This is private API (`QNativeInterface::Private`).
  - Offer it as a setting ("Tablet API: Windows Ink / WinTab"), as Krita does [U for Krita's UI].
  - [U] Wacom drivers need "Use Windows Ink" enabled for pressure via WM_POINTER.
- **[V]** QTabletEvent gives `pressure` (0..1), `xTilt`/`yTilt` (±60°), `rotation`, `tangentialPressure`, and float `globalPosF` precision ([QTabletEvent](https://doc.qt.io/qt-6/qtabletevent.html)). That page's "Windows needs wintab32.dll" note predates the WM_POINTER path.
- **Qt Quick delivery [V/U]:**
  - `QQuickDeliveryAgent` delivers TabletPress/Move/Release as pointer events to **Pointer Handlers** (e.g. `PointHandler { acceptedDevices: PointerDevice.Stylus }`). The source reads "visits HoverHandlers too". Unaccepted tablet events turn into synthesized mouse events ([qquickdeliveryagent.cpp](https://github.com/qt/qtdeclarative/blob/dev/src/quick/util/qquickdeliveryagent.cpp)).
  - From reading the code, plain QQuickItem subclasses receive *mouse* events (no pressure) unless a handler takes the tablet event [U, inferred from source].
- **Recommendation [U]:**
  - Install a C++ **event filter on the QQuickWindow**. When `QEvent::TabletPress/Move/Release` hits the canvas item's scene rect, map it with `item->mapFromScene(ev->scenePosition())`.
  - Push `{pos_f64, pressure, tilt, rotation, timestamp, pointer type (pen/eraser), buttons}` into a lock-free SPSC queue to the Rust brush worker, then accept the event. Accepting suppresses mouse synthesis.
  - Don't route samples through QML/JS properties.
  - Use mouse events as a fallback, with pressure fixed at 1.

### 3.3 Rust brush crates [V where linked]
- `hokusai` (above): MyPaint-style and young.
- Graphite moved its brush to a GPU "Basic Brush" node in Aug 2026 (wgpu, R16Float density plus Rgba16Float composite; [PR #4469](https://github.com/GraphiteEditor/Graphite/pull/4469), merged 2026-08-27). It is MIT/Apache, but tied to Graphite's node runtime. Worth reading, not reusing.
- `rnote` (handwriting/sketching) is **GPL-3.0** ([repo](https://github.com/flxzt/rnote)). It is incompatible with closed or permissive distribution; study it only.
- There is no established, general Rust raster brush crate. **Write your own M1 brush.** It is a few hundred lines.

### 3.4 M1 "solid basics" brush (recommended spec) [U]

- **Dab model:**
  - Round dabs with **hardness**: a radial falloff, e.g. `smoothstep(r_hard, r, d)`, analytically anti-aliased at sub-pixel positions.
  - Optional **texture tip**: a grayscale stamp image sampled bilinearly, with rotation following stroke direction or pen rotation.
- **Spacing:** dabs every `spacing × diameter` along the arc length, with a default of 10–15%. Interpolate position, pressure and tilt between input samples, using Catmull-Rom on positions to avoid polygonal strokes at speed.
- **Dynamics:** pressure → size, opacity and flow, each with an editable **curve** (a 1D LUT). Add a minimum size %. Tilt and rotation can come later.
- **Smoothing:** pick one per tool:
  - exponential moving average, or
  - "pulled string" / lazy-mouse stabiliser with a radius.
- **Opacity vs flow semantics:**
  - Dabs accumulate *into a per-stroke R16 coverage buffer* at `flow`.
  - The stroke is composited onto the layer with `min(coverage, opacity)`, i.e. Krita's "wash" / Photoshop opacity cap.
  - Show the stroke live as a *stroke overlay layer* in the compositor (no layer writes until stroke end).
  - At stroke end, commit into the layer tiles in a single undo step.
- **Eraser:** the same dab pipeline applied as destination-out on alpha. Pen-eraser end auto-selects it.
- **Colour:** paint in the layer's encoding (sRGB 8-bit for M1), with internal coverage math in f32.
- **Performance:** a dab of up to 512 px is about 0.26 MP. Rasterise with SIMD and `rayon` per tile. For very large brushes (>1000 px), use a Krita-style LOD preview: draw at ½ or ¼ resolution live and full resolution in the background.

---

## 4. Undo/redo for large raster edits [U, modelled on Krita V]

- **Representation:**
  - A layer is a persistent map `TileCoord → Arc<Tile>`, e.g. `im::HashMap` or `rpds::HashTrieMap`. I didn't evaluate these crates in depth.
  - Writes go through `Arc::make_mut`, so a tile shared with any snapshot is cloned on first write. That is the Krita memento COW semantic [V].
- **Undo step:**
  - Store `(layer_id, old_map_root)`, or just the list of `(coord, old Arc<Tile>)` for touched tiles, plus non-pixel metadata deltas (opacity, blend mode, order, transforms).
  - Memory per step = only the tiles actually overwritten. A brush stroke costs a few MB. A full-layer adjustment on 8K RGBA8 costs about 133 MB.
- **Budget:**
  - Default **undo RAM budget of about 2 GB**, plus a step cap (Krita's default is 200 [V]).
  - When the budget is exceeded, (a) compress old tiles (lz4/zstd) and (b) spill them to the **content-addressed tile store** on disk, keyed by tile hash. Spilling an already-hashed tile is a dedup no-op, so the CAS doubles as the swap file.
  - Steps past the cap get dropped. Their tiles are reference-counted out of the CAS by a garbage collector.
- **Content-addressed layer files:** hash tiles lazily, on idle, save, or spill. A saved document is then `layer → [tile hash]` manifests plus a tile pack. Incremental save writes only new hashes, and history and versions share tiles for free.
- **Cancellation:** a stroke in progress lives in its own overlay buffer (§3.4), so cancelling it is free.

---

## 5. Colour management

**Working space (M1) [U]:**
- Documents are tagged with an ICC profile. The default is sRGB IEC61966-2.1. Imported files keep their embedded profile, with the option to convert on import.
- 8-bit sRGB-encoded storage. Compositing happens in a float accumulator; blending stays gamma-encoded by default (§1.2).
- Add a "linear blending" document option later, together with RGBA16F linear documents. That is the right base for HDR and scene-referred work.

**moxcms 0.9.1 [V]** ([repo](https://github.com/awxkee/moxcms), [docs.rs](https://docs.rs/moxcms)):
- Pure Rust, BSD-3/Apache-2.0.
- It parses ICC profiles (`new_from_slice`), creates profiles from CICP, has built-in sRGB, Display P3, BT.2020 PQ/HLG and ACEScg profiles, and supports `RenderingIntent`.
- Transforms are available for **8/10/12/16-bit, f32 and f64**, including in-place variants.
- The `image` crate now depends on it.
- Use it for:
  - import/export conversions on the CPU;
  - building the **display 3D LUT**: sample a 33³ or 65³ grid from document space → monitor profile and upload it as a `ThreeDimensional` RGBA16F QRhiTexture [V flag exists], then apply it in the display shader after compositing.

**Qt and display colour management:**
- **[V]** `QColorSpace` handles RGB/Gray ICC profiles, including element-list (LUT) transforms since 6.8. It covers sRGB, Display P3, BT.2020, PQ and HLG ([QColorSpace](https://doc.qt.io/qt-6/qcolorspace.html)). `QImage` has `convertToColorSpace` and `colorTransformed` (6.4/6.8) and float formats `RGBA16FPx4`/`RGBA32FPx4` (6.2) ([QImage](https://doc.qt.io/qt-6/qimage.html)).
- **[V]** New in 6.12 on Windows: "the platform plugin now derives each screen's color space from its ICC profile" ([whatsnew612](https://doc.qt.io/qt-6/whatsnew612.html)).
- **[V]** That colour space is exposed via `QPlatformScreen::colorSpace()`, which is **private QPA API**. I found no public `QScreen::colorSpace()` in `qscreen.h` on `dev`.
- **[U]** Qt Quick does *not* colour-manage scene-graph content. You must apply the display transform yourself in the canvas shader.
- **Recommendation:**
  - Get the monitor ICC profile either via the private `QPlatformScreen::colorSpace().iccProfile()` from the C++ shim, or directly via the Windows Color System API (`ColorProfileGetDisplayDefault` etc., which Qt itself uses [V, qwindowsscreen.cpp]).
  - Rebuild the LUT on `QWindow::screenChanged`.
  - Feed the ICC bytes to moxcms.

**HDR/scRGB in Qt 6.12 on Windows:**
- **[V]** QRhi swapchains support `HDRExtendedSrgbLinear` (scRGB), `HDR10` and `HDRExtendedDisplayP3Linear`, with `QRhiSwapChainHdrInfo` since 6.6 ([docs](https://doc.qt.io/qt-6/qrhiswapchainhdrinfo.html)). This is in the private RHI header.
- **[V]** Qt Quick picks the swapchain format from the **`QSG_RHI_HDR`** env var (`scrgb`, `hdr10`, `p3`). Without it, Qt Quick reads the undocumented window property `_qt_sg_hdr_format`, and falls back to SDR if the screen isn't HDR-capable ([qsgrhisupport.cpp `applySwapChainFormat`](https://github.com/qt/qtdeclarative/blob/dev/src/quick/scenegraph/qsgrhisupport.cpp)). I found no public documentation for either switch.
- **[U]** Turning it on makes the **whole window** scRGB. Qt Quick's ordinary 2D UI (Text, Rectangles, images) is then written into a linear swapchain without conversion, so the UI looks wrong unless you compensate. There's no supported per-item HDR.
- **Verdict:** **not worth doing now.** Keep the pipeline HDR-ready: an RGBA16F projection, a separate display-transform stage, and a `colorBufferFormat` option. Ship SDR plus ICC display management in M1. Revisit HDR once Qt documents public HDR support in Qt Quick.

---

## 6. Selections and masks [U unless noted]

- **Representation:**
  - The selection is an R8 tile layer (alpha8), like Krita [V].
  - Layer masks are R8 tile layers attached to a layer.
  - Both sit in the same tile store, so they get COW, undo and the CAS for free.
- **Operations:**
  - **Rect and ellipse:** analytic coverage per pixel.
  - **Lasso and polygon:** rasterise the path with anti-aliasing via `tiny_skia::Mask::fill_path` [V method exists], working per tile region or on a bounding-box buffer, then split into tiles.
  - **AI masks** (SAM or similar via ComfyUI) arrive as grayscale PNG. Decode, resample to document space with fast_image_resize, and write R8 tiles.
  - **Boolean modes** (replace, add, subtract, intersect) are per-pixel `max`, `a*(1-b)`, and `min`/`mul`, all tile-local.
  - **Feather:** a separable Gaussian on R8, or 3× box blur, with a tile apron equal to the radius. Krita-style "need rect". Run it on CPU workers with rayon. The GPU can preview it for large radii.
  - **Grow/shrink:** max/min filters.
  - **Select by colour / magic wand:** a CPU flood fill over tiles.
- **Marching ants:**
  - Krita computes a vector outline (`KisOutlineGenerator` → `QPainterPath`) [V]. That costs CPU on big masks and needs recomputing on every change.
  - **Recommended for M1:** draw the ants **in the canvas display shader**. Sample the selection mask, which is mirrored into an R8 texture-array/page set with mips, at the screen pixel and its neighbours. Where it crosses 0.5, output an animated diagonal dash pattern, `fract((x + y + t·speed) / period)`.
  - This is resolution-independent, O(screen pixels), and updates instantly on edits. Use a 1–2 device-pixel stroke.
  - Keep a marching-squares outline in Rust only if you need path export ("selection to path").
- **Quick-mask view:** a red overlay is the same shader path.

---

## 7. Adjustments [U]

- **M1: build non-destructive adjustment layers, limited to point ops.** Levels, curves, hue/sat, brightness/contrast, exposure and invert all depend only on the pixel itself. In the per-tile compositor loop (§2.3), each one is just a function applied to the accumulator, with an optional R8 mask and opacity.
  - Cost: one more loop iteration. No caching, no apron.
  - Photopea does exactly this on the GPU [V].
  - Doing destructive-only versions first would mean building "preview on GPU, apply on CPU" plumbing anyway, so going non-destructive costs little.
- **Parameter encoding:**
  - **Levels and curves:** bake each to a 1D LUT. Use 256 or 1024 entries × RGBA (master plus per-channel) in an R16F/RGBA16F 1D-or-2D texture, so one sampler covers every curve layer. Store control points in the document; the LUT is derived.
  - **Hue/sat/lightness:** use Photoshop-like HSL math in the shader. Brightness/contrast uses a uniform-block formula.
  - Define the formulas once in Rust and mirror them in GLSL. Golden tests compare CPU vs GPU on test images within ±1/255.
- **Destructive "Apply" / "Merge down":** run the CPU reference for exactness and write tiles in one undo step. For speed, render on the GPU and read back asynchronously with `readBackTexture`, one mip and one layer per call [V].
- **Neighbourhood filters** (Gaussian blur, unsharp mask, noise reduction): destructive only in M1. Run them on the CPU with aprons, or on the GPU with readback, and show a progress preview. Non-destructive "smart filters" wait for M2, because they need need-rect propagation and cached intermediate results.

---

## 8. Performance targets and Qt Quick pitfalls

**Targets [U]**

| Metric | Target |
|---|---|
| Pan/zoom/rotate | Locked to display refresh (60/120/144 Hz). GPU canvas pass < 2 ms at 4K. Zero recomposites. |
| Brush input→photon | ≤ 2 frames at 60 Hz (~33 ms), ideally ~1.5 frames. Dab rasterisation < 1 ms per input sample for ≤ 500 px brushes. |
| Tablet sampling | Process every sample (200 pps Wacom Pro [P]). Never compress. |
| Dirty recomposite | ≤ 4 ms per frame budget. Visible tiles first; remainder deferred. |
| Layer property slider (opacity, blend mode, curves) | Interactive at 60 fps on 8K × 20 layers: the full-canvas recomposite of visible pages fits in a few ms on a 16 GB consumer GPU [U]. |
| Open 8K/20-layer doc | First frame from the low-res pyramid in < 1 s; full-resolution tiles stream in. |
| ComfyUI preview frame → screen | < 1 frame after receipt (decode off-thread). |

**ComfyUI live previews [V]:**
- Binary WebSocket frames start with a big-endian u32 event type: `1` = PREVIEW_IMAGE, `4` = PREVIEW_IMAGE_WITH_METADATA.
- For type 1, a u32 image type follows (`1` = JPEG, `2` = PNG), then the encoded bytes.
- Type 4 carries a length-prefixed JSON metadata block before the image ([server.py](https://github.com/comfyanonymous/ComfyUI/blob/master/server.py), [protocol.py](https://github.com/comfyanonymous/ComfyUI/blob/master/protocol.py)).
- Previews are downscaled to `--preview-size` (default **512**), and previews are off unless `--preview-method` is set ([cli_args.py](https://github.com/comfyanonymous/ComfyUI/blob/master/comfy/cli_args.py)).
- [U] Decode on a Rust worker and upload to a single **preview-overlay texture** drawn in the generation region, scaled up. Don't touch the tile store or undo until the final image arrives; then import it as a new layer (one undo step).

**Pitfalls and mitigations**
1. **The GUI thread is blocked during `synchronize()` [V].** Make it an O(1) pointer swap of a prepared `RenderPacket`. Never compute or copy pixels there.
2. **All QRhi calls happen on the render thread.** Rust worker threads must never touch QRhi objects. They produce data, and the renderer consumes it in `render()`.
3. **Don't route high-frequency data through QML.** No per-sample property bindings or JS handlers for strokes, and no QImage/QVariant pixel transfers. Use the C++ event filter → Rust queue (§3.2) and the C++ renderer ← Rust packet path.
4. **Tablet event compression:**
   - Keep `AA_CompressTabletEvents` off (the default) so Qt dispatches pen history frames [V].
   - Accept tablet events so Qt doesn't also synthesise mouse events [V].
   - If the GUI thread stalls (layout, a big model reset), history still arrives later in order, but latency grows. Keep the GUI thread light.
5. **Texture churn:** never create or destroy QRhiTextures per frame. Use texture arrays and pages with slot allocation (§2.2).
6. **Upload spikes:** zooming into new regions or opening documents can queue thousands of tiles. Rate-limit per frame and prioritise by visibility and the current mip level.
7. **The threaded render loop with vsync** adds about one frame of pipeline latency [U]. For the brush, draw the *current stroke's* overlay from the latest samples during `synchronize()`, so the cursor-to-ink gap stays minimal. Optionally predict 1 frame of pen motion for the brush cursor only.
8. **High-DPI:** use device pixels everywhere in the renderer (render target size, not item size [V note]). Map tablet scene positions with the DPR in mind.
9. **QRhi API instability across minor versions [V]:** pin Qt 6.12 LTS and isolate all QRhi use in the C++ shim.
10. **VRAM contention with ComfyUI** on the same 16 GB GPU (§1.2): a budgeted cache, trimmed on job start.
11. **`generateMips` is not guaranteed for every format [V].** Check at startup. If needed, fall back to a tiny compute downsample shader, which also allows **partial** mip updates for only the dirty region (better than regenerating a 2048² page).

---

## 9. Rust raster editors and libraries worth studying or reusing

| Crate / project | Version (crates.io, 2026) | License | Use for us |
|---|---|---|---|
| **moxcms** | 0.9.1 (2026-09-15) | BSD-3/Apache | **Use**: ICC transforms, display LUT [V] |
| **image** | 0.25.10 (2026-03-10) | MIT/Apache | **Use**: decode/encode only; no blend modes [V] |
| **fast_image_resize** | 6.1.0 (2026-07-21) | MIT/Apache | **Use**: SIMD resample, mips, thumbnails [V] |
| **tiny-skia** | 0.12.0 (2026-02-02) | BSD-3 | **Use**: AA path → `Mask` for lasso/rect selections; simple shapes [V] |
| **kurbo** | 0.13.1 (2026-05-13) | MIT/Apache | **Use**: Bézier/path math for lasso smoothing, stroke interpolation, transform handles [V version] |
| **glam** | 0.34.1 | MIT/Apache | Use: matrices for view/transform [V version] |
| **half** | 2.7.1 | MIT/Apache | Use: f16 tiles for 16-bit docs [V version] |
| vello / vello_cpu | 0.11.0 / 0.3.0 (2026-10-02) | MIT/Apache | Later: vector/text layers → tiles [V] |
| skia-safe | 0.153.3 (m155) | MIT (Skia BSD) | Later/optional: text, vector, CPU raster [V] |
| wgpu | 30.0.1 | MIT/Apache | Not in M1; interop only via HAL + D3D12 (§2.4) [V] |
| lcms2 (Rust bindings) | 6.2.0 | MIT (LittleCMS MIT) | Fallback CMS if moxcms gaps appear [V version] |
| hokusai | 0.3.0 (2026-06) | MIT/Apache | Watch: pure-Rust MyPaint-compatible brushes [V crates.io] |
| **Graphite** ([repo](https://github.com/GraphiteEditor/Graphite)) | alpha | MIT/Apache | **Study**: node-based non-destructive model, GPU brush PR #4469, Vello+wgpu integration; not reusable as a library [V] |
| rnote | — | GPL-3.0 | Study only (licence) [V] |
| cxx-qt | 0.10.0 (2026-08-24) | MIT/Apache | Use for QML-facing QObjects; QQuickRhiItem lives in the C++ shim [V] |

Also study these non-Rust references:
- Krita `libs/image/tiles3` (tile COW, mementos, swap) and `libs/ui/opengl` (texture tiles, borders, PBO uploads, mipmaps).
- GEGL buffer (COW tile copies, mip pyramid).
- GIMP `gimpprojection.c` (chunked, prioritised projection rendering).
- Photopea's blog (GPU compositing and adjustment layers).

---

## 10. Suggested M1 implementation order [U]

1. **Tile store and document model in Rust.** 128² tiles, uniform-tile tag, persistent maps, `Arc` COW, undo stack with budget, BLAKE3 hashing, and the CAS save/load manifest.
2. **CPU reference compositor.** All W3C blend modes, opacity, masks, clipping, groups, and point-op adjustments, with golden-image tests.
3. **C++ shim.** `CanvasRhiItem`/`Renderer`, texture-array slot cache, projection pages, compute compositor (same formulas, written as `qsb` GLSL), display pass with the moxcms LUT, and a GPU-vs-CPU golden test harness using offscreen QRhi readback.
4. **Input.** QQuickWindow tablet event filter → SPSC queue → Rust brush worker. Add a stamp brush, eraser, smoothing and pressure curves, with a stroke overlay and commit on release.
5. **Selections.** Rect/ellipse/lasso via tiny-skia, AI mask import, booleans, feather, and shader marching ants.
6. **Transform and crop.** GPU preview with the inverse matrix, CPU commit resample (fast_image_resize / custom bicubic), crop = canvas rect.
7. **Adjustment layers** (levels, curves, hue/sat, brightness/contrast) plus destructive Apply.
8. **ComfyUI.** WS preview overlay, final-image import as a layer, VRAM trim hook.

## Open questions and items to prototype first

- Can QRhi on D3D11 create RGBA16F textures that are `UsedWithLoadStore` and also `MipMapped|UsedWithGenerateMips`? Measure the cost of generating mips on 2048² pages every frame vs a partial-mip compute shader. [U]
- What do texture-array slot counts and upload throughput look like in practice via `QRhiTextureUploadDescription` with hundreds of entries per batch? Watch `hasOptimalCapacity()`. [U]
- Measure input→photon latency with the threaded render loop on a 120/144 Hz display. Try the D3D12 backend and the `QRhiSwapChain` frame-latency settings if it's above target. [U]
- If Photoshop-identical results are a goal, decide the blending policy (gamma vs linear) with real users and Photoshop reference files. [U]
- Check whether a public `QScreen` colour-space accessor appears in a 6.12.x patch. Today it is only `QPlatformScreen::colorSpace()`. [V for current state]
