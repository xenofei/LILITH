#pragma once
// GUI-thread responsiveness (S0.6). A Qt::PreciseTimer heartbeat on the GUI thread records each
// beat's lateness (measured interval minus the nominal interval, in ms); every attached window's
// frameSwapped (render thread) records the frame interval. report() logs n, p50, p95, p99 and max
// of both series and the raw samples, then clears them.

#include "spike_shim_global.h"

#include <QElapsedTimer>
#include <QHash>
#include <QMutex>
#include <QObject>
#include <QTimer>
#include <QVector>

class SPIKE_SHIM_EXPORT Heartbeat : public QObject
{
    Q_OBJECT
public:
    static Heartbeat *instance();

    Q_INVOKABLE void start(int intervalMs);
    Q_INVOKABLE void stop();
    // Records frameSwapped intervals of this QQuickWindow.
    Q_INVOKABLE void attach(QObject *window);
    // Clears both series (start of a measured window).
    Q_INVOKABLE void reset();
    // Logs "stall_report" with the label, the stats of both series and the raw samples.
    Q_INVOKABLE void report(const QString &label);

private:
    Heartbeat() = default;
    void beat();

    QTimer m_timer;
    QElapsedTimer m_clock;
    qint64 m_lastNs = -1;
    int m_intervalMs = 4;
    QVector<float> m_lateness;

    mutable QMutex m_frameMutex;
    QHash<QObject *, qint64> m_lastFrameNs;
    QVector<float> m_frames;
};
