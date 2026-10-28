// The Qt Quick Test runner for apps/lil-ui-kit/tests (QA-068). CTest runs each tst_*.qml in its
// own process (`-input <file>`), under QT_QPA_PLATFORM=offscreen (the qml and grabs presets).
#include "lilshim/DockFactory.h"
#include "lilshim/PreviewProvider.h"

#include "lil_qt/src/qt/preview_feed.cxxqt.h"
#include "lil_qt/src/qt/preview_sink.h"

#include <kddockwidgets/Config.h>
#include <kddockwidgets/qtquick/Platform.h>

#include <QCoreApplication>
#include <QQmlContext>
#include <QQmlEngine>
#include <QStandardPaths>
#include <QtQml/qqmlextensionplugin.h>
#include <QtQuickTest>

Q_IMPORT_QML_PLUGIN(Lilith_UiKitPlugin)
Q_IMPORT_QML_PLUGIN(Lilith_ShimPlugin)

// FakeCore.emitFromCore's way into the bridge's own event step (src/qt/preview_feed.rs), which
// Core's events go through: `BridgeHook.event(topic, kind, dataJson)` is the data QML then gets.
class BridgeHook : public QObject
{
    Q_OBJECT

public:
    using QObject::QObject;

    Q_INVOKABLE QString event(const QString &topic, const QString &kind, const QString &dataJson) const
    {
        const QByteArray t = topic.toUtf8();
        const QByteArray k = kind.toUtf8();
        const QByteArray d = dataJson.toUtf8();
        const rust::String out = lil_qt::preview_feed_event(rust::Str(t.constData(), size_t(t.size())),
                                                             rust::Str(k.constData(), size_t(k.size())),
                                                             rust::Str(d.constData(), size_t(d.size())));
        return QString::fromUtf8(out.data(), qsizetype(out.size()));
    }
};

class Setup : public QObject
{
    Q_OBJECT

public slots:
    void applicationAvailable()
    {
        QCoreApplication::setOrganizationName(QStringLiteral("LILITH-qmltest"));
        QCoreApplication::setApplicationName(QStringLiteral("image"));
        // The writable locations (the WebEngine profile, the QML cache) move to Qt's test
        // folders, away from the channels' own data folders.
        QStandardPaths::setTestModeEnabled(true);
        KDDockWidgets::initFrontend(KDDockWidgets::FrontendType::QtQuick);
        lilshim::DockFactory::install();
        lil_qt::setPreviewSink(&lilshim::putPreviewFrame, &lilshim::dropPreviewFrame);
    }

    void qmlEngineAvailable(QQmlEngine *engine)
    {
        KDDockWidgets::QtQuick::Platform::instance()->setQmlEngine(engine);
        // Lilith.Web is a dynamic module in <bin>/Lilith/Web, beside this executable.
        engine->addImportPath(QCoreApplication::applicationDirPath());
        engine->addImageProvider(QStringLiteral("lilpreview"), new lilshim::PreviewProvider);
        engine->rootContext()->setContextProperty(QStringLiteral("BridgeHook"), new BridgeHook(engine));
    }
};

// APP-137: set before the application object, which QUICK_TEST_MAIN_WITH_SETUP creates, so
// WebEngine can load later through a Loader.
static const bool s_shareContexts = [] {
    QCoreApplication::setAttribute(Qt::AA_ShareOpenGLContexts);
    return true;
}();

QUICK_TEST_MAIN_WITH_SETUP(lil_ui_kit, Setup)

#include "main.moc"
