#include "PreviewProvider.h"

#include <QImage>
#include <QMutexLocker>

PreviewHub::PreviewHub(int jobs, QObject *parent)
    : QObject(parent), m_latest(jobs), m_frames(jobs, 0)
{
}

QByteArray PreviewHub::latest(int job) const
{
    QMutexLocker lock(&m_mutex);
    return job >= 0 && job < m_latest.size() ? m_latest[job] : QByteArray();
}

int PreviewHub::decodedCount() const
{
    QMutexLocker lock(&m_mutex);
    return m_decoded;
}

void PreviewHub::countDecode()
{
    QMutexLocker lock(&m_mutex);
    ++m_decoded;
}

void PreviewHub::push(int job, const QByteArray &jpeg, double progress)
{
    int frame;
    {
        QMutexLocker lock(&m_mutex);
        if (job < 0 || job >= m_latest.size())
            return;
        m_latest[job] = jpeg;
        frame = ++m_frames[job];
    }
    emit updated(job, frame, progress);
}

PreviewProvider::PreviewProvider(PreviewHub *hub)
    : QQuickImageProvider(QQuickImageProvider::Image), m_hub(hub)
{
}

QImage PreviewProvider::requestImage(const QString &id, QSize *size, const QSize &requestedSize)
{
    // id: "<job>/<frame>"
    const int job = id.section(QLatin1Char('/'), 0, 0).toInt();
    QImage img = QImage::fromData(m_hub->latest(job), "JPG");
    if (img.isNull()) {
        img = QImage(64, 64, QImage::Format_RGB32);
        img.fill(Qt::black);
    } else {
        m_hub->countDecode();
    }
    if (requestedSize.isValid() && !requestedSize.isEmpty())
        img = img.scaled(requestedSize, Qt::KeepAspectRatio, Qt::SmoothTransformation);
    if (size)
        *size = img.size();
    return img;
}
