#pragma once
// Window activation and the shell glue the QML side can't reach (APP-030). Task 19 adds the
// foreground hand-off of a forwarded launch.

#include "lilshim/shim_global.h"

#include <QString>

class QWindow;

namespace lilshim {

// Shows `w`. With takeFocus, it is shown, raised and activated. Without, it is shown without
// being activated (no focus taken) and its taskbar button flashes until it comes to the front.
LILSHIM_EXPORT void activate(QWindow *w, bool takeFocus);

// Whether `dll` (a file name such as "Qt6WebEngineCore.dll") is loaded in this process
// (GetModuleHandleW). Tests use it to check that WebEngine stays out until its panel loads.
LILSHIM_EXPORT bool isModuleLoaded(const QString &dll);

// Shows `path` selected in Explorer. Not available yet: Task 25 fills it, for paths under the
// diagnostics folder only, launching explorer.exe with arguments and never through a shell.
// Until then it logs a warning and does nothing.
LILSHIM_EXPORT void showInExplorer(const QString &path);

} // namespace lilshim
