#pragma once
// LayoutSaver for QML (S0.1): save every window, save one main window alone
// (LayoutSaver::addWindowToSave), restore, and describe a saved file.

#include "spike_shim_global.h"

#include <QObject>
#include <QVariantMap>

class SPIKE_SHIM_EXPORT SpikeLayout : public QObject
{
    Q_OBJECT
public:
    static SpikeLayout *instance();

    Q_INVOKABLE bool saveAll(const QString &path);
    // Saves only the main window with this unique name (LayoutSaver::addWindowToSave()).
    Q_INVOKABLE bool saveMainWindow(const QString &uniqueName, const QString &path);
    Q_INVOKABLE bool restore(const QString &path);
    // {main_windows: [unique names], floating_windows: <count>, opened_docks: [unique names]}
    Q_INVOKABLE QVariantMap describe(const QString &path) const;

private:
    SpikeLayout() = default;
};
