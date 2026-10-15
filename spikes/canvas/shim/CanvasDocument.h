#pragma once
// CanvasDocument: the immutable test document of the compositing spike.
//
// The layer stack is a flat list, bottom to top, with the same encoding as the Rust reference
// (rust/src/cpu_composite.rs, LayerTile): pixel layers, GROUP_BEGIN/GROUP_END around an isolated
// group (one nesting level on the GPU path) and curves adjustments. Pixel layers are stored as
// 128x128 straight-alpha RGBA8 tiles; a missing tile is fully transparent.
//
// - golden(): the fixed golden scene (Task 3 criterion 1), generated in code from a fixed seed:
//   1024x1024, 6 pixel layers (normal, multiply, screen, overlay), one isolated group of 2 layers,
//   one curves adjustment, partial opacity, sparse layers.
// - procedural(): `--layers N --doc WxH` documents for timing runs. Each layer draws its tiles from
//   a small pool of generated variants, so an 8K x 20-layer document needs ~10 MB of RAM.

#include "spike_shim_global.h"

#include <QSize>
#include <QString>

#include <array>
#include <cstdint>
#include <memory>
#include <vector>

namespace canvas {

constexpr int kTile = 128;
constexpr int kTileBytes = kTile * kTile * 4;
constexpr int kLutBytes = 256 * 4;

enum NodeKind : std::uint8_t { KindPixel = 0, KindGroupBegin = 1, KindGroupEnd = 2, KindCurves = 3 };
enum BlendMode : std::uint8_t { BlendNormal = 0, BlendMultiply = 1, BlendScreen = 2, BlendOverlay = 3 };

struct StackEntry {
    NodeKind kind = KindPixel;
    BlendMode blend = BlendNormal;
    float opacity = 1.0f;
    bool visible = true;
    int pixelLayer = -1;  // KindPixel: index of the pixel layer
    int lut = -1;         // KindCurves: index into luts()
    QString name;
};

class SPIKE_SHIM_EXPORT CanvasDocument
{
public:
    static std::shared_ptr<CanvasDocument> golden();
    static std::shared_ptr<CanvasDocument> procedural(int layers, QSize size, bool withGroup);

    QSize size() const { return m_size; }
    int tilesX() const { return (m_size.width() + kTile - 1) / kTile; }
    int tilesY() const { return (m_size.height() + kTile - 1) / kTile; }
    int tileCount() const { return tilesX() * tilesY(); }
    int pixelLayerCount() const { return m_pixelLayers; }
    const std::vector<StackEntry> &stack() const { return m_stack; }
    const std::vector<std::array<std::uint8_t, kLutBytes>> &luts() const { return m_luts; }
    QString description() const { return m_description; }
    bool isGolden() const { return m_golden; }

    // 128x128 straight-alpha RGBA8 (65536 bytes), or nullptr if the tile is transparent.
    const std::uint8_t *tile(int pixelLayer, int tx, int ty) const;
    // Number of non-empty layer tiles in the whole document.
    qint64 nonEmptyTileCount() const;

private:
    CanvasDocument() = default;

    QSize m_size;
    int m_pixelLayers = 0;
    bool m_golden = false;
    QString m_description;
    std::vector<StackEntry> m_stack;
    std::vector<std::array<std::uint8_t, kLutBytes>> m_luts;

    // golden: one buffer per (layer, tile); empty = transparent.
    std::vector<std::vector<std::uint8_t>> m_tiles;
    // procedural: variant index per (layer, tile), -1 = transparent; and the variant pool.
    std::vector<std::int16_t> m_variantOf;
    std::vector<std::vector<std::uint8_t>> m_variants;  // layer * kVariants + v
};

} // namespace canvas
