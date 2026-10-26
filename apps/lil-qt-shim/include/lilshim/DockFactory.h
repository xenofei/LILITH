#pragma once
// The dock factory: KDDockWidgets' Qt Quick ViewFactory, which Task 18 themes through the suite's
// own QML (title bars, tabs). Until then it is KDDockWidgets' own look.

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

    // Makes a new factory KDDockWidgets' own. Call it after
    // KDDockWidgets::initFrontend(FrontendType::QtQuick) and before any dock exists.
    static void install();
};

} // namespace lilshim
