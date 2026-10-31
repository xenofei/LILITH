// The no-workspace state (APP-016): connected, with no workspace at all. "Create or choose a
// workspace" asks for a folder, and `workspace.create {path, default: true}` makes it a workspace,
// or registers one that is already there. It becomes the default, since the point of this state is
// a usable default, though that moves every `cli` and `mcp` caller too and can replace a default
// that is only missing for now (an unplugged drive), so `noticed` says it happened. Its id then goes
// to `Core.setWorkspace`, a new hello; the banner leaves when that hello's workspace reaches
// `Core.workspace`. A refusal stays on the banner.
//
// It says so before the click. `workspaces.list` is read when the banner shows (a session with no
// workspace may). Where a default exists but can't be found, the banner says what would be replaced,
// and "Use the default workspace" picks it up again once its drive is back. With no default at all
// nothing is replaced, and it only says the folder becomes the default. If the read fails, it says
// the same in general words, naming the command line and agents. There is no confirmation: the
// folder dialog is already a deliberate step.
import QtQuick
import QtQuick.Dialogs
import Lilith.UiKit

LBanner {
    id: root

    required property var core

    readonly property bool wanted: !!core && core.connection === "ready" && !core.workspace
    property alias createButton: createButton
    property alias defaultButton: defaultButton
    property var _call: null
    property string _refusal
    // The default row that can't be found, or null; "ok" or "failed" once the list is read.
    property var _lostDefault: null
    property string _listRead

    // Something happened that outlives the banner: (text, kind). workspace.create's warnings (a
    // network or cloud-synced folder, a volume without stable ids) are in the text.
    signal noticed(string text, string kind)

    closable: false
    kind: _refusal !== "" ? "danger" : "warning"
    visible: wanted
    text: {
        if (_refusal !== "")
            return _refusal
        const open = "No workspace is open. You can still generate; that is logged as loose usage."
        if (_lostDefault) {
            const name = core.shownText(_lostDefault.name || folderName(_lostDefault.path), 60)
            return "Your default workspace, " + name + " (" + core.shownText(String(_lostDefault.path), 120)
                   + "), can't be found. If its drive is unplugged, plug it in and use it again. Choosing another"
                   + " folder makes that one the default instead, for the command line and agents too."
        }
        if (_listRead === "ok")
            return open + " The folder you choose becomes your default workspace."
        if (_listRead === "failed")
            return open + " The folder you choose becomes your default workspace, for the command line and agents too."
        return open
    }

    onWantedChanged: {
        _refusal = ""
        _lostDefault = null
        _listRead = ""
        if (wanted) {
            announce()
            readDefault()
        }
    }

    Component.onCompleted: {
        if (wanted)
            readDefault()
    }

    function readDefault() {
        CoreCall.call(core, "workspaces.list", {}, {
            owner: root,
            ok: r => {
                const rows = (r.structured || {}).workspaces || []
                const lost = rows.find(w => w.default && (w.missing || w.problem))
                root._lostDefault = lost || null
                root._listRead = "ok"
                if (lost)
                    root.announce()
            },
            error: () => { root._listRead = "failed" }
        })
    }

    function useDefault() {
        if (_call)
            return
        _refusal = ""
        _call = CoreCall.useDefaultWorkspace(core, {
            owner: root,
            ok: hello => {
                root._call = null
                if (!hello.workspace) {
                    root._refusal = "Your default workspace still can't be found."
                    root.announce()
                }
            },
            error: e => {
                root._call = null
                root._refusal = "Couldn't use the default workspace: "
                                + root.core.shownText(e && e.message ? String(e.message) : "the core didn't answer.", 300)
                root.announce()
            }
        })
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

    function folderName(path) {
        const parts = String(path || "").split(/[\\/]/).filter(p => p !== "")
        return parts.length > 0 ? parts[parts.length - 1] : String(path || "")
    }

    function folderChosen(url) {
        if (_call) {
            noticed("Still setting up the last folder. Try this one again when that's done.", "warning")
            return
        }
        _refusal = ""
        _call = CoreCall.call(core, "workspace.create", {path: localPath(url), default: true}, {
            owner: root,
            ok: r => {
                const made = r.structured || {}
                const warnings = (made.warnings || []).map(w => root.core.shownText(String(w), 300))
                const name = root.core.shownText(root.folderName(made.path), 60)
                const words = made.default ? [name + " is now your default workspace."] : []
                if (words.length + warnings.length > 0)
                    root.noticed(words.concat(warnings).join(" "), warnings.length > 0 ? "warning" : "ok")
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
            requiresCore: true
            primary: true
            enabled: root.wanted
            text: "Create or choose a workspace"
            onClicked: root.chooseFolder()
        },
        LButton {
            id: defaultButton
            requiresCore: true
            visible: !!root._lostDefault
            text: "Use the default workspace"
            onClicked: root.useDefault()
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
