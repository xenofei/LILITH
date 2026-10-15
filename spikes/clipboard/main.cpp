// Clipboard and drag-out formats spike (APP-071, APP-068, APP-069).
//
// Switches:
//   --log <file>        JSON-lines event log
//   --auto-copy         press Copy once the window is up (what run-checks.ps1 uses)
//   --selftest-drag     send synthetic mouse events to our own window (not the desktop) so the
//                       thumbnail's drag code path runs; DoDragDrop is ended by a
//                       helper thread's in-process WM_MOUSEMOVE (no real mouse is involved). Logs drag_start / drag_end.
//   --quit-after <ms>   quit after <ms>
//   --export-dir <dir>  folder for the exported copy (default %TEMP%\ww-spike-export)

#include "ClipboardBridge.h"
#include "SpikeLog.h"

#include <QCommandLineParser>
#include <QDir>
#include <QFile>
#include <QGuiApplication>
#include <QJsonArray>
#include <QMouseEvent>
#include <QQmlApplicationEngine>
#include <QQuickItem>
#include <QQuickWindow>
#include <QTimer>

int main(int argc, char *argv[])
{
    QGuiApplication app(argc, argv);
    QCoreApplication::setApplicationName(QStringLiteral("clipboard_spike"));

    QCommandLineParser parser;
    parser.addHelpOption();
    const QCommandLineOption logOpt(QStringLiteral("log"), QStringLiteral("JSON-lines log file."), QStringLiteral("file"));
    const QCommandLineOption copyOpt(QStringLiteral("auto-copy"), QStringLiteral("Press Copy at start-up."));
    const QCommandLineOption dragOpt(QStringLiteral("selftest-drag"), QStringLiteral("Synthetic drag on our own window."));
    const QCommandLineOption quitOpt(QStringLiteral("quit-after"), QStringLiteral("Quit after <ms>."), QStringLiteral("ms"));
    const QCommandLineOption dirOpt(QStringLiteral("export-dir"), QStringLiteral("Export folder."), QStringLiteral("dir"));
    parser.addOptions({logOpt, copyOpt, dragOpt, quitOpt, dirOpt});
    parser.process(app);

    if (parser.isSet(logOpt))
        SpikeLog::open(parser.value(logOpt));
    SpikeLog::installMessageHandler();
    SpikeLog::write(QStringLiteral("start"),
                    {{"qt_version", QString::fromLatin1(qVersion())},
                     {"pid", qint64(QCoreApplication::applicationPid())}});

    QFile src(QStringLiteral(":/assets/source.png"));
    if (!src.open(QIODevice::ReadOnly)) {
        SpikeLog::write(QStringLiteral("error"), {{"what", "source.png resource missing"}});
        return 1;
    }
    const QString exportDir = parser.isSet(dirOpt)
        ? parser.value(dirOpt)
        : QDir(QDir::tempPath()).absoluteFilePath(QStringLiteral("ww-spike-export"));
    ClipboardBridge::instance()->setSource(src.readAll(), exportDir);

    qmlRegisterSingletonInstance("SpikeShim", 1, 0, "Bridge", ClipboardBridge::instance());

    QQmlApplicationEngine engine;
    engine.load(QUrl(QStringLiteral("qrc:/qml/Main.qml")));
    if (engine.rootObjects().isEmpty()) {
        SpikeLog::write(QStringLiteral("error"), {{"what", "Main.qml failed to load"}});
        return 1;
    }
    auto *window = qobject_cast<QQuickWindow *>(engine.rootObjects().first());
    SpikeLog::write(QStringLiteral("qml_loaded"));

    if (parser.isSet(copyOpt)) {
        QTimer::singleShot(500, &app, [] {
            const bool ok = ClipboardBridge::instance()->copyToClipboard();
            SpikeLog::write(QStringLiteral("auto_copy"), {{"ok", ok}});
        });
    }
    if (parser.isSet(dragOpt) && window) {
        ClipboardBridge::instance()->setAutoCancelDragMs(500);
        QTimer::singleShot(800, &app, [window] {
            auto *thumb = window->findChild<QQuickItem *>(QStringLiteral("thumb"));
            if (!thumb) {
                SpikeLog::write(QStringLiteral("error"), {{"what", "thumb not found"}});
                return;
            }
            const QPointF centre = thumb->mapToScene(QPointF(thumb->width() / 2, thumb->height() / 2));
            auto send = [window](QEvent::Type t, QPointF p, Qt::MouseButton b, Qt::MouseButtons bs) {
                QMouseEvent e(t, p, window->mapToGlobal(p), b, bs, Qt::NoModifier);
                QCoreApplication::sendEvent(window, &e);
            };
            send(QEvent::MouseButtonPress, centre, Qt::LeftButton, Qt::LeftButton);
            // The move starts the drag; exec() returns when DoDragDrop ends (cancelled at once).
            send(QEvent::MouseMove, centre + QPointF(24, 24), Qt::NoButton, Qt::LeftButton);
            send(QEvent::MouseButtonRelease, centre + QPointF(24, 24), Qt::LeftButton, Qt::NoButton);
            SpikeLog::write(QStringLiteral("selftest_drag_done"));
        });
    }
    if (parser.isSet(quitOpt))
        QTimer::singleShot(parser.value(quitOpt).toInt(), &app, [&app] {
            SpikeLog::write(QStringLiteral("quit"));
            app.quit();
        });

    const int rc = app.exec();
    SpikeLog::write(QStringLiteral("exit"), {{"code", rc}});
    return rc;
}

