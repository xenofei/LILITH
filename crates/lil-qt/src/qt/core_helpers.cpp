#include "lil_qt/src/qt/core_helpers.h"

#include "lil_qt/src/qt/core_object.cxxqt.h"

#include <QtCore/QByteArray>
#include <QtCore/QJsonDocument>

namespace lil_qt {

QVariant jsonVariant(rust::Str json)
{
    const QJsonDocument doc = QJsonDocument::fromJson(QByteArray(json.data(), qsizetype(json.size())));
    if (doc.isNull())
        return QVariant();
    return doc.toVariant();
}

std::unique_ptr<Core> newCore()
{
    return std::make_unique<Core>();
}

} // namespace lil_qt
