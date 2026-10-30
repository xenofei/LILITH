// The Jobs panel (APP-114, APP-139, CORE-160, CORE-161; P4a-R40, P4a-R41): rows that update in
// place at most 10 times a second with their live preview, lane headers with the pause reasons,
// waiting for GPU memory and ComfyUI's outside jobs, run next or last, the batch and all-mine
// cancels, Pause GPU work, the restart hold's own resume, the queue order and the queued ETA,
// the retry's cost chip, the raw names of what the app doesn't know, and floating it in the
// shell; and the live list's source: the running read and every queue page, never a summary's
// stale place. Every test runs on FakeCore.
import QtQuick
import QtTest
import Lilith.Shim
import Lilith.UiKit

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

    // Answers a live read: the running jobs, then the queue in pages of `pageSize`.
    function answerLive(core, running, queue, lanes, pageSize) {
        answer(core, isRunningRead, ok({jobs: running, next_cursor: null, lanes: lanes || [lane()]}), "running")
        const size = pageSize || 100
        for (let at = 0; at === 0 || at < queue.length; at += size) {
            const more = at + size < queue.length
            const call = answer(core, isQueueRead, ok({jobs: queue.slice(at, at + size),
                                                       next_cursor: more ? "c" + (at + size) : null,
                                                       lanes: lanes || [lane()]}), "queue page at " + at)
            compare(call.args.cursor, at === 0 ? undefined : "c" + at, "the cursor of the page before")
        }
    }

    // The panel, loaded: the history page, the live read, the engines and the caps. The running
    // read lists the running jobs of `jobs` unless `running` says otherwise.
    function load(setup) {
        const s = setup || {}
        const core = createTemporaryObject(fakeComponent, tc)
        const w = createTemporaryObject(panelWindow, tc, {core: core})
        windows.push(w)
        answer(core, isPage, ok({jobs: s.jobs || [], next_cursor: null, lanes: s.lanes || [lane()]}), "page")
        answerLive(core, s.running || (s.jobs || []).filter(j => j.status === "running"), s.queue || [],
                   s.lanes, s.pageSize)
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
        button(row(t.panel, "j2"), "Cancel batch").click()
        compare(answer(t.core, c => c.name === "job.cancel", ok({jobs: []})).args, {batch_id: "b_9"})
        button(row(t.panel, "j4"), "Cancel").click()
        compare(answer(t.core, c => c.name === "job.cancel", ok({job: queued("j4", 1)})).args, {job_id: "j4"})
        button(t.panel, "Cancel all from this app").click()
        compare(answer(t.core, c => c.name === "job.cancel", ok({jobs: [queued("j2", 0), queued("j4", 1)]})).args,
                {all_mine: true})
        tryVerify(() => t.panel.banner.visible && t.panel.banner.text === "Cancelled 2 jobs.", 2000,
                  t.panel.banner.text)
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
        const waiting = queued("j10", 0, {substatus: "submitting", retry_of: "j0", waiting_for_earlier_prompt: true,
                                          status_seq: [{status: "queued", substatus: "submitting", at: iso(6000)}]})
        // A retry slow to submit, and a second attempt, with no inherited prompt: just submitting.
        const slow = queued("j11", 1, {substatus: "submitting", retry_of: "j0",
                                       status_seq: [{status: "queued", substatus: "submitting", at: iso(2500)}]})
        const again = queued("j12", 2, {substatus: "submitting", attempt: 2,
                                        status_seq: [{status: "queued", substatus: "submitting", at: iso(3000)}]})
        const t = load({jobs: [waiting, slow, again], queue: [waiting, slow, again]})
        const r = row(t.panel, "j10")
        verify(r.statusText.indexOf("Waiting for an earlier ComfyUI prompt to finish") >= 0, r.statusText)
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
        t.core.emit("jobs", "event", {kind: "job", job: queued("j2", 1, {estimate: wallEstimate(60), phase: "x",
                                                                         updated: iso()})})
        wait(400)
        compare(callsOf(t.core, isQueueRead).length, reads + 1, "the same status needs no read")
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
        const t = load({jobs: q.slice(50).reverse(), running: [runningJob], queue: q.concat([cpu]), pageSize: 100})
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
        compare(core.subscriberCount("jobs"), 2, "the model's and the panel's subscriptions stay")
    }
}
