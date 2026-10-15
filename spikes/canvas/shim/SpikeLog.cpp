#include "SpikeLog.h"

#include <QElapsedTimer>
#include <QFile>
#include <QImage>
#include <QJsonDocument>
#include <QMutexLocker>
#include <QRecursiveMutex>
#include <QQuickWindow>

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
QElapsedTimer g_clock;
QtMessageHandler g_previousHandler = nullptr;

void messageHandler(QtMsgType type, const QMessageLogContext &ctx, const QString &msg)
{
    const QByteArray category(ctx.category ? ctx.category : "");
    const bool fromQml = category == "qml" || category == "js"
        || category == "qt.scenegraph.general"; // QSG_INFO=1: render loop and graphics API
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
} // namespace

SpikeLog *SpikeLog::instance()
{
    static SpikeLog s;
    return &s;
}

bool SpikeLog::open(const QString &path)
{
    QMutexLocker lock(&g_mutex);
    g_clock.start();
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
    QMutexLocker lock(&g_mutex);
    if (!g_file)
        return;
    QJsonObject o;
    o.insert(QStringLiteral("t_ms"), g_clock.elapsed());
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
#ifdef Q_OS_WIN
    return GetModuleHandleW(reinterpret_cast<LPCWSTR>(dllName.utf16())) != nullptr;
#else
    Q_UNUSED(dllName);
    return false;
#endif
}

bool SpikeLog::grabWindow(QObject *window, const QString &path)
{
    auto *w = qobject_cast<QQuickWindow *>(window);
    const bool ok = w && w->grabWindow().save(path, "PNG");
    write(QStringLiteral("screenshot"), {{"path", path}, {"ok", ok}});
    return ok;
}

qint64 SpikeLog::elapsedMs() const
{
    QMutexLocker lock(&g_mutex);
    return g_clock.isValid() ? g_clock.elapsed() : 0;
}
