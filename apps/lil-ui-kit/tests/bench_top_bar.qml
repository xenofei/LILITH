// The top bar's GUI-thread cost beside the Jobs panel (APP-139; label bench, offscreen), under the
// panel's own limit: no piece of work holds the GUI thread for more than a frame (16 ms). Both read
// the queue, and one event starts both their 150 ms timers, so their answers can land in one turn
// (Core::apply runs a whole batch in one). The bar's share of an event burst is bounded too: it shows
// no row, ETA or place, so it must not pay for them. The numbers are printed as `bench:` lines.
import QtQuick
import QtTest
import Lilith.UiKit
import "CoreShapes.js" as Shapes

TestCase {
    id: tc
    name: "TopBarBench"
    when: windowShown
    width: 200
    height: 100

    Component {
        id: fakeComponent
        FakeCore {}
    }

    Component {
        id: bothWindow
        Window {
            id: win
            required property var core
            property alias panel: jobs
            property alias bar: bar
            width: 800
            height: 780
            visible: true
            TopBar {
                id: bar
                width: parent.width
                core: win.core
            }
            JobsPanel {
                id: jobs
                anchors { left: parent.left; right: parent.right; top: bar.bottom; bottom: parent.bottom }
                core: win.core
                title: "Jobs"
            }
        }
    }

    Component {
        id: barWindow
        Window {
            id: win
            required property var core
            property alias bar: bar
            width: 800
            height: 100
            visible: true
            TopBar {
                id: bar
                width: parent.width
                core: win.core
            }
        }
    }

    function job(id, status, over) {
        const estimate = {local: {source: "measured_history", samples: 5, gpu_s_p50: "24.0", gpu_s_p90: "30.0",
                                  wh_p50: "1.000", wh_p90: "1.500"},
                          wall_s: {source: "measured_history", samples: 5, p50: "30.0", p90: "45.0"}, unknown: []}
        const at = new Date(Date.now() - 600000).toISOString().replace(/\.\d+Z$/, "Z")
        return Shapes.job(id, status, Object.assign({estimate: estimate, status_seq: [{status: status, substatus: "waiting", at: at}]}, over || {}))
    }

    function queue(n) {
        const out = []
        for (let i = 0; i < n; ++i)
            out.push(job("q" + (10000 + i), "queued", {queue_position: i}))
        return out
    }

    // Every call of `test` still open, answered in one turn with `outcome(call)`: how long the
    // turn took.
    function inOneTurn(core, test, outcome) {
        const calls = core.calls.filter(c => !answered[c.id] && test(c))
        calls.forEach(c => { answered[c.id] = true })
        const start = Date.now()
        for (const c of calls)
            core.respond(c.id, outcome(c))
        return {ms: Date.now() - start, calls: calls.length}
    }

    property var answered: ({})

    function isLiveRead(c) {
        return c.name === "jobs.list" && (c.args.status === "running" || c.args.order === "queue")
    }

    function page(c, q, run) {
        if (c.args.status === "running")
            return Shapes.jobsPage(run, [Shapes.lane()])
        const at = c.args.cursor ? Number(c.args.cursor) : 0
        const more = at + 100 < q.length
        return Shapes.jobsPage(q.slice(at, at + 100), [Shapes.lane()], more ? String(at + 100) : null)
    }

    // Loads a window: every call answered, one page per turn. Returns the longest turn.
    function settle(core, q, run) {
        let slowest = 0
        for (let n = 0; n < 40; ++n) {
            const pending = core.calls.filter(c => !answered[c.id])
            if (pending.length === 0)
                break
            const r = inOneTurn(core, isLiveRead, c => page(c, q, run))
            slowest = Math.max(slowest, r.ms)
            inOneTurn(core, c => c.name === "caps.get", () => Shapes.tool(Shapes.capsView([])))
            inOneTurn(core, c => c.name === "engines.status", () => Shapes.tool({engines: []}))
            inOneTurn(core, c => c.name === "jobs.list" && !isLiveRead(c), () => Shapes.jobsPage([], [Shapes.lane()]))
            wait(20)
        }
        return slowest
    }

    function init() {
        answered = {}
        wait(50)
    }

    function test_both_readers_answers_in_one_turn_hold_to_a_frame() { // spec: APP-139, APP-006
        const core = createTemporaryObject(fakeComponent, tc)
        const w = createTemporaryObject(bothWindow, tc, {core: core})
        const q = queue(300)
        settle(core, q, [job("r0", "running")])
        tryVerify(() => w.panel.live.count === 301 && w.bar.jobs.current, 3000)
        wait(300)

        // The front job starts: one event, both readers read again.
        core.emit("jobs", "event", {kind: "job", job: job("q10000", "running")})
        wait(300)
        const rest = q.slice(1).map((j, i) => Object.assign({}, j, {queue_position: i}))
        const samples = []
        for (let n = 0; n < 8; ++n) {
            const open = core.calls.filter(c => !answered[c.id] && isLiveRead(c))
            if (open.length === 0)
                break
            samples.push(inOneTurn(core, isLiveRead, c => page(c, rest, [job("q10000", "running")])))
            wait(20)
        }
        tryVerify(() => w.panel.live.count === 300, 3000)
        const slowest = Math.max(...samples.map(s => s.ms))
        console.log("bench: both readers, each page's answers in one turn: "
                    + samples.map(s => s.calls + " calls " + s.ms + " ms").join(", "))
        verify(slowest <= 16, "one turn held the GUI thread " + slowest + " ms")
    }

    function test_the_bars_share_of_an_event_is_small() { // spec: APP-139, APP-006
        const core = createTemporaryObject(fakeComponent, tc)
        const w = createTemporaryObject(barWindow, tc, {core: core})
        const q = queue(300)
        settle(core, q, [job("r0", "running")])
        tryVerify(() => w.bar.jobs.current, 3000)
        wait(100)

        // Progress: the status stays, so nothing but a lookup. Measured against an event nobody
        // follows, which is what FakeCore's own JSON costs.
        const n = 300
        const progress = i => ({kind: "job", job: job("r0", "running", {progress: {value: i, max: 300}})})
        let start = Date.now()
        for (let i = 0; i < n; ++i)
            core.emit("nobody", "event", progress(i))
        const empty = Date.now() - start
        start = Date.now()
        for (let i = 0; i < n; ++i)
            core.emit("jobs", "event", progress(i))
        const loaded = Date.now() - start
        const each = (loaded - empty) / n
        console.log("bench: bar, one progress event: " + each.toFixed(3) + " ms (" + loaded + " ms for " + n
                    + ", " + empty + " ms of it the fake's)")
        verify(each <= 0.5, "a progress event cost the bar " + each.toFixed(3) + " ms")

        // A burst of 300 jobs ending in one turn, with nothing to sort.
        start = Date.now()
        for (let i = 0; i < n; ++i)
            core.emit("jobs", "event", {kind: "job", job: job("q" + (10000 + i), "cancelled")})
        const burst = Date.now() - start
        console.log("bench: bar, 300 jobs ending in one turn: " + burst + " ms")
        verify(burst <= 160, "a burst cost the bar " + burst + " ms (limit: 16 ms per 30 events)")
        verify(w.bar.jobs.queued === 0, "they left: " + w.bar.jobs.queued)
    }
}
