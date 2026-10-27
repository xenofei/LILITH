#pragma once
// The live-preview image provider (APP-031), `image://lilpreview/<job_id>/<frame>`: one frame per
// job, the newest, in a lock-protected map. The bridge (lil_qt's preview feed) puts each frame
// as it takes it out of its preview:<job_id> event, off the GUI thread, and drops a job's frame
// when the job ends. Every frame has its own URL, so QML loads each once, with caching off;
// whatever frame a URL names, the provider gives the job's newest (or none once it is dropped).
// Images load on Qt Quick's image loader thread, never the GUI thread.

#include "lilshim/shim_global.h"

#include <QByteArray>
#include <QQuickImageProvider>
#include <QString>

namespace lilshim {

// Decodes `bytes` (a JPEG or PNG, told by its header) on the calling thread and keeps it as
// job `job`'s frame, in place of the one before. False, keeping nothing, when it doesn't decode.
LILSHIM_EXPORT bool putPreviewFrame(const QString &job, const QByteArray &bytes);

// Drops job `job`'s frame.
LILSHIM_EXPORT void dropPreviewFrame(const QString &job);

class LILSHIM_EXPORT PreviewProvider : public QQuickImageProvider
{
public:
    PreviewProvider();
    ~PreviewProvider() override;

    // `id` is `<job_id>/<frame>`.
    QImage requestImage(const QString &id, QSize *size, const QSize &requestedSize) override;
};

} // namespace lilshim
