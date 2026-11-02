// Home (minimal; APP-111, APP-016, P4a-R16, P4a-R21, P4a-R41): the Opened card, "While you were
// away", the workspace and its projects, the assets by status, the open jobs and the recent assets.
//
// Reads. There are four kinds, each with at most one read out, so a slow one holds up no other: the
// jobs, the confirmations, the cards (`projects.list` and `assets.query`) and the counts
// (`project.status`). Home reads them all when it first shows, and after a `jobs` or `confirmations`
// event that changes what it shows: a job that came, went or changed status (never its progress, nor
// `{kind: "queue"}`) reads the jobs, one that ended the cards too, and a confirmation's event the
// confirmations. They wait 150 ms after the last such event, at most 1 s after the first, so a burst
// is one read. The counts walk the project's folder, so job ends read them at most once in
// `statusGapMs`, with a last read so that they settle. The top bar's switcher having set the project,
// by a choice or again on a new session (`contextSet`), reads them at once: a read made before that
// answered for whatever the hint says. A
// snapshot, a `resync_required` or `ready` reads everything, and until each read lands what it covers
// is grey and says it is being read again. A failed read keeps what the card shows, greyed, says so,
// and is asked again after 1 s, doubling up to 30 s, or with the next read of anything, except the
// counts, which keep to the wait, so a failing walk of the folder doesn't run back to back. While Home
// isn't on screen nothing is read: what came meanwhile is read when it shows, and after a second or
// more off screen everything is (a float's moment off screen isn't that). A workspace switch cancels
// what is out and clears the cards. Everything needing a workspace waits for one (APP-016): without
// one only the open jobs are read, and the recent assets become the recent loose files (empty until
// plan 5). `NoWorkspaceBanner` holds the way out.
//
// Home never names a project (`project.status {}`): a call that names one makes it the session's
// active project and rewrites the workspace's hint (ARCH-088), and a read must do neither.
//
// Jobs. The open jobs are a LiveJobsModel of Home's own over every page of `jobs.list {status:
// "running"}`, `{order: "queue"}` and `{status: "queued"}`, the JSON handed over as it came. The
// card's jobs are every page of `{since: <the card's time>}`, up to `awayPages`, and the queued jobs,
// where the ones a restart holds are, whatever their age. `ShellState.awayItems` picks from them, as
// soon as the jobs and the confirmations have landed.
//
// Last seen. The time is kept per workspace in the app's state (`stateDir`, AppState); a shell
// without a `stateDir` keeps none, and so has no card. A person must never lose a line they weren't
// shown, and a line they watched arrive shouldn't come back. So after Home has been on screen in the
// active window for `seenAfterMs` in a row with no read of the card's out (not behind a tab, in a
// closed dock, on a minimized window, under another program, or with the core away; a locked screen
// is meant to count as not active, but no test or real run has shown it), it does one more jobs
// read, and the time written is when that read began (`seenStamp`, never `now`). That is a second or
// more after the read that showed the last line, which matters because the core's `since` lists
// again the second a fractional time falls in. If that read brings a new line,
// fails, or lands with Home off screen or inactive, nothing is written, and the wait starts again. A
// new line on the card starts it again too, and a steady stream of reads puts the write off, never the
// other way. Active is Qt's: a floating Home counts while any of the app's windows has the focus. A
// window watched on a second monitor while you work elsewhere holds its lines until you come back to
// it; a line scrolled out of view counts as shown, as the card's scroll area is on screen. A write that
// fails isn't taken as written: it is tried at the next mark, and the card says so if it keeps failing. A `{since}` read cut at `awayPages` writes nothing: the card says
// there is more, until Dismiss. The card goes on counting from the time it opened with, so its lines
// don't vanish as you read them: it moves to the kept time when Home has been off screen for
// `seenAfterMs` (a float, which hides it for a moment, doesn't). Dismiss forgets the lines it
// dismissed, the jobs a restart holds and the pending confirmations among them, until they change, the
// workspace changes or the app starts again, and moves both times to when the read they came from
// began, the usual write then taking the kept time past that second; with a jobs read due, out or
// failing it moves neither, and the usual write follows once a read has landed. There is no card on
// the first run. A kept time ahead of now (the clock was ahead) can't say when you last looked: the
// card counts from `aheadLookBackMs` before now, so lines may show twice but none is lost, and the
// usual write mends the kept time. A kept time the core's `since` wouldn't take counts as none
// (`ShellState.lastSeen`).
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
    readonly property alias scrollBar: scrollBar
    readonly property alias focusRing: focusRing
    readonly property alias live: live

    readonly property string workspaceId: core && core.workspace ? String(core.workspace.id) : ""
    readonly property bool connected: !!core && core.connection === "ready"
    readonly property bool hasWorkspace: workspaceId !== ""
    // The session is up and has no workspace at all (APP-016).
    readonly property bool noWorkspace: connected && !hasWorkspace
    // On screen, as far as Qt can tell: a window under another program is too, which is why the time
    // also asks for the active window. A locked screen is meant to count as not active; untested.
    readonly property bool onScreen: visible && width > 0 && height > 0 && Window.visibility !== Window.Hidden
                                     && Window.visibility !== Window.Minimized
    // The card has a time to count from.
    readonly property bool awayOn: hasWorkspace && lastSeen !== ""

    // How long Home must show before the time moves.
    readonly property int seenAfterMs: 3000
    // The most pages of `{since}` one read follows; past them the card says there is more.
    readonly property int awayPages: 20
    // The least time between two reads of the counts after job ends.
    property int statusGapMs: 30000
    // How far back the card counts when the kept time is ahead of now: a week.
    readonly property real aheadLookBackMs: 7 * 24 * 3600 * 1000

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

    // Each part's last read: "" (none yet), "ok", "none" (status: no such project), "failed", or "old"
    // (a reload is due or out).
    property var readState: ({})
    property var readError: ({})

    // The kinds of read out: jobs, confirmations, cards, status.
    property var _out: ({})
    readonly property int _open: Object.keys(_out).length
    // What is due, by kind.
    property var _due: ({jobs: true, confirmations: true, cards: true, status: true})
    // The kinds a reload has marked: what lands from a read asked before it is still old.
    property var _oldDue: ({})
    // The next jobs read carries nothing over (a snapshot or a resync came).
    property bool _fresh: false
    // Kind -> its read's calls, to cancel them.
    property var _calls: ({})
    // Request id -> {ok, fail}: the calls whose answer goes on as it came.
    property var _raw: ({})
    property var _pendingJobs: []
    property real _dueSince: 0
    property int _retryMs: 0
    property var _toRetry: ({})
    // When the counts were last read, ms.
    property real _statusAt: 0
    // Home has been off screen long enough for everything to be read when it shows.
    property bool _readOnShow: false
    // When the last jobs read that landed began (seenStamp): what the time may move to.
    property string _seenUpTo
    // The last pages holding the card's jobs, the pending confirmations, and the lines known so far.
    property string _awayPages: "[]"
    property var _pending: []
    property var _known: ({})
    property var _dismissed: ({})
    // The card's reads have landed for this time.
    property bool _awayLoaded: false
    // `Window.active` as last looked at.
    property bool _activeNow: false
    // Writes of the time that failed in a row: from two, the card says so.
    property int _writesFailed: 0
    readonly property bool seenWriteFailing: _writesFailed >= 2
    // The one more jobs read before the time is written is due or out.
    property bool _confirming: false
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
        const ms = typeof at === "string" ? ShellState.timeMs(at) : NaN
        return isNaN(ms) ? "" : Qt.formatDateTime(new Date(ms), "yyyy-MM-dd HH:mm")
    }

    // `ms` as a kept time. The core takes a fraction of .000 as a whole second, which would lose a job
    // later in that second, so that one reads a millisecond earlier.
    function seenStamp(ms) {
        return new Date(ms % 1000 === 0 ? ms - 1 : ms).toISOString()
    }

    // What a card says about its read, "" when it has nothing to say. `what` names what it reads.
    function noteFor(part, what) {
        const state = readState[part] || ""
        if (!connected)
            return state === "" ? "Not connected to the core." : "Not connected to the core, so this may be out of date."
        if (state === "failed")
            return "Couldn't read " + what + ": " + readError[part] + " Trying again…"
        if (state === "old")
            return "Reading again, so this may be out of date."
        return state === "" ? "Reading…" : ""
    }

    // What the card shows may be old.
    function isStale(part) {
        return !connected || readState[part] === "failed" || readState[part] === "old"
    }

    function kindOf(part) {
        return part === "projects" || part === "assets" ? "cards" : part
    }

    function setRead(part, state, error) {
        // Asked before a reload: it doesn't speak for now.
        if (state !== "failed" && _oldDue[kindOf(part)])
            state = "old"
        readState = Object.assign({}, readState, {[part]: state})
        if (state === "failed") {
            readError = Object.assign({}, readError, {[part]: shown(error && error.message ? error.message : "the core didn't answer.", 300)})
            _toRetry[kindOf(part)] = true
        }
    }

    // --- Reads. ---

    function due(what) {
        _due = Object.assign({}, _due, what)
    }

    function dueEverything() {
        due({jobs: true, confirmations: true, cards: true, status: true})
    }

    // A snapshot, a mark or `ready`: everything is read again, and until then it reads as old.
    function reload() {
        const state = Object.assign({}, readState)
        for (const part in state) {
            if (state[part] === "ok" || state[part] === "none")
                state[part] = "old"
        }
        readState = state
        _oldDue = {jobs: true, confirmations: true, cards: true, status: true}
        dueEverything()
    }

    // After job ends: the counts now, or at the end of their gap, once.
    function dueStatusSoon() {
        const left = statusGapMs - (Date.now() - _statusAt)
        if (left <= 0) {
            due({status: true})
        } else if (!statusGate.running) {
            statusGate.interval = left
            statusGate.start()
        }
    }

    function anyDue() {
        return !!(_due.jobs || _due.confirmations || _due.cards || _due.status)
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

    // Starts what is due and isn't out. What is out stays due, and is read when that one lands.
    function read() {
        if (!connectedNow() || !onScreen)
            return
        // A part that failed is asked again with this read, whatever brought it on; the counts wait
        // for their retry.
        const again = Object.assign({}, _toRetry)
        delete again.status
        due(again)
        _toRetry = _toRetry.status ? {status: true} : {}
        if (!_toRetry.status)
            retry.stop()
        const here = workspaceNow() !== ""
        const wanted = {
            jobs: !!_due.jobs,
            confirmations: !!_due.confirmations && here && lastSeen !== "",
            cards: !!_due.cards && here,
            status: !!_due.status && here
        }
        const left = {}
        const start = {}
        const out = Object.assign({}, _out)
        for (const kind in wanted) {
            if (wanted[kind] && _out[kind])
                left[kind] = true
            else if (wanted[kind])
                start[kind] = out[kind] = true
        }
        _due = left
        _dueSince = 0
        coalesce.stop()
        if (Object.keys(start).length === 0)
            return
        _out = out
        if (start.status && _toRetry.status) {
            _toRetry = {}
            retry.stop()
        }
        const old = Object.assign({}, _oldDue)
        for (const kind in start)
            delete old[kind]
        _oldDue = old
        if (start.jobs || start.confirmations)
            _awayLoaded = false
        for (const kind in start)
            _calls[kind] = []
        if (start.jobs)
            readJobs()
        if (start.confirmations)
            readConfirmations()
        if (start.cards)
            readCards()
        if (start.status)
            readStatus()
    }

    // A read of this kind is in.
    function landed(kind) {
        const out = Object.assign({}, _out)
        delete out[kind]
        _out = out
        const grew = kind === "jobs" || kind === "confirmations" ? updateAway() : false
        if (kind === "jobs" && _confirming)
            finishSeen(grew)
        if (Object.keys(_toRetry).length > 0) {
            if (!retry.running) {
                _retryMs = _retryMs === 0 ? 1000 : Math.min(_retryMs * 2, 30000)
                retry.interval = _retryMs
                retry.start()
            }
        } else if (_open === 0) {
            _retryMs = 0
        }
        if (anyDue())
            requestRead()
    }

    function readJobs() {
        const since = workspaceNow() !== "" ? lastSeen : ""
        const fresh = _fresh
        const startedAt = seenStamp(Date.now())
        _fresh = false
        applyPendingJobs()
        live.beginRead(fresh)
        let open = since !== "" ? 4 : 3
        let gone = false
        const sincePages = []
        const queuedPages = []
        let cut = false
        const done = () => {
            if (gone || --open > 0)
                return
            root.applyPendingJobs()
            live.commit(Date.now())
            const capped = JSON.parse(live.overview("image")).capped
            root.jobsCapped = capped === null || capped === undefined ? -1 : capped
            root._awayPages = "[" + sincePages.concat(queuedPages).join(",") + "]"
            root.awayCut = cut
            root._seenUpTo = startedAt
            root.setRead("jobs", "ok")
            root.landed("jobs")
        }
        const fail = e => {
            if (gone)
                return
            gone = true
            root._fresh = root._fresh || fresh
            root.setRead("jobs", "failed", e)
            root.landed("jobs")
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
        list({since: since}, json => {
            sincePages.push(json)
            const next = ShellState.nextCursor(json)
            if (next !== "" && sincePages.length >= root.awayPages) {
                cut = true
                return ""
            }
            return next
        })
    }

    function track(kind, call) {
        _calls[kind].push(call)
    }

    function readConfirmations() {
        track("confirmations", CoreCall.appCall(core, "app/confirmations.list", {state: "pending"}, {
            owner: root,
            ok: r => {
                root._pending = r.confirmations || []
                root.setRead("confirmations", "ok")
                root.landed("confirmations")
            },
            error: e => {
                root.setRead("confirmations", "failed", e)
                root.landed("confirmations")
            }
        }))
    }

    // `projects.list` and `assets.query`.
    function readCards() {
        let open = 2
        const done = () => {
            if (--open === 0)
                root.landed("cards")
        }
        track("cards", CoreCall.call(core, "projects.list", {}, {
            owner: root,
            ok: r => {
                root.projects = (r.structured || {}).projects || []
                root.setRead("projects", "ok")
                done()
            },
            error: e => {
                root.setRead("projects", "failed", e)
                done()
            }
        }))
        track("cards", CoreCall.call(core, "assets.query", {sort: {field: "updated", dir: "desc"}, limit: 12}, {
            owner: root,
            ok: r => {
                root.assets = (r.structured || {}).assets || []
                root.setRead("assets", "ok")
                done()
            },
            error: e => {
                root.setRead("assets", "failed", e)
                done()
            }
        }))
    }

    // The session's project, never named: naming one would move it and the workspace's hint.
    function readStatus() {
        _statusAt = Date.now()
        track("status", CoreCall.call(core, "project.status", {}, {
            owner: root,
            ok: r => {
                const s = r.structured || {}
                root.byStatus = s.assets_by_status || {}
                root.statusProject = s.project || ""
                root.setRead("status", "ok")
                root.landed("status")
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
                root.landed("status")
            }
        }))
    }

    function cancelReads() {
        for (const kind in _calls) {
            for (const call of _calls[kind])
                call.cancel()
        }
        _calls = {}
        for (const id in _raw) {
            try {
                core.cancel(id)
            } catch (e) {
                // The core object went first.
            }
        }
        _raw = {}
        _out = {}
        _dueSince = 0
        coalesce.stop()
        retry.stop()
        statusGate.stop()
        _confirming = false
    }

    // --- Events. ---

    function onJobsEvent(kind, data) {
        if (kind !== "event") {
            _fresh = true
            reload()
            read()
        } else if (data && data.kind === "job" && data.job) {
            _pendingJobs.push(data.job)
            if (!jobFrame.running)
                jobFrame.start()
        }
    }

    function onConfirmationsEvent(kind) {
        if (kind !== "event") {
            reload()
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
        if (ended)
            dueStatusSoon()
        requestRead()
    }

    // --- The card. ---

    function lineKey(item) {
        return item.kind + "|" + item.id + "|" + item.at
    }

    // Works out the card's lines; whether there is a new one.
    function updateAway() {
        let grew = false
        if (!awayOn) {
            awayItems = []
        } else {
            const items = JSON.parse(ShellState.awayItems(_awayPages, JSON.stringify(_pending), lastSeen))
                              .filter(item => !_dismissed[lineKey(item)])
            const known = {}
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
        _awayLoaded = readState.jobs === "ok" && !_out.jobs && !_out.confirmations
                      && (!awayOn || readState.confirmations === "ok")
        return grew
    }

    // The wait is over: one more jobs read, whose start is what the time may move to.
    function confirmSeen() {
        _confirming = true
        due({jobs: true})
        read()
    }

    // That read is in. `_unseen` stays as it is unless all is well, and the wait starts again.
    function finishSeen(grew) {
        _confirming = false
        if (readState.jobs === "ok" && !grew && !awayCut && onScreen && Window.active && connectedNow())
            writeSeen(_seenUpTo)
    }

    // Not written, it stays unseen: the wait starts again, and so the next mark tries again.
    function writeSeen(at) {
        if (stateDir !== "" && workspaceNow() !== "" && ShellState.setLastSeen(stateDir, workspaceNow(), at) !== "") {
            _writesFailed++
            return
        }
        _writesFailed = 0
        _unseen = false
        _wroteSeen = true
    }

    // Dismiss: these lines are done with. Both times move to when the read they came from began, unless
    // a jobs read is due, out or failing: then nothing has shown what came since, and they stay.
    function dismissAway() {
        const dismissed = Object.assign({}, _dismissed)
        for (const item of awayItems)
            dismissed[lineKey(item)] = true
        _dismissed = dismissed
        const clear = readState.jobs === "ok" && !_out.jobs && !_due.jobs && !_toRetry.jobs && _pendingJobs.length === 0
        if (clear && _seenUpTo !== "") {
            writeSeen(_seenUpTo)
            lastSeen = _seenUpTo
            awayCut = false
        }
        // The usual write follows, past the second of that read: a later start won't remember these.
        _unseen = true
        updateAway()
    }

    // The time to count from in this workspace: see the header for one ahead of now.
    function keptSince(workspace) {
        const kept = ShellState.lastSeen(stateDir, workspace)
        return kept !== "" && ShellState.timeMs(kept) > Date.now() ? seenStamp(Date.now() - aheadLookBackMs) : kept
    }

    // Home has been away long enough: the card now counts from the kept time.
    function moveCardTime() {
        _moveTime = false
        _wroteSeen = false
        // One that can't be read just now leaves the card's own: as a first run it would lose lines.
        const kept = keptSince(workspaceNow())
        if (kept !== "")
            lastSeen = kept
        _unseen = true
        _awayLoaded = false
        due({jobs: true, confirmations: true})
    }

    function startWorkspace() {
        _workspaceAt = workspaceNow()
        lastSeen = _workspaceAt !== "" ? keptSince(_workspaceAt) : ""
        _seenUpTo = ""
        _statusAt = 0
        _unseen = true
        _wroteSeen = false
        _moveTime = false
        _awayLoaded = false
        _confirming = false
        _writesFailed = 0
        _known = {}
        _dismissed = {}
    }

    // The top bar's switcher has set the session's project: the counts asked before that answered for
    // whatever the hint says.
    function contextSet() {
        due({status: true})
        requestRead()
    }

    onOnScreenChanged: {
        if (!onScreen)
            return
        if (_moveTime)
            moveCardTime()
        const waiting = anyDue() || Object.keys(_toRetry).length > 0
        if (_readOnShow) {
            _readOnShow = false
            due({jobs: true, confirmations: true, cards: true})
            dueStatusSoon()
        }
        if (waiting)
            read()
        else if (anyDue())
            requestRead()
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
                root.reload()
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
            root._oldDue = {}
            root._toRetry = {}
            live.beginRead(true)
            live.commit(Date.now())
            root.startWorkspace()
            root.dueEverything()
            root.read()
        }
        function onProjectChanged() {
            root.due({status: true})
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
            root.due(root._toRetry)
            root._toRetry = {}
            root.read()
        }
    }

    // The last read of the counts after job ends.
    Timer {
        id: statusGate
        onTriggered: {
            root.due({status: true})
            root.read()
        }
    }

    // One pass for the job events of a frame.
    Timer {
        id: jobFrame
        interval: 16
        onTriggered: root.applyPendingJobs()
    }

    // Qt signals `active` only on the window that gains or loses the focus, while a floating Home's
    // follows the app's other windows too, so a binding would go stale: it is looked at while it matters.
    Timer {
        interval: 250
        repeat: true
        triggeredOnStart: true
        running: root.onScreen && root._unseen
        onTriggered: root._activeNow = root.Window.active
    }

    // After this long on screen in the active window, the read that moves the time.
    Timer {
        id: seenTimer
        interval: root.seenAfterMs
        running: root.onScreen && root._activeNow && root.connected && root.hasWorkspace && root.stateDir !== ""
                 && root._awayLoaded && root._unseen && !root.awayCut
        onTriggered: root.confirmSeen()
    }

    // A float hides Home for a moment; this long off screen is really away.
    Timer {
        interval: root.seenAfterMs
        running: !root.onScreen && root._wroteSeen
        onTriggered: root._moveTime = true
    }

    // Off screen for longer than a float's moment: everything is read when it shows.
    Timer {
        interval: 1000
        running: !root.onScreen
        onTriggered: root._readOnShow = true
    }

    // Keeps the item with the keyboard focus in view.
    Connections {
        target: root.Window.window
        function onActiveFocusItemChanged() {
            const item = root.Window.window ? root.Window.window.activeFocusItem : null
            for (let p = item; p; p = p.parent) {
                if (p === scroller.contentItem) {
                    const y = item.mapToItem(scroller.contentItem, 0, 0).y
                    if (y < scroller.contentY)
                        scroller.contentY = Math.max(0, y - Theme.spacing)
                    else if (y + item.height > scroller.contentY + scroller.height)
                        scroller.contentY = Math.min(scroller.contentHeight - scroller.height,
                                                     y + item.height - scroller.height + Theme.spacing)
                    return
                }
            }
        }
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
                onDismissed: hadFocus => {
                    if (hadFocus)
                        scroller.forceActiveFocus(Qt.OtherFocusReason)
                }
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

    // Where the cards are scrolled to, and that there is more.
    Rectangle {
        id: scrollBar
        visible: scroller.visibleArea.heightRatio < 1
        x: scroller.x + scroller.width - width
        y: scroller.y + scroller.visibleArea.yPosition * scroller.height
        width: 3
        height: Math.max(16, scroller.visibleArea.heightRatio * scroller.height)
        radius: 1
        color: Theme.textMuted
    }

    // The keyboard focus on the cards.
    Rectangle {
        id: focusRing
        visible: scroller.activeFocus
        anchors.fill: scroller
        color: "transparent"
        border.color: Theme.focus
        border.width: Theme.focusWidth
    }
}
