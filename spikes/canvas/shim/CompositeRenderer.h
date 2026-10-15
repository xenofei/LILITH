#pragma once
// The canvas compositing spike's GPU side (Task 3: IMG-030, IMG-031, IMG-032, IMG-035).
//
// CompositeCanvas is a QQuickRhiItem. Its renderer keeps:
// - a GPU tile cache: RGBA8 2D-array textures of 128x128 slices (no sRGB flag, IMG-035), bound
//   only as sampled textures; a storage-buffer slot table maps (layer, tile) -> (array, slice);
// - the projection: one RGBA8 storage texture covering the document, plus each mip level as a
//   separate texture (IMG-032);
// - the group intermediate: an RGBA32F atlas with one 128x128 region per (tile, group) of a batch.
// Per frame it uploads a capped batch of missing layer tiles (visible first), runs pass 1 (each
// isolated group -> intermediate) and pass 2 (parent -> projection), one dispatch per output tile,
// regenerates the dirty region of each mip level from the level above, and draws the view.
//
// CanvasHub is the process-wide link between the renderer (render thread) and the app (GUI thread):
// configuration, frame counter, trim and read-back requests, hazard-check counters, results.

#include "CanvasDocument.h"
#include "spike_shim_global.h"

#include <QByteArray>
#include <QElapsedTimer>
#include <QMutex>
#include <QObject>
#include <QQuickRhiItem>
#include <QSize>

#include <atomic>
#include <memory>
#include <vector>

// GPU read-back of the projection and every mip level (level 0 = projection), RGBA8, tight rows.
struct ProjectionReadback {
    int renderer = 0;
    int frame = 0;      // frame being rendered when the read-back was recorded (1-based)
    bool first = false; // first complete view of this renderer
    bool final = false; // the read-back requested at the end of the run
    std::vector<QSize> sizes;
    std::vector<QByteArray> levels;
};
using ProjectionReadbackPtr = std::shared_ptr<ProjectionReadback>;
Q_DECLARE_METATYPE(ProjectionReadbackPtr)

struct CanvasConfig {
    std::shared_ptr<const canvas::CanvasDocument> doc;
    bool golden = false;        // fit the document, composite all of it, read back each complete view
    bool autoPan = false;       // pan at 1000 px/s, bouncing at the document edges
    double zoom = 1.0;          // pixels per document pixel (not used by golden mode, which fits)
    bool continuous = false;    // request a new frame after every frame
    bool recomposite = false;   // re-composite every visible tile every frame (composite timing)
    qint64 uploadBudgetBytes = qint64(24) << 20; // layer-tile upload budget per frame
    int maxTilesPerFrame = 64;  // composite batch size (output tiles per frame)
    bool injectHazard = false;  // negative control: the parent pass also samples the projection it writes
    // Task 4 (R5): composite variant.
    //   0 = two-pass (Task 3 / IMG-031: isolated groups -> RGBA32F intermediate, then the parent pass);
    //   1 = single pass, the isolated group accumulated in registers inside the parent dispatch
    //       (no intermediate, no read-write; HLSL SM 5.0, both backends);
    //   2 = in-place read-write: one dispatch per tile, the projection bound as a read-write image
    //       (typed UAV load of RGBA8); the parent backdrop is parked in the projection while the group
    //       accumulates in registers, then read back in place.
    int variant = 0;
    // Composite shaders (group, parent, single, in-place) as DXIL SM 6.6 from dxc instead of DXBC
    // SM 5.0 from fxc. D3D12 only. The mip and display shaders stay DXBC in every variant. fxc accepts
    // variant 2's RGBA8 typed UAV load too (flagged "Typed UAV Load Additional Formats"), so variant 2
    // also exists as SM 5.0 and runs on D3D11.
    bool sm66 = false;
    // Task 4 diagnostic (--gui-pan): the pan position comes from the item's panTime property, which a
    // QML NumberAnimation advances on the GUI thread every frame, so every frame needs a scene-graph
    // sync, as a pan driven by mouse or tablet events would. Default (--auto-pan): the render thread
    // computes the pan from the clock and repaints without syncing with the GUI thread.
    bool guiPan = false;
    // Task 8 (IMG-155 / R2): GPU tile-cache budget the process should hold, in bytes. The real cache is
    // capped by the four bound arrays (4 x TextureArraySizeMax slices of 128x128 RGBA8 = 512 MiB on
    // D3D11); --gpu-cache-mb N adds (N - that cap) of unbound RGBA8 texture arrays, created once and
    // kept for the renderer's lifetime (Task 8 passes 1536 so the process holds the ~1.5 GB the spec assumes).
    qint64 gpuCacheBytes = qint64(512) << 20; // default = the cap, so no reserve (Task 3/4 behaviour)
};

struct FrameStat {
    int frame = 0;
    qint64 tNs = 0;         // CanvasHub::elapsedNs() at the end of render()
    int composited = 0;     // output tiles composited this frame
    int uploads = 0;        // layer tiles uploaded this frame
    double gpuMs = 0.0;     // QRhiCommandBuffer::lastCompletedGpuTime() (needs timestamps)
    qint64 cacheBytes = 0;
};

struct TrimStat {
    qint64 tNs = 0;         // CanvasHub::elapsedNs() when the renderer trimmed
    int frame = 0;
    qint64 slicesBefore = 0, slicesAfter = 0, slicesCopied = 0;
};

class SPIKE_SHIM_EXPORT CanvasHub : public QObject
{
    Q_OBJECT
public:
    static CanvasHub *instance();

    void setConfig(const CanvasConfig &c);
    CanvasConfig config() const;

    std::atomic<int> frame{0};               // frames swapped so far (incremented by the app)
    std::atomic<int> generation{0};          // renderer generation (incremented per recreation)
    std::atomic<bool> finalReadbackRequested{false};
    std::atomic<bool> finalReadbackIssued{false};
    std::atomic<int> trimRequests{0};
    std::atomic<qint64> hazardChecks{0};     // R4 assertion: SRBs checked (pipelines + dispatches)
    std::atomic<qint64> hazardViolations{0};
    std::atomic<qint64> d3dWarnings{0};      // debug-layer messages of severity warning or worse

    qint64 elapsedNs() const { return m_clock.nsecsElapsed(); }
    qint64 startEpochMs() const { return m_startEpochMs; } // wall clock when elapsedNs() was 0
    void addFrameStat(const FrameStat &s);
    std::vector<FrameStat> frameStats() const;
    void addTrimStat(const TrimStat &s);
    std::vector<TrimStat> trimStats() const;

    void postReadback(ProjectionReadbackPtr rb) { Q_EMIT readbackReady(rb); }
    void postViewComplete(int renderer, int frame, int tiles) { Q_EMIT viewComplete(renderer, frame, tiles); }

Q_SIGNALS:
    void readbackReady(ProjectionReadbackPtr rb);
    void viewComplete(int renderer, int frame, int tiles);

private:
    CanvasHub();
    mutable QMutex m_mutex;
    CanvasConfig m_config;
    QElapsedTimer m_clock;
    qint64 m_startEpochMs = 0;
    std::vector<FrameStat> m_stats;
    std::vector<TrimStat> m_trims;
};

class SPIKE_SHIM_EXPORT CompositeCanvas : public QQuickRhiItem
{
    Q_OBJECT
    Q_PROPERTY(double panTime READ panTime WRITE setPanTime NOTIFY panTimeChanged)
public:
    explicit CompositeCanvas(QQuickItem *parent = nullptr);

    double panTime() const { return m_panTime; }
    void setPanTime(double t);

Q_SIGNALS:
    void panTimeChanged();

protected:
    QQuickRhiItemRenderer *createRenderer() override;

private:
    double m_panTime = 0.0; // seconds of panning at 1000 px/s (--gui-pan)
};
