// The component versions (ARCH-121, APP-134): the core's from its hello, this app's and the
// protocol from Core, the Qt pair and KDDockWidgets from the app (LilShim.componentVersions: no
// protocol field carries them), ComfyUI from `engines.status` and the driver from the telemetry.
// A version that can't be read says "not known", never an empty line.
import QtQuick
import Lilith.UiKit
import "DiagnosticsText.js" as Words

LCard {
    id: root

    required property var core
    required property var shim
    // engines.status's engines.
    property var engines: []
    // The telemetry snapshot's GPU, or null.
    property var gpu: null

    readonly property var built: shim ? shim.componentVersions() : ({})
    readonly property var comfyui: engines.find(e => e.engine === "comfyui") || null

    function known(text, maxChars) {
        const shownText = Words.shown(core, text, maxChars || 80)
        return shownText !== "" ? shownText : Words.notKnown
    }

    title: "Versions"

    Column {
        width: parent.width
        spacing: 2

        LineText { text: "Core " + root.known(root.core.coreBuildId) }
        LineText { text: "This app " + root.known(root.core.buildId) }
        LineText { text: "Protocol " + root.known(root.core.coreProtocol) }
        LineText { text: "Qt " + root.known(root.built.qt) }
        LineText { text: "Qt WebEngine " + root.known(root.built.webEngine) }
        LineText { text: "KDDockWidgets " + root.known(root.built.kddockwidgets) }
        LineText { text: "ComfyUI " + root.known(root.comfyui ? root.comfyui.version : "") }
        LineText { text: "GPU driver " + root.known(root.gpu ? root.gpu.driver : "") }
    }
}
