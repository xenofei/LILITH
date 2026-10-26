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
    w->show();
    if (takeFocus) {
        w->raise();
        w->requestActivate();
        return;
    }
    FLASHWINFO flash{};
    flash.cbSize = sizeof(flash);
    flash.hwnd = reinterpret_cast<HWND>(w->winId());
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
