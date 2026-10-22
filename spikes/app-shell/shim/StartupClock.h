#pragma once
// Milliseconds since this process was created (GetProcessTimes), so start-up timings include the
// loader, DLL initialisation and everything before main() (S0.5).

#include "spike_shim_global.h"

namespace StartupClock {
SPIKE_SHIM_EXPORT double msSinceProcessStart();
}
