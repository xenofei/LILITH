#include "CanvasController.h"
#include "SpikeLog.h"

#include "canvas_bridge/src/cpu_composite.cxx.h"

#include <QCoreApplication>
#include <QDateTime>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QImage>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QMutexLocker>
#include <QQuickWindow>
#include <QTimeZone>

#include <algorithm>
#include <array>
#include <climits>
#include <cmath>
#include <cstring>

using namespace canvas;

namespace {

rust::Slice<const std::uint8_t> bytes(const QByteArray &b)
{
    return {reinterpret_cast<const std::uint8_t *>(b.constData()), std::size_t(b.size())};
}

bool writeFile(const QString &path, const QByteArray &data)
{
    QFile f(path);
    return f.open(QIODevice::WriteOnly | QIODevice::Truncate) && f.write(data) == data.size();
}

// n, p50, p95, p99, max (nearest rank) and the raw samples.
QJsonObject series(std::vector<double> v, bool withSamples = true)
{
    QJsonObject o;
    o.insert(QStringLiteral("n"), qint64(v.size()));
    QJsonArray raw;
    if (withSamples)
        for (double x : v)
            raw.append(x);
    std::sort(v.begin(), v.end());
    auto pct = [&](double p) -> QJsonValue {
        if (v.empty())
            return QJsonValue();
        const std::size_t rank = std::size_t(std::ceil(p / 100.0 * double(v.size())));
        return v[std::min(v.size() - 1, rank == 0 ? 0 : rank - 1)];
    };
    o.insert(QStringLiteral("p50"), pct(50));
    o.insert(QStringLiteral("p95"), pct(95));
    o.insert(QStringLiteral("p99"), pct(99));
    o.insert(QStringLiteral("max"), v.empty() ? QJsonValue() : QJsonValue(v.back()));
    if (withSamples)
        o.insert(QStringLiteral("samples"), raw);
    return o;
}

} // namespace

CanvasController::CanvasController(const Options &o, std::shared_ptr<const CanvasDocument> doc, QObject *parent)
    : QObject(parent), m_opt(o), m_doc(std::move(doc))
{
    m_canvasActive = !m_opt.noCanvas;
    connect(CanvasHub::instance(), &CanvasHub::readbackReady, this, &CanvasController::onReadback, Qt::QueuedConnection);

    if (!m_opt.frametimesPath.isEmpty()) {
        m_heartbeat.setTimerType(Qt::PreciseTimer);
        m_heartbeat.setInterval(4);
        connect(&m_heartbeat, &QTimer::timeout, this, [this] {
            const qint64 now = CanvasHub::instance()->elapsedNs();
            if (m_lastBeatNs >= 0) {
                m_beatNs.push_back(now);
                m_lateMs.push_back(std::max(0.0, double(now - m_lastBeatNs) / 1e6 - 4.0));
            }
            m_lastBeatNs = now;
        });
    }
    if (!m_opt.trimFlag.isEmpty()) {
        m_trimTimer.setInterval(50);
        connect(&m_trimTimer, &QTimer::timeout, this, &CanvasController::pollTrimFlag);
        pollTrimFlag(); // baseline: an existing flag file does not count as a change
        m_trimTimer.start();
    }
    m_watchdog.setSingleShot(true);
    connect(&m_watchdog, &QTimer::timeout, this, [this] {
        SpikeLog::write(QStringLiteral("error"), {{"what", "final read-back did not arrive within 60 s"}});
        finish(3, QStringLiteral("watchdog"));
    });
    if (m_opt.quitAfterMs >= 0)
        QTimer::singleShot(m_opt.quitAfterMs, this, [this] { finish(0, QStringLiteral("quit-after")); });
}

void CanvasController::setCanvasActive(bool on)
{
    if (on == m_canvasActive)
        return;
    m_canvasActive = on;
    Q_EMIT canvasActiveChanged();
}

void CanvasController::attachWindow(QQuickWindow *w)
{
    m_window = w;
    // frameSwapped is emitted on the render thread: timestamp there, count there, then hand the
    // frame number to the GUI thread.
    connect(w, &QQuickWindow::frameSwapped, this, [this] {
        const qint64 now = CanvasHub::instance()->elapsedNs();
        {
            QMutexLocker lock(&m_frameMutex);
            m_swapNs.push_back(now);
        }
        const int n = ++CanvasHub::instance()->frame;
        QMetaObject::invokeMethod(this, [this, n] { onFrame(n); }, Qt::QueuedConnection);
    }, Qt::DirectConnection);
    if (!m_opt.frametimesPath.isEmpty()) {
        // Sync-wait instrumentation (see the header). afterAnimating is emitted on the GUI thread,
        // afterSynchronizing on the render thread while the GUI thread is still blocked.
        connect(w, &QQuickWindow::afterAnimating, this, [this] {
            m_syncRequestNs.store(CanvasHub::instance()->elapsedNs());
        }, Qt::DirectConnection);
        connect(w, &QQuickWindow::afterSynchronizing, this, [this] {
            const qint64 start = m_syncRequestNs.exchange(-1);
            if (start < 0)
                return;
            const qint64 now = CanvasHub::instance()->elapsedNs();
            QMutexLocker lock(&m_frameMutex);
            m_syncWaits.emplace_back(start, now);
        }, Qt::DirectConnection);
        m_heartbeat.start();
    }
}

bool CanvasController::prepareGolden()
{
    QElapsedTimer t;
    t.start();
    const CanvasDocument &doc = *m_doc;
    const QSize ds = doc.size();
    m_cpuL0 = QByteArray(qsizetype(ds.width()) * ds.height() * 4, '\0');
    std::vector<LayerTile> stack;
    auto out = std::make_unique<std::array<std::uint8_t, 65536>>();
    for (int ty = 0; ty < doc.tilesY(); ++ty) {
        for (int tx = 0; tx < doc.tilesX(); ++tx) {
            stack.clear();
            for (const StackEntry &e : doc.stack()) {
                LayerTile lt;
                lt.kind = std::uint8_t(e.kind);
                lt.blend = std::uint8_t(e.blend);
                lt.opacity = e.opacity;
                lt.visible = e.visible;
                if (e.kind == KindPixel) {
                    if (const std::uint8_t *p = doc.tile(e.pixelLayer, tx, ty))
                        lt.pixels = rust::Slice<const std::uint8_t>(p, kTileBytes);
                } else if (e.kind == KindCurves) {
                    lt.pixels = rust::Slice<const std::uint8_t>(doc.luts()[std::size_t(e.lut)].data(), kLutBytes);
                }
                stack.push_back(lt);
            }
            cpu_composite_tile(rust::Slice<const LayerTile>(stack.data(), stack.size()), *out);
            for (int y = 0; y < kTile && ty * kTile + y < ds.height(); ++y) {
                const int w = std::min(kTile, ds.width() - tx * kTile);
                memcpy(m_cpuL0.data() + (qsizetype(ty * kTile + y) * ds.width() + tx * kTile) * 4,
                       out->data() + y * kTile * 4, std::size_t(w) * 4);
            }
        }
    }
    const qint64 cpuMs = t.elapsed();

    QDir().mkpath(m_opt.goldenDir);
    const QDir dir(m_opt.goldenDir);
    bool ok = writeFile(dir.filePath(QStringLiteral("cpu_L0.rgba")), m_cpuL0);
    QJsonArray levels;
    QSize s = ds;
    for (;;) {
        levels.append(QJsonObject{{"width", s.width()}, {"height", s.height()}});
        if (s.width() == 1 && s.height() == 1)
            break;
        s = QSize(std::max(1, (s.width() + 1) / 2), std::max(1, (s.height() + 1) / 2));
    }
    QJsonObject meta{{"width", ds.width()},
                     {"height", ds.height()},
                     {"format", "RGBA8, premultiplied alpha, encoded sRGB values, rows top to bottom, no padding"},
                     {"scene", doc.description()},
                     {"levels", levels},
                     {"files", "cpu_L0.rgba = Rust reference; gpu_first_L<k>.rgba = first complete GPU view; "
                               "gpu_final_L<k>.rgba = read-back at the end of the run"}};
    ok = ok && writeFile(dir.filePath(QStringLiteral("meta.json")), QJsonDocument(meta).toJson());
    QImage(reinterpret_cast<const uchar *>(m_cpuL0.constData()), ds.width(), ds.height(), QImage::Format_RGBA8888_Premultiplied)
        .scaled(256, 256, Qt::IgnoreAspectRatio, Qt::SmoothTransformation)
        .save(dir.filePath(QStringLiteral("cpu_preview.png")));
    SpikeLog::write(QStringLiteral("golden_cpu"),
                    {{"ms", cpuMs}, {"dir", m_opt.goldenDir}, {"ok", ok}, {"scene", doc.description()}});
    return ok;
}

void CanvasController::onFrame(int n)
{
    CanvasHub *hub = CanvasHub::instance();
    const int maxRecreations = m_opt.frames > 0 && m_opt.recreateEvery > 0 ? m_opt.frames / m_opt.recreateEvery : INT_MAX;
    if (m_opt.recreateEvery > 0 && n % m_opt.recreateEvery == 0 && m_recreations < maxRecreations) {
        ++m_recreations;
        ++hub->generation;
        SpikeLog::write(QStringLiteral("recreate"), {{"frame", n}, {"k", m_recreations}});
        setCanvasActive(false); // the Loader destroys the item, and with it the node and renderer
        QTimer::singleShot(0, this, [this] { setCanvasActive(true); });
    }
    if (n % 300 == 0)
        SpikeLog::write(QStringLiteral("frames"), {{"frame", n}});
    if (m_opt.frames > 0 && n >= m_opt.frames && !m_endRequested) {
        m_endRequested = true;
        if (!m_opt.goldenDir.isEmpty()) {
            hub->finalReadbackRequested = true;
            SpikeLog::write(QStringLiteral("final_readback_requested"), {{"frame", n}, {"generation", hub->generation.load()}});
            requestCanvasUpdate();
            m_watchdog.start(60000);
        } else {
            finish(0, QStringLiteral("frames"));
        }
    }
}

void CanvasController::onReadback(ProjectionReadbackPtr rb)
{
    if (!rb || rb->levels.empty())
        return;
    const QByteArray &l0 = rb->levels[0];
    const int projErr = m_cpuL0.isEmpty() ? -1 : int(cpu_max_abs_diff(bytes(l0), bytes(m_cpuL0)));
    qint64 over1 = 0;
    if (!m_cpuL0.isEmpty() && l0.size() == m_cpuL0.size()) {
        for (qsizetype i = 0; i < l0.size(); i += 4) {
            int m = 0;
            for (int c = 0; c < 4; ++c)
                m = std::max(m, std::abs(int(quint8(l0[i + c])) - int(quint8(m_cpuL0[i + c]))));
            over1 += m > 1 ? 1 : 0;
        }
    }
    QJsonArray mipErrors;
    int mipsMax = 0;
    for (std::size_t k = 1; k < rb->levels.size(); ++k) {
        const QSize ps = rb->sizes[k - 1], ls = rb->sizes[k];
        QByteArray ref(qsizetype(ls.width()) * ls.height() * 4, '\0');
        cpu_downsample(bytes(rb->levels[k - 1]), quint32(ps.width()), quint32(ps.height()),
                       rust::Slice<std::uint8_t>(reinterpret_cast<std::uint8_t *>(ref.data()), std::size_t(ref.size())),
                       quint32(ls.width()), quint32(ls.height()));
        const int e = int(cpu_max_abs_diff(bytes(rb->levels[k]), bytes(ref)));
        mipsMax = std::max(mipsMax, e);
        mipErrors.append(e);
    }
    SpikeLog::write(QStringLiteral("golden_compare"),
                    {{"renderer", rb->renderer}, {"frame", rb->frame}, {"first", rb->first}, {"final", rb->final},
                     {"projection_max_abs_diff", projErr}, {"projection_pixels_over_1", over1},
                     {"mips_max_abs_diff", mipsMax}, {"mip_errors", mipErrors},
                     {"pass", projErr >= 0 && projErr <= 1 && mipsMax <= 1}});

    auto save = [&](const QString &prefix) {
        const QDir dir(m_opt.goldenDir);
        for (std::size_t k = 0; k < rb->levels.size(); ++k)
            writeFile(dir.filePath(QStringLiteral("%1_L%2.rgba").arg(prefix).arg(k)), rb->levels[k]);
        if (rb->levels.size() > 2) {
            const QSize s2 = rb->sizes[2];
            QImage(reinterpret_cast<const uchar *>(rb->levels[2].constData()), s2.width(), s2.height(),
                   QImage::Format_RGBA8888_Premultiplied)
                .save(dir.filePath(QStringLiteral("%1_L2.png").arg(prefix)));
        }
    };
    if (!m_opt.goldenDir.isEmpty()) {
        if (rb->first && !m_savedFirst) {
            m_savedFirst = true;
            save(QStringLiteral("gpu_first"));
        }
        if (rb->final) {
            save(QStringLiteral("gpu_final"));
            finish(0, QStringLiteral("final read-back"));
        }
    }
}

void CanvasController::requestCanvasUpdate()
{
    if (!m_window)
        return;
    if (auto *c = m_window->findChild<CompositeCanvas *>())
        c->update();
    m_window->update();
}

void CanvasController::pollTrimFlag()
{
    const QFileInfo fi(m_opt.trimFlag);
    const qint64 mtime = fi.exists() ? fi.lastModified(QTimeZone::UTC).toMSecsSinceEpoch() : -1;
    if (mtime == m_trimMtime)
        return;
    const bool first = !m_trimTimer.isActive();
    m_trimMtime = mtime;
    if (first || mtime < 0)
        return;
    ++m_trimCount;
    ++CanvasHub::instance()->trimRequests;
    SpikeLog::write(QStringLiteral("trim_requested"),
                    {{"count", m_trimCount}, {"frame", CanvasHub::instance()->frame.load()}, {"path", m_opt.trimFlag}});
    requestCanvasUpdate();
}

void CanvasController::writeFrametimes()
{
    CanvasHub *hub = CanvasHub::instance();
    const qint64 measureNs = qint64(std::max(0, m_opt.measureAfterMs)) * 1000000;
    const qint64 endNs = hub->elapsedNs();
    auto r3 = [](double x) { return std::round(x * 1000.0) / 1000.0; };

    std::vector<double> frameMs, warmFrameMs;
    std::vector<std::pair<qint64, qint64>> syncWaits;
    {
        QMutexLocker lock(&m_frameMutex);
        for (std::size_t i = 1; i < m_swapNs.size(); ++i) {
            const double ms = r3(double(m_swapNs[i] - m_swapNs[i - 1]) / 1e6);
            (m_swapNs[i - 1] >= measureNs ? frameMs : warmFrameMs).push_back(ms);
        }
        syncWaits = m_syncWaits;
    }

    // Heartbeats in the window; stalls with their time and their overlap with sync waits.
    std::vector<double> late, warmLate, stalls, warmStalls, stallSyncMs, syncWaitMs;
    QJsonArray stallEvents; // [t_s, late_ms, sync_overlap_ms]
    std::size_t si = 0;
    double stallTotal = 0, stallSyncTotal = 0;
    int stallsMostlySync = 0;
    QJsonArray syncEvents; // [t_s, ms]: when the GUI thread blocked for a sync, and for how long
    for (const auto &w : syncWaits) {
        if (w.first < measureNs)
            continue;
        syncWaitMs.push_back(r3(double(w.second - w.first) / 1e6));
        syncEvents.append(QJsonArray{r3(double(w.first) / 1e9), syncWaitMs.back()});
    }
    for (std::size_t i = 0; i < m_lateMs.size(); ++i) {
        const qint64 t = m_beatNs[i];
        const double l = r3(m_lateMs[i]);
        const bool inWindow = t - qint64(l * 1e6) - 4000000 >= measureNs;
        (inWindow ? late : warmLate).push_back(l);
        if (l <= 4.0)
            continue;
        if (!inWindow) {
            warmStalls.push_back(l);
            continue;
        }
        stalls.push_back(l);
        // The late window: from when the beat was due until it fired.
        const qint64 w0 = t - qint64(l * 1e6), w1 = t;
        while (si < syncWaits.size() && syncWaits[si].second < w0)
            ++si;
        qint64 overlap = 0;
        for (std::size_t k = si; k < syncWaits.size() && syncWaits[k].first < w1; ++k)
            overlap += std::max<qint64>(0, std::min(w1, syncWaits[k].second) - std::max(w0, syncWaits[k].first));
        const double ov = r3(double(overlap) / 1e6);
        stallSyncMs.push_back(ov);
        stallTotal += l;
        stallSyncTotal += ov;
        if (ov >= 0.5 * l)
            ++stallsMostlySync;
        stallEvents.append(QJsonArray{r3(double(t) / 1e9), l, ov});
    }

    std::vector<double> gpuMs, composited, cacheMib, warmGpuMs;
    for (const FrameStat &s : hub->frameStats()) {
        if (s.tNs < measureNs) {
            if (s.gpuMs > 0)
                warmGpuMs.push_back(s.gpuMs);
            continue;
        }
        if (s.gpuMs > 0)
            gpuMs.push_back(r3(s.gpuMs * 1000.0) / 1000.0);
        composited.push_back(s.composited);
        cacheMib.push_back(double(s.cacheBytes) / 1048576.0);
    }
    QJsonArray trims;
    for (const TrimStat &t : hub->trimStats())
        trims.append(QJsonObject{{"t_s", r3(double(t.tNs) / 1e9)},
                                 {"epoch_ms", hub->startEpochMs() + t.tNs / 1000000},
                                 {"frame", t.frame},
                                 {"slices_before", t.slicesBefore},
                                 {"slices_after", t.slicesAfter},
                                 {"slices_copied", t.slicesCopied},
                                 {"cache_mib_before", double(t.slicesBefore * kTileBytes) / 1048576.0},
                                 {"cache_mib_after", double(t.slicesAfter * kTileBytes) / 1048576.0}});

    QJsonObject attribution{
        {"rule", "a stall's late window runs from when the beat was due until it fired; sync_overlap is the part of "
                 "that window during which the GUI thread was blocked in the threaded render loop's sync "
                 "(afterAnimating on the GUI thread -> afterSynchronizing on the render thread)"},
        {"stall_ms_total", r3(stallTotal)},
        {"stall_ms_during_sync_wait", r3(stallSyncTotal)},
        {"fraction_during_sync_wait", stallTotal > 0 ? r3(stallSyncTotal / stallTotal) : QJsonValue()},
        {"stalls_mostly_sync_wait", stallsMostlySync},
        {"stall_sync_overlap_ms", series(stallSyncMs, false)},
        {"stall_events", stallEvents}};

    QJsonObject o{{"generated", QDateTime::currentDateTime().toString(Qt::ISODateWithMs)},
                  {"args", QJsonArray::fromStringList(QCoreApplication::arguments())},
                  {"scene", m_doc->description()},
                  {"backend_env", QString::fromLocal8Bit(qgetenv("QSG_RHI_BACKEND"))},
                  {"render_loop_env", QString::fromLocal8Bit(qgetenv("QSG_RENDER_LOOP"))},
                  {"start_epoch_ms", hub->startEpochMs()},
                  {"measure_after_s", double(measureNs) / 1e9},
                  {"end_s", r3(double(endNs) / 1e9)},
                  {"frame_ms", series(frameMs)},
                  {"heartbeat_late_ms", series(late, false)},
                  {"stall_ms", series(stalls)},
                  {"stall_rule", "a 4 ms GUI-thread heartbeat that fires more than 4 ms late is a stall"},
                  {"sync_wait_ms", series(syncWaitMs, false)},
                  {"sync_wait_events", syncEvents},
                  {"stall_attribution", attribution},
                  {"gpu_frame_ms", series(gpuMs)},
                  {"composited_tiles_per_frame", series(composited, false)},
                  {"cache_mib", series(cacheMib, false)},
                  {"trims", trims},
                  {"warmup", QJsonObject{{"frame_ms", series(warmFrameMs, false)},
                                         {"heartbeat_late_ms", series(warmLate, false)},
                                         {"stall_ms", series(warmStalls, false)},
                                         {"gpu_frame_ms", series(warmGpuMs, false)}}}};
    writeFile(m_opt.frametimesPath, QJsonDocument(o).toJson());
    SpikeLog::write(QStringLiteral("frametimes_written"),
                    {{"path", m_opt.frametimesPath}, {"frames", qint64(frameMs.size())}, {"stalls", qint64(stalls.size())}});
}

void CanvasController::finish(int code, const QString &reason)
{
    if (m_finished)
        return;
    m_finished = true;
    m_watchdog.stop();
    CanvasHub *hub = CanvasHub::instance();
    if (!m_opt.frametimesPath.isEmpty())
        writeFrametimes();
    if (!m_opt.screenshot.isEmpty() && m_window)
        SpikeLog::instance()->grabWindow(m_window, m_opt.screenshot);
    SpikeLog::write(QStringLiteral("hazard_checks"),
                    {{"checks", hub->hazardChecks.load()}, {"violations", hub->hazardViolations.load()},
                     {"d3d_warnings_or_worse", hub->d3dWarnings.load()}});
    SpikeLog::write(QStringLiteral("quit_requested"),
                    {{"reason", reason}, {"code", code}, {"frame", hub->frame.load()}, {"recreations", m_recreations}});
    QCoreApplication::exit(code);
}
