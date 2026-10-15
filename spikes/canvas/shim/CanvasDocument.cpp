#include "CanvasDocument.h"

#include <algorithm>
#include <cmath>

namespace canvas {
namespace {

constexpr std::uint64_t kGoldenSeed = 0x5EED2026ull;
constexpr int kVariants = 8;

std::uint32_t hash32(std::uint32_t x)
{
    x ^= x >> 16;
    x *= 0x7feb352du;
    x ^= x >> 15;
    x *= 0x846ca68bu;
    x ^= x >> 16;
    return x;
}

std::uint32_t hash3(std::uint32_t a, std::uint32_t b, std::uint32_t c)
{
    return hash32(a * 0x9E3779B1u ^ hash32(b * 0x85EBCA77u ^ hash32(c + 0x27D4EB2Fu)));
}

struct Rng {
    std::uint64_t s;
    std::uint64_t next()
    {
        std::uint64_t z = (s += 0x9E3779B97F4A7C15ull);
        z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ull;
        z = (z ^ (z >> 27)) * 0x94D049BB133111EBull;
        return z ^ (z >> 31);
    }
    double uniform() { return double(next() >> 11) * (1.0 / 9007199254740992.0); }
    double range(double lo, double hi) { return lo + (hi - lo) * uniform(); }
};

std::uint8_t u8(double v)
{
    return std::uint8_t(std::clamp(std::lround(v), 0L, 255L));
}

struct Rgba {
    double r = 0, g = 0, b = 0, a = 0;
};

struct Circle {
    double cx, cy, r, alpha;
    double cr, cg, cb;
};

std::vector<Circle> makeCircles(Rng &rng, int n, double w, double h, double rMin, double rMax)
{
    std::vector<Circle> out;
    for (int i = 0; i < n; ++i) {
        Circle c;
        c.cx = rng.range(0, w);
        c.cy = rng.range(0, h);
        c.r = rng.range(rMin, rMax);
        c.alpha = rng.range(160, 255);
        c.cr = rng.range(20, 255);
        c.cg = rng.range(20, 255);
        c.cb = rng.range(20, 255);
        out.push_back(c);
    }
    return out;
}

Rgba circlesAt(const std::vector<Circle> &circles, double x, double y, double soft)
{
    Rgba best;
    for (const Circle &c : circles) {
        const double d = std::hypot(x + 0.5 - c.cx, y + 0.5 - c.cy);
        const double cov = std::clamp((c.r - d) / soft, 0.0, 1.0);
        const double a = cov * c.alpha;
        if (a > best.a)
            best = {c.cr, c.cg, c.cb, a};
    }
    return best;
}

std::array<std::uint8_t, kLutBytes> makeLut(int variant)
{
    std::array<std::uint8_t, kLutBytes> lut{};
    for (int i = 0; i < 256; ++i) {
        const double t = i / 255.0;
        const double s = t * t * (3.0 - 2.0 * t);              // S-curve
        const double g = std::pow(t, variant == 0 ? 0.75 : 1.3); // gamma
        const double b = variant == 0 ? 30.0 + 0.8 * i : 255.0 - 0.6 * i;
        lut[i * 4 + 0] = u8(255.0 * s);
        lut[i * 4 + 1] = u8(255.0 * g);
        lut[i * 4 + 2] = u8(b);
        lut[i * 4 + 3] = 255;
    }
    return lut;
}

} // namespace

std::shared_ptr<CanvasDocument> CanvasDocument::golden()
{
    std::shared_ptr<CanvasDocument> doc(new CanvasDocument);
    doc->m_golden = true;
    doc->m_size = QSize(1024, 1024);
    doc->m_pixelLayers = 6;
    const int W = 1024, H = 1024;
    Rng rng{kGoldenSeed};
    const std::vector<Circle> blobs = makeCircles(rng, 14, W, H, 40, 170);

    // Straight-alpha texel of pixel layer `l` at document pixel (x, y).
    auto texel = [&](int l, int x, int y) -> Rgba {
        const auto ux = std::uint32_t(x), uy = std::uint32_t(y);
        switch (l) {
        case 0: { // "paper": opaque gradient + grain
            const double n = double(hash3(ux, uy, 0) & 31u) - 16.0;
            return {40.0 + 150.0 * x / W + n, 60.0 + 120.0 * y / H + n, 200.0 - 110.0 * (x + y) / (W + H) + n, 255};
        }
        case 1: // "blobs": soft circles, sparse
            return circlesAt(blobs, x, y, 12.0);
        case 2: { // "ink": stripes, left three quarters only
            if (x >= 768)
                return {};
            const bool s = ((x + 2 * y) / 24) % 2 == 1;
            const double n = double(hash3(ux, uy, 2) & 15u);
            return s ? Rgba{90 + n, 140 + n, 220, 255} : Rgba{230, 190 + n, 120 + n, 110};
        }
        case 3: { // "glow": radial, sparse
            const double t = 1.0 - std::hypot(x + 0.5 - 620.0, y + 0.5 - 380.0) / 460.0;
            if (t <= 0.0)
                return {};
            const double n = double(hash3(ux, uy, 3) & 15u);
            return {255, 170 + 60 * t + n, 80 + n, 255.0 * std::pow(t, 1.5)};
        }
        case 4: { // "grain": overlay texture, more transparent in the lower half
            const double v = 128.0 + double(int(hash3(ux, uy, 4) & 127u) - 64) + 40.0 * std::sin(x / 37.0) * std::cos(y / 53.0);
            return {v, v * 0.95, v * 1.05, y < 512 ? 255.0 : 140.0};
        }
        default: { // "veil": checker blocks; every third tile empty
            const int tx = x / kTile, ty = y / kTile;
            if ((tx + 2 * ty) % 3 == 0)
                return {};
            const bool block = ((x / 64) + (y / 64)) % 2 == 0;
            const std::uint32_t hc = hash3(std::uint32_t(x / 64), std::uint32_t(y / 64), 5);
            return {double(hc & 255u), double((hc >> 8) & 255u), double((hc >> 16) & 255u), block ? 220.0 : 70.0};
        }
        }
    };

    const int tiles = doc->tileCount();
    doc->m_tiles.resize(std::size_t(doc->m_pixelLayers) * tiles);
    for (int l = 0; l < doc->m_pixelLayers; ++l) {
        for (int ty = 0; ty < doc->tilesY(); ++ty) {
            for (int tx = 0; tx < doc->tilesX(); ++tx) {
                std::vector<std::uint8_t> buf(kTileBytes);
                bool any = false;
                for (int y = 0; y < kTile; ++y) {
                    for (int x = 0; x < kTile; ++x) {
                        const Rgba c = texel(l, tx * kTile + x, ty * kTile + y);
                        std::uint8_t *p = &buf[(y * kTile + x) * 4];
                        p[3] = u8(c.a);
                        if (p[3] == 0) {
                            p[0] = p[1] = p[2] = 0;
                            continue;
                        }
                        p[0] = u8(c.r);
                        p[1] = u8(c.g);
                        p[2] = u8(c.b);
                        any = true;
                    }
                }
                if (any)
                    doc->m_tiles[std::size_t(l) * tiles + ty * doc->tilesX() + tx] = std::move(buf);
            }
        }
    }

    doc->m_luts.push_back(makeLut(0));
    auto px = [](int layer, BlendMode b, float op, const char *name) {
        StackEntry e;
        e.kind = KindPixel;
        e.blend = b;
        e.opacity = op;
        e.pixelLayer = layer;
        e.name = QString::fromLatin1(name);
        return e;
    };
    StackEntry gBegin;
    gBegin.kind = KindGroupBegin;
    gBegin.blend = BlendNormal;
    gBegin.opacity = 0.8f;
    gBegin.name = QStringLiteral("group (isolated)");
    StackEntry gEnd;
    gEnd.kind = KindGroupEnd;
    StackEntry curves;
    curves.kind = KindCurves;
    curves.opacity = 0.75f;
    curves.lut = 0;
    curves.name = QStringLiteral("curves");
    doc->m_stack = {px(0, BlendNormal, 1.0f, "paper"),
                    px(1, BlendNormal, 0.7f, "blobs"),
                    gBegin,
                    px(2, BlendMultiply, 1.0f, "ink"),
                    px(3, BlendScreen, 0.6f, "glow"),
                    gEnd,
                    px(4, BlendOverlay, 0.9f, "grain"),
                    curves,
                    px(5, BlendNormal, 0.5f, "veil")};
    doc->m_description = QStringLiteral("golden 1024x1024: paper(normal 100%), blobs(normal 70%), "
                                        "group[isolated, normal 80%]{ink(multiply 100%), glow(screen 60%)}, "
                                        "grain(overlay 90%), curves(75%), veil(normal 50%)");
    return doc;
}

std::shared_ptr<CanvasDocument> CanvasDocument::procedural(int layers, QSize size, bool withGroup)
{
    std::shared_ptr<CanvasDocument> doc(new CanvasDocument);
    layers = std::clamp(layers, 1, 28);
    doc->m_size = size;
    doc->m_pixelLayers = layers;
    const int tiles = doc->tileCount();
    Rng rng{kGoldenSeed ^ (std::uint64_t(layers) << 32) ^ std::uint64_t(size.width() * 131 + size.height())};

    doc->m_variants.resize(std::size_t(layers) * kVariants);
    for (int l = 0; l < layers; ++l) {
        for (int v = 0; v < kVariants; ++v) {
            std::vector<std::uint8_t> buf(kTileBytes);
            const int pattern = l == 0 ? 0 : 1 + (l + v) % 3;
            const std::vector<Circle> circles = makeCircles(rng, 5, kTile, kTile, 12, 60);
            const double cr = rng.range(30, 255), cg = rng.range(30, 255), cb = rng.range(30, 255);
            for (int y = 0; y < kTile; ++y) {
                for (int x = 0; x < kTile; ++x) {
                    const std::uint32_t h = hash3(std::uint32_t(x), std::uint32_t(y), std::uint32_t(l * kVariants + v));
                    const double n = double(h & 31u) - 16.0;
                    Rgba c;
                    switch (pattern) {
                    case 0: c = {cr * 0.5 + x + n, cg * 0.5 + y + n, cb * 0.7 + n, 255}; break;
                    case 1: c = circlesAt(circles, x, y, 6.0); break;
                    case 2: c = ((x + y + v * 7) / 16) % 2 ? Rgba{cr + n, cg, cb, 230} : Rgba{cb, cr, cg + n, 90}; break;
                    default: c = {cr + n * 2, cg + n * 2, cb + n * 2, double(128 + (h >> 8) % 128)}; break;
                    }
                    std::uint8_t *p = &buf[(y * kTile + x) * 4];
                    p[3] = u8(c.a);
                    p[0] = p[3] ? u8(c.r) : 0;
                    p[1] = p[3] ? u8(c.g) : 0;
                    p[2] = p[3] ? u8(c.b) : 0;
                }
            }
            doc->m_variants[std::size_t(l) * kVariants + v] = std::move(buf);
        }
    }
    doc->m_variantOf.resize(std::size_t(layers) * tiles);
    for (int l = 0; l < layers; ++l) {
        for (int t = 0; t < tiles; ++t) {
            const std::uint32_t h = hash3(std::uint32_t(l), std::uint32_t(t), 0xC0FFEEu);
            const bool present = l == 0 || (h % 4u) != 0u;
            doc->m_variantOf[std::size_t(l) * tiles + t] = present ? std::int16_t((h >> 4) % kVariants) : std::int16_t(-1);
        }
    }

    static const BlendMode modes[] = {BlendNormal, BlendMultiply, BlendScreen, BlendOverlay};
    const bool group = withGroup && layers >= 4;
    const int curvesAfter = layers >= 3 ? layers / 2 : -1;
    doc->m_luts.push_back(makeLut(1));
    for (int l = 0; l < layers; ++l) {
        if (group && l == 1) {
            StackEntry g;
            g.kind = KindGroupBegin;
            g.opacity = 0.85f;
            g.name = QStringLiteral("group (isolated)");
            doc->m_stack.push_back(g);
        }
        StackEntry e;
        e.kind = KindPixel;
        e.blend = l == 0 ? BlendNormal : modes[l % 4];
        e.opacity = l == 0 ? 1.0f : float(0.55 + 0.45 * double(hash32(std::uint32_t(l) * 977u) % 1000u) / 1000.0);
        e.pixelLayer = l;
        e.name = QStringLiteral("layer %1").arg(l);
        doc->m_stack.push_back(e);
        if (group && l == 2) {
            StackEntry g;
            g.kind = KindGroupEnd;
            doc->m_stack.push_back(g);
        }
        if (l == curvesAfter) {
            StackEntry c;
            c.kind = KindCurves;
            c.opacity = 0.6f;
            c.lut = 0;
            c.name = QStringLiteral("curves");
            doc->m_stack.push_back(c);
        }
    }
    doc->m_description = QStringLiteral("procedural %1x%2, %3 layers%4, curves after layer %5, 75% tile coverage")
                             .arg(size.width()).arg(size.height()).arg(layers)
                             .arg(group ? QStringLiteral(", isolated group of layers 1-2") : QString())
                             .arg(curvesAfter);
    return doc;
}

const std::uint8_t *CanvasDocument::tile(int pixelLayer, int tx, int ty) const
{
    if (pixelLayer < 0 || pixelLayer >= m_pixelLayers || tx < 0 || ty < 0 || tx >= tilesX() || ty >= tilesY())
        return nullptr;
    const std::size_t idx = std::size_t(pixelLayer) * tileCount() + ty * tilesX() + tx;
    if (m_golden) {
        const auto &b = m_tiles[idx];
        return b.empty() ? nullptr : b.data();
    }
    const int v = m_variantOf[idx];
    return v < 0 ? nullptr : m_variants[std::size_t(pixelLayer) * kVariants + v].data();
}

qint64 CanvasDocument::nonEmptyTileCount() const
{
    qint64 n = 0;
    for (int l = 0; l < m_pixelLayers; ++l)
        for (int ty = 0; ty < tilesY(); ++ty)
            for (int tx = 0; tx < tilesX(); ++tx)
                n += tile(l, tx, ty) ? 1 : 0;
    return n;
}

} // namespace canvas
