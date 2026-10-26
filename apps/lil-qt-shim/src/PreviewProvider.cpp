#include "lilshim/PreviewProvider.h"

namespace lilshim {

PreviewProvider::PreviewProvider()
    : QQuickImageProvider(QQuickImageProvider::Image)
{
}

PreviewProvider::~PreviewProvider() = default;

QImage PreviewProvider::requestImage(const QString &id, QSize *size, const QSize &requestedSize)
{
    Q_UNUSED(id)
    Q_UNUSED(requestedSize)
    if (size)
        *size = QSize();
    return {};
}

} // namespace lilshim
