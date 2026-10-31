#include "LayoutBridge.h"

#include "lilshim/LogBridge.h"

#include <kddockwidgets/Config.h>
#include <kddockwidgets/LayoutSaver.h>
#include <kddockwidgets/core/DockRegistry.h>
#include <kddockwidgets/core/DockWidget.h>
#include <kddockwidgets/core/FloatingWindow.h>
#include <kddockwidgets/core/MainWindow.h>
#include <kddockwidgets/qtquick/views/DockWidget.h>
#include <kddockwidgets/qtquick/views/View.h>

#include <QGuiApplication>
#include <QSessionManager>
#include <QJSEngine>
#include <QJsonArray>
#include <QQuickItem>
#include <QQuickWindow>
#include <QScreen>
#include <QVariantMap>
#include <QWindow>
#include <QtGui/private/qhighdpiscaling_p.h>
#include <qpa/qplatformwindow.h>

namespace {

// The dock factory's JS function, set by the latest registerDockFactory: KDDockWidgets' factory
// is a plain function pointer.
QJSValue s_create;
const LayoutBridge *s_owner = nullptr;

KDDockWidgets::Core::DockWidget *createDock(const QString &name)
{
    if (!s_create.isCallable())
        return nullptr;
    QObject *made = s_create.call({QJSValue(name)}).toQObject();
    // A DockWidget made in QML is KDDockWidgets' instantiator; its view is `dockWidget`.
    QObject *view = made ? qvariant_cast<QObject *>(made->property("dockWidget")) : nullptr;
    auto *dock = qobject_cast<KDDockWidgets::QtQuick::DockWidget *>(view);
    lilshim::logEvent(QStringLiteral("layout_dock_factory"), {{"name", name}, {"made", dock != nullptr}});
    return dock ? dock->dockWidget() : nullptr;
}

QWindow *windowOf(KDDockWidgets::Core::Controller *controller)
{
    QQuickItem *item = KDDockWidgets::QtQuick::asQQuickItem(controller);
    return item ? item->window() : nullptr;
}

QVariantMap rectMap(const QRect &r)
{
    return {{"x", r.x()}, {"y", r.y()}, {"w", r.width()}, {"h", r.height()}};
}

QRect mapRect(const QVariantMap &m)
{
    return QRect(m.value("x").toInt(), m.value("y").toInt(), m.value("w").toInt(), m.value("h").toInt());
}

QVariantMap marginsMap(const QMargins &m)
{
    return {{"left", m.left()}, {"top", m.top()}, {"right", m.right()}, {"bottom", m.bottom()}};
}

} // namespace

LayoutBridge::LayoutBridge(QObject *parent)
    : QObject(parent)
{
    // An application filter runs before every object's own, KDDockWidgets' close filter among
    // them, whatever order they were installed in.
    qGuiApp->installEventFilter(this);
    // A logoff or shutdown quits without a close event: the shell saves then too.
    connect(qGuiApp, &QGuiApplication::commitDataRequest, this, [this](QSessionManager &) { emit saveRequested(); });
    connect(qGuiApp, &QCoreApplication::aboutToQuit, this, &LayoutBridge::saveRequested);
}

LayoutBridge::~LayoutBridge()
{
    if (s_owner == this) {
        KDDockWidgets::Config::self().setDockWidgetFactoryFunc(nullptr);
        s_create = QJSValue();
        s_owner = nullptr;
    }
}

void LayoutBridge::step(const QString &name, const QJsonObject &fields)
{
    m_steps << name;
    QJsonObject all = fields;
    all.insert(QStringLiteral("step"), name);
    m_details.append(all.toVariantMap());
    lilshim::logEvent(QStringLiteral("layout_step"), all);
}

bool LayoutBridge::eventFilter(QObject *watched, QEvent *event)
{
    const QEvent::Type type = event->type();
    if (type == QEvent::MouseButtonPress) {
        if (auto *window = qobject_cast<QWindow *>(watched))
            emit windowPressed(window);
        return false;
    }
    if (type != QEvent::Close && type != QEvent::Move && type != QEvent::Resize)
        return false;
    const auto it = m_watched.constFind(watched);
    if (it == m_watched.constEnd())
        return false;
    auto *w = static_cast<QWindow *>(watched);
    if (type == QEvent::Close)
        emit mainWindowClosing(it.value());
    else if (w->windowStates() == Qt::WindowNoState)
        m_normal.insert(w, w->geometry());
    return false;
}

// The geometry the window has when it isn't maximized: the platform's normal geometry (in
// device-independent pixels), else the last geometry seen while it was normal (the offscreen
// platform keeps none), else its geometry.
QRect LayoutBridge::normalGeometry(QWindow *w) const
{
    if (const QPlatformWindow *pw = w->handle()) {
        const QRect native = pw->normalGeometry();
        if (native.isValid())
            return QHighDpi::fromNativePixels(native, w);
    }
    if (w->windowStates() != Qt::WindowNoState && m_normal.contains(w))
        return m_normal.value(w);
    return w->geometry();
}

QStringList LayoutBridge::steps() const
{
    return m_steps;
}

QVariantList LayoutBridge::stepDetails() const
{
    return m_details;
}

void LayoutBridge::clearSteps()
{
    m_steps.clear();
    m_details.clear();
}

void LayoutBridge::windowsCreated()
{
    QJsonArray names;
    for (KDDockWidgets::Core::MainWindow *mw : KDDockWidgets::DockRegistry::self()->mainwindows()) {
        names.append(mw->uniqueName());
        QWindow *w = windowOf(mw);
        if (w && !m_watched.contains(w)) {
            m_watched.insert(w, mw->uniqueName());
            connect(w, &QObject::destroyed, this, [this, w] {
                m_watched.remove(w);
                m_normal.remove(w);
            });
        }
    }
    step(QStringLiteral("windows_created"), {{"main_windows", names}});
}

void LayoutBridge::registerDockFactory(const QJSValue &create)
{
    s_create = create;
    s_owner = this;
    KDDockWidgets::Config::self().setDockWidgetFactoryFunc(&createDock);
    step(QStringLiteral("factory_registered"));
}

bool LayoutBridge::restore(const QString &kddwJson)
{
    KDDockWidgets::LayoutSaver saver(KDDockWidgets::RestoreOption_CloseUnknownDockWidgets);
    const bool ok = saver.restoreLayout(kddwJson.toUtf8());
    step(QStringLiteral("restore"), {{"ok", ok}});
    return ok;
}

int LayoutBridge::clamp(const QVariantList &saved, const QJSValue &clampFn)
{
    QJSEngine *engine = qjsEngine(this);
    const QJSValue screenList = engine ? engine->toScriptValue(screens()) : QJSValue();
    // The frame is clamped, not the client area, so the title bar stays on the screen: the
    // platform window is made first, which gives the frame's margins.
    auto place = [&](QWindow *w, QRect geometry, const QString &screen, bool maximized) {
        w->create();
        const QJSValue r = clampFn.call({engine->toScriptValue(rectMap(geometry)), QJSValue(screen), screenList,
                                         engine->toScriptValue(marginsMap(w->frameMargins()))});
        const QRect clamped = mapRect(r.toVariant().toMap());
        if (!clamped.isValid())
            return false;
        w->setWindowStates(Qt::WindowNoState);
        w->setGeometry(clamped);
        if (m_watched.contains(w))
            m_normal.insert(w, clamped);
        if (maximized)
            w->setWindowStates(Qt::WindowMaximized);
        return true;
    };
    int n = 0;
    if (!engine || !clampFn.isCallable()) {
        step(QStringLiteral("clamp"), {{"windows", n}});
        return n;
    }
    for (KDDockWidgets::Core::MainWindow *mw : KDDockWidgets::DockRegistry::self()->mainwindows()) {
        QWindow *w = windowOf(mw);
        if (!w)
            continue;
        QRect geometry = normalGeometry(w);
        QString screen = w->screen() ? w->screen()->name() : QString();
        bool maximized = w->windowStates().testFlag(Qt::WindowMaximized);
        for (const QVariant &v : saved) {
            const QVariantMap entry = v.toMap();
            if (entry.value("name").toString() != mw->uniqueName())
                continue;
            geometry = mapRect(entry.value("geometry").toMap());
            screen = entry.value("screen").toMap().value("name").toString();
            maximized = entry.value("maximized").toBool();
        }
        n += place(w, geometry, screen, maximized);
    }
    for (KDDockWidgets::Core::FloatingWindow *fw : KDDockWidgets::DockRegistry::self()->floatingWindows()) {
        if (QWindow *w = windowOf(fw))
            n += place(w, w->geometry(), w->screen() ? w->screen()->name() : QString(), false);
    }
    step(QStringLiteral("clamp"), {{"windows", n}});
    return n;
}

QString LayoutBridge::serialize() const
{
    KDDockWidgets::LayoutSaver saver;
    return QString::fromUtf8(saver.serializeLayout());
}

QVariantList LayoutBridge::mainWindows() const
{
    QVariantList out;
    for (KDDockWidgets::Core::MainWindow *mw : KDDockWidgets::DockRegistry::self()->mainwindows()) {
        QWindow *w = windowOf(mw);
        if (!w)
            continue;
        QScreen *s = w->screen();
        out.append(QVariantMap{
            {"name", mw->uniqueName()},
            {"geometry", rectMap(normalGeometry(w))},
            {"maximized", w->windowStates().testFlag(Qt::WindowMaximized)},
            {"screen", QVariantMap{{"name", s ? s->name() : QString()},
                                   {"scale", s ? s->devicePixelRatio() : 1.0}}}});
    }
    return out;
}

QVariantList LayoutBridge::screens() const
{
    QList<QScreen *> all = QGuiApplication::screens();
    QScreen *primary = QGuiApplication::primaryScreen();
    if (all.removeOne(primary))
        all.prepend(primary);
    QVariantList out;
    for (QScreen *s : all) {
        out.append(QVariantMap{{"name", s->name()},
                               {"available", rectMap(s->availableGeometry())},
                               {"scale", s->devicePixelRatio()}});
    }
    return out;
}
