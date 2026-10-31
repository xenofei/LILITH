// The recent errors by code (ARCH-121, QA-083), from `diagnostics.recent`, read again on each
// event of the `diagnostics` topic, at most once a second: an event names the code but not the
// message. The core keeps them redacted; they can still name your files, so they are shown as
// untrusted text and logged nowhere new. A row for a job's failure offers "Show the job".
import QtQuick
import Lilith.Core
import Lilith.UiKit
import "DiagnosticsText.js" as Words

LCard {
    id: root

    required property var core

    readonly property var rows: errorsRead.result && errorsRead.result.errors ? errorsRead.result.errors : []

    signal showJob(string jobId)

    function headLine(row) {
        const times = row.count === 1 ? "once" : MoneyText.count(String(row.count)) + " times"
        return Words.shown(core, row.code, 120) + " · " + times
    }

    function detailLine(row) {
        const parts = []
        if (row.count > 1)
            parts.push("first " + Words.when(core, row.first_at))
        parts.push("last " + Words.when(core, row.last_at))
        if (row.last_tool)
            parts.push("from " + Words.shown(core, row.last_tool, 80))
        if (row.last_job_id)
            parts.push("job " + Words.shown(core, row.last_job_id, 60))
        return parts.join(", ")
    }

    readonly property ToolRead errorsRead: ToolRead {
        id: errorsRead
        core: root.core
        tool: "diagnostics.recent"
        args: ({limit: 50})
        topic: "diagnostics"
        onTopicEvent: root.errorsRead.request()
    }

    title: "Recent errors"

    Column {
        width: parent.width
        spacing: Theme.spacing

        LineText {
            visible: text !== ""
            tone: "warning"
            text: Words.errorText(root.core, errorsRead.error)
        }
        LineText {
            visible: errorsRead.loaded && root.rows.length === 0
            tone: "muted"
            text: "No errors since the core started."
        }

        Repeater {
            model: root.rows
            delegate: Column {
                id: row
                required property var modelData
                width: parent.width
                spacing: 2

                LineText {
                    text: root.headLine(row.modelData)
                    font.bold: true
                }
                LineText {
                    tone: "muted"
                    text: root.detailLine(row.modelData)
                }
                LineText {
                    text: Words.shown(root.core, row.modelData.last_message, 600)
                }
                LButton {
                    visible: !!row.modelData.last_job_id
                    text: "Show the job"
                    Accessible.name: "Show the job of " + Words.shown(root.core, row.modelData.code, 120)
                    onClicked: root.showJob(row.modelData.last_job_id)
                }
            }
        }
    }
}
