// The only app QML file that imports SpikeWeb (APP-137). Main.qml's Loader creates it the first
// time the Web dock is shown, so the SpikeWeb plugin and the WebEngine DLLs stay out of the
// process until then. The profile's storage and cache go under %LOCALAPPDATA%\<org>\<app>\.
import QtQuick
import SpikeWeb
import SpikeShim

SpikeWebView {
    storageName: "image"
    storagePath: SpikeLog.localDataDir() + "/webengine/storage"
    cachePath: SpikeLog.localDataDir() + "/webengine/cache"
    onLogged: (event, fields) => SpikeLog.log(event, fields)
}
