#pragma once
// Window activation and the shell glue the QML side can't reach (APP-030), and the foreground
// hand-off of a forwarded launch (APP-014).

#include "lilshim/shim_global.h"

#include <QString>

class QWindow;

namespace lilshim {

// Shows `w`. With takeFocus, it is restored if minimized, shown, raised and brought to the front
// with SetForegroundWindow, which works when this process may take the foreground (a second
// launch's AllowSetForegroundWindow gives it that right, APP-014). Without, it is shown without
// being activated (no focus taken) and its taskbar button flashes until it comes to the front
// (APP-065).
LILSHIM_EXPORT void activate(QWindow *w, bool takeFocus);

// Whether `dll` (a file name such as "Qt6WebEngineCore.dll") is loaded in this process
// (GetModuleHandleW). Tests use it to check that WebEngine stays out until its panel loads.
LILSHIM_EXPORT bool isModuleLoaded(const QString &dll);

// Shows `path` selected in Explorer. Not available yet: Task 25 fills it, for paths under the
// diagnostics folder only, launching explorer.exe with arguments and never through a shell.
// Until then it logs a warning and does nothing.
LILSHIM_EXPORT void showInExplorer(const QString &path);

} // namespace lilshim
