// Home's recent assets (APP-111): the workspace's 12 newest by `updated`, each with its project, its
// status (a candidate is a take, APP-163) and its time in the local calendar. With no workspace this
// is the recent loose files instead (APP-016), which plan 5 fills.
import QtQuick
import Lilith.UiKit

LCard {
    id: root

    required property var panel

    readonly property string note: {
        if (panel.noWorkspace)
            return "This version doesn't list loose files yet."
        if (!panel.connected)
            return panel.noteFor("assets", "")
        const n = panel.noteFor("assets", "the assets")
        if (n !== "")
            return n
        return panel.assets.length === 0 ? "No assets yet." : ""
    }
    readonly property alias rows: rows
    readonly property alias noteText: noteText

    function statusWord(status) {
        const known = {planned: "Planned", in_progress: "In progress", candidate: "Take waiting", in_review: "In review",
                       approved: "Approved", exported: "Exported", released: "Released", stale: "Stale",
                       blocked: "Blocked", deprecated: "Deprecated"}
        return !status ? "" : Object.prototype.hasOwnProperty.call(known, status) ? known[status] : panel.shown(status)
    }

    title: panel.noWorkspace ? "Recent loose files" : "Recent assets"

    Column {
        width: parent.width
        spacing: 4

        Repeater {
            id: rows
            model: root.panel.noWorkspace ? [] : root.panel.assets
            delegate: Text {
                required property var modelData
                width: parent.width
                text: [root.panel.shown(modelData.asset_id), root.panel.shown(modelData.project),
                       root.statusWord(modelData.status), root.panel.localStamp(modelData.updated)]
                      .filter(p => p !== "").join(" · ")
                textFormat: Text.PlainText
                color: root.panel.isStale("assets") ? Theme.textMuted : Theme.text
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
            color: root.panel.isStale("assets") && !root.panel.noWorkspace ? Theme.warning : Theme.textMuted
            font.pixelSize: Theme.fontSize
            wrapMode: Text.Wrap
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }
    }
}
