#pragma once
// JSON-lines event log for the spikes (`--log <file>`).
// Every line: {"t_ms": <ms since open()>, "event": "<name>", ...fields}. Thread-safe: the
// canvas renderer writes from the Qt Quick render thread, QML writes from the GUI thread.

#include "spike_shim_global.h"

#include <QJsonObject>
#include <QObject>
#include <QVariantMap>

class SPIKE_SHIM_EXPORT SpikeLog : public QObject
{
    Q_OBJECT
public:
    static SpikeLog *instance();

    // Opens (truncates) the log file. Without a call to open(), write() is a no-op.
    static bool open(const QString &path);
    static void write(const QString &event, const QJsonObject &fields = {});
    // Copies Qt warnings/criticals/fatals and QML console output into the log as "qt_msg".
    static void installMessageHandler();

    // QML API (registered as the singleton SpikeShim.SpikeLog).
    Q_INVOKABLE void log(const QString &event, const QVariantMap &fields = {});
    // True if a DLL with this file name is loaded in this process (Windows GetModuleHandle).
    Q_INVOKABLE bool moduleLoaded(const QString &dllName) const;
    // Saves QQuickWindow::grabWindow() of `window` as a PNG; logs a "screenshot" event.
    Q_INVOKABLE bool grabWindow(QObject *window, const QString &path);
    Q_INVOKABLE qint64 elapsedMs() const;

private:
    SpikeLog() = default;
};
