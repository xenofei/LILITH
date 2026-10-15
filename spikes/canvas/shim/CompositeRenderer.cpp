#include "CompositeRenderer.h"
#include "SpikeLog.h"

#include <QDateTime>
#include <QFile>
#include <QImage>
#include <QRect>
#include <QJsonArray>
#include <QMutexLocker>
#include <QVarLengthArray>
#include <rhi/qrhi.h>
#include <rhi/qrhi_platform.h>

#ifdef Q_OS_WIN
#  ifndef NOMINMAX
#    define NOMINMAX
#  endif
#  include <d3d11.h>
#  include <d3d11sdklayers.h>
#  include <d3d12.h>
#  include <d3d12sdklayers.h>
#endif

#include <algorithm>
#include <cmath>
#include <cstring>
#include <list>
#include <unordered_map>
#include <unordered_set>

using namespace canvas;

// ------------------------------------------------------------------------------------------------
// CanvasHub
// ------------------------------------------------------------------------------------------------

CanvasHub::CanvasHub()
{
    qRegisterMetaType<ProjectionReadbackPtr>();
    m_clock.start();
    m_startEpochMs = QDateTime::currentMSecsSinceEpoch();
}

CanvasHub *CanvasHub::instance()
{
    static CanvasHub hub;
    return &hub;
}

void CanvasHub::setConfig(const CanvasConfig &c)
{
    QMutexLocker lock(&m_mutex);
    m_config = c;
}

CanvasConfig CanvasHub::config() const
{
    QMutexLocker lock(&m_mutex);
    return m_config;
}

void CanvasHub::addFrameStat(const FrameStat &s)
{
    QMutexLocker lock(&m_mutex);
    m_stats.push_back(s);
}

std::vector<FrameStat> CanvasHub::frameStats() const
{
    QMutexLocker lock(&m_mutex);
    return m_stats;
}

void CanvasHub::addTrimStat(const TrimStat &s)
{
    QMutexLocker lock(&m_mutex);
    m_trims.push_back(s);
}

std::vector<TrimStat> CanvasHub::trimStats() const
{
    QMutexLocker lock(&m_mutex);
    return m_trims;
}

// ------------------------------------------------------------------------------------------------
// Renderer
// ------------------------------------------------------------------------------------------------

namespace {

constexpr int kMaxNodes = 32;      // must match composite.comp
constexpr int kMaxGroups = 4;      // must match composite.comp
constexpr int kMaxArrays = 4;      // tiles0..tiles3 in composite.comp
constexpr int kDisplayLevels = 8;  // level0..level7 in display.frag
constexpr int kLocal = 16;         // local_size_x/y of the compute shaders
constexpr int kAtlasCols = 8;      // group intermediate: 8 regions of 128 per row

constexpr int kKindPixel = 0, kKindGroup = 1, kKindCurves = 3; // composite.comp node kinds
constexpr int kKindGroupBegin = 4, kKindGroupEnd = 5;             // flat program (variants 1 and 2)

struct SceneUbo {
    qint32 header[4];
    qint32 nodeI[kMaxNodes][4];
    float nodeF[kMaxNodes][4];
};
struct JobUbo {
    qint32 job[4];
    qint32 atlasOut[4];
    qint32 groupOrigin[kMaxGroups][4];
};
struct DownUbo {
    qint32 sizes[4];
    qint32 region[4];
};
struct ViewUbo {
    float viewport[4];
    float pan[4];
};

std::atomic<int> g_rendererCount{0};
std::atomic<int> g_initializeCount{0};

QShader loadShader(const QString &path)
{
    QFile f(path);
    if (f.open(QIODevice::ReadOnly))
        return QShader::fromSerialized(f.readAll());
    SpikeLog::write(QStringLiteral("error"), {{"what", "shader not found"}, {"path", path}});
    return {};
}

template<typename T>
void retire(std::unique_ptr<T> &p)
{
    // deleteLater(): inside a frame the release waits until the GPU is done with the frame.
    if (p)
        p.release()->deleteLater();
}

// An SRB plus the textures it binds for reading (sampled) and for writing (image store).
// R4: every compute pipeline creation and every dispatch asserts that the two sets are disjoint.
struct TrackedSrb {
    std::unique_ptr<QRhiShaderResourceBindings> srb;
    std::vector<const QRhiTexture *> reads;
    std::vector<const QRhiTexture *> writes;
    QString name;
};

class SrbBuilder
{
public:
    using Stage = QRhiShaderResourceBinding::StageFlags;
    SrbBuilder &ubuf(int b, Stage s, QRhiBuffer *buf)
    {
        m_b.append(QRhiShaderResourceBinding::uniformBuffer(b, s, buf));
        return *this;
    }
    SrbBuilder &ubufDynamic(int b, Stage s, QRhiBuffer *buf, quint32 size)
    {
        m_b.append(QRhiShaderResourceBinding::uniformBufferWithDynamicOffset(b, s, buf, size));
        return *this;
    }
    SrbBuilder &bufferLoad(int b, Stage s, QRhiBuffer *buf)
    {
        m_b.append(QRhiShaderResourceBinding::bufferLoad(b, s, buf));
        return *this;
    }
    SrbBuilder &sampled(int b, Stage s, QRhiTexture *t, QRhiSampler *smp)
    {
        m_b.append(QRhiShaderResourceBinding::sampledTexture(b, s, t, smp));
        m_reads.push_back(t);
        return *this;
    }
    SrbBuilder &store(int b, Stage s, QRhiTexture *t)
    {
        m_b.append(QRhiShaderResourceBinding::imageStore(b, s, t, 0));
        m_writes.push_back(t);
        return *this;
    }
    // Variant 2 only: one read-write UAV binding (imageLoad + imageStore of the same texel by the
    // invocation that owns it). Recorded as a write: it is not sampled, so the R4 assertion still
    // proves that no texture is bound both as a sampled input and as an output.
    SrbBuilder &loadStore(int b, Stage s, QRhiTexture *t)
    {
        m_b.append(QRhiShaderResourceBinding::imageLoadStore(b, s, t, 0));
        m_writes.push_back(t);
        return *this;
    }
    TrackedSrb build(QRhi *rhi, const QString &name)
    {
        TrackedSrb t;
        t.srb.reset(rhi->newShaderResourceBindings());
        t.srb->setBindings(m_b.cbegin(), m_b.cend());
        t.srb->create();
        t.reads = m_reads;
        t.writes = m_writes;
        t.name = name;
        return t;
    }

private:
    QVarLengthArray<QRhiShaderResourceBinding, 16> m_b;
    std::vector<const QRhiTexture *> m_reads, m_writes;
};

bool assertDisjoint(const TrackedSrb &s, const char *where)
{
    CanvasHub *hub = CanvasHub::instance();
    ++hub->hazardChecks;
    for (const QRhiTexture *w : s.writes) {
        if (std::find(s.reads.begin(), s.reads.end(), w) != s.reads.end()) {
            ++hub->hazardViolations;
            SpikeLog::write(QStringLiteral("hazard_violation"),
                            {{"srb", s.name}, {"where", QString::fromLatin1(where)}});
            if (hub->config().injectHazard)
                return true; // --inject-hazard: dispatch anyway, so the debug layer sees it
            Q_ASSERT_X(false, where, "a compute dispatch would read and write the same texture");
            return false;
        }
    }
    return true;
}

double bounce(double x, double range)
{
    if (range <= 0.0)
        return 0.0;
    const double m = std::fmod(x, 2.0 * range);
    return m <= range ? m : 2.0 * range - m;
}

QString severityName(int s)
{
    switch (s) {
    case 0: return QStringLiteral("corruption");
    case 1: return QStringLiteral("error");
    case 2: return QStringLiteral("warning");
    case 3: return QStringLiteral("info");
    default: return QStringLiteral("message");
    }
}

#ifdef Q_OS_WIN
bool isD3D11HazardId(D3D11_MESSAGE_ID id)
{
    switch (id) {
    case D3D11_MESSAGE_ID_DEVICE_VSSETSHADERRESOURCES_HAZARD:
    case D3D11_MESSAGE_ID_DEVICE_VSSETCONSTANTBUFFERS_HAZARD:
    case D3D11_MESSAGE_ID_DEVICE_GSSETSHADERRESOURCES_HAZARD:
    case D3D11_MESSAGE_ID_DEVICE_GSSETCONSTANTBUFFERS_HAZARD:
    case D3D11_MESSAGE_ID_DEVICE_PSSETSHADERRESOURCES_HAZARD:
    case D3D11_MESSAGE_ID_DEVICE_PSSETCONSTANTBUFFERS_HAZARD:
    case D3D11_MESSAGE_ID_DEVICE_CSSETSHADERRESOURCES_HAZARD:
    case D3D11_MESSAGE_ID_DEVICE_CSSETCONSTANTBUFFERS_HAZARD:
    case D3D11_MESSAGE_ID_DEVICE_CSSETUNORDEREDACCESSVIEWS_HAZARD:
    case D3D11_MESSAGE_ID_DEVICE_OMSETRENDERTARGETS_HAZARD:
    case D3D11_MESSAGE_ID_DEVICE_IASETVERTEXBUFFERS_HAZARD:
    case D3D11_MESSAGE_ID_DEVICE_IASETINDEXBUFFER_HAZARD:
    case D3D11_MESSAGE_ID_DEVICE_SOSETTARGETS_HAZARD:
        return true;
    default:
        return false;
    }
}
#endif

class CompositeRenderer : public QQuickRhiItemRenderer
{
public:
    CompositeRenderer()
        : m_id(++g_rendererCount)
    {
        CanvasHub *hub = CanvasHub::instance();
        m_cfg = hub->config();
        m_generation = hub->generation.load();
        SpikeLog::write(QStringLiteral("renderer_created"),
                        {{"renderer", m_id}, {"generation", m_generation}, {"frame", hub->frame.load()}});
    }
    ~CompositeRenderer() override
    {
        releaseInfoQueue();
        SpikeLog::write(QStringLiteral("renderer_destroyed"),
                        {{"renderer", m_id}, {"frame", CanvasHub::instance()->frame.load()},
                         {"renders", m_renderCount}, {"tiles_composited_total", m_compositedTotal}});
    }

protected:
    void initialize(QRhiCommandBuffer *cb) override;
    void synchronize(QQuickRhiItem *item) override { m_guiPanT = static_cast<CompositeCanvas *>(item)->panTime(); }
    void render(QRhiCommandBuffer *cb) override;

private:
    struct CacheArray {
        std::unique_ptr<QRhiTexture> tex;
        int slices = 0;
    };
    struct Resident {
        int slot = -1;
        qint64 lastUsed = 0;
        std::list<quint64>::iterator lru;
    };

    bool buildPrograms();
    bool createStatic();
    void releaseAll();
    void rebuildComputeSrbs();
    bool ensureDisplayPipeline();
    int allocSlot();
    bool addArray(int slices);
    void trimToVisible(QRhiResourceUpdateBatch *u, const std::vector<int> &visible);
    qint64 cacheSlices() const;
    void setupInfoQueue();
    void releaseInfoQueue();
    void drainInfoQueue();
    void issueReadback(QRhiCommandBuffer *cb, bool first, bool final, int frame);

    const int m_id;
    int m_generation = 0;
    CanvasConfig m_cfg;
    QRhi *m_rhi = nullptr;
    bool m_failed = false;
    bool m_staticUploaded = false;

    // Programs (built from the document's flat stack).
    SceneUbo m_mainScene{};
    SceneUbo m_flatScene{};  // variants 1 and 2: the whole tree, groups as BEGIN/END nodes
    std::vector<SceneUbo> m_groupScenes;
    int m_groups = 0;
    int m_pixelLayers = 0;
    int m_levelCount = 0;
    std::vector<QSize> m_levelSizes;
    qint64 m_nonEmptyTiles = 0;
    int m_maxSlices = 2048;
    quint32 m_jobStride = 256;

    // GPU resources.
    std::vector<std::unique_ptr<QRhiTexture>> m_levels;  // [0] = projection
    std::unique_ptr<QRhiTexture> m_atlas, m_dummyAtlas, m_lut, m_dummyArray;
    std::unique_ptr<QRhiSampler> m_nearest, m_linear;
    std::unique_ptr<QRhiBuffer> m_mainSceneUbo, m_flatSceneUbo, m_jobUbo, m_slotBuf, m_viewUbo;
    std::vector<std::unique_ptr<QRhiBuffer>> m_groupSceneUbos, m_downUbos;
    TrackedSrb m_mainSrb;
    std::vector<TrackedSrb> m_groupSrbs, m_downSrbs;
    std::unique_ptr<QRhiComputePipeline> m_mainPipe, m_groupPipe, m_downPipe;
    std::unique_ptr<QRhiShaderResourceBindings> m_displaySrb;
    std::unique_ptr<QRhiGraphicsPipeline> m_displayPipe;
    int m_displaySampleCount = 0;
    bool m_srbsDirty = true;

    // Tile cache.
    std::vector<CacheArray> m_arrays;
    std::vector<std::unique_ptr<QRhiTexture>> m_reserve; // Task 8: --gpu-cache-mb beyond the cap
    bool m_reserveDone = false;
    std::vector<int> m_free;                          // encoded (array << 16) | slice
    std::unordered_map<quint64, Resident> m_resident; // key = layer * tileCount + tile
    std::unordered_map<int, quint64> m_owner;         // slot -> key
    std::list<quint64> m_lru;                         // front = least recently used

    // Projection state.
    std::vector<std::uint8_t> m_composited;
    int m_compositedCount = 0;
    qint64 m_compositedTotal = 0;
    qint64 m_renderCount = 0;
    bool m_firstViewDone = false;
    int m_trimSeen = 0;
    bool m_lostLogged = false;
    double m_guiPanT = 0.0; // --gui-pan: the item's panTime, copied in synchronize()

    // Debug layer (D3D11 / D3D12 info queue).
    void *m_infoQueue = nullptr;
    int m_infoQueueKind = 0; // 1 = D3D11, 2 = D3D12
    int m_msgLogged = 0;
};

bool CompositeRenderer::buildPrograms()
{
    const CanvasDocument &doc = *m_cfg.doc;
    m_pixelLayers = doc.pixelLayerCount();
    m_mainScene = {};
    m_flatScene = {};
    m_groupScenes.clear();
    m_groups = 0;
    int currentGroup = -1;
    auto addNode = [](SceneUbo &s, int kind, int blend, int z, bool visible, float opacity) {
        const int i = s.header[0];
        if (i >= kMaxNodes)
            return false;
        s.nodeI[i][0] = kind;
        s.nodeI[i][1] = blend;
        s.nodeI[i][2] = z;
        s.nodeI[i][3] = visible ? 1 : 0;
        s.nodeF[i][0] = opacity;
        s.header[0] = i + 1;
        return true;
    };
    bool ok = true;
    for (const StackEntry &e : doc.stack()) {
        switch (e.kind) {
        case KindGroupBegin:
            if (currentGroup >= 0 || m_groups >= kMaxGroups) {
                SpikeLog::write(QStringLiteral("error"), {{"what", "nested groups or more than 4 groups are not supported on the GPU path"}});
                return false;
            }
            currentGroup = m_groups++;
            m_groupScenes.push_back(SceneUbo{});
            ok &= addNode(m_mainScene, kKindGroup, e.blend, currentGroup, e.visible, e.opacity);
            ok &= addNode(m_flatScene, kKindGroupBegin, e.blend, currentGroup, e.visible, e.opacity);
            break;
        case KindGroupEnd:
            currentGroup = -1;
            ok &= addNode(m_flatScene, kKindGroupEnd, 0, 0, true, 1.0f);
            break;
        case KindPixel:
            ok &= addNode(currentGroup >= 0 ? m_groupScenes[currentGroup] : m_mainScene, kKindPixel, e.blend,
                          e.pixelLayer, e.visible, e.opacity);
            ok &= addNode(m_flatScene, kKindPixel, e.blend, e.pixelLayer, e.visible, e.opacity);
            break;
        case KindCurves:
            ok &= addNode(currentGroup >= 0 ? m_groupScenes[currentGroup] : m_mainScene, kKindCurves, 0, e.lut,
                          e.visible, e.opacity);
            ok &= addNode(m_flatScene, kKindCurves, 0, e.lut, e.visible, e.opacity);
            break;
        }
    }
    const QSize ds = doc.size();
    auto setHeader = [&](SceneUbo &s) {
        s.header[1] = std::max(1, m_pixelLayers);
        s.header[2] = ds.width();
        s.header[3] = ds.height();
    };
    setHeader(m_mainScene);
    setHeader(m_flatScene);
    for (SceneUbo &g : m_groupScenes)
        setHeader(g);
    if (!ok)
        SpikeLog::write(QStringLiteral("error"), {{"what", "more than 32 nodes in one program"}});
    return ok;
}

void CompositeRenderer::releaseAll()
{
    m_displayPipe.reset();
    m_displaySrb.reset();
    m_mainPipe.reset();
    m_groupPipe.reset();
    m_downPipe.reset();
    m_mainSrb = {};
    m_groupSrbs.clear();
    m_downSrbs.clear();
    m_levels.clear();
    m_atlas.reset();
    m_dummyAtlas.reset();
    m_lut.reset();
    m_dummyArray.reset();
    m_nearest.reset();
    m_linear.reset();
    m_mainSceneUbo.reset();
    m_flatSceneUbo.reset();
    m_jobUbo.reset();
    m_slotBuf.reset();
    m_viewUbo.reset();
    m_groupSceneUbos.clear();
    m_downUbos.clear();
    m_arrays.clear();
    m_reserve.clear();
    m_reserveDone = false;
    m_free.clear();
    m_resident.clear();
    m_owner.clear();
    m_lru.clear();
    m_composited.clear();
    m_compositedCount = 0;
    m_staticUploaded = false;
    m_srbsDirty = true;
    m_displaySampleCount = 0;
}

bool CompositeRenderer::createStatic()
{
    QRhi *r = m_rhi;
    const char *missing = nullptr;
    if (!r->isFeatureSupported(QRhi::Compute))
        missing = "QRhi::Compute";
    else if (!r->isFeatureSupported(QRhi::TextureArrays))
        missing = "QRhi::TextureArrays";
    else if (!r->isTextureFormatSupported(QRhiTexture::RGBA8, QRhiTexture::UsedWithLoadStore))
        missing = "RGBA8 load/store";
    else if (!r->isTextureFormatSupported(QRhiTexture::RGBA32F, QRhiTexture::UsedWithLoadStore))
        missing = "RGBA32F load/store";
    if (missing) {
        SpikeLog::write(QStringLiteral("error"), {{"what", "unsupported"}, {"feature", QString::fromLatin1(missing)}});
        return false;
    }
    if (!m_cfg.doc || !buildPrograms())
        return false;

    const CanvasDocument &doc = *m_cfg.doc;
    m_maxSlices = std::min(2048, std::max(1, r->resourceLimit(QRhi::TextureArraySizeMax)));
    m_nonEmptyTiles = doc.nonEmptyTileCount();
    const quint32 align = quint32(r->ubufAlignment());
    m_jobStride = (quint32(sizeof(JobUbo)) + align - 1) / align * align;

    m_nearest.reset(r->newSampler(QRhiSampler::Nearest, QRhiSampler::Nearest, QRhiSampler::None,
                                  QRhiSampler::ClampToEdge, QRhiSampler::ClampToEdge));
    m_linear.reset(r->newSampler(QRhiSampler::Linear, QRhiSampler::Linear, QRhiSampler::None,
                                 QRhiSampler::ClampToEdge, QRhiSampler::ClampToEdge));
    m_nearest->create();
    m_linear->create();

    // Projection + separate mip textures (no sRGB flag anywhere: IMG-035).
    m_levelSizes.clear();
    QSize s = doc.size();
    for (;;) {
        m_levelSizes.push_back(s);
        if (s.width() == 1 && s.height() == 1)
            break;
        s = QSize(std::max(1, (s.width() + 1) / 2), std::max(1, (s.height() + 1) / 2));
    }
    m_levelCount = int(m_levelSizes.size());
    for (const QSize &ls : m_levelSizes) {
        std::unique_ptr<QRhiTexture> t(r->newTexture(QRhiTexture::RGBA8, ls, 1,
                                                     QRhiTexture::UsedWithLoadStore | QRhiTexture::UsedAsTransferSource));
        if (!t->create())
            return false;
        m_levels.push_back(std::move(t));
    }

    const int regions = std::max(1, m_cfg.maxTilesPerFrame * m_groups);
    const int atlasRows = (regions + kAtlasCols - 1) / kAtlasCols;
    m_atlas.reset(r->newTexture(QRhiTexture::RGBA32F, QSize(kAtlasCols * kTile, atlasRows * kTile), 1,
                                QRhiTexture::UsedWithLoadStore));
    m_dummyAtlas.reset(r->newTexture(QRhiTexture::RGBA32F, QSize(1, 1)));
    const int lutRows = std::max<int>(1, int(doc.luts().size()));
    m_lut.reset(r->newTexture(QRhiTexture::RGBA8, QSize(256, lutRows)));
    m_dummyArray.reset(r->newTextureArray(QRhiTexture::RGBA8, 1, QSize(1, 1)));
    if (!m_atlas->create() || !m_dummyAtlas->create() || !m_lut->create() || !m_dummyArray->create())
        return false;

    m_mainSceneUbo.reset(r->newBuffer(QRhiBuffer::Dynamic, QRhiBuffer::UniformBuffer, sizeof(SceneUbo)));
    m_mainSceneUbo->create();
    m_flatSceneUbo.reset(r->newBuffer(QRhiBuffer::Dynamic, QRhiBuffer::UniformBuffer, sizeof(SceneUbo)));
    m_flatSceneUbo->create();
    for (int g = 0; g < m_groups; ++g) {
        std::unique_ptr<QRhiBuffer> b(r->newBuffer(QRhiBuffer::Dynamic, QRhiBuffer::UniformBuffer, sizeof(SceneUbo)));
        b->create();
        m_groupSceneUbos.push_back(std::move(b));
    }
    m_jobUbo.reset(r->newBuffer(QRhiBuffer::Dynamic, QRhiBuffer::UniformBuffer,
                                m_jobStride * quint32(m_cfg.maxTilesPerFrame * (1 + m_groups))));
    m_jobUbo->create();
    m_slotBuf.reset(r->newBuffer(QRhiBuffer::Static, QRhiBuffer::StorageBuffer,
                                 quint32(m_cfg.maxTilesPerFrame * std::max(1, m_pixelLayers) * 4)));
    m_slotBuf->create();
    m_viewUbo.reset(r->newBuffer(QRhiBuffer::Dynamic, QRhiBuffer::UniformBuffer, sizeof(ViewUbo)));
    m_viewUbo->create();
    for (int k = 1; k < m_levelCount; ++k) {
        std::unique_ptr<QRhiBuffer> b(r->newBuffer(QRhiBuffer::Dynamic, QRhiBuffer::UniformBuffer, sizeof(DownUbo)));
        b->create();
        m_downUbos.push_back(std::move(b));
    }

    rebuildComputeSrbs();

    auto makeCompute = [&](const QString &shader, const TrackedSrb &layout, const char *what) {
        std::unique_ptr<QRhiComputePipeline> p(r->newComputePipeline());
        p->setShaderStage({QRhiShaderStage::Compute, loadShader(shader)});
        p->setShaderResourceBindings(layout.srb.get());
        assertDisjoint(layout, what);
        if (!p->create()) {
            SpikeLog::write(QStringLiteral("error"), {{"what", "compute pipeline create failed"}, {"shader", shader}});
            p.reset();
        }
        return p;
    };
    if (m_cfg.sm66 && r->backend() != QRhi::D3D12) {
        SpikeLog::write(QStringLiteral("error"), {{"what", "the SM 6.6 (DXIL) composite shaders need the D3D12 backend"}});
        return false;
    }
    // The .qsb files of the SM 6.6 builds hold DXIL only (no DXBC, no HLSL source), so a pipeline that
    // is created from them on D3D12 runs dxc-compiled DXIL.
    const QString sm = m_cfg.sm66 ? QStringLiteral("_sm66") : QString();
    const QString mainShader = m_cfg.variant == 1 ? QStringLiteral(":/shaders/composite_single%1.comp.qsb").arg(sm)
                             : m_cfg.variant == 2 ? QStringLiteral(":/shaders/composite_inplace%1.comp.qsb").arg(m_cfg.sm66 ? QString() : QStringLiteral("_sm50"))
                                                  : QStringLiteral(":/shaders/composite_main%1.comp.qsb").arg(sm);
    m_mainPipe = makeCompute(mainShader, m_mainSrb, "pipeline:composite_main");
    if (m_groups > 0 && m_cfg.variant == 0)
        m_groupPipe = makeCompute(QStringLiteral(":/shaders/composite_group%1.comp.qsb").arg(sm), m_groupSrbs[0], "pipeline:composite_group");
    if (m_levelCount > 1)
        m_downPipe = makeCompute(QStringLiteral(":/shaders/downsample.comp.qsb"), m_downSrbs[0], "pipeline:downsample");
    if (!m_mainPipe || (m_groups > 0 && m_cfg.variant == 0 && !m_groupPipe) || (m_levelCount > 1 && !m_downPipe))
        return false;

    m_composited.assign(std::size_t(doc.tileCount()), 0);
    m_compositedCount = 0;
    SpikeLog::write(QStringLiteral("resources"),
                    {{"renderer", m_id}, {"levels", m_levelCount}, {"groups", m_groups},
                     {"pixel_layers", m_pixelLayers}, {"main_nodes", m_mainScene.header[0]},
                     {"max_array_slices", m_maxSlices}, {"non_empty_layer_tiles", m_nonEmptyTiles},
                     {"atlas", QStringLiteral("%1x%2 RGBA32F").arg(m_atlas->pixelSize().width()).arg(m_atlas->pixelSize().height())},
                     {"job_stride", int(m_jobStride)},
                     {"variant", m_cfg.variant == 1 ? "single" : m_cfg.variant == 2 ? "inplace" : "twopass"},
                     {"composite_shader", mainShader},
                     {"shader_model", m_cfg.sm66 ? "6.6 DXIL (dxc)" : "5.0 DXBC (fxc)"},
                     {"flat_nodes", m_flatScene.header[0]}});
    return true;
}

void CompositeRenderer::rebuildComputeSrbs()
{
    QRhi *r = m_rhi;
    using B = QRhiShaderResourceBinding;
    const auto cs = B::ComputeStage;
    QRhiTexture *arrays[kMaxArrays];
    for (int i = 0; i < kMaxArrays; ++i)
        arrays[i] = i < int(m_arrays.size()) ? m_arrays[i].tex.get() : m_dummyArray.get();

    auto common = [&](SrbBuilder &b, QRhiBuffer *scene) {
        b.ubuf(0, cs, scene).ubufDynamic(1, cs, m_jobUbo.get(), sizeof(JobUbo)).bufferLoad(2, cs, m_slotBuf.get());
        for (int i = 0; i < kMaxArrays; ++i)
            b.sampled(3 + i, cs, arrays[i], m_nearest.get());
        b.sampled(7, cs, m_lut.get(), m_nearest.get());
    };

    retire(m_mainSrb.srb);
    if (m_cfg.variant == 0) {
        SrbBuilder b;
        common(b, m_mainSceneUbo.get());
        QRhiTexture *groupInput = m_groups > 0 ? m_atlas.get() : m_dummyAtlas.get();
        if (m_cfg.injectHazard)
            groupInput = m_levels[0].get(); // negative control only: read and write the projection
        b.sampled(8, cs, groupInput, m_nearest.get());
        b.store(9, cs, m_levels[0].get());
        m_mainSrb = b.build(r, QStringLiteral("composite_main"));
    } else {
        SrbBuilder b;
        common(b, m_flatSceneUbo.get());
        b.sampled(8, cs, m_dummyAtlas.get(), m_nearest.get());
        if (m_cfg.variant == 2)
            b.loadStore(9, cs, m_levels[0].get());
        else
            b.store(9, cs, m_levels[0].get());
        m_mainSrb = b.build(r, m_cfg.variant == 2 ? QStringLiteral("composite_inplace") : QStringLiteral("composite_single"));
    }
    for (TrackedSrb &t : m_groupSrbs)
        retire(t.srb);
    m_groupSrbs.clear();
    for (int g = 0; g < m_groups; ++g) {
        SrbBuilder b;
        common(b, m_groupSceneUbos[g].get());
        b.sampled(8, cs, m_dummyAtlas.get(), m_nearest.get());
        b.store(9, cs, m_atlas.get());
        m_groupSrbs.push_back(b.build(r, QStringLiteral("composite_group_%1").arg(g)));
    }
    if (m_downSrbs.empty()) { // the mip chain never changes
        for (int k = 1; k < m_levelCount; ++k) {
            SrbBuilder b;
            b.ubuf(0, cs, m_downUbos[k - 1].get())
                .sampled(1, cs, m_levels[k - 1].get(), m_nearest.get())
                .store(2, cs, m_levels[k].get());
            m_downSrbs.push_back(b.build(r, QStringLiteral("downsample_%1").arg(k)));
        }
    }
    m_srbsDirty = false;
}

bool CompositeRenderer::ensureDisplayPipeline()
{
    const int sc = renderTarget()->sampleCount();
    if (m_displayPipe && m_displaySampleCount == sc)
        return true;
    retire(m_displayPipe);
    retire(m_displaySrb);
    QRhi *r = m_rhi;
    QVarLengthArray<QRhiShaderResourceBinding, 9> b;
    b.append(QRhiShaderResourceBinding::uniformBuffer(0, QRhiShaderResourceBinding::FragmentStage, m_viewUbo.get()));
    for (int i = 0; i < kDisplayLevels; ++i)
        b.append(QRhiShaderResourceBinding::sampledTexture(1 + i, QRhiShaderResourceBinding::FragmentStage,
                                                           m_levels[std::min(i, m_levelCount - 1)].get(), m_linear.get()));
    m_displaySrb.reset(r->newShaderResourceBindings());
    m_displaySrb->setBindings(b.cbegin(), b.cend());
    m_displaySrb->create();
    m_displayPipe.reset(r->newGraphicsPipeline());
    m_displayPipe->setShaderStages({{QRhiShaderStage::Vertex, loadShader(QStringLiteral(":/shaders/display.vert.qsb"))},
                                    {QRhiShaderStage::Fragment, loadShader(QStringLiteral(":/shaders/display.frag.qsb"))}});
    m_displayPipe->setVertexInputLayout({});
    m_displayPipe->setSampleCount(sc);
    m_displayPipe->setShaderResourceBindings(m_displaySrb.get());
    m_displayPipe->setRenderPassDescriptor(renderTarget()->renderPassDescriptor());
    if (!m_displayPipe->create()) {
        m_displayPipe.reset();
        return false;
    }
    m_displaySampleCount = sc;
    return true;
}

qint64 CompositeRenderer::cacheSlices() const
{
    qint64 n = 0;
    for (const CacheArray &a : m_arrays)
        n += a.slices;
    return n;
}

bool CompositeRenderer::addArray(int slices)
{
    if (int(m_arrays.size()) >= kMaxArrays || slices <= 0)
        return false;
    CacheArray a;
    a.slices = std::min(slices, m_maxSlices);
    a.tex.reset(m_rhi->newTextureArray(QRhiTexture::RGBA8, a.slices, QSize(kTile, kTile)));
    if (!a.tex->create())
        return false;
    const int idx = int(m_arrays.size());
    for (int s = a.slices - 1; s >= 0; --s)
        m_free.push_back((idx << 16) | s);
    m_arrays.push_back(std::move(a));
    m_srbsDirty = true;
    SpikeLog::write(QStringLiteral("cache_grow"),
                    {{"renderer", m_id}, {"array", idx}, {"slices", m_arrays.back().slices},
                     {"cache_mib", double(cacheSlices() * kTileBytes) / (1024.0 * 1024.0)}});
    return true;
}

int CompositeRenderer::allocSlot()
{
    if (m_free.empty() && int(m_arrays.size()) < kMaxArrays) {
        const qint64 wanted = std::max<qint64>(64, m_nonEmptyTiles - cacheSlices());
        addArray(int(std::min<qint64>(m_maxSlices, (wanted + 63) / 64 * 64)));
    }
    if (!m_free.empty()) {
        const int s = m_free.back();
        m_free.pop_back();
        return s;
    }
    // Evict the least recently used tile that is not needed by this frame's batch.
    if (m_lru.empty())
        return -1;
    const quint64 key = m_lru.front();
    auto it = m_resident.find(key);
    if (it == m_resident.end() || it->second.lastUsed >= m_renderCount)
        return -1; // everything resident is pinned by this frame
    const int slot = it->second.slot;
    m_lru.pop_front();
    m_resident.erase(it);
    m_owner.erase(slot);
    return slot;
}

void CompositeRenderer::trimToVisible(QRhiResourceUpdateBatch *u, const std::vector<int> &visible)
{
    const int tileCount = m_cfg.doc->tileCount();
    const std::unordered_set<int> vis(visible.begin(), visible.end());
    std::vector<std::pair<quint64, int>> keep; // key, old slot
    for (const auto &[key, res] : m_resident) {
        if (vis.count(int(key % quint64(tileCount))))
            keep.emplace_back(key, res.slot);
    }
    const qint64 slicesBefore = cacheSlices();
    std::vector<CacheArray> old = std::move(m_arrays);
    m_arrays.clear();
    m_free.clear();
    m_resident.clear();
    m_owner.clear();
    m_lru.clear();
    // New arrays sized for what is kept, then a GPU copy of each kept slice (no re-upload).
    qint64 remaining = qint64(keep.size());
    while (remaining > 0 && int(m_arrays.size()) < kMaxArrays) {
        const int n = int(std::min<qint64>(m_maxSlices, (remaining + 63) / 64 * 64));
        if (!addArray(n))
            break;
        remaining -= n;
    }
    int copied = 0;
    for (const auto &[key, oldSlot] : keep) {
        if (m_free.empty())
            break;
        const int slot = m_free.back();
        m_free.pop_back();
        QRhiTextureCopyDescription d;
        d.setPixelSize(QSize(kTile, kTile));
        d.setSourceLayer(oldSlot & 0xFFFF);
        d.setDestinationLayer(slot & 0xFFFF);
        u->copyTexture(m_arrays[slot >> 16].tex.get(), old[oldSlot >> 16].tex.get(), d);
        m_lru.push_back(key);
        m_resident[key] = Resident{slot, m_renderCount, std::prev(m_lru.end())};
        m_owner[slot] = key;
        ++copied;
    }
    for (CacheArray &a : old)
        retire(a.tex);
    m_srbsDirty = true;
    TrimStat ts;
    ts.tNs = CanvasHub::instance()->elapsedNs();
    ts.frame = CanvasHub::instance()->frame.load() + 1;
    ts.slicesBefore = slicesBefore;
    ts.slicesAfter = cacheSlices();
    ts.slicesCopied = copied;
    CanvasHub::instance()->addTrimStat(ts);
    SpikeLog::write(QStringLiteral("trim"),
                    {{"renderer", m_id}, {"frame", CanvasHub::instance()->frame.load() + 1},
                     {"visible_tiles", int(visible.size())}, {"slices_before", slicesBefore},
                     {"slices_after", cacheSlices()}, {"slices_copied", copied},
                     {"cache_mib_before", double(slicesBefore * kTileBytes) / 1048576.0},
                     {"cache_mib_after", double(cacheSlices() * kTileBytes) / 1048576.0}});
}

void CompositeRenderer::setupInfoQueue()
{
    releaseInfoQueue();
    const bool requested = qEnvironmentVariableIntValue("QSG_RHI_DEBUG_LAYER") != 0;
#ifdef Q_OS_WIN
    const QRhiNativeHandles *nh = m_rhi->nativeHandles();
    if (m_rhi->backend() == QRhi::D3D11 && nh) {
        auto *dev = static_cast<ID3D11Device *>(static_cast<const QRhiD3D11NativeHandles *>(nh)->dev);
        ID3D11InfoQueue *q = nullptr;
        if (dev && SUCCEEDED(dev->QueryInterface(__uuidof(ID3D11InfoQueue), reinterpret_cast<void **>(&q)))) {
            m_infoQueue = q;
            m_infoQueueKind = 1;
        }
    } else if (m_rhi->backend() == QRhi::D3D12 && nh) {
        auto *dev = static_cast<ID3D12Device *>(static_cast<const QRhiD3D12NativeHandles *>(nh)->dev);
        ID3D12InfoQueue *q = nullptr;
        if (dev && SUCCEEDED(dev->QueryInterface(__uuidof(ID3D12InfoQueue), reinterpret_cast<void **>(&q)))) {
            m_infoQueue = q;
            m_infoQueueKind = 2;
        }
    }
#endif
    SpikeLog::write(QStringLiteral("debug_layer"),
                    {{"renderer", m_id}, {"backend", QString::fromLatin1(m_rhi->backendName())},
                     {"requested", requested}, {"info_queue", m_infoQueue != nullptr}});
}

void CompositeRenderer::releaseInfoQueue()
{
#ifdef Q_OS_WIN
    if (m_infoQueue) {
        drainInfoQueue();
        if (m_infoQueueKind == 1)
            static_cast<ID3D11InfoQueue *>(m_infoQueue)->Release();
        else
            static_cast<ID3D12InfoQueue *>(m_infoQueue)->Release();
    }
#endif
    m_infoQueue = nullptr;
    m_infoQueueKind = 0;
}

void CompositeRenderer::drainInfoQueue()
{
#ifdef Q_OS_WIN
    if (!m_infoQueue)
        return;
    auto logMsg = [&](int severity, int id, int category, const char *text, bool hazardId) {
        if (severity <= 2)
            ++CanvasHub::instance()->d3dWarnings;
        if (severity > 2 || m_msgLogged >= 400)
            return; // info/message severities are not logged one by one
        ++m_msgLogged;
        SpikeLog::write(QStringLiteral("d3d_msg"),
                        {{"renderer", m_id}, {"api", m_infoQueueKind == 1 ? "D3D11" : "D3D12"},
                         {"severity", severityName(severity)}, {"id", id}, {"category", category},
                         {"id_name", hazardId ? QStringLiteral("HAZARD") : QString()},
                         {"text", QString::fromLocal8Bit(text)}});
    };
    if (m_infoQueueKind == 1) {
        auto *q = static_cast<ID3D11InfoQueue *>(m_infoQueue);
        const UINT64 n = q->GetNumStoredMessages();
        for (UINT64 i = 0; i < n; ++i) {
            SIZE_T len = 0;
            if (FAILED(q->GetMessage(i, nullptr, &len)) || !len)
                continue;
            QByteArray buf(qsizetype(len), Qt::Uninitialized);
            auto *m = reinterpret_cast<D3D11_MESSAGE *>(buf.data());
            if (SUCCEEDED(q->GetMessage(i, m, &len)))
                logMsg(int(m->Severity), int(m->ID), int(m->Category), m->pDescription, isD3D11HazardId(m->ID));
        }
        q->ClearStoredMessages();
    } else {
        auto *q = static_cast<ID3D12InfoQueue *>(m_infoQueue);
        const UINT64 n = q->GetNumStoredMessages();
        for (UINT64 i = 0; i < n; ++i) {
            SIZE_T len = 0;
            if (FAILED(q->GetMessage(i, nullptr, &len)) || !len)
                continue;
            QByteArray buf(qsizetype(len), Qt::Uninitialized);
            auto *m = reinterpret_cast<D3D12_MESSAGE *>(buf.data());
            if (SUCCEEDED(q->GetMessage(i, m, &len)))
                logMsg(int(m->Severity), int(m->ID), int(m->Category), m->pDescription, false);
        }
        q->ClearStoredMessages();
    }
#endif
}

void CompositeRenderer::initialize(QRhiCommandBuffer *)
{
    const bool rhiChanged = m_rhi != rhi();
    if (rhiChanged) {
        releaseAll();
        m_rhi = rhi();
        m_failed = false;
        SpikeLog::write(QStringLiteral("backend"),
                        {{"backend", QString::fromLatin1(m_rhi->backendName())}, {"renderer", m_id},
                         {"driver", QString::fromUtf8(m_rhi->driverInfo().deviceName)}});
        setupInfoQueue();
        if (!createStatic()) {
            m_failed = true;
            SpikeLog::write(QStringLiteral("error"), {{"what", "canvas resources could not be created"}, {"renderer", m_id}});
        }
    }
    if (!m_failed && !ensureDisplayPipeline()) {
        m_failed = true;
        SpikeLog::write(QStringLiteral("error"), {{"what", "display pipeline create failed"}, {"renderer", m_id}});
    }
    const QSize sz = renderTarget()->pixelSize();
    SpikeLog::write(QStringLiteral("initialize"),
                    {{"count", ++g_initializeCount}, {"renderer", m_id}, {"rhi_changed", rhiChanged},
                     {"width", sz.width()}, {"height", sz.height()}, {"failed", m_failed}});
    drainInfoQueue();
}

void CompositeRenderer::issueReadback(QRhiCommandBuffer *cb, bool first, bool final, int frame)
{
    struct State {
        std::vector<QRhiReadbackResult> results;
        std::atomic<int> remaining{0};
        ProjectionReadbackPtr rb;
    };
    auto st = std::make_shared<State>();
    st->results.resize(std::size_t(m_levelCount));
    st->remaining = m_levelCount;
    st->rb = std::make_shared<ProjectionReadback>();
    st->rb->renderer = m_id;
    st->rb->frame = frame;
    st->rb->first = first;
    st->rb->final = final;
    st->rb->sizes = m_levelSizes;
    st->rb->levels.resize(std::size_t(m_levelCount));
    QRhiResourceUpdateBatch *u = m_rhi->nextResourceUpdateBatch();
    for (int k = 0; k < m_levelCount; ++k) {
        State *raw = st.get();
        st->results[k].completed = [raw, k, keep = std::weak_ptr<State>(st)] {
            raw->rb->levels[k] = std::move(raw->results[k].data);
            if (--raw->remaining == 0) {
                CanvasHub::instance()->postReadback(raw->rb);
                if (auto s = keep.lock())
                    s->rb.reset();
            }
        };
        u->readBackTexture(QRhiReadbackDescription(m_levels[k].get()), &st->results[k]);
    }
    cb->resourceUpdate(u);
    // The State must outlive the callbacks: park it until this renderer goes away (a few per run).
    static QMutex parkMutex;
    static std::vector<std::shared_ptr<void>> parked;
    QMutexLocker lock(&parkMutex);
    parked.push_back(st);
    SpikeLog::write(QStringLiteral("readback_issued"),
                    {{"renderer", m_id}, {"frame", frame}, {"first", first}, {"final", final}, {"levels", m_levelCount}});
}

void CompositeRenderer::render(QRhiCommandBuffer *cb)
{
    ++m_renderCount;
    CanvasHub *hub = CanvasHub::instance();
    const int frame = hub->frame.load() + 1; // the frame being rendered (1-based)
    if (m_rhi->isDeviceLost() && !m_lostLogged) {
        m_lostLogged = true;
        SpikeLog::write(QStringLiteral("device_lost"), {{"renderer", m_id}, {"frame", frame}});
    }
    if (m_failed) {
        cb->beginPass(renderTarget(), QColor(120, 0, 0), {1.0f, 0});
        cb->endPass();
        return;
    }

    const CanvasDocument &doc = *m_cfg.doc;
    const QSize ds = doc.size();
    const QSize px = renderTarget()->pixelSize();
    QRhiResourceUpdateBatch *u = m_rhi->nextResourceUpdateBatch();
    if (!m_staticUploaded) {
        QByteArray lut(256 * 4 * std::max<int>(1, int(doc.luts().size())), '\0');
        for (std::size_t i = 0; i < doc.luts().size(); ++i)
            memcpy(lut.data() + i * kLutBytes, doc.luts()[i].data(), kLutBytes);
        u->uploadTexture(m_lut.get(), QImage(reinterpret_cast<const uchar *>(lut.constData()), 256,
                                             int(lut.size() / kLutBytes), QImage::Format_RGBA8888).copy());
        u->updateDynamicBuffer(m_mainSceneUbo.get(), 0, sizeof(SceneUbo), &m_mainScene);
        u->updateDynamicBuffer(m_flatSceneUbo.get(), 0, sizeof(SceneUbo), &m_flatScene);
        for (int g = 0; g < m_groups; ++g)
            u->updateDynamicBuffer(m_groupSceneUbos[g].get(), 0, sizeof(SceneUbo), &m_groupScenes[g]);
        m_staticUploaded = true;
    }
    if (!m_reserveDone) {
        // Task 8: hold the part of the --gpu-cache-mb budget the bound arrays cannot (see CanvasConfig).
        // Each reserve array gets one zero upload per slice so every allocation is written at least once.
        m_reserveDone = true;
        const qint64 capBytes = qint64(kMaxArrays) * m_maxSlices * kTileBytes;
        qint64 reserveSlices = std::max<qint64>(0, m_cfg.gpuCacheBytes - capBytes) / kTileBytes;
        static const QByteArray zeros(kTileBytes, '\0');
        qint64 made = 0;
        while (reserveSlices > 0) {
            const int n = int(std::min<qint64>(m_maxSlices, reserveSlices));
            std::unique_ptr<QRhiTexture> t(m_rhi->newTextureArray(QRhiTexture::RGBA8, n, QSize(kTile, kTile)));
            if (!t->create())
                break;
            for (int s = 0; s < n; ++s)
                u->uploadTexture(t.get(), QRhiTextureUploadDescription(
                    QRhiTextureUploadEntry(s, 0, QRhiTextureSubresourceUploadDescription(zeros)))); // shared, no copy
            m_reserve.push_back(std::move(t));
            made += n;
            reserveSlices -= n;
        }
        SpikeLog::write(QStringLiteral("cache_reserve"),
                        {{"renderer", m_id}, {"gpu_cache_mib", double(m_cfg.gpuCacheBytes) / 1048576.0},
                         {"bound_cap_mib", double(capBytes) / 1048576.0},
                         {"reserve_arrays", int(m_reserve.size())},
                         {"reserve_mib", double(made * kTileBytes) / 1048576.0}});
    }

    // --- View: zoom and the document position of the item's top-left pixel.
    double zoom = m_cfg.zoom;
    double panX = 0, panY = 0;
    if (m_cfg.golden) {
        zoom = 0.95 * std::min(double(px.width()) / ds.width(), double(px.height()) / ds.height());
        panX = (ds.width() - px.width() / zoom) / 2.0;
        panY = (ds.height() - px.height() / zoom) / 2.0;
    } else if (m_cfg.autoPan) {
        const double t = m_cfg.guiPan ? m_guiPanT : hub->elapsedNs() / 1e9;
        panX = bounce(800.0 * t, std::max(0.0, ds.width() - px.width() / zoom));   // 800,600 -> 1000 px/s
        panY = bounce(600.0 * t, std::max(0.0, ds.height() - px.height() / zoom));
    }
    const int tx0 = std::max(0, int(std::floor(panX / kTile)));
    const int ty0 = std::max(0, int(std::floor(panY / kTile)));
    const int tx1 = std::min(doc.tilesX(), int(std::ceil((panX + px.width() / zoom) / kTile)));
    const int ty1 = std::min(doc.tilesY(), int(std::ceil((panY + px.height() / zoom) / kTile)));
    std::vector<int> visible;
    for (int ty = ty0; ty < ty1; ++ty)
        for (int tx = tx0; tx < tx1; ++tx)
            visible.push_back(ty * doc.tilesX() + tx);

    // --- Trim request (--trim-flag): keep only the visible tiles' layer slices (IMG-040 stand-in).
    const int trims = hub->trimRequests.load();
    if (trims != m_trimSeen) {
        m_trimSeen = trims;
        trimToVisible(u, visible);
    }
    if (m_cfg.recomposite) {
        for (int t : visible) {
            if (m_composited[t]) {
                m_composited[t] = 0;
                --m_compositedCount;
            }
        }
    }

    // --- Output tiles still to composite, nearest to the view centre first.
    std::vector<int> needed;
    if (m_cfg.golden) {
        for (int t = 0; t < doc.tileCount(); ++t)
            if (!m_composited[t])
                needed.push_back(t);
    } else {
        for (int t : visible)
            if (!m_composited[t])
                needed.push_back(t);
    }
    const double cx = (panX + px.width() / zoom / 2.0) / kTile, cy = (panY + px.height() / zoom / 2.0) / kTile;
    std::sort(needed.begin(), needed.end(), [&](int a, int b) {
        auto d = [&](int t) {
            const double dx = t % doc.tilesX() + 0.5 - cx, dy = t / doc.tilesX() + 0.5 - cy;
            return dx * dx + dy * dy;
        };
        return d(a) < d(b);
    });

    // --- Build this frame's batch: upload missing layer tiles (budgeted), pick the jobs.
    const int tileCount = doc.tileCount();
    std::vector<int> jobs;
    std::vector<std::vector<QRhiTextureUploadEntry>> uploads(kMaxArrays);
    qint64 uploadBytes = 0;
    int uploadCount = 0;
    for (int t : needed) {
        if (int(jobs.size()) >= m_cfg.maxTilesPerFrame)
            break;
        const int tx = t % doc.tilesX(), ty = t / doc.tilesX();
        bool ready = true;
        for (int l = 0; l < m_pixelLayers; ++l) {
            const std::uint8_t *data = doc.tile(l, tx, ty);
            if (!data)
                continue;
            const quint64 key = quint64(l) * quint64(tileCount) + quint64(t);
            auto it = m_resident.find(key);
            if (it != m_resident.end()) {
                it->second.lastUsed = m_renderCount;
                m_lru.splice(m_lru.end(), m_lru, it->second.lru);
                continue;
            }
            if (uploadBytes + kTileBytes > m_cfg.uploadBudgetBytes) {
                ready = false;
                break;
            }
            const int slot = allocSlot();
            if (slot < 0) {
                ready = false;
                break;
            }
            m_lru.push_back(key);
            m_resident[key] = Resident{slot, m_renderCount, std::prev(m_lru.end())};
            m_owner[slot] = key;
            uploads[slot >> 16].emplace_back(slot & 0xFFFF, 0,
                                             QRhiTextureSubresourceUploadDescription(data, quint32(kTileBytes)));
            uploadBytes += kTileBytes;
            ++uploadCount;
        }
        if (!ready)
            break;
        jobs.push_back(t);
    }
    for (int a = 0; a < int(m_arrays.size()); ++a) {
        if (uploads[a].empty())
            continue;
        QRhiTextureUploadDescription desc;
        desc.setEntries(uploads[a].cbegin(), uploads[a].cend());
        u->uploadTexture(m_arrays[a].tex.get(), desc);
    }
    if (m_srbsDirty)
        rebuildComputeSrbs();

    // --- Composite + mips.
    if (!jobs.empty()) {
        const int G = m_cfg.variant == 0 ? m_groups : 0; // variants 1 and 2 have no group pass
        const int P = std::max(1, m_pixelLayers);
        std::vector<qint32> slotTable(std::size_t(jobs.size()) * P, -1);
        QByteArray jobData(qsizetype(m_jobStride) * qsizetype(jobs.size()) * (1 + G), '\0');
        auto jobAt = [&](int index) { return reinterpret_cast<JobUbo *>(jobData.data() + qsizetype(index) * m_jobStride); };
        auto region = [&](int j, int g, qint32 *out) {
            const int r = j * G + g;
            out[0] = (r % kAtlasCols) * kTile;
            out[1] = (r / kAtlasCols) * kTile;
        };
        int dx0 = ds.width(), dy0 = ds.height(), dx1 = 0, dy1 = 0;
        for (int j = 0; j < int(jobs.size()); ++j) {
            const int t = jobs[j], tx = t % doc.tilesX(), ty = t / doc.tilesX();
            for (int l = 0; l < m_pixelLayers; ++l) {
                auto it = m_resident.find(quint64(l) * quint64(tileCount) + quint64(t));
                if (it != m_resident.end() && doc.tile(l, tx, ty))
                    slotTable[std::size_t(j) * P + l] = it->second.slot;
            }
            for (int g = 0; g < G; ++g) {
                JobUbo *gj = jobAt(j * G + g);
                gj->job[0] = tx; gj->job[1] = ty; gj->job[2] = j;
                region(j, g, gj->atlasOut);
            }
            JobUbo *mj = jobAt(int(jobs.size()) * G + j);
            mj->job[0] = tx; mj->job[1] = ty; mj->job[2] = j;
            for (int g = 0; g < G; ++g)
                region(j, g, mj->groupOrigin[g]);
            dx0 = std::min(dx0, tx * kTile);
            dy0 = std::min(dy0, ty * kTile);
            dx1 = std::max(dx1, std::min(ds.width(), (tx + 1) * kTile));
            dy1 = std::max(dy1, std::min(ds.height(), (ty + 1) * kTile));
        }
        u->uploadStaticBuffer(m_slotBuf.get(), 0, quint32(slotTable.size() * sizeof(qint32)), slotTable.data());
        u->updateDynamicBuffer(m_jobUbo.get(), 0, quint32(jobData.size()), jobData.constData());

        // Dirty region of each mip level (destination texels covering the level above's region).
        std::vector<QRect> mipRegions;
        int rx0 = dx0, ry0 = dy0, rx1 = dx1, ry1 = dy1;
        for (int k = 1; k < m_levelCount; ++k) {
            const QSize d = m_levelSizes[k];
            rx0 = rx0 / 2;
            ry0 = ry0 / 2;
            rx1 = std::min(d.width(), (rx1 + 1) / 2);
            ry1 = std::min(d.height(), (ry1 + 1) / 2);
            mipRegions.emplace_back(rx0, ry0, std::max(1, rx1 - rx0), std::max(1, ry1 - ry0));
            DownUbo du{};
            du.sizes[0] = m_levelSizes[k - 1].width();
            du.sizes[1] = m_levelSizes[k - 1].height();
            du.sizes[2] = d.width();
            du.sizes[3] = d.height();
            du.region[0] = mipRegions.back().x();
            du.region[1] = mipRegions.back().y();
            du.region[2] = mipRegions.back().width();
            du.region[3] = mipRegions.back().height();
            u->updateDynamicBuffer(m_downUbos[k - 1].get(), 0, sizeof(DownUbo), &du);
        }

        const int groupsXY = kTile / kLocal;
        // Pass 1: every isolated group of every job -> its region of the RGBA32F intermediate.
        if (G > 0) {
            cb->beginComputePass(u);
            u = nullptr;
            cb->setComputePipeline(m_groupPipe.get());
            for (int j = 0; j < int(jobs.size()); ++j) {
                for (int g = 0; g < G; ++g) {
                    if (!assertDisjoint(m_groupSrbs[g], "dispatch:composite_group"))
                        continue;
                    const QRhiCommandBuffer::DynamicOffset off(1, quint32(j * G + g) * m_jobStride);
                    cb->setShaderResources(m_groupSrbs[g].srb.get(), 1, &off);
                    cb->dispatch(groupsXY, groupsXY, 1);
                }
            }
            cb->endComputePass();
        }
        // Pass 2: the parent stack (groups sampled as single layers) -> projection, store-only.
        cb->beginComputePass(u);
        u = nullptr;
        cb->setComputePipeline(m_mainPipe.get());
        for (int j = 0; j < int(jobs.size()); ++j) {
            if (!assertDisjoint(m_mainSrb, "dispatch:composite_main"))
                continue;
            const QRhiCommandBuffer::DynamicOffset off(1, quint32(int(jobs.size()) * G + j) * m_jobStride);
            cb->setShaderResources(m_mainSrb.srb.get(), 1, &off);
            cb->dispatch(groupsXY, groupsXY, 1);
            if (!m_composited[jobs[j]]) {
                m_composited[jobs[j]] = 1;
                ++m_compositedCount;
            }
        }
        cb->endComputePass();
        m_compositedTotal += qint64(jobs.size());
        // Mips: level k from level k-1, each a separate texture, each in its own pass.
        for (int k = 1; k < m_levelCount; ++k) {
            const QRect &rg = mipRegions[std::size_t(k - 1)];
            if (!assertDisjoint(m_downSrbs[std::size_t(k - 1)], "dispatch:downsample"))
                continue;
            cb->beginComputePass();
            cb->setComputePipeline(m_downPipe.get());
            cb->setShaderResources(m_downSrbs[std::size_t(k - 1)].srb.get());
            cb->dispatch((rg.width() + kLocal - 1) / kLocal, (rg.height() + kLocal - 1) / kLocal, 1);
            cb->endComputePass();
        }
    }

    // --- Golden read-backs: each renderer's first complete view, and the final one.
    const bool viewComplete = m_cfg.golden ? m_compositedCount == tileCount
                                           : std::all_of(visible.begin(), visible.end(), [&](int t) { return m_composited[t] != 0; });
    if (viewComplete && !m_firstViewDone) {
        m_firstViewDone = true;
        SpikeLog::write(QStringLiteral("view_complete"),
                        {{"renderer", m_id}, {"frame", frame}, {"renders", m_renderCount},
                         {"tiles", m_cfg.golden ? tileCount : int(visible.size())},
                         {"cache_slices", cacheSlices()}});
        hub->postViewComplete(m_id, frame, m_compositedCount);
        if (m_cfg.golden) {
            bool final = false;
            if (hub->finalReadbackRequested.load() && m_generation == hub->generation.load())
                final = !hub->finalReadbackIssued.exchange(true);
            issueReadback(cb, true, final, frame);
        }
    } else if (m_cfg.golden && viewComplete && hub->finalReadbackRequested.load()
               && m_generation == hub->generation.load() && !hub->finalReadbackIssued.load()) {
        if (!hub->finalReadbackIssued.exchange(true))
            issueReadback(cb, false, true, frame);
    }

    // --- Display.
    if (!u)
        u = m_rhi->nextResourceUpdateBatch();
    ViewUbo v{};
    v.viewport[0] = float(px.width());
    v.viewport[1] = float(px.height());
    v.viewport[2] = float(zoom);
    v.viewport[3] = float(std::min(m_levelCount, kDisplayLevels));
    v.pan[0] = float(panX);
    v.pan[1] = float(panY);
    v.pan[2] = float(ds.width());
    v.pan[3] = float(ds.height());
    u->updateDynamicBuffer(m_viewUbo.get(), 0, sizeof(ViewUbo), &v);
    cb->beginPass(renderTarget(), QColor(46, 46, 46), {1.0f, 0}, u);
    cb->setGraphicsPipeline(m_displayPipe.get());
    cb->setViewport(QRhiViewport(0, 0, float(px.width()), float(px.height())));
    cb->setShaderResources(m_displaySrb.get());
    cb->draw(3);
    cb->endPass();

    FrameStat fs;
    fs.frame = frame;
    fs.tNs = hub->elapsedNs();
    fs.composited = int(jobs.size());
    fs.uploads = uploadCount;
    fs.gpuMs = cb->lastCompletedGpuTime() * 1000.0;
    fs.cacheBytes = cacheSlices() * kTileBytes;
    hub->addFrameStat(fs);
    if (uploadCount || !jobs.empty())
        SpikeLog::write(QStringLiteral("composite"),
                        {{"renderer", m_id}, {"frame", frame}, {"tiles", int(jobs.size())},
                         {"uploads", uploadCount}, {"backlog", int(needed.size() - jobs.size())},
                         {"cache_slices", cacheSlices()}});

    if ((m_cfg.continuous && !m_cfg.guiPan) || needed.size() > jobs.size())
        update();
    drainInfoQueue();
}

} // namespace

// ------------------------------------------------------------------------------------------------
// Item
// ------------------------------------------------------------------------------------------------

CompositeCanvas::CompositeCanvas(QQuickItem *parent)
    : QQuickRhiItem(parent)
{
}

void CompositeCanvas::setPanTime(double t)
{
    if (t == m_panTime)
        return;
    m_panTime = t;
    Q_EMIT panTimeChanged();
    update();
}

QQuickRhiItemRenderer *CompositeCanvas::createRenderer()
{
    return new CompositeRenderer;
}
