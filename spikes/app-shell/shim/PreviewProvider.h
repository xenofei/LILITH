#pragma once
// Live previews (S0.6). PreviewHub keeps the latest JPEG of each job (pushed on the GUI thread
// from the Rust feed's frame signal) and tells QML a new frame exists; PreviewProvider is the
// `image://preview/<job>/<frame>` provider that decodes it (QML asks with asynchronous: true, so
// decoding runs on Qt Quick's image loader thread, not the GUI thread).

#include "spike_shim_global.h"

#include <QByteArray>
#include <QMutex>
#include <QObject>
#include <QQuickImageProvider>
#include <QVector>

class SPIKE_SHIM_EXPORT PreviewHub : public QObject
{
    Q_OBJECT
public:
    explicit PreviewHub(int jobs, QObject *parent = nullptr);

    QByteArray latest(int job) const;
    Q_INVOKABLE int decodedCount() const;
    void countDecode();

public Q_SLOTS:
    void push(int job, const QByteArray &jpeg, double progress);

Q_SIGNALS:
    // frame: a per-job counter, so `image://preview/<job>/<frame>` changes on every push.
    void updated(int job, int frame, double progress);

private:
    mutable QMutex m_mutex;
    QVector<QByteArray> m_latest;
    QVector<int> m_frames;
    int m_decoded = 0;
};

class SPIKE_SHIM_EXPORT PreviewProvider : public QQuickImageProvider
{
public:
    explicit PreviewProvider(PreviewHub *hub);
    QImage requestImage(const QString &id, QSize *size, const QSize &requestedSize) override;

private:
    PreviewHub *m_hub;
};
