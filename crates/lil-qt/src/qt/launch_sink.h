#pragma once
// Where the bridge's single-instance side (src/qt/boot.rs) hands each launch the window must
// show (APP-014, P4a-R21): the JSON of lil_qt::args::launch_json, which ShellWindow's
// handleLaunchJson takes. lil-image's main.cpp (and the QML test runner) names its window with
// deliverLaunchesTo once it is loaded, then calls lil_qt::launches_ready(). Called from the
// launches' router thread: the launch is queued to the GUI thread, and once the window has
// taken it the bridge is told (lil_qt::launch_handled), so a launch the window never took is
// started again when the instance ends (lil_qt::release_instance).

#include <QtCore/QObject>
#include <QtCore/QString>

#include <cstdint>

#include "rust/cxx.h"

namespace lil_qt {

// Launches go to `window`'s handleLaunchJson(json) on the GUI thread from now on; null stops.
void deliverLaunchesTo(QObject *window);

// The bridge's side: false when there is no window.
bool launchArrived(std::uint64_t seq, rust::Str json);

} // namespace lil_qt
