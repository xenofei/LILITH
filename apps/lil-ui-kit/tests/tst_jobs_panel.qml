// The Jobs panel (APP-114, APP-139, CORE-160, CORE-161; P4a-R40, P4a-R41): rows that update in
// place at most 10 times a second with their live preview, lane headers with the pause reasons,
// waiting for GPU memory and ComfyUI's outside jobs, run next or last, the batch and all-mine
// cancels, Pause GPU work, the restart hold's own resume, the queue order and the queued ETA,
// the retry's cost chip, the raw names of what the app doesn't know, and floating it in the
// shell; and the live list's source: the running read, every queue page and the jobs the core has
// dispatched, as FakeCore's `jobsListAnswer` answers like the real core, never a summary's stale
// place. Every test runs on FakeCore.
import QtQuick
import QtTest
import Lilith.Shim
import Lilith.UiKit
import "CoreShapes.js" as Shapes

TestCase {
    id: tc
    name: "JobsPanel"
    when: windowShown
    width: 200
    height: 100

    A11yWalk { id: walk }

    Component {
        id: fakeComponent
        FakeCore {}
    }

    // The panel in a window of its own, as wide as the Default layout's right column.
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

    Component {
        id: shellComponent
        ShellWindow {}
    }

    // A 2x1 and a 3x1 PNG (tst_preview's).
    readonly property string twoWide: "iVBORw0KGgoAAAANSUhEUgAAAAIAAAABCAIAAAB7QOjdAAAAD0lEQVR4nGP4z8Dwn4EBAAj+Af9IxWaQAAAAAElFTkSuQmCC"
    readonly property string threeWide: "iVBORw0KGgoAAAANSUhEUgAAAAMAAAABCAIAAACUgoPjAAAADUlEQVR4nGNgYPgPQQAL/gL+kc2Z/gAAAABJRU5ErkJggg=="

    readonly property var localEstimate: ({
        local: {source: "measured_history", samples: 4, gpu_s_p50: "8.0", gpu_s_p90: "10.0",
                wh_p50: "2.000", wh_p90: "2.400"},
        wall_s: {source: "measured_history", samples: 4, p50: "20.0", p90: "30.0"},
        unknown: []
    })

    function wallEstimate(p50) {
        return {wall_s: {source: "measured_history", samples: 3, p50: p50.toFixed(1),
                         p90: (p50 * 1.5).toFixed(1)}, unknown: []}
    }

    // KDDockWidgets' names are process-wide: a shell of the test before deletes its docks later.
    function init() {
        answered = {}
        failOnWarning(/Binding loop/)
        wait(100)
    }

    // The panels go before their FakeCore, so their subscriptions end on a live core.
    property var windows: []
    function cleanup() {
        for (const w of windows)
            w.destroy()
        windows = []
        wait(0)
    }

    function iso(msAgo) {
        return new Date(Date.now() - (msAgo || 0)).toISOString().replace(/\.\d+Z$/, "Z")
    }

    function job(id, over) {
        const base = {job_id: id, kind: "comfy.prompt", lane: "gpu-heavy", priority: "user",
                      status: "running", substatus: "executing", created: iso(60000),
                      updated: iso(), started: iso(5000), attempt: 1, trace_id: "t_" + id,
                      batch_id: "b_" + id, index: 0, origin: {surface: "panel", id: "image"},
                      status_seq: [{status: "running", substatus: "executing", at: iso(5000)}]}
        return Object.assign(base, over || {})
    }

    function queued(id, position, over) {
        return job(id, Object.assign({status: "queued", substatus: "waiting", started: undefined,
                                      queue_position: position,
                                      status_seq: [{status: "queued", substatus: "waiting", at: iso(1000)}]},
                                     over || {}))
    }

    function lane(over) {
        return Object.assign({lane: "gpu-heavy", paused: false, reasons: [], held: 0, restart_held: 0,
                              withdrawn: 0}, over || {})
    }

    function comfy(running, queuedCount) {
        return {engine: "comfyui", class: "owned", state: "ready", url: "http://127.0.0.1:8188",
                listen: "loopback", outside_jobs: {running: running, queued: queuedCount},
                restart_needed: false, launch_profile: null, asset_catalogue: "off"}
    }

    // An app/* method's result is its value itself (Core's appCall).
    function okState(state) {
        return Object.assign({status: "ok"}, state)
    }

    function ok(structured) {
        return {status: "ok", structured: structured, artifacts: [], summary_text: ""}
    }

    property var answered: ({})

    function openCalls(core, pred) {
        return core.calls.filter(c => !answered[c.id] && core.cancelled.indexOf(c.id) < 0 && pred(c))
    }

    // Answers the oldest open call `pred` matches, waiting for it.
    function answer(core, pred, outcome, what) {
        tryVerify(() => openCalls(core, pred).length > 0, 3000, "a call: " + (what || ""))
        const call = openCalls(core, pred)[0]
        answered[call.id] = true
        verify(core.respond(call.id, outcome), "answered " + (what || call.name))
        return call
    }

    function isPage(c) {
        return c.name === "jobs.list" && c.args.order === undefined && c.args.status === undefined
    }
    function isQueueRead(c) { return c.name === "jobs.list" && c.args.order === "queue" }
    function isRunningRead(c) { return c.name === "jobs.list" && c.args.status === "running" }
    function isQueuedRead(c) { return c.name === "jobs.list" && c.args.status === "queued" }

    // Answers one listing of a live read, every page of it, as the real core would over `jobs`.
    function answerListing(core, pred, jobs, lanes, what) {
        let cursor = undefined
        for (;;) {
            tryVerify(() => openCalls(core, pred).length > 0, 3000, "a call: " + what)
            const call = openCalls(core, pred)[0]
            compare(call.args.cursor, cursor, "the cursor of the page before: " + what)
            const outcome = core.jobsListAnswer(call.args, jobs, lanes)
            answered[call.id] = true
            verify(core.respond(call.id, outcome), "answered " + what)
            cursor = outcome.structured.next_cursor || undefined
            if (!cursor)
                return
        }
    }

    // Answers a live read as the real core does: the running jobs, the queue (`queue` holds every
    // queued job, those the core has dispatched included, which that read leaves out) and the
    // `{status: "queued"}` read, in pages.
    function answerLive(core, running, queue, lanes) {
        const all = running.concat(queue)
        const states = lanes || [lane()]
        answerListing(core, isRunningRead, all, states, "running")
        answerListing(core, isQueueRead, all, states, "queue")
        answerListing(core, isQueuedRead, all, states, "queued")
    }

    // The panel, loaded: the history page, the live read, the engines and the caps. The running
    // read lists the running jobs of `jobs` unless `running` says otherwise.
    function load(setup) {
        const s = setup || {}
        const core = createTemporaryObject(fakeComponent, tc)
        const w = createTemporaryObject(panelWindow, tc, {core: core})
        windows.push(w)
        answer(core, isPage, ok({jobs: s.jobs || [], next_cursor: null, lanes: s.lanes || [lane()]}), "page")
        answerLive(core, s.running || (s.jobs || []).filter(j => j.status === "running"), s.queue || [], s.lanes)
        answer(core, c => c.name === "engines.status", ok({engines: s.engines || [comfy(0, 0)]}), "engines")
        answer(core, c => c.name === "caps.get", ok(s.caps || {display_currency: "USD", period: "2026-10",
                                                              caps: []}), "caps")
        tryVerify(() => w.panel.model.loaded && w.panel.live.loaded)
        return {window: w, panel: w.panel, core: core}
    }

    // The live list's job ids, in order.
    function liveOrder(panel) {
        const out = []
        for (let i = 0; i < panel.live.count; ++i)
            out.push(panel.live.get(i).job_id)
        return out
    }

    function findItem(root, test) {
        if (!root)
            return null
        if (test(root))
            return root
        const kids = root.children || []
        for (let i = 0; i < kids.length; ++i) {
            const found = findItem(kids[i], test)
            if (found)
                return found
        }
        return null
    }

    function button(root, text) {
        return findItem(root, i => i.visible && typeof i.click === "function" && i.text === text)
    }

    // The row of `id`, with its controls in.
    function row(panel, id) {
        let r = null
        tryVerify(() => (r = panel.rowFor(id)) !== null && r.controlsReady, 3000, "the row of " + id)
        return r
    }

    function callsOf(core, pred) {
        return core.calls.filter(pred)
    }

    function test_rows_update_in_place_at_most_10_hz() { // spec: APP-114, APP-139
        const t = load({jobs: [job("j1", {progress: {value: 0, max: 50}})]})
        const r = row(t.panel, "j1")
        compare(r.progressValue, 0)
        const applied = r.progressApplied
        // 50 progress events in a second, one every 20 ms.
        const start = Date.now()
        for (let i = 1; i <= 50; ++i) {
            while (Date.now() < start + (i - 1) * 20)
                wait(1)
            t.core.emit("job:j1", "event", {kind: "progress", value: i, max: 50})
        }
        verify(Date.now() - start < 1100, "the events took " + (Date.now() - start) + " ms")
        tryCompare(r, "progressValue", 50, 1000, "the row shows the newest")
        const n = r.progressApplied - applied
        verify(n <= 11, "at most 11 updates applied: " + n)
        verify(n >= 5, "and not just the last: " + n)
        verify(t.panel.rowFor("j1") === r, "the same row, updated in place")
        // A jobs event of the job keeps the row too.
        t.core.emit("jobs", "event", {kind: "job", job: job("j1", {progress: {value: 50, max: 50},
                                                                   phase: "decoding", updated: iso()})})
        tryVerify(() => r.statusText.indexOf("Decoding") >= 0, 2000, r.statusText)
        verify(t.panel.rowFor("j1") === r, "still the same row")
    }

    function test_preview_url_changes_with_each_frame() { // spec: APP-114
        const t = load({jobs: [job("j1")]})
        const r = row(t.panel, "j1")
        t.core.emitFromCore("preview:j1", "event", {format: "png", data_b64: twoWide})
        tryVerify(() => r.preview.source.toString().startsWith("image://lilpreview/j1/"), 2000)
        const first = r.preview.source.toString()
        t.core.emitFromCore("preview:j1", "event", {format: "png", data_b64: threeWide})
        tryVerify(() => r.preview.source.toString() !== first, 2000, "a new URL for the new frame")
        verify(r.preview.source.toString().startsWith("image://lilpreview/j1/"))
        tryCompare(r.preview, "status", Image.Ready)
    }

    function test_lane_header_shows_waiting_for_vram_and_outside_jobs() { // spec: APP-114
        const t = load({jobs: [queued("j2", 0, {substatus: "waiting_for_vram"})],
                        queue: [queued("j2", 0, {substatus: "waiting_for_vram"})],
                        engines: [comfy(1, 2)]})
        const header = t.panel.laneHeader("gpu-heavy")
        verify(header.text.indexOf("Waiting for GPU memory") >= 0, header.text)
        verify(header.text.indexOf("ComfyUI: 1 outside job running, 2 queued") >= 0, header.text)
        verify(row(t.panel, "j2").statusText.indexOf("Waiting for GPU memory") >= 0)
        // The engines topic keeps it current.
        t.core.emit("engines", "event", {kind: "outside_job", engine: comfy(0, 3)})
        tryVerify(() => header.text.indexOf("ComfyUI: 3 outside jobs queued") >= 0, 2000, header.text)
        t.core.emit("engines", "event", {kind: "outside_job", engine: comfy(0, 0)})
        tryVerify(() => header.text.indexOf("outside") < 0, 2000, header.text)
    }

    function test_run_next_and_last_call_job_move() { // spec: APP-114, CORE-160
        const t = load({jobs: [queued("j2", 0), queued("j3", 1)], queue: [queued("j2", 0), queued("j3", 1)]})
        const r = row(t.panel, "j3")
        button(r, "Run next").click()
        const move = answer(t.core, c => c.name === "job.move", ok({job: queued("j3", 0)}), "move")
        compare(move.args, {job_id: "j3", to: "next"})
        // The queue order is read again after the move, and the places are the read's, whatever
        // the history rows' summaries still say (I-1).
        answerLive(t.core, [], [queued("j3", 0), queued("j2", 1)])
        tryVerify(() => row(t.panel, "j3").statusText.indexOf("#1 in queue") >= 0, 2000, row(t.panel, "j3").statusText)
        verify(row(t.panel, "j2").statusText.indexOf("#2 in queue") >= 0, row(t.panel, "j2").statusText)
        compare(liveOrder(t.panel), ["j3", "j2"])
        button(row(t.panel, "j2"), "Run last").click()
        const last = answer(t.core, c => c.name === "job.move",
                            {status: "error", error: {code: "job.not_queued", category: "conflict",
                                                      message: "The job isn't queued any more.",
                                                      trace_id: "t", retry: "never"}}, "move last")
        compare(last.args, {job_id: "j2", to: "last"})
        tryVerify(() => t.panel.banner.visible && t.panel.banner.text.indexOf("isn't queued") >= 0, 2000,
                  "the refusal is shown")
    }

    function test_cancel_batch_and_all_mine() { // spec: APP-114, CORE-160
        const batch = {batch_id: "b_9"}
        const t = load({jobs: [job("j1", batch), queued("j2", 0, Object.assign({index: 1}, batch)),
                               queued("j4", 1)],
                        queue: [queued("j2", 0, Object.assign({index: 1}, batch)), queued("j4", 1)]})
        verify(!button(row(t.panel, "j4"), "Cancel batch"), "a batch of one has no batch cancel")
        button(row(t.panel, "j2"), "Cancel batch (2)").click()
        compare(answer(t.core, c => c.name === "job.cancel", ok({jobs: []})).args, {batch_id: "b_9"})
        button(row(t.panel, "j4"), "Cancel").click()
        compare(answer(t.core, c => c.name === "job.cancel", ok({job: queued("j4", 1)})).args, {job_id: "j4"})
        button(t.panel, "Cancel all from this app").click()
        compare(answer(t.core, c => c.name === "job.cancel",
                       ok({jobs: [queued("j2", 0, {status: "cancelled"}), queued("j4", 1, {status: "cancelled"})]})).args,
                {all_mine: true})
        tryVerify(() => t.panel.banner.visible && t.panel.banner.text === "Cancelled 2 jobs.", 2000,
                  t.panel.banner.text)
    }

    // The panel with nothing answered yet: a link can ask for a job before any read is in.
    function unanswered() {
        const core = createTemporaryObject(fakeComponent, tc)
        const w = createTemporaryObject(panelWindow, tc, {core: core})
        windows.push(w)
        return {window: w, panel: w.panel, core: core}
    }

    // The top of the live list's header, and the lane's own control, are inside the view.
    function headerInView(panel) {
        const v = panel.liveView
        if (!v.headerItem || !panel.laneHeader("gpu-heavy"))
            return false
        const button = panel.laneHeader("gpu-heavy")
        return v.headerItem.mapToItem(v, 0, 0).y >= -1 && v.contentY <= v.originY + 1
               && button.mapToItem(v, 0, 0).y >= -1
    }

    function test_the_header_is_in_view_on_open_and_as_rows_come() { // spec: APP-114, CORE-160
        // The lanes come with the first read, so the header grows after the list is made: Pause
        // must not start above the view.
        const paused = [lane({paused: true, reasons: ["manual"], held: 3}), lane({lane: "cpu"})]
        // More rows than the list's share of the panel holds: then it can scroll, and did.
        const first = [Shapes.job("r1", "running"), Shapes.job("r2", "running")]
        const waiting = []
        for (let i = 0; i < 3; ++i)
            waiting.push(Shapes.job("q" + i, "queued", {queue_position: i}))
        const t = load({lanes: paused, running: first, queue: waiting})
        tryVerify(() => t.panel.liveView.count === 5 && t.panel.liveView.headerItem.height > 100, 2000, "the rows and the lane headers")
        wait(100)
        verify(headerInView(t.panel), "in view on open: contentY " + t.panel.liveView.contentY + ", originY " + t.panel.liveView.originY)

        // More rows arrive, by an event and by the read it starts.
        t.core.emit("jobs", "event", {kind: "job", job: Shapes.job("r3", "running")})
        answerLive(t.core, first.concat([Shapes.job("r3", "running")]), waiting, paused)
        tryVerify(() => t.panel.live.count === 6, 2000)
        wait(100)
        verify(headerInView(t.panel), "in view after rows arrive")

        // The panel's height changes.
        t.window.height = 500
        tryVerify(() => t.panel.height === 500, 2000)
        wait(50)
        verify(headerInView(t.panel), "in view at another height")
        t.window.height = 780
        wait(50)
        verify(headerInView(t.panel), "and back")
    }

    function test_the_view_does_not_jump_while_you_have_scrolled() { // spec: APP-114
        const queue = []
        for (let i = 0; i < 40; ++i)
            queue.push(Shapes.job("q" + i, "queued", {queue_position: i}))
        const t = load({queue: queue})
        const v = t.panel.liveView
        tryVerify(() => v.count === 40 && v.contentHeight > v.height + 400 && headerInView(t.panel), 3000,
                  "a long list that can scroll, from its top")

        const away = v.originY + 300
        v.contentY = away
        // The header grows above, and a row comes.
        t.core.emit("jobs", "event", {kind: "queue", state: lane({paused: true, reasons: ["manual"], held: 3})})
        tryVerify(() => t.panel.lanes["gpu-heavy"].paused, 2000)
        t.core.emit("jobs", "event", {kind: "job", job: Shapes.job("r1", "running")})
        answerLive(t.core, [Shapes.job("r1", "running")], queue)
        tryVerify(() => v.count === 41, 2000)
        wait(50)
        compare(v.contentY, away, "you stay where you scrolled")

        // Back at the top, the view follows the header again.
        v.contentY = v.originY
        t.core.emit("jobs", "event", {kind: "queue", state: lane()})
        tryVerify(() => !t.panel.lanes["gpu-heavy"].paused, 2000)
        wait(50)
        verify(headerInView(t.panel), "from the top it stays at the top")
    }

    // Every listing of a read refused, as a lost connection refuses them.
    function refuseLive(core) {
        const no = Shapes.refusal("core.unreachable", "No core.")
        answer(core, isRunningRead, no, "the running read")
        answer(core, isQueueRead, no, "the queue read")
        answer(core, isQueuedRead, no, "the queued read")
    }

    function pauseControl(panel) {
        return findText(panel.laneHeader("gpu-heavy"), ["Pause", "Resume"])
    }

    function findText(item, texts) {
        if (texts.indexOf(item.text) >= 0)
            return item
        for (let i = 0; i < item.children.length; ++i) {
            const found = findText(item.children[i], texts)
            if (found)
                return found
        }
        return null
    }

    function test_the_pause_recovers_after_failed_reads() { // spec: ARCH-100, CORE-160
        const paused = [lane({paused: true, reasons: ["manual"]})]
        const t = load({lanes: paused})
        const button = pauseControl(t.panel)
        verify(button)
        tryVerify(() => button.enabled, 2000)
        t.core.applyState({connection: "reconnecting"})
        t.core.applyState({connection: "ready", workspace: null})
        verify(!button.enabled, "waiting for the read")

        // The read fails, twice: the control stays off, and it is asked again.
        refuseLive(t.core)
        verify(!t.panel.lanesCurrent, "a failed read isn't current")
        verify(!button.enabled)
        tryVerify(() => openCalls(t.core, isRunningRead).length > 0, 3500, "read again")
        refuseLive(t.core)
        verify(!t.panel.lanesCurrent)
        tryVerify(() => openCalls(t.core, isRunningRead).length > 0, 3500, "and again")

        // Then it works: the state is the new core's, and the control comes back by itself.
        answerLive(t.core, [], [], [lane()])
        tryVerify(() => t.panel.lanesCurrent, 2000, "it recovers")
        tryVerify(() => button.enabled, 2000)
        compare(pauseControl(t.panel).text, "Pause", "a restart ended the pause")
    }

    function test_a_read_asked_for_during_a_failing_one_goes_at_once() { // spec: ARCH-100, CORE-160
        const t = load()
        const reads = () => t.core.calls.filter(isRunningRead).length
        // The connection flaps: `ready` finds a read out, so it asks for another.
        t.core.applyState({connection: "reconnecting"})
        t.core.applyState({connection: "ready", workspace: null})
        t.core.applyState({connection: "reconnecting"})
        t.core.applyState({connection: "ready", workspace: null})
        const before = reads()

        // The old connection's read fails: the one asked for goes now, not after the 2 s retry.
        refuseLive(t.core)
        compare(reads(), before + 1, "at once")
        verify(!t.panel.lanesCurrent)
        answerLive(t.core, [], [])
        tryVerify(() => t.panel.lanesCurrent, 2000, "and that one counts")
    }

    function test_a_wanted_job_can_land_after_the_panel_gave_up() { // spec: APP-114, P4a-R21
        // Everything is loaded: a link's job isn't there, and no more history can be fetched.
        const t = load()
        verify(!t.panel.showJob("j9", false))
        verify(t.panel.banner.visible)
        verify(t.panel.banner.text.indexOf("isn't in the list") >= 0, t.panel.banner.text)

        // Its event comes a moment later.
        t.core.emit("jobs", "event", {kind: "job", job: Shapes.job("j9", "running")})
        tryCompare(t.panel, "wantedJob", "", 2000, "it landed")
        compare(t.panel.live.get(t.panel.liveView.currentIndex).job_id, "j9")
        verify(!t.panel.banner.visible, "and the words went")

        // What comes after giving up doesn't say it again: the words are said once.
        verify(!t.panel.showJob("j8", false))
        verify(t.panel.banner.visible)
        t.panel.banner.close()
        t.core.emit("jobs", "event", {kind: "job", job: Shapes.job("old", "succeeded")})
        tryVerify(() => t.panel.historyView.count === 1, 2000, "a row came, which looks again")
        wait(50)
        verify(!t.panel.banner.visible, "said once")
        compare(t.panel.wantedJob, "j8")
        answerLiveIfAsked(t.core)

        // A selection by you still drops it, and the words with it.
        verify(!t.panel.showJob("j7", false))
        verify(t.panel.banner.visible)
        const r = t.panel.rowFor("j9")
        verify(r)
        wait(50)
        mouseClick(r, 5, 5)
        compare(t.panel.wantedJob, "")
        verify(!t.panel.banner.visible)
        const chosen = t.panel.liveView.currentIndex
        t.core.emit("jobs", "event", {kind: "job", job: Shapes.job("j7", "running")})
        tryVerify(() => t.panel.live.rowOf("j7") >= 0, 2000)
        compare(t.panel.live.get(t.panel.liveView.currentIndex).job_id, "j9", "still yours: " + chosen)
    }

    function answerLiveIfAsked(core) {
        if (openCalls(core, isRunningRead).length > 0)
            answerLive(core, [Shapes.job("j9", "running")], [])
    }

    function test_a_wanted_job_lands_when_an_event_brings_it() { // spec: APP-114, P4a-R21
        const t = unanswered()
        verify(!t.panel.showJob("j9", false), "it isn't listed yet")
        compare(t.panel.wantedJob, "j9")
        t.core.emit("jobs", "event", {kind: "job", job: Shapes.job("j9", "running")})
        // No read has been answered: the event alone brought the row.
        compare(answeredReads(t.core), 0)
        tryCompare(t.panel, "wantedJob", "", 2000, "it landed")
        const row = t.panel.liveView.currentIndex
        verify(row >= 0, "its row is selected")
        compare(t.panel.live.get(row).job_id, "j9")
    }

    function answeredReads(core) {
        return core.calls.filter(c => c.name === "jobs.list" && answered[c.id]).length
    }

    function test_a_late_job_does_not_replace_the_row_you_just_selected() { // spec: APP-114, P4a-R21
        // A link asks for a job that isn't listed yet; you pick a row while the panel looks.
        const t = unanswered()
        const r1 = Shapes.job("r1", "running")
        const r2 = Shapes.job("r2", "running", {created: "2026-10-30T12:00:05Z"})
        verify(!t.panel.showJob("j9", false))
        answerLive(t.core, [r1, r2], [])
        tryVerify(() => t.panel.live.count === 2 && t.panel.rowFor("r2"), 3000, "the rows")
        compare(t.panel.wantedJob, "j9", "still looked for")
        wait(50)
        const picked = t.panel.rowFor("r2")
        mouseClick(picked, 5, 5)
        const chosen = t.panel.live.rowOf("r2")
        compare(t.panel.liveView.currentIndex, chosen, "your row")
        compare(t.panel.wantedJob, "", "you chose: the panel stops looking")

        // The wanted job comes after all: the selection stays yours.
        t.core.emit("jobs", "event", {kind: "job", job: Shapes.job("j9", "running", {created: "2026-10-30T11:00:00Z"})})
        tryVerify(() => t.panel.live.rowOf("j9") >= 0, 2000, "its row came")
        wait(50)
        compare(t.panel.liveView.currentIndex, t.panel.live.rowOf("r2"), "still your row")
    }

    function test_the_arrow_keys_are_a_selection_too() { // spec: APP-114, APP-156
        const t = unanswered()
        verify(!t.panel.showJob("j9", false))
        answerLive(t.core, [Shapes.job("r1", "running"), Shapes.job("r2", "running")], [])
        tryVerify(() => t.panel.live.count === 2)
        t.window.requestActivate()
        tryVerify(() => t.window.active, 2000, "the window takes the keys")
        t.panel.liveView.forceActiveFocus(Qt.TabFocusReason)
        tryVerify(() => t.panel.liveView.activeFocus, 2000)
        t.panel.liveView.currentIndex = 0
        compare(t.panel.wantedJob, "j9", "only a person's selection stops it, not a row moving")
        keyClick(Qt.Key_Down)
        compare(t.panel.liveView.currentIndex, 1, "the key moved it")
        compare(t.panel.wantedJob, "")
    }

    function test_the_pause_waits_for_a_read_after_a_reconnect() { // spec: ARCH-100, CORE-160, APP-114
        // A restart ends a pause: the state from before the loss must not decide Pause or Resume.
        const paused = [lane({paused: true, reasons: ["manual"]})]
        const t = load({lanes: paused})
        tryVerify(() => t.panel.laneHeader("gpu-heavy") && t.panel.lanes["gpu-heavy"].paused)
        verify(t.panel.lanesCurrent)
        const queueCalls = () => t.core.calls.filter(c => c.name.startsWith("app/queue.")).length
        const reads = () => t.core.calls.filter(isRunningRead).length

        t.core.applyState({connection: "reconnecting"})
        verify(!t.panel.lanesCurrent, "at once")
        const before = reads()
        t.core.applyState({connection: "ready", workspace: null})
        compare(reads(), before + 1, "`ready` reads: no mark comes on a same-core reconnect")
        verify(!t.panel.lanesCurrent, "not until that read is in")
        t.panel.resume("gpu-heavy")
        compare(queueCalls(), 0, "the old state must not send a resume")

        // A mark during the read: that read no longer speaks for the new core.
        t.core.emit("jobs", "resync_required", null)
        answerLive(t.core, [], [], paused)
        verify(!t.panel.lanesCurrent, "a read from before the mark")
        t.panel.resume("gpu-heavy")
        compare(queueCalls(), 0)

        answerLive(t.core, [], [], [lane()])
        tryVerify(() => t.panel.lanesCurrent, 2000, "the read after it")
        verify(!t.panel.lanes["gpu-heavy"].paused, "the new core's state")
        // The mark reloaded the history too, and writes wait for that as well.
        answer(t.core, isPage, ok({jobs: [], next_cursor: null, lanes: [lane()]}), "the history's reload")
        tryVerify(() => t.panel.writable, 2000)
        t.panel.pause("gpu-heavy")
        compare(t.core.calls.filter(c => c.name === "app/queue.pause").length, 1)
    }

    function test_a_read_spanning_a_loss_does_not_count() { // spec: ARCH-100, CORE-160
        const t = load()
        t.core.emit("jobs", "event", {kind: "job", job: Shapes.job("q1", "queued")})
        tryVerify(() => openCalls(t.core, isRunningRead).length > 0, 2000, "a read out")
        t.core.applyState({connection: "reconnecting"})
        answerLive(t.core, [], [])
        verify(!t.panel.lanesCurrent, "it began before the loss")
        t.core.applyState({connection: "ready", workspace: null})
        tryVerify(() => openCalls(t.core, isRunningRead).length > 0, 2000, "the read for the new connection")
        answerLive(t.core, [], [])
        tryVerify(() => t.panel.lanesCurrent, 2000)
    }

    function test_a_lost_connection_disables_the_panels_writes() { // spec: APP-034, APP-114
        const t = load({jobs: [job("r1"), job("q1", {status: "queued", substatus: "waiting"})],
                        running: [job("r1")], queue: [job("q1", {status: "queued", substatus: "waiting"})]})
        verify(t.panel.writable)
        const marked = root => {
            const out = []
            const walk = item => {
                if (item.requiresCore === true && item.visible)
                    out.push(item)
                for (let i = 0; i < item.children.length; ++i)
                    walk(item.children[i])
            }
            walk(root)
            return out
        }
        tryVerify(() => marked(t.panel).length >= 3, 3000, "pause, and the rows' Cancel and Run next")
        const buttons = marked(t.panel)
        // By the property, not only through `writable`: that is what a test of the rule can walk.
        for (const text of ["Pause", "Run next", "Run last", "Cancel"])
            verify(buttons.some(b => b.text === text), text + " is marked `requiresCore`")
        for (const b of buttons)
            verify(b.enabled, "live while ready: " + b.text)
        const made = t.core.calls.length

        t.core.connection = "reconnecting"
        verify(!t.panel.writable)
        for (const b of buttons)
            verify(!b.enabled, "disabled at once: " + b.text)
        t.panel.pause("gpu-heavy")
        t.panel.cancelJob("r1")
        compare(t.core.calls.length, made, "no call goes out")

        t.core.connection = "ready"
        verify(t.panel.writable)
        answerLive(t.core, [job("r1")], [job("q1", {status: "queued", substatus: "waiting"})])
        tryVerify(() => t.panel.lanesCurrent, 2000, "the read after `ready`: the pause waits for it")
        for (const b of buttons)
            verify(b.enabled, "back: " + b.text)
    }

    function test_an_unknown_reason_is_cut_like_all_outside_text() { // spec: APP-114, P4a-R8
        const t = load()
        const label = t.panel.pausedLabel(["x".repeat(200)])
        verify(label.length <= 60, "cut: " + label.length)
    }

    function test_pause_and_resume_call_app_queue() { // spec: APP-114, CORE-160
        const t = load({jobs: [job("j1")]})
        const header = t.panel.laneHeader("gpu-heavy")
        const pause = button(header, "Pause")
        compare(LilShim.accessibleOf(pause).name, "Pause GPU work")
        pause.click()
        const p = answer(t.core, c => c.kind === "appCall" && c.name === "app/queue.pause",
                         okState(lane({paused: true, reasons: ["manual"]})), "pause")
        compare(p.args, {lane: "gpu-heavy"})
        tryVerify(() => header.text.indexOf("Paused by you") >= 0, 2000, header.text)
        wait(Qt.styleHints.mouseDoubleClickInterval + 50)
        button(header, "Resume").click()
        const r = answer(t.core, c => c.kind === "appCall" && c.name === "app/queue.resume", okState(lane()), "resume")
        compare(r.args, {lane: "gpu-heavy"})
        tryVerify(() => header.text.indexOf("Paused") < 0, 2000, header.text)
        verify(button(header, "Pause"), "Pause again")
    }

    function test_paused_header_names_manual_and_fullscreen() { // spec: APP-114, CORE-160
        const t = load()
        const header = t.panel.laneHeader("gpu-heavy")
        for (const [reasons, text] of [[["manual"], "Paused by you"],
                                       [["fullscreen"], "Paused while fullscreen"],
                                       [["manual", "fullscreen"], "Paused by you and while fullscreen"]]) {
            t.core.emit("jobs", "event", {kind: "queue", state: lane({paused: true, reasons: reasons})})
            tryVerify(() => header.text.indexOf(text) >= 0, 2000, header.text)
        }
        // Paused while fullscreen only: your own pause isn't set, so the control pauses.
        t.core.emit("jobs", "event", {kind: "queue", state: lane({paused: true, reasons: ["fullscreen"]})})
        tryVerify(() => header.text.indexOf("Paused while fullscreen") >= 0, 2000)
        verify(button(header, "Pause"), "Pause, since Resume clears only your own pause")
        // A burst of queue events is one header update, and the newest wins.
        wait(100)
        const before = t.panel.laneUpdates
        for (let i = 0; i < 5; ++i)
            t.core.emit("jobs", "event", {kind: "queue", state: lane({paused: i < 4, reasons: i < 4 ? ["manual"] : [],
                                                                     held: i})})
        tryVerify(() => t.panel.laneUpdates > before, 2000)
        wait(100)
        compare(t.panel.laneUpdates - before, 1, "one update for the burst")
        verify(header.text.indexOf("Paused") < 0, header.text)
    }

    function test_a_held_row_names_its_reasons() { // spec: APP-114, CORE-160
        const both = queued("j2", 0, {substatus: "queue_paused", pause_reasons: ["manual", "fullscreen"]})
        const held = queued("j3", undefined, {substatus: "queue_paused", pause_reasons: ["restart"],
                                              queue_position: undefined})
        const t = load({jobs: [both, held], queue: [both, held],
                        lanes: [lane({paused: true, reasons: ["manual", "fullscreen"], held: 2, restart_held: 1})]})
        const a = row(t.panel, "j2").statusText
        verify(a.indexOf("Paused by you") >= 0 && a.indexOf("Paused while fullscreen") >= 0, a)
        verify(a.indexOf("#1 in queue") >= 0, a)
        verify(a.indexOf("about") < 0, "no finish time while your pause holds it: " + a)
        const b = row(t.panel, "j3").statusText
        verify(b.indexOf("Held after a restart") >= 0, b)
        compare(b.toLowerCase().split("held").length - 1, 1, "it says held once: " + b)
        verify(b.indexOf("#") < 0 && b.indexOf(" 0") < 0, "a restart-held job has no place: " + b)
        // job.move leaves a restart-held job held: it offers Cancel, not a move.
        verify(!button(row(t.panel, "j3"), "Run next") && !button(row(t.panel, "j3"), "Run last"))
        verify(button(row(t.panel, "j3"), "Cancel"))
        verify(button(row(t.panel, "j2"), "Run next"), "a job your pause holds still moves")
    }

    function test_restart_held_jobs_show_a_resume_control_when_not_paused() { // spec: APP-114, CORE-160
        const t = load({lanes: [lane({paused: false, held: 3, restart_held: 3})]})
        const header = t.panel.laneHeader("gpu-heavy")
        const held = button(header, "Resume held jobs (3)")
        verify(held, "the held jobs' resume")
        verify(button(header, "Pause"), "the lane isn't paused")
        held.click()
        const r = answer(t.core, c => c.name === "app/queue.resume", okState(lane()), "resume held")
        compare(r.args, {lane: "gpu-heavy", held_only: true})
        tryVerify(() => !button(header, "Resume held jobs (3)"), 2000)
        t.core.emit("jobs", "event", {kind: "queue", state: lane({held: 0, restart_held: 0})})
        wait(100)
        verify(!findItem(header, i => i.visible && typeof i.click === "function"
                                      && String(i.text).startsWith("Resume held")), "the control is gone")
    }

    function test_resume_held_jobs_keeps_your_pause() { // spec: APP-114, CORE-160
        const held = queued("j3", undefined, {substatus: "queue_paused", pause_reasons: ["restart", "manual"],
                                              queue_position: undefined})
        const t = load({jobs: [held], queue: [held],
                        lanes: [lane({paused: true, reasons: ["manual"], held: 5, restart_held: 3})]})
        const header = t.panel.laneHeader("gpu-heavy")
        verify(button(header, "Resume"), "Resume")
        const control = button(header, "Resume held jobs (3)")
        verify(control, "and Resume held jobs (3)")
        const before = t.core.calls.length
        control.click()
        const r = answer(t.core, c => c.name === "app/queue.resume",
                         okState(lane({paused: true, reasons: ["manual"], held: 5, restart_held: 0})), "resume held")
        compare(r.args, {lane: "gpu-heavy", held_only: true}, "held_only, and nothing else")
        t.core.emit("jobs", "event", {kind: "queue", state: lane({paused: true, reasons: ["manual"], held: 5,
                                                                   restart_held: 0})})
        // The released job still waits under your pause.
        t.core.emit("jobs", "event", {kind: "job", job: queued("j3", 0, {substatus: "queue_paused",
                                                                         pause_reasons: ["manual"],
                                                                         updated: iso()})})
        wait(300)
        verify(header.text.indexOf("Paused by you") >= 0, header.text)
        verify(button(header, "Resume"), "Resume is still there")
        verify(!button(header, "Resume held jobs (3)"), "the held control is gone")
        const sent = t.core.calls.slice(before).filter(c => c.kind === "appCall")
        compare(sent.length, 1, "one app call: " + JSON.stringify(sent))
        verify(sent.every(c => c.args.held_only === true), "no resume of your pause")
        verify(!t.core.calls.some(c => c.name === "job.move" || c.name === "job.retry"), "no job is started")
        verify(row(t.panel, "j3").statusText.indexOf("Paused by you") >= 0)
        wait(Qt.styleHints.mouseDoubleClickInterval + 50)
        button(header, "Resume").click()
        compare(answer(t.core, c => c.name === "app/queue.resume", okState(lane())).args, {lane: "gpu-heavy"})
    }

    function test_unknown_kind_origin_and_reason_show_their_raw_names() { // spec: APP-114
        const odd = queued("j5", 0, {kind: "future.render", origin: {surface: "director_task", id: "j_0"},
                                     substatus: "queue_paused", pause_reasons: ["agent_quiet"]})
        const t = load({jobs: [odd], queue: [odd],
                        lanes: [lane({paused: true, reasons: ["agent_quiet"]})]})
        const r = row(t.panel, "j5")
        verify(r.titleText.indexOf("future.render") >= 0, r.titleText)
        verify(r.titleText.indexOf("director_task") >= 0, r.titleText)
        verify(r.statusText.indexOf("agent_quiet") >= 0, r.statusText)
        verify(t.panel.laneHeader("gpu-heavy").text.indexOf("agent_quiet") >= 0)
        // Known ones read as words.
        const known = row(t.panel, "j5")
        t.core.emit("jobs", "event", {kind: "job", job: queued("j5", 0, {kind: "comfy.prompt",
                                                                         origin: {surface: "mcp"},
                                                                         updated: iso()})})
        tryVerify(() => known.titleText.indexOf("ComfyUI prompt") >= 0 && known.titleText.indexOf("Claude Code") >= 0,
                  2000, known.titleText)
    }

    function test_retry_shows_the_cost_chip_from_the_jobs_estimate() { // spec: APP-114, APP-140
        const failed = job("j6", {status: "failed", substatus: null, finished: iso(1000), estimate: localEstimate,
                                  error: {code: "comfyui.out_of_memory", category: "resource",
                                          message: "ComfyUI ran out of GPU memory, also on 2 retries.",
                                          trace_id: "t", retry: "after_resolve",
                                          fix: {ui_action: {id: "retry_job", label: "Retry the job"},
                                                user_hint: "Close other programs that use the GPU."}}})
        const paid = job("j7", {status: "failed", substatus: null, finished: iso(1000),
                                error: {code: "comfyui.out_of_memory", category: "resource",
                                        message: "ComfyUI ran out of GPU memory.", trace_id: "t",
                                        retry: "after_resolve",
                                        fix: {ui_action: {id: "show_job", label: "Show the job"}}}})
        const later = job("j8", {status: "failed", substatus: null, finished: iso(1000),
                                 error: {code: "x.y", category: "internal", message: "Something broke.",
                                         trace_id: "t", retry: "never",
                                         fix: {ui_action: {id: "open_wizard", label: "Open the wizard"}}}})
        const t = load({jobs: [failed, paid, later]})
        const r = row(t.panel, "j6")
        verify(r.chip.visible, "the chip shows beside Retry")
        compare(r.chip.text, "local · ~8 s · ~2 Wh")
        verify(r.statusText.indexOf("Close other programs") >= 0, r.statusText)
        verify(!t.core.calls.some(c => c.name === "job.estimate"), "the job's own estimate, no call")
        button(r, "Retry the job").click()
        compare(answer(t.core, c => c.name === "job.retry", ok({job: job("j9")})).args, {job_id: "j6"})

        // A paid job's fix is to look at it: no Retry, and Show the job selects it.
        const p = row(t.panel, "j7")
        verify(!button(p, "Retry the job") && !button(p, "Retry"), "no one-click retry")
        button(p, "Show the job").click()
        verify(t.panel.historyView.currentItem === p, "the job is selected")
        // An action the app doesn't know: its label, and nothing else.
        const u = row(t.panel, "j8")
        verify(!button(u, "Open the wizard"), "not a button")
        verify(u.statusText.indexOf("Open the wizard") >= 0, u.statusText)
    }

    function test_a_retry_waiting_on_an_earlier_prompt_shows_it_with_cancel() { // spec: APP-114, CORE-160
        const waiting = queued("j10", undefined, {substatus: "submitting", retry_of: "j0",
                                                  waiting_for_earlier_prompt: true,
                                                  status_seq: [{status: "queued", substatus: "submitting", at: iso(6000)}]})
        // A retry slow to submit, and a second attempt, with no inherited prompt: just submitting.
        const slow = queued("j11", undefined, {substatus: "submitting", retry_of: "j0",
                                               status_seq: [{status: "queued", substatus: "submitting", at: iso(2500)}]})
        const again = queued("j12", undefined, {substatus: "submitting", attempt: 2,
                                                status_seq: [{status: "queued", substatus: "submitting", at: iso(3000)}]})
        const t = load({jobs: [waiting, slow, again], queue: [waiting, slow, again]})
        const r = row(t.panel, "j10")
        verify(r.statusText.indexOf("Waiting for an earlier ComfyUI prompt to finish") >= 0, r.statusText)
        verify(r.statusText.indexOf("#") < 0, "it has no place: " + r.statusText)
        verify(!button(r, "Run next") && !button(r, "Run last"), "it can't move")
        for (const id of ["j11", "j12"]) {
            const other = row(t.panel, id).statusText
            verify(other.indexOf("Submitting") >= 0 && other.indexOf("earlier") < 0, id + ": " + other)
        }
        button(r, "Cancel").click()
        compare(answer(t.core, c => c.name === "job.cancel", ok({job: waiting})).args, {job_id: "j10"})
    }

    function test_queued_rows_follow_queue_order_and_show_their_eta() { // spec: APP-114, CORE-161
        const running = job("j1", {estimate: wallEstimate(20)})
        const a = queued("j2", 1, {estimate: wallEstimate(60), created: iso(30000)})
        const b = queued("j3", 0, {estimate: wallEstimate(30), created: iso(20000)})
        const c = queued("j4", 3, {created: iso(10000)})
        const t = load({jobs: [c, b, a, running], queue: [b, a, c]})
        const order = () => [0, 1, 2, 3].map(i => {
            t.panel.liveView.currentIndex = i
            return t.panel.liveView.currentItem ? t.panel.liveView.currentItem.jobId : ""
        })
        tryVerify(() => JSON.stringify(order()) === JSON.stringify(["j1", "j3", "j2", "j4"]), 3000,
                  "running first, then queue order: " + order())
        // j2: 15 s left of j1, 30 for j3, 60 its own.
        tryVerify(() => row(t.panel, "j2").statusText.indexOf("about 2 min") >= 0, 3000, row(t.panel, "j2").statusText)
        // j4: one place the panel can't see (position 3, two listed ahead), and no estimate of its own.
        const d = row(t.panel, "j4").statusText
        verify(d.indexOf("after 1 job with no estimate") >= 0 && d.indexOf("plus its own time (no estimate)") >= 0, d)
        // A burst of new jobs re-reads the queue once; an update that changes no status none.
        const reads = callsOf(t.core, isQueueRead).length
        for (let i = 0; i < 5; ++i)
            t.core.emit("jobs", "event", {kind: "job", job: queued("j2" + i, 9, {updated: iso()})})
        tryVerify(() => callsOf(t.core, isQueueRead).length === reads + 1, 2000)
        wait(400)
        compare(callsOf(t.core, isQueueRead).length, reads + 1, "one read for the burst")
        answerLive(t.core, [running], [b, a, c])
        t.core.emit("jobs", "event", {kind: "job", job: job("j1", {estimate: wallEstimate(20), phase: "sampling",
                                                                   updated: iso()})})
        wait(400)
        compare(callsOf(t.core, isQueueRead).length, reads + 1, "a running job's own update needs no read")
    }

    // --- The jobs the core has dispatched: status queued, in no queue place (audit finding 7a). ---

    function dispatched(id, substatus, over) {
        return queued(id, undefined, Object.assign({substatus: substatus,
                                                    status_seq: [{status: "queued", substatus: substatus, at: iso(1000)}]},
                                                   over || {}))
    }

    function test_a_dispatched_job_stays_listed_with_no_place() { // spec: APP-114, CORE-160
        const inEngine = dispatched("j1", "queued_in_engine", {estimate: wallEstimate(30), created: iso(90000)})
        const next = queued("j2", 0, {estimate: wallEstimate(20), created: iso(20000)})
        const cpu = job("c1", {lane: "cpu"})
        const t = load({running: [cpu], queue: [next, inEngine]})
        compare(liveOrder(t.panel), ["c1", "j1", "j2"], "the running jobs, the dispatched one, then the queue")
        verify(!t.panel.emptyText.visible)
        const r = row(t.panel, "j1")
        verify(r.statusText.indexOf("In ComfyUI's queue") >= 0, r.statusText)
        verify(r.statusText.indexOf("#") < 0, "no place: " + r.statusText)
        verify(button(r, "Cancel"), "it can be cancelled")
        verify(!button(r, "Run next") && !button(r, "Run last"), "it can't move")
        // Its time is counted ahead of the queue; the queue's places stay the read's.
        const n = row(t.panel, "j2").statusText
        verify(n.indexOf("#1 in queue") >= 0 && n.indexOf("about 50 s") >= 0, n)
        // The read after another job's event still lists it.
        t.core.emit("jobs", "event", {kind: "job", job: queued("j3", 1, {updated: iso()})})
        answerLive(t.core, [cpu], [next, inEngine, queued("j3", 1)])
        tryVerify(() => liveOrder(t.panel).indexOf("j3") >= 0, 2000, JSON.stringify(liveOrder(t.panel)))
        compare(liveOrder(t.panel), ["c1", "j1", "j2", "j3"])
    }

    function test_nothing_is_running_is_never_said_with_a_dispatched_job() { // spec: APP-114, CORE-160
        for (const substatus of ["submitting", "queued_in_engine"]) {
            answered = {}
            const t = load({queue: [dispatched("j1", substatus)]})
            compare(t.panel.live.count, 1, substatus)
            verify(!t.panel.emptyText.visible, substatus + ": the list doesn't say it is empty")
            // An event of it, as a retry waiting on an earlier prompt gets, and the read it asks for.
            t.core.emit("jobs", "event", {kind: "job", job: dispatched("j1", substatus, {updated: iso()})})
            answerLive(t.core, [], [dispatched("j1", substatus)])
            wait(50)
            compare(t.panel.live.count, 1, substatus + " after the read")
            verify(!t.panel.emptyText.visible, substatus + " after the read")
        }
    }

    function test_every_page_of_the_status_reads_is_followed() { // spec: APP-114, CORE-160
        const running = []
        for (let i = 0; i < 130; ++i)
            running.push(job("r" + (1000 + i), {lane: i % 2 ? "cpu" : "gpu-heavy", created: iso(900000 - i)}))
        const queue = []
        for (let i = 0; i < 120; ++i)
            queue.push(queued("q" + (1000 + i), i, {created: iso(1000 + i)}))
        // Older than them all: on the second `{status: "queued"}` page.
        queue.push(dispatched("d0", "submitting", {created: iso(800000)}))
        const t = load({running: running, queue: queue})
        tryCompare(t.panel.live, "count", 251)
        compare(callsOf(t.core, isRunningRead).length, 2, "the running read follows next_cursor")
        compare(callsOf(t.core, isQueuedRead).length, 2, "the queued read follows next_cursor")
        const order = liveOrder(t.panel)
        compare(order[130], "d0", "after the running jobs, before the queue")
    }

    // --- The queue changes with no status change (Appendix G; audit finding 7b). ---

    function test_a_job_released_by_resume_held_gets_its_place_from_a_new_read() { // spec: APP-114, CORE-160
        const held = queued("j3", undefined, {substatus: "queue_paused", pause_reasons: ["restart"],
                                              estimate: wallEstimate(30)})
        const t = load({queue: [held], lanes: [lane({held: 1, restart_held: 1})]})
        const r = row(t.panel, "j3")
        verify(r.statusText.indexOf("Held after a restart") >= 0, r.statusText)
        button(t.panel.laneHeader("gpu-heavy"), "Resume held jobs (1)").click()
        answer(t.core, c => c.name === "app/queue.resume", okState(lane()), "resume held")
        wait(300)
        const reads = callsOf(t.core, isQueueRead).length
        // The core publishes the job's new state: still queued, so no status changed.
        const released = queued("j3", 0, {estimate: wallEstimate(30), updated: iso()})
        t.core.emit("jobs", "event", {kind: "job", job: released})
        tryVerify(() => callsOf(t.core, isQueueRead).length === reads + 1, 2000, "a read of the queue")
        answerLive(t.core, [], [released])
        tryVerify(() => r.statusText.indexOf("#1 in queue") >= 0, 2000, r.statusText)
        verify(r.statusText.indexOf("about 30 s") >= 0, r.statusText)
    }

    function test_a_move_from_another_surface_reorders_the_queue() { // spec: APP-114, CORE-160
        const a = queued("j2", 0), b = queued("j3", 1)
        const t = load({queue: [a, b]})
        compare(liveOrder(t.panel), ["j2", "j3"])
        // Claude Code moved j3: its summary comes on `jobs`, and nothing here called job.move.
        t.core.emit("jobs", "event", {kind: "job", job: queued("j3", 0, {updated: iso()})})
        answerLive(t.core, [], [queued("j3", 0), queued("j2", 1)])
        tryVerify(() => JSON.stringify(liveOrder(t.panel)) === JSON.stringify(["j3", "j2"]), 2000,
                  JSON.stringify(liveOrder(t.panel)))
        verify(row(t.panel, "j3").statusText.indexOf("#1 in queue") >= 0, row(t.panel, "j3").statusText)
        verify(row(t.panel, "j2").statusText.indexOf("#2 in queue") >= 0, row(t.panel, "j2").statusText)
        compare(callsOf(t.core, c => c.name === "job.move").length, 0)
    }

    // --- A read and a newer queue event (audit finding 7c). ---

    function test_a_read_never_undoes_a_newer_queue_event() { // spec: APP-114, CORE-160
        const q = []
        for (let i = 0; i < 150; ++i)
            q.push(queued("q" + (1000 + i), i))
        const t = load({queue: q})
        const header = t.panel.laneHeader("gpu-heavy")
        t.core.emit("jobs", "event", {kind: "job", job: queued("q1000", 0, {updated: iso()})})
        // A read begins; its first page has the lane as it was...
        tryVerify(() => openCalls(t.core, isQueueRead).length > 0, 2000, "the read")
        answer(t.core, isQueueRead, t.core.jobsListAnswer({order: "queue", limit: 100}, q, [lane()]), "page 1")
        // ...the lane is paused before the read ends...
        t.core.emit("jobs", "event", {kind: "queue", state: lane({paused: true, reasons: ["manual"]})})
        tryVerify(() => header.text.indexOf("Paused by you") >= 0, 2000, header.text)
        answer(t.core, isQueueRead, t.core.jobsListAnswer({order: "queue", limit: 100, cursor: "c100"}, q, [lane()]), "page 2")
        answerListing(t.core, isRunningRead, [], [lane()], "running")
        answerListing(t.core, isQueuedRead, q, [lane()], "queued")
        wait(100)
        // ...and stays paused.
        verify(header.text.indexOf("Paused by you") >= 0, "the read's older lane didn't undo it: " + header.text)
        // The next read began after that event: its lanes count.
        t.core.emit("jobs", "event", {kind: "job", job: queued("q1000", 0, {updated: iso()})})
        answerLive(t.core, [], q, [lane({paused: true, reasons: ["fullscreen"]})])
        tryVerify(() => header.text.indexOf("Paused while fullscreen") >= 0, 2000, header.text)
    }

    function test_a_pause_answered_during_a_read_stands() { // spec: APP-114, CORE-160
        const t = load({queue: [queued("j2", 0)]})
        const header = t.panel.laneHeader("gpu-heavy")
        t.core.emit("jobs", "event", {kind: "job", job: queued("j2", 0, {updated: iso()})})
        tryVerify(() => openCalls(t.core, isQueueRead).length > 0, 2000, "the read")
        button(header, "Pause").click()
        answer(t.core, c => c.name === "app/queue.pause", okState(lane({paused: true, reasons: ["manual"]})), "pause")
        answerLive(t.core, [], [queued("j2", 0)], [lane()])
        wait(100)
        verify(header.text.indexOf("Paused by you") >= 0, header.text)
    }

    // --- Words that must be true (audit finding 7e). ---

    function test_no_finished_jobs_isnt_said_while_loading() { // spec: APP-114
        const core = createTemporaryObject(fakeComponent, tc)
        const w = createTemporaryObject(panelWindow, tc, {core: core})
        windows.push(w)
        tryVerify(() => w.panel.historyView.headerItem !== null, 2000)
        wait(50)
        const said = () => w.panel.historyView.headerItem.text
        verify(said() !== "No finished jobs yet.", "before the first page: " + said())
        // A first page with no finished job, and more to come: still looking.
        answer(core, isPage, ok({jobs: [queued("j1", 0)], next_cursor: "h1", lanes: [lane()]}), "first page")
        wait(50)
        verify(said() !== "No finished jobs yet.", "before the next page: " + said())
        answer(core, c => isPage(c) && c.args.cursor === "h1", ok({jobs: [], next_cursor: null, lanes: [lane()]}), "the last")
        tryVerify(() => said() === "No finished jobs yet.", 2000, said())
    }

    function test_the_page_limit_says_what_it_looked_through() { // spec: APP-114
        const core = createTemporaryObject(fakeComponent, tc)
        const w = createTemporaryObject(panelWindow, tc, {core: core})
        windows.push(w)
        const said = () => w.panel.historyView.headerItem.text
        const page = n => {
            const jobs = []
            for (let i = 0; i < 100; ++i)
                jobs.push(queued("p" + n + "_" + i, undefined))
            return ok({jobs: jobs, next_cursor: "h" + (n + 1), lanes: [lane()]})
        }
        answer(core, isPage, page(0), "the first page")
        for (let n = 1; n <= 20; ++n)
            answer(core, c => isPage(c) && c.args.cursor === "h" + n, page(n), "page " + n)
        tryVerify(() => said() === "No finished job in the newest 2,100 jobs.", 3000, said())
    }

    function test_a_job_past_its_estimate_says_so() { // spec: APP-114, APP-139
        const late = job("j1", {estimate: wallEstimate(2), started: iso(30000),
                                status_seq: [{status: "running", substatus: "executing", at: iso(30000)}]})
        const t = load({jobs: [late]})
        const text = row(t.panel, "j1").statusText
        verify(text.indexOf("taking longer than estimated") >= 0, text)
        verify(text.indexOf("under 1 s") < 0, text)
    }

    // --- Writes (audit finding 8). ---

    function test_a_double_click_of_pause_is_one_call() { // spec: APP-114, CORE-160
        const t = load({jobs: [job("j1")]})
        const header = t.panel.laneHeader("gpu-heavy")
        const control = button(header, "Pause")
        const isQueueCall = c => c.kind === "appCall"
        control.click()
        verify(!control.enabled, "busy while its call is out")
        compare(callsOf(t.core, isQueueCall).length, 1)
        // The answer comes before the second click of a double-click: that click does nothing.
        answer(t.core, isQueueCall, okState(lane({paused: true, reasons: ["manual"]})), "pause")
        tryVerify(() => header.text.indexOf("Paused by you") >= 0, 2000, header.text)
        verify(control.enabled, "it keeps the focus")
        control.click()
        compare(callsOf(t.core, isQueueCall).length, 1, "the second click of a double-click")
        // A deliberate click after that goes through.
        wait(Qt.styleHints.mouseDoubleClickInterval + 50)
        control.click()
        verify(!control.enabled, "and busy for the resume")
        compare(callsOf(t.core, isQueueCall).length, 2)
        answer(t.core, isQueueCall, {status: "error", error: {code: "x.y", category: "internal", message: "No.",
                                                              trace_id: "t", retry: "never"}}, "resume")
        tryVerify(() => control.enabled, 2000, "free again after a refusal")
        verify(t.panel.banner.visible, "the refusal shows")
    }

    function test_an_event_waiting_for_its_frame_outlives_a_read_that_missed_it() { // spec: APP-114
        const t = load({jobs: [job("j1")]})
        const reads = callsOf(t.core, isRunningRead).length
        t.core.emit("jobs", "event", {kind: "job", job: queued("j2", 0, {updated: iso()})})
        tryVerify(() => callsOf(t.core, isRunningRead).length > reads, 2000, "a read is out")
        // The read lists nothing; this job's event came during it and waits for its frame.
        t.core.emit("jobs", "event", {kind: "job", job: dispatched("j9", "submitting", {updated: iso()})})
        answerLive(t.core, [], [])
        compare(liveOrder(t.panel), ["j9"], "the same turn: the commit took the waiting event first")
    }

    function test_the_held_resume_waits_for_its_lanes_call_too() { // spec: APP-114, CORE-160
        const t = load({lanes: [lane({held: 2, restart_held: 2})]})
        const header = t.panel.laneHeader("gpu-heavy")
        const held = button(header, "Resume held jobs (2)")
        held.click()
        verify(!held.enabled && !button(header, "Pause").enabled, "the lane's controls wait")
        answer(t.core, c => c.name === "app/queue.resume", okState(lane()), "resume held")
        tryVerify(() => button(header, "Pause").enabled, 2000)
    }

    function test_retry_is_busy_while_its_call_is_out() { // spec: APP-114
        const failed = job("j6", {status: "failed", substatus: null, finished: iso(1000),
                                  error: {code: "x", category: "internal", message: "Broke.", trace_id: "t",
                                          retry: "after_resolve"}})
        const t = load({jobs: [failed]})
        const control = button(row(t.panel, "j6"), "Retry")
        control.click()
        verify(!control.enabled, "busy while its call is out")
        compare(callsOf(t.core, c => c.name === "job.retry").length, 1)
        answer(t.core, c => c.name === "job.retry", ok({job: job("j9")}))
        tryVerify(() => control.enabled, 2000, "free again")
    }

    function test_cancel_batch_says_its_count_and_cancel_all_what_happened() { // spec: APP-114, CORE-160
        const batch = {batch_id: "b_9"}
        const members = []
        for (let i = 0; i < 12; ++i)
            members.push(queued("m" + (100 + i), i, Object.assign({index: i}, batch)))
        const t = load({queue: members})
        verify(button(row(t.panel, "m100"), "Cancel batch (12)"), "the count it knows")
        const cancelAll = button(t.panel, "Cancel all from this app")
        cancelAll.click()
        answer(t.core, c => c.name === "job.cancel",
               ok({jobs: [job("a", {status: "cancelled"}), job("b", {status: "cancelled"})]}))
        tryVerify(() => t.panel.banner.text === "Cancelled 2 jobs.", 2000, t.panel.banner.text)
        // A running job only begins to cancel.
        cancelAll.click()
        answer(t.core, c => c.name === "job.cancel",
               ok({jobs: [job("a", {status: "cancelled"}), job("b", {substatus: "cancelling"})]}))
        tryVerify(() => t.panel.banner.text === "Cancelling or cancelled 2 jobs.", 2000, t.panel.banner.text)
    }

    // A failed read says so, and the next good one takes it back.
    function test_a_failed_live_read_says_so_until_a_read_works() { // spec: APP-114
        const t = load({queue: [queued("j2", 0)]})
        verify(!t.panel.staleNote.visible, "nothing to say yet")
        t.core.emit("jobs", "event", {kind: "job", job: queued("j2", 0, {updated: iso()})})
        answer(t.core, isRunningRead, {status: "error", error: {code: "state.unavailable", category: "transient",
                                                                message: "No.", trace_id: "t", retry: "after_backoff"}})
        tryVerify(() => t.panel.staleNote.visible, 2000, "the note")
        compare(t.panel.staleNote.text, "Couldn't read the queue. Trying again…")
        verify(row(t.panel, "j2"), "the rows stay")
        // The other listings of that read count for nothing; the retry comes 2 s later.
        for (const c of openCalls(t.core, c => isQueueRead(c) || isQueuedRead(c)))
            answered[c.id] = true
        tryVerify(() => openCalls(t.core, isRunningRead).length === 1, 4000, "the retry")
        answerLive(t.core, [], [queued("j2", 0)])
        tryVerify(() => !t.panel.staleNote.visible, 2000, "gone")
    }

    // A list emptied under a row still being made off the frame's path: the view cancels that row
    // by an index past the list's end, and Qt warns.
    function test_a_list_emptied_under_a_row_being_made_warns_nothing() { // spec: APP-114, APP-139
        failOnWarning(/DelegateModel::cancel/)
        const q = []
        for (let i = 0; i < 30; ++i)
            q.push(queued("q" + (100 + i), i))
        const t = load({queue: q})
        wait(100)
        const cancelled = i => t.core.emit("jobs", "event", {kind: "job", job: queued("q" + (100 + i), i, {
            status: "cancelled", substatus: null, finished: iso(), updated: iso()})})
        // The first rows go, so the list makes the next ones; the rest go before it can finish.
        for (let i = 0; i < 3; ++i)
            cancelled(i)
        t.panel.applyPendingJobs()
        t.panel.liveView.forceLayout()
        for (let i = 3; i < 30; ++i)
            cancelled(i)
        t.panel.applyPendingJobs()
        t.panel.liveView.forceLayout()
        compare(t.panel.live.count, 0)
        wait(100)
    }

    // --- Untrusted text (audit finding 4; AI-222). ---

    // Every Text under `item`, shown or not, base controls' included.
    function allTexts(item, out) {
        const found = out || []
        if (typeof item.text === "string" && item.textFormat !== undefined)
            found.push(item)
        for (let i = 0; i < item.children.length; ++i)
            allTexts(item.children[i], found)
        return found
    }

    function test_untrusted_text_shows_literally_and_loads_nothing() { // spec: APP-114, AI-222
        const before = TestFiles.networkRequests()
        const img = '<img src="http://127.0.0.1:9/probe.png" width="40" height="40">'
        // Long enough that a path around shownText shows in the length.
        const bad = name => "<b>" + name + "</b>" + img + "x".repeat(2000)
        const err = extra => Object.assign({code: "x.y", category: "internal", message: bad("message"), trace_id: "t",
                                            retry: "after_resolve",
                                            fix: {user_hint: bad("hint"), ui_action: {id: "retry_job", label: bad("fixlabel")}}},
                                           extra || {})
        const failed = job("f1", {status: "failed", substatus: null, finished: iso(1000), kind: bad("kind"),
                                  origin: {surface: bad("surface")}, priority: bad("priority"), lane: bad("lane"),
                                  error: err()})
        const shown = job("f2", {status: "failed", substatus: null, finished: iso(1000),
                                 origin: {surface: "panel", id: bad("app")},
                                 error: err({fix: {ui_action: {id: bad("action"), label: bad("otherlabel")}}})})
        const showJob = job("f3", {status: "failed", substatus: null, finished: iso(1000),
                                   error: err({fix: {ui_action: {id: "show_job", label: bad("showlabel")}}})})
        const running = job("r1", {substatus: bad("runsub"), phase: bad("phase"), lane: "cpu"})
        const unknown = job("r2", {substatus: "interrupted_unknown", error: err({message: bad("unknownmsg")})})
        const held = queued("q1", 0, {substatus: "queue_paused", pause_reasons: [bad("reason")]})
        const waits = queued("q2", 1, {substatus: bad("waitsub")})
        const odd = job("o1", {status: bad("status"), substatus: bad("oddsub"), created: iso(1000)})
        const t = load({jobs: [failed, shown, showJob, odd], running: [running, unknown], queue: [held, waits],
                        lanes: [lane({paused: true, reasons: [bad("lanereason")], held: 1})]})
        // Tall enough to make every row, whatever their text's length.
        t.window.height = 8000
        for (const id of ["f1", "f2", "f3", "r1", "r2", "q1", "q2"])
            row(t.panel, id)
        tryVerify(() => liveOrder(t.panel).indexOf("o1") >= 0, 2000)
        row(t.panel, "o1")
        // A refusal's message in the banner.
        button(row(t.panel, "q1"), "Run next").click()
        answer(t.core, c => c.name === "job.move", {status: "error", error: err({message: bad("refusal")})})
        tryVerify(() => t.panel.banner.visible, 2000)
        wait(500)
        compare(TestFiles.networkRequests(), before, "no <img> was loaded")
        // Every Text is plain, and none shows the whole of what came.
        const all = allTexts(t.panel)
        verify(all.length > 20, "the walk found the texts: " + all.length)
        for (const text of all) {
            compare(text.textFormat, Text.PlainText, String(text) + ": " + text.text.slice(0, 60))
            verify(!/x{401}/.test(text.text), "through shownText: " + text.text.slice(0, 60))
        }
        const shows = marker => all.some(text => text.text.indexOf("<b>" + marker + "</b>") >= 0)
        for (const marker of ["kind", "surface", "priority", "lane", "message", "hint", "fixlabel", "app", "otherlabel",
                              "showlabel", "runsub", "phase", "unknownmsg", "reason", "waitsub", "status", "oddsub",
                              "lanereason", "refusal"])
            verify(shows(marker), marker + " is shown as written")
    }

    // --- A burst of `jobs` events (audit finding 9). ---

    function test_a_burst_of_events_is_one_pass() { // spec: APP-114, APP-139
        const q = []
        for (let i = 0; i < 30; ++i)
            q.push(queued("q" + (100 + i), i))
        const t = load({queue: q})
        wait(100)
        const passes = t.panel.jobPasses
        verify(passes !== undefined, "the panel counts its passes")
        // Twenty of them are cancelled in one burst.
        for (let i = 0; i < 20; ++i)
            t.core.emit("jobs", "event", {kind: "job", job: queued("q" + (100 + i), i, {status: "cancelled", substatus: null,
                                                                                        finished: iso(), updated: iso()})})
        tryCompare(t.panel.live, "count", 10)
        wait(50)
        compare(t.panel.jobPasses - passes, 1, "one pass for the burst")
        compare(liveOrder(t.panel)[0], "q120")
        // And one read after it.
        const open = () => openCalls(t.core, isQueueRead).length
        tryVerify(() => open() === 1, 2000, "a read")
        wait(300)
        compare(open(), 1, "one read for the burst")
    }

    function test_caps_are_read_again_on_caps_usage_and_finished_jobs() { // spec: APP-140
        const t = load({jobs: [job("j1")]})
        const caps = () => callsOf(t.core, c => c.name === "caps.get").length
        compare(caps(), 1)
        t.core.emit("caps", "event", {kind: "changed"})
        t.core.emit("usage", "event", {kind: "recorded"})
        tryVerify(() => caps() === 2, 3000, "read again, once for both")
        answer(t.core, c => c.name === "caps.get", ok({display_currency: "USD", period: "2026-10", caps: []}))
        wait(1200)
        compare(caps(), 2)
        // A running job's progress isn't a reason; its end is.
        t.core.emit("jobs", "event", {kind: "job", job: job("j1", {updated: iso()})})
        wait(1200)
        compare(caps(), 2, "a job still running isn't a reason")
        t.core.emit("jobs", "event", {kind: "job", job: job("j1", {status: "succeeded", substatus: null,
                                                                   finished: iso(), updated: iso()})})
        tryVerify(() => caps() === 3, 3000, "a finished job is")
    }

    function test_the_panel_passes_the_walk() { // spec: APP-156
        const t = load({jobs: [job("j1"), queued("j2", 0), job("j6", {status: "failed", substatus: null,
                                                                      estimate: localEstimate,
                                                                      error: {code: "x", category: "internal",
                                                                              message: "Broke.", trace_id: "t",
                                                                              retry: "never"}})],
                        queue: [queued("j2", 0)],
                        lanes: [lane({paused: true, reasons: ["manual"], restart_held: 2, held: 2})]})
        for (const id of ["j1", "j2", "j6"])
            row(t.panel, id)
        for (const [v, name] of [[t.panel.liveView, "Running and queued"], [t.panel.historyView, "Finished"]]) {
            const view = LilShim.accessibleOf(v)
            compare(view.role, Accessible.List)
            compare(view.name, name)
        }
        const r = LilShim.accessibleOf(row(t.panel, "j1"))
        compare(r.role, Accessible.ListItem)
        verify(r.name.indexOf("ComfyUI prompt") >= 0 && r.name.indexOf("Running") >= 0, r.name)
        // The figures that tick are its description, so its name holds still.
        verify(r.name.indexOf(" s") < 0, "no seconds in the name: " + r.name)
        verify(row(t.panel, "j1").Accessible.description.indexOf(" s") >= 0, row(t.panel, "j1").Accessible.description)
        verify(walk.interactiveCount(t.panel) >= 6, "the walk sees the controls: " + walk.interactiveCount(t.panel))
        compare(walk.report(t.panel), [])
    }

    function test_an_empty_list_says_so() { // spec: APP-114
        const t = load()
        tryVerify(() => t.panel.emptyText && t.panel.emptyText.visible, 2000)
        compare(t.panel.liveView.count, 0)
        compare(t.panel.historyView.count, 0)
        verify(!button(t.panel, "Cancel all from this app"), "nothing of this app's to cancel")
    }

    function test_a_long_queue_shows_its_running_job_and_its_front() { // spec: APP-114, CORE-160
        // 150 queued jobs and a running one older than them all: the history page holds only the
        // newest 100 queued, the queue read has them all in two pages, the CPU lane after them.
        const runningJob = job("r0", {created: iso(900000), estimate: wallEstimate(20)})
        const q = []
        for (let i = 0; i < 150; ++i)
            q.push(queued("q" + (1000 + i), i, {created: iso(800000 - i * 1000), estimate: wallEstimate(10)}))
        const cpu = queued("c0", 0, {lane: "cpu"})
        const t = load({jobs: q.slice(50).reverse(), running: [runningJob], queue: q.concat([cpu])})
        tryVerify(() => t.panel.live.count === 152, 3000, "every live job: " + t.panel.live.count)
        const order = liveOrder(t.panel)
        compare(order.slice(0, 3), ["r0", "q1000", "q1001"], "the running job, then the front of the queue")
        compare(order[151], "c0", "the CPU lane's job comes after the GPU lane's")
        t.panel.liveView.positionViewAtBeginning()
        verify(row(t.panel, "q1000").statusText.indexOf("#1 in queue") >= 0, row(t.panel, "q1000").statusText)
        verify(row(t.panel, "r0").statusText.indexOf("left") >= 0, row(t.panel, "r0").statusText)
        compare(t.panel.historyView.count, 0, "the queued jobs aren't history")
    }

    function test_running_jobs_come_first_in_every_lane() { // spec: APP-114
        const done = []
        for (let i = 0; i < 20; ++i)
            done.push(job("d" + i, {status: "succeeded", substatus: null, finished: iso(i * 1000)}))
        const install = job("cpu1", {kind: "comfy.install_node", lane: "cpu"})
        const t = load({jobs: done.concat([install]), running: [install], queue: [queued("g0", 0)]})
        compare(liveOrder(t.panel), ["cpu1", "g0"], "the running CPU job before the queued GPU one")
        t.panel.liveView.positionViewAtBeginning()
        const r = row(t.panel, "cpu1")
        verify(r.ListView.view === t.panel.liveView, "in the live list, not under the history")
        verify(r.titleText.indexOf("CPU work") >= 0, r.titleText)
    }

    function test_substatus_status_lane_and_class_show_by_name() { // spec: APP-114, CORE-117
        const retrying = job("j1", {substatus: "retrying_1_of_3"})
        const cancelling = job("j2", {substatus: "cancelling", lane: "cpu"})
        const unknown = job("j3", {substatus: "interrupted_unknown", lane: "network", priority: "agent",
                                   estimate: wallEstimate(10), started: iso(7200000),
                                   status_seq: [{status: "running", substatus: "executing", at: iso(7200000)}],
                                   error: {code: "job.interrupted_unknown", category: "unknown",
                                           message: "Whether it ran is unknown.", trace_id: "t", retry: "never"}})
        const odd = job("j4", {status: "paused_by_director", substatus: "planning", priority: "bulk",
                               created: iso(1000)})
        const t = load({jobs: [odd, unknown, cancelling, retrying], running: [retrying, cancelling, unknown]})
        verify(row(t.panel, "j1").statusText.indexOf("Retrying (1 of 3)") >= 0, row(t.panel, "j1").statusText)
        verify(row(t.panel, "j2").statusText.indexOf("Cancelling") >= 0, row(t.panel, "j2").statusText)
        const u = row(t.panel, "j3")
        verify(u.statusText.indexOf("Outcome unknown") >= 0 && u.statusText.indexOf("Whether it ran is unknown.") >= 0,
               u.statusText)
        compare(u.timeText, "", "no ETA and no clock")
        verify(u.titleText.indexOf("network") >= 0 && u.titleText.indexOf("agent batch") >= 0, u.titleText)
        verify(t.panel.laneHeader("network") && t.panel.laneHeader("network").visible, "a header of its own")
        // A status this build doesn't know shows with the live jobs, by name, and can be cancelled.
        tryVerify(() => liveOrder(t.panel).indexOf("j4") >= 0, 2000, JSON.stringify(liveOrder(t.panel)))
        compare(liveOrder(t.panel).slice(0, 3).sort(), ["j1", "j2", "j3"], "the running ones first")
        // Under the lane headers, it may be below the view: its row is made when it is scrolled to.
        t.panel.liveView.positionViewAtIndex(liveOrder(t.panel).indexOf("j4"), ListView.Contain)
        const o = row(t.panel, "j4")
        verify(o.statusText.indexOf("paused_by_director") >= 0 && o.statusText.indexOf("planning") >= 0, o.statusText)
        verify(o.titleText.indexOf("bulk") >= 0, o.titleText)
        verify(button(o, "Cancel"), "it can be cancelled")
    }

    function test_progress_never_steps_back_for_an_older_summary() { // spec: APP-114, APP-139
        const t = load({jobs: [job("j1", {progress: {value: 2, max: 30}})]})
        const r = row(t.panel, "j1")
        t.core.emit("job:j1", "event", {kind: "progress", value: 12, max: 30})
        tryCompare(r, "progressValue", 12)
        // A jobs summary made before that progress event.
        t.core.emit("jobs", "event", {kind: "job", job: job("j1", {progress: {value: 9, max: 30}, updated: iso()})})
        wait(50)
        compare(r.progressValue, 12)
        t.core.emit("jobs", "event", {kind: "job", job: job("j1", {progress: {value: 20, max: 30}, updated: iso()})})
        tryCompare(r, "progressValue", 20)
    }

    function test_done_counts_from_the_first_run() { // spec: APP-114
        const retried = job("j1", {status: "succeeded", substatus: null, started: iso(10000), finished: iso(0),
                                   status_seq: [{status: "running", substatus: "executing", at: iso(70000)},
                                                {status: "running", substatus: "retrying_1_of_5", at: iso(20000)},
                                                {status: "succeeded", substatus: null, at: iso(0)}]})
        const t = load({jobs: [retried]})
        const text = row(t.panel, "j1").statusText
        verify(text.indexOf("in 1 min 10 s") >= 0, text)
    }

    function test_cancel_all_shows_only_with_this_apps_jobs() { // spec: APP-114, CORE-160
        const t = load({jobs: [job("j1", {origin: {surface: "mcp"}}),
                               job("j2", {status: "succeeded", substatus: null})],
                        running: [job("j1", {origin: {surface: "mcp"}})]})
        verify(!button(t.panel, "Cancel all from this app"), "neither job is a live one of this app's")
        t.core.emit("jobs", "event", {kind: "job", job: queued("j3", 0, {updated: iso()})})
        tryVerify(() => button(t.panel, "Cancel all from this app"), 2000, "this app's queued job")
    }

    function test_finished_jobs_past_a_first_page_of_queued_ones() { // spec: APP-114
        // A batch of 100 queued jobs is newer than every finished job: the first history page
        // holds none of them, and the panel asks for the next.
        const batch = []
        for (let i = 0; i < 100; ++i)
            batch.push(queued("b" + (100 + i), i, {batch_id: "b_big", index: i, created: iso(1000 + i)}))
        const core = createTemporaryObject(fakeComponent, tc)
        const w = createTemporaryObject(panelWindow, tc, {core: core})
        windows.push(w)
        answer(core, isPage, ok({jobs: batch, next_cursor: "h1", lanes: [lane()]}), "the first page")
        answerLive(core, [], batch)
        answer(core, c => c.name === "engines.status", ok({engines: [comfy(0, 0)]}))
        answer(core, c => c.name === "caps.get", ok({display_currency: "USD", period: "2026-10", caps: []}))
        const failed = job("f1", {status: "failed", substatus: null, created: iso(500000), finished: iso(400000),
                                  estimate: localEstimate,
                                  error: {code: "x", category: "internal", message: "Broke.", trace_id: "t",
                                          retry: "after_resolve"}})
        const next = answer(core, c => isPage(c) && c.args.cursor === "h1",
                            ok({jobs: [failed, job("d1", {status: "succeeded", substatus: null,
                                                          finished: iso(450000)})],
                                next_cursor: null, lanes: [lane()]}), "the next page")
        compare(next.args, {limit: 100, cursor: "h1"})
        tryCompare(w.panel.historyView, "count", 2)
        verify(button(row(w.panel, "f1"), "Retry"), "the failed job's Retry is reachable")
        wait(300)
        verify(!core.calls.some(c => isPage(c) && c.args.cursor !== undefined && c.args.cursor !== "h1"),
               "no page past the last")
    }

    function test_an_unknown_status_stays_live_through_reads() { // spec: APP-114
        const odd = job("odd", {status: "paused_by_director", substatus: "waiting_quota", created: iso(1000)})
        const t = load({jobs: [odd], queue: [queued("q0", 0)]})
        tryVerify(() => liveOrder(t.panel).indexOf("odd") >= 0, 2000, JSON.stringify(liveOrder(t.panel)))
        // Three reads, each from a new job's event: the job no read can list stays.
        for (let i = 1; i <= 3; ++i) {
            t.core.emit("jobs", "event", {kind: "job", job: queued("q" + i, i, {updated: iso()})})
            const qs = []
            for (let k = 0; k <= i; ++k)
                qs.push(queued("q" + k, k))
            answerLive(t.core, [], qs)
            tryVerify(() => liveOrder(t.panel).length === i + 2, 2000, JSON.stringify(liveOrder(t.panel)))
            verify(liveOrder(t.panel).indexOf("odd") >= 0, "read " + i + ": " + JSON.stringify(liveOrder(t.panel)))
        }
        compare(t.panel.historyView.count, 0, "and it isn't history")
        // An event that ends it does.
        t.core.emit("jobs", "event", {kind: "job", job: job("odd", {status: "cancelled", substatus: null,
                                                                    finished: iso(), updated: iso()})})
        tryVerify(() => liveOrder(t.panel).indexOf("odd") < 0, 2000)
        tryCompare(t.panel.historyView, "count", 1)
    }

    function test_a_resync_drops_an_unknown_status_the_core_no_longer_lists() { // spec: APP-114
        const odd = job("odd", {status: "paused_by_director", substatus: "waiting_quota", created: iso(1000)})
        const t = load({jobs: [odd], queue: [queued("q0", 0)]})
        tryVerify(() => liveOrder(t.panel).indexOf("odd") >= 0, 2000, JSON.stringify(liveOrder(t.panel)))
        // A core restart or a workspace switch: the reload no longer lists the job.
        t.core.emit("jobs", "resync_required", null)
        answer(t.core, isPage, ok({jobs: [], next_cursor: null, lanes: [lane()]}), "the reload")
        answerLive(t.core, [], [queued("q0", 0)])
        tryVerify(() => liveOrder(t.panel).indexOf("odd") < 0, 2000, "gone: " + JSON.stringify(liveOrder(t.panel)))
        compare(liveOrder(t.panel), ["q0"])
        // One the reload still lists comes back.
        t.core.emit("jobs", "resync_required", null)
        answer(t.core, isPage, ok({jobs: [odd], next_cursor: null, lanes: [lane()]}), "the second reload")
        answerLive(t.core, [], [queued("q0", 0)])
        tryVerify(() => liveOrder(t.panel).indexOf("odd") >= 0, 2000, "back: " + JSON.stringify(liveOrder(t.panel)))
    }

    function test_a_read_cut_at_its_page_cap_says_so() { // spec: APP-114
        const core = createTemporaryObject(fakeComponent, tc)
        const w = createTemporaryObject(panelWindow, tc, {core: core})
        windows.push(w)
        answer(core, isPage, ok({jobs: [], next_cursor: null, lanes: [lane()]}))
        answer(core, isRunningRead, ok({jobs: [], next_cursor: null, lanes: [lane()]}))
        answer(core, isQueuedRead, ok({jobs: [], next_cursor: null, lanes: [lane()]}))
        // A cursor that never ends: one job a page, always more.
        for (let i = 0; i < 100; ++i)
            answer(core, isQueueRead, ok({jobs: [queued("q" + (1000 + i), i)], next_cursor: "c" + (i + 1),
                                          lanes: [lane()]}), "page " + i)
        answer(core, c => c.name === "engines.status", ok({engines: [comfy(0, 0)]}))
        answer(core, c => c.name === "caps.get", ok({display_currency: "USD", period: "2026-10", caps: []}))
        tryCompare(w.panel.live, "count", 100)
        wait(200)
        compare(openCalls(core, isQueueRead).length, 0, "it stops at the cap")
        verify(w.panel.cappedNote.visible, "and says so")
        compare(w.panel.cappedNote.text, "Showing the first 100 queued jobs")
    }

    function test_float_and_redock_keeps_jobs_state() { // spec: ARCH-116
        const core = createTemporaryObject(fakeComponent, tc)
        const shell = createTemporaryObject(shellComponent, tc, {core: core})
        shell.visible = true
        tryVerify(() => shell.docks.jobs.content && shell.docks.jobs.content.width > 0, 5000)
        const panel = shell.docks.jobs.content
        answer(core, isPage, ok({jobs: [job("j1"), queued("j2", 0)], next_cursor: null, lanes: [lane()]}))
        answerLive(core, [job("j1")], [queued("j2", 0)], [lane({paused: true, reasons: ["manual"]})])
        answer(core, c => c.name === "engines.status", ok({engines: [comfy(0, 0)]}))
        answer(core, c => c.name === "caps.get", ok({display_currency: "USD", period: "2026-10", caps: []}))
        tryVerify(() => panel.model.loaded && panel.liveView.count === 2)
        panel.liveView.currentIndex = 1
        const pages = core.calls.length
        // The model's and the panel's, plus any other docked panel's (Usage & spend follows jobs).
        const subscribers = core.subscriberCount("jobs")
        verify(subscribers >= 2, "the model and the panel follow jobs: " + subscribers)
        const dock = shell.docks.jobs
        dock.isFloating = true
        tryVerify(() => dock.isFloating && panel.Window.window && panel.Window.window !== shell, 5000,
                  "Jobs floats in its own window")
        verify(dock.content === panel, "the same panel item")
        compare(panel.liveView.count, 2)
        compare(panel.liveView.currentIndex, 1)
        verify(panel.laneHeader("gpu-heavy").text.indexOf("Paused by you") >= 0)
        dock.isFloating = false
        tryVerify(() => panel.Window.window === shell, 5000, "Jobs docks back")
        verify(dock.content === panel)
        compare(panel.liveView.count, 2)
        compare(panel.liveView.currentIndex, 1)
        compare(core.calls.length, pages, "nothing was loaded again")
        compare(core.subscriberCount("jobs"), subscribers, "the model's and the panel's subscriptions stay")
    }
}
