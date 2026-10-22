// App-shell spike (plan 4a, Task 0). See docs/spikes/10-app-shell.md §3.
//
// Switches (all optional):
//   --log <file>                JSON-lines event log (t_ms = ms since process creation)
//   --no-activate               show every window without activating it (no focus taken)
//   --on-top                    keep main-a on top (S0.6: an occluded window's timers are throttled)
//   --quit-after <ms>           quit after <ms>
//   --show-web <ms>             make the Web tab current after <ms> (its Loader then loads WebPanel.qml)
//   --web-url <url>             what the web view loads (default about:blank)
//   --layout-save <file>        S0.1: move the 3 windows to the test geometry, save everything, quit
//   --layout-restore <file>     S0.1: restore the file over the default layout, snapshot, quit
//   --window-roundtrip <file>   S0.1: save main-b alone, disturb, restore, snapshot, quit
//   --load                      S0.6: feed (8 jobs), list scrolling, charts
//   --load-rate <hz>            preview rate per job (default 10; S0.6's fallback is 5)
//   --charts-batched            S0.6's fallback: all charts redraw together once a second
//   --measure <s>               S0.6: 5 s warm-up, then a <s> measured window, then quit
//   --auto-counter <ms>         click the counter 3 times, 500 ms apart, after <ms>
//   --screenshot <png>          grab main-a just before an automatic quit

#include "ExitWatchdog.h"
#include "Heartbeat.h"
#include "NoActivate.h"
#include "PreviewProvider.h"
#include "SpikeDockFactory.h"
#include "SpikeLayout.h"
#include "SpikeLog.h"
#include "StartupClock.h"

#include "spike_shell_bridge/src/feed.cxxqt.h"

#include <kddockwidgets/Config.h>
#include <kddockwidgets/qtquick/Platform.h>

#include <QAbstractEventDispatcher>
#include <QCommandLineParser>
#include <QGuiApplication>
#include <QJsonArray>
#include <QQmlApplicationEngine>
#include <QQuickWindow>
#include <QStandardPaths>

int main(int argc, char *argv[])
{
    // S0.8: the caches (QML, pipeline, WebEngine) follow these names, so they are set before
    // anything can ask QStandardPaths.
    QCoreApplication::setOrganizationName(QStringLiteral("LILITH-spike"));
    QCoreApplication::setApplicationName(QStringLiteral("image"));
    // APP-137: set before the application object exists, so QtWebEngine can be loaded later by
    // the Web dock's Loader without QtWebEngineQuick::initialize().
    QCoreApplication::setAttribute(Qt::AA_ShareOpenGLContexts);
    QGuiApplication app(argc, argv);

    QCommandLineParser parser;
    parser.addHelpOption();
    const QCommandLineOption logOpt(QStringLiteral("log"), QStringLiteral("JSON-lines log."), QStringLiteral("file"));
    const QCommandLineOption noActOpt(QStringLiteral("no-activate"), QStringLiteral("Never activate a window."));
    const QCommandLineOption onTopOpt(QStringLiteral("on-top"), QStringLiteral("Keep main-a on top."));
    const QCommandLineOption quitOpt(QStringLiteral("quit-after"), QStringLiteral("Quit after <ms>."), QStringLiteral("ms"));
    const QCommandLineOption webOpt(QStringLiteral("show-web"), QStringLiteral("Show the Web tab after <ms>."), QStringLiteral("ms"));
    const QCommandLineOption urlOpt(QStringLiteral("web-url"), QStringLiteral("Web view URL."), QStringLiteral("url"));
    const QCommandLineOption saveOpt(QStringLiteral("layout-save"), QStringLiteral("S0.1 save run."), QStringLiteral("file"));
    const QCommandLineOption restoreOpt(QStringLiteral("layout-restore"), QStringLiteral("S0.1 restore run."), QStringLiteral("file"));
    const QCommandLineOption windowOpt(QStringLiteral("window-roundtrip"), QStringLiteral("S0.1 per-window run."), QStringLiteral("file"));
    const QCommandLineOption loadOpt(QStringLiteral("load"), QStringLiteral("S0.6 workload."));
    const QCommandLineOption rateOpt(QStringLiteral("load-rate"), QStringLiteral("Preview Hz per job."), QStringLiteral("hz"));
    const QCommandLineOption batchOpt(QStringLiteral("charts-batched"), QStringLiteral("Charts redraw once a second."));
    const QCommandLineOption measureOpt(QStringLiteral("measure"), QStringLiteral("S0.6 measured seconds."), QStringLiteral("s"));
    const QCommandLineOption counterOpt(QStringLiteral("auto-counter"), QStringLiteral("Click the counter 3 times after <ms>."), QStringLiteral("ms"));
    const QCommandLineOption shotOpt(QStringLiteral("screenshot"), QStringLiteral("PNG of main-a before an automatic quit."), QStringLiteral("png"));
    parser.addOptions({logOpt, noActOpt, onTopOpt, quitOpt, webOpt, urlOpt, saveOpt, restoreOpt, windowOpt,
                       loadOpt, rateOpt, batchOpt, measureOpt, counterOpt, shotOpt});
    parser.process(app);

    if (parser.isSet(logOpt))
        SpikeLog::open(parser.value(logOpt));
    SpikeLog::installMessageHandler();
    // Diagnostics: a GUI thread that stops running its event loop for 5 s gets its stacks written.
    if (parser.isSet(logOpt))
        ExitWatchdog::watchGuiThread(parser.value(logOpt) + QStringLiteral(".stall-stacks.txt"), 5000);
    SpikeLog::write(QStringLiteral("start"),
                    {{"qt_version", QString::fromLatin1(qVersion())},
                     {"pid", qint64(QCoreApplication::applicationPid())},
                     {"args", QJsonArray::fromStringList(QCoreApplication::arguments())},
                     {"cache_dir", QStandardPaths::writableLocation(QStandardPaths::CacheLocation)},
                     {"local_data_dir", QStandardPaths::writableLocation(QStandardPaths::AppLocalDataLocation)},
                     {"webengine_dll_in_process", SpikeLog::instance()->moduleLoaded(QStringLiteral("Qt6WebEngineCore.dll"))}});
    // S0.3: probe for the WebEngine DLL from here until the Web dock is shown.
    SpikeLog::instance()->startModuleProbe(QStringLiteral("Qt6WebEngineCore.dll"), 100);

    if (parser.isSet(noActOpt))
        NoActivate::install();

    KDDockWidgets::initFrontend(KDDockWidgets::FrontendType::QtQuick);
    SpikeDockFactory::install();

    auto *hub = new PreviewHub(8, &app);
    auto *feed = new JobsFeed(&app);
    QObject::connect(feed, &JobsFeed::frame, hub, &PreviewHub::push);

    qmlRegisterSingletonInstance("SpikeShim", 1, 0, "SpikeLog", SpikeLog::instance());
    qmlRegisterSingletonInstance("SpikeShim", 1, 0, "Heartbeat", Heartbeat::instance());
    qmlRegisterSingletonInstance("SpikeShim", 1, 0, "SpikeLayout", SpikeLayout::instance());
    qmlRegisterSingletonInstance("SpikeShim", 1, 0, "Previews", hub);
    qmlRegisterSingletonInstance("SpikeShim", 1, 0, "Feed", static_cast<QObject *>(feed));

    auto msValue = [&](const QCommandLineOption &o) { return parser.isSet(o) ? parser.value(o).toInt() : -1; };

    QQmlApplicationEngine engine;
    engine.addImageProvider(QStringLiteral("preview"), new PreviewProvider(hub));
    KDDockWidgets::QtQuick::Platform::instance()->setQmlEngine(&engine);

    // S0.5: first_frame is main-a's first frameSwapped (render thread).
    QObject::connect(&engine, &QQmlApplicationEngine::objectCreated, &app, [](QObject *obj, const QUrl &) {
        auto *w = qobject_cast<QQuickWindow *>(obj);
        if (!w)
            return;
        auto *once = new QMetaObject::Connection;
        *once = QObject::connect(w, &QQuickWindow::frameSwapped, w, [once] {
            SpikeLog::write(QStringLiteral("first_frame"), {{"ms", StartupClock::msSinceProcessStart()}});
            QObject::disconnect(*once);
            delete once;
        }, Qt::DirectConnection);
    });

    engine.setInitialProperties({
        {QStringLiteral("onTop"), parser.isSet(onTopOpt)},
        {QStringLiteral("quitAfterMs"), msValue(quitOpt)},
        {QStringLiteral("showWebMs"), msValue(webOpt)},
        {QStringLiteral("webUrl"), parser.isSet(urlOpt) ? parser.value(urlOpt) : QStringLiteral("about:blank")},
        {QStringLiteral("layoutSavePath"), parser.value(saveOpt)},
        {QStringLiteral("layoutRestorePath"), parser.value(restoreOpt)},
        {QStringLiteral("windowRoundtripPath"), parser.value(windowOpt)},
        {QStringLiteral("loadOn"), parser.isSet(loadOpt)},
        {QStringLiteral("loadRateHz"), parser.isSet(rateOpt) ? parser.value(rateOpt).toInt() : 10},
        {QStringLiteral("chartsBatched"), parser.isSet(batchOpt)},
        {QStringLiteral("measureSeconds"), msValue(measureOpt)},
        {QStringLiteral("autoCounterMs"), msValue(counterOpt)},
        {QStringLiteral("screenshotPath"), parser.value(shotOpt)},
    });
    engine.load(QUrl(QStringLiteral("qrc:/qml/Main.qml")));
    if (engine.rootObjects().isEmpty()) {
        SpikeLog::write(QStringLiteral("error"), {{"what", "Main.qml failed to load"}});
        return 1;
    }
    SpikeLog::write(QStringLiteral("qml_loaded"), {{"ms", StartupClock::msSinceProcessStart()}});

    // S0.5: interactive is the first time the event loop goes idle after the QML root completed.
    {
        auto *once = new QMetaObject::Connection;
        *once = QObject::connect(QAbstractEventDispatcher::instance(), &QAbstractEventDispatcher::aboutToBlock, &app, [once] {
            SpikeLog::write(QStringLiteral("interactive"), {{"ms", StartupClock::msSinceProcessStart()}});
            QObject::disconnect(*once);
            delete once;
        });
    }

    const QString stacksPath = parser.isSet(logOpt) ? parser.value(logOpt) + QStringLiteral(".stacks.txt") : QString();
    QObject::connect(&app, &QCoreApplication::aboutToQuit, [stacksPath] {
        SpikeLog::write(QStringLiteral("quit"));
        // Diagnostics: if the process is still alive 8 s from now, write every thread's stack.
        if (!stacksPath.isEmpty())
            ExitWatchdog::arm(stacksPath, 8);
    });
    const int rc = app.exec();
    feed->stop();
    SpikeLog::write(QStringLiteral("exit"), {{"code", rc}});
    return rc;
}
