// WebEngine <-> ComfyUI frontend scripting spike (IMG-159, IMG-127, IMG-129, ARCH-070).
// The Task 2 app (spikes/qt-stack) with only the Web dock, plus PageDriver (shim) exposed to QML.
//
// Switches:
//   --log <file>           JSON-lines event log
//   --phase <name>         run a check sequence and quit: visible | hidden | down | firstboot (default: none,
//                          the app just shows ComfyUI, e.g. for Task 8)
//   --fixtures <dir>       UI-format fixtures (<name>.ui.json)
//   --out <dir>            where the check sequence writes graphs and API JSON
//   --profile-dir <dir>    root of the persistent view profile (default: <exe dir>/../../profile)
//   --origin <url>         ComfyUI origin (default http://127.0.0.1:8188)
//   --conversions <n>      hidden conversions in the hidden phase (default 10)
//   --quit-after <ms>      watchdog quit
//   --screenshot <png>     grab the main window (QQuickWindow::grabWindow) just before quitting
//   --heartbeat <ms>       log a GUI-thread "heartbeat" every <ms>

#include "PageDriver.h"
#include "SpikeLog.h"

#include <kddockwidgets/Config.h>
#include <kddockwidgets/qtquick/Platform.h>

#include <QCommandLineParser>
#include <QDir>
#include <QGuiApplication>
#include <QJsonArray>
#include <QQmlApplicationEngine>
#include <QTimer>

int main(int argc, char *argv[])
{
    // As in Task 2 (APP-137): lets QtWebEngine start without QtWebEngineQuick::initialize().
    QCoreApplication::setAttribute(Qt::AA_ShareOpenGLContexts);
    QGuiApplication app(argc, argv);
    QCoreApplication::setApplicationName(QStringLiteral("webengine_spike"));

    QCommandLineParser parser;
    parser.addHelpOption();
    const QCommandLineOption logOpt(QStringLiteral("log"), QStringLiteral("JSON-lines log file."), QStringLiteral("file"));
    const QCommandLineOption phaseOpt(QStringLiteral("phase"), QStringLiteral("visible | hidden | down"), QStringLiteral("name"));
    const QCommandLineOption fixturesOpt(QStringLiteral("fixtures"), QStringLiteral("Fixture folder."), QStringLiteral("dir"));
    const QCommandLineOption outOpt(QStringLiteral("out"), QStringLiteral("Output folder."), QStringLiteral("dir"));
    const QCommandLineOption profileOpt(QStringLiteral("profile-dir"), QStringLiteral("Persistent view profile root."), QStringLiteral("dir"));
    const QCommandLineOption originOpt(QStringLiteral("origin"), QStringLiteral("ComfyUI origin."), QStringLiteral("url"));
    const QCommandLineOption convOpt(QStringLiteral("conversions"), QStringLiteral("Hidden conversions."), QStringLiteral("n"));
    const QCommandLineOption quitOpt(QStringLiteral("quit-after"), QStringLiteral("Quit after <ms>."), QStringLiteral("ms"));
    const QCommandLineOption shotOpt(QStringLiteral("screenshot"), QStringLiteral("Window PNG before quitting."), QStringLiteral("png"));
    const QCommandLineOption beatOpt(QStringLiteral("heartbeat"), QStringLiteral("Heartbeat every <ms>."), QStringLiteral("ms"));
    parser.addOptions({logOpt, phaseOpt, fixturesOpt, outOpt, profileOpt, originOpt, convOpt, quitOpt, shotOpt, beatOpt});
    parser.process(app);

    if (parser.isSet(logOpt))
        SpikeLog::open(parser.value(logOpt));
    SpikeLog::installMessageHandler();
    SpikeLog::write(QStringLiteral("start"),
                    {{"qt_version", QString::fromLatin1(qVersion())},
                     {"pid", qint64(QCoreApplication::applicationPid())},
                     {"args", QJsonArray::fromStringList(QCoreApplication::arguments())}});

    const QString phase = parser.value(phaseOpt);
    const QUrl origin(parser.isSet(originOpt) ? parser.value(originOpt) : QStringLiteral("http://127.0.0.1:8188"));
    const QString profileDir = parser.isSet(profileOpt)
        ? parser.value(profileOpt)
        : QDir(QCoreApplication::applicationDirPath()).absoluteFilePath(QStringLiteral("../../profile"));

    KDDockWidgets::initFrontend(KDDockWidgets::FrontendType::QtQuick);

    // Declared before the engine, so it outlives the QML view that uses its profile.
    PageDriver driver(QDir::cleanPath(profileDir), origin);
    qmlRegisterSingletonInstance("SpikeShim", 1, 0, "SpikeLog", SpikeLog::instance());
    qmlRegisterSingletonInstance("SpikeShim", 1, 0, "PageDriver", &driver);

    QQmlApplicationEngine engine;
    KDDockWidgets::QtQuick::Platform::instance()->setQmlEngine(&engine);
    engine.setInitialProperties({
        {QStringLiteral("phase"), phase},
        // The hidden and firstboot phases keep the visible view off ComfyUI, so only the hidden page
    // touches it.
        {QStringLiteral("viewUrl"), (phase == QLatin1String("hidden") || phase == QLatin1String("firstboot"))
                                        ? QStringLiteral("about:blank") : origin.toString()},
        {QStringLiteral("fixturesDir"), parser.value(fixturesOpt)},
        {QStringLiteral("outDir"), parser.value(outOpt)},
        {QStringLiteral("conversions"), parser.isSet(convOpt) ? parser.value(convOpt).toInt() : 10},
        {QStringLiteral("quitAfterMs"), parser.isSet(quitOpt) ? parser.value(quitOpt).toInt() : (phase.isEmpty() ? -1 : 200000)},
        {QStringLiteral("screenshotPath"), parser.value(shotOpt)},
    });
    engine.load(QUrl(QStringLiteral("qrc:/qml/Main.qml")));
    if (engine.rootObjects().isEmpty()) {
        SpikeLog::write(QStringLiteral("error"), {{"what", "Main.qml failed to load"}});
        return 1;
    }
    SpikeLog::write(QStringLiteral("qml_loaded"));

    QTimer heartbeat;
    QObject::connect(&heartbeat, &QTimer::timeout, [] { SpikeLog::write(QStringLiteral("heartbeat")); });
    if (parser.isSet(beatOpt) && parser.value(beatOpt).toInt() > 0)
        heartbeat.start(parser.value(beatOpt).toInt());

    QObject::connect(&app, &QCoreApplication::aboutToQuit, [] { SpikeLog::write(QStringLiteral("quit")); });
    const int rc = app.exec();
    SpikeLog::write(QStringLiteral("exit"), {{"code", rc}, {"download_attempts", driver.downloadAttempts()}});
    return rc;
}
