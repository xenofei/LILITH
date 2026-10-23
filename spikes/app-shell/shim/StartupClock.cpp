#include "StartupClock.h"

#include <QElapsedTimer>

#ifdef Q_OS_WIN
#  ifndef NOMINMAX
#    define NOMINMAX
#  endif
#  ifndef WIN32_LEAN_AND_MEAN
#    define WIN32_LEAN_AND_MEAN
#  endif
#  include <windows.h>
#endif

#ifdef Q_OS_WIN
namespace {
quint64 fileTimeTicks(const FILETIME &ft)
{
    return (quint64(ft.dwHighDateTime) << 32) | ft.dwLowDateTime;
}
} // namespace
#endif

double StartupClock::msSinceProcessStart()
{
#ifdef Q_OS_WIN
    FILETIME created, exited, kernel, user, now;
    if (!GetProcessTimes(GetCurrentProcess(), &created, &exited, &kernel, &user))
        return -1;
    GetSystemTimePreciseAsFileTime(&now);
    // FILETIME is in 100 ns units.
    return double(fileTimeTicks(now) - fileTimeTicks(created)) / 10000.0;
#else
    static QElapsedTimer t;
    if (!t.isValid())
        t.start();
    return double(t.nsecsElapsed()) / 1e6;
#endif
}
