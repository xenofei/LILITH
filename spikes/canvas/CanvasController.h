#pragma once
// CanvasController (GUI thread): frame counting, renderer recreation (--recreate-every), the trim
// flag (--trim-flag), frame-time and GUI-stall recording (--frametimes), and the golden check
// (--golden): the CPU reference is built with the Rust compositor at start-up, and every GPU
// read-back the renderer posts is compared with it (and with Rust box-filter downsamples for the
// mip levels), logged, and saved as raw RGBA8 files for run-composite-check.ps1.

#include "CanvasDocument.h"
#include "CompositeRenderer.h"

#include <QElapsedTimer>
#include <QMutex>
#include <QObject>
#include <QPointer>
#include <QTimer>

#include <atomic>
#include <memory>
#include <utility>
#include <vector>

class QQuickWindow;

class CanvasController : public QObject
{
    Q_OBJECT
    Q_PROPERTY(bool canvasActive READ canvasActive NOTIFY canvasActiveChanged)
    Q_PROPERTY(bool guiPan READ guiPan CONSTANT)
public:
    struct Options {
        int frames = -1;          // --frames N: stop after N frames
        int recreateEvery = 0;    // --recreate-every N
        QString goldenDir;        // --golden <dir>
        QString frametimesPath;   // --frametimes <json>
        QString trimFlag;         // --trim-flag <path>
        QString screenshot;       // --screenshot <png>
        int quitAfterMs = -1;     // --quit-after <ms>
        int measureAfterMs = 0;   // --measure-after <ms>: earlier samples are reported as warm-up
        bool noCanvas = false;    // --no-canvas: control run without the canvas item (heartbeat only)
        bool guiPan = false;      // --gui-pan: the pan is animated on the GUI thread (a sync per frame)
    };

    CanvasController(const Options &o, std::shared_ptr<const canvas::CanvasDocument> doc, QObject *parent = nullptr);

    bool canvasActive() const { return m_canvasActive; }
    bool guiPan() const { return m_opt.guiPan; }
    // Builds the CPU reference and writes cpu_L0.rgba + meta.json into the golden directory.
    bool prepareGolden();
    void attachWindow(QQuickWindow *w);

Q_SIGNALS:
    void canvasActiveChanged();

private:
    void setCanvasActive(bool on);
    void onFrame(int n);
    void onReadback(ProjectionReadbackPtr rb);
    void pollTrimFlag();
    void requestCanvasUpdate();
    void finish(int code, const QString &reason);
    void writeFrametimes();

    Options m_opt;
    std::shared_ptr<const canvas::CanvasDocument> m_doc;
    QPointer<QQuickWindow> m_window;
    bool m_canvasActive = true;
    int m_recreations = 0;
    bool m_endRequested = false;
    bool m_finished = false;
    bool m_savedFirst = false;
    QByteArray m_cpuL0;

    // Frame times (render thread writes, GUI thread reads at the end). All times are
    // CanvasHub::elapsedNs(), one clock for the GUI thread, the render thread and the renderer.
    QMutex m_frameMutex;
    std::vector<qint64> m_swapNs;
    // GUI-thread heartbeat (4 ms): fire time and lateness.
    QTimer m_heartbeat;
    qint64 m_lastBeatNs = -1;
    std::vector<qint64> m_beatNs;
    std::vector<double> m_lateMs;
    // Scene-graph sync: the GUI thread is blocked from afterAnimating (GUI thread, just before the
    // threaded render loop posts the sync request and waits) until afterSynchronizing (render thread,
    // end of sync; the GUI thread is released right after). Used to attribute stalls.
    std::atomic<qint64> m_syncRequestNs{-1};
    std::vector<std::pair<qint64, qint64>> m_syncWaits; // guarded by m_frameMutex

    QTimer m_trimTimer;
    qint64 m_trimMtime = -1;
    int m_trimCount = 0;
    QTimer m_watchdog;
};
