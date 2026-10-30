// The Jobs panel (APP-114, APP-139, CORE-160, CORE-161; P4a-R40, P4a-R41): rows that update in
// place at most 10 times a second with their live preview, lane headers with the pause reasons,
// waiting for GPU memory and ComfyUI's outside jobs, run next or last, the batch and all-mine
// cancels, Pause GPU work, the restart hold's own resume, the queue order and the queued ETA,
// the retry's cost chip, the raw names of what the app doesn't know, and floating it in the
// shell. Every test runs on FakeCore.
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

    function isPage(c) { return c.name === "jobs.list" && c.args.order === undefined }
    function isQueueRead(c) { return c.name === "jobs.list" && c.args.order === "queue" }

    // The panel, loaded: the jobs page, the queue order, the engines and the caps.
    function load(setup) {
        const s = setup || {}
        const core = createTemporaryObject(fakeComponent, tc)
        const w = createTemporaryObject(panelWindow, tc, {core: core})
        windows.push(w)
        answer(core, isPage, ok({jobs: s.jobs || [], next_cursor: null, lanes: s.lanes || [lane()]}), "page")
        answer(core, isQueueRead, ok({jobs: s.queue || [], next_cursor: null,
                                      lanes: s.lanes || [lane()]}), "queue order")
        answer(core, c => c.name === "engines.status", ok({engines: s.engines || [comfy(0, 0)]}), "engines")
        answer(core, c => c.name === "caps.get", ok(s.caps || {display_currency: "USD", period: "2026-10",
                                                              caps: []}), "caps")
        tryVerify(() => w.panel.model.loaded)
        return {window: w, panel: w.panel, core: core}
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

    function row(panel, id) {
        let r = null
        tryVerify(() => (r = panel.rowFor(id)) !== null, 3000, "the row of " + id)
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
        // The queue order is read again after the move.
        answer(t.core, isQueueRead, ok({jobs: [queued("j3", 0), queued("j2", 1)], next_cursor: null,
                                        lanes: [lane()]}), "the queue again")
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
        button(t.panel, "Cancel all mine").click()
        compare(answer(t.core, c => c.name === "job.cancel", ok({jobs: []})).args, {all_mine: true})
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
        const b = row(t.panel, "j3").statusText
        verify(b.indexOf("Held after a restart") >= 0, b)
        verify(b.indexOf("held") >= 0, b)
        verify(b.indexOf("#") < 0 && b.indexOf(" 0") < 0, "a restart-held job has no place: " + b)
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
        verify(t.panel.view.currentItem === p, "the job is selected")
        // An action the app doesn't know: its label, and nothing else.
        const u = row(t.panel, "j8")
        verify(!button(u, "Open the wizard"), "not a button")
        verify(u.statusText.indexOf("Open the wizard") >= 0, u.statusText)
    }

    function test_a_retry_waiting_on_an_earlier_prompt_shows_it_with_cancel() { // spec: APP-114, CORE-160
        const waiting = queued("j10", 0, {substatus: "submitting", retry_of: "j0",
                                          status_seq: [{status: "queued", substatus: "submitting", at: iso(6000)}]})
        const t = load({jobs: [waiting], queue: [waiting]})
        const r = row(t.panel, "j10")
        tryVerify(() => r.statusText.indexOf("Waiting for an earlier ComfyUI prompt to finish") >= 0, 3000, r.statusText)
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
            t.panel.view.currentIndex = i
            return t.panel.view.currentItem ? t.panel.view.currentItem.jobId : ""
        })
        tryVerify(() => JSON.stringify(order()) === JSON.stringify(["j1", "j3", "j2", "j4"]), 3000,
                  "running first, then queue order: " + order())
        // j2: 15 s left of j1, 30 for j3, 60 its own.
        tryVerify(() => row(t.panel, "j2").statusText.indexOf("about 2 min") >= 0, 3000, row(t.panel, "j2").statusText)
        // j4: one place the panel can't see (position 3, two listed ahead) and no estimate of its own.
        const d = row(t.panel, "j4").statusText
        verify(d.indexOf("after 2 jobs with no estimate") >= 0, d)
        // A burst of job events re-reads the queue order once.
        const reads = callsOf(t.core, isQueueRead).length
        for (let i = 0; i < 5; ++i)
            t.core.emit("jobs", "event", {kind: "job", job: queued("j2", 1, {estimate: wallEstimate(60),
                                                                             updated: iso()})})
        tryVerify(() => callsOf(t.core, isQueueRead).length === reads + 1, 2000)
        wait(400)
        compare(callsOf(t.core, isQueueRead).length, reads + 1, "one read for the burst")
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
        row(t.panel, "j6")
        const view = LilShim.accessibleOf(t.panel.view)
        compare(view.role, Accessible.List)
        compare(view.name, "Jobs")
        const r = LilShim.accessibleOf(row(t.panel, "j1"))
        compare(r.role, Accessible.ListItem)
        verify(r.name.indexOf("ComfyUI prompt") >= 0 && r.name.indexOf("Running") >= 0, r.name)
        verify(walk.interactiveCount(t.panel) >= 6, "the walk sees the controls: " + walk.interactiveCount(t.panel))
        compare(walk.report(t.panel), [])
    }

    function test_an_empty_list_says_so() { // spec: APP-114
        const t = load()
        tryVerify(() => t.panel.emptyText.visible, 2000)
        compare(t.panel.view.count, 0)
    }

    function test_float_and_redock_keeps_jobs_state() { // spec: ARCH-116
        const core = createTemporaryObject(fakeComponent, tc)
        const shell = createTemporaryObject(shellComponent, tc, {core: core})
        shell.visible = true
        tryVerify(() => shell.docks.jobs.content && shell.docks.jobs.content.width > 0, 5000)
        const panel = shell.docks.jobs.content
        answer(core, isPage, ok({jobs: [job("j1"), queued("j2", 0)], next_cursor: null, lanes: [lane()]}))
        answer(core, isQueueRead, ok({jobs: [queued("j2", 0)], next_cursor: null,
                                      lanes: [lane({paused: true, reasons: ["manual"]})]}))
        answer(core, c => c.name === "engines.status", ok({engines: [comfy(0, 0)]}))
        answer(core, c => c.name === "caps.get", ok({display_currency: "USD", period: "2026-10", caps: []}))
        tryVerify(() => panel.model.loaded && panel.view.count === 2)
        panel.view.currentIndex = 1
        const pages = core.calls.length
        const dock = shell.docks.jobs
        dock.isFloating = true
        tryVerify(() => dock.isFloating && panel.Window.window && panel.Window.window !== shell, 5000,
                  "Jobs floats in its own window")
        verify(dock.content === panel, "the same panel item")
        compare(panel.view.count, 2)
        compare(panel.view.currentIndex, 1)
        verify(panel.laneHeader("gpu-heavy").text.indexOf("Paused by you") >= 0)
        dock.isFloating = false
        tryVerify(() => panel.Window.window === shell, 5000, "Jobs docks back")
        verify(dock.content === panel)
        compare(panel.view.count, 2)
        compare(panel.view.currentIndex, 1)
        compare(core.calls.length, pages, "nothing was loaded again")
        compare(core.subscriberCount("jobs"), 2, "the model's and the panel's subscriptions stay")
    }
}
