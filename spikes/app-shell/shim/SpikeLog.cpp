#include "SpikeLog.h"
#include "StartupClock.h"

#include <QCoreApplication>
#include <QFile>
#include <QImage>
#include <QJsonDocument>
#include <QMutexLocker>
#include <QQuickWindow>
#include <QRecursiveMutex>
#include <QStandardPaths>
#include <QTimer>

#ifdef Q_OS_WIN
#  ifndef NOMINMAX
#    define NOMINMAX
#  endif
#  ifndef WIN32_LEAN_AND_MEAN
#    define WIN32_LEAN_AND_MEAN
#  endif
#  include <windows.h>
#endif

namespace {
QRecursiveMutex g_mutex; // recursive: a Qt warning raised while writing must not deadlock
QFile *g_file = nullptr;
QtMessageHandler g_previousHandler = nullptr;

QTimer *g_probeTimer = nullptr;
QString g_probeDll;
int g_probeCount = 0;
double g_probeFirstHitMs = -1;

void messageHandler(QtMsgType type, const QMessageLogContext &ctx, const QString &msg)
{
    const QByteArray category(ctx.category ? ctx.category : "");
    const bool fromQml = category == "qml" || category == "js";
    if ((type != QtDebugMsg && type != QtInfoMsg) || fromQml) {
        static const char *names[] = {"debug", "warning", "critical", "fatal", "info"};
        SpikeLog::write(QStringLiteral("qt_msg"),
                        {{"type", QString::fromLatin1(names[type])},
                         {"category", QString::fromLatin1(category)},
                         {"msg", msg}});
    }
    if (g_previousHandler)
        g_previousHandler(type, ctx, msg);
}

bool dllLoaded(const QString &dllName)
{
#ifdef Q_OS_WIN
    return GetModuleHandleW(reinterpret_cast<LPCWSTR>(dllName.utf16())) != nullptr;
#else
    Q_UNUSED(dllName);
    return false;
#endif
}
} // namespace

SpikeLog *SpikeLog::instance()
{
    static SpikeLog s;
    return &s;
}

bool SpikeLog::open(const QString &path)
{
    QMutexLocker lock(&g_mutex);
    delete g_file;
    g_file = new QFile(path);
    if (!g_file->open(QIODevice::WriteOnly | QIODevice::Truncate | QIODevice::Text)) {
        delete g_file;
        g_file = nullptr;
        return false;
    }
    return true;
}

void SpikeLog::write(const QString &event, const QJsonObject &fields)
{
    const double t = StartupClock::msSinceProcessStart();
    QMutexLocker lock(&g_mutex);
    if (!g_file)
        return;
    QJsonObject o;
    o.insert(QStringLiteral("t_ms"), qRound64(t * 1000.0) / 1000.0);
    o.insert(QStringLiteral("event"), event);
    for (auto it = fields.begin(); it != fields.end(); ++it)
        o.insert(it.key(), it.value());
    g_file->write(QJsonDocument(o).toJson(QJsonDocument::Compact));
    g_file->write("\n");
    g_file->flush();
}

void SpikeLog::installMessageHandler()
{
    g_previousHandler = qInstallMessageHandler(messageHandler);
}

void SpikeLog::log(const QString &event, const QVariantMap &fields)
{
    write(event, QJsonObject::fromVariantMap(fields));
}

bool SpikeLog::moduleLoaded(const QString &dllName) const
{
    return dllLoaded(dllName);
}

bool SpikeLog::grabWindow(QObject *window, const QString &path)
{
    auto *w = qobject_cast<QQuickWindow *>(window);
    const bool ok = w && w->grabWindow().save(path, "PNG");
    write(QStringLiteral("screenshot"), {{"path", path}, {"ok", ok}});
    return ok;
}

double SpikeLog::elapsedMs() const
{
    return StartupClock::msSinceProcessStart();
}

QString SpikeLog::localDataDir() const
{
    return QStandardPaths::writableLocation(QStandardPaths::AppLocalDataLocation);
}

void SpikeLog::startModuleProbe(const QString &dllName, int intervalMs)
{
    stopModuleProbe();
    g_probeDll = dllName;
    g_probeCount = 0;
    g_probeFirstHitMs = -1;
    g_probeTimer = new QTimer(this);
    g_probeTimer->setTimerType(Qt::PreciseTimer);
    connect(g_probeTimer, &QTimer::timeout, this, [] {
        ++g_probeCount;
        if (g_probeFirstHitMs < 0 && dllLoaded(g_probeDll))
            g_probeFirstHitMs = StartupClock::msSinceProcessStart();
    });
    // The first probe runs at once.
    ++g_probeCount;
    if (dllLoaded(g_probeDll))
        g_probeFirstHitMs = StartupClock::msSinceProcessStart();
    g_probeTimer->start(intervalMs);
}

void SpikeLog::stopModuleProbe()
{
    if (!g_probeTimer)
        return;
    g_probeTimer->stop();
    g_probeTimer->deleteLater();
    g_probeTimer = nullptr;
    write(QStringLiteral("module_probe"),
          {{"dll", g_probeDll},
           {"probes", g_probeCount},
           {"first_non_null_t_ms", g_probeFirstHitMs},
           {"loaded_now", dllLoaded(g_probeDll)}});
}

void SpikeLog::quit(const QString &reason, int code)
{
    write(QStringLiteral("quit_requested"), {{"reason", reason}, {"code", code}});
    QCoreApplication::exit(code);
}
