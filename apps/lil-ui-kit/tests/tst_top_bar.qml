// The top bar (APP-006, ARCH-098, P4a-R34): its indicators, the pause control, the project and
// workspace switchers, and its place in the shell. The counts come from what a read answers, never
// from counting events, so each test answers the `jobs.list` reads itself. Everything the fake core
// answers or publishes has the real core's shape (CoreShapes.js), and a state goes in as Core
// applies one (FakeCore.applyState).
import QtQuick
import QtTest
import Lilith.Core
import Lilith.Shim
import Lilith.UiKit
import "CoreShapes.js" as Shapes

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

    function isQueuedRead(c) {
        return c.name === "jobs.list" && c.args.status === "queued"
    }

    // `app/set_context {}`, which only reads.
    function isContextRead(c) {
        return c.name === "app/set_context" && c.args.project === undefined
    }

    // `app/set_context {project}`, which sets.
    function isProjectSet(c) {
        return c.name === "app/set_context" && c.args.project !== undefined
    }

    // One read of the live jobs, as the core answers it: the running jobs, and every queued job, the
    // dispatched ones included.
    function answerRead(core, running, queued, lanes, dispatched) {
        const all = running.concat(queued, dispatched || [])
        const states = lanes || [Shapes.lane(), Shapes.lane({lane: "cpu"})]
        answer(core, isRunningRead, core.jobsListAnswer({status: "running"}, all, states), "the running read")
        answer(core, isQueuedRead, core.jobsListAnswer({status: "queued"}, all, states), "the queued read")
    }

    function refuseRead(core, code, message) {
        answer(core, isRunningRead, Shapes.refusal(code, message), "the running read")
        answer(core, isQueuedRead, Shapes.refusal(code, message), "the queued read")
    }

    // Answers every read still out, in a shell where the Jobs panel reads too.
    function answerAllReads(core, lanes) {
        for (let n = 0; n < 20 && open(core, named("jobs.list")).length > 0; ++n)
            answer(core, named("jobs.list"), Shapes.jobsPage([], lanes))
    }

    // A bar in a window of its own, its first reads answered.
    function load(setup) {
        const s = setup || {}
        const workspace = s.workspace === undefined ? workspaceOne : s.workspace
        const core = createTemporaryObject(fakeComponent, tc, {workspace: workspace})
        const w = createTemporaryObject(barWindow, tc, {core: core})
        windows.push(w)
        tryVerify(() => w.bar.width > 0)
        if (!s.unanswered) {
            answerRead(core, s.running || [], s.queued || [], s.lanes)
            answer(core, named("caps.get"), Shapes.tool(Shapes.capsView(s.caps)), "caps.get")
            if (workspace)
                answer(core, isContextRead, Shapes.context(workspace, null, s.effective || "sketchbook"), "the context read")
            tryVerify(() => w.bar.jobs.current)
        }
        return {core: core, bar: w.bar, window: w}
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

    function openProjects(t, rows) {
        const sw = t.bar.projectSwitcher
        mouseClick(sw.button)
        tryVerify(() => sw.popup.opened)
        answer(t.core, named("projects.list"), Shapes.tool({projects: rows || Shapes.projectRows()}), "projects.list")
        answer(t.core, isContextRead, Shapes.context(t.core.workspace, t.core.project || null,
                                                     t.core.project || "sketchbook"), "the context read")
        tryVerify(() => sw.popup.view.count === (rows || Shapes.projectRows()).length)
        wait(0)
        return sw
    }

    function openWorkspacesRefused(t) {
        const sw = t.bar.workspaceSwitcher
        mouseClick(sw.button)
        tryVerify(() => sw.popup.opened)
        answer(t.core, named("workspaces.list"), Shapes.notServed("D:/art/quay", "D:/art/harbor"), "workspaces.list")
        return sw
    }

    function openWorkspaces(t, rows) {
        const sw = t.bar.workspaceSwitcher
        const list = rows || Shapes.workspaceRows()
        mouseClick(sw.button)
        tryVerify(() => sw.popup.opened)
        answer(t.core, named("workspaces.list"), Shapes.tool({workspaces: list}), "workspaces.list")
        tryVerify(() => sw.popup.view.count === list.length)
        wait(0)
        return sw
    }

    function makeDefaultButton(popup, id) {
        return findItem(row(popup, w => w.id === id), i => i.text === "Make this the default" && i.visible)
    }

    function test_indicators_follow_events() { // spec: APP-006, APP-027
        const t = load()
        compare(t.bar.jobsIndicator.text, "0 running · 0 queued")
        compare(t.bar.gpuIndicator.text, "GPU —")
        verify(!t.bar.spendIndicator.visible, "no caps, no figure")

        // A `jobs` event is one job: the counts are what the read it starts answers.
        t.core.emit("jobs", "event", {kind: "job", job: Shapes.job("q9", "queued")})
        answerRead(t.core, [Shapes.job("r1", "running"), Shapes.job("r2", "running")],
                   [Shapes.job("q1", "queued"), Shapes.job("q2", "queued"), Shapes.job("q9", "queued")])
        tryCompare(t.bar.jobsIndicator, "text", "2 running · 3 queued")

        t.core.emit("telemetry", "event", Shapes.sample(Shapes.gpu()))
        compare(t.bar.gpuIndicator.text, "GPU 63 % · 9.5/16 GiB · 71 °C")

        t.core.emit("caps", "event", Shapes.capsChanged([Shapes.capRow("monthly", 412)]))
        answer(t.core, named("caps.get"), Shapes.tool(Shapes.capsView([Shapes.capRow("monthly", 412)])))
        tryCompare(t.bar.spendIndicator, "text", "41 % of cap")
        verify(t.bar.spendIndicator.visible)

        compare(t.bar.connectionIndicator.text, "Connected")
    }

    function test_a_dispatched_job_counts_once() { // spec: APP-006, CORE-117
        // Status `queued`, substatus `submitting` or `queued_in_engine`: not in `status: "running"`,
        // and the one listing that holds every queued job is what the bar reads. It reads no queue.
        const t = load()
        compare(t.core.calls.filter(c => c.name === "jobs.list" && c.args.order === "queue").length, 0)
        t.core.emit("jobs", "resync_required", null)
        answerRead(t.core, [Shapes.job("r1", "running")], [Shapes.job("q1", "queued")], undefined,
                   [Shapes.job("d1", "queued", {substatus: "submitting"}),
                    Shapes.job("d2", "queued", {substatus: "queued_in_engine"})])
        tryCompare(t.bar.jobsIndicator, "text", "1 running · 3 queued")

        // Over more than a page, too.
        t.core.emit("jobs", "resync_required", null)
        answer(t.core, isRunningRead, Shapes.jobsPage([]))
        const first = answer(t.core, isQueuedRead, Shapes.jobsPage([Shapes.job("d1", "queued", {substatus: "submitting"})], undefined, "c2"))
        // The same job on a later page counts once.
        const second = answer(t.core, isQueuedRead, Shapes.jobsPage([Shapes.job("q1", "queued"), Shapes.job("d1", "queued", {substatus: "submitting"})]))
        compare(first.args.cursor, undefined)
        compare(second.args.cursor, "c2", "its next page")
        tryCompare(t.bar.jobsIndicator, "text", "0 running · 2 queued")
    }

    function test_a_cold_start_waits_for_the_core_then_reads_everything() { // spec: APP-006, ARCH-100
        const core = createTemporaryObject(fakeComponent, tc, {connection: "connecting", workspace: null})
        const w = createTemporaryObject(barWindow, tc, {core: core})
        windows.push(w)
        const bar = w.bar
        tryVerify(() => bar.width > 0)
        compare(bar.jobsIndicator.text, "—")
        verify(!bar.spendIndicator.visible)
        compare(bar.connectionIndicator.text, "Connecting…")
        wait(50)
        compare(core.calls.length, 0, "the session fails calls until it is connected, so none is made")

        // The core's order: the whole state, then `connection`.
        core.applyState({connection: "ready", coreProtocol: "1.2", workspace: workspaceOne})
        answerRead(core, [Shapes.job("r1", "running")], [])
        tryCompare(bar.jobsIndicator, "text", "1 running · 0 queued")
        answer(core, named("caps.get"), Shapes.tool(Shapes.capsView([Shapes.capRow("monthly", 980)])))
        tryCompare(bar.spendIndicator, "text", "98 % of cap")
        answer(core, isContextRead, Shapes.context(workspaceOne, null, "sketchbook"))
        tryCompare(bar.projectSwitcher.button, "text", "sketchbook ▾")
    }

    function test_a_read_that_fails_at_the_start_is_asked_again_on_ready() { // spec: APP-006, ARCH-100
        // Calls made as the bar is made can still fail, when the connection blinks first.
        const t = load({unanswered: true})
        answer(t.core, named("caps.get"), Shapes.refusal("core.unreachable", "No core."))
        refuseRead(t.core, "core.unreachable", "No core.")
        compare(t.bar.jobsIndicator.text, "—")
        verify(!t.bar.spendIndicator.visible)

        // Disconnected: nothing polls.
        t.core.applyState({connection: "reconnecting"})
        const made = t.core.calls.length
        wait(1300)
        compare(t.core.calls.length, made, "no polling through an outage")

        // Back: at once, without waiting out a backoff.
        const start = Date.now()
        t.core.applyState({connection: "ready", workspace: workspaceOne})
        answerRead(t.core, [], [Shapes.job("q1", "queued")])
        answer(t.core, named("caps.get"), Shapes.tool(Shapes.capsView([Shapes.capRow("monthly", 500)])))
        verify(Date.now() - start < 500, "no wait after `ready`")
        tryCompare(t.bar.jobsIndicator, "text", "0 running · 1 queued")
        tryCompare(t.bar.spendIndicator, "text", "50 % of cap")
    }

    function test_failed_reads_retry_until_they_succeed() { // spec: APP-006, ARCH-100
        const t = load({unanswered: true})
        // caps.get: after 1 s, then 2 s, until it is in.
        answer(t.core, named("caps.get"), Shapes.refusal("store.io", "Busy."))
        tryVerify(() => open(t.core, named("caps.get")).length === 1, 1500, "asked again after 1 s")
        answer(t.core, named("caps.get"), Shapes.refusal("store.io", "Busy."))
        tryVerify(() => open(t.core, named("caps.get")).length === 1, 2500, "and again")
        answer(t.core, named("caps.get"), Shapes.tool(Shapes.capsView([Shapes.capRow("monthly", 412)])))
        tryCompare(t.bar.spendIndicator, "text", "41 % of cap")
        wait(1200)
        compare(open(t.core, named("caps.get")).length, 0, "no more once it is in")

        // The jobs read likewise.
        refuseRead(t.core, "core.unreachable", "No core.")
        compare(t.bar.jobsIndicator.text, "—")
        tryVerify(() => open(t.core, isRunningRead).length > 0, 1500, "read again")
        answerRead(t.core, [Shapes.job("r1", "running")], [])
        tryCompare(t.bar.jobsIndicator, "text", "1 running · 0 queued")
    }

    function test_a_burst_of_job_events_is_one_read_at_a_time() { // spec: APP-006
        const t = load()
        const reads = () => t.core.calls.filter(isRunningRead).length
        const before = reads()
        for (let i = 0; i < 5; ++i)
            t.core.emit("jobs", "event", {kind: "job", job: Shapes.job("q" + i, "queued")})
        compare(reads(), before, "not before the 150 ms")
        tryCompare(t.bar.jobs, "_reading", true, 2000)
        compare(reads(), before + 1, "one read for the burst")

        // While it is out, another event only asks for one more.
        t.core.emit("jobs", "event", {kind: "job", job: Shapes.job("q7", "queued")})
        t.core.emit("jobs", "event", {kind: "job", job: Shapes.job("q8", "queued")})
        wait(300)
        compare(reads(), before + 1, "at most one read in flight")
        answerRead(t.core, [], [Shapes.job("q0", "queued")])
        tryVerify(() => reads() === before + 2, 2000, "the events that came meanwhile read once more")
        answerRead(t.core, [], [Shapes.job("q0", "queued")])
        wait(300)
        compare(reads(), before + 2)
        compare(t.bar.jobsIndicator.text, "0 running · 1 queued", "the read says, not the events")
    }

    function test_a_steady_stream_of_events_still_reads() { // spec: APP-006
        const t = load()
        const before = t.core.calls.filter(isRunningRead).length
        for (let i = 0; i < 16 && t.core.calls.filter(isRunningRead).length === before; ++i) {
            t.core.emit("jobs", "event", {kind: "job", job: Shapes.job("s" + i, "queued")})
            wait(100)
        }
        verify(t.core.calls.filter(isRunningRead).length > before, "a read within about a second")
    }

    function test_progress_events_cost_no_read() { // spec: APP-006, APP-139
        const t = load({running: [Shapes.job("r1", "running")]})
        tryCompare(t.bar.jobsIndicator, "text", "1 running · 0 queued")
        const before = t.core.calls.filter(isRunningRead).length
        for (let i = 1; i <= 5; ++i)
            t.core.emit("jobs", "event", {kind: "job", job: Shapes.job("r1", "running", {progress: {value: i, max: 30}})})
        wait(300)
        compare(t.core.calls.filter(isRunningRead).length, before, "the status stayed")
        compare(t.bar.jobsIndicator.text, "1 running · 0 queued")
    }

    function test_a_mark_reads_afresh_and_a_plain_read_keeps_what_it_cannot_list() { // spec: ARCH-100, APP-006
        const t = load({queued: [Shapes.job("q1", "queued")]})
        tryCompare(t.bar.jobsIndicator, "text", "0 running · 1 queued")

        // A status this build doesn't know is in neither listing: a plain read keeps it (P4a-R40)...
        t.core.emit("jobs", "event", {kind: "job", job: Shapes.job("odd", "paused_by_director")})
        answerRead(t.core, [], [Shapes.job("q1", "queued")])
        tryCompare(t.bar.jobsIndicator, "text", "0 running · 2 queued")

        // ...and a read after a mark carries nothing over.
        t.core.emit("jobs", "resync_required", null)
        verify(!t.bar.jobs.current, "held until the fresh read is in")
        answerRead(t.core, [], [Shapes.job("q1", "queued")])
        tryVerify(() => t.bar.jobs.current)
        compare(t.bar.jobsIndicator.text, "0 running · 1 queued")

        // A snapshot does the same.
        t.core.emit("jobs", "snapshot", {})
        verify(!t.bar.jobs.current)
        answerRead(t.core, [], [])
        tryVerify(() => t.bar.jobs.current)
        compare(t.bar.jobsIndicator.text, "0 running · 0 queued")
    }

    function test_the_lanes_are_not_trusted_from_a_loss_until_the_fresh_read() { // spec: ARCH-100, CORE-160
        const t = load({lanes: [Shapes.lane({paused: true, reasons: ["manual"]})]})
        tryCompare(t.bar.pauseButton, "text", "Resume GPU work")
        verify(t.bar.pauseButton.enabled)
        const toggles = () => t.core.calls.filter(c => c.name.startsWith("app/queue.")).length

        // The core goes away.
        t.core.applyState({connection: "reconnecting"})
        verify(!t.bar.jobs.current)
        verify(!t.bar.pauseButton.enabled, "at once")
        t.bar.togglePause()
        compare(toggles(), 0, "no write without a connection")
        verify(t.bar.notice.visible, "Ctrl+Shift+P says why")
        compare(t.bar.notice.text, "GPU work can't be paused or resumed until the core is connected.")

        // A restarted core: `ready` comes first, and a restart ended the pause.
        t.core.applyState({connection: "ready", workspace: workspaceOne})
        verify(!t.bar.jobs.current, "`ready` alone isn't current")
        verify(!t.bar.pauseButton.enabled, "still held")
        t.bar.togglePause()
        compare(toggles(), 0, "the old state must not send a resume")
        compare(t.bar.notice.text, "Wait a moment: the state of GPU work is still loading.")

        // The mark comes during the read: that read no longer speaks for the new core.
        tryVerify(() => open(t.core, isRunningRead).length > 0, 2000, "`ready` starts the fresh read")
        t.core.emit("jobs", "resync_required", null)
        answerRead(t.core, [], [], [Shapes.lane({paused: true, reasons: ["manual"]})])
        verify(!t.bar.jobs.current, "a read from before the mark")
        t.bar.togglePause()
        compare(toggles(), 0)

        answerRead(t.core, [], [], [Shapes.lane()])
        tryVerify(() => t.bar.jobs.current)
        verify(t.bar.pauseButton.enabled)
        compare(t.bar.pauseButton.text, "Pause GPU work", "the new core's state")
        verify(!t.bar.notice.visible, "the wait is over, and so is its notice")
        t.bar.togglePause()
        compare(t.core.calls.filter(named("app/queue.pause")).length, 1)
        compare(t.core.calls.filter(named("app/queue.resume")).length, 0)
    }

    function test_a_same_core_reconnect_needs_its_read_too() { // spec: ARCH-100, APP-035
        const t = load()
        // A status this build doesn't know is in no listing, and a reconnect isn't a new core.
        t.core.emit("jobs", "event", {kind: "job", job: Shapes.job("odd", "paused_by_director")})
        answerRead(t.core, [], [])
        tryCompare(t.bar.jobsIndicator, "text", "0 running · 1 queued")
        const before = t.core.calls.filter(isRunningRead).length
        t.core.applyState({connection: "reconnecting"})
        t.core.applyState({connection: "ready", workspace: workspaceOne})
        verify(!t.bar.jobs.current)
        compare(t.core.calls.filter(isRunningRead).length, before + 1, "one read, and no mark is coming")
        answerRead(t.core, [], [])
        tryVerify(() => t.bar.jobs.current)
        compare(t.bar.jobsIndicator.text, "0 running · 1 queued", "the bar has no history to get it back from (P4a-R40)")
    }

    function test_a_lost_connection_shows_no_figure_as_live() { // spec: APP-034, APP-006
        const t = load({running: [Shapes.job("r1", "running")], caps: [Shapes.capRow("monthly", 412)],
                        lanes: [Shapes.lane({paused: true, reasons: ["manual"]})]})
        t.core.emit("telemetry", "event", Shapes.sample(Shapes.gpu()))
        tryCompare(t.bar.jobsIndicator, "text", "1 running · 0 queued")
        verify(t.bar.pausedHolder.visible)
        compare(t.bar.spendIndicator.kind, "")

        t.core.applyState({connection: "reconnecting"})
        compare(t.bar.jobsIndicator.text, "—")
        compare(t.bar.gpuIndicator.text, "GPU —")
        verify(!t.bar.pausedHolder.visible, "a restart would end the pause")
        compare(t.bar.spendIndicator.kind, "muted", "last month's figure stays, but muted")
    }

    function test_missing_telemetry_is_a_dash_never_0() { // spec: APP-006
        const t = load()
        t.core.emit("telemetry", "event", Shapes.sample(null))
        compare(t.bar.gpuIndicator.text, "GPU —", "no GPU")
        t.core.emit("telemetry", "event", Shapes.sample(Shapes.gpu({measured: false})))
        compare(t.bar.gpuIndicator.text, "GPU —", "a lost GPU's last reading isn't shown as live")
        t.core.emit("telemetry", "event", Shapes.sample(Shapes.gpu()))
        compare(t.bar.gpuIndicator.text, "GPU 63 % · 9.5/16 GiB · 71 °C")
        t.core.applyState({connection: "reconnecting"})
        compare(t.bar.gpuIndicator.text, "GPU —", "no live reading without a connection")
        t.core.applyState({connection: "ready", workspace: workspaceOne})
        t.core.emit("telemetry", "resync_required", null)
        compare(t.bar.gpuIndicator.text, "GPU —", "a new core's readings start afresh")
    }

    function test_spend_shows_the_highest_cap_ratio() { // spec: APP-006, CORE-008
        const t = load({caps: [Shapes.capRow("per_call_max", 120), Shapes.capRow("monthly", 987), Shapes.capRow("provider:x", 412)]})
        compare(t.bar.spendIndicator.text, "98 % of cap", "the highest, rounded down to a whole percent")

        // Three events are one read.
        const reads = () => t.core.calls.filter(named("caps.get")).length
        const before = reads()
        t.core.emit("caps", "event", Shapes.capsChanged([]))
        t.core.emit("usage", "event", Shapes.usageRecord())
        t.core.emit("caps", "event", Shapes.capsAlert())
        answer(t.core, named("caps.get"), Shapes.tool(Shapes.capsView([Shapes.capRow("provider:x", 412)])))
        compare(reads(), before + 1, "one read for the three events")
        tryCompare(t.bar.spendIndicator, "text", "41 % of cap")

        // A job's end can spend without any caps event.
        t.core.emit("jobs", "event", {kind: "job", job: Shapes.job("r1", "succeeded")})
        answer(t.core, named("caps.get"), Shapes.tool(Shapes.capsView([Shapes.capRow("monthly", 1500)])))
        tryCompare(t.bar.spendIndicator, "text", "150 % of cap")

        // A usage event alone reads too; no caps shows nothing, not 0.
        t.core.emit("usage", "event", Shapes.usageRecord())
        answer(t.core, named("caps.get"), Shapes.tool(Shapes.capsView([])))
        tryVerify(() => !t.bar.spendIndicator.visible)
    }

    function test_an_invalid_cap_says_so_instead_of_a_figure() { // spec: CORE-008, APP-006
        // It counts as exceeded and calls are refused: "41 % of cap" would be false comfort.
        const t = load({caps: [Shapes.capRow("monthly", 0, {invalid: true}), Shapes.capRow("provider:x", 412)]})
        verify(t.bar.spendIndicator.visible)
        compare(t.bar.spendIndicator.text, "Cap invalid")
        compare(t.bar.spendIndicator.kind, "danger")
        verify(LilShim.accessibleOf(t.bar.spendIndicator).name.indexOf("calls are refused") >= 0)

        t.core.emit("caps", "event", Shapes.capsInvalid([]))
        answer(t.core, named("caps.get"), Shapes.tool(Shapes.capsView([Shapes.capRow("provider:x", 412)])))
        tryCompare(t.bar.spendIndicator, "text", "41 % of cap")
        compare(t.bar.spendIndicator.kind, "")
    }

    function test_paused_badge_names_the_lanes_and_the_reasons() { // spec: APP-006, CORE-160
        const t = load()
        verify(!t.bar.pausedHolder.visible, "no badge while nothing is paused")
        const before = t.bar.jobs.laneUpdates
        t.core.emit("jobs", "event", {kind: "queue", state: Shapes.lane({paused: true, reasons: ["fullscreen"]})})
        t.core.emit("jobs", "event", {kind: "queue", state: Shapes.lane({paused: true, reasons: ["manual", "fullscreen"]})})
        tryCompare(t.bar.pausedBadge, "text", "GPU work paused by you and while fullscreen")
        verify(t.bar.pausedHolder.visible)
        compare(t.bar.jobs.laneUpdates, before + 1, "two events in a frame are one update")
        compare(t.bar.pauseButton.text, "Resume GPU work", "your own pause is set")

        t.core.emit("jobs", "event", {kind: "queue", state: Shapes.lane({paused: true, reasons: ["fullscreen"]})})
        tryCompare(t.bar.pausedBadge, "text", "GPU work paused while fullscreen")
        compare(t.bar.pauseButton.text, "Pause GPU work", "the fullscreen pause isn't yours to resume")

        t.core.emit("jobs", "event", {kind: "queue", state: Shapes.lane({paused: true, reasons: ["<b>director</b>"]})})
        tryCompare(t.bar.pausedBadge, "text", "GPU work paused by <b>director</b>")

        // Another lane's pause names that lane, beside a button that is the GPU's.
        t.core.emit("jobs", "event", {kind: "queue", state: Shapes.lane()})
        t.core.emit("jobs", "event", {kind: "queue", state: Shapes.lane({lane: "cpu", paused: true, reasons: ["manual"]})})
        tryCompare(t.bar.pausedBadge, "text", "CPU work paused by you")
        compare(t.bar.pauseButton.text, "Pause GPU work")

        // Both.
        t.core.emit("jobs", "event", {kind: "queue", state: Shapes.lane({paused: true, reasons: ["manual"]})})
        tryCompare(t.bar.pausedBadge, "text", "CPU work paused by you; GPU work paused by you")

        t.core.emit("jobs", "event", {kind: "queue", state: Shapes.lane()})
        t.core.emit("jobs", "event", {kind: "queue", state: Shapes.lane({lane: "cpu"})})
        tryVerify(() => !t.bar.pausedHolder.visible)
    }

    function test_a_read_does_not_undo_a_newer_queue_event() { // spec: CORE-160
        const t = load()
        t.core.emit("jobs", "event", {kind: "job", job: Shapes.job("q1", "queued")})
        tryCompare(t.bar.jobs, "_reading", true, 2000)
        t.core.emit("jobs", "event", {kind: "queue", state: Shapes.lane({paused: true, reasons: ["manual"]})})
        // Its frame passes first, so only the read could undo it.
        tryCompare(t.bar.pausedBadge, "text", "GPU work paused by you")
        wait(50)
        answerRead(t.core, [], [], [Shapes.lane()])
        tryCompare(t.bar.pausedBadge, "text", "GPU work paused by you")
        wait(50)
        verify(t.bar.yourPause, "the event came after the read's lanes")
    }

    function test_pause_control_refusal_and_arguments() { // spec: APP-011, CORE-160
        const t = load({unanswered: true})
        t.bar.togglePause()
        compare(t.core.calls.filter(c => c.name.startsWith("app/queue.")).length, 0, "held until the state has loaded")
        verify(!t.bar.pauseButton.enabled)
        compare(t.bar.notice.text, "Wait a moment: the state of GPU work is still loading.")
        answerRead(t.core, [], [])
        tryVerify(() => t.bar.pauseButton.enabled)
        verify(!t.bar.notice.visible, "the wait is over")

        t.bar.togglePause()
        t.bar.togglePause()
        compare(t.core.calls.filter(named("app/queue.pause")).length, 1, "one call at a time")
        const pause = t.core.calls.filter(named("app/queue.pause"))[0]
        compare(pause.args, {lane: "gpu-heavy"}, "QueueControls' arguments")
        answer(t.core, named("app/queue.pause"), Shapes.app(Shapes.lane({paused: true, reasons: ["manual"]})))
        compare(t.bar.pauseButton.text, "Resume GPU work")

        t.bar.togglePause()
        const resume = t.core.calls.filter(named("app/queue.resume"))[0]
        compare(resume.args, {lane: "gpu-heavy"})
        answer(t.core, named("app/queue.resume"), Shapes.refusal("caller.not_allowed", "<b>Not</b> now."))
        compare(t.bar.pauseButton.text, "Resume GPU work", "a refusal changes nothing")
        verify(t.bar.notice.visible)
        compare(t.bar.notice.text, "Couldn't resume GPU work: <b>Not</b> now.")
        compare(t.bar.notice.kind, "danger")

        // A new attempt clears the old refusal.
        t.bar.togglePause()
        verify(!t.bar.notice.visible, "gone as the new attempt starts")
        answer(t.core, named("app/queue.resume"), Shapes.app(Shapes.lane()))
        verify(!t.bar.notice.visible, "and it stays gone on success")
        compare(t.bar.pauseButton.text, "Pause GPU work")

        // But only its own: what another part of the bar said stays.
        t.bar.showNotice("Quay is now your default workspace.", "ok")
        t.bar.togglePause()
        verify(t.bar.notice.visible, "not the pause control's to close")
        compare(t.bar.notice.text, "Quay is now your default workspace.")
        answer(t.core, named("app/queue.pause"), Shapes.app(Shapes.lane({paused: true, reasons: ["manual"]})))
        verify(t.bar.notice.visible)
    }

    function test_make_this_the_default_then_a_new_hello() { // spec: APP-006, ARCH-030, ARCH-090
        const t = load()
        compare(t.bar.workspaceSwitcher.button.text, "harbor ▾", "the folder's name until the list says more")
        const sw = openWorkspaces(t)
        compare(t.bar.workspaceSwitcher.currentName, "Harbor")
        verify(!sw.popup.message)
        compare(t.core.calls.filter(c => c.kind === "setWorkspace").length, 0, "nothing without a click")

        // "Make this the default" is its own click.
        mouseClick(makeDefaultButton(sw.popup, "ws2"))
        const calls = t.core.calls.filter(named("workspace.set_default"))
        compare(calls.length, 1)
        compare(calls[0].args, {path: "D:/art/quay"})
        compare(t.core.calls.filter(c => c.kind === "setWorkspace").length, 0, "not before it is the default")

        // The core's real behaviour: from its answer on, the old session's every tool call is
        // refused until a new hello. The id to say hello to is in the answer, never the old list.
        answer(t.core, named("workspace.set_default"), Shapes.tool({id: "ws2-from-the-answer", path: "D:/art/quay"}))
        compare(open(t.core, named("workspaces.list")).length, 0, "the old session can't list")
        const hello = open(t.core, c => c.kind === "setWorkspace")
        compare(hello.length, 1, "a new hello at once")
        compare(hello[0].name, "ws2-from-the-answer")
        for (let n = 0; n < 5 && open(t.core, c => c.kind === "call").length > 0; ++n)
            answer(t.core, c => c.kind === "call", Shapes.refusal("workspace.unsupported", "This core serves only the default workspace, \"D:/art/quay\"."))

        const moved = {id: "ws2-from-the-answer", path: "D:/art/quay"}
        answered[hello[0].id] = true
        t.core.respond(hello[0].id, Shapes.hello(moved))
        t.core.applyState({connection: "ready", workspace: moved})
        tryVerify(() => !sw.popup.visible)
        compare(t.bar.workspaceSwitcher.currentName, "quay", "the hello's workspace is what is current")
        verify(t.bar.notice.visible, "the UI says what happened")
        compare(t.bar.notice.text, "Quay is now your default workspace, and the app has switched to it.")
        compare(t.bar.notice.kind, "ok")

        // The session's reload follows its mark, as every panel's does.
        t.core.emit("jobs", "resync_required", null)
        answerRead(t.core, [], [])
        tryVerify(() => t.bar.jobs.current)
        compare(t.core.calls.filter(named("workspace.set_default")).length, 1)
    }

    function test_a_failed_hello_after_the_default_still_has_a_way_on() { // spec: ARCH-090, P4a-R34
        const t = load()
        const sw = openWorkspaces(t)
        mouseClick(makeDefaultButton(sw.popup, "ws2"))
        answer(t.core, named("workspace.set_default"), Shapes.tool({id: "ws2", path: "D:/art/quay"}))
        const hello = open(t.core, c => c.kind === "setWorkspace")[0]
        answered[hello.id] = true
        t.core.respond(hello.id, Shapes.refusal("core.unreachable", "No core answered."))
        compare(t.core.workspace.id, "ws1", "nothing shown changes by itself")
        verify(sw.popup.opened, "it stays open to say why")
        compare(sw.popup.message, "Quay is now your default workspace, but the app couldn't switch to it: No core answered.")
        compare(t.bar.notice.kind, "danger")

        // The rows say what the core now serves, without asking the old session.
        const quay = row(sw.popup, w => w.id === "ws2")
        tryCompare(quay, "reason", "")
        verify(!makeDefaultButton(sw.popup, "ws2"), "it is the default now")
        const old = row(sw.popup, w => w.id === "ws1")
        tryCompare(old, "reason", "This core serves only the default workspace")

        // A switch by itself: Core.setWorkspace, and never set_default (P4a-R34).
        mouseClick(row(sw.popup, w => w.id === "ws2"), 5, 5)
        const again = open(t.core, c => c.kind === "setWorkspace")
        compare(again.length, 1)
        compare(again[0].name, "ws2")
        compare(t.core.calls.filter(named("workspace.set_default")).length, 1, "a switch never sets the default")

        // The core refuses: nothing shown changes but the message.
        answered[again[0].id] = true
        t.core.respond(again[0].id, Shapes.refusal("workspace.unsupported", "This core serves \"Harbor\"."))
        compare(t.core.workspace.id, "ws1")
        compare(sw.popup.message, "Couldn't switch the workspace: This core serves \"Harbor\".")
    }

    function test_a_late_resend_answer_cannot_cross_a_workspace_switch() { // spec: ARCH-088
        const t = load()
        t.core.project = "harbor"
        t.core.applyState({connection: "reconnecting"})
        t.core.applyState({connection: "ready", workspace: workspaceOne})
        tryVerify(() => open(t.core, isProjectSet).length === 1)
        // The switch overtakes the old session's answer.
        const quay = {id: "ws2", path: "D:/art/quay"}
        t.core.applyState({connection: "ready", workspace: quay})
        compare(t.core.project, "")
        answer(t.core, isProjectSet, Shapes.context(workspaceOne, "harbor", "harbor"))
        compare(t.core.project, "", "the old project stays out of the new workspace")
        answer(t.core, isContextRead, Shapes.context(quay, null, "sketchbook"))
        compare(t.bar.projectSwitcher.effectiveProject, "sketchbook")
    }

    function test_the_row_already_shown_only_closes() { // spec: ARCH-088
        // It would pin a fallback nobody chose, from a stray Enter.
        const t = load()
        const sw = openProjects(t)
        compare(sw.popup.view.currentIndex, 0, "it opens on the current row")
        // What Enter on that row does.
        sw.popup.activated(sw.popup.view.currentIndex)
        tryVerify(() => !sw.popup.visible)
        openProjects(t)
        const r = row(sw.popup, p => p.project === "sketchbook")
        wait(50)
        mouseClick(r, 5, 5)
        tryVerify(() => !sw.popup.visible)
        compare(t.core.calls.filter(isProjectSet).length, 0, "nothing was set")
        compare(t.core.project, "")
    }

    function test_another_client_moves_the_default_while_connected() { // spec: ARCH-090, APP-006, ARCH-100
        // `lil workspace use` in a terminal: the connection stays up, every call is refused.
        const t = load()
        const sw = t.bar.workspaceSwitcher
        compare(t.core.subscriberCount("workspace"), 1, "it follows the topic")
        const quay = {id: "ws2", path: "D:/art/quay"}
        t.core.emit("workspace", "event", Shapes.defaultChanged(quay.id, quay.path))
        verify(t.bar.movedReason.indexOf("quay became the default workspace") >= 0, t.bar.movedReason)
        compare(t.bar.connectionIndicator.text, "Workspace refused")
        compare(t.bar.connectionIndicator.kind, "danger")
        verify(sw.button.enabled)
        verify(t.bar.projectSwitcher.button.enabled)
        compare(t.bar.projectSwitcher.button.text, "No project ▾")

        // The list isn't read: the way out is what it offers.
        const reads = t.core.calls.filter(named("workspaces.list")).length
        mouseClick(sw.button)
        tryVerify(() => sw.popup.opened)
        compare(t.core.calls.filter(named("workspaces.list")).length, reads)
        compare(sw.popup.message, t.bar.movedReason)
        verify(sw.popup.defaultButton.visible)
        wait(50)
        mouseClick(sw.popup.defaultButton)
        const hello = open(t.core, c => c.kind === "useDefaultWorkspace")
        compare(hello.length, 1)
        compare(t.core.calls.filter(named("workspace.set_default")).length, 0)
        answered[hello[0].id] = true
        t.core.respond(hello[0].id, Shapes.hello(quay))
        t.core.applyState({connection: "ready", workspace: quay})
        compare(t.bar.movedReason, "", "a new hello ends it")
        compare(t.bar.connectionIndicator.text, "Connected")
        tryVerify(() => !sw.popup.visible, 2000, "a list left open has nothing in it now")

        // Back to this one: nothing to say.
        t.core.emit("workspace", "event", Shapes.defaultChanged("ws1", "D:/art/harbor"))
        verify(t.bar.movedReason !== "")
        t.core.emit("workspace", "event", Shapes.defaultChanged(quay.id, quay.path))
        compare(t.bar.movedReason, "", "this session's own workspace is the default again")
    }

    function test_a_list_left_open_as_a_refusal_ends_closes() { // spec: ARCH-100, APP-006
        // It has nothing in it, and nothing would fill it. No click of its own ends this one.
        const t = load()
        t.core.applyState({connection: "unavailable", unavailable: Shapes.refusedHello("workspace.unknown", "The folder is gone.")})
        for (const sw of [t.bar.workspaceSwitcher, t.bar.projectSwitcher]) {
            mouseClick(sw.button)
            tryVerify(() => sw.popup.opened)
            verify(sw.popup.defaultButton.visible)
            t.core.applyState({connection: "ready", workspace: workspaceOne})
            tryVerify(() => !sw.popup.visible, 2000, "the refusal ended")
            t.core.applyState({connection: "unavailable", unavailable: Shapes.refusedHello("workspace.unknown", "The folder is gone.")})
        }
    }

    function test_a_refused_count_or_caps_read_is_the_same_state() { // spec: ARCH-090, ARCH-100
        // A hand-edited suite.toml publishes nothing, so a read is the first to hear of it.
        const t = load({running: [Shapes.job("r1", "running")], caps: [Shapes.capRow("monthly", 412)],
                        lanes: [Shapes.lane({paused: true, reasons: ["manual"]})]})
        tryCompare(t.bar.jobsIndicator, "text", "1 running · 0 queued")
        t.core.emit("jobs", "event", {kind: "job", job: Shapes.job("q1", "queued")})
        tryVerify(() => open(t.core, isRunningRead).length > 0, 2000, "the read")
        answer(t.core, isRunningRead, Shapes.notServed("D:/art/quay", "D:/art/harbor"))
        verify(t.bar.movedReason.indexOf("This core serves only the default workspace") >= 0, t.bar.movedReason)
        compare(t.bar.connectionIndicator.text, "Workspace refused")
        compare(t.bar.jobsIndicator.text, "—", "the last counts aren't shown as live")
        verify(!t.bar.pausedHolder.visible)
        compare(t.bar.spendIndicator.kind, "muted")

        // A new hello ends it, and caps.get alone can start it.
        t.core.applyState({connection: "ready", workspace: workspaceOne})
        compare(t.bar.movedReason, "")
        t.core.emit("caps", "event", Shapes.capsChanged([]))
        answer(t.core, named("caps.get"), Shapes.notServed("D:/art/quay", "D:/art/harbor"), "caps.get")
        verify(t.bar.movedReason !== "")

        // A refusal from before a switch isn't about the new workspace.
        t.core.applyState({connection: "ready", workspace: workspaceOne})
        t.core.emit("usage", "event", Shapes.usageRecord())
        tryVerify(() => open(t.core, named("caps.get")).length > 0, 3000, "caps.get again")
        t.core.applyState({connection: "ready", workspace: {id: "ws2", path: "D:/art/quay"}})
        answer(t.core, named("caps.get"), Shapes.notServed("D:/art/quay", "D:/art/harbor"))
        compare(t.bar.movedReason, "")
    }

    function test_a_refused_list_read_is_the_same_state() { // spec: ARCH-090, ARCH-100
        // The event can be missed; the core's real refusal says the same.
        const t = load()
        const sw = openWorkspacesRefused(t)
        verify(t.bar.movedReason.indexOf("This core serves only the default workspace") >= 0, t.bar.movedReason)
        compare(sw.popup.message, t.bar.movedReason)
        verify(sw.popup.defaultButton.visible)
        compare(t.bar.connectionIndicator.text, "Workspace refused")
        sw.popup.close()
        tryVerify(() => !sw.popup.visible)

        // Another refusal is only a refusal.
        t.core.applyState({connection: "ready", workspace: workspaceOne})
        compare(t.bar.movedReason, "")
        mouseClick(sw.button)
        tryVerify(() => sw.popup.opened)
        answer(t.core, named("workspaces.list"), Shapes.refusal("store.io", "Busy."))
        compare(t.bar.movedReason, "")
        compare(sw.popup.message, "Couldn't list the workspaces: Busy.")
    }

    function test_own_make_default_is_not_another_clients_move() { // spec: ARCH-090
        const t = load()
        const sw = openWorkspaces(t)
        mouseClick(makeDefaultButton(sw.popup, "ws2"))
        const quay = {id: "ws2", path: "D:/art/quay"}
        // The event overtakes the answer, and the answer the hello.
        t.core.emit("workspace", "event", Shapes.defaultChanged(quay.id, quay.path))
        compare(t.bar.movedReason, "")
        answer(t.core, named("workspace.set_default"), Shapes.tool(quay))
        t.core.emit("workspace", "event", Shapes.defaultChanged(quay.id, quay.path))
        compare(t.bar.movedReason, "")
    }

    function test_unavailable_workspaces_say_why_and_cannot_be_chosen() { // spec: APP-006, ARCH-090
        const t = load()
        const sw = openWorkspaces(t)
        const reasons = {ws2: "This core serves only the default workspace", ws3: "This core serves only the default workspace",
                         ws4: "The folder is gone", ws5: "It isn't a folder", ws6: "It can't be read"}
        for (const id in reasons) {
            const r = row(sw.popup, w => w.id === id)
            verify(LilShim.accessibleOf(r).name.indexOf(reasons[id]) >= 0, LilShim.accessibleOf(r).name)
            verify(LilShim.accessibleOf(r).name.indexOf("unavailable") >= 0, LilShim.accessibleOf(r).name)
            verify(findItem(r, i => i.text === reasons[id]), "the row shows: " + reasons[id])
            mouseClick(r, 5, 5)
        }
        compare(t.core.calls.filter(c => c.kind === "setWorkspace").length, 0, "none of them can be chosen")
        // A folder that isn't there can't be the default; an unserved one that is there can.
        verify(makeDefaultButton(sw.popup, "ws3"))
        verify(!makeDefaultButton(sw.popup, "ws4"))
        verify(!makeDefaultButton(sw.popup, "ws5"))
        // Nor is the default offered again.
        verify(!makeDefaultButton(sw.popup, "ws1"))

        // Choosing the current one changes nothing.
        mouseClick(row(sw.popup, w => w.id === "ws1"), 5, 5)
        compare(t.core.calls.filter(c => c.kind === "setWorkspace").length, 0)
        tryVerify(() => !sw.popup.visible)
    }

    function test_project_switch_calls_set_context() { // spec: APP-006, ARCH-088
        const t = load()
        const sw = openProjects(t)
        mouseClick(row(sw.popup, p => p.project === "harbor"), 5, 5)
        const calls = t.core.calls.filter(isProjectSet)
        compare(calls.length, 1)
        compare(calls[0].kind, "appCall")
        compare(calls[0].args, {project: "harbor"})
        compare(sw.current, "sketchbook", "not before the core answers")
        answer(t.core, isProjectSet, Shapes.context(workspaceOne, "harbor", "harbor"))
        compare(t.core.project, "harbor", "the answer reaches Core.project")
        compare(sw.button.text, "Harbor ▾")
        tryVerify(() => !sw.popup.visible)

        // A refusal shows, the project stays, and the label is read again.
        openProjects(t)
        mouseClick(row(sw.popup, p => p.project === "sketchbook"), 5, 5)
        answer(t.core, isProjectSet, Shapes.refusal("project.unknown", "No such project."))
        compare(t.core.project, "harbor")
        verify(sw.popup.opened)
        compare(sw.popup.message, "Couldn't switch the project: No such project.")
        answer(t.core, isContextRead, Shapes.context(workspaceOne, "harbor", "harbor"), "read again after a refusal")

        // From the keyboard: the list has the focus, the arrows move and Enter chooses.
        tryVerify(() => sw.popup.view.activeFocus)
        sw.popup.view.currentIndex = 1
        keyClick(Qt.Key_Up)
        compare(sw.popup.view.currentIndex, 0)
        keyClick(Qt.Key_Return)
        const last = t.core.calls.filter(isProjectSet).pop()
        compare(last.args, {project: "sketchbook"})
        answer(t.core, isProjectSet, Shapes.context(workspaceOne, "sketchbook", "sketchbook"))
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

    Component {
        id: otherWindow
        Window {
            width: 200
            height: 100
            visible: true
        }
    }

    function test_a_press_in_another_window_of_the_app_closes_a_list() { // spec: APP-006, APP-001
        const t = load()
        const other = createTemporaryObject(otherWindow, tc)
        tryVerify(() => other.visible)
        wait(50)
        const sw = openProjects(t)
        mouseClick(other.contentItem, 20, 20)
        tryVerify(() => !sw.popup.visible, 2000, "a floating dock is another window")

        // A touch does the same.
        openProjects(t)
        touchEvent(other.contentItem).press(0, other.contentItem, 20, 20).commit()
        tryVerify(() => !sw.popup.visible, 2000, "a touch in another window")
        touchEvent(other.contentItem).release(0, other.contentItem, 20, 20).commit()
    }

    function test_sketchbook_is_the_default_project() { // spec: ARCH-098, APP-015
        const t = load({effective: "sketchbook"})
        compare(t.core.project, "")
        compare(t.bar.projectSwitcher.current, "sketchbook")
        compare(t.bar.projectSwitcher.button.text, "sketchbook ▾")
        compare(t.core.calls.filter(isProjectSet).length, 0, "only `{}` at start: nothing is set")
        const sw = openProjects(t)
        verify(row(sw.popup, p => p.project === "sketchbook").isCurrent)
    }

    function test_the_label_is_the_project_calls_would_really_use() { // spec: ARCH-088, ARCH-098
        // Another client left a hint: this session would use it, and the bar says so.
        const t = load({effective: "harbor"})
        compare(t.bar.projectSwitcher.button.text, "harbor ▾")
        compare(t.bar.projectSwitcher.effectiveProject, "harbor")
        compare(t.core.project, "", "the fallback isn't what you chose")

        // A reconnect only reads: it never pins the fallback, which would rewrite the hint.
        t.core.applyState({connection: "reconnecting"})
        t.core.applyState({connection: "ready", workspace: workspaceOne})
        answer(t.core, isContextRead, Shapes.context(workspaceOne, null, "sketchbook"), "every `ready` reads it")
        compare(t.core.calls.filter(isProjectSet).length, 0, "`{}` only")
        compare(t.bar.projectSwitcher.button.text, "sketchbook ▾", "the core moved on: true when read")
        compare(t.core.project, "")

        // Another workspace reads it too.
        const quay = {id: "ws2", path: "D:/art/quay"}
        t.core.applyState({connection: "ready", workspace: quay})
        answer(t.core, isContextRead, Shapes.context(quay, null, "sketchbook"), "a workspace change reads it")
        compare(t.core.calls.filter(isProjectSet).length, 0)
    }

    function test_no_workspace_means_no_project() { // spec: APP-016
        const t = load({workspace: null})
        verify(!t.bar.projectSwitcher.button.enabled, "a project is set within a workspace")
        compare(t.bar.projectSwitcher.button.text, "No project ▾", "never `sketchbook` here")
        verify(t.bar.workspaceSwitcher.button.enabled, "the workspaces can still be listed")
        compare(t.bar.workspaceSwitcher.button.text, "No workspace ▾")
        compare(t.core.calls.filter(named("app/set_context")).length, 0, "nothing to read")
    }

    function test_a_legal_slug_cannot_break_the_label() { // spec: ARCH-088
        const t = load({effective: "constructor"})
        compare(t.bar.projectSwitcher.button.text, "constructor ▾")
        const sw = openProjects(t, [{project: "__proto__", name: "Odd", status: "active", assets: 0},
                                    {project: "toString", name: "toString", status: "active", assets: 0}])
        compare(sw.button.text, "sketchbook ▾")
    }

    function test_a_reconnect_sends_the_chosen_project_again() { // spec: ARCH-088, APP-035
        const t = load()
        t.core.project = "harbor"
        t.core.applyState({connection: "reconnecting"})
        compare(t.core.calls.filter(isProjectSet).length, 0)
        t.core.applyState({connection: "ready", workspace: workspaceOne})
        compare(t.core.calls.filter(isProjectSet).length, 1, "the new session has no project yet")
        compare(t.core.calls.filter(isProjectSet)[0].args, {project: "harbor"})
        answer(t.core, isProjectSet, Shapes.context(workspaceOne, "harbor", "harbor"))
        compare(t.core.project, "harbor")
        compare(t.bar.projectSwitcher.effectiveProject, "harbor", "its answer is applied")

        // A resend that fails says nothing to you, but the label is read again: the session has no
        // project now, so calls go to the hint.
        t.core.applyState({connection: "reconnecting"})
        t.core.applyState({connection: "ready", workspace: workspaceOne})
        answer(t.core, isProjectSet, Shapes.refusal("store.io", "Busy."))
        answer(t.core, isContextRead, Shapes.context(workspaceOne, null, "sketchbook"), "any failed resend reads")
        compare(t.core.project, "harbor", "the choice stands for the next connection")
        compare(t.bar.projectSwitcher.button.text, "sketchbook ▾", "the label says what calls would use")

        // One that is gone now is dropped.
        t.core.applyState({connection: "reconnecting"})
        t.core.applyState({connection: "ready", workspace: workspaceOne})
        answer(t.core, isProjectSet, Shapes.refusal("project.unknown", "No such project."))
        compare(t.core.project, "")
        answer(t.core, isContextRead, Shapes.context(workspaceOne, null, "sketchbook"), "read after project.unknown")
        compare(t.bar.projectSwitcher.current, "sketchbook")
    }

    function test_another_workspace_forgets_the_project() { // spec: ARCH-088
        const t = load()
        t.core.project = "harbor"
        t.core.workspace = {id: "ws1", path: "D:/art/harbor"}
        compare(t.core.project, "harbor", "the same workspace, as each hello sets it")
        const quay = {id: "ws2", path: "D:/art/quay"}
        t.core.applyState({connection: "ready", workspace: quay})
        compare(t.core.project, "")
        compare(t.bar.projectSwitcher.effectiveProject, "", "nor is the old label carried over")
        compare(t.bar.workspaceSwitcher.button.text, "quay ▾")
        answer(t.core, isContextRead, Shapes.context(quay, null, "sketchbook"))
        compare(t.bar.projectSwitcher.button.text, "sketchbook ▾")
    }

    function test_a_list_stays_inside_the_window() { // spec: APP-156
        const t = load()
        // So narrow that a list opened at the project switcher has to move left to fit.
        t.window.width = 480
        t.window.height = 240
        tryVerify(() => t.bar.width === 480)
        wait(50)
        const many = []
        for (let i = 0; i < 30; ++i)
            many.push({project: "p" + i, name: "Project " + i, status: "active", assets: i})
        const sw = t.bar.projectSwitcher
        mouseClick(sw.button)
        tryVerify(() => sw.popup.opened)
        answer(t.core, named("projects.list"), Shapes.tool({projects: many}))
        answer(t.core, isContextRead, Shapes.context(workspaceOne, null, "sketchbook"))
        tryVerify(() => sw.popup.view.count === 30)
        wait(50)
        const at = sw.popup.contentItem.mapToItem(null, 0, 0)
        verify(at.y + sw.popup.contentItem.height <= t.window.height, "ends at " + (at.y + sw.popup.contentItem.height))
        verify(at.x + sw.popup.contentItem.width <= t.window.width, "ends at " + (at.x + sw.popup.contentItem.width))
        verify(sw.popup.view.contentHeight > sw.popup.view.height, "the list scrolls within it")
    }

    function test_app_switcher_leads_with_the_plain_name() { // spec: APP-164, APP-006
        const t = load()
        compare(t.bar.appName.children[0].text, "Image studio")
        compare(t.bar.appName.children[1].text, "ATHANOR")
        compare(LilShim.accessibleOf(t.bar.appName).name, "App: Image studio, ATHANOR")
    }

    function test_assistant_toggle_is_shown_disabled_and_says_why() { // spec: APP-006, APP-156
        const t = load()
        verify(t.bar.assistantToggle.visible)
        verify(!t.bar.assistantToggle.enabled)
        verify(!t.bar.assistantToggle.checked)
        compare(t.bar.assistantReason, "The assistant arrives in a later version")

        // To the pointer.
        wait(50)
        mouseMove(t.bar.assistantToggle)
        tryVerify(() => t.bar.assistantTip.visible, 2000, "the pointer finds out why")
        compare(t.bar.assistantTip.text, t.bar.assistantReason)
        mouseMove(t.bar.gpuIndicator)
        tryVerify(() => !t.bar.assistantTip.visible)

        // To the keyboard and a screen reader, on a tab stop of its own.
        const a = LilShim.accessibleOf(t.bar.assistantHolder)
        compare(a.role, Accessible.StaticText)
        verify(a.name.indexOf(t.bar.assistantReason) >= 0, a.name)
        verify(walk.tabChain(t.bar).indexOf(t.bar.assistantHolder) >= 0, "Tab reaches it")
        t.bar.assistantHolder.forceActiveFocus(Qt.TabFocusReason)
        tryVerify(() => t.bar.assistantTip.visible, 2000, "and its reason shows")
    }

    function test_outside_text_is_plain_cut_and_cleaned() { // spec: APP-156, P4a-R8
        const t = load()
        const hostile = "<img src=\"http://localhost:9/x.png\"><b>big</b>\u202e\u0007" + "x".repeat(200)
        const plain = root => eachItem(root, i => {
            if (i.textFormat !== undefined)
                compare(i.textFormat, Text.PlainText, "plain text: " + i.text)
        })
        const clean = (text, max) => {
            verify(text.length <= max, "cut: " + text.length)
            verify(text.indexOf("\u0007") < 0, "no controls")
            verify(text.indexOf("\u202e") < 0, "a bidi mark is shown, not obeyed")
        }

        // A project's name and status.
        const psw = openProjects(t, [{project: "harbor", name: hostile, status: hostile, assets: 1}])
        const r = row(psw.popup, p => p.project === "harbor")
        clean(r.name, 60)
        clean(r.detail, 60)
        plain(psw.popup.contentItem)
        psw.popup.close()
        tryVerify(() => !psw.popup.visible)

        // A workspace's name, path and problem, and the core's message.
        const wsw = t.bar.workspaceSwitcher
        openWorkspaces(t, [{id: "ws1", path: "D:/art/harbor", name: hostile, default: true, served: true, missing: false},
                           {id: "ws7", path: "D:/art/" + hostile, default: false, served: true, missing: false},
                           {id: "ws8", path: "D:/art/x", default: false, served: true, missing: false, problem: hostile}])
        clean(row(wsw.popup, w => w.id === "ws1").name, 60)
        clean(row(wsw.popup, w => w.id === "ws7").name, 60)
        clean(row(wsw.popup, w => w.id === "ws8").reason, 40)
        clean(findItem(row(wsw.popup, w => w.id === "ws7"), i => i.elide === Text.ElideMiddle).text, 80)
        plain(wsw.popup.contentItem)
        mouseClick(row(wsw.popup, w => w.id === "ws7"), 5, 5)
        const hello = open(t.core, c => c.kind === "setWorkspace")[0]
        answered[hello.id] = true
        t.core.respond(hello.id, Shapes.refusal("workspace.unsupported", hostile))
        clean(wsw.popup.message, 340)
        wsw.popup.close()
        tryVerify(() => !wsw.popup.visible)

        // The label from the session's path, and an unknown reason on the badge.
        t.core.workspace = {id: "ws9", path: "D:/art/" + hostile}
        clean(wsw.button.text, 62)
        t.core.emit("jobs", "event", {kind: "queue", state: Shapes.lane({paused: true, reasons: [hostile]})})
        tryVerify(() => t.bar.pausedHolder.visible)
        clean(t.bar.pausedBadge.text, 60)
        plain(t.bar)
    }

    // The Task 22 audit's probe, on the bar alone: the panel beside it in a shell has its own.
    function test_an_img_on_the_bar_shows_as_text_and_loads_nothing() { // spec: APP-156, P4a-R8
        const img = "<img src=\"http://localhost:9/x.png\"><b>bad</b>"
        const before = TestFiles.networkRequests()
        const t = load()
        t.bar.togglePause()
        answer(t.core, named("app/queue.pause"), Shapes.refusal("caller.not_allowed", img))
        verify(t.bar.notice.text.indexOf(img) >= 0, t.bar.notice.text)
        t.core.emit("jobs", "event", {kind: "queue", state: Shapes.lane({paused: true, reasons: [img]})})
        tryVerify(() => t.bar.pausedHolder.visible)
        verify(t.bar.pausedBadge.text.indexOf("<img src=") >= 0, t.bar.pausedBadge.text)
        t.core.emit("workspace", "event", Shapes.defaultChanged("ws2", "D:/art/<img src=x.png><b>bad"))
        verify(t.bar.movedReason.indexOf("<img src=x.png><b>bad") >= 0, t.bar.movedReason)
        eachItem(t.bar, i => {
            if (i.textFormat !== undefined)
                compare(i.textFormat, Text.PlainText, "plain text: " + i.text)
        })
        wait(500)
        compare(TestFiles.networkRequests(), before, "nothing loaded the <img>")
    }

    function test_the_bar_wraps_rather_than_hiding_a_control() { // spec: APP-156
        const t = load({lanes: [Shapes.lane({paused: true, reasons: ["manual", "fullscreen"]})],
                        caps: [Shapes.capRow("monthly", 412)]})
        t.core.emit("telemetry", "event", Shapes.sample(Shapes.gpu()))
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
        const t = load({lanes: [Shapes.lane({paused: true, reasons: ["manual"]})]})
        tryVerify(() => t.bar.pauseButton.enabled)
        verify(walk.interactiveCount(t.bar) >= 4, "the switchers, the pause and the assistant: " + walk.interactiveCount(t.bar))
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
        tryVerify(() => shell.topBar.jobs.current)
        shell.gpuPauseToggleRequested()
        compare(core.calls.filter(named("app/queue.pause")).length, 1)
        answer(core, named("app/queue.pause"), Shapes.app(Shapes.lane({paused: true, reasons: ["manual"]})))
        shell.gpuPauseToggleRequested()
        compare(core.calls.filter(named("app/queue.resume")).length, 1)

        // The window's title follows the project calls would use (APP-164), then the chosen one.
        compare(shell.title, "LILITH")
        answer(core, isContextRead, Shapes.context(workspaceOne, null, "sketchbook"))
        compare(shell.title, "LILITH — sketchbook")
        core.noteProject("harbor")
        compare(shell.title, "LILITH — harbor")

        // The event log runs from the shell's start, whichever panel opens later.
        verify(shell.eventLog, "a shell property")
        const logged = shell.eventLog.rowCount()
        core.emit("jobs", "event", {kind: "queue", state: Shapes.lane()})
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
