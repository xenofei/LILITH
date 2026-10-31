// The native event filter behind ConfirmInput (P4a-R10). Mouse messages reach a native event filter
// in the event dispatcher, after they leave the thread's queue and before they are dispatched;
// Windows' own sent messages (capture, activation) reach it in the window procedure. It changes
// nothing: every message goes on to Qt. With nothing watched it reads nothing.
#include "lil_qt/src/qt/pointer_filter.h"
#include "lil_qt/src/qt/confirm_input.cxxqt.h"

#include <QtCore/QAbstractNativeEventFilter>
#include <QtCore/QCoreApplication>
#include <QtCore/QList>
#include <QtCore/QPointer>
#include <QtQuick/QQuickItem>
#include <QtQuick/QQuickWindow>

#include <cmath>
#include <optional>

#ifndef NOMINMAX
#  define NOMINMAX
#endif
#ifndef WIN32_LEAN_AND_MEAN
#  define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

#pragma comment(lib, "user32.lib")

namespace lil_qt {

namespace {

struct Watched
{
    QPointer<QQuickItem> item;
    std::uint64_t serial = 0;
    RECT rect{};
    // Its rectangle is known from the last message: it was visible on that message's window.
    bool placed = false;
};

QList<Watched> s_watched;
std::uint64_t s_nextSerial = 1;

std::optional<PointerMessage> kindOf(const MSG &m)
{
    switch (m.message) {
    case WM_MOUSEMOVE: return PointerMessage::Move;
    case WM_LBUTTONDOWN:
    case WM_LBUTTONDBLCLK: return PointerMessage::Press;
    case WM_LBUTTONUP: return PointerMessage::Release;
    case WM_MOUSELEAVE: return PointerMessage::Leave;
    case WM_CAPTURECHANGED: return PointerMessage::CaptureChanged;
    case WM_CANCELMODE: return PointerMessage::CancelMode;
    case WM_MOUSEACTIVATE:
        if (HIWORD(m.lParam) == WM_LBUTTONDOWN)
            return PointerMessage::MouseActivate;
        return std::nullopt;
    default: return std::nullopt;
    }
}

// The item's rectangle on the screen, in physical pixels, when it is visible on `hwnd`.
std::optional<RECT> placeOn(HWND hwnd, const POINT &origin, const QQuickItem &item)
{
    QQuickWindow *window = item.window();
    if (!window || !window->handle() || reinterpret_cast<HWND>(window->winId()) != hwnd
        || !item.isVisible() || !item.isEnabled())
        return std::nullopt;
    const qreal scale = window->effectiveDevicePixelRatio();
    const QPointF topLeft = item.mapToScene(QPointF(0, 0)) * scale;
    RECT rect;
    rect.left = origin.x + LONG(std::lround(topLeft.x()));
    rect.top = origin.y + LONG(std::lround(topLeft.y()));
    rect.right = rect.left + LONG(std::lround(item.width() * scale));
    rect.bottom = rect.top + LONG(std::lround(item.height() * scale));
    return rect;
}

class PointerFilter : public QAbstractNativeEventFilter
{
public:
    bool nativeEventFilter(const QByteArray &, void *message, qintptr *) override
    {
        const MSG &m = *static_cast<const MSG *>(message);
        const std::optional<PointerMessage> kind = kindOf(m);
        if (!kind || s_watched.isEmpty())
            return false;
        POINT cursor{};
        const bool haveCursor = GetCursorPos(&cursor) != FALSE;
        POINT origin{0, 0};
        ClientToScreen(m.hwnd, &origin);
        std::uint64_t serial = 0;
        RECT target{};
        bool changed = false;
        for (Watched &watched : s_watched) {
            const std::optional<RECT> place = watched.item ? placeOn(m.hwnd, origin, *watched.item)
                                                           : std::nullopt;
            if (!place) {
                watched.placed = false;
                continue;
            }
            const bool moved = !watched.placed || !EqualRect(&*place, &watched.rect);
            watched.rect = *place;
            watched.placed = true;
            if (serial == 0 && haveCursor && PtInRect(&*place, cursor)) {
                serial = watched.serial;
                target = *place;
                changed = moved;
            }
        }
        pointer_message(*kind, reinterpret_cast<std::intptr_t>(m.hwnd), std::intptr_t(m.lParam), serial,
                        target.left, target.top, target.right, target.bottom, changed);
        return false;
    }
};

QQuickItem *itemOf(const QVariant &item)
{
    return qobject_cast<QQuickItem *>(item.value<QObject *>());
}

} // namespace

void pointerWatch(const QVariant &item)
{
    QQuickItem *quick = itemOf(item);
    if (!quick)
        return;
    static PointerFilter *filter = nullptr;
    if (!filter && QCoreApplication::instance()) {
        filter = new PointerFilter;
        QCoreApplication::instance()->installNativeEventFilter(filter);
    }
    s_watched.removeIf([](const Watched &w) { return !w.item; });
    pointerUnwatch(item);
    if (s_watched.isEmpty())
        pointer_gate_reset();
    Watched watched;
    watched.item = quick;
    watched.serial = s_nextSerial++;
    s_watched.append(watched);
}

void pointerUnwatch(const QVariant &item)
{
    QQuickItem *quick = itemOf(item);
    s_watched.removeIf([quick](const Watched &w) { return !w.item || w.item == quick; });
}

std::uint64_t pointerSerial(const QVariant &item)
{
    QQuickItem *quick = itemOf(item);
    for (const Watched &watched : s_watched) {
        if (quick && watched.item == quick)
            return watched.serial;
    }
    return 0;
}

} // namespace lil_qt
