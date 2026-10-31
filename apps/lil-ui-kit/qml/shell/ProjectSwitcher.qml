// The project switcher (APP-006, ARCH-088, ARCH-098). Two values, kept apart:
//
// - **`Core.project`** is what you chose: `app/set_context {project}`'s answer (noteProject). The
//   core keeps a session's project per connection, so after a reconnect it is sent again. One the
//   core no longer knows is dropped, and another workspace forgets it. Each resend also rewrites the
//   workspace's hint, so a reconnect can undo a choice a CLI made since.
// - **`effectiveProject`** is what the bar shows: the answer's `effective_project`, the project a
//   call naming none would really use (the session's, else the hint, else `sketchbook`). It is never
//   sent back, so showing the fallback never pins it or rewrites the hint. It is read with
//   `app/set_context {}` on every `ready`, after a workspace change, after a failed resend or switch,
//   and when the list opens. It is true when read: another client can move the hint, and a call
//   naming a project moves the session's, with no event for either.
//
// With no workspace there is no project (APP-016): "No project". The list is `projects.list`, read
// each time it opens. While the core refuses the workspace, the button stays usable and offers the
// way out (SwitcherPopup).
import QtQuick
import Lilith.Core
import Lilith.UiKit
import "../core/CoreGate.js" as CoreGate

Item {
    id: root

    required property var core
    // The core is ready and has a workspace: a project is set within one (`workspace.required`).
    required property bool ready

    readonly property bool refused: CoreGate.refusedWorkspace(core)
    readonly property bool hasWorkspace: !!core && !!core.workspace && !refused
    // See the header; empty until it is read.
    property string effectiveProject
    // The slug the rows and the label go by.
    readonly property string current: effectiveProject !== "" ? effectiveProject : core && core.project ? core.project : ""
    readonly property string currentName: {
        if (!hasWorkspace)
            return "No project"
        return current === "" ? "—" : shown(_names[current] || current, 60)
    }
    property alias button: button
    property alias popup: popup
    // projects.list's rows.
    property var projects: []
    // slug -> name, from the last list. No prototype: `constructor` is a legal slug.
    property var _names: Object.create(null)
    property var _listCall: null
    property var _setCall: null
    property var _readCall: null
    property string _workspaceId

    implicitWidth: Math.min(button.implicitWidth, 200)
    implicitHeight: button.implicitHeight

    onReadyChanged: {
        if (!ready && !refused)
            popup.close()
    }

    function shown(text, max) {
        return core ? core.shownText(String(text), max) : ""
    }

    function refusedText(what, error) {
        return what + ": " + shown(error && error.message ? error.message : "the core didn't answer.", 300)
    }

    function open() {
        popup.message = ""
        if (refused) {
            popup.message = shown(core.unavailable.reason, 300)
        } else if (ready) {
            read()
            readEffective()
        } else {
            return
        }
        popup.open()
    }

    function read() {
        if (_listCall)
            _listCall.cancel()
        _listCall = CoreCall.call(core, "projects.list", {}, {
            owner: root,
            ok: r => {
                root._listCall = null
                const rows = (r.structured || {}).projects || []
                const names = Object.create(null)
                for (const p of rows)
                    names[p.project] = p.name
                root._names = names
                root.projects = rows
                popup.view.currentIndex = rows.findIndex(p => p.project === root.current)
            },
            error: e => {
                root._listCall = null
                popup.message = root.refusedText("Couldn't list the projects", e)
            }
        })
    }

    function readEffective() {
        if (_readCall)
            _readCall.cancel()
        _readCall = CoreCall.appCall(core, "app/set_context", {}, {
            owner: root,
            ok: r => {
                root._readCall = null
                root.effectiveProject = r.effective_project || ""
            },
            error: () => { root._readCall = null }
        })
    }

    function choose(slug) {
        if (!ready || _setCall)
            return
        popup.message = ""
        if (_readCall)
            _readCall.cancel()
        _readCall = null
        _setCall = CoreCall.appCall(core, "app/set_context", {project: slug}, {
            owner: root,
            ok: r => {
                root._setCall = null
                root.core.noteProject(r.project || "")
                root.effectiveProject = r.effective_project || ""
                popup.close()
            },
            error: e => {
                root._setCall = null
                popup.message = root.refusedText("Couldn't switch the project", e)
                root.readEffective()
            },
            pending: () => { root._setCall = null }
        })
    }

    function useDefault() {
        CoreCall.useDefaultWorkspace(core, {
            owner: root,
            ok: () => popup.close(),
            error: e => { popup.message = root.refusedText("Couldn't use the default workspace", e) }
        })
    }

    // A new connection has no project of its own yet.
    function sendAgain() {
        const slug = core.project
        CoreCall.appCall(core, "app/set_context", {project: slug}, {
            owner: root,
            ok: r => {
                root.core.noteProject(r.project || "")
                root.effectiveProject = r.effective_project || ""
            },
            error: e => {
                if (e && e.code === "project.unknown" && root.core.project === slug)
                    root.core.noteProject("")
                root.readEffective()
            }
        })
    }

    function workspaceId() {
        return core && core.workspace ? String(core.workspace.id) : ""
    }

    Component.onCompleted: {
        _workspaceId = workspaceId()
        if (ready)
            readEffective()
    }

    Connections {
        target: root.core
        ignoreUnknownSignals: true
        function onConnectionChanged() {
            if (root.core.connection !== "ready" || !root.core.workspace)
                return
            if (root.core.project)
                root.sendAgain()
            else
                root.readEffective()
        }
        function onWorkspaceChanged() {
            const id = root.workspaceId()
            if (id === root._workspaceId)
                return
            root._workspaceId = id
            root.core.noteProject("")
            root.effectiveProject = ""
            if (root.core.connection === "ready" && id !== "")
                root.readEffective()
        }
    }

    LButton {
        id: button
        requiresCore: !root.refused
        width: root.width
        height: 28
        enabled: root.ready || root.refused
        text: root.currentName + " ▾"
        Accessible.name: "Project: " + root.currentName
        onClicked: popup.visible ? popup.close() : root.open()
    }

    SwitcherPopup {
        id: popup
        listName: "Projects"
        rows: root.projects
        offerDefault: root.refused
        onUseDefault: root.useDefault()
        onActivated: index => {
            if (index >= 0 && index < root.projects.length)
                root.choose(root.projects[index].project)
        }
        rowDelegate: Rectangle {
            id: row
            required property var modelData
            required property int index
            readonly property string name: root.shown(modelData.name || modelData.project, 60)
            readonly property string detail: {
                const n = Number(modelData.assets) || 0
                const assets = n === 1 ? "1 asset" : MoneyText.count(n) + " assets"
                return modelData.status ? assets + " · " + root.shown(modelData.status, 24) : assets
            }
            readonly property bool isCurrent: modelData.project === root.current
            width: ListView.view.width
            height: rowText.implicitHeight + 8
            radius: Theme.radius
            color: ListView.isCurrentItem ? Theme.bg : "transparent"
            Accessible.role: Accessible.ListItem
            Accessible.name: name + ", " + detail + (isCurrent ? ", current" : "")
            Accessible.onPressAction: root.choose(modelData.project)

            MouseArea {
                anchors.fill: parent
                onClicked: {
                    row.ListView.view.currentIndex = row.index
                    root.choose(row.modelData.project)
                }
            }

            Column {
                id: rowText
                x: Theme.spacing
                y: 4
                width: parent.width - 2 * Theme.spacing

                Text {
                    width: parent.width
                    text: row.name + (row.isCurrent ? " (current)" : "")
                    textFormat: Text.PlainText
                    elide: Text.ElideRight
                    color: Theme.text
                    font.pixelSize: Theme.fontSize
                }
                Text {
                    width: parent.width
                    text: row.detail
                    textFormat: Text.PlainText
                    elide: Text.ElideRight
                    color: Theme.textMuted
                    font.pixelSize: Theme.fontSize - 1
                }
            }
        }
    }
}
