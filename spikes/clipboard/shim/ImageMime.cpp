#include "ImageMime.h"

#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QImage>
#include <QMimeData>
#include <QUrl>

QString ImageMime::writeExport(const Options &o)
{
    QDir().mkpath(o.exportDir);
    const QString path = QDir(o.exportDir).absoluteFilePath(o.exportFileName);
    QFile f(path);
    if (f.open(QIODevice::ReadOnly) && f.readAll() == o.pngBytes) // already exported, unchanged
        return QDir::toNativeSeparators(path);
    f.close();
    if (!f.open(QIODevice::WriteOnly | QIODevice::Truncate) || f.write(o.pngBytes) != o.pngBytes.size())
        return {};
    return QDir::toNativeSeparators(path);
}

QMimeData *ImageMime::build(const Options &o, QString *exportedPath)
{
    const QString exported = writeExport(o);
    if (exportedPath)
        *exportedPath = exported;

    auto *mime = new QMimeData;
    // 1. Custom format: Qt registers a Windows clipboard format with this very name.
    mime->setData(QLatin1String(kRefFormat), o.refJson.toUtf8());
    // 2. Standard bitmap: Qt serves CF_DIBV5 / CF_DIB (Windows synthesizes CF_BITMAP).
    const QImage image = QImage::fromData(o.pngBytes, "PNG");
    if (image.isNull())
        qWarning("ImageMime: PNG bytes did not decode; the bitmap formats (CF_DIB/CF_DIBV5) are skipped");
    else
        mime->setImageData(image);
    // 3. The exact PNG bytes under the Windows registered format "PNG".
    mime->setData(QStringLiteral("application/x-qt-windows-mime;value=\"PNG\""), o.pngBytes);
    // 4. File URL of the exported copy: Qt turns it into CF_HDROP (Explorer drop, file paste) plus
    //    UniformResourceLocator(W). It does not produce FileNameW.
    if (!exported.isEmpty())
        mime->setUrls({QUrl::fromLocalFile(exported)});
    // 5. Deliberately no setText() / setHtml(): no app may fall back to a path or text.
    return mime;
}
