// The Hardware panel's energy (APP-131): this session, today, this month and this year, each by
// telemetry.energy over its period (Hardware.energyPeriods: from its local midnight, the session
// from this process's start, to now), with the GPU, the CPU, the whole system and, when an
// electricity rate is set, the cost; then the energy of the most recent jobs (telemetry.jobs).
// The whole system is always an estimate ((GPU + CPU + the rest-of-system baseline) / PSU
// efficiency), and says so. A part that wasn't measured throughout is labelled "estimated", and
// one with no energy at all and nothing measured (no GPU, a CPU with no meter and no TDP, a
// period with no samples yet) is "—", "not measured", never 0 Wh; "not measured on this PC" when
// the snapshot says no source here can measure it (`parts`: its `system_parts`), and a row of
// nothing on a PC that can measure nothing says that instead of "no data yet". The cost comes from
// the whole system's estimate, so it says so too, and shows through MoneyText; a period or a job
// with no energy figures is "—", never 0. It reads again every minute while it shows, and on
// reload(); a read that fails shows a notice and "—", not "…" or old figures that look current,
// until a later read succeeds.
import QtQuick
import Lilith.Core
import Lilith.UiKit
import "HardwareFormat.js" as Format

LCard {
    id: root

    required property var core
    // The snapshot's `system_parts`: [{part, mw, measured}], measured being the basis.
    property var parts: []

    // The recent jobs listed.
    readonly property int jobCount: 5
    readonly property var labels: ({session: "This session", day: "Today", month: "This month",
                                     year: "This year"})

    // [{period, label, from, to, result, failed}]; `result` is telemetry.energy's, null until it is
    // in or when the last read of it failed.
    property var rows: []
    property var jobs: []
    // The last read of the recent jobs failed. A row has `failed` for its own.
    property bool jobsFailed: false
    readonly property bool readFailed: jobsFailed || rows.some(r => r.failed)
    readonly property bool hasCost: rows.some(r => r.result && r.result.energy_cost)

    property int refreshMs: 60000
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
                                         to: new Date(p.to).toISOString(), result: null, failed: false}))
        // The old figures, or the failure, stay until the new answer is in.
        for (const r of fresh) {
            const old = rows.find(o => o.period === r.period)
            if (old) {
                r.result = old.result
                r.failed = old.failed
            }
        }
        rows = fresh
        const calls = []
        for (const r of fresh) {
            const period = r.period
            calls.push(CoreCall.call(core, "telemetry.energy", {from: r.from, to: r.to}, {
                owner: root,
                ok: out => setResult(period, out.structured, false),
                error: () => setResult(period, null, true)
            }))
        }
        calls.push(CoreCall.call(core, "telemetry.jobs", {limit: jobCount}, {
            owner: root,
            ok: out => {
                root.jobsFailed = false
                root.jobs = (out.structured && out.structured.jobs) || []
            },
            error: () => {
                root.jobsFailed = true
                root.jobs = []
            }
        }))
        _calls = calls
    }

    function setResult(period, result, failed) {
        rows = rows.map(r => r.period === period ? Object.assign({}, r, {result: result, failed: failed}) : r)
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

    // The cost, from the whole system's estimate; "" for a period without that figure.
    function cost(e) {
        if (!e.energy_cost || system(e) === "")
            return ""
        return MoneyText.text(String(e.energy_cost.amount), String(e.energy_cost.currency)) + " (estimated)"
    }

    // A job's energy; "—" for one with no energy figures.
    function jobLine(j) {
        if (system(j) === "")
            return "—"
        return system(j) + " (estimated)"
               + "  ·  " + partName("GPU", part(j.gpu_mj_above_idle, j.measured.gpu), "gpu")
               + " above idle" + (j.idle_unknown ? " (no idle baseline)" : "")
    }

    function jobName(j) {
        return system(j) !== "" ? jobLine(j) : nothingMeasurable() ? "not measured on this PC" : "not measured"
    }

    function noData(e) {
        return system(e) === "" && part(e.gpu_mj, e.measured.gpu) === "" && part(e.cpu_mj, e.measured.cpu) === ""
    }

    // No source on this PC can measure `key` ("gpu", "cpu", "rest"): its basis is unknown.
    function unmeasurable(key) {
        const p = parts.find(x => x.part === key)
        return !!p && p.measured === "unknown"
    }

    function nothingMeasurable() {
        return parts.length > 0 && ["gpu", "cpu", "rest"].every(unmeasurable)
    }

    function partName(name, text, key) {
        return name + (text ? " " + text : unmeasurable(key) ? " not measured on this PC" : " not measured")
    }

    function rowName(r) {
        const e = r.result
        if (r.failed)
            return r.label + ": can't be read now"
        if (!e)
            return r.label + ": not read yet"
        if (noData(e))
            return r.label + (nothingMeasurable() ? ": not measured on this PC" : ": no data yet")
        const whole = system(e)
        return r.label + ": " + (whole ? "whole system " + whole + " (estimated)" : "whole system not measured")
               + ", " + partName("GPU", part(e.gpu_mj, e.measured.gpu), "gpu")
               + ", " + partName("CPU", part(e.cpu_mj, e.measured.cpu), "cpu")
               + (cost(e) ? ", cost " + cost(e) : "")
    }

    onVisibleChanged: {
        if (visible && _readMs > 0 && Date.now() - _readMs >= refresh.interval)
            reload()
    }

    Timer {
        id: refresh
        // A failed read tries again here too.
        interval: root.refreshMs
        repeat: true
        running: root.visible
        onTriggered: root.reload()
    }

    Column {
        id: table
        width: parent.width
        spacing: 4

        Text {
            objectName: "energyNotice"
            visible: root.readFailed
            width: parent.width
            text: "The energy figures can't be read now. Trying again every minute."
            color: Theme.warning
            font.pixelSize: Theme.fontSize
            wrapMode: Text.Wrap
            textFormat: Text.PlainText
        }

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
                readonly property string unread: modelData.failed ? "—" : "…"
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
                    text: row.e ? (root.system(row.e) || "—") : row.unread
                    color: Theme.text
                    font.pixelSize: Theme.fontSize
                    font.bold: true
                    elide: Text.ElideRight
                }
                Text {
                    objectName: "energyGpu"
                    width: table.col
                    text: row.e ? root.cell(row.e.gpu_mj, row.e.measured.gpu) : row.unread
                    color: Theme.text
                    font.pixelSize: Theme.fontSize
                    wrapMode: Text.Wrap
                }
                Text {
                    objectName: "energyCpu"
                    width: table.col
                    text: row.e ? root.cell(row.e.cpu_mj, row.e.measured.cpu) : row.unread
                    color: Theme.text
                    font.pixelSize: Theme.fontSize
                    wrapMode: Text.Wrap
                }
                Text {
                    objectName: "energyCost"
                    visible: root.hasCost
                    width: table.col
                    text: (row.e && root.cost(row.e)) || "—"
                    color: Theme.text
                    font.pixelSize: Theme.fontSize
                    wrapMode: Text.Wrap
                    textFormat: Text.PlainText
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
                objectName: "energyJob:" + modelData.job_id
                Accessible.role: Accessible.StaticText
                Accessible.name: "Job " + jobId + ": " + root.jobName(modelData)

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
                    objectName: "energyJobText"
                    width: table.width - table.labelWidth
                    text: root.jobLine(modelData)
                    color: Theme.textMuted
                    font.pixelSize: Theme.fontSize
                    elide: Text.ElideRight
                }
            }
        }
    }
}
