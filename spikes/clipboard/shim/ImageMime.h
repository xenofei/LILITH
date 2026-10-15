#pragma once
// ImageMime: the QMimeData recipe for "copy / drag out an image" (APP-071, APP-068, APP-069).
// Pure Qt, no spike-specific logic, so plan 4's shim can lift it as is.
#include "spike_shim_global.h"

#include <QByteArray>
#include <QString>

class QMimeData;

class SPIKE_SHIM_EXPORT ImageMime
{
public:
    static constexpr const char *kRefFormat = "application/x-worldwright-ref";

    struct Options {
        QByteArray pngBytes;        // the exact PNG file bytes; put on the clipboard unchanged
        QString refJson;            // payload of application/x-worldwright-ref (UTF-8 JSON)
        QString exportDir;          // folder for the exported copy (created if missing)
        QString exportFileName;     // file name of the exported copy
    };

    // Writes the exported copy (byte-identical to pngBytes) and returns its absolute path, or an
    // empty string on failure.
    static QString writeExport(const Options &o);

    // Builds the mime data. Ownership passes to the caller (QClipboard / QDrag take it).
    // Formats: custom ref, standard bitmap (setImageData), exact PNG bytes under the Windows
    // registered format "PNG", file URL of the exported copy. Never text/plain.
    static QMimeData *build(const Options &o, QString *exportedPath = nullptr);
};
