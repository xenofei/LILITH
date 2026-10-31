// The Qt Quick Test runner for apps/lil-ui-kit/tests (QA-068). CTest runs each tst_*.qml in its
// own process (`-input <file>`), under QT_QPA_PLATFORM=offscreen (the qml and grabs presets).
#include "lilshim/DockFactory.h"
#include "lilshim/Fonts.h"
#include "lilshim/PreviewProvider.h"

#include "lil_qt/src/qt/boot.cxxqt.h"
#include "lil_qt/src/qt/launch_sink.h"
#include "lil_qt/src/qt/preview_feed.cxxqt.h"
#include "lil_qt/src/qt/preview_sink.h"

#include <kddockwidgets/Config.h>
#include <kddockwidgets/qtquick/Platform.h>

#include <atomic>
#include <cstring>
#include <string>

#include <QCoreApplication>
#include <QNetworkAccessManager>
#include <QNetworkRequest>
#include <QQmlNetworkAccessManagerFactory>
#include <QDir>
#include <QProcess>
#include <QFile>
#include <QQmlContext>
#include <QQmlEngine>
#include <QStandardPaths>
#include <QTemporaryDir>
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

// Every network request the QML engine makes (an <img> in rich text among them), counted: a
// test of the controls checks that markup in their text loads nothing.
static std::atomic<int> s_networkRequests{0};

class CountingNam : public QNetworkAccessManager
{
public:
    using QNetworkAccessManager::QNetworkAccessManager;

protected:
    QNetworkReply *createRequest(Operation op, const QNetworkRequest &request, QIODevice *data) override
    {
        ++s_networkRequests;
        return QNetworkAccessManager::createRequest(op, request, data);
    }
};

class CountingNamFactory : public QQmlNetworkAccessManagerFactory
{
public:
    QNetworkAccessManager *create(QObject *parent) override { return new CountingNam(parent); }
};

// TestFiles (a context property): the file access the QML tests need, all of it under one
// temporary folder per test process (Task 18's layout tests), and the environment.
class TestFiles : public QObject
{
    Q_OBJECT

public:
    // A new, empty folder under this process's temporary folder.
    Q_INVOKABLE QString newDir(const QString &name)
    {
        const QString path = m_root.filePath(QStringLiteral("%1-%2").arg(name).arg(++m_dirs));
        QDir().mkpath(path);
        return path;
    }

    Q_INVOKABLE bool write(const QString &path, const QString &text)
    {
        QFile f(path);
        return f.open(QIODevice::WriteOnly | QIODevice::Truncate) && f.write(text.toUtf8()) >= 0;
    }

    // The file's text, or null when there is none.
    Q_INVOKABLE QVariant read(const QString &path) const
    {
        QFile f(path);
        if (!f.open(QIODevice::ReadOnly))
            return {};
        return QString::fromUtf8(f.readAll());
    }

    Q_INVOKABLE QStringList list(const QString &dir) const
    {
        return QDir(dir).entryList(QDir::Files | QDir::Hidden, QDir::Name);
    }

    Q_INVOKABLE QString env(const QString &name) const { return qEnvironmentVariable(name.toUtf8().constData()); }

    Q_INVOKABLE bool mkpath(const QString &path) const { return QDir().mkpath(path); }

    // How many network requests the QML engine has made so far.
    Q_INVOKABLE int networkRequests() const { return s_networkRequests; }

private:
    QTemporaryDir m_root;
    int m_dirs = 0;
};

// TestLaunch (a context property): lil-image's own launch path for tst_launch (review I-5),
// on a test channel of the process's own. The second launch is this runner started again with
// `--forward-launch <rest>`, which runs lil_qt::boot_from and claim_instance as main.cpp's step
// 3b does and exits with their code (main, below).
class TestLaunch : public QObject
{
    Q_OBJECT

public:
    // A fresh test channel for this process and the launches it starts: its root under the
    // temporary folder, with roaming, local and profile.
    Q_INVOKABLE bool useTestChannel()
    {
        if (!m_root.isValid())
            return false;
        for (const char *dir : {"roaming", "local", "profile"})
            QDir().mkpath(m_root.filePath(QString::fromLatin1(dir)));
        qputenv("LIL_CHANNEL", QStringLiteral("test-qml%1").arg(QCoreApplication::applicationPid(), 0, 16).toUtf8());
        qputenv("LIL_TEST_ROOT", QDir::toNativeSeparators(m_root.path()).toUtf8());
        return true;
    }

    // This process claims the app's pipe: -1 when it is the instance (main.cpp's step 3b).
    Q_INVOKABLE int claim(const QString &rest)
    {
        const QByteArray line = ("\"tst\" " + rest).toUtf8();
        const lil_qt::QtBoot plan = lil_qt::boot_from(rust::Str(line.constData(), size_t(line.size())));
        if (!plan.refused.empty())
            return 2;
        return lil_qt::claim_instance();
    }

    // Launches go to `window` from now on (main.cpp, once its window is loaded).
    Q_INVOKABLE void deliverTo(QObject *window)
    {
        lil_qt::deliverLaunchesTo(window);
        lil_qt::launches_ready();
    }

    // A second launch with the command line's rest `rest`: its exit code.
    Q_INVOKABLE int secondLaunch(const QString &rest) const
    {
        return QProcess::execute(QCoreApplication::applicationFilePath(), {QStringLiteral("--forward-launch"), rest});
    }

    // The instance's end (main.cpp, after the event loop).
    Q_INVOKABLE void release()
    {
        lil_qt::deliverLaunchesTo(nullptr);
        lil_qt::release_instance();
    }

private:
    QTemporaryDir m_root;
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
        engine->setNetworkAccessManagerFactory(&m_nam);
        // Lilith.Web is a dynamic module in <bin>/Lilith/Web, beside this executable.
        engine->addImportPath(QCoreApplication::applicationDirPath());
        engine->addImageProvider(QStringLiteral("lilpreview"), new lilshim::PreviewProvider);
        engine->rootContext()->setContextProperty(QStringLiteral("BridgeHook"), new BridgeHook(engine));
        engine->rootContext()->setContextProperty(QStringLiteral("TestFiles"), &m_files);
        engine->rootContext()->setContextProperty(QStringLiteral("TestLaunch"), &m_launch);
        // The theme's bundled face as the application font, as lil-image sets it.
        if (QObject *theme = engine->singletonInstance<QObject *>("Lilith.UiKit", "Theme"))
            lilshim::applyThemeFont(theme->property("fontFamily").toString());
    }

private:
    TestFiles m_files;
    TestLaunch m_launch;
    CountingNamFactory m_nam;
};

// APP-137: set before the application object, which quick_test_main_with_setup creates, so
// WebEngine can load later through a Loader.
static const bool s_shareContexts = [] {
    QCoreApplication::setAttribute(Qt::AA_ShareOpenGLContexts);
    return true;
}();

// QUICK_TEST_MAIN_WITH_SETUP's main, with tst_launch's second launch first: `--forward-launch
// <rest>` runs lil-image's step 3b on the command line `"tst" <rest>`, before any Qt object, and
// exits with its code (3 when it became the instance instead, which that test never wants).
int main(int argc, char **argv)
{
    if (argc >= 2 && std::strcmp(argv[1], "--forward-launch") == 0) {
        const std::string line = std::string("\"tst\" ") + (argc >= 3 ? argv[2] : "");
        const lil_qt::QtBoot plan = lil_qt::boot_from(rust::Str(line.data(), line.size()));
        if (!plan.refused.empty())
            return 2;
        const int code = lil_qt::claim_instance();
        return code >= 0 ? code : 3;
    }
    // As lil-image does (lil_qt::qt_env_to_clear): QML's local time is Windows', whatever TZ the
    // shell that started the tests has.
    qunsetenv("TZ");
    Setup setup;
    return quick_test_main_with_setup(argc, argv, "lil_ui_kit", nullptr, &setup);
}

#include "main.moc"
