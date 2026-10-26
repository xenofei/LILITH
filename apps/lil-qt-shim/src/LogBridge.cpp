#include "lilshim/LogBridge.h"

#include <QJsonDocument>
#include <QMutex>
#include <QMutexLocker>

#include <cstdio>

#ifndef NOMINMAX
#  define NOMINMAX
#endif
#ifndef WIN32_LEAN_AND_MEAN
#  define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

namespace {

QMutex s_writeLock;

quint64 ticks(const FILETIME &ft)
{
    return (quint64(ft.dwHighDateTime) << 32) | ft.dwLowDateTime;
}

void writeLine(const QJsonObject &line)
{
    const QByteArray text = QJsonDocument(line).toJson(QJsonDocument::Compact) + '\n';
    QMutexLocker lock(&s_writeLock);
    std::fwrite(text.constData(), 1, size_t(text.size()), stderr);
    std::fflush(stderr);
}

const char *typeName(QtMsgType type)
{
    switch (type) {
    case QtDebugMsg: return "debug";
    case QtInfoMsg: return "info";
    case QtWarningMsg: return "warning";
    case QtCriticalMsg: return "critical";
    case QtFatalMsg: return "fatal";
    }
    return "unknown";
}

void handler(QtMsgType type, const QMessageLogContext &context, const QString &msg)
{
    lilshim::logEvent(QStringLiteral("qt_msg"),
                      {{"type", QString::fromLatin1(typeName(type))},
                       {"category", QString::fromLatin1(context.category ? context.category : "")},
                       {"msg", msg}});
}

} // namespace

namespace lilshim {

void installLogBridge()
{
    qInstallMessageHandler(handler);
}

void logEvent(const QString &event, const QJsonObject &fields)
{
    QJsonObject line{{"t_ms", msSinceProcessStart()}, {"event", event}};
    for (auto it = fields.begin(); it != fields.end(); ++it)
        line.insert(it.key(), it.value());
    writeLine(line);
}

double msSinceProcessStart()
{
    FILETIME created, exited, kernel, user, now;
    if (!GetProcessTimes(GetCurrentProcess(), &created, &exited, &kernel, &user))
        return -1;
    GetSystemTimePreciseAsFileTime(&now);
    // FILETIME counts 100 ns units.
    return double(ticks(now) - ticks(created)) / 10000.0;
}

} // namespace lilshim
