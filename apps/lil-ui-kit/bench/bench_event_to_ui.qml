// bench_event_to_ui (label bench; APP-139): how long an event takes to reach the screen, in the whole
// shell under the fake feed. The events reach the shell as Core's do: through BenchPump, the runner's
// stand-in for the session's event pump, which runs the bridge's own event step on a thread of its own
// (a preview's base64 and JPEG decode inline, so the events behind a preview wait for it: the Task 17
// and 23 carry), then the pump's leading-edge throttle for a job's progress (APP-027: at most one every
// 100 ms), and then signals each on the GUI thread, in order, as Core.coreEvent.
// - Job progress: 8 running jobs, each with a progress event and a 512 x 512 JPEG preview every
//   100 ms, for MeasureS seconds. Each event is stamped as the app receives it (Core's signal, after the
//   pump), and counts as shown at the first frame (`frameSwapped`) whose row has reached its value.
//   Pass: p99 under LIL_BUDGET_EVENT_TO_UI_MS (test-params.toml's event_to_ui_ms). The pump's own
//   stage, from the event's arrival to its signal (the decode queue and the throttle's wait, which
//   APP-139's "leading-edge throttle" allows), is printed beside it, with the whole way.
// - A confirmation (never throttled): it is in the Confirmations panel's scene once its event is
//   handled, so it shows in the frame after it arrives; each of 20, with the feed running, reaches a
//   frame under the budget and within two of the platform's frame intervals.
import QtQuick
import QtTest
import Lilith.Core
import Lilith.Shim
import Lilith.UiKit
import "../tests"
import "../tests/CoreShapes.js" as Shapes
import "../tests/ShellScene.js" as ShellScene

TestCase {
    id: tc
    name: "BenchEventToUi"
    when: windowShown
    width: 200
    height: 100

    readonly property real budgetMs: Number(TestFiles.env("LIL_BUDGET_EVENT_TO_UI_MS"))
    readonly property int measureS: Number(TestFiles.env("LIL_BENCH_MEASURE_S")) || 20
    readonly property int jobCount: 8
    // Two 512 x 512 JPEG frames (bench_gui_stalls' kind), made once.
    property var jpegs: []

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
    // At each frame: (frame time) => nothing.
    property var onFrame: null

    Connections {
        id: frames
        target: null
        function onFrameSwapped() {
            if (tc.onFrame)
                tc.onFrame(BenchProbe.nowMs())
        }
    }

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

    function percentile(values, q) {
        const v = values.slice().sort((a, b) => a - b)
        return v.length ? v[Math.min(v.length - 1, Math.floor(q * v.length))] : 0
    }

    function makeShell() {
        const core = fakeComponent.createObject(tc)
        const shell = shellComponent.createObject(tc, {core: core, stateDir: "", setup: fakeSetupComponent.createObject(tc)})
        shell.visible = true
        tryVerify(() => shell.layoutSettled, 5000, "the docks are laid out")
        const jobs = []
        for (let n = 0; n < jobCount; ++n)
            jobs.push(Shapes.job("j_00000000000000" + (10 + n), "running", {progress: {value: 0, max: 100000}}))
        const s = ShellScene.scene({jobs: jobs})
        for (const id of ["usage", "diagnostics", "settings", "home", "hardware", "confirmations", "jobs"]) {
            shell.docks[id].setAsCurrentTab()
            tryVerify(() => {
                ShellScene.answerOpen(core, answered, s)
                return shell.docks[id].content.visible && shell.docks[id].content.width > 0
            }, 5000, id + " is in front")
        }
        tryVerify(() => {
            ShellScene.answerOpen(core, answered, s)
            return shell.docks.jobs.content.liveView.count === jobCount
        }, 5000, "Jobs shows the running jobs")
        spinner.createObject(shell.contentItem)
        frames.target = shell
        wait(300)
        return {core: core, shell: shell, jobs: jobs, scene: s}
    }

    // A new FakeCore numbers its calls from 1 again; the shell before deletes its docks later
    // (KDDockWidgets' names are process-wide).
    function init() {
        answered = {}
        tryVerify(() => {
            const kddw = JSON.parse(DockLayout.serialize())
            return DockLayout.mainWindows().length === 0
                   && !(Array.isArray(kddw.allDockWidgets) && kddw.allDockWidgets.length > 0)
        }, 5000, "the earlier shell's docks are gone")
    }

    function cleanup() {
        onFrame = null
        frames.target = null
    }

    // The feed, through the pump: each 25 ms, two jobs' progress (a value one up) and preview. With
    // `pending`, each job's progress events are recorded there as Core signals them: {value, at, pump},
    // `at` the signal's time and `pump` the time it spent in the pump.
    function startFeed(r, pending, value) {
        if (jpegs.length === 0)
            jpegs = framesB64()
        // Each frame's event text made once: in the app the bytes never reach JavaScript, so the
        // bench mustn't make 4 MB a second of garbage strings for the GUI thread to collect.
        const previewJson = jpegs.map(f => JSON.stringify({format: "jpeg", data_b64: f}))
        const index = {}
        r.jobs.forEach((j, n) => { index["job:" + j.job_id] = n })
        const seqs = {}
        const deliver = (topic, kind, json, posted) => {
            const at = BenchPump.nowMs()
            if (pending && topic in index)
                pending[index[topic]].push({value: JSON.parse(json).value, at: at, pump: at - posted})
            seqs[topic] = (seqs[topic] || 0) + 1
            r.core.coreEvent(topic, kind, seqs[topic], json)
        }
        BenchPump.delivered.connect(deliver)
        let tick = 0
        const feed = Qt.createQmlObject("import QtQuick; Timer { interval: 25; repeat: true }", tc)
        feed.triggered.connect(() => {
            for (let k = 0; k < 2; ++k) {
                const n = (tick * 2 + k) % jobCount
                const id = r.jobs[n].job_id
                value[n] += 1
                BenchPump.post("job:" + id, "event", JSON.stringify({kind: "progress", value: value[n], max: 100000}))
                BenchPump.post("preview:" + id, "event", previewJson[tick % 2])
            }
            ShellScene.answerOpen(r.core, answered, r.scene)
            ++tick
        })
        feed.start()
        return {timer: feed, end: () => {
            feed.stop()
            wait(300)
            BenchPump.delivered.disconnect(deliver)
            feed.destroy()
        }}
    }

    function test_job_progress_reaches_the_screen_within_the_budget() { // spec: APP-139
        verify(budgetMs > 0, "LIL_BUDGET_EVENT_TO_UI_MS is set (CMake passes test-params.toml's budgets)")
        const r = makeShell()
        const panel = r.shell.docks.jobs.content
        // Every job is fed; the rows in view are timed. They are chosen after 2 s of the feed, once the
        // previews have their place, and must stay the same rows to the end.
        const pending = r.jobs.map(() => [])
        const value = r.jobs.map(() => 0)
        const feed = startFeed(r, pending, value)
        wait(2000)
        const measured = []
        const rows = []
        for (let n = 0; n < jobCount; ++n) {
            rows.push(panel.rowFor(r.jobs[n].job_id))
            if (rows[n])
                measured.push(n)
        }
        verify(measured.length >= 2, "rows in view to time: " + measured.length)
        const latencies = []
        const whole = []
        const pumpStage = []
        const startedAt = BenchPump.nowMs()
        onFrame = now => {
            for (const n of measured) {
                const shown = rows[n].progressValue
                while (pending[n].length && pending[n][0].value <= shown) {
                    const e = pending[n].shift()
                    if (e.at < startedAt)
                        continue
                    latencies.push(now - e.at)
                    whole.push(now - e.at + e.pump)
                    pumpStage.push(e.pump)
                }
            }
        }
        wait(measureS * 1000)
        feed.timer.stop()
        wait(500)
        onFrame = null
        feed.end()
        for (const n of measured)
            verify(panel.rowFor(r.jobs[n].job_id) === rows[n], "row " + n + " stayed the same row")
        const left = measured.reduce((sum, n) => sum + pending[n].length, 0)
        const ms = v => v.toFixed(1)
        const p99 = percentile(latencies, 0.99)
        console.log("bench: event to UI, job progress, from Core's signal to the screen: " + latencies.length
                    + " events on " + measured.length + " rows in view, p50 " + ms(percentile(latencies, 0.5)) + " ms, p99 "
                    + ms(p99) + " ms, max " + ms(percentile(latencies, 1)) + " ms (budget " + budgetMs + " ms); "
                    + left + " never shown")
        console.log("bench: event to UI, job progress, the pump's stage (decode queue and the 10 Hz throttle's wait): p50 "
                    + ms(percentile(pumpStage, 0.5)) + " ms, p99 " + ms(percentile(pumpStage, 0.99)) + " ms; the whole way, p50 "
                    + ms(percentile(whole, 0.5)) + " ms, p99 " + ms(percentile(whole, 0.99)) + " ms")
        verify(latencies.length >= measureS * measured.length * 9, "the events were measured: " + latencies.length)
        compare(left, 0, "every event was shown")
        verify(p99 < budgetMs, "job progress p99 " + p99.toFixed(1) + " ms, budget " + budgetMs + " ms")
        r.shell.destroy()
        wait(100)
    }

    function test_a_confirmation_shows_in_the_frame_after_it_arrives() { // spec: APP-139, APP-115
        verify(budgetMs > 0, "LIL_BUDGET_EVENT_TO_UI_MS is set")
        const r = makeShell()
        const panel = r.shell.docks.confirmations.content
        r.shell.docks.confirmations.setAsCurrentTab()
        tryVerify(() => panel.visible && panel.width > 0, 5000, "Confirmations is in front")
        // The platform's own frame pace, with the feed.
        const value = r.jobs.map(() => 0)
        const feed = startFeed(r, null, value)
        BenchProbe.start(r.shell)
        wait(2000)
        const pace = BenchProbe.stop().frames.p50
        const delays = []
        for (let i = 0; i < 20; ++i) {
            const before = panel.cardRepeater.count
            let shownAt = -1
            onFrame = now => { if (shownAt < 0) shownAt = now }
            const at = BenchProbe.nowMs()
            r.core.emit("confirmations", "event", Shapes.confirmationEvent("pending", Shapes.confirmation(
                "cf_0000000000000" + (100 + i), "comfy.run", new Date().toISOString())))
            compare(panel.cardRepeater.count, before + 1, "the card is in the scene as the event is handled")
            verify(panel.cardRepeater.itemAt(before) || panel.cardRepeater.itemAt(0), "its card item")
            tryVerify(() => shownAt >= 0, 2000, "a frame follows")
            delays.push(shownAt - at)
            onFrame = null
            wait(30)
        }
        feed.end()
        const worst = percentile(delays, 1)
        console.log("bench: event to UI, a confirmation to its frame: p50 " + percentile(delays, 0.5).toFixed(1)
                    + " ms, max " + worst.toFixed(1) + " ms (frame pace " + pace.toFixed(1) + " ms, budget " + budgetMs + " ms)")
        verify(worst < budgetMs, "a confirmation took " + worst.toFixed(1) + " ms to a frame")
        verify(worst < 2 * pace + 5, "each showed in the frame after it arrived: " + worst.toFixed(1)
                                      + " ms against a frame pace of " + pace.toFixed(1) + " ms")
        r.shell.destroy()
        wait(100)
    }
}
