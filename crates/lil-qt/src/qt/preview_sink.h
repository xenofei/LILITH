#pragma once
// Where the bridge's preview feed (src/qt/preview_feed.rs) hands each frame it takes out of a
// preview:<job_id> event (APP-031): the shim's image provider, which lil-image's main.cpp (and
// the QML tests' runner) names with setPreviewSink before the core session starts. Without a
// sink a frame goes nowhere, and its event's url stays null. Called from the session's threads.

#include <QtCore/QByteArray>
#include <QtCore/QString>

#include <cstdint>

#include "rust/cxx.h"

namespace lil_qt {

// lilshim::putPreviewFrame and lilshim::dropPreviewFrame.
using PreviewPut = bool (*)(const QString &job, const QByteArray &bytes);
using PreviewDrop = void (*)(const QString &job);

void setPreviewSink(PreviewPut put, PreviewDrop drop);

// The feed's side: false when there is no sink or the frame doesn't decode.
bool previewPut(rust::Str job, rust::Slice<const std::uint8_t> bytes);
void previewDrop(rust::Str job);

} // namespace lil_qt
