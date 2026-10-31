#pragma once
// LilShim, the QML singleton of module Lilith.Shim: the shim calls QML may make. The module is
// static, linked into lil-image.exe and the test executable (Ruling Q8).

#include <QObject>
#include <QString>
#include <QVariantMap>
#include <QtQml/qqmlregistration.h>

class LilShim : public QObject
{
    Q_OBJECT
    QML_ELEMENT
    QML_SINGLETON

public:
    explicit LilShim(QObject *parent = nullptr);

    // lilshim::activate on `window` (a QWindow, such as the shell's Window): brought to the front
    // with the focus, or shown with its taskbar button flashing (APP-014, APP-065).
    Q_INVOKABLE void activate(QObject *window, bool takeFocus) const;

    // lilshim::isModuleLoaded: whether `dll` is loaded in this process.
    Q_INVOKABLE bool isModuleLoaded(const QString &dll) const;

    // lilshim::showInExplorer.
    Q_INVOKABLE void showInExplorer(const QString &path) const;

    // lilshim::explorerSelectArgument: what showInExplorer would run, for a test that opens no
    // window.
    Q_INVOKABLE QString explorerSelectArgument(const QString &path) const;

    // The versions the core can't tell (Diagnostics; no protocol field carries them), each empty
    // when it can't be read: {qt (the running Qt), webEngine (the Qt WebEngine this build pairs
    // with, read without loading it, APP-137), kddockwidgets}.
    Q_INVOKABLE QVariantMap componentVersions() const;

    // What the accessibility layer (UI Automation's source) exposes for `item`:
    // {role: QAccessible::Role (QML's Accessible.Role), name, focusable, actions: [action names
    // such as "Press" and "SetFocus"]}; role 0 (NoRole) and an empty name when it has no
    // accessible interface. A11yWalk reads it.
    Q_INVOKABLE QVariantMap accessibleOf(QObject *item) const;

    // Sets the application palette from the theme (APP-012), so what Qt and KDDockWidgets
    // colour from the system palette (separators, floating-window frames, group borders) is
    // dark too. Keys: window, windowText, base, alternateBase, text, button, buttonText, light,
    // mid, dark, highlight, highlightedText, placeholderText; colours as QML colors.
    Q_INVOKABLE void applyPalette(const QVariantMap &colors) const;

    // lilshim::startStallWatchdog and what it has logged: {stalls, stacks} (the bench test).
    Q_INVOKABLE void startStallWatchdog() const;
    Q_INVOKABLE QVariantMap stallCounts() const;

    // What the application font resolves to at `weight` (QFont::Weight: 400 regular, 700
    // bold), through QFontInfo, and the style name of the face file behind it (QRawFont):
    // {family, weight, style}.
    Q_INVOKABLE QVariantMap resolvedFont(int weight) const;
};
