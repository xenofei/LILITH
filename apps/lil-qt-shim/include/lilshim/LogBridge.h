#pragma once
// The Qt log bridge. Every Qt message, and every event() the app logs, becomes one JSON line on
// stderr: {"t_ms": <ms since the process was created>, "event": "...", ...}. A Qt message is
// {"event": "qt_msg", "type": "debug|info|warning|critical|fatal", "category", "msg"}. Task 16
// sends the same lines to the app's log file (lil_platform::logs) instead.

#include "lilshim/shim_global.h"

#include <QJsonObject>
#include <QString>

namespace lilshim {

// Installs the Qt message handler. Call it right after the application object exists.
LILSHIM_EXPORT void installLogBridge();

// Writes one event line, with `fields` merged in.
LILSHIM_EXPORT void logEvent(const QString &event, const QJsonObject &fields = {});

// Milliseconds since the process was created (GetProcessTimes), the clock spike 10 measured
// first_frame and interactive with (APP-136).
LILSHIM_EXPORT double msSinceProcessStart();

} // namespace lilshim
