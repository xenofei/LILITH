// The Hardware panel (APP-127 to APP-129, APP-131, APP-132; P4a-R17): the live state from the
// `telemetry` topic, charts over a window of 15 minutes (where it opens) to 24 hours, energy by
// period and the alerts.
//
// - **State.** It follows `telemetry` (CoreCall) and loads `telemetry.snapshot`. Each `sample`
//   event carries the whole snapshot and replaces it, unless it is older than the one shown
//   (events can overtake one another); `alert` and `alert_cleared` change the alerts at once, and
//   the next sample's `alerts` are the state again. A `snapshot` of the topic is the state too,
//   and like `resync_required` it loads everything again. It never reloads on `ready`, but for a
//   first load that failed (the session wasn't connected yet). The cards' rows are fixed items
//   bound to the snapshot, updated in place every second, never rebuilt.
// - **Charts.** One `telemetry.series` call loads every chart for the window, at
//   Hardware.resolutionFor(window) (1 s up to an hour, else 1 min), from `to` minus the window
//   plus two steps, so a range stays under the tool's 3,600 points even with a clock running a
//   little fast. Each sample is appended to every chart, with NaN where it has no value, and
//   ends every chart's window: a chart that stopped shows it. The samples come at 1 Hz through
//   Core's throttle (APP-027). Each chart keeps at most a point per pixel (ChartSeries, in
//   Rust).
// - **No CPU temperature** (APP-128): Windows doesn't give it without a driver.
// - A figure that isn't measured says so: "estimated" (the whole system always, and a part
//   whose basis in `system_parts` is estimated), "unknown", "not reported", "not measured",
//   "rest of system not set", or "last reading" for a GPU that was lost.
// - Text from outside the panel (the GPU's and the disks' and adapters' names, the core's error
//   messages, an unknown alert kind) is shown through Core.shownText (P4a-R8) and as plain text.
import QtQuick
import QtQuick.Controls.Basic as QQC
import Lilith.Core
import Lilith.UiKit
import "HardwareFormat.js" as Format

Rectangle {
    id: root

    required property var core
    required property string title

    // The chart window in seconds: 15 minutes to 24 hours.
    property int windowS: 15 * 60
    readonly property var windows: [
        {s: 15 * 60, text: "15 min", words: "last 15 minutes"},
        {s: 3600, text: "1 h", words: "last hour"},
        {s: 6 * 3600, text: "6 h", words: "last 6 hours"},
        {s: 24 * 3600, text: "24 h", words: "last 24 hours"}
    ]
    readonly property string windowWords: (windows.find(w => w.s === windowS) || windows[0]).words

    // The telemetry snapshot shown, null until the first is in.
    property var snapshot: null
    // Whether a snapshot has been loaded, so a later `ready` never reloads.
    property bool loaded: false
    // The last failure to show, "" for none.
    property string problem: ""

    readonly property var gpu: snapshot ? snapshot.gpu || null : null
    readonly property var cpu: snapshot ? snapshot.cpu : null

    // The charts, in order, as they are made.
    property var chartTiles: []

    // The charts: metric, label, value as text and the live value in a snapshot (NaN for none).
    // Constant: what depends on the snapshot (the scale, the estimate note) is bound per tile.
    readonly property var chartSpecs: [
        {metric: "gpu.util", label: "GPU load", format: v => Format.percent(v),
         live: s => s.gpu && s.gpu.measured ? s.gpu.util_pct : NaN},
        {metric: "gpu.vram_used", label: "GPU memory", format: v => Format.gib(v),
         live: s => s.gpu && s.gpu.measured ? s.gpu.vram_used_mib : NaN},
        {metric: "gpu.temp", label: "GPU temperature", format: v => Math.round(v) + " °C",
         live: s => s.gpu && s.gpu.measured ? s.gpu.temp_c : NaN},
        {metric: "gpu.power", label: "GPU power", format: v => Format.watts(v),
         live: s => s.gpu && s.gpu.power_mw > 0 ? s.gpu.power_mw : NaN},
        {metric: "cpu.util", label: "CPU load", format: v => Format.percent(v / 10),
         live: s => s.cpu.util_permille},
        {metric: "cpu.power", label: "CPU power", format: v => Format.watts(v),
         live: s => s.cpu.package_power_mw > 0 ? s.cpu.package_power_mw : NaN},
        {metric: "ram.used", label: "RAM", format: v => Format.gib(v),
         live: s => s.ram.used_mib},
        {metric: "system.power", label: "System power", format: v => Format.watts(v),
         live: s => s.system_power_mw > 0 ? s.system_power_mw : NaN},
        {metric: "net.rx", label: "Network in", format: v => Format.rate(v),
         live: s => s.net.reduce((n, a) => n + a.rx_bps, 0)},
        {metric: "net.tx", label: "Network out", format: v => Format.rate(v),
         live: s => s.net.reduce((n, a) => n + a.tx_bps, 0)}
    ]

    property var _seriesCall: null
    // Set once made: a window change before that (its first value) loads nothing of its own.
    property bool _complete: false
    property var _snapshotCall: null

    color: Theme.bg

    Accessible.role: Accessible.Pane
    Accessible.name: title

    Component.onCompleted: {
        CoreCall.subscribe(core, "telemetry", root, onTelemetry)
        _complete = true
        reload()
    }

    onWindowSChanged: {
        if (_complete)
            loadSeries()
    }
    onProblemChanged: {
        if (problem !== "")
            problemBanner.show(problem)
        else
            problemBanner.close()
    }

    Connections {
        target: root.core
        function onConnectionChanged() {
            if (root.core.connection === "ready" && !root.loaded)
                root.reload()
        }
    }

    function reload() {
        loadSnapshot()
        loadSeries()
        energy.reload()
    }

    // A failure's message as shown (it is the core's text).
    function failure(what, e) {
        return what + shown(String(e.message || e.code || ""), 200)
    }

    function loadSnapshot() {
        if (_snapshotCall)
            _snapshotCall.cancel()
        _snapshotCall = CoreCall.call(core, "telemetry.snapshot", {}, {
            owner: root,
            ok: out => {
                root.loaded = true
                root.problem = ""
                root.apply(out.structured)
            },
            error: e => { root.problem = root.failure("The hardware can't be read now: ", e) }
        })
    }

    function loadSeries() {
        if (_seriesCall)
            _seriesCall.cancel()
        const resolution = Hardware.resolutionFor(windowS)
        const step = resolution === "1s" ? 1000 : 60000
        const to = Date.now()
        const window = windowS
        _seriesCall = CoreCall.call(core, "telemetry.series", {
            metrics: chartSpecs.map(c => c.metric),
            from: new Date(to - windowS * 1000 + 2 * step).toISOString(),
            to: new Date(to).toISOString(),
            resolution: resolution
        }, {
            owner: root,
            ok: out => {
                if (window !== root.windowS)
                    return
                const series = (out.structured && out.structured.series) || []
                for (const s of series) {
                    const tile = root.chartTiles.find(t => t.metric === s.metric)
                    if (tile)
                        tile.series.load(JSON.stringify(s.points))
                }
            },
            error: e => { root.problem = root.failure("The hardware history can't be read now: ", e) }
        })
    }

    // A snapshot: the state shown, unless it is older than the one shown. `seq` is the topic's
    // for an event, none for a loaded snapshot.
    function apply(s, append, seq) {
        if (!s || !s.cpu)
            return
        if (snapshot && s.ts < snapshot.ts)
            return
        snapshot = s
        alerts.setAll(s.alerts || [], s.ts, seq)
        if (append) {
            const ts = Date.parse(s.ts)
            for (let i = 0; i < chartSpecs.length; ++i) {
                const tile = charts.itemAt(i)
                if (tile)
                    tile.series.append(ts, chartSpecs[i].live(s))
            }
        }
    }

    function onTelemetry(kind, seq, data) {
        if (kind === "resync_required") {
            alerts.resetSeqs()
            reload()
        } else if (kind === "snapshot") {
            apply(data, false, seq)
            loadSeries()
            energy.reload()
        } else if (data && data.kind === "sample") {
            apply(data.snapshot, true, seq)
        } else if (data && data.kind === "alert" && data.alert) {
            alerts.upsert(data.alert)
        } else if (data && data.kind === "alert_cleared") {
            alerts.clear(data.id, seq)
        }
    }

    // Untrusted text as shown (Core.shownText, P4a-R8); nothing once the core has gone.
    function shown(text, maxChars) {
        return core ? core.shownText(text, maxChars) : ""
    }

    // A part's basis in the snapshot's whole-system figure: "measured", "estimated", "unknown".
    function basis(part) {
        const p = snapshot ? (snapshot.system_parts || []).find(x => x.part === part) : null
        return p ? p.measured : "unknown"
    }

    function partsText(parts) {
        const names = {gpu: "GPU", cpu: "CPU", rest: "rest"}
        return parts.map(p => p.measured === "unknown"
                              ? (p.part === "rest" ? "rest of system not set" : (names[p.part] || p.part) + " unknown")
                              : (names[p.part] || p.part) + " " + Format.watts(p.mw) + " " + p.measured)
                    .join("  ·  ")
    }

    function gpuPowerText(g) {
        if (g.power_mw <= 0 || basis("gpu") === "unknown")
            return "unknown"
        return Format.watts(g.power_mw) + (basis("gpu") === "estimated" ? " (estimated)" : "")
    }

    function cpuPowerText(c) {
        if (c.package_power_mw <= 0 && !c.measured)
            return "unknown"
        return Format.watts(c.package_power_mw) + (c.measured ? "" : " (estimated)")
    }

    // A chart's estimate note.
    function noteFor(metric) {
        if (metric === "system.power")
            return " (estimated)"
        if (metric === "gpu.power")
            return basis("gpu") === "estimated" ? " (estimated)" : ""
        if (metric === "cpu.power")
            return cpu && !cpu.measured ? " (estimated)" : ""
        return ""
    }

    // A chart's scale top; 0 for its window's peak.
    function scaleFor(metric) {
        switch (metric) {
        case "gpu.util":
        case "gpu.temp":
            return 100
        case "cpu.util":
            return 1000
        case "gpu.vram_used":
            return gpu ? gpu.vram_total_mib : 0
        case "ram.used":
            return snapshot ? snapshot.ram.total_mib : 0
        default:
            return 0
        }
    }

    // Keeps the item with the keyboard focus in view.
    Connections {
        target: root.Window.window
        function onActiveFocusItemChanged() {
            const item = root.Window.window ? root.Window.window.activeFocusItem : null
            if (!item || !flick.contentItem)
                return
            for (let p = item; p; p = p.parent) {
                if (p === flick.contentItem) {
                    const y = item.mapToItem(flick.contentItem, 0, 0).y
                    if (y < flick.contentY)
                        flick.contentY = Math.max(0, y - Theme.spacing)
                    else if (y + item.height > flick.contentY + flick.height)
                        flick.contentY = Math.min(flick.contentHeight - flick.height,
                                                  y + item.height - flick.height + Theme.spacing)
                    return
                }
            }
        }
    }

    Flickable {
        id: flick
        anchors.fill: parent
        contentWidth: width
        contentHeight: body.implicitHeight + 2 * Theme.spacing
        clip: true
        boundsBehavior: Flickable.StopAtBounds

        QQC.ScrollBar.vertical: QQC.ScrollBar {}

        Column {
            id: body
            x: Theme.spacing
            y: Theme.spacing
            width: flick.width - 2 * Theme.spacing
            spacing: Theme.spacing

            LBanner {
                id: problemBanner
                width: parent.width
                kind: "warning"
                onClosed: root.problem = ""
            }

            AlertsStrip {
                id: alerts
                objectName: "alertsStrip"
                width: parent.width
                core: root.core
            }

            // The whole system: always an estimate.
            Row {
                spacing: Theme.spacing
                width: parent.width

                Text {
                    text: "Whole system"
                    color: Theme.textMuted
                    font.pixelSize: Theme.fontSize
                    anchors.baseline: systemPower.baseline
                }
                Text {
                    id: systemPower
                    objectName: "systemPower"
                    text: root.snapshot ? Format.watts(root.snapshot.system_power_mw) + " (estimated)" : "…"
                    color: Theme.text
                    font.pixelSize: Theme.fontSize + 4
                    font.bold: true
                }
                Text {
                    objectName: "systemParts"
                    anchors.baseline: systemPower.baseline
                    width: parent.width - x
                    text: root.snapshot ? root.partsText(root.snapshot.system_parts) : ""
                    color: Theme.textMuted
                    font.pixelSize: Theme.fontSize - 1
                    elide: Text.ElideRight
                }
            }

            Flow {
                width: parent.width
                spacing: Theme.spacing

                LCard {
                    id: gpuCard
                    objectName: "gpuCard"
                    width: 300
                    title: root.gpu ? "GPU · " + root.shown(String(root.gpu.name), 40) : "GPU"

                    Column {
                        width: parent.width
                        spacing: 2

                        Text {
                            visible: !root.gpu
                            text: root.snapshot ? "No GPU reading: no NVIDIA GPU was found, or it can't be read."
                                                : "…"
                            width: parent.width
                            wrapMode: Text.Wrap
                            color: Theme.textMuted
                            font.pixelSize: Theme.fontSize
                        }
                        Text {
                            visible: !!root.gpu && !root.gpu.measured
                            text: "The GPU was lost: this is its last reading."
                            width: parent.width
                            wrapMode: Text.Wrap
                            color: Theme.warning
                            font.pixelSize: Theme.fontSize
                        }
                        ValueRow {
                            visible: !!root.gpu
                            label: "Load"
                            value: root.gpu ? Format.percent(root.gpu.util_pct) : ""
                        }
                        ValueRow {
                            visible: !!root.gpu
                            label: "Memory"
                            value: root.gpu ? Format.gibOf(root.gpu.vram_used_mib, root.gpu.vram_total_mib) : ""
                        }
                        ValueRow {
                            visible: !!root.gpu
                            label: "Temperature"
                            value: root.gpu ? root.gpu.temp_c + " °C" : ""
                        }
                        ValueRow {
                            visible: !!root.gpu
                            label: "Power"
                            value: root.gpu ? root.gpuPowerText(root.gpu) : ""
                        }
                        ValueRow {
                            visible: !!root.gpu
                            label: "Core clock"
                            value: root.gpu ? Format.mhz(root.gpu.sm_clock_mhz) : ""
                        }
                        ValueRow {
                            visible: !!root.gpu
                            label: "Memory clock"
                            value: root.gpu ? Format.mhz(root.gpu.mem_clock_mhz) : ""
                        }
                        ValueRow {
                            visible: !!root.gpu
                            label: "Fan"
                            value: !root.gpu ? "" : root.gpu.fan_pct !== undefined ? Format.percent(root.gpu.fan_pct)
                                                                                   : "not reported"
                        }
                        ValueRow {
                            visible: !!root.gpu
                            label: "Throttling"
                            value: root.gpu ? Format.throttle(root.gpu.throttle) : ""
                        }
                    }
                }

                GpuUsers {
                    objectName: "gpuUsers"
                    width: 300
                    core: root.core
                    users: root.snapshot ? root.snapshot.gpu_users || [] : []
                }

                LCard {
                    objectName: "cpuCard"
                    width: 300
                    title: "CPU"

                    Column {
                        width: parent.width
                        spacing: 2

                        ValueRow {
                            visible: !!root.cpu
                            label: "Load"
                            value: root.cpu ? Format.percent(root.cpu.util_permille / 10) : ""
                        }
                        ValueRow {
                            visible: !!root.cpu
                            label: "Package power"
                            value: root.cpu ? root.cpuPowerText(root.cpu) : ""
                        }

                        // The per-core load strip.
                        Row {
                            id: cores
                            readonly property var load: root.cpu ? root.cpu.per_core_permille : []
                            width: parent.width
                            height: 28
                            spacing: 2

                            Repeater {
                                model: cores.load.length
                                delegate: Rectangle {
                                    required property int index
                                    readonly property real share: Math.max(0, Math.min(1, cores.load[index] / 1000))
                                    objectName: "coreBar"
                                    width: Math.max(2, (cores.width - (cores.load.length - 1) * cores.spacing) / cores.load.length)
                                    height: cores.height
                                    color: Theme.surfaceRaised
                                    Accessible.role: Accessible.ProgressBar
                                    Accessible.name: "Core " + (index + 1) + ": " + Format.percent(share * 100)

                                    Rectangle {
                                        width: parent.width
                                        height: parent.height * parent.share
                                        anchors.bottom: parent.bottom
                                        color: Theme.accent
                                    }
                                }
                            }
                        }
                    }
                }

                LCard {
                    objectName: "ramCard"
                    width: 300
                    title: "RAM"

                    Column {
                        width: parent.width
                        ValueRow {
                            visible: !!root.snapshot
                            label: "In use"
                            value: root.snapshot ? Format.gibOf(root.snapshot.ram.used_mib, root.snapshot.ram.total_mib) : ""
                        }
                    }
                }

                LCard {
                    objectName: "disksCard"
                    width: 300
                    title: "Disks"

                    Column {
                        width: parent.width
                        spacing: 2
                        // By count, so a sample updates the rows in place.
                        Repeater {
                            model: root.snapshot ? root.snapshot.disks.length : 0
                            delegate: ValueRow {
                                required property int index
                                readonly property var disk: root.snapshot ? root.snapshot.disks[index] || null : null
                                label: disk ? root.shown(String(disk.volume), 24) : ""
                                value: disk ? Format.gib(disk.free_mib) + " free" : ""
                            }
                        }
                        ValueRow {
                            visible: !!root.snapshot
                            label: "Workspace"
                            value: root.snapshot && root.snapshot.sizes.workspace_mib !== undefined
                                   ? Format.gib(root.snapshot.sizes.workspace_mib) : "not measured"
                        }
                        ValueRow {
                            visible: !!root.snapshot
                            label: "Models"
                            value: root.snapshot && root.snapshot.sizes.models_mib !== undefined
                                   ? Format.gib(root.snapshot.sizes.models_mib) : "not measured"
                        }
                    }
                }

                LCard {
                    objectName: "networkCard"
                    width: 300
                    title: "Network"

                    Column {
                        width: parent.width
                        spacing: 2
                        Text {
                            visible: !!root.snapshot && root.snapshot.net.length === 0
                            text: "No network adapter reported."
                            color: Theme.textMuted
                            font.pixelSize: Theme.fontSize
                        }
                        Repeater {
                            model: root.snapshot ? root.snapshot.net.length : 0
                            delegate: ValueRow {
                                required property int index
                                readonly property var adapter: root.snapshot ? root.snapshot.net[index] || null : null
                                label: adapter ? root.shown(String(adapter.adapter), 32) : ""
                                value: adapter ? Format.rate(adapter.rx_bps) + " in · " + Format.rate(adapter.tx_bps) + " out" : ""
                            }
                        }
                    }
                }
            }

            // The window.
            Row {
                spacing: Theme.spacing

                Text {
                    text: "History"
                    color: Theme.textMuted
                    font.pixelSize: Theme.fontSize
                    anchors.verticalCenter: parent.verticalCenter
                }

                Repeater {
                    model: root.windows
                    delegate: LButton {
                        required property var modelData
                        text: modelData.text
                        checked: root.windowS === modelData.s
                        primary: checked
                        Accessible.name: "Show the " + modelData.words
                        Accessible.checkable: true
                        Accessible.checked: checked
                        onClicked: root.windowS = modelData.s
                    }
                }
            }

            Flow {
                width: parent.width
                spacing: Theme.spacing

                Repeater {
                    id: charts
                    model: root.chartSpecs.length
                    onItemAdded: (index, item) => {
                        const tiles = root.chartTiles.slice()
                        tiles[index] = item
                        root.chartTiles = tiles
                    }
                    delegate: ChartTile {
                        required property int index
                        readonly property var spec: root.chartSpecs[index]
                        width: 240
                        metric: spec.metric
                        label: spec.label
                        note: root.noteFor(spec.metric)
                        format: spec.format
                        scaleMax: root.scaleFor(spec.metric)
                        windowS: root.windowS
                        windowText: root.windowWords
                    }
                }
            }

            EnergyTable {
                id: energy
                objectName: "energyTable"
                width: Math.min(parent.width, 720)
                core: root.core
                parts: root.snapshot ? root.snapshot.system_parts || [] : []
            }
        }
    }

    // A label and its value in a card.
    component ValueRow: Item {
        property string label
        property string value
        width: parent ? parent.width : 0
        height: valueText.implicitHeight

        Text {
            width: parent.width - valueText.implicitWidth - Theme.spacing
            text: parent.label
            color: Theme.textMuted
            font.pixelSize: Theme.fontSize
            elide: Text.ElideRight
            textFormat: Text.PlainText
        }
        Text {
            id: valueText
            anchors.right: parent.right
            text: parent.value
            color: Theme.text
            font.pixelSize: Theme.fontSize
            font.bold: true
            textFormat: Text.PlainText
        }
    }
}
