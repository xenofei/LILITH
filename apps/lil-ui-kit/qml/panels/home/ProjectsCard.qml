// Home's workspace and projects (APP-111): the workspace's folder, and each project of
// `projects.list` with its asset count. With no workspace, it says so (APP-016).
import QtQuick
import Lilith.Core
import Lilith.UiKit

LCard {
    id: root

    required property var panel

    readonly property string workspaceText: {
        const path = panel.core && panel.core.workspace ? String(panel.core.workspace.path || "") : ""
        const parts = path.split(/[\\/]/).filter(p => p !== "")
        return parts.length > 0 ? "Workspace: " + panel.shown(parts[parts.length - 1]) : ""
    }
    readonly property string note: {
        if (panel.noWorkspace)
            return "No workspace is open."
        if (!panel.connected)
            return panel.noteFor("projects", "")
        const n = panel.noteFor("projects", "the projects")
        if (n !== "")
            return n
        return panel.projects.length === 0 ? "No projects in this workspace yet." : ""
    }
    readonly property alias rows: rows
    readonly property alias noteText: noteText

    function rowText(p) {
        const n = Number(p.assets) || 0
        const assets = n === 1 ? "1 asset" : MoneyText.count(String(n)) + " assets"
        const status = p.status && p.status !== "active" ? " · " + panel.shown(p.status, 24) : ""
        return panel.shown(p.name || p.project) + " · " + assets + status
    }

    title: "Projects"

    Column {
        width: parent.width
        spacing: 4

        Text {
            width: parent.width
            visible: root.workspaceText !== ""
            text: root.workspaceText
            textFormat: Text.PlainText
            color: Theme.textMuted
            font.pixelSize: Theme.fontSize
            elide: Text.ElideRight
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }

        Repeater {
            id: rows
            model: root.panel.projects
            delegate: Text {
                required property var modelData
                width: parent.width
                text: root.rowText(modelData)
                textFormat: Text.PlainText
                color: root.panel.isStale("projects") ? Theme.textMuted : Theme.text
                font.pixelSize: Theme.fontSize
                elide: Text.ElideRight
                Accessible.role: Accessible.StaticText
                Accessible.name: text
            }
        }

        Text {
            id: noteText
            width: parent.width
            visible: root.note !== ""
            text: root.note
            textFormat: Text.PlainText
            color: root.panel.isStale("projects") ? Theme.warning : Theme.textMuted
            font.pixelSize: Theme.fontSize
            wrapMode: Text.Wrap
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }
    }
}
