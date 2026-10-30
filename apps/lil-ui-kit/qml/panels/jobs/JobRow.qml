// A job's row in the Jobs panel (APP-114, APP-139, CORE-160, CORE-161): what it is and who
// started it, its status in words, and its controls.
//
// - **Running:** its phase, its progress, how long it has run and its time left (its estimate's
//   `wall_s`, never below 0), the "unresponsive" and slow flags, its live preview (PreviewImage),
//   and Cancel. Progress comes only on `job:<id>`, which the row follows while the job runs, and
//   is applied at most 10 times a second (leading edge: the first at once, then the newest
//   waiting one every 100 ms); a status event of the job drops the waiting one.
// - **Queued:** why it waits (a held job names its reasons, "Held after a restart" among them;
//   "Waiting for GPU memory"; a retry waiting on the earlier ComfyUI prompt it inherited), its
//   place ("#3 in queue", or "held" for a job a restart holds, which has none), its ETA, Run next,
//   Run last and Cancel.
// - **Ended:** done (and "Nothing changed" when every node was cached), failed with its message
//   and its fix (`retry_job`: the Retry button, labelled by the fix, with the cost and time chip
//   from the job's own estimate; `show_job`: "Show the job", which selects it; any other: its
//   label, which does nothing), or cancelled, with Retry.
// - Jobs of a batch with more than one unfinished job get "Cancel batch".
// Role ListItem, named "<what>, <status>"; a tap selects it.
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
    readonly property var job: ({job_id: model.job_id, lane: model.lane, status: model.status,
                                 estimate: model.estimate, queue_position: model.queue_position,
                                 status_seq: model.status_seq, started: model.started})
    readonly property var error: model.error || null
    readonly property var fixAction: error && error.fix && error.fix.ui_action ? error.fix.ui_action : null
    // A job a restart holds has no place in the queue.
    readonly property bool restartHeld: queued && (model.queue_position === undefined || model.queue_position === null)
                                        && (model.pause_reasons || []).indexOf("restart") >= 0
    readonly property bool movable: queued && ["waiting", "waiting_for_vram", "queue_paused"].indexOf(substatus) >= 0
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

    property alias preview: preview
    property alias chip: chip

    readonly property string titleText: {
        const origin = panel.originLabel(model.origin)
        return panel.kindLabel(model.kind) + (origin ? " · " + origin : "")
    }

    readonly property string statusText: {
        const now = panel.nowMs
        const parts = []
        if (running) {
            parts.push("Running")
            if (model.phase)
                parts.push(panel.phaseLabel(model.phase))
            if (progress && progress.max > 0)
                parts.push(progress.value + " of " + progress.max)
            if (model.unresponsive || liveUnresponsive)
                parts.push("unresponsive")
            if (model.watchdog === "slow" || liveSlow)
                parts.push("taking longer than usual")
            const elapsed = panel.elapsedS(job)
            parts.push(JobText.elapsedText(elapsed))
            const eta = JobText.etaText(JSON.stringify(model.estimate || null), elapsed)
            if (eta !== "—")
                parts.push(eta)
        } else if (queued) {
            parts.push("Queued")
            parts.push(waitText(now))
            parts.push(restartHeld ? "held"
                       : model.queue_position === undefined || model.queue_position === null ? ""
                       : "#" + (model.queue_position + 1) + " in queue")
            if (!restartHeld)
                parts.push(panel.queuedEta(job))
        } else if (status === "succeeded") {
            parts.push("Done")
            if (model.all_cached)
                parts.push("Nothing changed")
            const took = (Date.parse(model.finished) - Date.parse(model.started)) / 1000
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
            parts.push(status)
        }
        return parts.filter(p => p !== "").join(" · ")
    }

    // Why a queued job waits.
    function waitText(now) {
        switch (substatus) {
        case "queue_paused":
            return (model.pause_reasons || []).map(r => panel.reasonLabel(r)).join(", ") || "Paused"
        case "waiting_for_vram":
            return "Waiting for GPU memory"
        case "waiting":
            return "Waiting"
        case "queued_in_engine":
            return "In ComfyUI's queue"
        case "submitting": {
            // A retry (or a re-run) whose earlier prompt was withdrawn waits, in `submitting`,
            // until ComfyUI's old prompt leaves, asking every 2 s: past that, say so.
            const seq = model.status_seq || []
            const since = seq.length ? Date.parse(seq[seq.length - 1].at) : NaN
            const inherited = (model.retry_of || model.attempt > 1) && !isNaN(since) && now - since >= 2000
            return inherited ? "Waiting for an earlier ComfyUI prompt to finish" : "Submitting"
        }
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
    // A newer summary of the job carries its progress.
    readonly property var modelProgress: model.progress
    onModelProgressChanged: liveProgress = null
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
    Accessible.name: titleText + ", " + statusText

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

        PreviewImage {
            id: preview
            visible: preview.status === Image.Ready && root.running
            core: root.panel.core
            jobId: root.running ? root.jobId : ""
            width: parent.width
            height: visible ? 96 : 0
            horizontalAlignment: Image.AlignLeft
        }

        Flow {
            width: parent.width
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
                visible: root.queued || root.running
                text: "Cancel"
                enabled: root.panel.writable
                onClicked: root.panel.cancelJob(root.jobId)
            }
            LButton {
                visible: (root.queued || root.running) && (root.panel.batchSizes[root.model.batch_id] || 0) >= 2
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
