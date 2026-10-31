// grab_jobs: the shell's Default layout with the Jobs panel running, paused and empty, offscreen,
// saved as <grab dir>/jobs-<state>.png (LIL_GRAB_DIR, else CMake's build/<preset>/grabs) for the
// reviewer, on FakeCore's scripted answers. Each grab must have the window's size and not be one
// flat colour.
import QtQuick
import QtTest
import Lilith.UiKit
import "CoreShapes.js" as Shapes

TestCase {
    id: tc
    name: "GrabJobs"
    when: windowShown
    width: 200
    height: 100

    Component {
        id: fakeComponent
        FakeCore {
            project: "sketchbook"
            workspace: ({id: "ws1", path: "D:/art/harbor"})
        }
    }

    Component {
        id: shellComponent
        ShellWindow {}
    }

    function iso(msAgo) {
        return new Date(Date.now() - (msAgo || 0)).toISOString().replace(/\.\d+Z$/, "Z")
    }

    function estimate(wall) {
        return {local: {source: "measured_history", samples: 6, gpu_s_p50: (wall * 0.8).toFixed(1),
                        gpu_s_p90: wall.toFixed(1), wh_p50: (wall / 20).toFixed(3), wh_p90: (wall / 15).toFixed(3)},
                wall_s: {source: "measured_history", samples: 6, p50: wall.toFixed(1), p90: (wall * 1.4).toFixed(1)},
                unknown: []}
    }

    function job(id, over) {
        return Object.assign({job_id: id, kind: "comfy.prompt", lane: "gpu-heavy", priority: "user",
                              status: "queued", substatus: "waiting", created: iso(60000), updated: iso(),
                              attempt: 1, trace_id: "t_" + id, batch_id: "b_" + id, index: 0,
                              origin: {surface: "panel", id: "image"}, status_seq: []}, over || {})
    }

    function lane(over) {
        return Object.assign({lane: "gpu-heavy", paused: false, reasons: [], held: 0, restart_held: 0,
                              withdrawn: 0}, over || {})
    }

    function ok(structured) {
        return {status: "ok", structured: structured, artifacts: [], summary_text: ""}
    }

    property var answered: ({})

    // Every call still open that the panel and the top bar make, answered as the core does.
    function answerAll(core, jobs, lanes, outside) {
        for (const c of core.calls.filter(c => !answered[c.id])) {
            let outcome = null
            if (c.name === "jobs.list")
                outcome = core.jobsListAnswer(c.args, jobs, lanes)
            else if (c.name === "engines.status")
                outcome = ok({engines: [{engine: "comfyui", state: "ready", outside_jobs: outside || {running: 0, queued: 0}}]})
            else if (c.name === "caps.get")
                outcome = Shapes.tool(Shapes.capsView([]))
            else if (c.name === "app/set_context")
                outcome = Shapes.context(core.workspace, null, "sketchbook")
            if (outcome) {
                answered[c.id] = true
                core.respond(c.id, outcome)
            }
        }
    }

    function distinctColours(img) {
        const seen = {}
        let n = 0
        for (let gy = 0; gy < 32; ++gy) {
            for (let gx = 0; gx < 32; ++gx) {
                const key = String(img.pixel(Math.floor((gx + 0.5) * img.width / 32),
                                             Math.floor((gy + 0.5) * img.height / 32)))
                if (!seen[key]) {
                    seen[key] = true
                    ++n
                }
            }
        }
        return n
    }

    function grab(state, jobs, lanes, outside) {
        answered = {}
        const core = createTemporaryObject(fakeComponent, tc)
        const shell = createTemporaryObject(shellComponent, tc, {core: core})
        shell.visible = true
        tryVerify(() => shell.docks.jobs.content && shell.docks.jobs.content.width > 0, 5000)
        const panel = shell.docks.jobs.content
        tryVerify(() => {
            answerAll(core, jobs, lanes, outside)
            return panel.model.loaded && panel.live.loaded && shell.topBar.jobs.current
                   && panel.liveView.count + panel.historyView.count === jobs.length
        }, 5000)
        core.emit("telemetry", "event", Shapes.sample(Shapes.gpu()))
        wait(500)
        compare(shell.width, 1280)
        const img = grabImage(shell.contentItem)
        const dpr = shell.devicePixelRatio
        compare(img.width, Math.round(shell.width * dpr))
        compare(img.height, Math.round(shell.height * dpr))
        verify(distinctColours(img) >= 4, "not one flat colour")
        const dir = TestFiles.env("LIL_GRAB_DIR") || TestFiles.env("LIL_GRAB_DEFAULT_DIR")
        verify(dir !== "", "a grab folder")
        verify(TestFiles.mkpath(dir))
        img.save(dir + "/jobs-" + state + ".png")
        verify(TestFiles.list(dir).indexOf("jobs-" + state + ".png") >= 0)
        shell.destroy()
        wait(100)
    }

    function test_grab_jobs_running() { // spec: APP-114, APP-140
        const running = job("j_1", {status: "running", substatus: "executing", phase: "sampling",
                                    started: iso(12000), progress: {value: 14, max: 30},
                                    subtasks: [{name: "loading_models"}, {name: "sampling", value: 14, max: 30}],
                                    status_seq: [{status: "running", substatus: "executing", at: iso(12000)}],
                                    estimate: estimate(28), batch_id: "b_a"})
        const next = job("j_2", {queue_position: 0, estimate: estimate(28), batch_id: "b_a", index: 1})
        const agent = job("j_3", {queue_position: 1, priority: "agent", origin: {surface: "mcp"}})
        const failed = job("j_4", {status: "failed", substatus: null, started: iso(50000), finished: iso(40000),
                                   estimate: estimate(20),
                                   error: {code: "comfyui.out_of_memory", category: "resource",
                                           message: "ComfyUI ran out of GPU memory, also on 2 retries.",
                                           trace_id: "t", retry: "after_resolve",
                                           fix: {ui_action: {id: "retry_job", label: "Retry the job"}}}})
        const done = job("j_5", {status: "succeeded", substatus: null, started: iso(90000), finished: iso(70000)})
        grab("running", [running, next, agent, failed, done], [lane()], {running: 0, queued: 1})
    }

    function test_grab_jobs_paused() { // spec: APP-114, CORE-160
        const paused = job("j_6", {substatus: "queue_paused", pause_reasons: ["manual"], queue_position: 0,
                                   estimate: estimate(28)})
        const held = job("j_7", {substatus: "queue_paused", pause_reasons: ["restart", "manual"],
                                 origin: {surface: "cli"}})
        const held2 = job("j_8", {substatus: "queue_paused", pause_reasons: ["restart", "manual"],
                                  origin: {surface: "cli"}})
        grab("paused", [paused, held, held2], [lane({paused: true, reasons: ["manual"], held: 3, restart_held: 2})])
    }

    function test_grab_jobs_empty() { // spec: APP-114
        grab("empty", [], [lane()])
    }
}
