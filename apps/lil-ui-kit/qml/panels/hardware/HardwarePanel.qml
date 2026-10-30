// The Hardware panel (APP-127 to APP-129, APP-131, APP-132; P4a-R17): the live state from the
// `telemetry` topic, charts over a window of 15 minutes (where it opens) to 24 hours, energy by
// period and the alerts.
//
// - **State.** It follows `telemetry` (CoreCall) and loads `telemetry.snapshot`. Each `sample`
//   event carries the whole snapshot and replaces it, unless it is older than the one shown
//   (events can overtake one another); `alert` and `alert_cleared` change the alerts at once, and
//   the next sample's `alerts` are the state again. A `snapshot` of the topic is the state too,
//   and like `resync_required` it loads everything again. It never reloads on `ready`, but for a
//   first load that failed (the session wasn't connected yet).
// - **Charts.** One `telemetry.series` call loads every chart for the window, at
//   Hardware.resolutionFor(window) (1 s up to an hour, else 1 min), from `to` minus the window
//   plus one step, so a range never holds more than the tool's 3,600 points. Each sample appends
//   its values; the samples come at 1 Hz through Core's throttle (APP-027). Each chart keeps at
//   most a point per pixel (ChartSeries, in Rust).
// - **No CPU temperature** (APP-128): Windows doesn't give it without a driver.
// - A figure that isn't measured says so: "estimated", "unknown", "not reported", or "last
//   reading" for a GPU that was lost.
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

    // The charts: metric, label, value as text, the top of the scale (0: the window's peak) and
    // the live value in a snapshot (NaN for none).
    readonly property var chartSpecs: [
        {metric: "gpu.util", label: "GPU load", format: v => Format.percent(v), max: 100,
         live: s => s.gpu && s.gpu.measured ? s.gpu.util_pct : NaN},
        {metric: "gpu.vram_used", label: "GPU memory", format: v => Format.gib(v),
         max: gpu ? gpu.vram_total_mib : 0,
         live: s => s.gpu && s.gpu.measured ? s.gpu.vram_used_mib : NaN},
        {metric: "gpu.temp", label: "GPU temperature", format: v => Math.round(v) + " °C", max: 100,
         live: s => s.gpu && s.gpu.measured ? s.gpu.temp_c : NaN},
        {metric: "gpu.power", label: "GPU power", format: v => Format.watts(v), max: 0,
         live: s => s.gpu && s.gpu.power_mw > 0 ? s.gpu.power_mw : NaN},
        {metric: "cpu.util", label: "CPU load", format: v => Format.percent(v / 10), max: 1000,
         live: s => s.cpu.util_permille},
        {metric: "cpu.power", label: "CPU power", format: v => Format.watts(v), max: 0,
         live: s => s.cpu.package_power_mw > 0 ? s.cpu.package_power_mw : NaN},
        {metric: "ram.used", label: "RAM", format: v => Format.gib(v), max: snapshot ? snapshot.ram.total_mib : 0,
         live: s => s.ram.used_mib},
        {metric: "system.power", label: "Whole-system power", format: v => Format.watts(v), max: 0,
         live: s => s.system_power_mw > 0 ? s.system_power_mw : NaN},
        {metric: "net.rx", label: "Network in", format: v => Format.rate(v), max: 0,
         live: s => s.net.reduce((n, a) => n + a.rx_bps, 0)},
        {metric: "net.tx", label: "Network out", format: v => Format.rate(v), max: 0,
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
            error: e => { root.problem = "The hardware can't be read now: " + (e.message || e.code) }
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
            from: new Date(to - windowS * 1000 + step).toISOString(),
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
            error: e => { root.problem = "The hardware history can't be read now: " + (e.message || e.code) }
        })
    }

    // A snapshot: the state shown, unless it is older than the one shown.
    function apply(s, append) {
        if (!s || !s.cpu)
            return
        if (snapshot && s.ts < snapshot.ts)
            return
        snapshot = s
        alerts.setAll(s.alerts || [])
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
            reload()
        } else if (kind === "snapshot") {
            apply(data, false)
            loadSeries()
            energy.reload()
        } else if (data && data.kind === "sample") {
            apply(data.snapshot, true)
        } else if (data && data.kind === "alert" && data.alert) {
            alerts.upsert(data.alert)
        } else if (data && data.kind === "alert_cleared") {
            alerts.clear(data.id)
        }
    }

    // Untrusted text as shown (Core.shownText, P4a-R8); nothing once the core has gone.
    function shown(text, maxChars) {
        return core ? core.shownText(text, maxChars) : ""
    }

    function partsText(parts) {
        const names = {gpu: "GPU", cpu: "CPU", rest: "rest"}
        return parts.map(p => (names[p.part] || p.part) + (p.measured === "unknown" ? " unknown"
                                                           : " " + Format.watts(p.mw) + " " + p.measured))
                    .join("  ·  ")
    }

    function cpuPowerText(c) {
        if (c.package_power_mw <= 0 && !c.measured)
            return "unknown"
        return Format.watts(c.package_power_mw) + (c.measured ? "" : " (estimated)")
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
            }

            // The whole system.
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
                    text: root.snapshot ? Format.watts(root.snapshot.system_power_mw) : "…"
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
                        Repeater {
                            model: root.gpu ? [
                                ["Load", Format.percent(root.gpu.util_pct)],
                                ["Memory", Format.gibOf(root.gpu.vram_used_mib, root.gpu.vram_total_mib)],
                                ["Temperature", root.gpu.temp_c + " °C"],
                                ["Power", root.gpu.power_mw > 0 ? Format.watts(root.gpu.power_mw) : "unknown"],
                                ["Core clock", Format.mhz(root.gpu.sm_clock_mhz)],
                                ["Memory clock", Format.mhz(root.gpu.mem_clock_mhz)],
                                ["Fan", root.gpu.fan_pct !== undefined ? Format.percent(root.gpu.fan_pct) : "not reported"],
                                ["Throttling", Format.throttle(root.gpu.throttle)]
                            ] : []
                            delegate: ValueRow {}
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

                        Repeater {
                            model: root.cpu ? [
                                ["Load", Format.percent(root.cpu.util_permille / 10)],
                                ["Package power", root.cpuPowerText(root.cpu)]
                            ] : []
                            delegate: ValueRow {}
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
                        Repeater {
                            model: root.snapshot ? [["In use", Format.gibOf(root.snapshot.ram.used_mib, root.snapshot.ram.total_mib)]] : []
                            delegate: ValueRow {}
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
                        Repeater {
                            model: root.snapshot ? root.snapshot.disks.map(d => [root.shown(String(d.volume), 24), Format.gib(d.free_mib) + " free"])
                                                   .concat([
                                                       ["Workspace", root.snapshot.sizes.workspace_mib !== undefined ? Format.gib(root.snapshot.sizes.workspace_mib) : "not measured"],
                                                       ["Models", root.snapshot.sizes.models_mib !== undefined ? Format.gib(root.snapshot.sizes.models_mib) : "not measured"]
                                                   ]) : []
                            delegate: ValueRow {}
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
                            model: root.snapshot ? root.snapshot.net.map(n => [root.shown(String(n.adapter), 32),
                                                                              Format.rate(n.rx_bps) + " in · " + Format.rate(n.tx_bps) + " out"]) : []
                            delegate: ValueRow {}
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
                        format: spec.format
                        scaleMax: spec.max
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
            }
        }
    }

    // A label and its value in a card.
    component ValueRow: Item {
        required property var modelData
        width: parent ? parent.width : 0
        height: valueText.implicitHeight

        Text {
            width: parent.width - valueText.implicitWidth - Theme.spacing
            text: parent.modelData[0]
            color: Theme.textMuted
            font.pixelSize: Theme.fontSize
            elide: Text.ElideRight
            textFormat: Text.PlainText
        }
        Text {
            id: valueText
            anchors.right: parent.right
            text: parent.modelData[1]
            color: Theme.text
            font.pixelSize: Theme.fontSize
            font.bold: true
            textFormat: Text.PlainText
        }
    }
}
