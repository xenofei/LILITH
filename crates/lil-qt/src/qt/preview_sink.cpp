#include "lil_qt/src/qt/preview_sink.h"

#include <atomic>

namespace lil_qt {

namespace {

std::atomic<PreviewPut> s_put{nullptr};
std::atomic<PreviewDrop> s_drop{nullptr};

QString toQString(rust::Str s)
{
    return QString::fromUtf8(s.data(), qsizetype(s.size()));
}

} // namespace

void setPreviewSink(PreviewPut put, PreviewDrop drop)
{
    s_put = put;
    s_drop = drop;
}

bool previewPut(rust::Str job, rust::Slice<const std::uint8_t> bytes)
{
    const PreviewPut put = s_put;
    if (!put)
        return false;
    // The sink decodes the bytes before it returns, so they are lent, not copied.
    return put(toQString(job), QByteArray::fromRawData(reinterpret_cast<const char *>(bytes.data()), qsizetype(bytes.size())));
}

void previewDrop(rust::Str job)
{
    if (const PreviewDrop drop = s_drop)
        drop(toQString(job));
}

} // namespace lil_qt
