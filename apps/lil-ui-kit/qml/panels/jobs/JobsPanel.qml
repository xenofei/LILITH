// The Jobs panel (APP-114, APP-139, CORE-160, CORE-161; P4a-R40, P4a-R41): every job of the
// session's scope, newest first from `jobs.list`, kept live by the `jobs` topic (one
// JsonListModel), shown by lane: the running job, then the queued ones in queue order, then the
// rest, newest first. Each lane has a header (LaneHeader: its pause and its reasons, waiting for
// GPU memory, ComfyUI's outside jobs, and its QueueControls); each job a row (JobRow: live
// progress at most 10 times a second, its preview, its ETA, and its controls).
//
// - **Queue order** (Appendix G "Queue position"): the model's live inserts go to the top, so the
//   panel reads `jobs.list {order: "queue"}` itself, after a load, after a `job.move` and on every
//   `jobs` event of a job that is queued now or shown as queued, coalesced (one read for a burst,
//   at most one at a time). That read gives each queued job's place, the lanes' `QueueState`s,
//   and the jobs ahead of each queued job for its ETA (JobText.queuedEtaText).
// - **Lanes** come from that read and from the `{kind: "queue", state}` events on `jobs`, applied
//   as full states, a burst coalesced into one header update per frame.
// - **Outside jobs** come from `engines.status` and the `engines` topic (ComfyUI's entry).
// - **Caps:** `caps.get` is read at load and again, throttled to once a second, on
//   `caps` and `usage` events and when a job reaches a final status; the retry's cost chip shows
//   the share of the monthly cap from it.
// - **Writes** (run next or last, the cancels, retry, pause and resume) wait until the model has
//   loaded; their refusals show in the banner. A reload comes only from a snapshot or a
//   `resync_required` (the model's own, and the panel's reads with it), never from `ready`.
//
// Ctrl+Shift+P is the top bar's (Task 20): the panel's Pause calls the same `app/queue.*`.
import QtQuick
import QtQml.Models
import Lilith.Core
import Lilith.UiKit

Rectangle {
    id: root

    required property var core
    required property string title

    property alias model: jobsModel
    property alias view: list
    property alias banner: banner
    property alias emptyText: emptyText

    // lane -> QueueState.
    property var lanes: ({})
    // ComfyUI's prompts that the suite didn't submit: {running, queued}.
    property var outsideJobs: ({running: 0, queued: 0})
    // caps.get's view, or null.
    property var caps: null
    // lane -> its queued JobSummaries in queue order (the queue read); job_id -> its place.
    property var queueByLane: ({})
    property var queueRank: ({})
    // batch_id -> its unfinished jobs the panel knows of.
    property var batchSizes: ({})
    // Now, for elapsed times and ETAs, once a second.
    property real nowMs: Date.now()
    // The lane header updates applied (one per coalesced burst).
    property int laneUpdates: 0

    readonly property bool writable: jobsModel.loaded
    readonly property var laneOrder: ["gpu-heavy", "cpu"]

    property var _pendingLanes: ({})
    property bool _queueReading: false
    property bool _queueAgain: false
    property bool _capsReading: false

    color: Theme.surface

    Accessible.role: Accessible.Pane
    Accessible.name: title

    // --- Words for what the core names (an unknown name shows as it is). ---

    function kindLabel(kind) {
        const known = {"comfy.prompt": "ComfyUI prompt", "comfy.inventory": "ComfyUI inventory",
                       "comfy.install_node": "Node pack install", "test.job": "Test job",
                       "assistant_task": "Director task"}
        return known[kind] || kind || "Job"
    }

    function originLabel(origin) {
        if (!origin || !origin.surface)
            return ""
        switch (origin.surface) {
        case "panel": return origin.id ? "from the " + origin.id + " app" : "from a panel"
        case "mcp": return "from Claude Code"
        case "agent_session": return "from the assistant"
        case "cli": return "from the command line"
        case "core": return "from LILITH"
        default: return "from " + origin.surface
        }
    }

    // A held job's reason.
    function reasonLabel(reason) {
        const known = {manual: "Paused by you", fullscreen: "Paused while fullscreen",
                       restart: "Held after a restart"}
        return known[reason] || reason
    }

    // A lane's pause: "Paused by you", "Paused while fullscreen", "Paused by you and while
    // fullscreen", an unknown reason by its name.
    function pausedLabel(reasons) {
        const parts = (reasons || []).map(r => r === "manual" ? "by you"
                                               : r === "fullscreen" ? "while fullscreen" : "by " + r)
        if (parts.length === 0)
            return "Paused"
        const last = parts.pop()
        return "Paused " + (parts.length ? parts.join(", ") + " and " : "") + last
    }

    function laneLabel(lane) {
        return lane === "gpu-heavy" ? "GPU work" : lane === "cpu" ? "CPU work" : lane
    }

    function phaseLabel(phase) {
        const known = {loading_models: "Loading models", encoding: "Encoding", sampling: "Sampling",
                       decoding: "Decoding", saving: "Saving", ingesting: "Bringing in the outputs",
                       other: "Working"}
        return known[phase] || phase
    }

    // --- Rows. ---

    // The row of `jobId` that the view has made, or null. (A row's own `index` can lag behind a
    // re-sort, so places are read from the view.)
    function rowFor(jobId) {
        const kids = list.contentItem.children
        for (let i = 0; i < kids.length; ++i) {
            if (kids[i].jobId === jobId)
                return kids[i]
        }
        return null
    }

    // The header of `lane`.
    function laneHeader(lane) {
        const kids = list.headerItem ? list.headerItem.children : []
        for (let i = 0; i < kids.length; ++i) {
            if (kids[i].lane === lane)
                return kids[i]
        }
        return null
    }

    // Selects the job's row (a fix's "Show the job", show_job).
    function showJob(jobId) {
        let at = -1
        const made = rowFor(jobId)
        if (made) {
            at = list.indexAt(made.x + 1, made.y + 1)
        }
        if (at < 0) {
            const role = roleOf("job_id")
            for (let i = 0; i < sorted.rowCount() && at < 0; ++i) {
                if (sorted.data(sorted.index(i, 0), role) === jobId)
                    at = i
            }
        }
        if (at < 0)
            return false
        list.currentIndex = at
        list.positionViewAtIndex(at, ListView.Contain)
        list.forceActiveFocus(Qt.OtherFocusReason)
        return true
    }

    function roleOf(name) {
        return 0x0100 + jobsModel.roleNames.indexOf(name)
    }

    // The running job of `lane` among the rows the model holds, or null.
    function runningIn(lane) {
        for (let i = 0; i < jobsModel.rowCount(); ++i) {
            const j = jobsModel.get(i)
            if (j && j.status === "running" && j.lane === lane)
                return j
        }
        return null
    }

    // When the job's current run began executing (status_seq), else when it started; ms.
    function executingSince(job) {
        const seq = job.status_seq || []
        for (let i = seq.length - 1; i >= 0; --i) {
            if (seq[i].status === "running" && seq[i].substatus === "executing")
                return Date.parse(seq[i].at)
        }
        return job.started ? Date.parse(job.started) : NaN
    }

    // The seconds the job has run since it began executing.
    function elapsedS(job) {
        const since = executingSince(job)
        return isNaN(since) ? 0 : Math.max(0, (nowMs - since) / 1000)
    }

    // A queued job's finish (Appendix G): the running job's time left, the jobs ahead of it in
    // its lane (each place the panel can't see counted as a job with no estimate), and its own.
    function queuedEta(job) {
        const queue = queueByLane[job.lane] || []
        const at = queue.findIndex(q => q.job_id === job.job_id)
        if (at < 0)
            return ""
        const ahead = queue.slice(0, at).filter(q => q.queue_position !== undefined && q.queue_position !== null)
                           .map(q => q.estimate || null)
        const unseen = Math.max(0, (job.queue_position || 0) - ahead.length)
        for (let i = 0; i < unseen; ++i)
            ahead.push(null)
        let left = -1
        const running = runningIn(job.lane)
        if (running) {
            left = JobText.runningLeft(JSON.stringify(running.estimate || null), elapsedS(running))
            if (left < 0)
                ahead.unshift(null)
        }
        return JobText.queuedEtaText(left, JSON.stringify(ahead), JSON.stringify(job.estimate || null))
    }

    // --- Calls. ---

    function failed(what, error) {
        banner.kind = "danger"
        banner.show(what + ": " + (error && error.message ? error.message : "the core didn't answer."))
    }

    function callTool(tool, args, what, onOk) {
        if (!writable)
            return
        CoreCall.call(core, tool, args, {owner: root, ok: r => { if (onOk) onOk(r) },
                                         error: e => root.failed(what, e)})
    }

    function moveJob(jobId, to) {
        callTool("job.move", {job_id: jobId, to: to}, to === "next" ? "Couldn't run it next" : "Couldn't run it last",
                 () => root.requestQueueRead())
    }

    function cancelJob(jobId) {
        callTool("job.cancel", {job_id: jobId}, "Couldn't cancel the job")
    }

    function cancelBatch(batchId) {
        callTool("job.cancel", {batch_id: batchId}, "Couldn't cancel the batch")
    }

    function cancelAllMine() {
        callTool("job.cancel", {all_mine: true}, "Couldn't cancel your jobs")
    }

    function retryJob(jobId) {
        callTool("job.retry", {job_id: jobId}, "Couldn't retry the job")
    }

    function queueCall(method, params, what) {
        if (!writable)
            return
        CoreCall.appCall(core, method, params, {owner: root,
                                                ok: state => root.applyLanes([state]),
                                                error: e => root.failed(what, e)})
    }

    function pause(lane) {
        queueCall("app/queue.pause", {lane: lane}, "Couldn't pause " + laneLabel(lane))
    }

    function resume(lane) {
        queueCall("app/queue.resume", {lane: lane}, "Couldn't resume " + laneLabel(lane))
    }

    function resumeHeld(lane) {
        queueCall("app/queue.resume", {lane: lane, held_only: true}, "Couldn't resume the held jobs")
    }

    // Full lane states, applied as they are.
    function applyLanes(states) {
        const next = Object.assign({}, lanes)
        for (const s of states || []) {
            if (s && s.lane)
                next[s.lane] = s
        }
        lanes = next
        laneUpdates++
    }

    // A `{kind: "queue", state}` event: coalesced into one header update per frame.
    function queueEvent(state) {
        if (!state || !state.lane)
            return
        const pending = Object.assign({}, _pendingLanes)
        pending[state.lane] = state
        _pendingLanes = pending
        laneFrame.restart()
    }

    function requestQueueRead() {
        if (!queueCoalesce.running)
            queueCoalesce.start()
    }

    function readQueue() {
        if (!core)
            return
        if (_queueReading) {
            _queueAgain = true
            return
        }
        _queueReading = true
        CoreCall.call(core, "jobs.list", {order: "queue", limit: 500}, {owner: root,
            ok: r => {
                root._queueReading = false
                const s = r.structured || {}
                const byLane = {}
                const rank = {}
                ;(s.jobs || []).forEach((j, i) => {
                    if (!byLane[j.lane])
                        byLane[j.lane] = []
                    byLane[j.lane].push(j)
                    rank[j.job_id] = i
                })
                const batches = {}
                for (const j of s.jobs || [])
                    batches[j.batch_id] = (batches[j.batch_id] || 0) + 1
                for (let i = 0; i < jobsModel.rowCount(); ++i) {
                    const j = jobsModel.get(i)
                    if (j && j.status === "running")
                        batches[j.batch_id] = (batches[j.batch_id] || 0) + 1
                }
                root.queueByLane = byLane
                root.queueRank = rank
                root.batchSizes = batches
                root.applyLanes(s.lanes)
                queueSorter.invalidate()
                if (root._queueAgain) {
                    root._queueAgain = false
                    root.requestQueueRead()
                }
            },
            error: () => {
                root._queueReading = false
                retryQueue.start()
            }})
    }

    function readEngines() {
        CoreCall.call(core, "engines.status", {}, {owner: root,
            ok: r => root.applyEngines((r.structured || {}).engines)})
    }

    function applyEngines(engines) {
        for (const e of engines || []) {
            if (e && e.engine === "comfyui" && e.outside_jobs)
                outsideJobs = e.outside_jobs
        }
    }

    function readCaps() {
        if (_capsReading)
            return
        _capsReading = true
        CoreCall.call(core, "caps.get", {}, {owner: root,
            ok: r => {
                root._capsReading = false
                root.caps = r.structured || null
            },
            error: () => { root._capsReading = false }})
    }

    function requestCapsRead() {
        if (!capsThrottle.running)
            capsThrottle.start()
    }

    function onJobsEvent(kind, data) {
        if (kind !== "event") {
            // A snapshot or a resync: the model reloads; the panel reads its own state again.
            readQueue()
            readEngines()
            readCaps()
            return
        }
        if (!data)
            return
        if (data.kind === "queue") {
            queueEvent(data.state)
        } else if (data.kind === "job" && data.job) {
            const j = data.job
            if (j.status === "queued" || queueRank[j.job_id] !== undefined || j.status === "running")
                requestQueueRead()
            if (j.status === "succeeded" || j.status === "failed" || j.status === "cancelled")
                requestCapsRead()
        }
    }

    function start() {
        if (!core)
            return
        CoreCall.subscribe(core, "jobs", root, (kind, seq, data) => root.onJobsEvent(kind, data))
        CoreCall.subscribe(core, "engines", root, (kind, seq, data) => {
            if (kind === "snapshot")
                root.applyEngines(data && data.engines)
            else if (kind === "resync_required")
                root.readEngines()
            else if (data && data.engine)
                root.applyEngines([data.engine])
        })
        CoreCall.subscribe(core, "caps", root, () => root.requestCapsRead())
        CoreCall.subscribe(core, "usage", root, () => root.requestCapsRead())
        readQueue()
        readEngines()
        readCaps()
    }

    Component.onCompleted: start()

    JsonListModel {
        id: jobsModel
        core: root.core
        tool: "jobs.list"
        argsJson: "{}"
        listKey: "jobs"
        idKey: "job_id"
        topic: "jobs"
        eventKind: "job"
        rowKey: "job"
        roleNames: ["job_id", "kind", "lane", "priority", "status", "substatus", "created", "updated",
                    "started", "finished", "attempt", "progress", "error", "retry_of", "batch_id", "index",
                    "status_seq", "origin", "phase", "subtasks", "indeterminate", "unresponsive",
                    "pause_reasons", "estimate", "queue_position", "watchdog", "all_cached"]
    }

    // By lane, then running, queued in queue order, the rest newest first.
    SortFilterProxyModel {
        id: sorted
        model: jobsModel
        sorters: [
            FunctionSorter {
                id: queueSorter
                component RowData: QtObject {
                    property string job_id
                    property string lane
                    property string status
                    property string created
                }
                function compare(lhs: RowData, rhs: RowData): int {
                    const laneRank = l => { const i = root.laneOrder.indexOf(l); return i < 0 ? 9 : i }
                    const group = s => s === "running" ? 0 : s === "queued" ? 1 : 2
                    const keys = d => [laneRank(d.lane), group(d.status),
                                       d.status === "queued" && root.queueRank[d.job_id] !== undefined
                                           ? root.queueRank[d.job_id] : 1e9]
                    const a = keys(lhs), b = keys(rhs)
                    for (let i = 0; i < a.length; ++i) {
                        if (a[i] !== b[i])
                            return a[i] < b[i] ? -1 : 1
                    }
                    // Newest first.
                    return lhs.created > rhs.created ? -1 : lhs.created < rhs.created ? 1
                         : lhs.job_id < rhs.job_id ? -1 : lhs.job_id > rhs.job_id ? 1 : 0
                }
            }
        ]
    }

    Timer {
        interval: 1000
        repeat: true
        running: true
        onTriggered: root.nowMs = Date.now()
    }

    // One header update per frame for a burst of queue events.
    Timer {
        id: laneFrame
        interval: 16
        onTriggered: {
            const pending = root._pendingLanes
            root._pendingLanes = {}
            root.applyLanes(Object.keys(pending).map(k => pending[k]))
        }
    }

    // One queue read for a burst of job events.
    Timer {
        id: queueCoalesce
        interval: 150
        onTriggered: root.readQueue()
    }

    Timer {
        id: retryQueue
        interval: 2000
        onTriggered: root.readQueue()
    }

    // caps.get at most once a second.
    Timer {
        id: capsThrottle
        interval: 1000
        onTriggered: root.readCaps()
    }

    Connections {
        target: jobsModel
        function onErrorChanged() {
            if (jobsModel.error)
                root.failed("Couldn't load the jobs", jobsModel.error)
        }
    }

    LBanner {
        id: banner
        anchors { left: parent.left; right: parent.right; top: parent.top; margins: Theme.spacing }
        visible: false
    }

    ListView {
        id: list
        anchors {
            left: parent.left; right: parent.right; bottom: parent.bottom
            top: banner.visible ? banner.bottom : parent.top
            topMargin: Theme.spacing
        }
        clip: true
        model: sorted
        spacing: 4
        activeFocusOnTab: true
        currentIndex: -1
        boundsBehavior: Flickable.StopAtBounds

        Accessible.role: Accessible.List
        Accessible.name: root.title

        header: Column {
            id: headerColumn
            width: list.width
            spacing: Theme.spacing
            bottomPadding: Theme.spacing

            // The dock's title bar names the panel; this row holds the panel-wide control.
            Item {
                width: parent.width
                height: cancelAll.visible ? cancelAll.implicitHeight : 0
                LButton {
                    id: cancelAll
                    anchors { right: parent.right; rightMargin: Theme.spacing }
                    visible: list.count > 0
                    text: "Cancel all mine"
                    enabled: root.writable
                    onClicked: root.cancelAllMine()
                }
            }

            Repeater {
                model: root.laneOrder
                LaneHeader {
                    required property string modelData
                    width: headerColumn.width
                    panel: root
                    lane: modelData
                    queueState: root.lanes[modelData] || null
                    // The CPU lane shows only when there is something to say.
                    visible: modelData === "gpu-heavy" || (queueState && (queueState.paused || queueState.held > 0))
                }
            }
        }

        delegate: JobRow {
            width: list.width
            panel: root
        }

        Text {
            id: emptyText
            visible: jobsModel.loaded && list.count === 0
            anchors { left: parent.left; right: parent.right; margins: 2 * Theme.spacing }
            y: list.headerItem ? list.headerItem.height + Theme.spacing : 0
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            text: "No jobs yet. Jobs you, an agent or LILITH start show here."
            color: Theme.textMuted
            font.pixelSize: Theme.fontSize
        }
    }
}
