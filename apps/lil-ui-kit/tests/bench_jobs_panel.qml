// The Jobs panel's GUI-thread cost (APP-139; label bench, offscreen): applying a 300-job queue
// read never holds the GUI thread for more than a frame (16 ms) at a time, and the 1 s clock
// tick with 100 live rows costs at most 2 ms. Each piece of GUI-thread work is timed on its own:
// each page's answer, handled as Core's arrive (FakeCore's own JSON of the page included), and
// the list's layout that the next frame does (forced at once). The row controls come later, made
// off the frame's critical path. And a burst of 300 `jobs` events over 3,000 live jobs (a batch
// being cancelled) is one pass over them, and holds the GUI thread no longer than the bound below.
// The numbers are printed as `bench:` lines.
import QtQuick
import QtTest
import Lilith.UiKit

TestCase {
    id: tc
    name: "JobsPanelBench"
    when: windowShown
    width: 200
    height: 100

    Component {
        id: fakeComponent
        FakeCore {}
    }

    Component {
        id: panelWindow
        Window {
            id: win
            required property var core
            property alias panel: jobs
            width: 420
            height: 780
            visible: true
            JobsPanel {
                id: jobs
                anchors.fill: parent
                core: win.core
                title: "Jobs"
            }
        }
    }

    function iso(msAgo) {
        return new Date(Date.now() - (msAgo || 0)).toISOString().replace(/\.\d+Z$/, "Z")
    }

    function estimate(p50) {
        return {local: {source: "measured_history", samples: 5, gpu_s_p50: (p50 * 0.8).toFixed(1),
                        gpu_s_p90: p50.toFixed(1), wh_p50: "1.000", wh_p90: "1.500"},
                wall_s: {source: "measured_history", samples: 5, p50: p50.toFixed(1), p90: (p50 * 1.5).toFixed(1)},
                unknown: []}
    }

    function job(id, over) {
        return Object.assign({job_id: id, kind: "comfy.prompt", lane: "gpu-heavy", priority: "user",
                              status: "queued", substatus: "waiting", created: iso(600000), updated: iso(),
                              attempt: 1, trace_id: "t_" + id, batch_id: "b_big", index: 0,
                              origin: {surface: "panel", id: "image"}, estimate: estimate(30),
                              status_seq: [{status: "queued", substatus: "waiting", at: iso(600000)}]},
                             over || {})
    }

    function running(id) {
        return job(id, {status: "running", substatus: "executing", started: iso(5000), phase: "sampling",
                        progress: {value: 3, max: 30},
                        status_seq: [{status: "running", substatus: "executing", at: iso(5000)}]})
    }

    function ok(structured) {
        return {status: "ok", structured: structured, artifacts: [], summary_text: ""}
    }

    function lanes() {
        return [{lane: "gpu-heavy", paused: false, reasons: [], held: 0, restart_held: 0, withdrawn: 0}]
    }

    property var answered: ({})

    function open(core, pred) {
        let call = null
        tryVerify(() => (call = core.calls.find(c => !answered[c.id] && pred(c)) || null) !== null, 3000)
        answered[call.id] = true
        return call
    }

    // Answers the live read, each page on its own turn of the event loop, then lays the list out:
    // {answer, layout}, the longest of each, in ms.
    function answerLive(t, run, queue) {
        let slowest = 0
        const timed = (call, structured) => {
            const start = Date.now()
            t.core.respond(call.id, ok(structured))
            slowest = Math.max(slowest, Date.now() - start)
            wait(20)
        }
        timed(open(t.core, c => c.name === "jobs.list" && c.args.status === "running"),
              {jobs: run, next_cursor: null, lanes: lanes()})
        let layout = 0
        // The `{status: "queued"}` read's pages (the queue again, as the real core's), then the
        // queue's, whose last one commits the read.
        let cursor = null
        do {
            const asked = open(t.core, c => c.name === "jobs.list" && c.args.status === "queued")
            const outcome = t.core.jobsListAnswer(asked.args, queue, lanes())
            cursor = outcome.structured.next_cursor
            const start = Date.now()
            t.core.respond(asked.id, outcome)
            slowest = Math.max(slowest, Date.now() - start)
            wait(20)
        } while (cursor)
        for (let at = 0; at < queue.length; at += 100) {
            const more = at + 100 < queue.length
            const call = open(t.core, c => c.name === "jobs.list" && c.args.order === "queue")
            const start = Date.now()
            t.core.respond(call.id, ok({jobs: queue.slice(at, at + 100), next_cursor: more ? "c" + at : null,
                                        lanes: lanes()}))
            slowest = Math.max(slowest, Date.now() - start)
            if (!more) {
                const before = Date.now()
                t.panel.liveView.forceLayout()
                layout = Date.now() - before
            }
            wait(20)
        }
        return {answer: slowest, layout: layout}
    }

    function loaded(queueCount) {
        answered = {}
        const core = createTemporaryObject(fakeComponent, tc)
        const w = createTemporaryObject(panelWindow, tc, {core: core})
        const page = open(core, c => c.name === "jobs.list" && c.args.order === undefined && c.args.status === undefined)
        core.respond(page.id, ok({jobs: [], next_cursor: null, lanes: lanes()}))
        core.respond(open(core, c => c.name === "engines.status").id, ok({engines: []}))
        core.respond(open(core, c => c.name === "caps.get").id, ok({display_currency: "USD", period: "2026-10", caps: []}))
        const queue = []
        for (let i = 0; i < queueCount; ++i)
            queue.push(job("q" + (10000 + i), {queue_position: i}))
        return {core: core, window: w, panel: w.panel, queue: queue}
    }

    function test_a_300_job_queue_read_never_blocks_more_than_a_frame() { // spec: APP-139
        const t = loaded(300)
        const first = answerLive(t, [running("r0")], t.queue)
        tryVerify(() => t.panel.live.count === 301, 3000)
        wait(300)
        // The front job starts: the next read moves it and shifts every place.
        t.core.emit("jobs", "event", {kind: "job", job: running("q10000")})
        const second = answerLive(t, [running("q10000")], t.queue.slice(1).map((j, i) =>
            Object.assign({}, j, {queue_position: i})))
        tryVerify(() => t.panel.live.count === 300, 3000)
        console.log("bench: 300-job read, first: longest answer " + first.answer + " ms, layout " + first.layout
                    + " ms; after a start: longest answer " + second.answer + " ms, layout " + second.layout + " ms")
        for (const [what, ms] of [["first answer", first.answer], ["first layout", first.layout],
                                  ["second answer", second.answer], ["second layout", second.layout]])
            verify(ms <= 16, what + " held the GUI thread " + ms + " ms")
    }

    function test_a_clock_tick_with_100_rows_costs_at_most_2_ms() { // spec: APP-139
        const t = loaded(99)
        answerLive(t, [running("r0")], t.queue)
        tryVerify(() => t.panel.live.count === 100, 3000)
        wait(300)
        let slowest = 0
        let total = 0
        const ticks = 20
        for (let i = 1; i <= ticks; ++i) {
            const start = Date.now()
            t.panel.live.tick(Date.now() + i * 1000)
            const took = Date.now() - start
            slowest = Math.max(slowest, took)
            total += took
            wait(20)
        }
        console.log("bench: clock tick with 100 rows: mean " + (total / ticks).toFixed(2) + " ms, slowest "
                    + slowest + " ms")
        verify(slowest <= 2, "a tick took " + slowest + " ms")
    }

    // The longest the GUI thread went without running a timer.
    Timer {
        id: beat
        interval: 1
        repeat: true
        property real last: 0
        property real longest: 0
        onTriggered: {
            const now = Date.now()
            longest = Math.max(longest, now - last)
            last = now
        }
    }

    function test_a_burst_of_300_events_over_3000_jobs_keeps_the_window_alive() { // spec: APP-139
        failOnWarning(/DelegateModel::cancel/)
        const t = loaded(3000)
        const read = answerLive(t, [running("r0")], t.queue)
        tryVerify(() => t.panel.live.count === 3001, 20000)
        wait(300)
        beat.longest = 0
        beat.last = Date.now()
        beat.start()
        const start = Date.now()
        // The first 300 of a batch being cancelled, the rows on screen among them.
        for (let i = 0; i < 300; ++i)
            t.core.emit("jobs", "event", {kind: "job", job: job("q" + (10000 + i),
                                                                {status: "cancelled", substatus: null, finished: iso(),
                                                                 updated: iso()})})
        const emitted = Date.now() - start
        tryVerify(() => t.panel.live.count === 2701, 60000)
        const applied = Date.now() - start
        wait(100)
        beat.stop()
        console.log("bench: 300 events over 3,000 jobs: events handled in " + emitted + " ms, applied by " + applied
                    + " ms; the GUI thread's longest hold " + beat.longest + " ms; a read of them: longest answer "
                    + read.answer + " ms, layout " + read.layout + " ms")
        verify(beat.longest <= burstHoldMs, "the GUI thread held " + beat.longest + " ms")
    }

    // A hold a window's user doesn't feel as a freeze, and about twice what this PC measures (36 to
    // 56 ms; 10.4 s with one sort and one set of model changes per event).
    readonly property int burstHoldMs: 100
}
