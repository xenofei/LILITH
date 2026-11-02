// Home (minimal; APP-111, APP-016, P4a-R16, P4a-R21, P4a-R41): the Opened card, "While you were
// away", the workspace and its projects, the assets by status, the open jobs and the recent assets.
//
// Reads. Home reads its cards when it first shows and after a `jobs` or `confirmations` event that
// changes what it shows: a job that came, went or changed status (never its progress, nor
// `{kind: "queue"}`) reads the jobs, one that ended the workspace's cards too, and a confirmation's
// event the confirmations. They wait 150 ms after the last such event, at most 1 s after the first,
// and one read is out at a time, so a burst is one read. A snapshot, a `resync_required` or `ready`
// reads everything. A failed read keeps what the card shows, greyed, says so, and is asked again
// after 1 s, doubling up to 30 s. While Home isn't on screen nothing is read, and what came meanwhile
// is read when it shows. A workspace switch cancels what is out and clears the cards. Everything
// needing a workspace waits for one (APP-016): without one only the open jobs are read, and the
// recent assets become the recent loose files (empty until plan 5). `NoWorkspaceBanner` holds the
// way out.
//
// Jobs. The open jobs are a LiveJobsModel of Home's own over every page of `jobs.list {status:
// "running"}`, `{order: "queue"}` and `{status: "queued"}`, the JSON handed over as it came. The
// card's jobs are every page of `{since: <the card's time>}`, up to `awayPages`, and the queued jobs,
// where the ones a restart holds are, whatever their age. `ShellState.awayItems` picks from them.
//
// Last seen. The time is kept per workspace in the app's state (`stateDir`, AppState); a shell
// without a `stateDir` keeps none, and so has no card. It becomes now after Home has been really on
// screen for `seenAfterMs` in a row, the card's reads in: not behind a tab, in a closed dock, on a
// minimized window, or with the core away. A new line on the card starts that wait again. The card
// goes on counting from the time it opened with, so its lines don't vanish as you read them: it moves
// to the kept time when Home has been off screen for `seenAfterMs` (a float, which hides it for a
// moment, doesn't), or at once on Dismiss, which also forgets the lines it dismissed, the jobs a
// restart holds and the pending confirmations among them, until they change or the app starts again.
// There is no card on the first run.
//
// Everything from outside is plain text through `core.shownText` (P4a-R8), and every count goes
// through MoneyText.count.
import QtQuick
import Lilith.Core
import Lilith.UiKit

Rectangle {
    id: root

    required property var core
    required property string title
    // The app's state folder (ShellWindow's `stateDir`, through Panels.js); empty: no last seen.
    property string stateDir

    readonly property alias openedCard: openedCard
    readonly property alias awayCard: awayCard
    readonly property alias projectsCard: projectsCard
    readonly property alias statusCounts: statusCounts
    readonly property alias openJobs: openJobs
    readonly property alias recentAssets: recentAssets
    readonly property alias scroller: scroller
    readonly property alias live: live

    readonly property string workspaceId: core && core.workspace ? String(core.workspace.id) : ""
    readonly property bool connected: !!core && core.connection === "ready"
    readonly property bool hasWorkspace: workspaceId !== ""
    // The session is up and has no workspace at all (APP-016).
    readonly property bool noWorkspace: connected && !hasWorkspace
    // Really on screen, as far as Qt can tell.
    readonly property bool onScreen: visible && width > 0 && height > 0 && Window.visibility !== Window.Hidden
                                     && Window.visibility !== Window.Minimized
    // The card has a time to count from.
    readonly property bool awayOn: hasWorkspace && lastSeen !== ""

    // How long Home must show before the time moves.
    readonly property int seenAfterMs: 3000
    // The most pages of `{since}` one read follows; past them the card says there is more.
    readonly property int awayPages: 20

    // projects.list's rows, assets.query's rows, and project.status's counts and project.
    property var projects: []
    property var assets: []
    property var byStatus: null
    property string statusProject
    // The card's lines ({kind, id, title, at}), and whether its `{since}` read stopped early.
    property var awayItems: []
    property bool awayCut: false
    // How many queued jobs the last read listed when it stopped at its page cap, else -1.
    property int jobsCapped: -1
    // The time the card counts from: empty before the first time Home was seen here.
    property string lastSeen

    // Each part's last read: "" (none yet), "ok", "none" (status: no such project) or "failed".
    property var readState: ({})
    property var readError: ({})

    property int _open: 0
    property bool _again: false
    // What is due: {jobs, confirmations, cards}.
    property var _due: ({jobs: true, confirmations: true, cards: true})
    // The next jobs read carries nothing over (a snapshot or a resync came).
    property bool _fresh: false
    property var _calls: []
    // Request id -> {ok, fail}: the calls whose answer goes on as it came.
    property var _raw: ({})
    property var _pendingJobs: []
    property real _dueSince: 0
    property int _retryMs: 0
    property var _toRetry: ({})
    // The last pages holding the card's jobs, the pending confirmations, and the lines known so far.
    property string _awayPages: "[]"
    property var _pending: []
    property var _known: ({})
    property var _dismissed: ({})
    // The card's reads have landed for this time.
    property bool _awayLoaded: false
    // There is something Home hasn't been seen with yet.
    property bool _unseen: true
    // The time moved since the card's time was taken, and Home has been off screen since.
    property bool _wroteSeen: false
    property bool _moveTime: false
    property string _workspaceAt

    color: Theme.surface

    Accessible.role: Accessible.Pane
    Accessible.name: title

    // The core as it is now: a signal's handler can run before the bindings on it have moved.
    function connectedNow() {
        return !!core && core.connection === "ready"
    }

    function workspaceNow() {
        return core && core.workspace ? String(core.workspace.id) : ""
    }

    // Outside text as shown (P4a-R8).
    function shown(text, maxChars) {
        return core && text !== undefined && text !== null ? core.shownText(String(text), maxChars || 60) : ""
    }

    // A time from the core in the person's local calendar, "" when it doesn't read.
    function localStamp(at) {
        const ms = Date.parse(at)
        return isNaN(ms) ? "" : Qt.formatDateTime(new Date(ms), "yyyy-MM-dd HH:mm")
    }

    // What a card says about its read, "" when it has nothing to say. `what` names what it reads.
    function noteFor(part, what) {
        const state = readState[part] || ""
        if (!connected)
            return state === "" ? "Not connected to the core." : "Not connected to the core, so this may be out of date."
        if (state === "failed")
            return "Couldn't read " + what + ": " + readError[part] + " Trying again…"
        return state === "" ? "Reading…" : ""
    }

    // What the card shows may be old.
    function isStale(part) {
        return !connected || readState[part] === "failed"
    }

    function setRead(part, state, error) {
        readState = Object.assign({}, readState, {[part]: state})
        if (state === "failed") {
            readError = Object.assign({}, readError, {[part]: shown(error && error.message ? error.message : "the core didn't answer.", 300)})
            _toRetry[part === "jobs" || part === "confirmations" ? part : "cards"] = true
        }
    }

    // --- Reads. ---

    function due(what) {
        _due = Object.assign({}, _due, what)
    }

    function dueEverything() {
        due({jobs: true, confirmations: true, cards: true})
    }

    // 150 ms after the last event, at most 1 s after the first.
    function requestRead() {
        const now = Date.now()
        if (_dueSince === 0)
            _dueSince = now
        if (now - _dueSince < 1000)
            coalesce.restart()
        else if (!coalesce.running)
            coalesce.start()
    }

    function rawCall(tool, args, ok, fail) {
        const id = core.call(tool, JSON.stringify(args))
        _raw[id] = {ok: ok, fail: fail}
    }

    function read() {
        if (!connectedNow() || !onScreen)
            return
        if (_open > 0) {
            _again = true
            return
        }
        const jobs = !!_due.jobs
        const here = workspaceNow() !== ""
        const confirmations = !!_due.confirmations && here && lastSeen !== ""
        const cards = !!_due.cards && here
        _due = {}
        if (!jobs && !confirmations && !cards)
            return
        _again = false
        _dueSince = 0
        coalesce.stop()
        retry.stop()
        _toRetry = {}
        _calls = []
        _open = (jobs ? 1 : 0) + (confirmations ? 1 : 0) + (cards ? 3 : 0)
        if (jobs)
            readJobs()
        if (confirmations)
            readConfirmations()
        if (cards) {
            readProjects()
            readStatus()
            readAssets()
        }
    }

    // One of the reads is in.
    function landed() {
        if (--_open > 0)
            return
        updateAway()
        const failed = Object.keys(_toRetry)
        if (failed.length > 0) {
            _retryMs = _retryMs === 0 ? 1000 : Math.min(_retryMs * 2, 30000)
            retry.interval = _retryMs
            retry.start()
        } else {
            _retryMs = 0
        }
        if (_again || _due.jobs || _due.confirmations || _due.cards) {
            _again = false
            requestRead()
        }
    }

    function readJobs() {
        const since = workspaceNow() !== "" ? lastSeen : ""
        const fresh = _fresh
        _fresh = false
        applyPendingJobs()
        live.beginRead(fresh)
        let open = since !== "" ? 4 : 3
        let gone = false
        const sincePages = []
        const queuedPages = []
        const done = () => {
            if (gone || --open > 0)
                return
            root.applyPendingJobs()
            live.commit(Date.now())
            const capped = JSON.parse(live.overview("image")).capped
            root.jobsCapped = capped === null || capped === undefined ? -1 : capped
            root._awayPages = "[" + sincePages.concat(queuedPages).join(",") + "]"
            root.setRead("jobs", "ok")
            root.landed()
        }
        const fail = e => {
            if (gone)
                return
            gone = true
            root._fresh = root._fresh || fresh
            root.setRead("jobs", "failed", e)
            root.landed()
        }
        const list = (args, add) => {
            const page = cursor => root.rawCall("jobs.list", Object.assign({limit: 100}, args, cursor ? {cursor: cursor} : {}), json => {
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
        list({status: "queued"}, json => {
            queuedPages.push(json)
            return live.addQueuedPage(json)
        })
        if (since === "")
            return
        awayCut = false
        list({since: since}, json => {
            sincePages.push(json)
            const next = ShellState.nextCursor(json)
            if (next !== "" && sincePages.length >= root.awayPages) {
                root.awayCut = true
                return ""
            }
            return next
        })
    }

    function track(call) {
        _calls.push(call)
    }

    function readConfirmations() {
        track(CoreCall.appCall(core, "app/confirmations.list", {state: "pending"}, {
            owner: root,
            ok: r => {
                root._pending = r.confirmations || []
                root.setRead("confirmations", "ok")
                root.landed()
            },
            error: e => {
                root.setRead("confirmations", "failed", e)
                root.landed()
            }
        }))
    }

    function readProjects() {
        track(CoreCall.call(core, "projects.list", {}, {
            owner: root,
            ok: r => {
                root.projects = (r.structured || {}).projects || []
                root.setRead("projects", "ok")
                root.landed()
            },
            error: e => {
                root.setRead("projects", "failed", e)
                root.landed()
            }
        }))
    }

    function readStatus() {
        track(CoreCall.call(core, "project.status", core.project ? {project: core.project} : {}, {
            owner: root,
            ok: r => {
                const s = r.structured || {}
                root.byStatus = s.assets_by_status || {}
                root.statusProject = s.project || ""
                root.setRead("status", "ok")
                root.landed()
            },
            error: e => {
                // The project the core falls back on isn't there: nothing to count, no fault.
                if (e && e.code === "project.unknown") {
                    root.byStatus = null
                    root.statusProject = ""
                    root.setRead("status", "none")
                } else {
                    root.setRead("status", "failed", e)
                }
                root.landed()
            }
        }))
    }

    function readAssets() {
        track(CoreCall.call(core, "assets.query", {sort: {field: "updated", dir: "desc"}, limit: 12}, {
            owner: root,
            ok: r => {
                root.assets = (r.structured || {}).assets || []
                root.setRead("assets", "ok")
                root.landed()
            },
            error: e => {
                root.setRead("assets", "failed", e)
                root.landed()
            }
        }))
    }

    function cancelReads() {
        for (const call of _calls)
            call.cancel()
        _calls = []
        for (const id in _raw) {
            try {
                core.cancel(id)
            } catch (e) {
                // The core object went first.
            }
        }
        _raw = {}
        _open = 0
        _again = false
        _dueSince = 0
        coalesce.stop()
        retry.stop()
    }

    // --- Events. ---

    function onJobsEvent(kind, data) {
        if (kind !== "event") {
            _fresh = true
            dueEverything()
            read()
        } else if (data && data.kind === "job" && data.job) {
            _pendingJobs.push(data.job)
            if (!jobFrame.running)
                jobFrame.start()
        }
    }

    function onConfirmationsEvent(kind) {
        if (kind !== "event") {
            dueEverything()
            read()
        } else {
            due({confirmations: true})
            requestRead()
        }
    }

    // The events' jobs of this frame, in one pass. A running job's progress changes no status, and
    // asks for nothing.
    function applyPendingJobs() {
        jobFrame.stop()
        if (_pendingJobs.length === 0)
            return
        const jobs = _pendingJobs
        _pendingJobs = []
        const change = live.applyJobs(JSON.stringify(jobs), Date.now())
        const ended = jobs.some(j => j.status === "succeeded" || j.status === "failed" || j.status === "cancelled")
        const unknown = jobs.some(j => j.substatus === "interrupted_unknown")
        if (change !== 2 && !ended && !unknown)
            return
        due(ended ? {jobs: true, cards: true} : {jobs: true})
        requestRead()
    }

    // --- The card. ---

    function lineKey(item) {
        return item.kind + "|" + item.id + "|" + item.at
    }

    function updateAway() {
        if (!awayOn) {
            awayItems = []
        } else {
            const items = JSON.parse(ShellState.awayItems(_awayPages, JSON.stringify(_pending), lastSeen))
                              .filter(item => !_dismissed[lineKey(item)])
            const known = {}
            let grew = false
            for (const item of items) {
                known[lineKey(item)] = true
                grew = grew || !_known[lineKey(item)]
            }
            _known = known
            awayItems = items
            if (grew) {
                _unseen = true
                if (seenTimer.running)
                    seenTimer.restart()
            }
        }
        _awayLoaded = readState.jobs === "ok" && (!awayOn || readState.confirmations === "ok")
    }

    function writeSeen(at) {
        if (stateDir !== "" && workspaceNow() !== "")
            ShellState.setLastSeen(stateDir, workspaceNow(), at)
        _unseen = false
        _wroteSeen = true
    }

    // Dismiss: these lines are done with, and the card counts from now.
    function dismissAway() {
        const dismissed = Object.assign({}, _dismissed)
        for (const item of awayItems)
            dismissed[lineKey(item)] = true
        _dismissed = dismissed
        const at = new Date().toISOString()
        writeSeen(at)
        lastSeen = at
        updateAway()
    }

    // Home has been away long enough: the card now counts from the kept time.
    function moveCardTime() {
        _moveTime = false
        _wroteSeen = false
        lastSeen = ShellState.lastSeen(stateDir, workspaceNow())
        _unseen = true
        _awayLoaded = false
        due({jobs: true, confirmations: true})
    }

    function startWorkspace() {
        _workspaceAt = workspaceNow()
        lastSeen = _workspaceAt !== "" ? ShellState.lastSeen(stateDir, _workspaceAt) : ""
        _unseen = true
        _wroteSeen = false
        _moveTime = false
        _awayLoaded = false
        _known = {}
        _dismissed = {}
    }

    onOnScreenChanged: {
        if (!onScreen)
            return
        if (_moveTime)
            moveCardTime()
        read()
    }

    Component.onCompleted: {
        startWorkspace()
        if (!core)
            return
        CoreCall.subscribe(core, "jobs", root, (kind, seq, data) => root.onJobsEvent(kind, data))
        CoreCall.subscribe(core, "confirmations", root, kind => root.onConfirmationsEvent(kind))
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
            if (root.connectedNow()) {
                root._retryMs = 0
                root.dueEverything()
                root.read()
            } else {
                // A read that spans a loss doesn't speak for the new connection.
                root.cancelReads()
            }
        }
        function onWorkspaceChanged() {
            if (root.workspaceNow() === root._workspaceAt)
                return
            root.cancelReads()
            root.projects = []
            root.assets = []
            root.byStatus = null
            root.statusProject = ""
            root.awayItems = []
            root.awayCut = false
            root._pending = []
            root._awayPages = "[]"
            root._pendingJobs = []
            root.readState = {}
            root.readError = {}
            live.beginRead(true)
            live.commit(Date.now())
            root.startWorkspace()
            root.dueEverything()
            root.read()
        }
        function onProjectChanged() {
            root.due({cards: true})
            root.requestRead()
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
        onTriggered: {
            const again = root._toRetry
            root._toRetry = {}
            root.due(again)
            root.read()
        }
    }

    // One pass for the job events of a frame.
    Timer {
        id: jobFrame
        interval: 16
        onTriggered: root.applyPendingJobs()
    }

    // The time moves after this long really on screen.
    Timer {
        id: seenTimer
        interval: root.seenAfterMs
        running: root.onScreen && root.connected && root.hasWorkspace && root.stateDir !== "" && root._awayLoaded
                 && root._unseen
        onTriggered: root.writeSeen(new Date().toISOString())
    }

    // A float hides Home for a moment; this long off screen is really away.
    Timer {
        interval: root.seenAfterMs
        running: !root.onScreen && root._wroteSeen
        onTriggered: root._moveTime = true
    }

    Flickable {
        id: scroller

        readonly property bool wide: width >= 700

        anchors.fill: parent
        clip: true
        contentWidth: width
        contentHeight: content.height + 2 * Theme.spacing
        boundsBehavior: Flickable.StopAtBounds
        // The keyboard scrolls it: one tab stop, then the arrows, Page Up and Page Down, Home and End.
        activeFocusOnTab: true

        Accessible.role: Accessible.Pane
        Accessible.name: root.title + " cards"

        function scrollBy(dy) {
            contentY = Math.max(0, Math.min(contentHeight - height, contentY + dy))
        }

        Keys.onUpPressed: scrollBy(-40)
        Keys.onDownPressed: scrollBy(40)
        Keys.onPressed: event => {
            if (event.key === Qt.Key_PageUp)
                scrollBy(-height)
            else if (event.key === Qt.Key_PageDown)
                scrollBy(height)
            else if (event.key === Qt.Key_Home)
                contentY = 0
            else if (event.key === Qt.Key_End)
                scrollBy(contentHeight)
            else
                return
            event.accepted = true
        }

        Column {
            id: content
            x: Theme.spacing
            y: Theme.spacing
            width: scroller.width - 2 * Theme.spacing
            spacing: Theme.spacing

            OpenedCard {
                id: openedCard
                width: parent.width
                onOpened: scroller.contentY = 0
            }

            AwayCard {
                id: awayCard
                width: parent.width
                panel: root
            }

            Grid {
                id: columns
                readonly property real columnWidth: scroller.wide ? (width - spacing) / 2 : width
                width: parent.width
                columns: scroller.wide ? 2 : 1
                spacing: Theme.spacing

                Column {
                    width: columns.columnWidth
                    spacing: Theme.spacing

                    ProjectsCard {
                        id: projectsCard
                        width: parent.width
                        panel: root
                    }

                    StatusCounts {
                        id: statusCounts
                        width: parent.width
                        panel: root
                    }
                }

                Column {
                    width: columns.columnWidth
                    spacing: Theme.spacing

                    OpenJobs {
                        id: openJobs
                        width: parent.width
                        panel: root
                    }

                    RecentAssets {
                        id: recentAssets
                        width: parent.width
                        panel: root
                    }
                }
            }
        }
    }
}
