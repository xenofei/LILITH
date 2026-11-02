// bench_gui_stalls (label bench; APP-135, APP-139): the whole shell under the fake feed, as spike 10's
// S0.6 measured its own: Jobs with 8 running jobs, each with a 512 x 512 JPEG preview and a progress
// event at 10 Hz, Hardware live with a telemetry sample a second, Home in front, and Diagnostics and
// Settings loaded behind their tabs (a docked panel's Loader never unloads). For MeasureS seconds it
// measures the GUI thread's 4 ms heartbeat lateness and the shell window's frame interval
// (BenchProbe, the runner's: a precise timer and `frameSwapped`, as the spike's shim measured them).
//
// The events reach the shell as Core's do: through BenchPump, the runner's stand-in for the session's
// event pump, which runs the bridge's own event step (a preview's base64 and JPEG decode, inline) on a
// thread of its own and then signals each event on the GUI thread, in order. So the GUI thread does
// what it does in the app, and the decode's own cost is printed beside it (the Task 17 and 23 carry).
//
// Pass: heartbeat lateness p99 under LIL_BUDGET_GUI_STALL_P99_MS (test-params.toml's
// gui_stall_p99_ms), and the frame interval's p99 under the same budget over the platform's own pace.
// The offscreen platform paces its frames with a timer, not a display (its idle frame interval has a
// p99 near 25 ms here with nothing to do), so the frame check is the spike's rule made relative: the
// feed may add less than the budget to the idle p99 (bound: max(budget, idle p99 + budget / 2)). The
// idle figures are measured first, with the same small animation running.
//
// Set LIL_BENCH_MEASURE_S to measure for longer (the run under ComfyUI's load, Task 29 Step 4).
import QtQuick
import QtTest
import Lilith.Core
import Lilith.UiKit
import "../tests"
import "../tests/CoreShapes.js" as Shapes
import "../tests/ShellScene.js" as ShellScene

TestCase {
    id: tc
    name: "BenchGuiStalls"
    when: windowShown
    width: 200
    height: 100

    readonly property real budgetMs: Number(TestFiles.env("LIL_BUDGET_GUI_STALL_P99_MS"))
    readonly property int measureS: Number(TestFiles.env("LIL_BENCH_MEASURE_S")) || 30
    readonly property int jobCount: 8

    Component {
        id: fakeComponent
        FakeCore {
            project: "sketchbook"
            workspace: ({id: "ws1", path: "D:/art/harbor"})
        }
    }

    Component {
        id: fakeSetupComponent
        QtObject {
            property bool busy: false
            property bool appMissing: false
            property string appPath
            signal finished(string reportJson)
            function run() { return false }
        }
    }

    Component {
        id: shellComponent
        ShellWindow {
            shim: QtObject {
                function activate(window, takeFocus) {}
            }
        }
    }

    // A 512 x 512 frame of 64 x 64 coloured cells, grabbed and saved as a JPEG (a noisy image, so
    // the file is as big as a real preview's, not a flat one's).
    Component {
        id: frameWindow
        Window {
            property alias grid: grid
            width: 512
            height: 512
            visible: true
            Grid {
                id: grid
                columns: 64
                property int seed: 0
                Repeater {
                    model: 64 * 64
                    Rectangle {
                        required property int index
                        // A hash of the cell and the seed: neighbouring cells differ, as noise does.
                        readonly property int h: ((index + 1) * 2654435761 + grid.seed * 40503) >>> 0
                        width: 8
                        height: 8
                        color: Qt.rgba((h & 255) / 255, ((h >>> 8) & 255) / 255, ((h >>> 16) & 255) / 255, 1)
                    }
                }
            }
        }
    }

    // Keeps frames coming at the platform's own pace, as the spike's scrolled list did.
    Component {
        id: spinner
        Rectangle {
            width: 2
            height: 2
            color: Theme.surface
            NumberAnimation on opacity { from: 0.2; to: 1; duration: 500; loops: Animation.Infinite }
        }
    }

    property var answered: ({})

    function framesB64() {
        const dir = TestFiles.newDir("frames")
        const w = createTemporaryObject(frameWindow, tc)
        const out = []
        for (let i = 0; i < 2; ++i) {
            w.grid.seed = i + 1
            waitForRendering(w.contentItem)
            const img = grabImage(w.contentItem)
            const path = dir + "/frame" + i + ".jpg"
            img.save(path)
            out.push(TestFiles.readBase64(path))
            verify(out[i].length > 30000, "a real frame: " + out[i].length + " base64 characters")
        }
        w.destroy()
        return out
    }

    function job(n) {
        return Shapes.job("j_00000000000000" + (10 + n), "running", {progress: {value: 0, max: 1000},
                                                                    started: new Date(Date.now() - 5000).toISOString()})
    }

    function test_the_shell_under_the_fake_feed_holds_the_gui_thread_to_a_frame() { // spec: APP-135, APP-139
        verify(budgetMs > 0, "LIL_BUDGET_GUI_STALL_P99_MS is set (CMake passes test-params.toml's budgets)")
        const frames = framesB64()
        // Each frame's event text made once: in the app the bytes never reach JavaScript, so the
        // bench mustn't make 4 MB a second of garbage strings for the GUI thread to collect.
        const previewJson = frames.map(f => JSON.stringify({format: "jpeg", data_b64: f}))
        const core = fakeComponent.createObject(tc)
        const shell = shellComponent.createObject(tc, {core: core, stateDir: "", setup: fakeSetupComponent.createObject(tc)})
        shell.visible = true
        tryVerify(() => shell.layoutSettled, 5000, "the docks are laid out")
        const jobs = []
        for (let n = 0; n < jobCount; ++n)
            jobs.push(job(n))
        const s = ShellScene.scene({jobs: jobs, desktop: Shapes.desktopSettings()})
        // Every panel once in front, so each has loaded; then Home, Jobs and Hardware in front.
        for (const id of ["confirmations", "usage", "diagnostics", "settings", "home", "jobs", "hardware"]) {
            shell.docks[id].setAsCurrentTab()
            tryVerify(() => {
                ShellScene.answerOpen(core, answered, s)
                return shell.docks[id].content.visible && shell.docks[id].content.width > 0
            }, 5000, id + " is in front")
        }
        const panel = shell.docks.jobs.content
        tryVerify(() => {
            ShellScene.answerOpen(core, answered, s)
            return panel.liveView.count === jobCount
        }, 5000, "Jobs shows the running jobs")
        spinner.createObject(shell.contentItem)
        wait(500)
        ShellScene.answerOpen(core, answered, s)

        // What the bridge's step costs a frame by itself (the base64 and the JPEG decode, on the pump's
        // thread in the app), timed here on a job no row shows; its frame is dropped after.
        let t0 = BenchProbe.nowMs()
        for (let i = 0; i < 50; ++i)
            BridgeHook.event("preview:j_calibration", "event", JSON.stringify({format: "jpeg", data_b64: frames[i % 2]}))
        const decodeEach = (BenchProbe.nowMs() - t0) / 50
        BridgeHook.event("job:j_calibration", "event",
                         JSON.stringify({kind: "status", job: {job_id: "j_calibration", status: "succeeded"}}))

        // The idle frame interval, with only the animation.
        BenchProbe.start(shell)
        wait(3000)
        const idle = BenchProbe.stop()

        // The feed, through the pump: every 25 ms two jobs' progress and preview, so each job's at
        // 10 Hz; a telemetry sample a second. The GUI thread's time on each delivered event is counted.
        const cost = {progress: 0, preview: 0, telemetry: 0, answers: 0}
        const seqs = {}
        const deliver = (topic, kind, json, posted) => {
            seqs[topic] = (seqs[topic] || 0) + 1
            const t = BenchProbe.nowMs()
            core.coreEvent(topic, kind, seqs[topic], json)
            const spent = BenchProbe.nowMs() - t
            if (topic.startsWith("preview:"))
                cost.preview += spent
            else if (topic === "telemetry")
                cost.telemetry += spent
            else
                cost.progress += spent
        }
        BenchPump.delivered.connect(deliver)
        let tick = 0
        let sent = 0
        const feed = Qt.createQmlObject("import QtQuick; Timer { interval: 25; repeat: true }", tc)
        feed.triggered.connect(() => {
            for (let k = 0; k < 2; ++k) {
                const n = (tick * 2 + k) % jobCount
                const id = jobs[n].job_id
                BenchPump.post("job:" + id, "event", JSON.stringify({kind: "progress", value: (tick >> 2) % 1000, max: 1000}))
                BenchPump.post("preview:" + id, "event", previewJson[tick % 2])
                ++sent
            }
            if (tick % 40 === 0)
                BenchPump.post("telemetry", "event", JSON.stringify(Shapes.sample(Shapes.gpu({util_pct: 50 + tick % 40}))))
            const t = BenchProbe.nowMs()
            ShellScene.answerOpen(core, answered, s)
            cost.answers += BenchProbe.nowMs() - t
            ++tick
        })
        BenchProbe.start(shell)
        feed.start()
        wait(measureS * 1000)
        feed.stop()
        const load = BenchProbe.stop()
        feed.destroy()
        wait(200)
        BenchPump.delivered.disconnect(deliver)

        const ms = v => Number(v).toFixed(2)
        const frameBound = Math.max(budgetMs, idle.frames.p99 + budgetMs / 2)
        console.log("bench: gui stalls, idle: heartbeat lateness p99 " + ms(idle.lateness.p99) + " ms; frame interval p50 "
                    + ms(idle.frames.p50) + " ms, p99 " + ms(idle.frames.p99) + " ms over " + idle.frames.n + " frames")
        console.log("bench: gui stalls, " + measureS + " s of " + jobCount + " jobs at 10 Hz (" + sent
                    + " previews and progress events): heartbeat lateness p99 " + ms(load.lateness.p99) + " ms, max "
                    + ms(load.lateness.max) + " ms over " + load.lateness.n + " beats; frame interval p50 "
                    + ms(load.frames.p50) + " ms, p99 " + ms(load.frames.p99) + " ms, max " + ms(load.frames.max)
                    + " ms over " + load.frames.n + " frames (bounds " + budgetMs + " and " + ms(frameBound) + " ms)")
        console.log("bench: gui stalls, GUI-thread time on the delivered events: previews " + ms(cost.preview)
                    + " ms (" + ms(cost.preview / Math.max(1, sent)) + " ms each), progress " + ms(cost.progress) + " ms ("
                    + ms(cost.progress / Math.max(1, sent)) + " ms each), telemetry " + ms(cost.telemetry)
                    + " ms, the fake's answers " + ms(cost.answers) + " ms, of " + (measureS * 1000)
                    + " ms; on the pump's thread, the bridge's decode of a preview: " + ms(decodeEach) + " ms each ("
                    + ms(decodeEach * sent / (measureS * 10)) + " % of that thread)")
        // The 4 ms timer runs at about 100 Hz on this platform (Windows' timers), which sets the
        // heartbeat's floor: its idle lateness is printed first.
        verify(load.lateness.n > measureS * 50, "the heartbeat ran: " + load.lateness.n + " beats")
        verify(load.frames.n > measureS * 10, "frames were drawn: " + load.frames.n)
        verify(load.lateness.p99 < budgetMs, "heartbeat lateness p99 " + ms(load.lateness.p99) + " ms, budget " + budgetMs)
        verify(load.frames.p99 < frameBound, "frame interval p99 " + ms(load.frames.p99) + " ms, bound " + ms(frameBound))
        verify(panel.rowFor(jobs[0].job_id).progressValue > 0, "the rows took the progress")
        shell.destroy()
        wait(100)
    }
}
