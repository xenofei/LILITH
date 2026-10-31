// The project switcher (APP-006, ARCH-088, ARCH-098): shows the app's project, `sketchbook` when
// none was chosen, and lists `projects.list` each time it opens. Choosing a row calls
// `app/set_context {project}`, and the answer goes to `Core.project` (noteProject).
//
// The core keeps a session's project per connection, so after a reconnect the chosen one is sent
// again; one the core no longer knows is dropped. Another workspace forgets it.
import QtQuick
import Lilith.Core
import Lilith.UiKit

Item {
    id: root

    required property var core
    // The core is ready and has a workspace: a project is set within one (`workspace.required`).
    required property bool ready

    readonly property string current: core && core.project ? core.project : "sketchbook"
    readonly property string currentName: shown(_names[current] || current, 60)
    property alias button: button
    property alias popup: popup
    // projects.list's rows.
    property var projects: []
    // slug -> name, from the last list.
    property var _names: ({})
    property var _listCall: null
    property var _setCall: null
    property string _workspaceId

    implicitWidth: Math.min(button.implicitWidth, 200)
    implicitHeight: button.implicitHeight

    onReadyChanged: {
        if (!ready)
            popup.close()
    }

    function shown(text, max) {
        return core ? core.shownText(String(text), max) : ""
    }

    function refused(what, error) {
        popup.message = what + ": " + shown(error && error.message ? error.message : "the core didn't answer.", 300)
    }

    function open() {
        if (!ready)
            return
        popup.message = ""
        popup.open()
        read()
    }

    function read() {
        if (_listCall)
            _listCall.cancel()
        _listCall = CoreCall.call(core, "projects.list", {}, {
            owner: root,
            ok: r => {
                root._listCall = null
                const rows = (r.structured || {}).projects || []
                const names = {}
                for (const p of rows)
                    names[p.project] = p.name
                root._names = names
                root.projects = rows
                popup.view.currentIndex = rows.findIndex(p => p.project === root.current)
            },
            error: e => {
                root._listCall = null
                root.refused("Couldn't list the projects", e)
            }
        })
    }

    function choose(slug) {
        if (!ready || _setCall)
            return
        popup.message = ""
        _setCall = CoreCall.appCall(core, "app/set_context", {project: slug}, {
            owner: root,
            ok: r => {
                root._setCall = null
                root.core.noteProject(r.project || "")
                popup.close()
            },
            error: e => {
                root._setCall = null
                root.refused("Couldn't switch the project", e)
            },
            pending: () => { root._setCall = null }
        })
    }

    // A new connection has no project of its own yet. Runs once `ready` and the hello's workspace
    // have both come in.
    function sendAgain() {
        if (!core || core.connection !== "ready" || !core.project)
            return
        const slug = core.project
        CoreCall.appCall(core, "app/set_context", {project: slug}, {
            owner: root,
            error: e => {
                if (e && e.code === "project.unknown" && root.core.project === slug)
                    root.core.noteProject("")
            }
        })
    }

    function workspaceId() {
        return core && core.workspace ? String(core.workspace.id) : ""
    }

    Component.onCompleted: _workspaceId = workspaceId()

    Connections {
        target: root.core
        ignoreUnknownSignals: true
        function onConnectionChanged() {
            if (root.core.connection === "ready")
                Qt.callLater(root.sendAgain)
        }
        function onWorkspaceChanged() {
            const id = root.workspaceId()
            if (id === root._workspaceId)
                return
            root._workspaceId = id
            root.core.noteProject("")
        }
    }

    LButton {
        id: button
        property bool requiresCore: true
        width: root.width
        height: 28
        enabled: root.ready
        text: root.currentName + " ▾"
        Accessible.name: "Project: " + root.currentName
        onClicked: popup.visible ? popup.close() : root.open()
    }

    SwitcherPopup {
        id: popup
        listName: "Projects"
        rows: root.projects
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
