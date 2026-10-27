#pragma once
// The GUI-thread stall watchdog (APP-135; spike 10 §8, follow-ups 3 and 4) and the font warm-up.
//
// startStallWatchdog() runs a heartbeat on the GUI thread and a watcher thread beside it:
// - each stall of the GUI thread longer than kStallLogMs is logged once it ends, as
//   {"event": "gui_stall", "ms"} (spike 10 saw 14 such stalls a minute under ComfyUI load, so
//   they are expected and only logged);
// - while a stall lasts longer than kStallDumpMs, the watcher copies the GUI thread's registers
//   and the top of its stack and logs the return addresses it finds there as
//   {"event": "gui_stall_stack", "ms", "frames": ["<module>+0x<offset>", ...]}, at most once a
//   stall and once every kDumpEveryMs, so the 0.1 to 0.4 s stalls spike 10 couldn't explain
//   leave a trace (its watchdog dumped only after 5 s).
// warmFonts() populates Qt's font database on the global thread pool, off the GUI thread, so the
// first show doesn't (a first show once stalled 10.6 s in DirectWrite's font enumeration).

#include "lilshim/shim_global.h"

namespace lilshim {

constexpr int kStallLogMs = 50;
constexpr int kStallDumpMs = 250;
constexpr int kDumpEveryMs = 60'000;

// Call once, on the GUI thread, after the application object exists.
LILSHIM_EXPORT void startStallWatchdog();

// Call once, after the application object exists and before the QML engine loads the window.
LILSHIM_EXPORT void warmFonts();

} // namespace lilshim
