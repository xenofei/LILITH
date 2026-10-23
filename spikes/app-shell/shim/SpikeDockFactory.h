#pragma once
// The dock factory (S0.1): KDDockWidgets' Qt Quick ViewFactory, themed through our own QML.
// Only the title bar is replaced (qrc:/spike/SpikeTitleBar.qml); tabs, separators and floating
// windows stay KDDockWidgets' own, so S0.7 reads its tab bar as shipped.

#include "spike_shim_global.h"

#include <kddockwidgets/qtquick/ViewFactory.h>

class SPIKE_SHIM_EXPORT SpikeDockFactory : public KDDockWidgets::QtQuick::ViewFactory
{
public:
    ~SpikeDockFactory() override;
    QUrl titleBarFilename() const override;

    // Installs a new factory into KDDockWidgets::Config (call after initFrontend()).
    static void install();
};
