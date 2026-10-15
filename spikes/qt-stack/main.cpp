// Qt stack spike (ARCH-115, ARCH-116, APP-137): cxx-qt + KDDockWidgets (Qt Quick) +
// QQuickRhiItem + a lazily loaded QtWebEngine view.
//
// Switches:
//   --log <file>            JSON-lines event log (initialize, backend, webengine_loaded, quit, ...)
//   --auto-float-canvas     float the Canvas dock at 2 s, re-dock at 4 s, quit at 6 s
//   --show-web <ms>         make the Web dock the current tab after <ms>
//   --auto-counter <ms>     click the Counter button three times, starting after <ms>
//   --quit-after <ms>       quit after <ms>
//   --screenshot <png>      grab the main window just before an automatic quit
//   --heartbeat <ms>        log a GUI-thread "heartbeat" every <ms> (responsiveness evidence)

#include "SpikeCanvas.h"
#include "SpikeLog.h"

#include <kddockwidgets/Config.h>
#include <kddockwidgets/qtquick/Platform.h>

#include <QCommandLineParser>
#include <QGuiApplication>
#include <QJsonArray>
#include <QQmlApplicationEngine>
#include <QQmlContext>
#include <QTimer>

int main(int argc, char *argv[])
{
    // APP-137: must be set before the application object exists. This is what lets QtWebEngine
    // be loaded later (by the Web dock's Loader) without QtWebEngineQuick::initialize().
    QCoreApplication::setAttribute(Qt::AA_ShareOpenGLContexts);
    QGuiApplication app(argc, argv);
    QCoreApplication::setApplicationName(QStringLiteral("qt_stack_spike"));

    QCommandLineParser parser;
    parser.addHelpOption();
    const QCommandLineOption logOpt(QStringLiteral("log"), QStringLiteral("JSON-lines log file."), QStringLiteral("file"));
    const QCommandLineOption floatOpt(QStringLiteral("auto-float-canvas"),
                                      QStringLiteral("Float the canvas dock at 2 s, re-dock at 4 s, quit at 6 s."));
    const QCommandLineOption webOpt(QStringLiteral("show-web"), QStringLiteral("Show the Web dock after <ms>."), QStringLiteral("ms"));
    const QCommandLineOption counterOpt(QStringLiteral("auto-counter"),
                                        QStringLiteral("Click the counter button 3 times after <ms>."), QStringLiteral("ms"));
    const QCommandLineOption quitOpt(QStringLiteral("quit-after"), QStringLiteral("Quit after <ms>."), QStringLiteral("ms"));
    const QCommandLineOption shotOpt(QStringLiteral("screenshot"),
                                     QStringLiteral("Save the main window as PNG before an automatic quit."), QStringLiteral("png"));
    const QCommandLineOption beatOpt(QStringLiteral("heartbeat"), QStringLiteral("Log a GUI-thread heartbeat every <ms>."), QStringLiteral("ms"));
    parser.addOptions({logOpt, floatOpt, webOpt, counterOpt, quitOpt, shotOpt, beatOpt});
    parser.process(app);

    if (parser.isSet(logOpt))
        SpikeLog::open(parser.value(logOpt));
    SpikeLog::installMessageHandler();
    SpikeLog::write(QStringLiteral("start"),
                    {{"qt_version", QString::fromLatin1(qVersion())},
                     {"pid", qint64(QCoreApplication::applicationPid())},
                     {"args", QJsonArray::fromStringList(QCoreApplication::arguments())}});

    KDDockWidgets::initFrontend(KDDockWidgets::FrontendType::QtQuick);

    qmlRegisterType<SpikeCanvas>("SpikeShim", 1, 0, "SpikeCanvas");
    qmlRegisterSingletonInstance("SpikeShim", 1, 0, "SpikeLog", SpikeLog::instance());

    auto msValue = [&](const QCommandLineOption &o) { return parser.isSet(o) ? parser.value(o).toInt() : -1; };

    QQmlApplicationEngine engine;
    KDDockWidgets::QtQuick::Platform::instance()->setQmlEngine(&engine);
    engine.setInitialProperties({
        {QStringLiteral("autoFloatCanvas"), parser.isSet(floatOpt)},
        {QStringLiteral("showWebMs"), msValue(webOpt)},
        {QStringLiteral("autoCounterMs"), msValue(counterOpt)},
        {QStringLiteral("quitAfterMs"), msValue(quitOpt)},
        {QStringLiteral("screenshotPath"), parser.value(shotOpt)},
    });
    engine.load(QUrl(QStringLiteral("qrc:/qml/Main.qml")));
    if (engine.rootObjects().isEmpty()) {
        SpikeLog::write(QStringLiteral("error"), {{"what", "Main.qml failed to load"}});
        return 1;
    }
    SpikeLog::write(QStringLiteral("qml_loaded"));
    // --heartbeat <ms>: a GUI-thread timer; gaps between heartbeats show event-loop stalls.
    QTimer heartbeat;
    QObject::connect(&heartbeat, &QTimer::timeout, [] { SpikeLog::write(QStringLiteral("heartbeat")); });
    if (msValue(beatOpt) > 0)
        heartbeat.start(msValue(beatOpt));

    QObject::connect(&app, &QCoreApplication::aboutToQuit, [] { SpikeLog::write(QStringLiteral("quit")); });
    const int rc = app.exec();
    SpikeLog::write(QStringLiteral("exit"), {{"code", rc}});
    return rc;
}
