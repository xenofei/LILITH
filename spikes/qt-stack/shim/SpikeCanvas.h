#pragma once
// SpikeCanvas: a QQuickRhiItem whose renderer clears to a colour, runs one compute dispatch that
// fills a 256x256 RGBA16F storage texture, and samples that texture into the item.
// Every renderer construction, initialize() call and the QRhi backend name are logged (SpikeLog).

#include "spike_shim_global.h"

#include <QColor>
#include <QQuickRhiItem>

class SPIKE_SHIM_EXPORT SpikeCanvas : public QQuickRhiItem
{
    Q_OBJECT
    Q_PROPERTY(QColor clearColor READ clearColor WRITE setClearColor NOTIFY clearColorChanged)
    Q_PROPERTY(int initializeCount READ initializeCount NOTIFY rendererInitialized)
    Q_PROPERTY(QString backend READ backend NOTIFY rendererInitialized)

public:
    explicit SpikeCanvas(QQuickItem *parent = nullptr);

    QColor clearColor() const { return m_clearColor; }
    void setClearColor(const QColor &c);
    int initializeCount() const { return m_initializeCount; }
    QString backend() const { return m_backend; }

    // Called by the renderer during synchronize() (GUI thread blocked); emits on the GUI thread.
    void reportInitialized(int processWideCount, const QString &backend);

Q_SIGNALS:
    void clearColorChanged();
    // Emitted on the GUI thread after the renderer's initialize() ran (first time, after a resize,
    // or after the renderer was recreated, e.g. when the item moved to another window).
    void rendererInitialized(int count, const QString &backend);

protected:
    QQuickRhiItemRenderer *createRenderer() override;

private:
    QColor m_clearColor = QColor(0x1e, 0x3a, 0x5f);
    int m_initializeCount = 0;
    QString m_backend;
};
