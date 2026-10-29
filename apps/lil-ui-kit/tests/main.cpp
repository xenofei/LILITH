// The Qt Quick Test runner for apps/lil-ui-kit/tests (QA-068). CTest runs each tst_*.qml in its
// own process (`-input <file>`), under QT_QPA_PLATFORM=offscreen (the qml and grabs presets).
#include "lilshim/DockFactory.h"
#include "lilshim/Fonts.h"
#include "lilshim/PreviewProvider.h"

#include "lil_qt/src/qt/preview_feed.cxxqt.h"
#include "lil_qt/src/qt/preview_sink.h"

#include <kddockwidgets/Config.h>
#include <kddockwidgets/qtquick/Platform.h>

#include <QCoreApplication>
#include <QDir>
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

private:
    QTemporaryDir m_root;
    int m_dirs = 0;
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
        engine->rootContext()->setContextProperty(QStringLiteral("TestFiles"), &m_files);
        // The theme's bundled face as the application font, as lil-image sets it.
        if (QObject *theme = engine->singletonInstance<QObject *>("Lilith.UiKit", "Theme"))
            lilshim::applyThemeFont(theme->property("fontFamily").toString());
    }

private:
    TestFiles m_files;
};

// APP-137: set before the application object, which QUICK_TEST_MAIN_WITH_SETUP creates, so
// WebEngine can load later through a Loader.
static const bool s_shareContexts = [] {
    QCoreApplication::setAttribute(Qt::AA_ShareOpenGLContexts);
    return true;
}();

QUICK_TEST_MAIN_WITH_SETUP(lil_ui_kit, Setup)

#include "main.moc"
