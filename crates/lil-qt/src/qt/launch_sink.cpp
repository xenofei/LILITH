#include "lil_qt/src/qt/launch_sink.h"
#include "lil_qt/src/qt/boot.cxxqt.h"

#include <QtCore/QCoreApplication>
#include <QtCore/QMetaObject>
#include <QtCore/QMutex>
#include <QtCore/QPointer>
#include <QtCore/QVariant>

namespace lil_qt {

namespace {

QMutex s_lock;
QPointer<QObject> s_window;

} // namespace

void deliverLaunchesTo(QObject *window)
{
    QMutexLocker locker(&s_lock);
    s_window = window;
}

bool launchArrived(std::uint64_t seq, rust::Str json)
{
    QPointer<QObject> window;
    {
        QMutexLocker locker(&s_lock);
        window = s_window;
    }
    QCoreApplication *app = QCoreApplication::instance();
    if (!window || !app)
        return false;
    const QString text = QString::fromUtf8(json.data(), qsizetype(json.size()));
    QMetaObject::invokeMethod(app, [window, text, seq] {
        if (!window)
            return;
        QMetaObject::invokeMethod(window.data(), "handleLaunchJson", Q_ARG(QVariant, QVariant(text)));
        lil_qt::launch_handled(seq);
    }, Qt::QueuedConnection);
    return true;
}

} // namespace lil_qt
