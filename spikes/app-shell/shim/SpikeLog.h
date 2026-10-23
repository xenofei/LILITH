#pragma once
// JSON-lines event log (`--log <file>`).
// Every line: {"t_ms": <ms since the process was created>, "event": "<name>", ...fields}.
// Thread-safe: the render thread and the GUI thread both write.

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
    // Copies Qt debug/info/warning/critical/fatal messages and QML console output into the log
    // as "qt_msg" (debug and info only from the qml/js categories).
    static void installMessageHandler();

    // QML API (the singleton SpikeShim.SpikeLog).
    Q_INVOKABLE void log(const QString &event, const QVariantMap &fields = {});
    // True if a DLL with this file name is loaded in this process (GetModuleHandleW).
    Q_INVOKABLE bool moduleLoaded(const QString &dllName) const;
    // Saves QQuickWindow::grabWindow() of `window` as a PNG; logs a "screenshot" event.
    Q_INVOKABLE bool grabWindow(QObject *window, const QString &path);
    Q_INVOKABLE double elapsedMs() const;
    // QStandardPaths::AppLocalDataLocation (%LOCALAPPDATA%/<org>/<app>), forward slashes.
    Q_INVOKABLE QString localDataDir() const;
    // Probes GetModuleHandleW(dllName) every `intervalMs` until stopModuleProbe(); logs
    // "module_probe" with the probe count and the time of the first non-null answer.
    Q_INVOKABLE void startModuleProbe(const QString &dllName, int intervalMs);
    Q_INVOKABLE void stopModuleProbe();
    // Stops the app with this exit code after logging "quit_requested".
    Q_INVOKABLE void quit(const QString &reason, int code = 0);

private:
    SpikeLog() = default;
};
