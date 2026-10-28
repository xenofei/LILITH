#pragma once
// The shell's bundled UI face (APP-012): Inter 4.1, Regular and Bold, compiled into this DLL as a
// Qt resource (fonts/inter/, OFL-1.1; see NOTICE).

#include "lilshim/shim_global.h"

#include <QString>

namespace lilshim {

// Adds the bundled faces to the font database and makes `family` (Theme.fontFamily) the
// application font. Call it after the application object exists and before the first window.
// True when the application font then resolves to `family`. Otherwise the application font is
// left as it was (the system UI font) and the failure is logged, as an `error` event and a Qt
// warning, never silently.
LILSHIM_EXPORT bool applyThemeFont(const QString &family);

} // namespace lilshim
