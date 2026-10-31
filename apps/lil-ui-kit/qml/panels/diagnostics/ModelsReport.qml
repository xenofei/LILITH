// The model inventory (CORE-164), from `models.report`, read again when an `engines` event shows a
// newer inventory. The states are the inventory's glossary; nothing is moved or deleted because of
// one. Unless `registry` is `ok` the report can't tell what is recorded, so the card says why and
// calls no file orphan or missing. Paths, families and names are untrusted: plain text, normalised
// (P4a-R8). One tab stop; the arrow keys move through the rows.
import QtQuick
import Lilith.Core
import Lilith.UiKit
import "DiagnosticsText.js" as Words
import "../hardware/HardwareFormat.js" as Format

LCard {
    id: root

    required property var core

    readonly property alias view: list
    readonly property var report: reportRead.result
    readonly property bool registryRead: !!report && report.registry === "ok"
    // The missing entries first, then the files.
    readonly property var rows: {
        if (!report)
            return []
        const missing = registryRead ? (report.missing || []).map(m => ({missing: true, entry: m})) : []
        return missing.concat((report.models || []).map(m => ({missing: false, entry: m})))
    }
    property string _lastInventory

    function count(n) {
        return MoneyText.count(String(n))
    }

    readonly property string registryText: {
        if (!report || registryRead)
            return ""
        const end = ", so no file is called orphan or missing."
        if (report.registry === "missing")
            return "registry/models.yaml doesn't exist" + end
        if (report.registry === "unreadable")
            return "registry/models.yaml couldn't be read" + end
        if (report.registry === "no_workspace")
            return "There is no default workspace to read registry/models.yaml from" + end
        return "The model registry's state is " + Words.shown(core, report.registry, 40) + end
    }

    readonly property string summaryText: {
        if (!report)
            return ""
        if (report.refreshed === null || report.refreshed === undefined)
            return "No inventory yet: it is read when ComfyUI is ready."
        const n = (report.models || []).length
        return count(n) + (n === 1 ? " model file" : " model files") + ", in " + Words.shown(core, report.root, 200)
               + ", read " + Words.when(core, report.refreshed)
    }

    readonly property string statesText: {
        if (!report)
            return ""
        const has = state => (report.models || []).filter(m => (m.states || []).indexOf(state) >= 0).length
        const parts = []
        if (registryRead && has("orphan") > 0)
            parts.push(count(has("orphan")) + " orphan")
        if (has("duplicate") > 0)
            parts.push(count(has("duplicate")) + " duplicate")
        if (has("unknown_family") > 0)
            parts.push(count(has("unknown_family")) + " of unknown family")
        if (registryRead && (report.missing || []).length > 0)
            parts.push(count(report.missing.length) + " missing")
        return parts.join(", ")
    }

    function stateWords(entry) {
        const out = []
        for (const s of entry.states || []) {
            if (s === "orphan" && !registryRead)
                continue
            out.push(s === "unknown_family" ? "unknown family" : Words.shown(core, s, 40))
        }
        return out
    }

    function nameLine(row) {
        const e = row.entry
        if (row.missing)
            return "Missing: " + Words.shown(core, e.model_name || e.model_id, 200)
        return Words.shown(core, e.path, 260)
    }

    function detailLine(row) {
        const e = row.entry
        if (row.missing)
            return "recorded in registry/models.yaml as " + Words.shown(core, e.model_id, 80) + ", not on disk"
        const parts = [Format.gib(e.size / 1048576)]
        if (e.model_name)
            parts.push(Words.shown(core, e.model_name, 120))
        if (e.family)
            parts.push(Words.shown(core, e.family, 80))
        if (e.sha256 === null || e.sha256 === undefined)
            parts.push("not hashed")
        return parts.concat(stateWords(e)).join(" · ")
    }

    readonly property ToolRead reportRead: ToolRead {
        id: reportRead
        core: root.core
        tool: "models.report"
        topic: "engines"
        onTopicEvent: data => {
            const inventory = data && data.engine ? data.engine.inventory : null
            const refreshed = inventory ? inventory.refreshed : ""
            if (refreshed && refreshed !== root._lastInventory) {
                root._lastInventory = refreshed
                root.reportRead.request()
            }
        }
        onResultChanged: {
            if (result && result.refreshed)
                root._lastInventory = result.refreshed
        }
    }

    title: "Model inventory"

    Column {
        width: parent.width
        spacing: Theme.spacing

        LineText {
            visible: text !== ""
            tone: "warning"
            text: Words.errorText(root.core, reportRead.error)
        }
        LineText {
            visible: text !== ""
            text: root.summaryText
        }
        LineText {
            visible: text !== ""
            tone: "warning"
            text: root.registryText
        }
        LineText {
            visible: text !== ""
            text: root.statesText
        }

        ListView {
            id: list

            width: parent.width
            height: Math.min(contentHeight, 240)
            visible: count > 0
            clip: true
            model: root.rows
            activeFocusOnTab: true
            boundsBehavior: Flickable.StopAtBounds
            highlightMoveDuration: 0
            highlightResizeDuration: 0

            Accessible.role: Accessible.List
            Accessible.name: "Model files"

            highlight: Rectangle {
                color: Theme.surfaceRaised
                visible: list.activeFocus
            }

            delegate: Item {
                id: row
                required property var modelData

                readonly property string nameText: root.nameLine(modelData)
                readonly property string detailText: root.detailLine(modelData)

                width: ListView.view.width
                height: rowColumn.implicitHeight + 6

                Accessible.role: Accessible.ListItem
                Accessible.name: nameText + ": " + detailText

                Column {
                    id: rowColumn
                    x: 4
                    y: 3
                    width: parent.width - 8

                    Text {
                        width: parent.width
                        text: row.nameText
                        textFormat: Text.PlainText
                        color: row.modelData.missing ? Theme.warning : Theme.text
                        font.pixelSize: Theme.fontSize
                        elide: Text.ElideMiddle
                    }
                    Text {
                        width: parent.width
                        text: row.detailText
                        textFormat: Text.PlainText
                        color: Theme.textMuted
                        font.pixelSize: Theme.fontSize - 1
                        elide: Text.ElideRight
                    }
                }
            }
        }
    }

    // The keyboard focus on the list.
    Rectangle {
        visible: list.activeFocus
        parent: list
        anchors.fill: parent
        color: "transparent"
        border.color: Theme.focus
        border.width: Theme.focusWidth
        z: 2
    }
}
