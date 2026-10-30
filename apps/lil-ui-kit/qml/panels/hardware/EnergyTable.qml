// The Hardware panel's energy (APP-131): this session, today, this month and this year, each by
// telemetry.energy over its period (Hardware.energyPeriods: from its local midnight, the session
// from this process's start, to now), with the GPU, the CPU, the whole system and, when an
// electricity rate is set, the cost; then the energy of the most recent jobs (telemetry.jobs).
// The whole system is always an estimate ((GPU + CPU + the rest-of-system baseline) / PSU
// efficiency), and says so. A part that wasn't measured throughout is labelled "estimated", and
// one with no energy at all and nothing measured (no GPU, a CPU with no meter and no TDP, a
// period with no samples yet) is "—", "not measured", never 0 Wh. It reads again every minute
// while it shows, and on reload().
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
    property real _readMs: 0
    // The minute's re-read runs (it pauses while the panel is closed or behind another tab).
    readonly property alias refreshing: refresh.running

    title: "Energy"

    Accessible.role: Accessible.Table
    Accessible.name: "Energy by period"

    function reload() {
        for (const c of _calls)
            c.cancel()
        _readMs = Date.now()
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

    // A part: its energy when measured, labelled when estimated, "" when not measured at all.
    function part(mj, measured) {
        if (!measured && mj <= 0)
            return ""
        return Format.energy(mj) + (measured ? "" : " (estimated)")
    }

    function cell(mj, measured) {
        return part(mj, measured) || "—"
    }

    // The whole system's estimate; "" for a period with nothing in it.
    function system(e) {
        return e.system_mj <= 0 && !e.measured.gpu && !e.measured.cpu ? "" : Format.energy(e.system_mj)
    }

    function noData(e) {
        return system(e) === "" && part(e.gpu_mj, e.measured.gpu) === "" && part(e.cpu_mj, e.measured.cpu) === ""
    }

    function partName(name, text) {
        return name + (text ? " " + text : " not measured")
    }

    function rowName(r) {
        const e = r.result
        if (!e)
            return r.label + ": not read yet"
        if (noData(e))
            return r.label + ": no data yet"
        const whole = system(e)
        return r.label + ": " + (whole ? "whole system " + whole + " (estimated)" : "whole system not measured")
               + ", " + partName("GPU", part(e.gpu_mj, e.measured.gpu))
               + ", " + partName("CPU", part(e.cpu_mj, e.measured.cpu))
               + (e.energy_cost ? ", cost " + Format.cost(e.energy_cost) : "")
    }

    onVisibleChanged: {
        if (visible && _readMs > 0 && Date.now() - _readMs >= refresh.interval)
            reload()
    }

    Timer {
        id: refresh
        interval: 60000
        repeat: true
        running: root.visible
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
                model: ["Period", "Whole system (estimated)", "GPU", "CPU"].concat(root.hasCost ? ["Cost"] : [])
                delegate: Text {
                    required property string modelData
                    required property int index
                    width: index === 0 ? table.labelWidth : table.col
                    text: modelData
                    color: Theme.textMuted
                    font.pixelSize: Theme.fontSize - 1
                    font.bold: true
                    wrapMode: Text.Wrap
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
                    objectName: "energySystem"
                    width: table.col
                    text: row.e ? (root.system(row.e) || "—") : "…"
                    color: Theme.text
                    font.pixelSize: Theme.fontSize
                    font.bold: true
                    elide: Text.ElideRight
                }
                Text {
                    objectName: "energyGpu"
                    width: table.col
                    text: row.e ? root.cell(row.e.gpu_mj, row.e.measured.gpu) : "…"
                    color: Theme.text
                    font.pixelSize: Theme.fontSize
                    elide: Text.ElideRight
                }
                Text {
                    objectName: "energyCpu"
                    width: table.col
                    text: row.e ? root.cell(row.e.cpu_mj, row.e.measured.cpu) : "…"
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
                readonly property string jobId: root.core ? root.core.shownText(String(modelData.job_id), 16) : ""
                Accessible.role: Accessible.StaticText
                Accessible.name: "Job " + jobId + ": " + jobText.text

                Text {
                    width: table.labelWidth
                    text: parent.jobId
                    color: Theme.text
                    font.pixelSize: Theme.fontSize
                    elide: Text.ElideRight
                    textFormat: Text.PlainText
                }
                Text {
                    id: jobText
                    width: table.width - table.labelWidth
                    text: Format.energy(modelData.system_mj) + " (estimated)"
                          + "  ·  " + root.partName("GPU", root.part(modelData.gpu_mj_above_idle, modelData.measured.gpu))
                          + " above idle" + (modelData.idle_unknown ? " (no idle baseline)" : "")
                    color: Theme.textMuted
                    font.pixelSize: Theme.fontSize
                    elide: Text.ElideRight
                }
            }
        }
    }
}
