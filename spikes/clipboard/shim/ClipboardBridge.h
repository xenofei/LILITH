#pragma once
// QML-facing side of the spike: Copy button and drag-out of the thumbnail.
#include "spike_shim_global.h"

#include <QObject>
#include <QString>

class QQuickItem;

class SPIKE_SHIM_EXPORT ClipboardBridge : public QObject
{
    Q_OBJECT
    Q_PROPERTY(QString exportPath READ exportPath NOTIFY exportPathChanged)
    Q_PROPERTY(QString status READ status NOTIFY statusChanged)
public:
    static ClipboardBridge *instance();
    // Source PNG bytes (the file embedded as a Qt resource) and export folder.
    void setSource(const QByteArray &png, const QString &exportDir);
    QString exportPath() const { return m_exportPath; }
    QString status() const { return m_status; }

    // Puts the recipe on the system clipboard.
    Q_INVOKABLE bool copyToClipboard();
    // Starts a QDrag (Qt::CopyAction only) from `item`; blocks until the drop ends.
    Q_INVOKABLE void startDrag(QQuickItem *item);
    Q_INVOKABLE bool exportExists() const;
    // Self test only: cancel the drag this many ms after it starts (no real mouse is involved).
    void setAutoCancelDragMs(int ms) { m_autoCancelMs = ms; }

signals:
    void exportPathChanged();
    void statusChanged();

private:
    ClipboardBridge() = default;
    void setStatus(const QString &s);

    QByteArray m_png;
    QString m_exportDir;
    QString m_exportPath;
    QString m_status = QStringLiteral("Ready");
    int m_autoCancelMs = -1;
};
