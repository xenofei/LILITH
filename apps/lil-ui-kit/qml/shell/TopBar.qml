// The top bar (APP-006): the app's name, the workspace and project switchers, and the indicators.
//
// - **Jobs** (TopBarJobs): "2 running · 3 queued", with a badge naming each paused lane and its
//   reasons. The pause control is here: its button and Ctrl+Shift+P (togglePause, wired by the shell)
//   call `app/queue.pause` or `app/queue.resume {lane: "gpu-heavy"}`, as QueueControls does. They act
//   only on current state (TopBarJobs.current); a press that can't says why in `notice`, as does a
//   refusal, and a new attempt clears it.
// - **Spend** ("41 % of cap"): the highest `ratio_permille` of `caps.get` over the rows without
//   `invalid`, as a whole percent, and nothing at all when there are no caps. A cap whose value
//   doesn't parse counts as exceeded and calls are refused, so "Cap invalid" takes over from the
//   figure. The `caps` topic says nothing about spend between alerts, so `caps.get` is read again on
//   `caps` and `usage` events and when a job ends, at most once a second. A failed read is asked
//   again after 1 s, doubling up to 30 s while connected, and at once on `ready` while it has none.
// - **GPU** ("GPU 63 % · 9.5/16 GiB · 71 °C"): the `telemetry` topic's samples, whose throttle is
//   Core's (APP-027).
// - **Connection:** `Core.connection` in words. While connected, the core can still refuse this
//   session: another client moved the default (`lil workspace use`, an agent), and it serves only
//   the default. `movedReason` says so, from the `workspace` topic's `default_changed`, or from a
//   switcher's list read refused with `workspace.unsupported`; the connection banner and both
//   switchers then offer "Use the default workspace". A new hello ends it.
//
// Without a connection no figure is live: the GPU and the jobs read "—", the spend is muted, and the
// badge goes.
//
// The app switcher has only this app in M1 (APP-164: the plain name first, the proper name beneath
// it), so it is a label until there is a second. The assistant toggle shows, off and disabled; a tab
// stop around it says why, to the keyboard and to a screen reader.
import QtQuick
import QtQuick.Controls.Basic as QQC
import Lilith.Core
import Lilith.UiKit
import "../panels/hardware/HardwareFormat.js" as Format
import "../panels/jobs/PausedText.js" as PausedText

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
    readonly property bool capInvalid: !!capsView && (capsView.caps || []).some(row => row.invalid)
    readonly property var gpuLane: jobs.lanes["gpu-heavy"] || null
    readonly property bool yourPause: !!gpuLane && (gpuLane.reasons || []).indexOf("manual") >= 0
    // "GPU work paused by you", one for each paused lane.
    readonly property string pausedText: {
        const out = []
        for (const lane of Object.keys(jobs.lanes).sort()) {
            const state = jobs.lanes[lane]
            if (state.paused) {
                const words = PausedText.reasonWords(state.reasons, core)
                out.push(PausedText.laneLabel(lane, core) + " paused" + (words !== "" ? " " + words : ""))
            }
        }
        return out.join("; ")
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
    property alias assistantHolder: assistantHolder
    property alias assistantToggle: assistantToggle
    property alias assistantTip: assistantTip
    readonly property string assistantReason: "The assistant arrives in a later version"

    property var _pauseCall: null
    property bool _capsReading: false
    property bool _capsDue: false
    property bool _capsFailed: false
    property int _capsRetryMs: 0
    // What `notice` holds from the pause control: "wait" (a press had to wait; it goes when the wait
    // is over), "refusal" (it goes with the next attempt), else "" (not the pause control's: stays).
    property string _pauseNotice
    // See the header. Shown text; empty while the core serves this session.
    property string movedReason
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

    function jobsText() {
        if (!jobs.current)
            return "—"
        const queued = MoneyText.count(jobs.capped >= 0 ? jobs.capped : jobs.queued)
        return MoneyText.count(jobs.running) + " running · " + queued + (jobs.capped >= 0 ? "+" : "") + " queued"
    }

    function spendText() {
        if (capInvalid)
            return "Cap invalid"
        return capPermille >= 0 ? Math.floor(capPermille / 10) + " % of cap" : ""
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
        case "ready": return movedReason !== "" ? "Workspace refused" : "Connected"
        case "connecting": return "Connecting…"
        case "reconnecting": return "Reconnecting…"
        case "incompatible": return "Core incompatible"
        default: return "Core unavailable"
        }
    }

    function togglePause() {
        if (_pauseCall)
            return
        if (!coreReady || !jobs.current) {
            showNotice(coreReady ? "Wait a moment: the state of GPU work is still loading."
                                 : "GPU work can't be paused or resumed until the core is connected.", "warning")
            _pauseNotice = "wait"
            return
        }
        if (_pauseNotice !== "")
            notice.close()
        _pauseNotice = ""
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
                root._pauseNotice = "refusal"
            },
            pending: () => { root._pauseCall = null }
        })
    }

    function showNotice(text, kind) {
        _pauseNotice = ""
        notice.kind = kind
        notice.show(text)
    }

    function folderName(path) {
        const parts = String(path || "").split(/[\\/]/).filter(p => p !== "")
        return parts.length > 0 ? parts[parts.length - 1] : String(path || "")
    }

    function onWorkspaceEvent(kind, data) {
        if (kind !== "event" || !data || data.kind !== "default_changed" || !core.workspace)
            return
        if (String(data.id) === String(core.workspace.id))
            movedReason = ""
        else if (!workspaceSwitcher.changingDefault())
            movedReason = core.shownText(folderName(data.path), 60)
                          + " became the default workspace, and this core serves only the default, so it refuses this one's calls."
    }

    function readCaps() {
        if (!coreReady) {
            _capsFailed = true
            return
        }
        if (_capsReading) {
            _capsDue = true
            return
        }
        _capsReading = true
        CoreCall.call(core, "caps.get", {}, {
            owner: root,
            ok: r => {
                root.capsView = r.structured || null
                root._capsFailed = false
                root._capsRetryMs = 0
                root.capsRead()
            },
            error: () => {
                root._capsFailed = true
                if (root.coreReady) {
                    root._capsRetryMs = root._capsRetryMs === 0 ? 1000 : Math.min(root._capsRetryMs * 2, 30000)
                    capsRetry.interval = root._capsRetryMs
                    capsRetry.restart()
                }
                root.capsRead()
            }
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
        CoreCall.subscribe(core, "workspace", root, (kind, seq, data) => root.onWorkspaceEvent(kind, data))
        readCaps()
    }

    Connections {
        target: root.core
        ignoreUnknownSignals: true
        function onWorkspaceChanged() {
            root.movedReason = ""
        }
        function onConnectionChanged() {
            root.movedReason = ""
            capsRetry.stop()
            if (root.coreReady && (root._capsFailed || !root.capsView)) {
                root._capsRetryMs = 0
                root.readCaps()
            }
        }
    }

    Connections {
        target: jobs
        function onCurrentChanged() {
            if (jobs.current && root._pauseNotice === "wait") {
                root._pauseNotice = ""
                notice.close()
            }
        }
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

    Timer {
        id: capsRetry
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
                movedReason: root.movedReason
                onNoticed: (text, kind) => root.showNotice(text, kind)
                onWorkspaceRefused: message => { root.movedReason = root.core.shownText(message, 300) }
            }

            ProjectSwitcher {
                id: projectSwitcher
                width: implicitWidth
                height: 28
                core: root.core
                ready: root.coreReady && !!root.core.workspace
                movedReason: root.movedReason
                onWorkspaceRefused: message => { root.movedReason = root.core.shownText(message, 300) }
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
                visible: root.pausedText !== "" && root.jobs.current
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
                requiresCore: true
                height: 28
                enabled: root.jobs.current
                text: root.yourPause ? "Resume GPU work" : "Pause GPU work"
                onClicked: root.togglePause()
            }

            StatusIndicator {
                id: spendIndicator
                visible: root.spendText() !== ""
                text: root.spendText()
                name: root.capInvalid ? "Spend this month: a cap's value can't be read, so calls are refused"
                                      : "Spend this month: " + text
                kind: !root.coreReady ? "muted" : root.capInvalid ? "danger" : ""
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
                kind: state_ === "ready" ? (root.movedReason !== "" ? "danger" : "ok")
                      : state_ === "reconnecting" || state_ === "connecting" ? "warning" : "danger"
            }

            Item {
                id: assistantHolder
                width: assistantToggle.width
                height: 28
                activeFocusOnTab: true
                Accessible.role: Accessible.StaticText
                Accessible.name: "Assistant, off: " + root.assistantReason

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

                Rectangle {
                    anchors.fill: parent
                    visible: assistantHolder.activeFocus
                    color: "transparent"
                    radius: Theme.radius
                    border.color: Theme.focus
                    border.width: Theme.focusWidth
                }

                QQC.ToolTip {
                    id: assistantTip
                    visible: assistantHover.hovered || assistantHolder.activeFocus
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
