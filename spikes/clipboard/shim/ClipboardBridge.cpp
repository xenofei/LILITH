#include "ClipboardBridge.h"

#include "ImageMime.h"
#include "SpikeLog.h"

#include <QClipboard>
#include <QCryptographicHash>
#include <QDrag>
#include <QFile>
#include <QGuiApplication>
#include <QImage>
#include <QJsonArray>
#include <QMimeData>
#include <QPixmap>
#include <atomic>
#include <chrono>
#include <thread>
#include <QQuickItem>
#include <QQuickWindow>

#ifdef Q_OS_WIN
#  ifndef NOMINMAX
#    define NOMINMAX
#  endif
#  ifndef WIN32_LEAN_AND_MEAN
#    define WIN32_LEAN_AND_MEAN
#  endif
#  include <windows.h>
#endif

namespace {
const char *actionName(Qt::DropAction a)
{
    switch (a) {
    case Qt::CopyAction: return "copy";
    case Qt::MoveAction: return "move";
    case Qt::LinkAction: return "link";
    case Qt::IgnoreAction: return "ignore";
    default: return "other";
    }
}
QJsonArray actionsToJson(Qt::DropActions as)
{
    QJsonArray a;
    for (Qt::DropAction x : {Qt::CopyAction, Qt::MoveAction, Qt::LinkAction})
        if (as.testFlag(x))
            a.append(QLatin1String(actionName(x)));
    return a;
}
ImageMime::Options makeOptions(const QByteArray &png, const QString &dir)
{
    ImageMime::Options o;
    o.pngBytes = png;
    o.exportDir = dir;
    o.exportFileName = QStringLiteral("worldwright-spike-image.png");
    const QString sha = QString::fromLatin1(QCryptographicHash::hash(png, QCryptographicHash::Sha256).toHex());
    o.refJson = QStringLiteral("{\"kind\":\"image\",\"id\":\"spike-0001\",\"sha256\":\"%1\"}").arg(sha);
    return o;
}
} // namespace

ClipboardBridge *ClipboardBridge::instance()
{
    static ClipboardBridge s;
    return &s;
}

void ClipboardBridge::setSource(const QByteArray &png, const QString &exportDir)
{
    m_png = png;
    m_exportDir = exportDir;
}

void ClipboardBridge::setStatus(const QString &s)
{
    m_status = s;
    emit statusChanged();
}

bool ClipboardBridge::copyToClipboard()
{
    QString exported;
    QMimeData *mime = ImageMime::build(makeOptions(m_png, m_exportDir), &exported);
    m_exportPath = exported;
    emit exportPathChanged();
    const QStringList formats = mime->formats();
    QGuiApplication::clipboard()->setMimeData(mime); // takes ownership
    SpikeLog::write(QStringLiteral("copied"),
                    {{"export_path", exported},
                     {"png_bytes", m_png.size()},
                     {"qt_formats", QJsonArray::fromStringList(formats)}});
    setStatus(QStringLiteral("Copied. Now paste somewhere."));
    return !exported.isEmpty();
}

bool ClipboardBridge::exportExists() const
{
    return !m_exportPath.isEmpty() && QFile::exists(m_exportPath);
}

void ClipboardBridge::startDrag(QQuickItem *item)
{
    QString exported;
    QMimeData *mime = ImageMime::build(makeOptions(m_png, m_exportDir), &exported);
    m_exportPath = exported;
    emit exportPathChanged();

    QObject *parent = (item && item->window()) ? static_cast<QObject *>(item->window()) : static_cast<QObject *>(this);
    auto *drag = new QDrag(parent);
    drag->setMimeData(mime); // QDrag owns it
    const QImage img = QImage::fromData(m_png, "PNG");
    drag->setPixmap(QPixmap::fromImage(img.scaled(96, 96, Qt::KeepAspectRatio, Qt::SmoothTransformation)));
    SpikeLog::write(QStringLiteral("drag_start"),
                    {{"formats", QJsonArray::fromStringList(mime->formats())},
                     {"export_path", exported},
                     {"export_exists_before", QFile::exists(exported)}});
    // Copy only: with supportedActions == Copy the OLE effect mask is DROPEFFECT_COPY, so Explorer
    // can neither offer nor perform a move, and the exported file is never removed by the target.
    // Self test only. DoDragDrop runs its own loop that does not dispatch Qt timers, and with an
    // idle mouse nothing ever makes it ask "continue?" (it would block forever). A helper thread
    // therefore posts a WM_MOUSEMOVE to the GUI thread's active window (an in-process message, no
    // desktop input); OLE then sees no button down and ends the drag as a drop with no target.
    std::atomic<bool> stopHelper{false};
    std::thread helper;
#ifdef Q_OS_WIN
    if (m_autoCancelMs >= 0) {
        const DWORD guiThread = GetCurrentThreadId();
        const int interval = m_autoCancelMs;
        helper = std::thread([&stopHelper, guiThread, interval] {
            while (!stopHelper) {
                std::this_thread::sleep_for(std::chrono::milliseconds(interval));
                GUITHREADINFO gi{};
                gi.cbSize = sizeof(gi);
                const bool ok = GetGUIThreadInfo(guiThread, &gi);
                const HWND target = gi.hwndCapture ? gi.hwndCapture : gi.hwndActive;
                if (!stopHelper && ok && target)
                    PostMessageW(target, WM_MOUSEMOVE, 0, MAKELPARAM(40, 40));
            }
        });
    }
#endif
    const Qt::DropAction result = drag->exec(Qt::CopyAction, Qt::CopyAction);
    stopHelper = true;
    if (helper.joinable())
        helper.join();
    SpikeLog::write(QStringLiteral("drag_end"),
                    {{"result", QLatin1String(actionName(result))},
                     {"supported_actions", actionsToJson(drag->supportedActions())},
                     {"default_action", QLatin1String(actionName(drag->defaultAction()))},
                     {"export_exists_after", QFile::exists(exported)}});
    setStatus(result == Qt::CopyAction ? QStringLiteral("Dropped (copy)")
                                       : QStringLiteral("Drag ended (%1)").arg(QLatin1String(actionName(result))));
}
