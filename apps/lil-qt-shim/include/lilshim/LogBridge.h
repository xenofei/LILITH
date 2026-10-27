#pragma once
// The Qt log bridge. Every Qt message, and every event() the app logs, becomes one JSON line:
// {"t_ms": <ms since the process was created>, "event": "...", ...}. A Qt message is
// {"event": "qt_msg", "type": "debug|info|warning|critical|fatal", "category", "msg"}. Each line
// goes to the sink the app sets (lil-image: its log file through lil_platform::logs, ARCH-117,
// where a Qt message keeps its qt_category) and to stderr, which the deploy check reads.

#include "lilshim/shim_global.h"

#include <QJsonObject>
#include <QString>

namespace lilshim {

// Installs the Qt message handler. Call it right after the application object exists.
LILSHIM_EXPORT void installLogBridge();

// Where each line goes besides stderr: `line` is UTF-8 JSON, `size` bytes, without a newline.
// It may be called from any thread, one line at a time.
using LogSink = void (*)(const char *line, qsizetype size);
LILSHIM_EXPORT void setLogSink(LogSink sink);

// Writes one event line, with `fields` merged in.
LILSHIM_EXPORT void logEvent(const QString &event, const QJsonObject &fields = {});

// Milliseconds since the process was created (GetProcessTimes), the clock spike 10 measured
// first_frame and interactive with (APP-136).
LILSHIM_EXPORT double msSinceProcessStart();

} // namespace lilshim
