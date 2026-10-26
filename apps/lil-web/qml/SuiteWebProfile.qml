// The suite's WebEngine profile (IMG-126): a named, persistent profile whose storage and cache sit
// under the app's local data folder (%LOCALAPPDATA%\<channel folder>\<app>\webengine\).
// WebEngineProfilePrototype reads its properties only until it completes, so they are set here,
// in its declaration, and a view takes the profile through instance() (spike 10, S0.3; a binding
// silently gives the default off-the-record profile).
import QtQuick
import QtCore
import QtWebEngine

WebEngineProfilePrototype {
    id: proto

    // The app's local data folder as a plain path (StandardPaths gives a file: URL).
    readonly property string dataDir: {
        const url = String(StandardPaths.writableLocation(StandardPaths.AppLocalDataLocation))
        return decodeURIComponent(url.replace(/^file:\/\/\//, ""))
    }

    storageName: "image"
    persistentStoragePath: proto.dataDir + "/webengine/storage"
    cachePath: proto.dataDir + "/webengine/cache"
}
