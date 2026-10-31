// The top bar (APP-006, ARCH-098, P4a-R34): its indicators, the pause control, the project and
// workspace switchers, and its place in the shell. The counts come from what a read answers, never
// from counting events, so each test answers the `jobs.list` reads itself.
import QtQuick
import QtTest
import Lilith.Core
import Lilith.Shim
import Lilith.UiKit

TestCase {
    id: tc
    name: "TopBar"
    when: windowShown
    width: 200
    height: 100

    readonly property var workspaceOne: ({id: "ws1", path: "D:/art/harbor"})

    Component {
        id: fakeComponent
        FakeCore {}
    }

    Component {
        id: barWindow
        Window {
            id: win
            required property var core
            property alias bar: bar
            width: 1280
            height: 400
            visible: true
            color: Theme.bg
            TopBar {
                id: bar
                width: parent.width
                core: win.core
            }
        }
    }

    Component {
        id: shellComponent
        ShellWindow {}
    }

    A11yWalk { id: walk }

    // Request ids answered in this test.
    property var answered: ({})
    // The windows go before their FakeCore, so their subscriptions end on a live core.
    property var windows: []

    function init() {
        answered = {}
        tryVerify(() => {
            const kddw = JSON.parse(DockLayout.serialize())
            return DockLayout.mainWindows().length === 0
                   && !(Array.isArray(kddw.allDockWidgets) && kddw.allDockWidgets.length > 0)
        }, 5000, "the earlier shell's docks are gone")
    }

    function cleanup() {
        for (const w of windows)
            w.destroy()
        windows = []
        wait(0)
    }

    function ok(structured) {
        return {status: "ok", structured: structured}
    }

    function refusal(code, message) {
        return {status: "error", error: {code: code, category: "user_fixable", message: message,
                                         trace_id: "t_test", retry: "never"}}
    }

    function iso(msAgo) {
        return new Date(Date.now() - (msAgo || 0)).toISOString().replace(/\.\d+Z$/, "Z")
    }

    function job(id, status, over) {
        return Object.assign({job_id: id, kind: "comfy.prompt", lane: "gpu-heavy", priority: "user",
                              status: status, substatus: status === "running" ? "executing" : "waiting",
                              created: iso(60000), updated: iso(), attempt: 1, trace_id: "t_" + id,
                              batch_id: "b_" + id, index: 0, origin: {surface: "panel", id: "image"}}, over || {})
    }

    function lane(over) {
        return Object.assign({lane: "gpu-heavy", paused: false, reasons: [], held: 0, restart_held: 0, withdrawn: 0},
                             over || {})
    }

    function open(core, test) {
        return core.calls.filter(c => !answered[c.id] && test(c))
    }

    function answer(core, test, outcome, what) {
        tryVerify(() => open(core, test).length > 0, 2000, what || "the call")
        const c = open(core, test)[0]
        answered[c.id] = true
        verify(core.respond(c.id, outcome), "request " + c.id + " is open")
        return c
    }

    function named(name) {
        return c => c.name === name
    }

    function isRunningRead(c) {
        return c.name === "jobs.list" && c.args.status === "running"
    }

    function isQueueRead(c) {
        return c.name === "jobs.list" && c.args.order === "queue"
    }

    // One read of the live jobs: the running jobs, the queue, and the lanes.
    function answerRead(core, running, queued, lanes) {
        answer(core, isRunningRead, ok({jobs: running, next_cursor: null, lanes: lanes || [lane()]}), "the running read")
        answer(core, isQueueRead, ok({jobs: queued, next_cursor: null, lanes: lanes || [lane()]}), "the queue read")
    }

    // Answers every read still out, in a shell where the Jobs panel reads too.
    function answerAllReads(core, lanes) {
        for (let n = 0; n < 20 && open(core, named("jobs.list")).length > 0; ++n)
            answer(core, named("jobs.list"), ok({jobs: [], next_cursor: null, lanes: lanes || [lane()]}))
    }

    // A bar in a window of its own, its first read and caps answered.
    function load(setup) {
        const s = setup || {}
        const core = createTemporaryObject(fakeComponent, tc, {workspace: s.workspace === undefined ? workspaceOne : s.workspace})
        const w = createTemporaryObject(barWindow, tc, {core: core})
        windows.push(w)
        tryVerify(() => w.bar.width > 0)
        if (!s.unanswered) {
            answerRead(core, s.running || [], s.queued || [], s.lanes)
            answer(core, named("caps.get"), ok({display_currency: "USD", caps: s.caps || [], period: "2026-10"}), "caps.get")
        }
        return {core: core, bar: w.bar, window: w}
    }

    function sample(gpu) {
        return {kind: "sample", snapshot: {ts: iso(), gpu: gpu}}
    }

    function gpuReading(over) {
        return Object.assign({name: "GPU", util_pct: 63, vram_used_mib: 9728, vram_total_mib: 16384, temp_c: 71,
                              measured: true}, over || {})
    }

    function capRow(cap, permille, over) {
        return Object.assign({cap: cap, limit: "100", settled: "1", held: "0", ratio_permille: permille}, over || {})
    }

    // Visits every item under `item`; the first one `test` accepts.
    function findItem(item, test) {
        if (test(item))
            return item
        for (let i = 0; i < item.children.length; ++i) {
            const found = findItem(item.children[i], test)
            if (found)
                return found
        }
        return null
    }

    function eachItem(item, fn) {
        fn(item)
        for (let i = 0; i < item.children.length; ++i)
            eachItem(item.children[i], fn)
    }

    function rowIn(popup, test) {
        const kids = popup.view.contentItem.children
        for (let i = 0; i < kids.length; ++i) {
            if (kids[i].modelData !== undefined && test(kids[i].modelData))
                return kids[i]
        }
        return null
    }

    // The list makes its rows as it lays out.
    function row(popup, test) {
        tryVerify(() => rowIn(popup, test) !== null, 2000, "the row")
        return rowIn(popup, test)
    }

    function projectRows() {
        return [{project: "sketchbook", name: "Sketchbook", status: "active", assets: 3},
                {project: "harbor", name: "Harbor", status: "active", assets: 1234}]
    }

    function openProjects(t, rows) {
        const sw = t.bar.projectSwitcher
        mouseClick(sw.button)
        tryVerify(() => sw.popup.opened)
        answer(t.core, named("projects.list"), ok({projects: rows || projectRows()}), "projects.list")
        tryVerify(() => sw.popup.view.count === (rows || projectRows()).length)
        wait(0)
        return sw
    }

    function workspaceRows() {
        return [{id: "ws1", path: "D:/art/harbor", name: "Harbor", default: true, served: true, missing: false},
                {id: "ws2", path: "D:/art/quay", name: "Quay", default: false, served: true, missing: false},
                {id: "ws3", path: "D:/art/other", name: "Other", default: false, served: false, missing: false},
                {id: "ws4", path: "D:/art/gone", default: false, served: false, missing: true},
                {id: "ws5", path: "D:/art/file.txt", default: false, served: false, missing: false, problem: "not_a_folder"},
                {id: "ws6", path: "D:/art/locked", default: false, served: false, missing: false, problem: "unreadable"}]
    }

    function openWorkspaces(t) {
        const sw = t.bar.workspaceSwitcher
        mouseClick(sw.button)
        tryVerify(() => sw.popup.opened)
        answer(t.core, named("workspaces.list"), ok({workspaces: workspaceRows()}), "workspaces.list")
        tryVerify(() => sw.popup.view.count === workspaceRows().length)
        wait(0)
        return sw
    }

    function test_indicators_follow_events() { // spec: APP-006, APP-027
        const t = load()
        compare(t.bar.jobsIndicator.text, "0 running · 0 queued")
        compare(t.bar.gpuIndicator.text, "GPU —")
        verify(!t.bar.spendIndicator.visible, "no caps, no figure")

        // A `jobs` event is one job: the counts are what the read it starts answers.
        t.core.emit("jobs", "event", {kind: "job", job: job("q9", "queued")})
        answerRead(t.core, [job("r1", "running"), job("r2", "running")],
                   [job("q1", "queued"), job("q2", "queued"), job("q9", "queued")])
        tryCompare(t.bar.jobsIndicator, "text", "2 running · 3 queued")

        t.core.emit("telemetry", "event", sample(gpuReading()))
        compare(t.bar.gpuIndicator.text, "GPU 63 % · 9.5/16 GiB · 71 °C")

        t.core.emit("caps", "event", {kind: "changed"})
        answer(t.core, named("caps.get"), ok({display_currency: "USD", caps: [capRow("monthly", 412)], period: "2026-10"}))
        tryCompare(t.bar.spendIndicator, "text", "41 % of cap")
        verify(t.bar.spendIndicator.visible)

        compare(t.bar.connectionIndicator.text, "Connected")
    }

    function test_counts_wait_for_the_first_read() { // spec: APP-006
        const t = load({unanswered: true})
        compare(t.bar.jobsIndicator.text, "—", "never a count before the read")
        answerRead(t.core, [job("r1", "running")], [])
        tryCompare(t.bar.jobsIndicator, "text", "1 running · 0 queued")
    }

    function test_a_burst_of_job_events_is_one_read_at_a_time() { // spec: APP-006
        const t = load()
        const reads = () => t.core.calls.filter(isRunningRead).length
        const before = reads()
        for (let i = 0; i < 5; ++i)
            t.core.emit("jobs", "event", {kind: "job", job: job("q" + i, "queued")})
        compare(reads(), before, "not before the 150 ms")
        tryCompare(t.bar.jobs, "_reading", true, 2000)
        compare(reads(), before + 1, "one read for the burst")

        // While it is out, another event only asks for one more.
        t.core.emit("jobs", "event", {kind: "job", job: job("q7", "queued")})
        t.core.emit("jobs", "event", {kind: "job", job: job("q8", "queued")})
        wait(300)
        compare(reads(), before + 1, "at most one read in flight")
        answerRead(t.core, [], [job("q0", "queued")])
        tryVerify(() => reads() === before + 2, 2000, "the events that came meanwhile read once more")
        answerRead(t.core, [], [job("q0", "queued")])
        wait(300)
        compare(reads(), before + 2)
        compare(t.bar.jobsIndicator.text, "0 running · 1 queued", "the read says, not the events")
    }

    function test_a_steady_stream_of_events_still_reads() { // spec: APP-006
        const t = load()
        const before = t.core.calls.filter(isRunningRead).length
        for (let i = 0; i < 16 && t.core.calls.filter(isRunningRead).length === before; ++i) {
            t.core.emit("jobs", "event", {kind: "job", job: job("s" + i, "queued")})
            wait(100)
        }
        verify(t.core.calls.filter(isRunningRead).length > before, "a read within about a second")
    }

    function test_a_resync_reads_afresh_and_ready_does_not() { // spec: ARCH-100
        const t = load({queued: [job("q1", "queued")]})
        tryCompare(t.bar.jobsIndicator, "text", "0 running · 1 queued")
        const before = t.core.calls.filter(isRunningRead).length
        t.core.connection = "reconnecting"
        t.core.connection = "ready"
        wait(300)
        compare(t.core.calls.filter(isRunningRead).length, before, "`ready` reads nothing")
        t.core.emit("jobs", "resync_required", null)
        answerRead(t.core, [], [])
        tryCompare(t.bar.jobsIndicator, "text", "0 running · 0 queued")
    }

    function test_a_failed_read_tries_again() { // spec: APP-006
        const t = load({unanswered: true})
        answer(t.core, isRunningRead, refusal("core.unreachable", "No core."))
        answer(t.core, isQueueRead, refusal("core.unreachable", "No core."))
        compare(t.bar.jobsIndicator.text, "—")
        tryVerify(() => open(t.core, isRunningRead).length > 0, 3000, "read again")
        answerRead(t.core, [job("r1", "running")], [])
        tryCompare(t.bar.jobsIndicator, "text", "1 running · 0 queued")
    }

    function test_missing_telemetry_is_a_dash_never_0() { // spec: APP-006
        const t = load()
        t.core.emit("telemetry", "event", sample(null))
        compare(t.bar.gpuIndicator.text, "GPU —", "no GPU")
        t.core.emit("telemetry", "event", sample(gpuReading({measured: false})))
        compare(t.bar.gpuIndicator.text, "GPU —", "a lost GPU's last reading isn't shown as live")
        t.core.emit("telemetry", "event", sample(gpuReading()))
        compare(t.bar.gpuIndicator.text, "GPU 63 % · 9.5/16 GiB · 71 °C")
        t.core.connection = "reconnecting"
        compare(t.bar.gpuIndicator.text, "GPU —", "no live reading without a connection")
        t.core.connection = "ready"
        t.core.emit("telemetry", "resync_required", null)
        compare(t.bar.gpuIndicator.text, "GPU —", "a new core's readings start afresh")
    }

    function test_spend_shows_the_highest_cap_ratio() { // spec: APP-006, CORE-008
        const t = load({caps: [capRow("per_call_max", 120), capRow("monthly", 987), capRow("provider:x", 412)]})
        compare(t.bar.spendIndicator.text, "98 % of cap", "the highest, rounded down to a whole percent")

        // A cap that can't be measured isn't a ratio.
        const reads = () => t.core.calls.filter(named("caps.get")).length
        const before = reads()
        t.core.emit("caps", "event", {kind: "invalid"})
        t.core.emit("usage", "event", {kind: "recorded"})
        t.core.emit("caps", "event", {kind: "alert"})
        answer(t.core, named("caps.get"), ok({display_currency: "USD", period: "2026-10",
                                              caps: [capRow("monthly", 9999, {invalid: true}), capRow("provider:x", 412)]}))
        compare(reads(), before + 1, "one read for the three events")
        tryCompare(t.bar.spendIndicator, "text", "41 % of cap")

        // A job's end can spend without any caps event.
        t.core.emit("jobs", "event", {kind: "job", job: job("r1", "succeeded")})
        answer(t.core, named("caps.get"), ok({display_currency: "USD", caps: [capRow("monthly", 1500)], period: "2026-10"}))
        tryCompare(t.bar.spendIndicator, "text", "150 % of cap")

        // A usage event alone reads too; no caps shows nothing, not 0.
        t.core.emit("usage", "event", {kind: "recorded"})
        answer(t.core, named("caps.get"), ok({display_currency: "USD", caps: [], period: "2026-10"}))
        tryVerify(() => !t.bar.spendIndicator.visible)
    }

    function test_paused_badge_names_the_reasons() { // spec: APP-006, CORE-160
        const t = load()
        verify(!t.bar.pausedHolder.visible, "no badge while nothing is paused")
        const before = t.bar.jobs.laneUpdates
        t.core.emit("jobs", "event", {kind: "queue", state: lane({paused: true, reasons: ["fullscreen"]})})
        t.core.emit("jobs", "event", {kind: "queue", state: lane({paused: true, reasons: ["manual", "fullscreen"]})})
        tryCompare(t.bar.pausedBadge, "text", "Paused by you and while fullscreen")
        verify(t.bar.pausedHolder.visible)
        compare(t.bar.jobs.laneUpdates, before + 1, "two events in a frame are one update")
        compare(t.bar.pauseButton.text, "Resume GPU work", "your own pause is set")

        t.core.emit("jobs", "event", {kind: "queue", state: lane({paused: true, reasons: ["fullscreen"]})})
        tryCompare(t.bar.pausedBadge, "text", "Paused while fullscreen")
        compare(t.bar.pauseButton.text, "Pause GPU work", "the fullscreen pause isn't yours to resume")

        t.core.emit("jobs", "event", {kind: "queue", state: lane({paused: true, reasons: ["<b>director</b>"]})})
        tryCompare(t.bar.pausedBadge, "text", "Paused by <b>director</b>")

        // Another lane's pause shows too.
        t.core.emit("jobs", "event", {kind: "queue", state: lane()})
        t.core.emit("jobs", "event", {kind: "queue", state: lane({lane: "cpu", paused: true, reasons: ["manual"]})})
        tryCompare(t.bar.pausedBadge, "text", "Paused by you")
        compare(t.bar.pauseButton.text, "Pause GPU work", "the button is the GPU lane's")

        t.core.emit("jobs", "event", {kind: "queue", state: lane({lane: "cpu"})})
        tryVerify(() => !t.bar.pausedHolder.visible)
    }

    function test_a_read_does_not_undo_a_newer_queue_event() { // spec: CORE-160
        const t = load()
        t.core.emit("jobs", "resync_required", null)
        tryCompare(t.bar.jobs, "_reading", true, 2000)
        t.core.emit("jobs", "event", {kind: "queue", state: lane({paused: true, reasons: ["manual"]})})
        // Its frame passes first, so only the read could undo it.
        tryCompare(t.bar.pausedBadge, "text", "Paused by you")
        wait(50)
        answerRead(t.core, [], [], [lane()])
        tryCompare(t.bar.pausedBadge, "text", "Paused by you")
        wait(50)
        verify(t.bar.yourPause, "the event came after the read's lanes")
    }

    function test_pause_control_refusal_and_arguments() { // spec: APP-011, CORE-160
        const t = load({unanswered: true})
        t.bar.togglePause()
        compare(t.core.calls.filter(c => c.kind === "appCall").length, 0, "held until the state has loaded")
        verify(!t.bar.pauseButton.enabled)
        answerRead(t.core, [], [])
        tryVerify(() => t.bar.pauseButton.enabled)

        t.bar.togglePause()
        t.bar.togglePause()
        compare(t.core.calls.filter(named("app/queue.pause")).length, 1, "one call at a time")
        const pause = t.core.calls.filter(named("app/queue.pause"))[0]
        compare(pause.args, {lane: "gpu-heavy"}, "QueueControls' arguments")
        answer(t.core, named("app/queue.pause"), {status: "ok", lane: "gpu-heavy", paused: true, reasons: ["manual"],
                                                  held: 0, restart_held: 0, withdrawn: 0})
        compare(t.bar.pauseButton.text, "Resume GPU work")

        t.bar.togglePause()
        const resume = t.core.calls.filter(named("app/queue.resume"))[0]
        compare(resume.args, {lane: "gpu-heavy"})
        answer(t.core, named("app/queue.resume"), refusal("caller.not_allowed", "<b>Not</b> now."))
        compare(t.bar.pauseButton.text, "Resume GPU work", "a refusal changes nothing")
        verify(t.bar.notice.visible)
        compare(t.bar.notice.text, "Couldn't resume GPU work: <b>Not</b> now.")
        compare(t.bar.notice.kind, "danger")
    }

    function test_workspace_switch_rehellos_and_keeps_the_default() { // spec: APP-006, ARCH-030
        const t = load()
        compare(t.bar.workspaceSwitcher.button.text, "harbor ▾", "the folder's name until the list says more")
        const sw = openWorkspaces(t)
        compare(t.bar.workspaceSwitcher.currentName, "Harbor")

        const quay = row(sw.popup, w => w.id === "ws2")
        verify(quay, "the Quay row")
        mouseClick(quay, 5, 5)
        const switches = t.core.calls.filter(c => c.kind === "setWorkspace")
        compare(switches.length, 1)
        compare(switches[0].name, "ws2", "Core.setWorkspace with the row's id")
        compare(t.core.calls.filter(named("workspace.set_default")).length, 0, "P4a-R34: a switch never sets the default")

        // The core refuses: nothing shown changes.
        answered[switches[0].id] = true
        t.core.respond(switches[0].id, refusal("workspace.unsupported", "This core serves \"Harbor\"."))
        compare(t.bar.workspaceSwitcher.currentName, "Harbor")
        verify(sw.popup.opened, "it stays open to say why")
        compare(sw.popup.message, "Couldn't switch the workspace: This core serves \"Harbor\".")
        compare(t.core.workspace.id, "ws1")

        // "Make this the default" is its own click.
        const button = findItem(quay, i => i.text === "Make this the default")
        verify(button, "its button")
        mouseClick(button)
        const calls = t.core.calls.filter(named("workspace.set_default"))
        compare(calls.length, 1)
        compare(calls[0].args, {path: "D:/art/quay"})
        compare(t.core.calls.filter(c => c.kind === "setWorkspace").length, 1, "it switches nothing")
        answer(t.core, named("workspace.set_default"), ok({id: "ws2", path: "D:/art/quay"}))
        answer(t.core, named("workspaces.list"), ok({workspaces: workspaceRows()}), "the list is read again")

        // A switch that works closes the list; the hello's workspace says what is current.
        mouseClick(row(sw.popup, w => w.id === "ws2"), 5, 5)
        const again = open(t.core, c => c.kind === "setWorkspace")
        compare(again.length, 1)
        answered[again[0].id] = true
        t.core.respond(again[0].id, {status: "ok", workspace: {id: "ws2", path: "D:/art/quay"}})
        tryVerify(() => !sw.popup.visible)
    }

    function test_unavailable_workspaces_say_why_and_cannot_be_chosen() { // spec: APP-006, ARCH-090
        const t = load()
        const sw = openWorkspaces(t)
        const reasons = {ws3: "This core serves only the default workspace", ws4: "The folder is gone",
                         ws5: "It isn't a folder", ws6: "It can't be read"}
        for (const id in reasons) {
            const r = row(sw.popup, w => w.id === id)
            verify(r, id)
            verify(LilShim.accessibleOf(r).name.indexOf(reasons[id]) >= 0, LilShim.accessibleOf(r).name)
            verify(LilShim.accessibleOf(r).name.indexOf("unavailable") >= 0, LilShim.accessibleOf(r).name)
            verify(findItem(r, i => i.text === reasons[id]), "the row shows: " + reasons[id])
            mouseClick(r, 5, 5)
        }
        compare(t.core.calls.filter(c => c.kind === "setWorkspace").length, 0, "none of them can be chosen")
        // A folder that isn't there can't be the default; an unserved one that is there can.
        verify(findItem(row(sw.popup, w => w.id === "ws3"), i => i.text === "Make this the default" && i.visible))
        verify(!findItem(row(sw.popup, w => w.id === "ws4"), i => i.text === "Make this the default" && i.visible))
        verify(!findItem(row(sw.popup, w => w.id === "ws5"), i => i.text === "Make this the default" && i.visible))
        // Nor is the default offered again.
        verify(!findItem(row(sw.popup, w => w.id === "ws1"), i => i.text === "Make this the default" && i.visible))

        // Choosing the current one changes nothing.
        mouseClick(row(sw.popup, w => w.id === "ws1"), 5, 5)
        compare(t.core.calls.filter(c => c.kind === "setWorkspace").length, 0)
        tryVerify(() => !sw.popup.visible)
    }

    function test_project_switch_calls_set_context() { // spec: APP-006, ARCH-088
        const t = load()
        const sw = openProjects(t)
        mouseClick(row(sw.popup, p => p.project === "harbor"), 5, 5)
        const calls = t.core.calls.filter(named("app/set_context"))
        compare(calls.length, 1)
        compare(calls[0].kind, "appCall")
        compare(calls[0].args, {project: "harbor"})
        compare(sw.current, "sketchbook", "not before the core answers")
        answer(t.core, named("app/set_context"), {status: "ok", workspace: workspaceOne, project: "harbor"})
        compare(t.core.project, "harbor", "the answer reaches Core.project")
        compare(sw.button.text, "Harbor ▾")
        tryVerify(() => !sw.popup.visible)

        // A refusal shows, and the project stays.
        openProjects(t)
        mouseClick(row(sw.popup, p => p.project === "sketchbook"), 5, 5)
        answer(t.core, named("app/set_context"), refusal("project.unknown", "No such project."))
        compare(t.core.project, "harbor")
        verify(sw.popup.opened)
        compare(sw.popup.message, "Couldn't switch the project: No such project.")

        // From the keyboard: the list has the focus, the arrows move and Enter chooses.
        tryVerify(() => sw.popup.view.activeFocus)
        sw.popup.view.currentIndex = 0
        keyClick(Qt.Key_Down)
        compare(sw.popup.view.currentIndex, 1)
        keyClick(Qt.Key_Return)
        const last = t.core.calls.filter(named("app/set_context")).pop()
        compare(last.args, {project: "harbor"})
        answer(t.core, named("app/set_context"), {status: "ok", workspace: workspaceOne, project: "harbor"})
    }

    function test_a_switcher_button_toggles_its_list() { // spec: APP-006
        const t = load()
        const sw = openProjects(t)
        mouseClick(sw.button)
        tryVerify(() => !sw.popup.visible, 2000, "its own button closes it")
        openProjects(t)
        mouseClick(t.window.contentItem, 900, 300)
        tryVerify(() => !sw.popup.visible, 2000, "a press elsewhere closes it")
        openProjects(t)
        keyClick(Qt.Key_Escape)
        tryVerify(() => !sw.popup.visible, 2000, "and Escape")
    }

    function test_sketchbook_is_the_default_project() { // spec: ARCH-098, APP-015
        const t = load()
        compare(t.core.project, "")
        compare(t.bar.projectSwitcher.current, "sketchbook")
        compare(t.bar.projectSwitcher.button.text, "sketchbook ▾")
        compare(t.core.calls.filter(named("app/set_context")).length, 0, "nothing is asked of the core at start")
        const sw = openProjects(t)
        verify(row(sw.popup, p => p.project === "sketchbook").isCurrent)
    }

    function test_project_needs_a_workspace() { // spec: APP-016
        const t = load({workspace: null})
        verify(!t.bar.projectSwitcher.button.enabled, "a project is set within a workspace")
        verify(t.bar.workspaceSwitcher.button.enabled, "the workspaces can still be listed")
        compare(t.bar.workspaceSwitcher.button.text, "No workspace ▾")
    }

    function test_a_reconnect_sends_the_project_again() { // spec: ARCH-088, APP-035
        const t = load()
        t.core.project = "harbor"
        t.core.connection = "reconnecting"
        wait(0)
        compare(t.core.calls.filter(named("app/set_context")).length, 0)
        t.core.connection = "ready"
        tryVerify(() => t.core.calls.filter(named("app/set_context")).length === 1, 2000, "the new session has no project yet")
        compare(t.core.calls.filter(named("app/set_context"))[0].args, {project: "harbor"})
        answer(t.core, named("app/set_context"), {status: "ok", workspace: workspaceOne, project: "harbor"})
        compare(t.core.project, "harbor")

        // One that is gone now is dropped.
        t.core.connection = "reconnecting"
        t.core.connection = "ready"
        answer(t.core, named("app/set_context"), refusal("project.unknown", "No such project."))
        compare(t.core.project, "")
        compare(t.bar.projectSwitcher.current, "sketchbook")

        // With none chosen nothing is sent.
        const before = t.core.calls.filter(named("app/set_context")).length
        t.core.connection = "reconnecting"
        t.core.connection = "ready"
        wait(50)
        compare(t.core.calls.filter(named("app/set_context")).length, before)
    }

    function test_another_workspace_forgets_the_project() { // spec: ARCH-088
        const t = load()
        t.core.project = "harbor"
        t.core.workspace = {id: "ws1", path: "D:/art/harbor"}
        compare(t.core.project, "harbor", "the same workspace, as each hello sets it")
        t.core.workspace = {id: "ws2", path: "D:/art/quay"}
        compare(t.core.project, "")
        compare(t.bar.workspaceSwitcher.button.text, "quay ▾")
    }

    function test_app_switcher_leads_with_the_plain_name() { // spec: APP-164, APP-006
        const t = load()
        compare(t.bar.appName.children[0].text, "Image studio")
        compare(t.bar.appName.children[1].text, "ATHANOR")
        compare(LilShim.accessibleOf(t.bar.appName).name, "App: Image studio, ATHANOR")
    }

    function test_assistant_toggle_is_shown_disabled() { // spec: APP-006
        const t = load()
        verify(t.bar.assistantToggle.visible)
        verify(!t.bar.assistantToggle.enabled)
        verify(!t.bar.assistantToggle.checked)
        compare(t.bar.assistantReason, "The assistant arrives in a later version")
        compare(t.bar.assistantToggle.Accessible.description, t.bar.assistantReason)
        wait(50)
        mouseMove(t.bar.assistantToggle)
        tryVerify(() => t.bar.assistantTip.visible, 2000, "the pointer finds out why")
        compare(t.bar.assistantTip.text, t.bar.assistantReason)
    }

    function test_names_from_outside_are_plain_and_cut() { // spec: APP-156
        const t = load()
        const hostile = "<img src=\"http://localhost:9/x.png\"><b>big</b>" + "x".repeat(200)
        const sw = openProjects(t, [{project: "harbor", name: hostile, status: "<i>odd</i>", assets: 1}])
        const r = row(sw.popup, p => p.project === "harbor")
        verify(r.name.length <= 60, "cut: " + r.name.length)
        eachItem(sw.popup.contentItem, i => {
            if (i.textFormat !== undefined)
                compare(i.textFormat, Text.PlainText, "plain text: " + i.text)
        })
        const wsw = t.bar.workspaceSwitcher
        sw.popup.close()
        tryVerify(() => !sw.popup.visible)
        t.core.workspace = {id: "ws1", path: "D:/art/<img src=x>" + "y".repeat(200)}
        verify(wsw.button.text.length <= 62, "cut: " + wsw.button.text.length)
    }

    function test_the_bar_wraps_rather_than_hiding_a_control() { // spec: APP-156
        const t = load({lanes: [lane({paused: true, reasons: ["manual", "fullscreen"]})],
                        caps: [capRow("monthly", 412)]})
        t.core.emit("telemetry", "event", sample(gpuReading()))
        const wide = t.bar.height
        t.window.width = 640
        tryVerify(() => t.bar.height > wide, 2000, "a next line")
        wait(0)
        eachItem(t.bar, i => {
            if (i.visible && i.width > 0 && i.parent === t.bar.pauseButton.parent)
                verify(i.x + i.width <= t.bar.width + 1, "inside the bar: " + i + " at " + i.x)
        })
    }

    function test_the_top_bar_passes_the_walk() { // spec: APP-156
        const t = load({lanes: [lane({paused: true, reasons: ["manual"]})]})
        tryVerify(() => t.bar.pauseButton.enabled)
        verify(walk.interactiveCount(t.bar) >= 3, "the switchers and the pause: " + walk.interactiveCount(t.bar))
        compare(walk.report(t.bar), [])

        // Each list, open.
        const sw = openWorkspaces(t)
        tryVerify(() => walk.interactiveCount(sw.popup.contentItem) >= 5, 2000, "the list and its rows")
        compare(walk.report(sw.popup.contentItem), [])
        sw.popup.close()
        tryVerify(() => !sw.popup.visible)
        const psw = openProjects(t)
        tryVerify(() => walk.interactiveCount(psw.popup.contentItem) >= 3, 2000, "the list and its rows")
        compare(walk.report(psw.popup.contentItem), [])
    }

    function test_the_shell_holds_the_bar_and_wires_it() { // spec: APP-006, APP-011, APP-001
        const core = createTemporaryObject(fakeComponent, tc, {workspace: workspaceOne})
        const shell = createTemporaryObject(shellComponent, tc, {core: core})
        windows.push(shell)
        shell.visible = true
        tryVerify(() => shell.layoutSettled, 5000, "the docks are laid out")

        // Above the docks.
        verify(shell.topBar.height > 0)
        verify(shell.dockingArea.y >= shell.topBar.height, "the docking area starts under the bar")
        compare(shell.topBar.width, shell.width)

        // Ctrl+Shift+P's signal reaches the pause control, once the state has loaded.
        shell.gpuPauseToggleRequested()
        compare(core.calls.filter(named("app/queue.pause")).length, 0, "held until the state has loaded")
        answerAllReads(core)
        tryVerify(() => shell.topBar.jobs.loaded)
        shell.gpuPauseToggleRequested()
        compare(core.calls.filter(named("app/queue.pause")).length, 1)
        answer(core, named("app/queue.pause"), {status: "ok", lane: "gpu-heavy", paused: true, reasons: ["manual"],
                                                held: 0, restart_held: 0, withdrawn: 0})
        shell.gpuPauseToggleRequested()
        compare(core.calls.filter(named("app/queue.resume")).length, 1)

        // The window's title follows the project (APP-164).
        core.noteProject("harbor")
        compare(shell.title, "LILITH — harbor")

        // The event log runs from the shell's start, whichever panel opens later.
        verify(shell.eventLog, "a shell property")
        const logged = shell.eventLog.rowCount()
        core.emit("jobs", "event", {kind: "queue", state: lane()})
        compare(shell.eventLog.rowCount(), logged + 1)
        compare(shell.eventLog.get(logged).topic, "jobs")
    }

    function test_core_keeps_the_noted_project() { // spec: ARCH-088
        const changed = Qt.createQmlObject('import QtTest; SignalSpy { signalName: "projectChanged" }', tc)
        changed.target = Core
        Core.noteProject("harbor")
        compare(Core.project, "harbor")
        compare(changed.count, 1)
        Core.noteProject("harbor")
        compare(changed.count, 1, "no change, no signal")
        Core.noteProject("")
        compare(Core.project, "")
        changed.destroy()
    }
}
