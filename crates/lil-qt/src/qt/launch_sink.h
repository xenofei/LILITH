#pragma once
// Where the bridge's single-instance side (src/qt/boot.rs) hands each launch the window must
// show (APP-014, P4a-R21): the JSON of lil_qt::args::launch_json, which ShellWindow's
// handleLaunchJson takes. lil-image's main.cpp names the sink with setLaunchSink once its window
// is loaded, then calls lil_qt::launches_ready(). Called from the instance's pipe thread: the
// sink queues the launch to the GUI thread itself.

#include <QtCore/QString>

#include "rust/cxx.h"

namespace lil_qt {

using LaunchSink = void (*)(const QString &json);

void setLaunchSink(LaunchSink sink);

// The bridge's side: false when there is no sink yet.
bool launchArrived(rust::Str json);

} // namespace lil_qt
