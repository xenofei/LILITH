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

// The command line that shows `path` selected in Explorer, or empty when `path` isn't an absolute
// path of an existing .zip file: a diagnostics bundle, in the core's folder or where you saved it.
LILSHIM_EXPORT QString explorerSelectArgument(const QString &path);

// Shows `path` selected in Explorer, launching explorer.exe with arguments and never through a
// shell; a path explorerSelectArgument refuses logs a warning and does nothing.
LILSHIM_EXPORT void showInExplorer(const QString &path);

} // namespace lilshim
