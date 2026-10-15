#include "SpikeCanvas.h"
#include "SpikeLog.h"

#include <QFile>
#include <QPointer>
#include <rhi/qrhi.h>

#include <atomic>
#include <memory>

namespace {

constexpr int kTexSize = 256;   // storage texture is kTexSize x kTexSize RGBA16F
constexpr int kLocalSize = 16;  // must match local_size_x/y in fill.comp

// Process-wide counters: they keep counting across renderer instances.
std::atomic<int> g_initializeCount{0};
std::atomic<int> g_rendererCount{0};

QShader loadShader(const QString &path)
{
    QFile f(path);
    if (f.open(QIODevice::ReadOnly))
        return QShader::fromSerialized(f.readAll());
    SpikeLog::write(QStringLiteral("error"), {{"what", "shader not found"}, {"path", path}});
    return {};
}

class SpikeCanvasRenderer : public QQuickRhiItemRenderer
{
public:
    SpikeCanvasRenderer() : m_id(++g_rendererCount)
    {
        SpikeLog::write(QStringLiteral("renderer_created"), {{"renderer", m_id}});
    }
    ~SpikeCanvasRenderer() override
    {
        SpikeLog::write(QStringLiteral("renderer_destroyed"), {{"renderer", m_id}});
    }

protected:
    void initialize(QRhiCommandBuffer *cb) override;
    void synchronize(QQuickRhiItem *item) override;
    void render(QRhiCommandBuffer *cb) override;

private:
    void releaseAll();
    bool createResources();

    const int m_id;
    QRhi *m_rhi = nullptr;
    int m_sampleCount = 1;
    QColor m_clearColor;
    bool m_needCompute = false;
    int m_pendingCount = 0; // last initialize() count not yet reported to the item
    QString m_backend;

    std::unique_ptr<QRhiTexture> m_storageTex;
    std::unique_ptr<QRhiSampler> m_sampler;
    std::unique_ptr<QRhiShaderResourceBindings> m_computeSrb;
    std::unique_ptr<QRhiComputePipeline> m_computePipeline;
    std::unique_ptr<QRhiShaderResourceBindings> m_srb;
    std::unique_ptr<QRhiGraphicsPipeline> m_pipeline;
};

void SpikeCanvasRenderer::releaseAll()
{
    m_pipeline.reset();
    m_srb.reset();
    m_computePipeline.reset();
    m_computeSrb.reset();
    m_sampler.reset();
    m_storageTex.reset();
}

bool SpikeCanvasRenderer::createResources()
{
    QRhi *r = rhi();
    if (!r->isFeatureSupported(QRhi::Compute)) {
        SpikeLog::write(QStringLiteral("error"), {{"what", "QRhi::Compute not supported"}});
        return false;
    }
    if (!r->isTextureFormatSupported(QRhiTexture::RGBA16F, QRhiTexture::UsedWithLoadStore)) {
        SpikeLog::write(QStringLiteral("error"), {{"what", "RGBA16F load/store not supported"}});
        return false;
    }

    m_storageTex.reset(r->newTexture(QRhiTexture::RGBA16F, QSize(kTexSize, kTexSize), 1,
                                     QRhiTexture::UsedWithLoadStore));
    m_storageTex->create();

    m_sampler.reset(r->newSampler(QRhiSampler::Linear, QRhiSampler::Linear, QRhiSampler::None,
                                  QRhiSampler::ClampToEdge, QRhiSampler::ClampToEdge));
    m_sampler->create();

    // Compute: writes the storage texture (and reads nothing).
    m_computeSrb.reset(r->newShaderResourceBindings());
    m_computeSrb->setBindings({QRhiShaderResourceBinding::imageStore(
        0, QRhiShaderResourceBinding::ComputeStage, m_storageTex.get(), 0)});
    m_computeSrb->create();

    m_computePipeline.reset(r->newComputePipeline());
    m_computePipeline->setShaderStage(
        {QRhiShaderStage::Compute, loadShader(QStringLiteral(":/shaders/fill.comp.qsb"))});
    m_computePipeline->setShaderResourceBindings(m_computeSrb.get());
    if (!m_computePipeline->create())
        return false;

    // Graphics: one full-viewport triangle that samples the storage texture.
    m_srb.reset(r->newShaderResourceBindings());
    m_srb->setBindings({QRhiShaderResourceBinding::sampledTexture(
        1, QRhiShaderResourceBinding::FragmentStage, m_storageTex.get(), m_sampler.get())});
    m_srb->create();

    m_pipeline.reset(r->newGraphicsPipeline());
    m_pipeline->setShaderStages(
        {{QRhiShaderStage::Vertex, loadShader(QStringLiteral(":/shaders/blit.vert.qsb"))},
         {QRhiShaderStage::Fragment, loadShader(QStringLiteral(":/shaders/blit.frag.qsb"))}});
    m_pipeline->setVertexInputLayout({}); // positions come from gl_VertexIndex
    m_pipeline->setSampleCount(m_sampleCount);
    m_pipeline->setShaderResourceBindings(m_srb.get());
    m_pipeline->setRenderPassDescriptor(renderTarget()->renderPassDescriptor());
    return m_pipeline->create();
}

void SpikeCanvasRenderer::initialize(QRhiCommandBuffer *)
{
    const bool rhiChanged = m_rhi != rhi();
    if (rhiChanged) {
        releaseAll();
        m_rhi = rhi();
        m_backend = QString::fromLatin1(m_rhi->backendName());
        SpikeLog::write(QStringLiteral("backend"),
                        {{"backend", m_backend}, {"renderer", m_id},
                         {"driver", QString::fromUtf8(m_rhi->driverInfo().deviceName)}});
    }
    if (m_pipeline && m_sampleCount != renderTarget()->sampleCount())
        releaseAll();
    m_sampleCount = renderTarget()->sampleCount();

    bool created = false;
    if (!m_pipeline) {
        created = createResources();
        m_needCompute = created;
    }

    const int count = ++g_initializeCount;
    m_pendingCount = count;
    const QSize sz = renderTarget()->pixelSize();
    SpikeLog::write(QStringLiteral("initialize"),
                    {{"count", count}, {"renderer", m_id}, {"rhi_changed", rhiChanged},
                     {"resources_created", created}, {"width", sz.width()}, {"height", sz.height()}});
    update(); // make sure synchronize() runs again so the item hears about it
}

void SpikeCanvasRenderer::synchronize(QQuickRhiItem *item)
{
    auto *canvas = static_cast<SpikeCanvas *>(item);
    m_clearColor = canvas->clearColor();
    if (m_pendingCount) {
        canvas->reportInitialized(m_pendingCount, m_backend);
        m_pendingCount = 0;
    }
}

void SpikeCanvasRenderer::render(QRhiCommandBuffer *cb)
{
    if (!m_pipeline) { // resources failed: still clear so the item is not garbage
        cb->beginPass(renderTarget(), m_clearColor, {1.0f, 0});
        cb->endPass();
        return;
    }
    if (m_needCompute) {
        cb->beginComputePass();
        cb->setComputePipeline(m_computePipeline.get());
        cb->setShaderResources(m_computeSrb.get());
        cb->dispatch(kTexSize / kLocalSize, kTexSize / kLocalSize, 1);
        cb->endComputePass();
        m_needCompute = false;
        SpikeLog::write(QStringLiteral("compute_dispatch"),
                        {{"renderer", m_id}, {"groups_x", kTexSize / kLocalSize},
                         {"groups_y", kTexSize / kLocalSize}, {"texture", "256x256 RGBA16F"}});
    }

    const QSize sz = renderTarget()->pixelSize();
    const float side = 0.8f * float(qMin(sz.width(), sz.height()));
    cb->beginPass(renderTarget(), m_clearColor, {1.0f, 0});
    cb->setGraphicsPipeline(m_pipeline.get());
    cb->setViewport(QRhiViewport((float(sz.width()) - side) / 2, (float(sz.height()) - side) / 2,
                                 side, side));
    cb->setShaderResources(m_srb.get());
    cb->draw(3);
    cb->endPass();
}

} // namespace

SpikeCanvas::SpikeCanvas(QQuickItem *parent) : QQuickRhiItem(parent) {}

void SpikeCanvas::setClearColor(const QColor &c)
{
    if (c == m_clearColor)
        return;
    m_clearColor = c;
    emit clearColorChanged();
    update();
}

void SpikeCanvas::reportInitialized(int processWideCount, const QString &backend)
{
    // Render thread, GUI thread blocked: store, then emit on the GUI thread.
    QPointer<SpikeCanvas> self(this);
    QMetaObject::invokeMethod(
        this,
        [self, processWideCount, backend] {
            if (!self)
                return;
            self->m_initializeCount = processWideCount;
            self->m_backend = backend;
            emit self->rendererInitialized(processWideCount, backend);
        },
        Qt::QueuedConnection);
}

QQuickRhiItemRenderer *SpikeCanvas::createRenderer()
{
    return new SpikeCanvasRenderer;
}
