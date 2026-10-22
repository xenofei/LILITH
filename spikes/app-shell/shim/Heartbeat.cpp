#include "Heartbeat.h"
#include "SpikeLog.h"

#include <QJsonArray>
#include <QJsonObject>
#include <QMutexLocker>
#include <QGuiApplication>
#include <QQuickWindow>
#include <QScreen>

#include <algorithm>
#include <cmath>

namespace {
// Nearest-rank percentile of a sorted series.
double percentile(const QVector<float> &sorted, double p)
{
    if (sorted.isEmpty())
        return 0;
    const int rank = std::max(1, int(std::ceil(p / 100.0 * sorted.size())));
    return sorted[std::min(rank, int(sorted.size())) - 1];
}

QJsonObject stats(QVector<float> v)
{
    QJsonArray raw;
    for (float x : v)
        raw.append(std::round(double(x) * 1000.0) / 1000.0);
    std::sort(v.begin(), v.end());
    return {{"n", int(v.size())},
            {"p50", percentile(v, 50)},
            {"p95", percentile(v, 95)},
            {"p99", percentile(v, 99)},
            {"max", v.isEmpty() ? 0.0 : double(v.last())},
            {"raw", raw}};
}
} // namespace

Heartbeat *Heartbeat::instance()
{
    static Heartbeat h;
    return &h;
}

void Heartbeat::start(int intervalMs)
{
    m_intervalMs = intervalMs;
    m_clock.start();
    m_lastNs = -1;
    m_timer.setTimerType(Qt::PreciseTimer);
    m_timer.setInterval(intervalMs);
    connect(&m_timer, &QTimer::timeout, this, &Heartbeat::beat, Qt::UniqueConnection);
    m_timer.start();
    SpikeLog::write(QStringLiteral("heartbeat_started"), {{"interval_ms", intervalMs}});
}

void Heartbeat::stop()
{
    m_timer.stop();
}

void Heartbeat::beat()
{
    const qint64 now = m_clock.nsecsElapsed();
    if (m_lastNs >= 0)
        m_lateness.append(float(double(now - m_lastNs) / 1e6 - m_intervalMs));
    m_lastNs = now;
}

void Heartbeat::attach(QObject *window)
{
    auto *w = qobject_cast<QQuickWindow *>(window);
    if (!w)
        return;
    if (!m_clock.isValid())
        m_clock.start();
    m_windows.append(w);
    // frameSwapped is emitted on the render thread: record it there, under the mutex.
    connect(w, &QQuickWindow::frameSwapped, this, [this, w] {
        const qint64 now = m_clock.nsecsElapsed();
        QMutexLocker lock(&m_frameMutex);
        const auto it = m_lastFrameNs.find(w);
        if (it != m_lastFrameNs.end())
            m_frames.append(float(double(now - it.value()) / 1e6));
        m_lastFrameNs[w] = now;
    }, Qt::DirectConnection);
}

void Heartbeat::reset()
{
    m_lateness.clear();
    m_lastNs = -1;
    QMutexLocker lock(&m_frameMutex);
    m_frames.clear();
    m_lastFrameNs.clear();
}

void Heartbeat::report(const QString &label)
{
    QVector<float> frames;
    {
        QMutexLocker lock(&m_frameMutex);
        frames = m_frames;
    }
    // The frame interval can't be shorter than the vsync period, so the screen and its refresh
    // rate go with every report (S0.6 judges the frame bar against it).
    QJsonArray screens;
    for (QObject *o : m_windows) {
        auto *w = qobject_cast<QQuickWindow *>(o);
        QScreen *s = w ? w->screen() : nullptr;
        screens.append(QJsonObject{{"window", o->objectName()},
                                   {"screen", s ? s->name() : QString()},
                                   {"refresh_hz", s ? s->refreshRate() : 0.0},
                                   {"primary", s && s == QGuiApplication::primaryScreen()}});
    }
    SpikeLog::write(QStringLiteral("stall_report"),
                    {{"label", label},
                     {"screens", screens},
                     {"heartbeat_interval_ms", m_intervalMs},
                     {"heartbeat_lateness_ms", stats(m_lateness)},
                     {"frame_interval_ms", stats(frames)}});
    reset();
}
