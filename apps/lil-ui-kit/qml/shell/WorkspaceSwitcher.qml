// The workspace switcher (APP-006, ARCH-030, ARCH-090, P4a-R34): shows the session's workspace and
// lists `workspaces.list` each time it opens.
//
// Choosing a served row is `Core.setWorkspace(id)`, a new hello, and never `workspace.set_default`:
// that would change every `cli` and `mcp` caller's workspace. A row the core doesn't serve, whose
// folder is gone or that has a problem is unavailable and says why. A refusal
// (`workspace.unsupported`) changes nothing shown but the message; `Core.workspace`, which the hello
// sets, says what is current.
//
// "Make this the default" is its own click, on its own button, and it moves the app too: in M1 the
// core serves only the default, so from that answer on it refuses every call of the old session
// (the list's re-read included) until a new hello. So the answer's id goes straight to
// `Core.setWorkspace`, and `noticed` says what happened. If that hello fails, the rows say what
// the core now serves, so the new default can be chosen by hand.
//
// While the core refuses the session's workspace, nothing can be listed, but the button stays usable
// and offers the way out. That is a refused hello (`lil workspace use` elsewhere, then a lost
// connection, a renamed folder), or `movedReason` while connected (the top bar's: another client
// moved the default, so every call is refused). A list read refused with `workspace.unsupported`
// tells it through `workspaceRefused`.
import QtQuick
import Lilith.UiKit
import "../core/CoreGate.js" as CoreGate

Item {
    id: root

    required property var core
    // The core is ready.
    required property bool ready
    // Why the core refuses this session while connected, shown text; empty when it doesn't.
    property string movedReason

    readonly property bool refused: CoreGate.refusedWorkspace(core) || movedReason !== ""
    readonly property string refusedReason: movedReason !== "" ? movedReason
                                            : refused ? shown(core.unavailable.reason, 300) : ""
    readonly property string currentId: core && core.workspace ? String(core.workspace.id) : ""
    readonly property string currentName: {
        if (!core || !core.workspace)
            return "No workspace"
        const listed = workspaces.find(w => w.id === currentId)
        return shown(listed && listed.name ? listed.name : folderName(core.workspace.path), 60)
    }
    property alias button: button
    property alias popup: popup
    // workspaces.list's rows.
    property var workspaces: []
    property var _listCall: null
    property var _switchCall: null
    property var _defaultCall: null

    // Something happened that outlives the list: (text, kind).
    signal noticed(string text, string kind)
    // The core refused this session's workspace while connected, in its words.
    signal workspaceRefused(string message)

    // Its own click is moving the default, and the hello that follows is on the way.
    function changingDefault() {
        return !!_defaultCall || !!_switchCall
    }

    implicitWidth: Math.min(button.implicitWidth, 220)
    implicitHeight: button.implicitHeight

    onReadyChanged: {
        if (!ready && !refused)
            popup.close()
    }
    // A list left open as the refusal ends has nothing in it, and would stay empty.
    onRefusedChanged: {
        if (!refused)
            popup.close()
    }

    function shown(text, max) {
        return core ? core.shownText(String(text), max) : ""
    }

    function folderName(path) {
        const parts = String(path || "").split(/[\\/]/).filter(p => p !== "")
        return parts.length > 0 ? parts[parts.length - 1] : String(path || "")
    }

    // Why a row can't be chosen, or "".
    function unavailableReason(w) {
        if (w.missing)
            return "The folder is gone"
        if (w.problem === "not_a_folder")
            return "It isn't a folder"
        if (w.problem === "unreadable")
            return "It can't be read"
        if (w.problem)
            return shown(w.problem, 40)
        if (!w.served)
            return "This core serves only the default workspace"
        return ""
    }

    // The folder is there, so it can be the default.
    function canBeDefault(w) {
        return !w.default && !w.missing && !w.problem
    }

    function refusedText(what, error) {
        return what + ": " + shown(error && error.message ? error.message : "the core didn't answer.", 300)
    }

    function open() {
        popup.message = ""
        if (refused)
            popup.message = refusedReason
        else if (ready)
            read()
        else
            return
        popup.open()
    }

    function read() {
        if (_listCall)
            _listCall.cancel()
        const asked = currentId
        _listCall = CoreCall.call(core, "workspaces.list", {}, {
            owner: root,
            ok: r => {
                root._listCall = null
                const rows = (r.structured || {}).workspaces || []
                root.workspaces = rows
                popup.view.currentIndex = rows.findIndex(w => w.id === root.currentId)
            },
            error: e => {
                root._listCall = null
                if (e && e.code === "workspace.unsupported" && asked === root.currentId) {
                    root.workspaceRefused(String(e.message))
                    popup.message = root.refusedReason
                    return
                }
                popup.message = root.refusedText("Couldn't list the workspaces", e)
            }
        })
    }

    function choose(w) {
        if (!ready || _switchCall || unavailableReason(w) !== "")
            return
        if (w.id === currentId) {
            popup.close()
            return
        }
        popup.message = ""
        _switchCall = CoreCall.setWorkspace(core, w.id, {
            owner: root,
            ok: () => {
                root._switchCall = null
                popup.close()
            },
            error: e => {
                root._switchCall = null
                popup.message = root.refusedText("Couldn't switch the workspace", e)
            }
        })
    }

    function makeDefault(w) {
        if (!ready || _defaultCall || _switchCall)
            return
        popup.message = ""
        const name = shown(w.name || folderName(w.path), 60)
        _defaultCall = CoreCall.call(core, "workspace.set_default", {path: w.path}, {
            owner: root,
            ok: r => {
                root._defaultCall = null
                root.switchToDefault(String((r.structured || {}).id), name)
            },
            error: e => {
                root._defaultCall = null
                popup.message = root.refusedText("Couldn't make it the default", e)
            }
        })
    }

    function switchToDefault(id, name) {
        // The core serves only this one from now on, whatever the hello below does.
        workspaces = workspaces.map(w => Object.assign({}, w, {default: w.id === id, served: w.id === id}))
        _switchCall = CoreCall.setWorkspace(core, id, {
            owner: root,
            ok: () => {
                root._switchCall = null
                root.noticed(name + " is now your default workspace, and the app has switched to it.", "ok")
                popup.close()
            },
            error: e => {
                root._switchCall = null
                const text = name + " is now your default workspace, but the app couldn't switch to it: "
                             + root.shown(e && e.message ? e.message : "the core didn't answer.", 300)
                popup.message = text
                root.noticed(text, "danger")
            }
        })
    }

    function useDefault() {
        CoreCall.useDefaultWorkspace(core, {
            owner: root,
            ok: () => popup.close(),
            error: e => { popup.message = root.refusedText("Couldn't use the default workspace", e) }
        })
    }

    LButton {
        id: button
        requiresCore: !root.refused
        width: root.width
        height: 28
        enabled: root.ready || root.refused
        text: root.currentName + " ▾"
        Accessible.name: "Workspace: " + root.currentName
        onClicked: popup.visible ? popup.close() : root.open()
    }

    SwitcherPopup {
        id: popup
        listName: "Workspaces"
        rows: root.workspaces
        offerDefault: root.refused
        onUseDefault: root.useDefault()
        onActivated: index => {
            if (index >= 0 && index < root.workspaces.length)
                root.choose(root.workspaces[index])
        }
        rowDelegate: Rectangle {
            id: row
            required property var modelData
            required property int index
            readonly property string name: root.shown(modelData.name || root.folderName(modelData.path), 60)
            readonly property string reason: root.unavailableReason(modelData)
            readonly property bool isCurrent: modelData.id === root.currentId
            readonly property string stateWords: {
                const out = []
                if (isCurrent)
                    out.push("current")
                if (modelData.default)
                    out.push("default")
                if (reason !== "")
                    out.push("unavailable")
                return out.join(", ")
            }
            width: ListView.view.width
            height: Math.max(rowText.implicitHeight, defaultButton.visible ? defaultButton.height : 0) + 8
            radius: Theme.radius
            color: ListView.isCurrentItem ? Theme.bg : "transparent"
            Accessible.role: Accessible.ListItem
            Accessible.name: name + (stateWords !== "" ? ", " + stateWords : "") + (reason !== "" ? ": " + reason : "")
            Accessible.onPressAction: root.choose(modelData)

            MouseArea {
                anchors.fill: parent
                onClicked: {
                    row.ListView.view.currentIndex = row.index
                    root.choose(row.modelData)
                }
            }

            Column {
                id: rowText
                x: Theme.spacing
                y: 4
                width: parent.width - 2 * Theme.spacing
                       - (defaultButton.visible ? defaultButton.width + Theme.spacing : 0)

                Text {
                    width: parent.width
                    text: row.name + (row.stateWords !== "" ? " (" + row.stateWords + ")" : "")
                    textFormat: Text.PlainText
                    elide: Text.ElideRight
                    color: row.reason !== "" ? Theme.textMuted : Theme.text
                    font.pixelSize: Theme.fontSize
                }
                Text {
                    width: parent.width
                    text: row.reason !== "" ? row.reason : root.shown(row.modelData.path, 80)
                    textFormat: Text.PlainText
                    // A reason is read in full; a long path keeps both its ends.
                    wrapMode: row.reason !== "" ? Text.Wrap : Text.NoWrap
                    elide: row.reason !== "" ? Text.ElideNone : Text.ElideMiddle
                    color: row.reason !== "" ? Theme.warning : Theme.textMuted
                    font.pixelSize: Theme.fontSize - 1
                }
            }

            LButton {
                id: defaultButton
                requiresCore: true
                core: root.core
                anchors.right: parent.right
                anchors.rightMargin: Theme.spacing
                anchors.verticalCenter: parent.verticalCenter
                visible: root.canBeDefault(row.modelData)
                text: "Make this the default"
                Accessible.name: "Make " + row.name + " the default"
                onClicked: root.makeDefault(row.modelData)
            }
        }
    }
}
