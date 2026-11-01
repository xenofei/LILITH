// The native side of ConfirmInput (P4a-R10). Mouse messages reach a native event filter in the event
// dispatcher, after they leave the thread's queue and before they are dispatched; Windows' own sent
// messages (capture, activation) reach it in the window procedure. A mouse message another program
// *sends* reaches neither: Qt keeps input messages out of the window procedure's filter, so a thread
// hook (WH_CALLWNDPROC, which sees sent messages only) tells the gate. Neither changes anything:
// every message goes on to Qt. A button is watched only while it shows (ApproveButton), and with nothing
// watched the filter reads nothing and tracks nothing, so ordinary clicks in the app go past untouched.
#include "lil_qt/src/qt/pointer_filter.h"
#include "lil_qt/src/qt/confirm_input.cxxqt.h"

#include <QtCore/QAbstractNativeEventFilter>
#include <QtCore/QCoreApplication>
#include <QtGui/QGuiApplication>
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
    // Coming to the front and leaving it, from Windows itself, whatever Qt's own focus does next.
    case WM_ACTIVATE:
        return LOWORD(m.wParam) == WA_INACTIVE ? PointerMessage::FrontLost : PointerMessage::FrontGained;
    default: return std::nullopt;
    }
}

// The item's rectangle on the screen, in physical pixels, as it is drawn: without what a scrolling
// ancestor clips. None when it isn't visible on `hwnd`.
std::optional<RECT> placeOn(HWND hwnd, const POINT &origin, const QQuickItem &item)
{
    QQuickWindow *window = item.window();
    if (!window || !window->handle() || reinterpret_cast<HWND>(window->winId()) != hwnd
        || !item.isVisible() || !item.isEnabled())
        return std::nullopt;
    QRectF box = item.mapRectToScene(item.boundingRect());
    for (const QQuickItem *parent = item.parentItem(); parent; parent = parent->parentItem()) {
        if (parent->clip())
            box &= parent->mapRectToScene(parent->boundingRect());
    }
    if (box.isEmpty())
        return std::nullopt;
    const qreal scale = window->effectiveDevicePixelRatio();
    RECT rect;
    rect.left = origin.x + LONG(std::lround(box.left() * scale));
    rect.top = origin.y + LONG(std::lround(box.top() * scale));
    rect.right = origin.x + LONG(std::lround(box.right() * scale));
    rect.bottom = origin.y + LONG(std::lround(box.bottom() * scale));
    return rect;
}

// Gives one message to the gate, with the watched button under the cursor as Qt places it now.
void deliver(PointerMessage kind, const MSG &m, bool viaHook)
{
    {
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
        pointer_message(kind, reinterpret_cast<std::intptr_t>(m.hwnd), std::intptr_t(m.lParam), serial,
                        target.left, target.top, target.right, target.bottom, changed, viaHook);
    }
}

// Sees every message sent to a window of this thread, from any thread, before its window procedure.
// Activation is sent, never posted, so this sees it whatever Qt's own path does with it; the filter
// sees it too, in the window procedure, and the gate takes it twice without harm.
LRESULT CALLBACK sentHook(int code, WPARAM wparam, LPARAM lparam)
{
    if (code == HC_ACTION) {
        const CWPSTRUCT &sent = *reinterpret_cast<const CWPSTRUCT *>(lparam);
        const UINT message = sent.message;
        if (message == WM_LBUTTONDOWN || message == WM_LBUTTONDBLCLK || message == WM_LBUTTONUP) {
            pointer_sent();
        } else if ((message == WM_ACTIVATE || message == WM_MOUSEACTIVATE) && !s_watched.isEmpty()) {
            MSG m{};
            m.hwnd = sent.hwnd;
            m.message = message;
            m.wParam = sent.wParam;
            m.lParam = sent.lParam;
            if (const std::optional<PointerMessage> kind = kindOf(m))
                deliver(*kind, m, true);
        }
    }
    return CallNextHookEx(nullptr, code, wparam, lparam);
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
        deliver(*kind, m, false);
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
        pointer_guard(SetWindowsHookExW(WH_CALLWNDPROC, sentHook, nullptr, GetCurrentThreadId()) != nullptr);
    }
    s_watched.removeIf([](const Watched &w) { return !w.item; });
    // Nothing was watched until now, so what the gate knew is old. Watching a button again isn't that:
    // a press that is down, or a re-entry it is owed, still stands.
    if (s_watched.isEmpty())
        pointer_gate_reset();
    pointerUnwatch(item);
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


bool pointerWindowInFront(const QVariant &item)
{
    // Only Windows has a front to read; elsewhere (the offscreen tests) Qt's own word stands.
    if (QGuiApplication::platformName() != QLatin1String("windows"))
        return true;
    QQuickItem *quick = itemOf(item);
    QQuickWindow *window = quick ? quick->window() : nullptr;
    if (!window || !window->handle())
        return false;
    return GetForegroundWindow() == reinterpret_cast<HWND>(window->winId());
}

} // namespace lil_qt
