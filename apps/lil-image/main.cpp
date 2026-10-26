// lil-image.exe, the image app (APP-001). Rust settles the start before Qt is initialised
// (lil_qt::boot), and Qt sees only what the plan hands it (Checkpoint A I-3, as ruled at review).
// Every failure below is a refusal: exit 2, before the application object exists.
//   0. DLL searches are locked to this exe's folder and System32 (SetDefaultDllDirectories, and
//      SetDllDirectoryW(L"") takes the current folder out), so a DLL planted on PATH or in the
//      current folder never loads, whatever name Qt or a driver asks for (opengl32sw.dll, say).
//   1. lil_qt::boot() reads the raw command line (P4a-R3) and refuses anything but the switches
//      this build takes. Task 19 adds the forwarded launch, which exits 0 here, and the one
//      positional argument. It also refuses a start without an absolute exe folder.
//   2. The variables that choose where Qt, QML, the scene graph, WebEngine or the Vulkan loader
//      load code, QML or data from, which exe WebEngine starts, or that open debugging, all names
//      under their prefixes (lil_qt::qt_env_to_clear), are removed from the process environment,
//      and checked gone.
//   3. The names (the caches and the WebEngine profile follow them), AA_ShareOpenGLContexts
//      (APP-137) and the library path (this exe's folder only) are set; qt.conf beside the exe
//      pins Qt's other folders to it.
//   4. QGuiApplication is built from the program name alone: no Qt option reaches it.
// "Before Qt is initialised", not "before any Qt code": Qt's DLLs run their own static
// initialisers when the process loads them, before main, and this file's Q_IMPORT_QML_PLUGIN
// lines and cxx-qt's generated initialisers run before main too. They only register plugin and
// type functions and read no environment variable; a static initialiser that reads one must not
// be added.
// So it runs from a deploy folder (build/<preset>/deploy), never from bin/: its own folder is
// where Qt's platform plugin must be.
//
// Switches: --no-activate shows the window without taking the focus (its taskbar button
// flashes); --smoke-exit quits with 0 once the first frame is shown and the event loop has gone
// idle (tools/deploy/check-deploy.ps1). The log bridge writes `first_frame` and `interactive` as
// JSON lines on stderr (APP-136). Under --smoke-exit the `start` line also says whether the DLL
// search can still find `lil-search-probe.dll` (searched by bare name and mapped as data, never
// run), which the deploy check plants on PATH and in the current folder.
#include "lilshim/Activation.h"
#include "lilshim/DockFactory.h"
#include "lilshim/LogBridge.h"

#include "lil_qt/src/qt/mod.cxxqt.h"

#include <kddockwidgets/Config.h>
#include <kddockwidgets/qtquick/Platform.h>

#include <QAbstractEventDispatcher>
#include <QGuiApplication>
#include <QJsonArray>
#include <QJsonObject>
#include <QQmlApplicationEngine>
#include <QQuickWindow>
#include <QSGRendererInterface>
#include <QTimer>
#include <QtQml/qqmlextensionplugin.h>

#include <atomic>
#include <cstdio>
#include <string>
#include <vector>

#ifndef NOMINMAX
#  define NOMINMAX
#endif
#ifndef WIN32_LEAN_AND_MEAN
#  define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

#ifdef QT_QML_DEBUG
#  error "QML debugging stays off in lil-image (Checkpoint A I-3)"
#endif

Q_IMPORT_QML_PLUGIN(Lilith_UiKitPlugin)
Q_IMPORT_QML_PLUGIN(Lilith_ShimPlugin)

namespace {

int refuse(const char *why)
{
    std::fprintf(stderr, "%s\n", why);
    return 2;
}

// Removes `name` from the CRT's tables (which Qt's qgetenv reads) and from the process
// environment (which child processes such as QtWebEngineProcess inherit). True only when both
// no longer have it.
bool clearVariable(const std::string &name)
{
    const int len = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, name.c_str(), -1, nullptr, 0);
    if (len <= 1)
        return false;
    std::wstring wide(size_t(len), L'\0');
    if (MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, name.c_str(), -1, wide.data(), len) != len)
        return false;
    if (_wputenv_s(wide.c_str(), L"") != 0)
        return false;
    size_t crtSize = 0;
    _wgetenv_s(&crtSize, nullptr, 0, wide.c_str());
    SetLastError(ERROR_SUCCESS);
    const DWORD processSize = GetEnvironmentVariableW(wide.c_str(), nullptr, 0);
    return crtSize == 0 && processSize == 0 && GetLastError() == ERROR_ENVVAR_NOT_FOUND;
}

QString fromRust(const rust::String &s)
{
    return QString::fromUtf8(s.data(), qsizetype(s.size()));
}

QString graphicsApiName(QQuickWindow *window)
{
    const QSGRendererInterface *ri = window->rendererInterface();
    switch (ri ? ri->graphicsApi() : QSGRendererInterface::Unknown) {
    case QSGRendererInterface::Direct3D11: return QStringLiteral("d3d11");
    case QSGRendererInterface::Direct3D12: return QStringLiteral("d3d12");
    case QSGRendererInterface::OpenGL: return QStringLiteral("opengl");
    case QSGRendererInterface::Vulkan: return QStringLiteral("vulkan");
    case QSGRendererInterface::Software: return QStringLiteral("software");
    default: return QStringLiteral("other");
    }
}

} // namespace

int main(int, char **)
{
    // 0. DLL searches: this exe's folder and System32 only.
    if (!SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_APPLICATION_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32)
        || !SetDllDirectoryW(L""))
        return refuse("LILITH didn't start: it couldn't restrict where DLLs load from.");

    // 1. The plan, before Qt is initialised.
    const lil_qt::QtBoot plan = lil_qt::boot();
    if (!plan.refused.empty())
        return refuse(std::string(plan.refused).c_str());

    // 2. The environment Qt, WebEngine and the Vulkan loader would read.
    for (const rust::String &name : plan.clear_env) {
        if (!clearVariable(std::string(name)))
            return refuse("LILITH didn't start: an environment variable Qt reads couldn't be cleared.");
    }

    // 3. Names, attributes and the library path, before the application object exists.
    QCoreApplication::setOrganizationName(fromRust(plan.org));
    QCoreApplication::setApplicationName(fromRust(plan.app));
    QCoreApplication::setAttribute(Qt::AA_ShareOpenGLContexts);
    QCoreApplication::setLibraryPaths({fromRust(plan.app_dir)});

    // 4. Qt's argv: the program name alone. It outlives the application object.
    std::vector<std::string> qtArgs;
    for (const rust::String &arg : plan.qt_args)
        qtArgs.emplace_back(arg);
    std::vector<char *> qtArgv;
    for (std::string &arg : qtArgs)
        qtArgv.push_back(arg.data());
    qtArgv.push_back(nullptr);
    int qtArgc = int(qtArgs.size());
    QGuiApplication app(qtArgc, qtArgv.data());

    lilshim::installLogBridge();
    // env_left: what step 2 left of the cleared variables (none); the deploy check reads it,
    // with the platform, the import paths and the first frame's graphics API.
    QJsonArray envLeft;
    for (const rust::String &name : lil_qt::env_left())
        envLeft.append(fromRust(name));
    QJsonObject startFields{{"qt_version", QString::fromLatin1(qVersion())},
                            {"platform", QGuiApplication::platformName()},
                            {"env_left", envLeft},
                            {"no_activate", plan.no_activate},
                            {"smoke_exit", plan.smoke_exit}};
    if (plan.smoke_exit) {
        // Step 0's check: a bare-name search for the deploy check's probe, mapped as data only.
        HMODULE probe = LoadLibraryExW(L"lil-search-probe.dll", nullptr, LOAD_LIBRARY_AS_DATAFILE);
        startFields.insert(QStringLiteral("search_probe_found"), probe != nullptr);
        if (probe)
            FreeLibrary(probe);
    }
    lilshim::logEvent(QStringLiteral("start"), startFields);

    KDDockWidgets::initFrontend(KDDockWidgets::FrontendType::QtQuick);
    lilshim::DockFactory::install();

    QQmlApplicationEngine engine;
    lilshim::logEvent(QStringLiteral("qml_import_paths"),
                      {{"paths", QJsonArray::fromStringList(engine.importPathList())}});
    KDDockWidgets::QtQuick::Platform::instance()->setQmlEngine(&engine);
    engine.loadFromModule("Lilith.UiKit", "ShellWindow");
    auto *window = engine.rootObjects().isEmpty()
                       ? nullptr
                       : qobject_cast<QQuickWindow *>(engine.rootObjects().constFirst());
    if (!window) {
        lilshim::logEvent(QStringLiteral("error"), {{"what", "ShellWindow failed to load"}});
        return 1;
    }

    // --smoke-exit quits once both have been logged.
    static std::atomic<int> seen{0};
    const bool smokeExit = plan.smoke_exit;
    auto reached = [smokeExit, &app] {
        if (++seen == 2 && smokeExit)
            QMetaObject::invokeMethod(&app, [] { QCoreApplication::exit(0); }, Qt::QueuedConnection);
    };

    // first_frame: the window's first frameSwapped (render thread).
    auto *frameOnce = new QMetaObject::Connection;
    *frameOnce = QObject::connect(window, &QQuickWindow::frameSwapped, window, [frameOnce, reached, window] {
        QObject::disconnect(*frameOnce);
        delete frameOnce;
        lilshim::logEvent(QStringLiteral("first_frame"),
                          {{"ms", lilshim::msSinceProcessStart()},
                           {"graphics_api", graphicsApiName(window)}});
        reached();
    }, Qt::DirectConnection);

    lilshim::activate(window, !plan.no_activate);

    // interactive: the first time the event loop goes idle after the window was shown.
    auto *idleOnce = new QMetaObject::Connection;
    *idleOnce = QObject::connect(QAbstractEventDispatcher::instance(), &QAbstractEventDispatcher::aboutToBlock,
                                 &app, [idleOnce, reached] {
        QObject::disconnect(*idleOnce);
        delete idleOnce;
        // APP-137: WebEngine isn't in the process at this point.
        lilshim::logEvent(QStringLiteral("interactive"),
                          {{"ms", lilshim::msSinceProcessStart()},
                           {"webengine_loaded", lilshim::isModuleLoaded(QStringLiteral("Qt6WebEngineCore.dll"))}});
        reached();
    });

    const int rc = app.exec();
    lilshim::logEvent(QStringLiteral("exit"), {{"code", rc}});
    return rc;
}
