// APP-137: WebEngine stays out of the process until a Loader creates Lilith.Web's WebPanel, and
// the panel's profile is the suite's persistent one, under the app's local data folder (IMG-126).
// Its own process (CTest's lazy_web), so nothing before it has loaded WebEngine.
import QtQuick
import QtCore
import QtTest
import Lilith.Shim

TestCase {
    id: tc
    name: "LazyWeb"
    when: windowShown
    width: 400
    height: 300

    Component {
        id: holderComponent
        Item {
            property alias loader: loader
            width: 400
            height: 300
            Loader {
                id: loader
                anchors.fill: parent
            }
        }
    }

    function plainPath(url) {
        return decodeURIComponent(String(url).replace(/^file:\/\/\//, "")).replace(/\\/g, "/")
    }

    function test_webengine_loads_only_with_the_web_panel() {
        const dll = "Qt6WebEngineCore.dll"
        verify(!LilShim.isModuleLoaded(dll), "WebEngine isn't loaded at start")
        const holder = createTemporaryObject(holderComponent, tc)
        verify(holder, "holder created")
        wait(100)
        verify(!LilShim.isModuleLoaded(dll), "an empty Loader loads nothing")

        holder.loader.sourceComponent = Qt.createComponent("Lilith.Web", "WebPanel")
        tryVerify(() => holder.loader.status === Loader.Ready || holder.loader.status === Loader.Error,
                  30000, "the Loader finished")
        compare(holder.loader.status, Loader.Ready, "WebPanel loaded")
        verify(LilShim.isModuleLoaded(dll), "WebEngine is loaded once the panel is")

        const profile = holder.loader.item.profile
        verify(profile, "the panel has a profile")
        verify(!profile.offTheRecord, "the profile is persistent")
        compare(profile.storageName, "image")
        const dataDir = plainPath(StandardPaths.writableLocation(StandardPaths.AppLocalDataLocation)) + "/"
        const storage = plainPath(profile.persistentStoragePath)
        const cache = plainPath(profile.cachePath)
        verify(storage.toLowerCase().startsWith(dataDir.toLowerCase()),
               "storage " + storage + " is under " + dataDir)
        verify(cache.toLowerCase().startsWith(dataDir.toLowerCase()),
               "cache " + cache + " is under " + dataDir)
    }
}
