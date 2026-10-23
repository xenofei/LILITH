#include "SpikeDockFactory.h"

#include <kddockwidgets/Config.h>

SpikeDockFactory::~SpikeDockFactory() = default;

QUrl SpikeDockFactory::titleBarFilename() const
{
    return QUrl(QStringLiteral("qrc:/spike/SpikeTitleBar.qml"));
}

void SpikeDockFactory::install()
{
    KDDockWidgets::Config::self().setViewFactory(new SpikeDockFactory());
}
