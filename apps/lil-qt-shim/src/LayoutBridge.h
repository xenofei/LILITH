#pragma once
// DockLayout, a QML singleton of module Lilith.Shim: KDDockWidgets' LayoutSaver for the shell
// (APP-002, APP-003). The shell restores in APP-003's order, one call per step, and each call
// records its step (the test hook, `steps()`, also logged as `layout_step`):
//   1. windowsCreated()           the main window and its docks exist (from here on, each main
//                                 window's close emits mainWindowClosing(name) first, from an
//                                 application event filter, before KDDockWidgets closes its
//                                 docks, so the shell can save; saveRequested() comes at a
//                                 logoff, a shutdown and the application's quit);
//   2. registerDockFactory(fn)    KDDockWidgets' dock factory: fn(name) makes the dock of a panel
//                                 a layout names that doesn't exist yet (or returns null);
//   3. restore(kddwJson)          LayoutSaver's restore; false when it refuses the blob;
//   4. clamp(saved, fn)           every main and floating window clamped to a screen's available
//                                 area: fn(geometry, savedScreenName, screens, frameMargins)
//                                 returns the client geometry whose frame fits (lil_qt's
//                                 clamp_client). A main window takes its
//                                 geometry, screen and maximized state from `saved` (the layout
//                                 file's windows) when it is listed there.
// The layout file itself (reading, writing, setting aside) is lil_qt's (ShellState).

#include <QJSValue>
#include <QJsonObject>
#include <QHash>
#include <QObject>
#include <QWindow>
#include <QRect>
#include <QStringList>
#include <QVariantList>
#include <QtQml/qqmlregistration.h>


class LayoutBridge : public QObject
{
    Q_OBJECT
    QML_NAMED_ELEMENT(DockLayout)
    QML_SINGLETON

public:
    explicit LayoutBridge(QObject *parent = nullptr);
    ~LayoutBridge() override;

    // The steps recorded so far, oldest first: windows_created, factory_registered, restore,
    // clamp.
    Q_INVOKABLE QStringList steps() const;
    // The same steps with what each found: windows_created {main_windows}, restore {ok},
    // clamp {windows}.
    Q_INVOKABLE QVariantList stepDetails() const;
    Q_INVOKABLE void clearSteps();

    Q_INVOKABLE void windowsCreated();
    Q_INVOKABLE void registerDockFactory(const QJSValue &create);
    Q_INVOKABLE bool restore(const QString &kddwJson);
    // The number of windows clamped.
    Q_INVOKABLE int clamp(const QVariantList &saved, const QJSValue &clampFn);

    // Every window's layout, as KDDockWidgets saves it (JSON text).
    Q_INVOKABLE QString serialize() const;
    // The main windows as the layout file lists them: [{name, geometry: {x, y, w, h} (the
    // normal geometry), maximized, screen: {name, scale}}].
    Q_INVOKABLE QVariantList mainWindows() const;
    // The screens, the primary first: [{name, available: {x, y, w, h}, scale}].
    Q_INVOKABLE QVariantList screens() const;

    bool eventFilter(QObject *watched, QEvent *event) override;

signals:
    // Main window `name` is about to close: its docks are still where they are.
    void mainWindowClosing(const QString &name);
    // A press, a touch or a pen reached this window (a floating dock's is another window: a popup's own
    // close-on-press-outside never hears of it, and it may not change which window is active).
    void windowPressed(QWindow *window);
    // The session is ending or the application quits: save now.
    void saveRequested();

private:
    void step(const QString &name, const QJsonObject &fields = {});
    QRect normalGeometry(QWindow *w) const;

    QStringList m_steps;
    QVariantList m_details;
    QHash<QObject *, QString> m_watched;
    // Each main window's last geometry while it was neither maximized nor minimized.
    QHash<QObject *, QRect> m_normal;
};
