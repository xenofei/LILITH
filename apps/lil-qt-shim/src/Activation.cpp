#include "lilshim/Activation.h"

#include <QDir>
#include <QFileInfo>
#include <QLoggingCategory>
#include <QProcess>
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

QString explorerSelectArgument(const QString &path)
{
    const QFileInfo info(path);
    if (!info.isAbsolute() || !info.isFile() || info.suffix().compare(QLatin1String("zip"), Qt::CaseInsensitive) != 0)
        return {};
    return QStringLiteral("/select,\"%1\"").arg(QDir::toNativeSeparators(info.absoluteFilePath()));
}

bool showInExplorer(const QString &path)
{
    // The path is never logged: it may name the owner's folders.
    const QString argument = explorerSelectArgument(path);
    if (argument.isEmpty()) {
        qCWarning(lcActivation) << "showInExplorer was refused: not an existing .zip file";
        return false;
    }
    wchar_t windows[MAX_PATH];
    const UINT length = GetWindowsDirectoryW(windows, MAX_PATH);
    if (length == 0 || length >= MAX_PATH) {
        qCWarning(lcActivation) << "showInExplorer: the Windows folder can't be found";
        return false;
    }
    // By its full path, never through a shell or a search of PATH. Explorer takes the argument
    // only as it is written, so it goes in as the raw command line.
    QProcess explorer;
    explorer.setProgram(QString::fromWCharArray(windows, int(length)) + QStringLiteral("\\explorer.exe"));
    explorer.setNativeArguments(argument);
    if (!explorer.startDetached()) {
        qCWarning(lcActivation) << "showInExplorer: Explorer didn't start";
        return false;
    }
    return true;
}

} // namespace lilshim
