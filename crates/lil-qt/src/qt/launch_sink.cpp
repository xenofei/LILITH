#include "lil_qt/src/qt/launch_sink.h"

#include <atomic>

namespace lil_qt {

namespace {

std::atomic<LaunchSink> s_sink{nullptr};

} // namespace

void setLaunchSink(LaunchSink sink)
{
    s_sink = sink;
}

bool launchArrived(rust::Str json)
{
    const LaunchSink sink = s_sink;
    if (!sink)
        return false;
    sink(QString::fromUtf8(json.data(), qsizetype(json.size())));
    return true;
}

} // namespace lil_qt
