// The Hardware panel's energy (APP-131): this session, today, this month and this year, each by
// telemetry.energy over its period (Hardware.energyPeriods: from its local midnight, the session
// from this process's start, to now), with the GPU, the CPU, the whole system and, when an
// electricity rate is set, the cost; then the energy of the most recent jobs (telemetry.jobs).
// A part that wasn't measured throughout is labelled "estimated". It reads again every minute
// and on reload().
import QtQuick
import Lilith.Core
import Lilith.UiKit
import "HardwareFormat.js" as Format

LCard {
    id: root

    required property var core

    // The recent jobs listed.
    readonly property int jobCount: 5
    readonly property var labels: ({session: "This session", day: "Today", month: "This month",
                                     year: "This year"})

    // [{period, label, from, to, result}]; `result` is telemetry.energy's, null until it is in.
    property var rows: []
    property var jobs: []
    readonly property bool hasCost: rows.some(r => r.result && r.result.energy_cost)

    property var _calls: []

    title: "Energy"

    Accessible.role: Accessible.Table
    Accessible.name: "Energy by period"

    function reload() {
        for (const c of _calls)
            c.cancel()
        const periods = JSON.parse(Hardware.energyPeriods(Date.now()))
        const fresh = periods.map(p => ({period: p.period, label: labels[p.period],
                                         from: new Date(p.from).toISOString(),
                                         to: new Date(p.to).toISOString(), result: null}))
        // The old figures stay until the new ones are in.
        for (const r of fresh) {
            const old = rows.find(o => o.period === r.period)
            r.result = old ? old.result : null
        }
        rows = fresh
        const calls = []
        for (const r of fresh) {
            const period = r.period
            calls.push(CoreCall.call(core, "telemetry.energy", {from: r.from, to: r.to}, {
                owner: root,
                ok: out => setResult(period, out.structured)
            }))
        }
        calls.push(CoreCall.call(core, "telemetry.jobs", {limit: jobCount}, {
            owner: root,
            ok: out => { root.jobs = (out.structured && out.structured.jobs) || [] }
        }))
        _calls = calls
    }

    function setResult(period, result) {
        rows = rows.map(r => r.period === period ? Object.assign({}, r, {result: result}) : r)
    }

    function part(mj, measured) {
        return Format.energy(mj) + (measured ? "" : " (estimated)")
    }

    function rowName(r) {
        const e = r.result
        if (!e)
            return r.label + ": not read yet"
        return r.label + ": whole system " + part(e.system_mj, e.measured.gpu && e.measured.cpu)
               + ", GPU " + part(e.gpu_mj, e.measured.gpu) + ", CPU " + part(e.cpu_mj, e.measured.cpu)
               + (e.energy_cost ? ", cost " + Format.cost(e.energy_cost) : "")
    }

    Timer {
        interval: 60000
        repeat: true
        running: true
        onTriggered: root.reload()
    }

    Column {
        id: table
        width: parent.width
        spacing: 4

        readonly property real col: (width - labelWidth) / (root.hasCost ? 4 : 3)
        readonly property real labelWidth: Math.min(120, width * 0.25)

        Row {
            Repeater {
                model: ["Period", "Whole system", "GPU", "CPU"].concat(root.hasCost ? ["Cost"] : [])
                delegate: Text {
                    required property string modelData
                    required property int index
                    width: index === 0 ? table.labelWidth : table.col
                    text: modelData
                    color: Theme.textMuted
                    font.pixelSize: Theme.fontSize - 1
                    font.bold: true
                    elide: Text.ElideRight
                }
            }
        }

        Repeater {
            model: root.rows

            delegate: Row {
                id: row
                required property var modelData
                readonly property var e: modelData.result
                objectName: "energyRow:" + modelData.period

                Accessible.role: Accessible.Row
                Accessible.name: root.rowName(modelData)

                Text {
                    width: table.labelWidth
                    text: row.modelData.label
                    color: Theme.text
                    font.pixelSize: Theme.fontSize
                    elide: Text.ElideRight
                }
                Text {
                    width: table.col
                    text: row.e ? root.part(row.e.system_mj, row.e.measured.gpu && row.e.measured.cpu) : "…"
                    color: Theme.text
                    font.pixelSize: Theme.fontSize
                    font.bold: true
                    elide: Text.ElideRight
                }
                Text {
                    objectName: "energyGpu"
                    width: table.col
                    text: row.e ? root.part(row.e.gpu_mj, row.e.measured.gpu) : "…"
                    color: Theme.text
                    font.pixelSize: Theme.fontSize
                    elide: Text.ElideRight
                }
                Text {
                    objectName: "energyCpu"
                    width: table.col
                    text: row.e ? root.part(row.e.cpu_mj, row.e.measured.cpu) : "…"
                    color: Theme.text
                    font.pixelSize: Theme.fontSize
                    elide: Text.ElideRight
                }
                Text {
                    visible: root.hasCost
                    width: table.col
                    text: row.e && row.e.energy_cost ? Format.cost(row.e.energy_cost) : "—"
                    color: Theme.text
                    font.pixelSize: Theme.fontSize
                    elide: Text.ElideRight
                }
            }
        }

        Text {
            visible: root.jobs.length > 0
            topPadding: Theme.spacing
            text: "Recent jobs"
            color: Theme.textMuted
            font.pixelSize: Theme.fontSize - 1
            font.bold: true
        }

        Repeater {
            model: root.jobs

            delegate: Row {
                required property var modelData
                Accessible.role: Accessible.StaticText
                Accessible.name: "Job " + modelData.job_id + ": " + jobText.text

                Text {
                    width: table.labelWidth
                    text: root.core ? root.core.shownText(String(modelData.job_id), 16) : ""
                    color: Theme.text
                    font.pixelSize: Theme.fontSize
                    elide: Text.ElideRight
                    textFormat: Text.PlainText
                }
                Text {
                    id: jobText
                    width: table.width - table.labelWidth
                    text: root.part(modelData.system_mj, modelData.measured.gpu && modelData.measured.cpu)
                          + "  ·  GPU " + root.part(modelData.gpu_mj_above_idle, modelData.measured.gpu)
                          + " above idle" + (modelData.idle_unknown ? " (no idle baseline)" : "")
                    color: Theme.textMuted
                    font.pixelSize: Theme.fontSize
                    elide: Text.ElideRight
                }
            }
        }
    }
}
