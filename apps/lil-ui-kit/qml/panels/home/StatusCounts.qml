// Home's assets by status (APP-111): the current project's `project.status` counts. In copy a
// candidate is a take (APP-163). The core lists only the statuses that have assets, so one it leaves
// out reads 0. It needs a workspace (APP-016).
import QtQuick
import Lilith.Core
import Lilith.UiKit

LCard {
    id: root

    required property var panel

    readonly property var statuses: [{key: "candidate", word: "Takes waiting"}, {key: "in_review", word: "In review"},
                                     {key: "stale", word: "Stale"}, {key: "blocked", word: "Blocked"}]
    readonly property string countsText: {
        const counts = panel.byStatus
        if (!counts)
            return ""
        return statuses.map(s => s.word + " " + MoneyText.count(String(counts[s.key] || 0))).join(" · ")
    }
    readonly property string note: {
        if (!panel.connected)
            return panel.noteFor("status", "")
        if (panel.readState.status === "none")
            return panel.projects.length > 0 ? "Choose a project in the top bar to see its counts." : "No project to count yet."
        return panel.noteFor("status", "the counts")
    }
    readonly property alias countsLabel: countsLabel
    readonly property alias noteText: noteText

    title: panel.statusProject !== "" ? "Assets in " + panel.shown(panel.statusProject) : "Assets by status"
    visible: panel.hasWorkspace

    Column {
        width: parent.width
        spacing: 4

        Text {
            id: countsLabel
            width: parent.width
            visible: text !== ""
            text: root.countsText
            textFormat: Text.PlainText
            color: root.panel.isStale("status") ? Theme.textMuted : Theme.text
            font.pixelSize: Theme.fontSize
            wrapMode: Text.Wrap
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }

        Text {
            id: noteText
            width: parent.width
            visible: root.note !== ""
            text: root.note
            textFormat: Text.PlainText
            color: root.panel.isStale("status") ? Theme.warning : Theme.textMuted
            font.pixelSize: Theme.fontSize
            wrapMode: Text.Wrap
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }
    }
}
