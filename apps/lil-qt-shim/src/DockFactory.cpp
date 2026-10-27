#include "lilshim/DockFactory.h"

#include <kddockwidgets/Config.h>

namespace lilshim {

DockFactory::DockFactory() = default;

DockFactory::~DockFactory() = default;

void DockFactory::install()
{
    // KDDockWidgets' Config owns the factory from here on.
    KDDockWidgets::Config::self().setViewFactory(new DockFactory());
}

} // namespace lilshim
