// The Qt Quick Test runner for apps/lil-ui-kit/tests (QA-068). CTest runs each tst_*.qml in its
// own process (`-input <file>`), under QT_QPA_PLATFORM=offscreen (the qml and grabs presets).
#include "lilshim/DockFactory.h"

#include <kddockwidgets/Config.h>
#include <kddockwidgets/qtquick/Platform.h>

#include <QCoreApplication>
#include <QQmlEngine>
#include <QStandardPaths>
#include <QtQml/qqmlextensionplugin.h>
#include <QtQuickTest>

Q_IMPORT_QML_PLUGIN(Lilith_UiKitPlugin)
Q_IMPORT_QML_PLUGIN(Lilith_ShimPlugin)

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
    }

    void qmlEngineAvailable(QQmlEngine *engine)
    {
        KDDockWidgets::QtQuick::Platform::instance()->setQmlEngine(engine);
        // Lilith.Web is a dynamic module in <bin>/Lilith/Web, beside this executable.
        engine->addImportPath(QCoreApplication::applicationDirPath());
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
