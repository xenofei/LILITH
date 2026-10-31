// What the top bar knows of the jobs (APP-006): how many run and wait, and the lanes' states. It
// follows the `jobs` topic and shows nothing itself.
//
// Counts come from a read of its own: every page of `jobs.list {status: "running"}` and of
// `{status: "queued"}`, which lists every queued job, the dispatched ones (substatus `submitting` or
// `queued_in_engine`) among them. A count needs no place in a queue, so unlike the Jobs panel it reads
// no `{order: "queue"}`. Their JSON goes to a LiveJobsModel of its own as it came, never parsed here;
// repeats are dropped by job id. The model is `countsOnly`:
// no rows, ETAs or sorting, so a job's event is a lookup. A `jobs` event is one job, never a count:
// one that came, went or changed status asks for a read 150 ms after the last such event (and at
// most 1 s after the first, so a steady stream still reads); a progress-only one does nothing more.
// One read is out at a time.
//
// `current` says the lanes are the core's as it is now. It is false from a connection loss or a
// `resync_required` until the fresh read made after it has landed, and the pause control waits for
// it: a core restart ends a pause, so the old state must not decide a toggle. `ready` after a loss
// is what starts that read. It replaces nothing while it is out, and is no model reload: calls made
// after `ready` reach the new connection, and a mark that comes after it only reads once more. Only
// a mark's read carries nothing over; `ready`'s keeps a job of a status this build doesn't know
// (P4a-R40), which no listing names. After a mark such a job isn't counted until its next event: the
// panel gets it back from its reloaded history, and the bar has none.
//
// A failed read is asked again after 1 s, doubling up to 30 s while connected, as JsonListModel's
// are; while disconnected nothing is asked, and `ready` reads at once.
//
// Lanes come from the read and from `{kind: "queue", state}` events, applied as full states, at
// most one update per frame. An event that comes during a read is newer than that read's lanes.
import QtQuick
import Lilith.Core
import Lilith.UiKit

Item {
    id: root

    required property var core

    // See the header.
    property bool current: false
    readonly property alias running: live.runningCount
    readonly property alias queued: live.queuedCount
    // How many queued jobs the last read listed when it stopped at its page cap, else -1.
    readonly property alias capped: live.cappedCount
    // lane -> QueueState.
    property var lanes: ({})
    // The lane updates applied (at most one per frame).
    property int laneUpdates: 0

    // A job reached a terminal status.
    signal jobEnded()
    // The core refused a read with `workspace.unsupported`: it no longer serves this workspace.
    signal workspaceRefused(string message)

    readonly property int _maxWaitMs: 1000
    readonly property bool _connected: !!core && core.connection === "ready"
    property real _dueSince: 0
    property bool _reading: false
    property bool _again: false
    // The next read must carry nothing over (the first, or after a resync).
    property bool _fresh: true
    // The next read is what makes the lanes current again (the first, or after a resync or a loss).
    property bool _stale: true
    // Connection losses so far: a read that spans one doesn't speak for the new connection.
    property int _losses: 0
    property int _retryMs: 0
    property var _pendingLanes: ({})
    property var _lanesDuringRead: ({})
    // Request id -> {ok, fail}: the calls whose answer goes to the model as it came.
    property var _raw: ({})

    visible: false

    function applyLanes(states) {
        const next = Object.assign({}, lanes)
        for (const s of states || []) {
            if (s && s.lane)
                next[s.lane] = s
        }
        lanes = next
        laneUpdates++
    }

    function queueEvent(state) {
        if (!state || !state.lane)
            return
        if (_reading)
            _lanesDuringRead[state.lane] = state
        _pendingLanes[state.lane] = state
        if (!laneFrame.running)
            laneFrame.start()
    }

    function rawCall(args, ok, fail) {
        const id = core.call("jobs.list", JSON.stringify(args))
        _raw[id] = {ok: ok, fail: fail}
    }

    function requestRead() {
        const now = Date.now()
        if (_dueSince === 0)
            _dueSince = now
        if (now - _dueSince < _maxWaitMs)
            coalesce.restart()
        else if (!coalesce.running)
            coalesce.start()
    }

    function read() {
        if (!_connected)
            return
        if (_reading) {
            _again = true
            return
        }
        const asked = core.workspace ? String(core.workspace.id) : ""
        const fresh = _fresh
        const stale = _stale
        const losses = _losses
        _fresh = false
        _stale = false
        _reading = true
        _lanesDuringRead = {}
        live.beginRead(fresh)
        let open = 2
        let gone = false
        const done = () => {
            if (gone || --open > 0)
                return
            live.commit(Date.now())
            root._reading = false
            root._retryMs = 0
            root.applyLanes(JSON.parse(live.lanesJson()))
            root.applyLanes(Object.keys(root._lanesDuringRead).map(k => root._lanesDuringRead[k]))
            if (stale && !root._stale && losses === root._losses && root._connected)
                root.current = true
            if (root._again) {
                root._again = false
                if (root._stale)
                    root.read()
                else
                    root.requestRead()
            }
        }
        const fail = e => {
            if (gone)
                return
            gone = true
            if (e && e.code === "workspace.unsupported" && asked === (root.core.workspace ? String(root.core.workspace.id) : ""))
                root.workspaceRefused(String(e.message))
            root._reading = false
            root._fresh = root._fresh || fresh
            root._stale = root._stale || stale
            if (root._again) {
                root._again = false
                root.read()
                return
            }
            if (root._connected) {
                root._retryMs = root._retryMs === 0 ? 1000 : Math.min(root._retryMs * 2, 30000)
                retry.interval = root._retryMs
                retry.restart()
            }
        }
        const pages = (args, add, cursor) => rawCall(Object.assign({limit: 100}, args, cursor ? {cursor: cursor} : {}), json => {
            if (gone)
                return
            const next = add(json)
            if (next !== "")
                pages(args, add, next)
            else
                done()
        }, fail)
        pages({status: "running"}, json => live.addRunningPage(json), "")
        pages({status: "queued"}, json => live.addQueuedPage(json), "")
    }

    function onJobsEvent(kind, data) {
        if (kind !== "event") {
            _fresh = true
            _stale = true
            current = false
            read()
            return
        }
        if (!data)
            return
        if (data.kind === "queue") {
            queueEvent(data.state)
        } else if (data.kind === "job" && data.job) {
            const j = data.job
            const change = live.applyJob(JSON.stringify({job_id: j.job_id, status: j.status, updated: j.updated}), 0)
            if (change === 2)
                requestRead()
            if (j.status === "succeeded" || j.status === "failed" || j.status === "cancelled")
                jobEnded()
        }
    }

    Component.onCompleted: {
        if (!core)
            return
        CoreCall.subscribe(core, "jobs", root, (kind, seq, data) => root.onJobsEvent(kind, data))
        read()
    }

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
        function onConnectionChanged() {
            if (root._connected) {
                retry.stop()
                root._retryMs = 0
                root._stale = true
                root.read()
            } else {
                root.current = false
                root._losses++
                retry.stop()
            }
        }
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
        countsOnly: true
    }

    Timer {
        id: coalesce
        interval: 150
        onTriggered: {
            root._dueSince = 0
            root.read()
        }
    }

    Timer {
        id: retry
        onTriggered: root.read()
    }

    // At most one lane update per frame.
    Timer {
        id: laneFrame
        interval: 16
        onTriggered: {
            const pending = root._pendingLanes
            root._pendingLanes = {}
            root.applyLanes(Object.keys(pending).map(k => pending[k]))
        }
    }
}
