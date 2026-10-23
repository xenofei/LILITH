#pragma once
// --no-activate: every window, including KDDockWidgets' floating windows, is shown without being
// activated (Qt's Windows platform plugin honours the `_q_showWithoutActivating` window property
// with SW_SHOWNOACTIVATE), so scripted runs never take focus from the desktop.

#include "spike_shim_global.h"

#include <QObject>

class SPIKE_SHIM_EXPORT NoActivate : public QObject
{
    Q_OBJECT
public:
    // Installs an application-wide event filter that marks each window before it is shown.
    static void install();

protected:
    bool eventFilter(QObject *watched, QEvent *event) override;
};
