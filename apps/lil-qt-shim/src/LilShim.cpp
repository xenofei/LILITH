#include "LilShim.h"

#include "lilshim/Activation.h"

LilShim::LilShim(QObject *parent)
    : QObject(parent)
{
}

bool LilShim::isModuleLoaded(const QString &dll) const
{
    return lilshim::isModuleLoaded(dll);
}

void LilShim::showInExplorer(const QString &path) const
{
    lilshim::showInExplorer(path);
}
