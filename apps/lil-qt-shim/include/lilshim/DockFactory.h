#pragma once
// The dock factory: KDDockWidgets' Qt Quick ViewFactory, themed through the suite's own QML
// (APP-001, APP-012): the title bars and the tab bars are Lilith.UiKit's DockTitleBar.qml and
// DockTabBar.qml, which carry the accessible roles and names (APP-156). The rest of
// KDDockWidgets' look follows the application palette the shell sets from its theme.

#include "lilshim/shim_global.h"

#include <kddockwidgets/qtquick/ViewFactory.h>

namespace lilshim {

class LILSHIM_EXPORT DockFactory : public KDDockWidgets::QtQuick::ViewFactory
{
public:
    // KDDockWidgets 2.5.0 added overloads of these that take the dock's userType(). Bringing the
    // base's overloads into scope keeps an override of one form from hiding the other, so a
    // higher warning level never flags this class (the 2.5.0 release notes' fix).
    using KDDockWidgets::QtQuick::ViewFactory::createFloatingWindow;
    using KDDockWidgets::QtQuick::ViewFactory::createGroup;
    using KDDockWidgets::QtQuick::ViewFactory::createTabBar;
    using KDDockWidgets::QtQuick::ViewFactory::createTitleBar;

    DockFactory();
    ~DockFactory() override;

    QUrl titleBarFilename() const override;
    QUrl tabbarFilename() const override;

    // Makes a new factory KDDockWidgets' own. Call it after
    // KDDockWidgets::initFrontend(FrontendType::QtQuick) and before any dock exists.
    static void install();
};

} // namespace lilshim
