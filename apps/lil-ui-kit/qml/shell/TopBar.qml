// The top bar (APP-006): the app's name, the workspace and project switchers, and the indicators.
//
// - **Jobs** (TopBarJobs): "2 running · 3 queued", with a badge naming the reasons while a lane is
//   paused. The pause control is here: its button and Ctrl+Shift+P (togglePause, wired by the
//   shell) call `app/queue.pause` or `app/queue.resume {lane: "gpu-heavy"}`, as QueueControls does,
//   and wait for the first read. A refusal shows in `notice`.
// - **Spend** ("41 % of cap"): the highest `ratio_permille` of `caps.get` over the rows without
//   `invalid`, as a whole percent, and nothing at all when there are no caps. The `caps` topic says
//   nothing about spend between alerts, so `caps.get` is read again on `caps` and `usage` events
//   and when a job ends, at most once a second.
// - **GPU** ("GPU 63 % · 9.5/16 GiB · 71 °C"): the `telemetry` topic's samples, whose throttle is
//   Core's (APP-027). "GPU —" while there is no measured reading, or no connection to one.
// - **Connection:** `Core.connection` in words.
//
// The app switcher has only this app in M1 (APP-164: the plain name first, the proper name beneath
// it), so it is a label until there is a second. The assistant toggle shows, off and disabled.
//
// An action marked `requiresCore: true` is enabled only while `Core.connection` is `ready`
// (APP-034); these are bindings, so that holds within the frame the state changes.
import QtQuick
import QtQuick.Controls.Basic as QQC
import Lilith.Core
import Lilith.UiKit
import "../panels/hardware/HardwareFormat.js" as Format

Rectangle {
    id: root

    required property var core

    readonly property bool coreReady: !!core && core.connection === "ready"
    // telemetry's snapshot, null until the first sample.
    property var snapshot: null
    // caps.get's view, null until it is read.
    property var capsView: null
    // The highest ratio over the rows that can be measured, -1 with none.
    readonly property int capPermille: {
        let top = -1
        for (const row of capsView ? capsView.caps || [] : []) {
            if (!row.invalid && row.ratio_permille > top)
                top = row.ratio_permille
        }
        return top
    }
    readonly property var gpuLane: jobs.lanes["gpu-heavy"] || null
    readonly property bool yourPause: !!gpuLane && (gpuLane.reasons || []).indexOf("manual") >= 0
    readonly property string pausedText: {
        const paused = Object.keys(jobs.lanes).map(l => jobs.lanes[l]).filter(s => s.paused)
        if (paused.length === 0)
            return ""
        const reasons = []
        for (const state of paused) {
            for (const r of state.reasons || []) {
                if (reasons.indexOf(r) < 0)
                    reasons.push(r)
            }
        }
        return pausedLabel(reasons)
    }

    property alias jobs: jobs
    property alias notice: notice
    property alias appName: appName
    property alias workspaceSwitcher: workspaceSwitcher
    property alias projectSwitcher: projectSwitcher
    property alias jobsIndicator: jobsIndicator
    property alias pausedBadge: pausedBadge
    property alias pausedHolder: pausedHolder
    property alias pauseButton: pauseButton
    property alias spendIndicator: spendIndicator
    property alias gpuIndicator: gpuIndicator
    property alias connectionIndicator: connectionIndicator
    property alias assistantToggle: assistantToggle
    property alias assistantTip: assistantTip
    readonly property string assistantReason: "The assistant arrives in a later version"

    property var _pauseCall: null
    property bool _capsReading: false
    property bool _capsDue: false
    // What the bar's items leave of its width: all of it pushes the indicators to the right. When
    // they don't all fit they wrap to a next line instead, so none is out of reach.
    readonly property real _spare: {
        let used = 2 * bar.padding
        for (const item of [appName, workspaceSwitcher, projectSwitcher, jobsIndicator, pausedHolder,
                            pauseButton, spendIndicator, gpuIndicator, connectionIndicator, assistantHolder]) {
            if (item.visible)
                used += item.width + bar.spacing
        }
        return Math.max(0, Math.floor(bar.width - used) - 1)
    }

    implicitHeight: column.implicitHeight
    color: Theme.surface

    function pausedLabel(reasons) {
        const parts = reasons.map(r => r === "manual" ? "by you" : r === "fullscreen" ? "while fullscreen"
                                       : "by " + core.shownText(String(r), 40))
        if (parts.length === 0)
            return "Paused"
        const last = parts.pop()
        return "Paused " + (parts.length ? parts.join(", ") + " and " : "") + last
    }

    function jobsText() {
        if (!jobs.loaded)
            return "—"
        const queued = MoneyText.count(jobs.capped >= 0 ? jobs.capped : jobs.queued)
        return MoneyText.count(jobs.running) + " running · " + queued + (jobs.capped >= 0 ? "+" : "") + " queued"
    }

    function gpuText() {
        const g = coreReady && snapshot ? snapshot.gpu : null
        if (!g || !g.measured)
            return "GPU —"
        return "GPU " + Format.percent(g.util_pct) + " · " + Format.oneDecimal(g.vram_used_mib / 1024) + "/"
               + Format.oneDecimal(g.vram_total_mib / 1024) + " GiB · " + g.temp_c + " °C"
    }

    function connectionText() {
        switch (core ? core.connection : "") {
        case "ready": return "Connected"
        case "connecting": return "Connecting…"
        case "reconnecting": return "Reconnecting…"
        case "incompatible": return "Core incompatible"
        default: return "Core unavailable"
        }
    }

    function togglePause() {
        if (!coreReady || !jobs.loaded || _pauseCall)
            return
        const resume = yourPause
        _pauseCall = CoreCall.appCall(core, resume ? "app/queue.resume" : "app/queue.pause", {lane: "gpu-heavy"}, {
            owner: root,
            ok: state => {
                root._pauseCall = null
                root.jobs.applyLanes([state])
            },
            error: e => {
                root._pauseCall = null
                root.showNotice((resume ? "Couldn't resume GPU work: " : "Couldn't pause GPU work: ")
                                + root.core.shownText(e && e.message ? String(e.message) : "the core didn't answer.", 300),
                                "danger")
            },
            pending: () => { root._pauseCall = null }
        })
    }

    function showNotice(text, kind) {
        notice.kind = kind
        notice.show(text)
    }

    function readCaps() {
        if (_capsReading) {
            _capsDue = true
            return
        }
        _capsReading = true
        CoreCall.call(core, "caps.get", {}, {
            owner: root,
            ok: r => {
                root.capsView = r.structured || null
                root.capsRead()
            },
            error: () => root.capsRead()
        })
    }

    function capsRead() {
        _capsReading = false
        if (_capsDue) {
            _capsDue = false
            readCaps()
        }
    }

    function requestCapsRead() {
        if (!capsThrottle.running)
            capsThrottle.start()
    }

    function applySnapshot(s) {
        if (s && !(snapshot && s.ts < snapshot.ts))
            snapshot = s
    }

    function onTelemetry(kind, data) {
        if (kind === "resync_required")
            snapshot = null
        else if (kind === "snapshot")
            applySnapshot(data)
        else if (data && data.kind === "sample")
            applySnapshot(data.snapshot)
    }

    Component.onCompleted: {
        if (!core)
            return
        CoreCall.subscribe(core, "telemetry", root, (kind, seq, data) => root.onTelemetry(kind, data))
        CoreCall.subscribe(core, "caps", root, () => root.requestCapsRead())
        CoreCall.subscribe(core, "usage", root, () => root.requestCapsRead())
        readCaps()
    }

    TopBarJobs {
        id: jobs
        core: root.core
        onJobEnded: root.requestCapsRead()
    }

    Timer {
        id: capsThrottle
        interval: 1000
        onTriggered: root.readCaps()
    }

    Column {
        id: column
        width: parent.width

        Flow {
            id: bar
            width: parent.width
            padding: 4
            spacing: Theme.spacing

            Column {
                id: appName
                height: 28
                Accessible.role: Accessible.StaticText
                Accessible.name: "App: Image studio, ATHANOR"

                Text {
                    text: "Image studio"
                    textFormat: Text.PlainText
                    color: Theme.text
                    font.pixelSize: Theme.fontSize
                    font.bold: true
                }
                Text {
                    text: "ATHANOR"
                    textFormat: Text.PlainText
                    color: Theme.textMuted
                    font.pixelSize: Theme.fontSize - 3
                }
            }

            WorkspaceSwitcher {
                id: workspaceSwitcher
                width: implicitWidth
                height: 28
                core: root.core
                ready: root.coreReady
            }

            ProjectSwitcher {
                id: projectSwitcher
                width: implicitWidth
                height: 28
                core: root.core
                ready: root.coreReady && !!root.core.workspace
            }

            Item {
                width: root._spare
                height: 1
            }

            StatusIndicator {
                id: jobsIndicator
                text: root.jobsText()
                name: "Jobs: " + text
            }

            Item {
                id: pausedHolder
                visible: root.pausedText !== ""
                width: pausedBadge.width
                height: 28

                LBadge {
                    id: pausedBadge
                    anchors.verticalCenter: parent.verticalCenter
                    kind: "warning"
                    text: root.pausedText
                }
            }

            LButton {
                id: pauseButton
                property bool requiresCore: true
                height: 28
                enabled: root.coreReady && root.jobs.loaded
                text: root.yourPause ? "Resume GPU work" : "Pause GPU work"
                onClicked: root.togglePause()
            }

            StatusIndicator {
                id: spendIndicator
                visible: root.capPermille >= 0
                text: Math.floor(root.capPermille / 10) + " % of cap"
                name: "Spend this month: " + text
            }

            StatusIndicator {
                id: gpuIndicator
                text: root.gpuText()
            }

            StatusIndicator {
                id: connectionIndicator
                readonly property string state_: root.core ? root.core.connection : ""
                text: root.connectionText()
                name: "Core connection: " + text
                kind: state_ === "ready" ? "ok" : state_ === "reconnecting" || state_ === "connecting" ? "warning" : "danger"
            }

            Item {
                id: assistantHolder
                width: assistantToggle.width
                height: 28

                HoverHandler {
                    id: assistantHover
                }

                LToggle {
                    id: assistantToggle
                    anchors.verticalCenter: parent.verticalCenter
                    enabled: false
                    text: "Assistant"
                    Accessible.description: root.assistantReason
                }

                QQC.ToolTip {
                    id: assistantTip
                    visible: assistantHover.hovered
                    text: root.assistantReason
                    font.family: Theme.fontFamily
                    font.pixelSize: Theme.fontSize
                }
            }
        }

        LBanner {
            id: notice
            width: parent.width
        }
    }
}
