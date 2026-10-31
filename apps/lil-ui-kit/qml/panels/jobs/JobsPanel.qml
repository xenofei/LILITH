// The Jobs panel (APP-114, APP-139, CORE-160, CORE-161; P4a-R40, P4a-R41), in two lists:
//
// - **Running and queued** (`liveView`, over LiveJobsModel): the running jobs first, then the jobs
//   the core has dispatched (`submitting`, `queued_in_engine`), then the queued ones in the order
//   they will run, each lane after the other. Its rows come from the queue's own source (Appendix
//   G "Queue position"): every page of `jobs.list {status: "running"}`, `{order: "queue"}` and
//   `{status: "queued"}`, which alone lists the dispatched jobs, following `next_cursor`. They are
//   read after a load, after a `job.move` and on every `jobs` event of a job that is queued now or
//   that the list shows as queued, or that came, went or changed status, coalesced (150 ms, one
//   read at a time). Between reads the `jobs` events' jobs are applied as they are, those of a
//   frame in one pass with one sort and one set of model changes, so a burst (a batch being
//   cancelled) holds the GUI thread once, not once per event. The pages' JSON goes to Rust as it
//   came: the order, each job's place (the read's, never a summary's `queue_position`), its ETA
//   and its elapsed time are worked out there, and the 1 s clock updates only the rows whose times
//   read differently. Its header holds the lane headers
//   (LaneHeader: the pause and its reasons, the held jobs, waiting for GPU memory, ComfyUI's
//   outside jobs, and QueueControls) and "Cancel all from this app".
// - **Finished** (`historyView`): the jobs that ended, newest first, from one JsonListModel over
//   `jobs.list` with the `jobs` topic. Its pages hold every status (`jobs.list` filters on one
//   status only), so while the list is shorter than a screenful the panel asks for more pages, at
//   most 20 per load, and its header says what it knows: looking, none yet, or none in the newest
//   jobs it looked through. A job of a status this build doesn't know is shown with the live ones,
//   by its name, and stays there until an event ends it (P4a-R40). A live read stopped at its page
//   cap says how many queued jobs it shows.
//
// Each row is a JobRow. The lanes come from the read and from the `{kind: "queue", state}` events
// on `jobs`, applied as full states, at most one header update per frame; a read's lanes don't undo
// a state of that lane that came after the read began. Outside jobs come from `engines.status` and
// the `engines` topic. `caps.get` is read at load and again, at most once a second, on `caps` and
// `usage` events and when a job ends, for the retry's cost chip. Writes wait until both lists have
// loaded, and refusals show in the banner; Pause, Resume and Retry also wait for their own call,
// so a double-click can't undo itself. A snapshot or a `resync_required` of `jobs` reads everything
// again; `ready` never does.
//
// Everything from outside (messages, hints, fix labels, kinds, origins, classes, lanes, statuses,
// substatuses, phases, reasons) is shown through Core.shownText (P4a-R8), and every Text is plain,
// so markup shows as written and an <img> in it loads nothing (AI-222).
//
// showJob(id, focus) selects a job's row. A link can ask for it before the first read has returned
// (P4a-R21), so the panel remembers the job (`wantedJob`) and tries again after each read that loads
// rows, paging back through the history while it looks, until it lands, a new request replaces it,
// or the reads have finished without the row: then the banner says the job isn't in the list. The
// keyboard focus moves to the row only when asked, and not if it has moved on while the panel looked.
//
// Ctrl+Shift+P is the top bar's (Task 20): the panel's Pause calls the same `app/queue.*`.
//
// For later panels: a delegate's `required property int index` can lag behind a
// SortFilterProxyModel re-sort (Qt 6.12), so places are read from the view (`ListView.indexAt`).
import QtQuick
import QtQml.Models
import Lilith.Core
import Lilith.UiKit

Rectangle {
    id: root

    required property var core
    required property string title
    // The app whose own jobs "Cancel all from this app" cancels (its jobs' `origin.id`).
    property string app: "image"

    property alias model: jobsModel
    property alias live: live
    property alias liveView: liveList
    property alias historyView: historyList
    property alias banner: banner
    // The live list's "Nothing is running or queued." line (made in its header).
    property Item emptyText: null
    // The live list's "Showing the first N queued jobs" line (made in its header).
    property Item cappedNote: null
    // The live list's line while its last read failed (made in its header).
    property Item staleNote: null

    // lane -> QueueState.
    property var lanes: ({})
    // ComfyUI's prompts that the suite didn't submit: {running, queued}.
    property var outsideJobs: ({running: 0, queued: 0})
    // caps.get's view, or null.
    property var caps: null
    // The live rows' lanes, the lanes with a job waiting for GPU memory, and how many of the
    // live jobs this app's panels started (LiveJobsModel.overview).
    property var rowLanes: []
    property var vramLanes: []
    property int mine: 0
    // How many queued jobs the last read listed when it stopped at its page cap, else -1.
    property int capped: -1
    // The history pages asked for to fill the Finished list since its load (at most
    // `historyFillPages`): a first page can hold no finished job at all (a batch).
    property int _historyPages: 0
    readonly property int historyFillPages: 20
    // The lane header updates applied (at most one per frame).
    property int laneUpdates: 0
    // The passes of `jobs` events applied to the live rows (at most one per frame).
    property int jobPasses: 0
    // The Finished list's header while it has no row: still looking, or the jobs it looked through
    // when the page limit stopped it (-1: it saw them all).
    property bool _historySearching: true
    property int _historyCutAt: -1
    // A page asked for to fill the Finished list is out.
    property bool _historyAsked: false
    readonly property string historyHeader: {
        if (historyList.count > 0)
            return "Finished"
        if (_historySearching)
            return "Looking for finished jobs…"
        if (_historyCutAt >= 0)
            return "No finished job in the newest " + grouped(_historyCutAt) + " jobs."
        return "No finished jobs yet."
    }
    // The lanes (lane -> true) with a call of theirs out, and the jobs with a retry out.
    property var busyLanes: ({})
    property var _laneAnswered: ({})
    property var busyRetries: ({})
    // The job showJob was asked for that isn't in the lists yet (empty: none), and whether
    // landing on it may take the keyboard focus.
    property string wantedJob
    property bool _wantedFocus: false
    // The banner is saying a job isn't in the list.
    property bool _saysNotListed: false

    readonly property bool writable: jobsModel.loaded && live.loaded
    readonly property var knownLanes: ["gpu-heavy", "cpu"]
    readonly property var headerLanes: {
        const out = knownLanes.slice()
        for (const l of rowLanes.concat(Object.keys(lanes))) {
            if (out.indexOf(l) < 0)
                out.push(l)
        }
        return out
    }

    // How much of each kind of outside text is shown (Core.shownText).
    readonly property int nameChars: 60
    readonly property int messageChars: 300

    property var _pendingLanes: ({})
    // The `jobs` events' jobs waiting for this frame's pass.
    property var _pendingJobs: []
    // The lanes with a state newer than the read that is out.
    property var _newerLanes: ({})
    property bool _liveReading: false
    // The last read failed: the rows may be old, and it is asked again.
    property bool _liveReadFailed: false
    property bool _liveAgain: false
    // The read asked for again must carry nothing over (a snapshot or a resync came).
    property bool _liveFreshAgain: false
    property bool _capsReading: false
    // Request id -> {ok, fail}: the calls whose answer goes to Rust as it came.
    property var _raw: ({})

    color: Theme.surface

    Accessible.role: Accessible.Pane
    Accessible.name: title

    // --- Words for what the core names (an unknown name shows as it is, P4a-R40). ---

    // Outside text as shown (Core.shownText, P4a-R8): at most `maxChars`, a name's length by default.
    function shown(text, maxChars) {
        return core && text !== undefined && text !== null ? core.shownText(String(text), maxChars || nameChars) : ""
    }

    // `name`'s word in `known`, or the name itself as shown. `name` comes from outside.
    function word(known, name) {
        return Object.prototype.hasOwnProperty.call(known, name) ? known[name] : shown(name)
    }

    // A count with its thousands marked.
    function grouped(n) {
        return String(n).replace(/\B(?=(\d{3})+(?!\d))/g, ",")
    }

    function kindLabel(kind) {
        const known = {"comfy.prompt": "ComfyUI prompt", "comfy.inventory": "ComfyUI inventory",
                       "comfy.install_node": "Node pack install", "test.job": "Test job",
                       "assistant_task": "Director task"}
        return kind ? word(known, kind) : "Job"
    }

    function originLabel(origin) {
        if (!origin || !origin.surface)
            return ""
        switch (origin.surface) {
        case "panel": return origin.id ? "from the " + shown(origin.id) + " app" : "from a panel"
        case "mcp": return "from Claude Code"
        case "agent_session": return "from the assistant"
        case "cli": return "from the command line"
        case "core": return "from LILITH"
        default: return "from " + shown(origin.surface)
        }
    }

    // The class, when it isn't your own generations' (`user`).
    function classLabel(priority) {
        const known = {user: "", interactive: "interactive", agent: "agent batch", live: "live"}
        return priority ? word(known, priority) : ""
    }

    // A held job's reason.
    function reasonLabel(reason) {
        const known = {manual: "Paused by you", fullscreen: "Paused while fullscreen",
                       restart: "Held after a restart"}
        return word(known, reason)
    }

    // A lane's pause: "Paused by you", "Paused while fullscreen", "Paused by you and while
    // fullscreen", an unknown reason by its name.
    function pausedLabel(reasons) {
        const parts = (reasons || []).map(r => r === "manual" ? "by you"
                                               : r === "fullscreen" ? "while fullscreen" : "by " + shown(r))
        if (parts.length === 0)
            return "Paused"
        const last = parts.pop()
        return "Paused " + (parts.length ? parts.join(", ") + " and " : "") + last
    }

    function laneLabel(lane) {
        return lane === "gpu-heavy" ? "GPU work" : lane === "cpu" ? "CPU work" : shown(lane)
    }

    function phaseLabel(phase) {
        const known = {loading_models: "Loading models", encoding: "Encoding", sampling: "Sampling",
                       decoding: "Decoding", saving: "Saving", ingesting: "Bringing in the outputs",
                       other: "Working"}
        return word(known, phase)
    }

    // A running job's substatus: nothing for `executing`, else its words or its name.
    function runningLabel(substatus) {
        if (!substatus || substatus === "executing")
            return ""
        const retrying = /^retrying_(\d+)_of_(\d+)$/.exec(substatus)
        if (retrying)
            return "Retrying (" + retrying[1] + " of " + retrying[2] + ")"
        const known = {cancelling: "Cancelling", interrupted_unknown: "Outcome unknown",
                       ingesting: "Bringing in the outputs"}
        return word(known, substatus)
    }

    // --- Rows. ---

    // The row of `jobId` that a view has made, or null.
    function rowFor(jobId) {
        for (const view of [liveList, historyList]) {
            const kids = view.contentItem.children
            for (let i = 0; i < kids.length; ++i) {
                if (kids[i].jobId === jobId)
                    return kids[i]
            }
        }
        return null
    }

    // The header of `lane`.
    function laneHeader(lane) {
        const kids = liveList.headerItem ? liveList.headerItem.children : []
        for (let i = 0; i < kids.length; ++i) {
            if (kids[i].lane === lane)
                return kids[i]
        }
        return null
    }

    // Selects the job's row (a fix's "Show the job", show_job; a `task` link, P4a-R21), and moves
    // the keyboard focus to it unless `focus` is false. A job that isn't listed yet is looked for
    // (`wantedJob`): the answer is whether it has landed now.
    function showJob(jobId, focus) {
        wantedJob = jobId
        _wantedFocus = focus !== false
        if (_saysNotListed) {
            _saysNotListed = false
            banner.close()
        }
        if (landOnJob(jobId, _wantedFocus)) {
            wantedJob = ""
            return true
        }
        _historyPages = 0
        fillHistory()
        return false
    }

    function landOnJob(jobId, takeFocus) {
        let view = liveList
        let at = live.rowOf(jobId)
        if (at < 0) {
            view = historyList
            const made = rowFor(jobId)
            if (made && made.ListView.view === historyList)
                at = historyList.indexAt(made.x + 1, made.y + 1)
            const role = 0x0100 + jobsModel.roleNames.indexOf("job_id")
            for (let i = 0; i < history.rowCount() && at < 0; ++i) {
                if (history.data(history.index(i, 0), role) === jobId)
                    at = i
            }
        }
        if (at < 0)
            return false
        const other = view === liveList ? historyList : liveList
        other.currentIndex = -1
        view.currentIndex = at
        view.positionViewAtIndex(at, ListView.Contain)
        if (takeFocus)
            view.forceActiveFocus(Qt.OtherFocusReason)
        return true
    }

    // Lands on the wanted job if a read has brought its row. The focus goes along only if it is
    // still in this panel, or nowhere: the person may have gone on while the panel looked.
    function tryWantedJob() {
        if (wantedJob === "")
            return
        let focused = root.Window.activeFocusItem
        while (focused && focused !== root)
            focused = focused.parent
        const focusStayed = !root.Window.activeFocusItem || focused === root
        if (landOnJob(wantedJob, _wantedFocus && focusStayed))
            wantedJob = ""
    }

    // The reads have finished without the wanted job.
    function wantedJobNotListed() {
        const id = shown(wantedJob)
        wantedJob = ""
        _saysNotListed = true
        banner.kind = "warning"
        banner.show("Job " + id + (_historyCutAt >= 0 ? " isn't in the newest " + grouped(_historyCutAt) + " jobs."
                                                      : " isn't in the list."))
    }

    // --- Calls. ---

    function failed(what, error) {
        _saysNotListed = false
        banner.kind = "danger"
        banner.show(what + ": " + (error && error.message ? shown(error.message, messageChars)
                                                          : "the core didn't answer."))
    }

    function callTool(tool, args, what, onOk) {
        if (!writable)
            return
        CoreCall.call(core, tool, args, {owner: root, ok: r => { if (onOk) onOk(r) },
                                         error: e => root.failed(what, e)})
    }

    function moveJob(jobId, to) {
        callTool("job.move", {job_id: jobId, to: to}, to === "next" ? "Couldn't run it next" : "Couldn't run it last",
                 () => root.requestLiveRead())
    }

    function cancelJob(jobId) {
        callTool("job.cancel", {job_id: jobId}, "Couldn't cancel the job")
    }

    function cancelBatch(batchId) {
        callTool("job.cancel", {batch_id: batchId}, "Couldn't cancel the batch")
    }

    function cancelAllMine() {
        callTool("job.cancel", {all_mine: true}, "Couldn't cancel this app's jobs", r => {
            const jobs = (r.structured || {}).jobs || []
            const jobsWord = jobs.length === 1 ? "1 job." : jobs.length + " jobs."
            banner.kind = "ok"
            // A running job only begins to cancel: its row says "Cancelling" until it stops.
            banner.show((jobs.every(j => j.status === "cancelled") ? "Cancelled " : "Cancelling or cancelled ")
                        + jobsWord)
        })
    }

    function retryJob(jobId) {
        if (!writable || busyRetries[jobId])
            return
        const done = () => {
            const busy = Object.assign({}, root.busyRetries)
            delete busy[jobId]
            root.busyRetries = busy
        }
        const busy = Object.assign({}, busyRetries)
        busy[jobId] = true
        busyRetries = busy
        CoreCall.call(core, "job.retry", {job_id: jobId}, {owner: root, ok: done,
                                                           error: e => {
                                                               done()
                                                               root.failed("Couldn't retry the job", e)
                                                           }})
    }

    // One call at a time for a lane, and none for a double-click's length after an answer: the
    // core answers at once, so without that a double-click of Pause pauses and then resumes,
    // withdrawing the prompts on the way. The control stays enabled, so it keeps the focus.
    function queueCall(method, params, what) {
        if (!writable || busyLanes[params.lane])
            return
        const answered = _laneAnswered[params.lane]
        if (answered !== undefined && Date.now() - answered < Qt.styleHints.mouseDoubleClickInterval)
            return
        const done = () => {
            const busy = Object.assign({}, root.busyLanes)
            delete busy[params.lane]
            root.busyLanes = busy
            root._laneAnswered[params.lane] = Date.now()
        }
        const busy = Object.assign({}, busyLanes)
        busy[params.lane] = true
        busyLanes = busy
        CoreCall.appCall(core, method, params, {owner: root,
                                                ok: state => {
                                                    done()
                                                    root.laneStateCame(state)
                                                    root.applyLanes([state])
                                                },
                                                pending: done,
                                                error: e => {
                                                    done()
                                                    delete root._laneAnswered[params.lane]
                                                    root.failed(what, e)
                                                }})
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

    // A state of a lane that the read now out may be older than.
    function laneStateCame(state) {
        if (_liveReading && state && state.lane)
            _newerLanes[state.lane] = true
    }

    // A `{kind: "queue", state}` event: at most one header update per frame.
    function queueEvent(state) {
        if (!state || !state.lane)
            return
        laneStateCame(state)
        const pending = Object.assign({}, _pendingLanes)
        pending[state.lane] = state
        _pendingLanes = pending
        if (!laneFrame.running)
            laneFrame.start()
    }

    // A call whose result JSON goes on as it came (`ok(json)`), or its error (`fail(error)`).
    function rawCall(tool, args, ok, fail) {
        const id = core.call(tool, JSON.stringify(args))
        _raw[id] = {ok: ok, fail: fail}
    }

    function requestLiveRead() {
        if (!liveCoalesce.running)
            liveCoalesce.start()
    }

    // One read of the live rows: every page of the running jobs, of the queue and of the queued
    // jobs, then one commit. A `fresh` read (after a snapshot or a resync) carries nothing over.
    // The events waiting for this frame's pass go first: they are older than the read.
    function readLive(fresh) {
        if (!core)
            return
        if (_liveReading) {
            _liveAgain = true
            _liveFreshAgain = _liveFreshAgain || !!fresh
            return
        }
        applyPendingJobs()
        _liveReading = true
        _newerLanes = {}
        live.beginRead(!!fresh)
        let open = 3
        let gone = false
        const done = () => {
            if (gone || --open > 0)
                return
            // A job dispatched between the listings has its event waiting for its frame: without
            // it, the commit would drop the row and the next frame would make a new one.
            root.applyPendingJobs()
            live.commit(Date.now())
            root._liveReading = false
            root._liveReadFailed = false
            root.overviewChanged()
            const newer = root._newerLanes
            root.applyLanes(JSON.parse(live.lanesJson()).filter(s => !newer[s.lane]))
            if (root.wantedJob !== "")
                root.fillHistory()
            if (root._liveAgain) {
                const again = root._liveFreshAgain
                root._liveAgain = false
                root._liveFreshAgain = false
                if (again)
                    root.readLive(true)
                else
                    root.requestLiveRead()
            }
        }
        const fail = () => {
            if (gone)
                return
            gone = true
            root._liveReading = false
            root._liveReadFailed = true
            retryLive.start()
        }
        // A listing, each page after the page before.
        const list = (args, add) => {
            const page = cursor => rawCall("jobs.list", Object.assign({limit: 100}, args, cursor ? {cursor: cursor} : {}),
                                           json => {
                if (gone)
                    return
                const next = add(json)
                if (next !== "")
                    page(next)
                else
                    done()
            }, fail)
            page("")
        }
        list({status: "running"}, json => live.addRunningPage(json))
        list({order: "queue"}, json => live.addQueuePage(json))
        list({status: "queued"}, json => live.addQueuedPage(json))
    }

    function overviewChanged() {
        const o = JSON.parse(live.overview(app))
        if (JSON.stringify(o.lanes) !== JSON.stringify(rowLanes))
            rowLanes = o.lanes
        if (JSON.stringify(o.waiting_for_vram) !== JSON.stringify(vramLanes))
            vramLanes = o.waiting_for_vram
        mine = o.mine
        capped = o.capped === null || o.capped === undefined ? -1 : o.capped
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
            // A snapshot or a resync: the history reloads itself; the panel reads its own state
            // afresh, and the reload hands back the unknown-status jobs the core still lists.
            readLive(true)
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
            _pendingJobs.push(j)
            if (!jobFrame.running)
                jobFrame.start()
            if (j.status === "succeeded" || j.status === "failed" || j.status === "cancelled")
                requestCapsRead()
        }
    }

    // The events' jobs of this frame, in one pass: one sort and one set of model changes.
    function applyPendingJobs() {
        jobFrame.stop()
        if (_pendingJobs.length === 0)
            return
        const jobs = JSON.stringify(_pendingJobs)
        _pendingJobs = []
        jobPasses++
        const change = live.applyJobs(jobs, Date.now())
        if (change > 0)
            overviewChanged()
        if (change === 2)
            requestLiveRead()
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
        readLive()
        readEngines()
        readCaps()
    }

    Component.onCompleted: start()
    Component.onDestruction: {
        for (const id in _raw) {
            try {
                core.cancel(id)
            } catch (e) {
                // The core object went first.
            }
        }
        _raw = {}
    }

    Connections {
        target: root.core
        ignoreUnknownSignals: true
        function onCallFinished(id, json) {
            const h = root._raw[id]
            if (!h)
                return
            delete root._raw[id]
            h.ok(json)
        }
        function onCallFailed(id, json) {
            const h = root._raw[id]
            if (!h)
                return
            delete root._raw[id]
            h.fail(JSON.parse(json))
        }
    }

    LiveJobsModel {
        id: live
    }

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

        onLoadedChanged: {
            root._historyPages = 0
            root._historyAsked = false
            root._historySearching = true
            root._historyCutAt = -1
            if (!loaded)
                return
            root.seedUnknown(0, rowCount() - 1)
            Qt.callLater(root.fillHistory)
        }
        onRowsInserted: (parent, first, last) => {
            root.seedUnknown(first, last)
            Qt.callLater(root.fillHistory)
        }
        onPageLoaded: {
            root._historyAsked = false
            Qt.callLater(root.fillHistory)
        }
    }

    // A job of a status this build doesn't know shows with the live ones (P4a-R40).
    function seedUnknown(first, last) {
        const role = 0x0100 + jobsModel.roleNames.indexOf("status")
        const known = ["queued", "running", "succeeded", "failed", "cancelled"]
        for (let i = first; i <= last; ++i) {
            if (known.indexOf(jobsModel.data(jobsModel.index(i, 0), role)) < 0
                    && live.applyJob(JSON.stringify(jobsModel.get(i)), Date.now()) > 0)
                overviewChanged()
        }
    }

    // The history is newest first and holds every status, so its first pages can hold no
    // finished job (a batch queued after them): ask for more while the Finished list is shorter
    // than a screenful, at most `historyFillPages` pages per load, and say what came of it. While
    // a wanted job isn't in either list, it is looked for the same way, past the screenful: every
    // read that loads rows comes through here. With no more to look through, it isn't listed.
    function fillHistory() {
        tryWantedJob()
        if (!jobsModel.loaded || _historyAsked)
            return
        const seeking = wantedJob !== "" && live.loaded
        const top = jobsModel.index(-1, 0)
        if ((historyList.count >= Math.max(20, Math.ceil(historyList.height / 40)) && !seeking)
                || !jobsModel.canFetchMore(top)) {
            _historySearching = false
            _historyCutAt = -1
            if (seeking)
                wantedJobNotListed()
            return
        }
        if (_historyPages >= historyFillPages) {
            _historySearching = false
            _historyCutAt = jobsModel.rowCount()
            if (seeking)
                wantedJobNotListed()
            return
        }
        _historyPages++
        _historyAsked = true
        _historySearching = true
        jobsModel.fetchMore(top)
    }

    // The jobs that ended, newest first as the list gives them.
    SortFilterProxyModel {
        id: history
        model: jobsModel
        filters: [
            AnyOfFilter {
                ValueFilter { roleName: "status"; value: "succeeded" }
                ValueFilter { roleName: "status"; value: "failed" }
                ValueFilter { roleName: "status"; value: "cancelled" }
            }
        ]
    }

    // The clock: only the live rows' times.
    Timer {
        interval: 1000
        repeat: true
        running: true
        onTriggered: live.tick(Date.now())
    }

    // At most one header update per frame for queue events.
    Timer {
        id: laneFrame
        interval: 16
        onTriggered: {
            const pending = root._pendingLanes
            root._pendingLanes = {}
            root.applyLanes(Object.keys(pending).map(k => pending[k]))
        }
    }

    // One pass for the job events of a frame.
    Timer {
        id: jobFrame
        interval: 16
        onTriggered: root.applyPendingJobs()
    }

    // One live read for a burst of job events.
    Timer {
        id: liveCoalesce
        interval: 150
        onTriggered: root.readLive()
    }

    Timer {
        id: retryLive
        interval: 2000
        onTriggered: root.readLive()
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

    Item {
        id: lists
        anchors {
            left: parent.left; right: parent.right; bottom: parent.bottom
            top: banner.visible ? banner.bottom : parent.top
            topMargin: Theme.spacing
        }

        ListView {
            id: liveList
            anchors { left: parent.left; right: parent.right; top: parent.top }
            // As tall as it needs, up to 60 % of the panel; the finished list has the rest.
            height: Math.min(contentHeight, parent.height * 0.6)
            clip: true
            model: live
            spacing: 4
            // No rows made ahead off the frame's path: a list emptied under one being made cancels
            // it by an index past its end, and Qt warns ("DelegateModel::cancel: index out range").
            cacheBuffer: 0
            activeFocusOnTab: true
            currentIndex: -1
            boundsBehavior: Flickable.StopAtBounds

            Accessible.role: Accessible.List
            Accessible.name: "Running and queued"

            header: Column {
                id: headerColumn
                width: liveList.width
                spacing: Theme.spacing
                bottomPadding: Theme.spacing

                Item {
                    width: parent.width
                    height: cancelAll.visible ? cancelAll.implicitHeight : 0
                    LButton {
                        id: cancelAll
                        anchors { right: parent.right; rightMargin: Theme.spacing }
                        visible: root.mine > 0
                        text: "Cancel all from this app"
                        enabled: root.writable
                        onClicked: root.cancelAllMine()
                    }
                }

                Repeater {
                    model: root.headerLanes
                    LaneHeader {
                        required property string modelData
                        width: headerColumn.width
                        panel: root
                        lane: modelData
                        queueState: root.lanes[modelData] || null
                        // Another lane shows only when there is something to say.
                        visible: modelData === "gpu-heavy" || root.rowLanes.indexOf(modelData) >= 0
                                 || (queueState && (queueState.paused || queueState.held > 0))
                    }
                }

                // A read cut at its page cap says so, rather than dropping the rest unseen.
                Text {
                    id: cappedText
                    visible: root.capped >= 0
                    width: parent.width
                    leftPadding: Theme.spacing
                    wrapMode: Text.Wrap
                    textFormat: Text.PlainText
                    text: "Showing the first " + root.grouped(root.capped) + " queued jobs"
                    color: Theme.warning
                    font.pixelSize: Theme.fontSize
                    Component.onCompleted: root.cappedNote = cappedText
                }

                // A failed read says so: the rows above may be old until the retry works.
                Text {
                    id: staleText
                    visible: root._liveReadFailed
                    width: parent.width
                    leftPadding: Theme.spacing
                    wrapMode: Text.Wrap
                    textFormat: Text.PlainText
                    text: "Couldn't read the queue. Trying again…"
                    color: Theme.warning
                    font.pixelSize: Theme.fontSize
                    Component.onCompleted: root.staleNote = staleText
                }

                Text {
                    id: liveEmptyText
                    visible: live.loaded && live.count === 0
                    width: parent.width
                    horizontalAlignment: Text.AlignHCenter
                    wrapMode: Text.Wrap
                    textFormat: Text.PlainText
                    text: "Nothing is running or queued."
                    color: Theme.textMuted
                    font.pixelSize: Theme.fontSize
                    Component.onCompleted: root.emptyText = liveEmptyText
                }
            }

            delegate: JobRow {
                width: liveList.width
                panel: root
            }
        }

        ListView {
            id: historyList
            anchors {
                left: parent.left; right: parent.right; bottom: parent.bottom
                top: liveList.bottom; topMargin: Theme.spacing
            }
            clip: true
            model: history
            spacing: 4
            activeFocusOnTab: true
            currentIndex: -1
            boundsBehavior: Flickable.StopAtBounds

            Accessible.role: Accessible.List
            Accessible.name: "Finished"

            header: Text {
                width: historyList.width
                leftPadding: Theme.spacing
                bottomPadding: 4
                textFormat: Text.PlainText
                text: root.historyHeader
                color: Theme.textMuted
                font.pixelSize: Theme.fontSize
                font.bold: historyList.count > 0
            }

            delegate: JobRow {
                width: historyList.width
                panel: root
            }
        }
    }
}
