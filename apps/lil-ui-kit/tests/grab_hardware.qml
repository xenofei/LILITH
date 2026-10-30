// grab_hardware: the Hardware panel offscreen at 1280 x 800, saved for the reviewer in the grab
// folder (LIL_GRAB_DIR, else CMake's build/<preset>/grabs): hardware-live.png (15 minutes of
// charts, an alert, the GPU users and the energy with a cost) and hardware-estimated.png (24
// hours, a lost GPU, estimated and unknown parts). Each grab must have the window's size and not
// be one flat colour. The data is a fixture; the core is FakeCore.
import QtQuick
import QtTest
import Lilith.UiKit

TestCase {
    id: tc
    name: "GrabHardware"
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

    function snapshot(ts) {
        const cores = []
        for (let i = 0; i < 16; ++i)
            cores.push(Math.round(120 + 600 * Math.abs(Math.sin(i * 1.7))))
        return {
            ts: new Date(ts).toISOString(),
            gpu: {name: "Test GPU", driver: "999.99", util_pct: 63, power_mw: 285000, temp_c: 71,
                  sm_clock_mhz: 1905, mem_clock_mhz: 10501, fan_pct: 45, throttle: ["sw_power_cap"],
                  vram_total_mib: 16384, vram_used_mib: 9728, measured: true},
            processes: [],
            cpu: {util_permille: 180, per_core_permille: cores, package_power_mw: 64000, measured: true},
            ram: {total_mib: 65536, used_mib: 41984},
            disks: [{volume: "C:\\", free_mib: 122880}, {volume: "D:\\", free_mib: 512000}],
            sizes: {workspace_mib: 12288, models_mib: 175104},
            net: [{adapter: "Ethernet", rx_bps: 1200000, tx_bps: 40000}],
            system_power_mw: 412000,
            system_parts: [{part: "gpu", mw: 285000, measured: "measured"},
                           {part: "cpu", mw: 64000, measured: "measured"},
                           {part: "rest", mw: 63000, measured: "estimated"}],
            alerts: [{id: "gpu_temperature", kind: "temperature", value: 87,
                      since: new Date(ts - 90000).toISOString()}],
            gpu_users: [{pid: 4100, name: "comfyui", vram_local_mib: 6144, sm_permille: 900, role: "comfyui"},
                        {pid: 3300, name: "NightRacer.exe", vram_local_mib: 2048, sm_permille: 60},
                        {pid: 1200, name: "app", vram_local_mib: 512, role: "app"}]
        }
    }

    // Each metric over the call's range, a wave with a spike.
    function series(args) {
        const from = Date.parse(args.from), to = Date.parse(args.to)
        const step = args.resolution === "1s" ? 1000 : 60000
        const scale = {"gpu.util": 100, "gpu.vram_used": 16384, "gpu.temp": 90, "gpu.power": 350000,
                       "cpu.util": 1000, "cpu.power": 120000, "ram.used": 65536, "system.power": 550000,
                       "net.rx": 3000000, "net.tx": 400000}
        return {series: args.metrics.map((m, k) => {
            const points = []
            for (let ts = from, i = 0; ts <= to; ts += step, ++i) {
                const v = scale[m] * (0.45 + 0.25 * Math.sin(i / (25 + 7 * k)) + 0.08 * Math.sin(i / 3.1))
                const spike = i % 337 === 200 ? scale[m] * 0.3 : 0
                points.push([new Date(ts).toISOString(), Math.round(v * 0.9), Math.round(v), Math.round(Math.min(scale[m], v * 1.05 + spike))])
            }
            return {metric: m, points: points}
        })}
    }

    function answerAll(fake, tool, make) {
        for (const c of fake.calls.filter(c => c.name === tool))
            fake.respond(c.id, {status: "ok", structured: make(c.args)})
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

    function save(w, name) {
        wait(300)
        compare(Qt.application.font.family, Theme.fontFamily, "the grab shows the theme's face")
        const img = grabImage(w.contentItem)
        compare(img.width, Math.round(w.width * w.devicePixelRatio))
        compare(img.height, Math.round(w.height * w.devicePixelRatio))
        const colours = distinctColours(img)
        verify(colours >= 4, "the grab has " + colours + " distinct colours")
        const dir = TestFiles.env("LIL_GRAB_DIR") || TestFiles.env("LIL_GRAB_DEFAULT_DIR")
        verify(dir !== "", "a grab folder")
        verify(TestFiles.mkpath(dir))
        img.save(dir + "/" + name)
        verify(TestFiles.list(dir).indexOf(name) >= 0, "saved in " + dir)
    }

    function energyOf(args) {
        const span = Date.parse(args.to) - Date.parse(args.from)
        const mj = Math.round(span / 1000 * 412000)
        return {from: args.from, to: args.to, gpu_mj: Math.round(mj * 0.69), cpu_mj: Math.round(mj * 0.16),
                system_mj: mj, measured: {gpu: true, cpu: true},
                energy_cost: {amount: (mj / 3.6e9 * 0.31).toFixed(2), currency: "EUR"}}
    }

    function test_grab_hardware_live() { // spec: APP-127, APP-129, APP-131, APP-132
        const fake = createTemporaryObject(fakeComponent, tc)
        const w = createTemporaryObject(panelWindow, tc, {core: fake})
        tryVerify(() => w.panel.width > 0)
        const now = Date.now()
        answerAll(fake, "telemetry.snapshot", () => snapshot(now))
        answerAll(fake, "telemetry.series", series)
        answerAll(fake, "telemetry.energy", energyOf)
        answerAll(fake, "telemetry.jobs", () => ({jobs: [
            {job_id: "j_7f3a9c21", workspace: "loose", from: new Date(now - 600000).toISOString(),
             to: new Date(now - 300000).toISOString(), gpu_mj_gross: 90000000, gpu_mj_above_idle: 72000000,
             cpu_mj_gross: 9000000, cpu_mj_above_idle: 5000000, system_mj: 126000000,
             gpu_seconds_milli: 290000, measured: {gpu: true, cpu: true}}]}))
        save(w, "hardware-live.png")
    }

    function test_grab_hardware_estimated() { // spec: APP-127, APP-129
        const fake = createTemporaryObject(fakeComponent, tc)
        const w = createTemporaryObject(panelWindow, tc, {core: fake})
        tryVerify(() => w.panel.width > 0)
        w.panel.windowS = 24 * 3600
        const now = Date.now()
        const s = snapshot(now)
        s.gpu.measured = false
        delete s.gpu.fan_pct
        s.cpu.measured = false
        s.cpu.package_power_mw = 65000
        delete s.sizes.models_mib
        s.alerts = []
        s.system_parts = [{part: "gpu", mw: 285000, measured: "measured"},
                          {part: "cpu", mw: 65000, measured: "estimated"},
                          {part: "rest", mw: 0, measured: "unknown"}]
        answerAll(fake, "telemetry.snapshot", () => s)
        answerAll(fake, "telemetry.series", series)
        answerAll(fake, "telemetry.energy", args => Object.assign(energyOf(args), {measured: {gpu: true, cpu: false},
                                                                                  energy_cost: undefined}))
        answerAll(fake, "telemetry.jobs", () => ({jobs: []}))
        save(w, "hardware-estimated.png")
    }
}
