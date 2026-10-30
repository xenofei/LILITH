// A job's row in the Jobs panel (APP-114, APP-139, CORE-160, CORE-161; P4a-R40): what it is,
// who started it, its class and its lane when they aren't the usual (`user`, `gpu-heavy`), its
// status in words, and its controls. The same row shows a live job (LiveJobsModel, whose `place`,
// `eta_text`, `elapsed_text` and `batch_size` Rust works out) and a finished one (the panel's
// JsonListModel); nothing in it reads the clock.
//
// - **Running:** its substatus ("Retrying (1 of 3)", "Cancelling", "Outcome unknown" with its
//   error and no clock, any other by its name), its phase, its progress, how long it has run and
//   its time left, the "unresponsive" and slow flags, its live preview (PreviewImage), and
//   Cancel. Progress comes only on `job:<id>`, which the row follows while the job runs, and is
//   applied at most 10 times a second (leading edge: the first at once, then the newest waiting
//   one every 100 ms); a status event of the job drops the waiting one.
// - **Queued:** why it waits (a held job names its reasons, "Held after a restart" among them;
//   "Waiting for GPU memory"; a job the core has dispatched, which has no place: "Submitting", "In
//   ComfyUI's queue", or a retry waiting on the earlier ComfyUI prompt it inherited, as the core
//   says), its place from the queue read ("#3 in queue"; a job a restart holds has none), its ETA,
//   Run next, Run last and Cancel.
// - **A status this build doesn't know:** its name and substatus, with the live ones, and Cancel.
// - **Ended:** done (and "Nothing changed" when every node was cached), failed with its message
//   and its fix (`retry_job`: the Retry button, labelled by the fix, with the cost and time chip
//   from the job's own estimate; `show_job`: "Show the job", which selects it; any other: its
//   label, which does nothing), or cancelled, with Retry.
// - Jobs of a batch with more than one live job get "Cancel batch".
// Role ListItem, named "<what>, <status>" without the figures that tick (those are its
// description); a tap selects it.
import QtQuick
import Lilith.Core
import Lilith.UiKit

Rectangle {
    id: root

    // The JobsPanel.
    required property var panel
    // The row's roles.
    required property var model

    readonly property string jobId: model.job_id || ""
    readonly property string status: model.status || ""
    readonly property string substatus: model.substatus || ""
    readonly property bool running: status === "running"
    readonly property bool queued: status === "queued"
    readonly property bool ended: status === "succeeded" || status === "failed" || status === "cancelled"
    // Live: running, queued, or a status this build doesn't know (shown with the queue).
    readonly property bool active: status !== "" && !ended
    readonly property bool outcomeUnknown: running && substatus === "interrupted_unknown"
    readonly property var error: model.error || null
    readonly property var fixAction: error && error.fix && error.fix.ui_action ? error.fix.ui_action : null
    // A job a restart holds: the queue read lists it with no place.
    readonly property bool restartHeld: queued && (model.pause_reasons || []).indexOf("restart") >= 0
                                        && (model.place === undefined || model.place === null)
    // A restart-held job doesn't move until it is released (job.move leaves it held).
    readonly property bool movable: queued && !restartHeld
                                    && ["waiting", "waiting_for_vram", "queue_paused"].indexOf(substatus) >= 0
    readonly property bool retryable: (status === "failed" || status === "cancelled")
                                      && !(fixAction && fixAction.id === "show_job")
                                      && !(error && error.retry === "never")

    // The progress shown: the newest applied from job:<id>, else the row's own.
    property var liveProgress: null
    readonly property var progress: liveProgress || model.progress || null
    readonly property real progressValue: progress ? progress.value : 0
    // How many progress updates were applied (the 10 Hz check).
    property int progressApplied: 0
    property bool liveUnresponsive: false
    property bool liveSlow: false

    // The live preview, while it runs; the retry's cost chip, once its controls are in.
    readonly property var preview: previewLoader.item
    readonly property var chip: controls.item ? controls.item.chip : null
    // The controls come a frame or so after the row (made off the frame's critical path, so a
    // long list appearing never holds the GUI thread): a test waits for this.
    readonly property bool controlsReady: !hasControls || controls.status === Loader.Ready
    readonly property bool hasControls: active || retryable || (!!fixAction && fixAction.id === "show_job")

    readonly property string titleText: {
        const parts = [panel.kindLabel(model.kind)]
        parts.push(panel.originLabel(model.origin))
        parts.push(panel.classLabel(model.priority))
        if (model.lane && model.lane !== "gpu-heavy")
            parts.push(panel.laneLabel(model.lane))
        return parts.filter(p => p !== "").join(" · ")
    }

    // What the row says, without the figures that tick (its accessible name).
    readonly property string stateText: {
        const parts = []
        if (running) {
            parts.push("Running")
            parts.push(panel.runningLabel(substatus))
            if (outcomeUnknown && error && error.message)
                parts.push(error.message)
            if (model.phase && !outcomeUnknown)
                parts.push(panel.phaseLabel(model.phase))
            if (progress && progress.max > 0 && !outcomeUnknown)
                parts.push(progress.value + " of " + progress.max)
            if (model.unresponsive || liveUnresponsive)
                parts.push("unresponsive")
            if (model.watchdog === "slow" || liveSlow)
                parts.push("taking longer than usual")
        } else if (queued) {
            parts.push("Queued")
            parts.push(waitText())
            // The place is the queue read's, never the summary's own `queue_position`.
            if (!restartHeld && model.place !== undefined && model.place !== null)
                parts.push("#" + (model.place + 1) + " in queue")
        } else if (status === "succeeded") {
            parts.push("Done")
            if (model.all_cached)
                parts.push("Nothing changed")
            const took = (Date.parse(model.finished) - firstRun()) / 1000
            if (!isNaN(took))
                parts.push("in " + JobText.elapsedText(took))
        } else if (status === "failed") {
            parts.push("Failed")
            if (error && error.message)
                parts.push(error.message)
            if (error && error.fix && error.fix.user_hint)
                parts.push(error.fix.user_hint)
            if (fixAction && fixAction.id !== "retry_job" && fixAction.id !== "show_job")
                parts.push(fixAction.label)
        } else if (status === "cancelled") {
            parts.push("Cancelled")
        } else {
            // A status this build doesn't know, by its name (P4a-R40).
            parts.push(status)
            parts.push(substatus)
        }
        return parts.filter(p => p !== "").join(" · ")
    }

    // The figures that tick (worked out in Rust: LiveJobsModel's `elapsed_text` and `eta_text`).
    readonly property string timeText: [model.elapsed_text || "", model.eta_text || ""]
                                       .filter(p => p !== "").join(" · ")
    readonly property string statusText: [stateText, timeText].filter(p => p !== "").join(" · ")

    // When the job first ran (its first `running` step), else its `started`; ms.
    function firstRun() {
        const seq = model.status_seq || []
        for (let i = 0; i < seq.length; ++i) {
            if (seq[i].status === "running")
                return Date.parse(seq[i].at)
        }
        return Date.parse(model.started)
    }

    // Why a queued job waits.
    function waitText() {
        switch (substatus) {
        case "queue_paused":
            return (model.pause_reasons || []).map(r => panel.reasonLabel(r)).join(", ") || "Paused"
        case "waiting_for_vram":
            return "Waiting for GPU memory"
        case "waiting":
            return "Waiting"
        case "queued_in_engine":
            return "In ComfyUI's queue"
        case "submitting":
            // The core says when a retry waits for the earlier prompt it inherited.
            return model.waiting_for_earlier_prompt ? "Waiting for an earlier ComfyUI prompt to finish"
                                                    : "Submitting"
        default:
            return substatus
        }
    }

    // --- Live progress, at most 10 times a second. ---

    property var _waiting: null
    property var _subscription: null

    function applyProgress(p) {
        liveProgress = p
        progressApplied++
    }

    function offerProgress(p) {
        if (gate.running) {
            _waiting = p
        } else {
            applyProgress(p)
            gate.start()
        }
    }

    function follow() {
        if (_subscription) {
            _subscription.unsubscribe()
            _subscription = null
        }
        _waiting = null
        if (!running || !jobId || !panel.core)
            return
        _subscription = CoreCall.subscribe(panel.core, "job:" + jobId, root, (kind, seq, data) => {
            if (kind !== "event" || !data)
                return
            if (data.kind === "progress")
                root.offerProgress({value: data.value, max: data.max})
            else if (data.kind === "status" || data.kind === "retrying")
                root._waiting = null
            else if (data.kind === "unresponsive")
                root.liveUnresponsive = !!data.on
            else if (data.kind === "watchdog")
                root.liveSlow = data.watchdog === "slow"
        })
    }

    onRunningChanged: follow()
    onJobIdChanged: follow()
    Component.onCompleted: follow()
    // A newer summary of the job carries its progress; one made before the newest progress
    // event (a lower value of the same steps) doesn't take it back.
    readonly property var modelProgress: model.progress
    onModelProgressChanged: {
        const m = modelProgress
        if (liveProgress && m && m.max === liveProgress.max && m.value < liveProgress.value)
            return
        liveProgress = null
    }
    Connections {
        target: root
        function onSubstatusChanged() {
            root.liveUnresponsive = false
            root.liveSlow = false
        }
    }

    Timer {
        id: gate
        interval: 100
        onTriggered: {
            if (root._waiting) {
                root.applyProgress(root._waiting)
                root._waiting = null
                gate.start()
            }
        }
    }

    implicitHeight: column.implicitHeight + 2 * Theme.spacing
    radius: Theme.radius
    color: ListView.isCurrentItem ? Theme.surfaceRaised : Theme.bg
    border.color: ListView.isCurrentItem && ListView.view && ListView.view.activeFocus ? Theme.focus : Theme.surfaceRaised
    border.width: ListView.isCurrentItem ? Theme.focusWidth : 1

    Accessible.role: Accessible.ListItem
    // The ticking figures stay out of the name, so a focused row isn't read out every second.
    Accessible.name: titleText + ", " + stateText
    Accessible.description: timeText

    TapHandler {
        onTapped: root.ListView.view.currentIndex = root.ListView.view.indexAt(root.x + 1, root.y + 1)
    }

    Column {
        id: column
        x: Theme.spacing
        y: Theme.spacing
        width: parent.width - 2 * Theme.spacing
        spacing: 4

        Text {
            width: parent.width
            text: root.titleText
            color: Theme.text
            font.pixelSize: Theme.fontSize
            font.bold: true
            elide: Text.ElideRight
        }

        Text {
            width: parent.width
            text: root.statusText
            color: root.status === "failed" ? Theme.danger : Theme.textMuted
            font.pixelSize: Theme.fontSize
            wrapMode: Text.Wrap
        }

        // The progress bar, while it runs with measurable progress.
        Rectangle {
            visible: root.running && !!root.progress && root.progress.max > 0
            width: parent.width
            height: 4
            radius: 2
            color: Theme.surfaceRaised
            Rectangle {
                width: root.progress && root.progress.max > 0
                       ? parent.width * Math.min(1, root.progress.value / root.progress.max) : 0
                height: parent.height
                radius: 2
                color: Theme.accent
            }
        }

        Loader {
            id: previewLoader
            active: root.running
            width: parent.width
            height: item && item.visible ? 96 : 0
            sourceComponent: PreviewImage {
                visible: status === Image.Ready
                core: root.panel.core
                jobId: root.jobId
                horizontalAlignment: Image.AlignLeft
            }
        }

        Loader {
            id: controls
            active: root.hasControls
            asynchronous: true
            width: parent.width
            // The buttons' height while they load, so the row doesn't jump.
            height: item ? item.implicitHeight : (active ? 28 : 0)
            sourceComponent: Flow {
                property alias chip: chip
                width: controls.width
                spacing: Theme.spacing

                LButton {
                    visible: root.movable
                    text: "Run next"
                    enabled: root.panel.writable
                    onClicked: root.panel.moveJob(root.jobId, "next")
                }
                LButton {
                    visible: root.movable
                    text: "Run last"
                    enabled: root.panel.writable
                    onClicked: root.panel.moveJob(root.jobId, "last")
                }
                LButton {
                    visible: root.active
                    text: "Cancel"
                    enabled: root.panel.writable
                    onClicked: root.panel.cancelJob(root.jobId)
                }
                LButton {
                    visible: root.active && (root.model.batch_size || 0) >= 2
                    text: "Cancel batch"
                    enabled: root.panel.writable
                    onClicked: root.panel.cancelBatch(root.model.batch_id)
                }
                LButton {
                    visible: root.retryable
                    text: root.fixAction && root.fixAction.id === "retry_job" ? root.fixAction.label : "Retry"
                    enabled: root.panel.writable
                    onClicked: root.panel.retryJob(root.jobId)
                }
                CostChip {
                    id: chip
                    visible: root.retryable
                    estimate: root.model.estimate || null
                    caps: root.panel.caps
                }
                LButton {
                    visible: !!root.fixAction && root.fixAction.id === "show_job"
                    text: root.fixAction ? root.fixAction.label : ""
                    onClicked: root.panel.showJob(root.jobId)
                }
            }
        }
    }
}
