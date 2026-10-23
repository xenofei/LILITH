// S0.9: the offscreen Qt Quick Test. Run it with QT_QPA_PLATFORM=offscreen (run-spike.ps1 records
// which extra backend variables, if any, it needs):
//   tst_app_shell.exe -input <spikes/app-shell/tests> [test functions]
#include "SpikeLog.h"

#include <kddockwidgets/Config.h>
#include <kddockwidgets/qtquick/Platform.h>

#include <QCoreApplication>
#include <QGuiApplication>
#include <QQmlEngine>
#include <QtQuickTest>

class Setup : public QObject
{
    Q_OBJECT
public slots:
    void applicationAvailable()
    {
        // Keep the test's caches beside the spike's own (S0.8), not in a folder named after the exe.
        QCoreApplication::setOrganizationName(QStringLiteral("LILITH-spike"));
        QCoreApplication::setApplicationName(QStringLiteral("image-test"));
        KDDockWidgets::initFrontend(KDDockWidgets::FrontendType::QtQuick);
    }

    void qmlEngineAvailable(QQmlEngine *engine)
    {
        KDDockWidgets::QtQuick::Platform::instance()->setQmlEngine(engine);
        // SpikeWeb lives in <bin>/SpikeWeb; the test exe is in the same folder, which is on the
        // import path, but the build folder is added explicitly too.
        engine->addImportPath(QStringLiteral(SPIKE_BIN_DIR));
        qmlRegisterSingletonInstance("SpikeShim", 1, 0, "SpikeLog", SpikeLog::instance());
    }
};

// AA_ShareOpenGLContexts must be set before the application object (APP-137's pattern), and
// QUICK_TEST_MAIN_WITH_SETUP creates it, so it's set from a static initialiser.
static const bool s_shareContexts = [] {
    QCoreApplication::setAttribute(Qt::AA_ShareOpenGLContexts);
    return true;
}();

QUICK_TEST_MAIN_WITH_SETUP(tst_app_shell, Setup)

#include "tst_main.moc"
