// Loaded by tst_shell.qml's Loader: the SpikeWeb plugin, with its own storage folder.
import QtQuick
import SpikeWeb
import SpikeShim

SpikeWebView {
    id: web
    signal loaded(string status)
    storageName: "image-test"
    storagePath: SpikeLog.localDataDir() + "/webengine/storage"
    cachePath: SpikeLog.localDataDir() + "/webengine/cache"
    webUrl: "about:blank"
    onLogged: (event, fields) => {
        if (event === "web_load" && (fields.status === "succeeded" || fields.status === "failed"))
            web.loaded(fields.status)
    }
}
