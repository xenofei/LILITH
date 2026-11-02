// Home's open jobs (APP-111): the running and queued jobs in run order, from the panel's
// LiveJobsModel, at most `maxRows` of them. A job with no project, or a project the list doesn't
// know, is shown like any other, and a kind, status or substatus this build doesn't know by its name
// (P4a-R40). Jobs has the rest, and the controls.
import QtQuick
import Lilith.Core
import Lilith.UiKit
import "../jobs/PausedText.js" as PausedText

LCard {
    id: root

    required property var panel

    readonly property int maxRows: 8
    readonly property int rowHeight: Theme.fontSize + 10
    readonly property int count: panel.live.count
    readonly property string note: {
        if (!panel.connected)
            return panel.noteFor("jobs", "")
        const n = panel.noteFor("jobs", "the jobs")
        if (n !== "")
            return n
        return count === 0 ? "Nothing is running or queued." : ""
    }
    readonly property alias view: view
    readonly property alias noteText: noteText
    readonly property alias moreLabel: moreLabel

    // What a job's row says about its state.
    function stateWords(status, substatus, reasons) {
        if (status === "running") {
            const known = {executing: "", cancelling: "Cancelling", interrupted_unknown: "Outcome unknown",
                           ingesting: "Bringing in the outputs"}
            const word = Object.prototype.hasOwnProperty.call(known, substatus) ? known[substatus] : panel.shown(substatus)
            return word !== "" ? "Running · " + word : "Running"
        }
        if (status === "queued") {
            if ((reasons || []).indexOf("restart") >= 0)
                return "Held after a restart"
            if (substatus === "queue_paused")
                return PausedText.label(reasons, panel.core)
            const known = {waiting: "Queued", waiting_for_vram: "Waiting for GPU memory", submitting: "Submitting",
                           queued_in_engine: "In ComfyUI's queue"}
            return Object.prototype.hasOwnProperty.call(known, substatus) ? known[substatus]
                 : substatus ? "Queued · " + panel.shown(substatus) : "Queued"
        }
        // A status this build doesn't know, by its name.
        return substatus ? panel.shown(status) + " · " + panel.shown(substatus) : panel.shown(status)
    }

    title: "Open jobs"

    Column {
        width: parent.width
        spacing: 4

        ListView {
            id: view
            width: parent.width
            height: Math.min(root.count, root.maxRows) * root.rowHeight
            visible: root.count > 0
            interactive: false
            clip: true
            cacheBuffer: 0
            model: root.panel.live

            Accessible.role: Accessible.List
            Accessible.name: "Open jobs"

            delegate: Text {
                required property var model
                readonly property string jobId: model.job_id || ""
                width: ListView.view.width
                height: root.rowHeight
                verticalAlignment: Text.AlignVCenter
                text: [JobText.kindWord(model.kind || "") || root.panel.shown(model.kind || "Job"),
                       model.project ? root.panel.shown(model.project) : "no project",
                       root.stateWords(model.status, model.substatus, model.pause_reasons)].join(" · ")
                textFormat: Text.PlainText
                color: root.panel.isStale("jobs") ? Theme.textMuted : Theme.text
                font.pixelSize: Theme.fontSize
                elide: Text.ElideRight
                Accessible.role: Accessible.ListItem
                Accessible.name: text
            }
        }

        Text {
            id: moreLabel
            width: parent.width
            visible: text !== ""
            text: {
                const capped = root.panel.jobsCapped
                if (root.count > root.maxRows)
                    return "and " + MoneyText.count(String(root.count - root.maxRows)) + (capped >= 0 ? " or more" : " more")
                return capped >= 0 ? "and more" : ""
            }
            textFormat: Text.PlainText
            color: Theme.textMuted
            font.pixelSize: Theme.fontSize
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }

        Text {
            id: noteText
            width: parent.width
            visible: root.note !== ""
            text: root.note
            textFormat: Text.PlainText
            color: root.panel.isStale("jobs") ? Theme.warning : Theme.textMuted
            font.pixelSize: Theme.fontSize
            wrapMode: Text.Wrap
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }
    }
}
