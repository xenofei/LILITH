// bench_hardware (label bench): what a 1 Hz telemetry sample costs the Hardware panel at
// 1280 x 800, 10 charts with a full 15-minute and 24-hour window: the event's own work (apply),
// a sample with the frame after it (the scene graph's polish, sync and render included), and a
// frame with only a colour change for comparison, each the mean of 100; and a 4 ms heartbeat's
// p99 and max gap during each (the GUI thread's lateness). It prints the figures and fails only
// past a generous bound (a frame's 16 ms for the event's own work), so the numbers can be
// checked again; the offscreen platform renders on the GUI thread, unlike the app's threaded
// render loop.
import QtQuick
import QtTest
import Lilith.UiKit

TestCase {
    id: tc
    name: "BenchHardware"
    when: windowShown

    Component {
        id: fakeComponent
        FakeCore {}
    }

    Component {
        id: panelWindow
        Window {
            id: win
            property alias panel: panel
            required property var core
            width: 1280
            height: 800
            visible: true
            HardwarePanel {
                id: panel
                anchors.fill: parent
                core: win.core
                title: "Hardware"
            }
        }
    }

    property var gaps: []
    property real lastBeat: 0

    Timer {
        id: beat
        interval: 4
        repeat: true
        onTriggered: {
            const now = Date.now()
            if (tc.lastBeat > 0)
                tc.gaps.push(now - tc.lastBeat)
            tc.lastBeat = now
        }
    }

    function beatStart() {
        gaps = []
        lastBeat = 0
        beat.start()
    }

    function beatStop() {
        beat.stop()
        const g = gaps.slice().sort((a, b) => a - b)
        return "p99 " + g[Math.floor(g.length * 0.99)] + " ms, max " + g[g.length - 1] + " ms"
    }

    function snapshot(ts, v) {
        const cores = []
        for (let i = 0; i < 16; ++i)
            cores.push(100 + 10 * i)
        return {
            ts: new Date(ts).toISOString(),
            gpu: {name: "Test GPU", driver: "1", util_pct: v % 100, power_mw: 285000 + v, temp_c: 71,
                  sm_clock_mhz: 1905, mem_clock_mhz: 10501, fan_pct: 45, throttle: [],
                  vram_total_mib: 16384, vram_used_mib: 9728 + v, measured: true},
            processes: [],
            cpu: {util_permille: 180 + v % 500, per_core_permille: cores, package_power_mw: 64000 + v, measured: true},
            ram: {total_mib: 65536, used_mib: 41984 + v},
            disks: [{volume: "C:\\", free_mib: 122880}],
            sizes: {},
            net: [{adapter: "Ethernet", rx_bps: 1200000 + v, tx_bps: 40000}],
            system_power_mw: 412000 + v,
            system_parts: [{part: "gpu", mw: 285000, measured: "measured"},
                           {part: "cpu", mw: 64000, measured: "measured"},
                           {part: "rest", mw: 63000, measured: "estimated"}],
            alerts: [],
            gpu_users: []
        }
    }

    function run(windowS) {
        const n = 100
        const fake = createTemporaryObject(fakeComponent, tc)
        const w = createTemporaryObject(panelWindow, tc, {core: fake})
        tryVerify(() => w.panel.width > 0)
        w.panel.windowS = windowS
        fake.respond(fake.calls.find(c => c.name === "telemetry.snapshot").id,
                     {status: "ok", structured: snapshot(Date.now(), 0)})
        const calls = fake.calls.filter(c => c.name === "telemetry.series")
        const last = calls[calls.length - 1]
        const step = last.args.resolution === "1s" ? 1000 : 60000
        const to = Date.parse(last.args.to)
        const series = last.args.metrics.map(m => {
            const points = []
            for (let t = Date.parse(last.args.from), i = 0; t <= to; t += step, ++i)
                points.push([new Date(t).toISOString(), 10 + i % 7, 20 + i % 5, 30 + i % 11])
            return {metric: m, points: points}
        })
        fake.respond(last.id, {status: "ok", structured: {series: series}})
        wait(300)
        let ts = to
        let t0 = Date.now()
        for (let i = 0; i < n; ++i) {
            ts += 1000
            fake.emit("telemetry", "event", {kind: "sample", snapshot: snapshot(ts, i)})
        }
        const apply = (Date.now() - t0) / n
        beatStart()
        t0 = Date.now()
        for (let i = 0; i < n; ++i) {
            ts += 1000
            fake.emit("telemetry", "event", {kind: "sample", snapshot: snapshot(ts, i)})
            waitForRendering(w.contentItem, 2000)
        }
        const frame = (Date.now() - t0) / n
        const frameBeat = beatStop()
        beatStart()
        t0 = Date.now()
        for (let i = 0; i < n; ++i) {
            w.color = (i % 2) ? "#101010" : "#111111"
            waitForRendering(w.contentItem, 2000)
        }
        const baseline = (Date.now() - t0) / n
        const baselineBeat = beatStop()
        console.log("BENCH hardware window " + windowS + " s, " + series[0].points.length + " points a chart, "
                    + w.panel.chartTiles[0].series.pointCount + " drawn: apply " + apply.toFixed(2)
                    + " ms; sample and frame " + frame.toFixed(2) + " ms (heartbeat " + frameBeat
                    + "); colour-only frame " + baseline.toFixed(2) + " ms (heartbeat " + baselineBeat + ")")
        verify(apply < 16, "a sample's own work stays within a frame: " + apply + " ms")
    }

    function test_15_minutes() { run(15 * 60) }
    function test_24_hours() { run(24 * 3600) }
}
