#pragma once
// The native side of ConfirmInput (src/qt/confirm_input.rs): a native event filter that gives each
// pointer message to crate::pointer_gate before Qt sees it, with the watched button under the
// cursor as Qt places it now (P4a-R10).

#include <QtCore/QVariant>

#include <cstdint>

namespace lil_qt {

// Watches the QQuickItem in `item`, with a new serial if it was watched already. The filter is
// installed at the first call.
void pointerWatch(const QVariant &item);

void pointerUnwatch(const QVariant &item);

// The item's serial, or 0 when it isn't watched.
std::uint64_t pointerSerial(const QVariant &item);

// Whether the item's window is the foreground window, by Windows itself (always on other platforms).
bool pointerWindowInFront(const QVariant &item);

} // namespace lil_qt
