#include "SpikeLayout.h"
#include "SpikeLog.h"

#include <kddockwidgets/LayoutSaver.h>
#include <kddockwidgets/core/DockRegistry.h>
#include <kddockwidgets/core/MainWindow.h>

#include <QFile>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>

SpikeLayout *SpikeLayout::instance()
{
    static SpikeLayout s;
    return &s;
}

bool SpikeLayout::saveAll(const QString &path)
{
    KDDockWidgets::LayoutSaver saver;
    const bool ok = saver.saveToFile(path);
    SpikeLog::write(QStringLiteral("layout_save_all"), {{"path", path}, {"ok", ok}});
    return ok;
}

bool SpikeLayout::saveMainWindow(const QString &uniqueName, const QString &path)
{
    auto *mw = KDDockWidgets::DockRegistry::self()->mainWindowByName(uniqueName);
    bool ok = false;
    if (mw) {
        KDDockWidgets::LayoutSaver saver;
        saver.addWindowToSave(mw);
        ok = saver.saveToFile(path);
    }
    SpikeLog::write(QStringLiteral("layout_save_window"),
                    {{"path", path}, {"window", uniqueName}, {"found", mw != nullptr}, {"ok", ok}});
    return ok;
}

bool SpikeLayout::restore(const QString &path)
{
    KDDockWidgets::LayoutSaver saver;
    const bool ok = saver.restoreFromFile(path);
    SpikeLog::write(QStringLiteral("layout_restore"), {{"path", path}, {"ok", ok}});
    return ok;
}

QVariantMap SpikeLayout::describe(const QString &path) const
{
    QVariantMap out;
    QFile f(path);
    if (!f.open(QIODevice::ReadOnly))
        return out;
    const QJsonObject root = QJsonDocument::fromJson(f.readAll()).object();
    QStringList mains;
    for (const QJsonValue &v : root.value(QStringLiteral("mainWindows")).toArray())
        mains << v.toObject().value(QStringLiteral("uniqueName")).toString();
    out.insert(QStringLiteral("main_windows"), mains);
    out.insert(QStringLiteral("floating_windows"),
               root.value(QStringLiteral("floatingWindows")).toArray().size());
    QStringList docks;
    for (const QString &d : KDDockWidgets::LayoutSaver::openedDockWidgetsInLayout(path))
        docks << d;
    out.insert(QStringLiteral("opened_docks"), docks);
    return out;
}
