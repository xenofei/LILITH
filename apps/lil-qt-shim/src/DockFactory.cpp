#include "lilshim/DockFactory.h"

#include <kddockwidgets/Config.h>

namespace lilshim {

DockFactory::DockFactory() = default;

DockFactory::~DockFactory() = default;

// Files of the static Lilith.UiKit module, which every exe with docks links.
QUrl DockFactory::titleBarFilename() const
{
    return QUrl(QStringLiteral("qrc:/qt/qml/Lilith/UiKit/qml/shell/DockTitleBar.qml"));
}

QUrl DockFactory::tabbarFilename() const
{
    return QUrl(QStringLiteral("qrc:/qt/qml/Lilith/UiKit/qml/shell/DockTabBar.qml"));
}

void DockFactory::install()
{
    // KDDockWidgets' Config owns the factory from here on.
    KDDockWidgets::Config::self().setViewFactory(new DockFactory());
}

} // namespace lilshim
