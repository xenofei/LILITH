#pragma once
// The live-preview image provider (image://preview/<job_id>, APP-030). Task 17 fills it: until
// then every request gives an empty image.

#include "lilshim/shim_global.h"

#include <QQuickImageProvider>

namespace lilshim {

class LILSHIM_EXPORT PreviewProvider : public QQuickImageProvider
{
public:
    PreviewProvider();
    ~PreviewProvider() override;

    QImage requestImage(const QString &id, QSize *size, const QSize &requestedSize) override;
};

} // namespace lilshim
