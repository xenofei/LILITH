// The no-workspace state (APP-016): connected, with no workspace at all. "Create or choose a
// workspace" asks for a folder, and `workspace.create {path}` makes it a workspace, or registers
// one that is already there. Its id then goes to `Core.setWorkspace`, a new hello; the banner leaves
// when that hello's workspace reaches `Core.workspace`. A refusal stays on the banner.
import QtQuick
import QtQuick.Dialogs
import Lilith.UiKit

LBanner {
    id: root

    required property var core

    readonly property bool wanted: !!core && core.connection === "ready" && !core.workspace
    property alias createButton: createButton
    property var _call: null
    property string _refusal

    // workspace.create's warnings (a network or cloud-synced folder, a volume without stable ids).
    signal warned(string text)

    closable: false
    kind: _refusal !== "" ? "danger" : "warning"
    visible: wanted
    text: _refusal !== "" ? _refusal
        : "No workspace is open. You can still generate; that is logged as loose usage."

    onWantedChanged: {
        _refusal = ""
        if (wanted)
            announce()
    }

    function refused(error) {
        _call = null
        _refusal = "That folder couldn't be used: "
                   + core.shownText(error && error.message ? String(error.message) : "the core didn't answer.", 300)
        announce()
    }

    function chooseFolder() {
        if (dialogLoader.active)
            dialogLoader.item.open()
        else
            dialogLoader.active = true
    }

    // A file: URL as a path: "file:///D:/art" is "D:/art", "file://server/share" is "//server/share".
    function localPath(url) {
        const s = String(url)
        if (s.startsWith("file:///"))
            return decodeURIComponent(s.slice(8))
        if (s.startsWith("file://"))
            return "//" + decodeURIComponent(s.slice(7))
        return s
    }

    function folderChosen(url) {
        if (_call)
            return
        _refusal = ""
        _call = CoreCall.call(core, "workspace.create", {path: localPath(url)}, {
            owner: root,
            ok: r => {
                const made = r.structured || {}
                for (const w of made.warnings || [])
                    root.warned(root.core.shownText(String(w), 300))
                root._call = CoreCall.setWorkspace(root.core, String(made.id), {
                    owner: root,
                    ok: () => { root._call = null },
                    error: e => root.refused(e)
                })
            },
            error: e => root.refused(e)
        })
    }

    actions: [
        LButton {
            id: createButton
            property bool requiresCore: true
            primary: true
            enabled: root.wanted
            text: "Create or choose a workspace"
            onClicked: root.chooseFolder()
        }
    ]

    Loader {
        id: dialogLoader
        active: false
        sourceComponent: FolderDialog {
            title: "Create or choose a workspace"
            onAccepted: root.folderChosen(selectedFolder)
        }
        onLoaded: item.open()
    }
}
