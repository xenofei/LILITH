// Canvas compositing spike (Task 3: IMG-152, IMG-030-032, IMG-035, IMG-036).
// The Task 2 app with the canvas dock only: a KDDockWidgets window whose Canvas dock holds a
// CompositeCanvas (QQuickRhiItem, store-only compute compositing).
//
// Switches:
//   --log <file>            JSON-lines event log
//   --golden <dir>          golden scene; writes the CPU reference and the GPU read-backs to <dir>
//   --layers N              procedural document with N layers (default 6 when --doc is given)
//   --doc WxH               procedural document size (default 1024x1024 when --layers is given)
//   --no-group              procedural document without the isolated group
//   --backend d3d11|d3d12   sets QSG_RHI_BACKEND before Qt starts
//   --frames N              stop after N frames (golden: after the final read-back that follows)
//   --frametimes <json>     frame-to-frame times (frameSwapped) and 4 ms GUI-heartbeat stalls
//   --recreate-every N      destroy and recreate the canvas item (and so its renderer) every N frames
//   --auto-pan              pan continuously at 1000 px/s, bouncing at the document edges
//   --zoom Z                pixels per document pixel (default 1; golden mode fits the document)
//   --trim-flag <path>      when that file's modification time changes, trim the GPU tile cache to
//                           the visible tiles (stands in for IMG-040's "trim on submit")
//   --recomposite           re-composite every visible tile every frame (composite timing)
//   --gpu-timestamps        enable QRhi timestamps (per-frame GPU time in --frametimes)
//   --upload-mb N           layer-tile upload budget per frame (default 24)
//   --gpu-cache-mb N        Task 8: GPU tile-cache budget the process holds (default 512 = the bound arrays'
//                           cap, i.e. Task 3/4 behaviour; Task 8's companions.ps1 passes 1536). Above the cap
//                           the extra (N - 512) MiB is held as unbound RGBA8 texture arrays
//   --screenshot <png>      QQuickWindow::grabWindow() of the main window before quitting
//   --quit-after <ms>       quit after <ms>
//   --inject-hazard         negative control for criterion 2: the parent composite pass also samples
//                           the projection texture it writes (the output is then wrong on purpose)
// Task 4 (IMG-153, APP-135, IMG-033):
//   --composite-variant V   twopass (default; Task 3's group pass + parent pass), single (group in
//                           registers, one pass) or inplace (projection read-write, one pass), each as
//                           DXBC SM 5.0; append -sm66 for the dxc DXIL SM 6.6 build (D3D12 only).
//                           "inplace" alone means inplace-sm66 (R5); inplace-sm50 is the fxc build.
//   --measure-after <ms>    --frametimes: samples before <ms> are reported separately as warm-up
//   --no-canvas             control run: the window without the canvas item (heartbeat baseline)
//   --gui-pan               with --auto-pan: a QML NumberAnimation on the GUI thread drives the pan, so
//                           every frame needs a scene-graph sync (as a pan driven by input events would)
//   --on-top                keep the window above other windows (Qt::WindowStaysOnTopHint). Windows 11
//                           drops a fully occluded or minimized window's process to the default 15.6 ms
//                           timer resolution, which makes every 4 ms heartbeat ~11.6 ms "late"; the
//                           frame-time runs therefore keep the window visible.

#include "CanvasController.h"
#include "CanvasDocument.h"
#include "CompositeRenderer.h"
#include "SpikeLog.h"

#include <kddockwidgets/Config.h>
#include <kddockwidgets/qtquick/Platform.h>

#include <QCommandLineParser>
#include <QGuiApplication>
#include <QJsonArray>
#include <QQmlApplicationEngine>
#include <QQuickGraphicsConfiguration>
#include <QQuickWindow>

#include <cstring>

int main(int argc, char *argv[])
{
    // --backend must take effect before Qt Quick picks its graphics API.
    for (int i = 1; i + 1 < argc; ++i) {
        if (std::strcmp(argv[i], "--backend") == 0)
            qputenv("QSG_RHI_BACKEND", argv[i + 1]);
    }
    QGuiApplication app(argc, argv);
    QCoreApplication::setApplicationName(QStringLiteral("canvas_spike"));

    QCommandLineParser parser;
    parser.addHelpOption();
    const QCommandLineOption logOpt(QStringLiteral("log"), QStringLiteral("JSON-lines log file."), QStringLiteral("file"));
    const QCommandLineOption goldenOpt(QStringLiteral("golden"), QStringLiteral("Golden scene; output directory."), QStringLiteral("dir"));
    const QCommandLineOption layersOpt(QStringLiteral("layers"), QStringLiteral("Procedural document layer count."), QStringLiteral("N"));
    const QCommandLineOption docOpt(QStringLiteral("doc"), QStringLiteral("Procedural document size WxH."), QStringLiteral("WxH"));
    const QCommandLineOption noGroupOpt(QStringLiteral("no-group"), QStringLiteral("Procedural document without the isolated group."));
    const QCommandLineOption backendOpt(QStringLiteral("backend"), QStringLiteral("d3d11 or d3d12 (QSG_RHI_BACKEND)."), QStringLiteral("api"));
    const QCommandLineOption framesOpt(QStringLiteral("frames"), QStringLiteral("Stop after N frames."), QStringLiteral("N"));
    const QCommandLineOption frametimesOpt(QStringLiteral("frametimes"), QStringLiteral("Write frame times to <json>."), QStringLiteral("json"));
    const QCommandLineOption recreateOpt(QStringLiteral("recreate-every"), QStringLiteral("Recreate the renderer every N frames."), QStringLiteral("N"));
    const QCommandLineOption autoPanOpt(QStringLiteral("auto-pan"), QStringLiteral("Pan at 1000 px/s, bouncing at the edges."));
    const QCommandLineOption zoomOpt(QStringLiteral("zoom"), QStringLiteral("Pixels per document pixel."), QStringLiteral("Z"));
    const QCommandLineOption trimOpt(QStringLiteral("trim-flag"), QStringLiteral("Trim the GPU tile cache when <path>'s mtime changes."), QStringLiteral("path"));
    const QCommandLineOption recompOpt(QStringLiteral("recomposite"), QStringLiteral("Re-composite all visible tiles every frame."));
    const QCommandLineOption tsOpt(QStringLiteral("gpu-timestamps"), QStringLiteral("Enable QRhi GPU timestamps."));
    const QCommandLineOption cacheMbOpt(QStringLiteral("gpu-cache-mb"), QStringLiteral("GPU tile-cache budget in MiB (default 512)."), QStringLiteral("N"));
    const QCommandLineOption uploadOpt(QStringLiteral("upload-mb"), QStringLiteral("Upload budget per frame in MiB."), QStringLiteral("N"));
    const QCommandLineOption shotOpt(QStringLiteral("screenshot"), QStringLiteral("Grab the main window before quitting."), QStringLiteral("png"));
    const QCommandLineOption quitOpt(QStringLiteral("quit-after"), QStringLiteral("Quit after <ms>."), QStringLiteral("ms"));
    const QCommandLineOption hazardOpt(QStringLiteral("inject-hazard"), QStringLiteral("Negative control: read+write the projection."));
    const QCommandLineOption variantOpt(QStringLiteral("composite-variant"),
                                        QStringLiteral("twopass[-sm66], single[-sm66], inplace (= inplace-sm66) or inplace-sm50."), QStringLiteral("V"));
    const QCommandLineOption measureOpt(QStringLiteral("measure-after"), QStringLiteral("Frame-time warm-up in ms."), QStringLiteral("ms"));
    const QCommandLineOption noCanvasOpt(QStringLiteral("no-canvas"), QStringLiteral("Control run without the canvas item."));
    const QCommandLineOption onTopOpt(QStringLiteral("on-top"), QStringLiteral("Keep the window above other windows."));
    const QCommandLineOption guiPanOpt(QStringLiteral("gui-pan"), QStringLiteral("Drive the pan from the GUI thread (QML animation)."));
    parser.addOptions({logOpt, goldenOpt, layersOpt, docOpt, noGroupOpt, backendOpt, framesOpt, frametimesOpt, recreateOpt,
                       autoPanOpt, zoomOpt, trimOpt, recompOpt, tsOpt, uploadOpt, shotOpt, quitOpt, hazardOpt,
                       variantOpt, measureOpt, noCanvasOpt, onTopOpt, guiPanOpt, cacheMbOpt});
    parser.process(app);

    if (parser.isSet(logOpt))
        SpikeLog::open(parser.value(logOpt));
    SpikeLog::installMessageHandler();
    SpikeLog::write(QStringLiteral("start"),
                    {{"qt_version", QString::fromLatin1(qVersion())},
                     {"pid", qint64(QCoreApplication::applicationPid())},
                     {"args", QJsonArray::fromStringList(QCoreApplication::arguments())},
                     {"QSG_RHI_BACKEND", QString::fromLocal8Bit(qgetenv("QSG_RHI_BACKEND"))},
                     {"QSG_RHI_DEBUG_LAYER", QString::fromLocal8Bit(qgetenv("QSG_RHI_DEBUG_LAYER"))}});

    // The document.
    const bool golden = parser.isSet(goldenOpt) || (!parser.isSet(layersOpt) && !parser.isSet(docOpt));
    std::shared_ptr<canvas::CanvasDocument> doc;
    if (golden) {
        doc = canvas::CanvasDocument::golden();
    } else {
        QSize size(1024, 1024);
        if (parser.isSet(docOpt)) {
            const QStringList wh = parser.value(docOpt).toLower().split(QLatin1Char('x'));
            if (wh.size() == 2)
                size = QSize(wh[0].toInt(), wh[1].toInt());
        }
        if (size.width() < 1 || size.height() < 1 || size.width() > 16384 || size.height() > 16384) {
            SpikeLog::write(QStringLiteral("error"), {{"what", "bad --doc"}, {"value", parser.value(docOpt)}});
            return 2;
        }
        const int layers = parser.isSet(layersOpt) ? parser.value(layersOpt).toInt() : 6;
        doc = canvas::CanvasDocument::procedural(layers, size, !parser.isSet(noGroupOpt));
    }
    SpikeLog::write(QStringLiteral("document"),
                    {{"scene", doc->description()}, {"tiles", doc->tileCount()},
                     {"pixel_layers", doc->pixelLayerCount()}, {"non_empty_layer_tiles", doc->nonEmptyTileCount()}});

    auto intValue = [&](const QCommandLineOption &o, int def) { return parser.isSet(o) ? parser.value(o).toInt() : def; };
    CanvasController::Options opt;
    opt.frames = intValue(framesOpt, -1);
    opt.recreateEvery = intValue(recreateOpt, 0);
    opt.goldenDir = parser.isSet(goldenOpt) ? parser.value(goldenOpt) : QString();
    opt.frametimesPath = parser.value(frametimesOpt);
    opt.trimFlag = parser.value(trimOpt);
    opt.screenshot = parser.value(shotOpt);
    opt.quitAfterMs = intValue(quitOpt, -1);
    opt.measureAfterMs = intValue(measureOpt, 0);
    opt.noCanvas = parser.isSet(noCanvasOpt);
    opt.guiPan = parser.isSet(guiPanOpt);

    CanvasConfig cfg;
    cfg.doc = doc;
    cfg.golden = golden;
    cfg.autoPan = parser.isSet(autoPanOpt) || parser.isSet(guiPanOpt);
    cfg.guiPan = parser.isSet(guiPanOpt);
    cfg.zoom = parser.isSet(zoomOpt) ? std::max(0.01, parser.value(zoomOpt).toDouble()) : 1.0;
    cfg.recomposite = parser.isSet(recompOpt);
    cfg.injectHazard = parser.isSet(hazardOpt);
    if (parser.isSet(variantOpt)) {
        QString v = parser.value(variantOpt);
        if (v == QLatin1String("inplace"))
            v = QStringLiteral("inplace-sm66");
        cfg.sm66 = v.endsWith(QLatin1String("-sm66"));
        if (v.endsWith(QLatin1String("-sm66")) || v.endsWith(QLatin1String("-sm50")))
            v.chop(5);
        const QStringList names{QStringLiteral("twopass"), QStringLiteral("single"), QStringLiteral("inplace")};
        cfg.variant = int(names.indexOf(v));
        if (cfg.variant < 0) {
            SpikeLog::write(QStringLiteral("error"), {{"what", "bad --composite-variant"}, {"value", v}});
            return 2;
        }
    }
    cfg.continuous = opt.frames > 0 || cfg.autoPan || cfg.recomposite || !opt.frametimesPath.isEmpty() || opt.recreateEvery > 0;
    cfg.uploadBudgetBytes = qint64(std::max(1, intValue(uploadOpt, 24))) << 20;
    cfg.gpuCacheBytes = qint64(std::max(0, intValue(cacheMbOpt, 512))) << 20;
    CanvasHub::instance()->setConfig(cfg);

    CanvasController controller(opt, doc);
    if (!opt.goldenDir.isEmpty() && !controller.prepareGolden()) {
        SpikeLog::write(QStringLiteral("error"), {{"what", "could not write the golden reference"}, {"dir", opt.goldenDir}});
        return 2;
    }

    KDDockWidgets::initFrontend(KDDockWidgets::FrontendType::QtQuick);
    qmlRegisterType<CompositeCanvas>("CanvasShim", 1, 0, "CompositeCanvas");
    qmlRegisterSingletonInstance("CanvasShim", 1, 0, "CanvasController", &controller);
    qmlRegisterSingletonInstance("CanvasShim", 1, 0, "SpikeLog", SpikeLog::instance());

    QQmlApplicationEngine engine;
    KDDockWidgets::QtQuick::Platform::instance()->setQmlEngine(&engine);
    engine.load(QUrl(QStringLiteral("qrc:/qml/Main.qml")));
    auto *window = engine.rootObjects().isEmpty() ? nullptr : qobject_cast<QQuickWindow *>(engine.rootObjects().first());
    if (!window) {
        SpikeLog::write(QStringLiteral("error"), {{"what", "Main.qml failed to load"}});
        return 1;
    }
    SpikeLog::write(QStringLiteral("qml_loaded"));
    if (parser.isSet(tsOpt)) {
        QQuickGraphicsConfiguration gc = window->graphicsConfiguration();
        gc.setTimestamps(true);
        window->setGraphicsConfiguration(gc);
    }
    controller.attachWindow(window);
    if (parser.isSet(onTopOpt))
        window->setFlag(Qt::WindowStaysOnTopHint, true);
    window->show();
    if (parser.isSet(onTopOpt))
        window->requestActivate();
    SpikeLog::write(QStringLiteral("window_shown"), {{"on_top", parser.isSet(onTopOpt)}, {"visibility", int(window->visibility())}});

    QObject::connect(&app, &QCoreApplication::aboutToQuit, [] { SpikeLog::write(QStringLiteral("quit")); });
    const int rc = app.exec();
    SpikeLog::write(QStringLiteral("exit"), {{"code", rc}});
    return rc;
}
