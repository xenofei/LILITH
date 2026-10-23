#include "NoActivate.h"

#include <QCoreApplication>
#include <QEvent>
#include <QVariant>
#include <QWindow>

void NoActivate::install()
{
    static NoActivate filter;
    QCoreApplication::instance()->installEventFilter(&filter);
}

bool NoActivate::eventFilter(QObject *watched, QEvent *event)
{
    // QWindow sends QShowEvent to itself before the platform window is shown, and also marks the
    // platform surface creation; either is early enough for the property to take effect.
    if (event->type() == QEvent::Show || event->type() == QEvent::PlatformSurface) {
        if (auto *w = qobject_cast<QWindow *>(watched)) {
            if (!w->property("_q_showWithoutActivating").toBool())
                w->setProperty("_q_showWithoutActivating", true);
        }
    }
    return QObject::eventFilter(watched, event);
}
