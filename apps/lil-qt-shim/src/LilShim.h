#pragma once
// LilShim, the QML singleton of module Lilith.Shim: the shim calls QML may make. The module is
// static, linked into lil-image.exe and the test executable (Ruling Q8).

#include <QObject>
#include <QString>
#include <QtQml/qqmlregistration.h>

class LilShim : public QObject
{
    Q_OBJECT
    QML_ELEMENT
    QML_SINGLETON

public:
    explicit LilShim(QObject *parent = nullptr);

    // lilshim::isModuleLoaded: whether `dll` is loaded in this process.
    Q_INVOKABLE bool isModuleLoaded(const QString &dll) const;

    // lilshim::showInExplorer (not available until Task 25).
    Q_INVOKABLE void showInExplorer(const QString &path) const;
};
