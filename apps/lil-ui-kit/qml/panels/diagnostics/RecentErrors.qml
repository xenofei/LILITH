// The recent errors by code (ARCH-121, QA-083), from `diagnostics.recent`, read again on each
// event of the `diagnostics` topic, at most once a second: an event names the code but not the
// message. The core keeps them redacted; they can still name your files, so they are shown as
// untrusted text and logged nowhere new. A row for a job's failure offers "Show the job". The rows
// are kept by code and updated in place, so a re-read leaves the keyboard focus where it was.
import QtQuick
import Lilith.Core
import Lilith.UiKit
import "DiagnosticsText.js" as Words

LCard {
    id: root

    required property var core
    property bool active: true

    readonly property var rows: errorsRead.result && errorsRead.result.errors ? errorsRead.result.errors : []
    readonly property alias rowsModel: rowsModel

    signal showJob(string jobId)

    onRowsChanged: syncRows()

    // `rowsModel` made the same as `rows`, moving a kept row rather than making it again.
    function syncRows() {
        const wanted = rows.map(r => ({
            code: String(r.code), times: r.count, first_at: String(r.first_at), last_at: String(r.last_at),
            last_message: String(r.last_message), last_tool: r.last_tool ? String(r.last_tool) : "",
            last_job_id: r.last_job_id ? String(r.last_job_id) : ""
        }))
        for (let i = 0; i < wanted.length; ++i) {
            let at = -1
            for (let j = i; j < rowsModel.count && at < 0; ++j) {
                if (rowsModel.get(j).code === wanted[i].code)
                    at = j
            }
            if (at < 0) {
                rowsModel.insert(i, wanted[i])
                continue
            }
            if (at !== i)
                rowsModel.move(at, i, 1)
            rowsModel.set(i, wanted[i])
        }
        if (rowsModel.count > wanted.length)
            rowsModel.remove(wanted.length, rowsModel.count - wanted.length)
    }

    function headLine(row) {
        const times = row.times === 1 ? "once" : MoneyText.count(String(row.times)) + " times"
        return Words.shown(core, row.code, 120) + " · " + times
    }

    function detailLine(row) {
        const parts = []
        if (row.times > 1)
            parts.push("first " + Words.when(core, row.first_at))
        parts.push("last " + Words.when(core, row.last_at))
        if (row.last_tool !== "")
            parts.push("from " + Words.shown(core, row.last_tool, 80))
        if (row.last_job_id !== "")
            parts.push("job " + Words.shown(core, row.last_job_id, 60))
        return parts.join(", ")
    }

    readonly property ToolRead errorsRead: ToolRead {
        id: errorsRead
        core: root.core
        tool: "diagnostics.recent"
        args: ({limit: 50})
        topic: "diagnostics"
        active: root.active
        onTopicEvent: root.errorsRead.request()
    }

    ListModel {
        id: rowsModel
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
            model: rowsModel
            delegate: Column {
                id: row
                required property string code
                required property int times
                required property string first_at
                required property string last_at
                required property string last_message
                required property string last_tool
                required property string last_job_id

                width: parent ? parent.width : 0
                spacing: 2

                LineText {
                    text: root.headLine(row)
                    font.bold: true
                }
                LineText {
                    tone: "muted"
                    text: root.detailLine(row)
                }
                LineText {
                    text: Words.shown(root.core, row.last_message, 600)
                }
                LButton {
                    visible: row.last_job_id !== ""
                    text: "Show the job"
                    Accessible.name: "Show the job of " + Words.shown(root.core, row.code, 120)
                    onClicked: root.showJob(row.last_job_id)
                }
            }
        }
    }
}
