#include "lilshim/Activation.h"

#include <QLoggingCategory>
#include <QVariant>
#include <QWindow>

#ifndef NOMINMAX
#  define NOMINMAX
#endif
#ifndef WIN32_LEAN_AND_MEAN
#  define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

Q_LOGGING_CATEGORY(lcActivation, "lil.shim.activation")

namespace lilshim {

void activate(QWindow *w, bool takeFocus)
{
    if (!w)
        return;
    // Qt's Windows platform plugin shows a window with SW_SHOWNOACTIVATE while this property is
    // set; it is read when the window is shown, so it is set first.
    w->setProperty("_q_showWithoutActivating", !takeFocus);
    const HWND hwnd = reinterpret_cast<HWND>(w->winId());
    if (takeFocus) {
        // A minimized window is restored, a hidden one shown, then brought to the front. A
        // forwarded launch called AllowSetForegroundWindow for this process (APP-014), which is
        // what lets SetForegroundWindow take the front from the window that has it; Qt's own
        // requestActivate only flashes the taskbar button while another app is in front.
        if (w->windowStates() & Qt::WindowMinimized)
            w->setWindowStates(w->windowStates() & ~Qt::WindowMinimized);
        w->show();
        w->raise();
        if (IsIconic(hwnd))
            ShowWindow(hwnd, SW_RESTORE);
        // One line either way, so a launch that didn't come forward says why in the app's log:
        // refused (the launch that asked had no foreground right to give), or taken and then
        // lost to another process.
        if (!SetForegroundWindow(hwnd)) {
            DWORD frontPid = 0;
            GetWindowThreadProcessId(GetForegroundWindow(), &frontPid);
            qCWarning(lcActivation) << "SetForegroundWindow was refused: the taskbar button flashes;"
                                    << "the front is held by process" << frontPid;
        } else {
            qCInfo(lcActivation) << "the window took the front";
        }
        w->requestActivate();
        return;
    }
    w->show();
    FLASHWINFO flash{};
    flash.cbSize = sizeof(flash);
    flash.hwnd = hwnd;
    flash.dwFlags = FLASHW_TRAY | FLASHW_TIMERNOFG;
    FlashWindowEx(&flash);
}

bool isModuleLoaded(const QString &dll)
{
    return GetModuleHandleW(reinterpret_cast<LPCWSTR>(dll.utf16())) != nullptr;
}

void showInExplorer(const QString &path)
{
    Q_UNUSED(path)
    // The path isn't logged: it may name the owner's folders.
    qCWarning(lcActivation) << "showInExplorer isn't available yet";
}

} // namespace lilshim
