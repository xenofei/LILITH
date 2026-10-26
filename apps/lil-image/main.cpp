// lil-image.exe, the image app (APP-001). Rust settles the start before Qt does (lil_qt::boot),
// and Qt sees only what the plan hands it (Checkpoint A I-3):
//   1. lil_qt::boot() reads the raw command line (P4a-R3) and refuses anything but the switches
//      this build takes, before Qt starts (exit 2). Task 19 adds the forwarded launch, which
//      exits 0 here, and the one positional argument.
//   2. The variables that load Qt or QML code from another folder, or open WebEngine to
//      debugging, are removed from the process environment.
//   3. The names (the caches and the WebEngine profile follow them), AA_ShareOpenGLContexts
//      (APP-137) and the library path (this exe's folder only) are set; qt.conf beside the exe
//      pins Qt's other folders to it.
//   4. QGuiApplication is built from the program name alone: no Qt option reaches it.
// So it runs from a deploy folder (build/<preset>/deploy), never from bin/: its own folder is
// where Qt's platform plugin must be.
//
// Switches: --no-activate shows the window without taking the focus (its taskbar button
// flashes); --smoke-exit quits with 0 once the first frame is shown and the event loop has gone
// idle (tools/deploy/check-deploy.ps1). The log bridge writes `first_frame` and `interactive` as
// JSON lines on stderr (APP-136).
#include "lilshim/Activation.h"
#include "lilshim/DockFactory.h"
#include "lilshim/LogBridge.h"

#include "lil_qt/src/qt/mod.cxxqt.h"

#include <kddockwidgets/Config.h>
#include <kddockwidgets/qtquick/Platform.h>

#include <QAbstractEventDispatcher>
#include <QGuiApplication>
#include <QQmlApplicationEngine>
#include <QQuickWindow>
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

// Removes `name` from the CRT's tables (which Qt's qgetenv reads) and from the process
// environment (which child processes such as QtWebEngineProcess inherit).
void clearVariable(const std::string &name)
{
    const int len = MultiByteToWideChar(CP_UTF8, 0, name.c_str(), -1, nullptr, 0);
    if (len <= 1)
        return;
    std::wstring wide(size_t(len), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, name.c_str(), -1, wide.data(), len);
    _wputenv_s(wide.c_str(), L"");
}

QString fromRust(const rust::String &s)
{
    return QString::fromUtf8(s.data(), qsizetype(s.size()));
}

} // namespace

int main(int, char **)
{
    // 1. The plan, before anything of Qt runs.
    const lil_qt::QtBoot plan = lil_qt::boot();
    if (!plan.refused.empty()) {
        const std::string why(plan.refused);
        std::fprintf(stderr, "%s\n", why.c_str());
        return 2;
    }

    // 2. The environment Qt and WebEngine would read.
    for (const rust::String &name : plan.clear_env)
        clearVariable(std::string(name));

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
    lilshim::logEvent(QStringLiteral("start"),
                      {{"qt_version", QString::fromLatin1(qVersion())},
                       {"platform", QGuiApplication::platformName()},
                       {"no_activate", plan.no_activate},
                       {"smoke_exit", plan.smoke_exit}});

    KDDockWidgets::initFrontend(KDDockWidgets::FrontendType::QtQuick);
    lilshim::DockFactory::install();

    QQmlApplicationEngine engine;
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
    *frameOnce = QObject::connect(window, &QQuickWindow::frameSwapped, window, [frameOnce, reached] {
        QObject::disconnect(*frameOnce);
        delete frameOnce;
        lilshim::logEvent(QStringLiteral("first_frame"), {{"ms", lilshim::msSinceProcessStart()}});
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
