#include "lilshim/PreviewProvider.h"

#include <QHash>
#include <QImage>
#include <QMutex>
#include <QMutexLocker>

namespace lilshim {

namespace {

// Each job's newest frame, shared by every engine's provider and the bridge's threads.
struct Frames
{
    QMutex lock;
    QHash<QString, QImage> byJob;
};

Frames &frames()
{
    static Frames f;
    return f;
}

} // namespace

bool putPreviewFrame(const QString &job, const QByteArray &bytes)
{
    // Decoded before the lock is taken, so a request never waits on a decode.
    QImage image = QImage::fromData(bytes);
    if (image.isNull())
        return false;
    Frames &f = frames();
    QMutexLocker lock(&f.lock);
    f.byJob.insert(job, std::move(image));
    return true;
}

void dropPreviewFrame(const QString &job)
{
    Frames &f = frames();
    QMutexLocker lock(&f.lock);
    f.byJob.remove(job);
}

PreviewProvider::PreviewProvider()
    : QQuickImageProvider(QQuickImageProvider::Image, QQmlImageProviderBase::ForceAsynchronousImageLoading)
{
}

PreviewProvider::~PreviewProvider() = default;

QImage PreviewProvider::requestImage(const QString &id, QSize *size, const QSize &requestedSize)
{
    const QString job = id.section(QLatin1Char('/'), 0, -2);
    QImage image;
    {
        Frames &f = frames();
        QMutexLocker lock(&f.lock);
        image = f.byJob.value(job);
    }
    if (!image.isNull() && requestedSize.isValid() && !requestedSize.isEmpty())
        image = image.scaled(requestedSize, Qt::KeepAspectRatio, Qt::SmoothTransformation);
    if (size)
        *size = image.size();
    return image;
}

} // namespace lilshim
