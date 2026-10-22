#pragma once
// Diagnostics for slow or stuck shutdowns: arm() starts a thread that, if the process is still
// alive `seconds` later, writes every other thread's call stack (dbghelp, with the build's PDBs)
// to `path`. It never ends the process itself.

#include "spike_shim_global.h"

#include <QString>

namespace ExitWatchdog {
SPIKE_SHIM_EXPORT void arm(const QString &path, int seconds);
// Call on the GUI thread: if the GUI thread doesn't run its event loop for staleMs, writes every
// other thread's stack to path once (a stalled start, spike 02's 145 s case).
SPIKE_SHIM_EXPORT void watchGuiThread(const QString &path, int staleMs);
}
