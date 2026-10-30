// The Hardware panel (APP-127 to APP-129, APP-131, APP-132; P4a-R17): live values from the
// telemetry snapshot, no CPU temperature, the 15-minute to 24-hour charts, the energy table by
// period, the alerts strip, "who's using the GPU", the estimated and missing labels, its state
// kept through a float and re-dock (ARCH-116), and the accessibility walk. The core is FakeCore.
import QtQuick
import QtTest
import Lilith.Shim
import Lilith.UiKit

TestCase {
    id: tc
    name: "Hardware"
    when: windowShown
    width: 200
    height: 100

    A11yWalk { id: walk }

    Component {
        id: fakeComponent
        FakeCore {}
    }

    // The panel alone in a window of its own, as large as the offscreen screen.
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

    Component {
        id: shellComponent
        ShellWindow {}
    }

    // The shell of a test before deletes its docks later (KDDockWidgets' names are
    // process-wide): let that run first.
    function init() {
        wait(100)
    }

    function snapshot(over) {
        const cores = []
        for (let i = 0; i < 16; ++i)
            cores.push(100 + 10 * i)
        const s = {
            ts: "2026-10-28T12:00:00.000Z",
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
            alerts: [],
            gpu_users: []
        }
        return Object.assign(s, over || {})
    }

    function makePanel() {
        const fake = createTemporaryObject(fakeComponent, tc)
        const w = createTemporaryObject(panelWindow, tc, {core: fake})
        verify(w, "the panel's window")
        tryVerify(() => w.panel.width > 0)
        return {fake: fake, w: w, panel: w.panel}
    }

    function callsTo(fake, tool) {
        return fake.calls.filter(c => c.name === tool)
    }

    function answer(fake, tool, structured) {
        const open = callsTo(fake, tool)
        verify(open.length > 0, "a " + tool + " call")
        verify(fake.respond(open[open.length - 1].id, {status: "ok", structured: structured}), tool + " answered")
    }

    function texts(root) {
        const out = []
        visitAll(root, item => {
            if (item.visible && typeof item.text === "string" && String(item).startsWith("QQuickText"))
                out.push(item.text)
        })
        return out
    }

    function visitAll(item, fn) {
        fn(item)
        for (let i = 0; i < item.children.length; ++i)
            visitAll(item.children[i], fn)
    }

    function find(root, test) {
        if (test(root))
            return root
        for (let i = 0; i < root.children.length; ++i) {
            const found = find(root.children[i], test)
            if (found)
                return found
        }
        return null
    }

    function named(root, name) {
        return find(root, item => item.objectName === name)
    }

    function shows(root, text) {
        return texts(root).some(t => t.indexOf(text) >= 0)
    }

    function verifyShows(root, text, where) {
        verify(shows(root, text), (where || "the panel") + " shows \"" + text + "\": " + JSON.stringify(texts(root)))
    }

    function test_live_values_from_the_snapshot() { // spec: APP-127
        const t = makePanel()
        compare(t.fake.subscribed.indexOf("telemetry") >= 0, true, "the panel follows telemetry")
        answer(t.fake, "telemetry.snapshot", snapshot())
        const gpu = named(t.panel, "gpuCard")
        verifyShows(gpu, "63 %", "GPU")
        verifyShows(gpu, "9.5 / 16 GiB", "GPU")
        verifyShows(gpu, "71 °C", "GPU")
        verifyShows(gpu, "285 W", "GPU")
        verifyShows(gpu, "1,905 MHz", "GPU")
        verifyShows(gpu, "10,501 MHz", "GPU")
        verifyShows(gpu, "45 %", "GPU")
        verifyShows(gpu, "Power cap", "GPU")
        const cpu = named(t.panel, "cpuCard")
        verifyShows(cpu, "18 %", "CPU")
        verifyShows(cpu, "64 W", "CPU")
        const bars = []
        visitAll(cpu, item => { if (item.objectName === "coreBar" && item.visible) bars.push(item) })
        compare(bars.length, 16, "a load bar per core")
        compare(LilShim.accessibleOf(bars[0]).name, "Core 1: 10 %")
        verifyShows(named(t.panel, "ramCard"), "41 / 64 GiB", "RAM")
        const disks = named(t.panel, "disksCard")
        verifyShows(disks, "120 GiB free", "Disks")
        verifyShows(disks, "500 GiB free", "Disks")
        verifyShows(disks, "12 GiB", "Disks")
        verifyShows(disks, "171 GiB", "Disks")
        const net = named(t.panel, "networkCard")
        verifyShows(net, "Ethernet", "Network")
        verifyShows(net, "1.2 MB/s", "Network")
        verifyShows(net, "40 kB/s", "Network")
        verifyShows(t.panel, "412 W", "the panel")

        // A sample replaces the state; an older one, overtaken, is dropped.
        t.fake.emit("telemetry", "event", {kind: "sample", snapshot: snapshot({ts: "2026-10-28T12:00:02.000Z",
            gpu: Object.assign(snapshot().gpu, {util_pct: 88})})})
        verifyShows(gpu, "88 %", "GPU after a sample")
        t.fake.emit("telemetry", "event", {kind: "sample", snapshot: snapshot({ts: "2026-10-28T12:00:01.000Z"})})
        verifyShows(gpu, "88 %", "GPU after an older sample")
        verify(!shows(gpu, "63 %"), "the older sample isn't applied")

        // No GPU: said so, nothing guessed.
        t.fake.emit("telemetry", "event", {kind: "sample", snapshot: snapshot({ts: "2026-10-28T12:00:03.000Z", gpu: undefined})})
        verifyShows(gpu, "No GPU reading", "GPU")
    }

    function test_cpu_temperature_is_never_shown() { // spec: APP-128
        const t = makePanel()
        // Even a snapshot that carried one.
        const s = snapshot()
        s.cpu.temp_c = 83
        answer(t.fake, "telemetry.snapshot", s)
        const cpu = named(t.panel, "cpuCard")
        verify(texts(cpu).length > 0, "the CPU card shows its values")
        for (const text of texts(cpu))
            verify(text.indexOf("°C") < 0 && !/temperature/i.test(text), "the CPU card shows no temperature: " + text)
        for (const text of texts(t.panel)) {
            verify(text.indexOf("83") < 0, "no CPU temperature anywhere: " + text)
            verify(!/CPU temp/i.test(text), "no CPU temperature anywhere: " + text)
        }
        for (const tile of t.panel.chartTiles)
            verify(!/CPU temp/i.test(tile.label), "no CPU temperature chart: " + tile.label)
    }

    function test_opens_on_15_minutes_and_zooms_to_24_hours() { // spec: APP-129
        const t = makePanel()
        compare(t.panel.windowS, 15 * 60, "the panel opens on 15 minutes")
        let calls = callsTo(t.fake, "telemetry.series")
        compare(calls.length, 1, "one series call for every chart")
        let args = calls[0].args
        compare(args.resolution, "1s")
        compare(Date.parse(args.to) - Date.parse(args.from), 15 * 60 * 1000 - 1000, "15 minutes of 1-second points")
        verify(args.metrics.indexOf("gpu.util") >= 0 && args.metrics.indexOf("ram.used") >= 0, JSON.stringify(args.metrics))
        verify(args.metrics.indexOf("cpu.temp") < 0)

        // The answer reaches the charts, and live samples follow.
        const to = Date.parse(args.to)
        const points = []
        for (let i = 0; i < 900; ++i) {
            const ts = new Date(to - (899 - i) * 1000).toISOString()
            points.push([ts, 40, 50, i === 450 ? 99 : 60])
        }
        answer(t.fake, "telemetry.series", {series: [{metric: "gpu.util", points: points}]})
        const tile = t.panel.chartTiles.find(c => c.metric === "gpu.util")
        verify(tile, "the GPU load chart")
        verify(tile.series.pointCount > 0, "the chart has points")
        verify(tile.series.pointCount <= tile.plotWidth, "no more points than pixels: "
               + tile.series.pointCount + " for " + tile.plotWidth)
        compare(tile.series.upperPath.length, tile.series.pointCount, "the polyline has a point per pixel at most")
        compare(tile.series.upperPath[tile.series.pointCount - 1].y >= 0, true)
        compare(tile.series.peak, 99, "the spike is kept")
        t.fake.emit("telemetry", "event", {kind: "sample", snapshot: snapshot({ts: new Date(to + 1000).toISOString()})})
        compare(tile.series.latest, 63, "a live sample is appended")
        verify(/chart/.test(LilShim.accessibleOf(tile).name), LilShim.accessibleOf(tile).name)

        // Zoom out to 24 hours: 1-minute points.
        const day = find(t.panel, item => String(item).startsWith("LButton") && item.text === "24 h")
        verify(day, "the 24 h button")
        mouseClick(day)
        compare(t.panel.windowS, 24 * 3600)
        compare(tile.series.windowS, 24 * 3600, "the charts follow the window")
        calls = callsTo(t.fake, "telemetry.series")
        compare(calls.length, 2)
        args = calls[1].args
        compare(args.resolution, "1m")
        compare(Date.parse(args.to) - Date.parse(args.from), 24 * 3600 * 1000 - 60 * 1000)
        verify(day.checked, "24 h is the window shown")
        const quarter = find(t.panel, item => String(item).startsWith("LButton") && item.text === "15 min")
        verify(!quarter.checked)
        mouseClick(quarter)
        compare(t.panel.windowS, 15 * 60)
        compare(callsTo(t.fake, "telemetry.series")[2].args.resolution, "1s")
    }

    function test_energy_table_calls_telemetry_energy_for_each_period() { // spec: APP-131
        const t = makePanel()
        const calls = callsTo(t.fake, "telemetry.energy")
        compare(calls.length, 4, "one call per period")
        const to = calls[0].args.to
        for (const c of calls)
            compare(c.args.to, to, "every period ends now")
        const now = new Date(Date.parse(to))
        const day = new Date(now); day.setHours(0, 0, 0, 0)
        const month = new Date(now.getFullYear(), now.getMonth(), 1)
        const year = new Date(now.getFullYear(), 0, 1)
        verify(Date.parse(calls[0].args.from) <= Date.parse(to), "the session began before now")
        compare(calls[1].args.from, day.toISOString(), "today, from local midnight")
        compare(calls[2].args.from, month.toISOString(), "this month, from its local 1st")
        compare(calls[3].args.from, year.toISOString(), "this year, from its local 1 January")
        verify(callsTo(t.fake, "telemetry.jobs").length === 1, "the recent jobs' energy")

        const measured = {gpu: true, cpu: true}
        t.fake.respond(calls[0].id, {status: "ok", structured: {from: calls[0].args.from, to: to,
            gpu_mj: 3600000, cpu_mj: 1800000, system_mj: 7200000, measured: measured}})
        t.fake.respond(calls[1].id, {status: "ok", structured: {from: calls[1].args.from, to: to,
            gpu_mj: 360000000, cpu_mj: 36000000, system_mj: 1800000000, measured: measured,
            energy_cost: {amount: "0.42", currency: "EUR"}}})
        t.fake.respond(calls[2].id, {status: "ok", structured: {from: calls[2].args.from, to: to,
            gpu_mj: 36000000000, cpu_mj: 3600000000, system_mj: 54000000000, measured: measured,
            energy_cost: {amount: "12.60", currency: "EUR"}}})
        t.fake.respond(calls[3].id, {status: "ok", structured: {from: calls[3].args.from, to: to,
            gpu_mj: 0, cpu_mj: 0, system_mj: 0, measured: measured}})
        answer(t.fake, "telemetry.jobs", {jobs: [{job_id: "j_0123456789", workspace: "loose",
            from: "2026-10-28T11:00:00.000Z", to: "2026-10-28T11:05:00.000Z", gpu_mj_gross: 36000000,
            gpu_mj_above_idle: 18000000, cpu_mj_gross: 3600000, cpu_mj_above_idle: 1800000,
            system_mj: 54000000, gpu_seconds_milli: 250000, measured: measured}]})

        const table = named(t.panel, "energyTable")
        verify(table, "the energy table")
        const row = p => named(table, "energyRow:" + p)
        verifyShows(row("session"), "This session", "the session row")
        verifyShows(row("session"), "2.0 Wh", "the session row")
        verifyShows(row("day"), "Today", "the day row")
        verifyShows(row("day"), "500 Wh", "the day row")
        verifyShows(row("day"), "100 Wh", "the day row")
        verifyShows(row("day"), "0.42 EUR", "the day row's cost")
        verifyShows(row("month"), "15.00 kWh", "the month row")
        verifyShows(row("month"), "12.60 EUR", "the month row's cost")
        verifyShows(row("year"), "This year", "the year row")
        verifyShows(row("year"), "0.0 Wh", "the year row")
        compare(LilShim.accessibleOf(row("day")).role, Accessible.Row)
        verify(/Today/.test(LilShim.accessibleOf(row("day")).name) && /0\.42 EUR/.test(LilShim.accessibleOf(row("day")).name),
               LilShim.accessibleOf(row("day")).name)
        verifyShows(table, "15 Wh", "the job's whole-system energy")

        // Without a rate, no cost column.
        const u = makePanel()
        const c2 = callsTo(u.fake, "telemetry.energy")
        for (const c of c2)
            u.fake.respond(c.id, {status: "ok", structured: {from: c.args.from, to: c.args.to,
                gpu_mj: 0, cpu_mj: 0, system_mj: 0, measured: measured}})
        verify(!shows(named(u.panel, "energyTable"), "Cost"), "no cost column without a rate")
        verifyShows(named(t.panel, "energyTable"), "Cost", "a cost column with a rate")
    }

    function test_alerts_show_and_clear() { // spec: APP-132
        const t = makePanel()
        answer(t.fake, "telemetry.snapshot", snapshot())
        const strip = named(t.panel, "alertsStrip")
        verify(strip, "the alerts strip")
        verify(!strip.visible, "no alerts, no strip")
        t.fake.emit("telemetry", "event", {kind: "alert", alert: {id: "gpu_temperature", kind: "temperature",
                                                                 value: 88, since: "2026-10-28T12:00:05.000Z"}})
        tryVerify(() => strip.visible, 2000, "an alert shows the strip")
        verifyShows(strip, "88 °C", "the alert")
        t.fake.emit("telemetry", "event", {kind: "alert", alert: {id: "gpu_throttle_thermal", kind: "throttle_thermal",
                                                                 value: 64, since: "2026-10-28T12:00:06.000Z"}})
        verifyShows(strip, "slowing down", "the throttle alert")
        const banners = []
        visitAll(strip, item => { if (String(item).startsWith("LBanner") && item.visible) banners.push(item) })
        compare(banners.length, 2)
        compare(LilShim.accessibleOf(banners[0]).role, Accessible.AlertMessage)
        t.fake.emit("telemetry", "event", {kind: "alert_cleared", id: "gpu_temperature"})
        verify(!shows(strip, "88 °C"), "the cleared alert goes")
        verifyShows(strip, "slowing down", "the other stays")
        // The next sample's alerts are the state.
        t.fake.emit("telemetry", "event", {kind: "sample", snapshot: snapshot({ts: "2026-10-28T12:00:09.000Z", alerts: []})})
        tryVerify(() => !strip.visible, 2000, "all cleared, no strip")
        t.fake.emit("telemetry", "event", {kind: "sample", snapshot: snapshot({ts: "2026-10-28T12:00:10.000Z",
            alerts: [{id: "gpu_throttle_power_brake", kind: "throttle_power_brake", value: 128, since: "2026-10-28T12:00:10.000Z"}]})})
        tryVerify(() => strip.visible, 2000)
        verifyShows(strip, "power brake", "an alert from a sample")
    }

    function test_gpu_users_list_names_the_app_and_engines() { // spec: APP-127
        const t = makePanel()
        // An untracked process's name is untrusted: shown through Core.shownText (P4a-R8).
        const longName = "Night Racer " + "x".repeat(80) + ".exe"
        answer(t.fake, "telemetry.snapshot", snapshot({gpu_users: [
            {pid: 4100, name: "comfyui", vram_local_mib: 6144, sm_permille: 900, role: "comfyui"},
            {pid: 3300, name: longName, vram_local_mib: 2048},
            {pid: 1200, name: "app", vram_local_mib: 512, role: "app"}
        ]}))
        const users = named(t.panel, "gpuUsers")
        verify(users, "the GPU users list")
        const rows = []
        visitAll(users, item => { if (item.objectName === "gpuUser" && item.visible) rows.push(item) })
        compare(rows.length, 3)
        verifyShows(rows[0], "ComfyUI", "the engine's row")
        verifyShows(rows[0], "6,144 MiB", "the engine's row")
        verifyShows(rows[0], "90 %", "the engine's SM share")
        verifyShows(rows[1], "Night Racer", "the game's row")
        verifyShows(rows[1], "2,048 MiB", "the game's row")
        verify(shows(rows[1], t.fake.shownText(longName, 48)), "the untrusted name is shown through shownText")
        verify(!shows(rows[1], longName), "never as it came")
        verifyShows(rows[2], "LILITH (this app)", "the app's row")
        verifyShows(rows[2], "512 MiB", "the app's row")
        compare(LilShim.accessibleOf(rows[0]).name, "ComfyUI: 6,144 MiB, 90 % of the GPU")
    }

    function test_estimated_and_missing_parts_are_labelled() { // spec: APP-127, APP-131
        const t = makePanel()
        const s = snapshot()
        s.gpu.measured = false
        delete s.gpu.fan_pct
        s.cpu.measured = false
        s.cpu.package_power_mw = 65000
        delete s.sizes.models_mib
        s.system_parts = [{part: "gpu", mw: 285000, measured: "measured"},
                          {part: "cpu", mw: 65000, measured: "estimated"},
                          {part: "rest", mw: 0, measured: "unknown"}]
        answer(t.fake, "telemetry.snapshot", s)
        verifyShows(named(t.panel, "gpuCard"), "last reading", "a lost GPU")
        verifyShows(named(t.panel, "gpuCard"), "not reported", "no fan reading")
        verifyShows(named(t.panel, "cpuCard"), "65 W (estimated)", "the CPU's estimate")
        verifyShows(named(t.panel, "disksCard"), "not measured", "no model-folder size")
        const parts = named(t.panel, "systemParts")
        verifyShows(parts, "CPU 65 W estimated", "the parts")
        verifyShows(parts, "rest unknown", "the parts")
        // An unknown CPU power is unknown, not 0 W.
        t.fake.emit("telemetry", "event", {kind: "sample", snapshot: Object.assign(s, {ts: "2026-10-28T12:00:01.000Z",
            cpu: Object.assign({}, s.cpu, {package_power_mw: 0})})})
        verifyShows(named(t.panel, "cpuCard"), "unknown", "an unknown CPU power")
        verify(!shows(named(t.panel, "cpuCard"), "0 W"))

        const calls = callsTo(t.fake, "telemetry.energy")
        for (const c of calls)
            t.fake.respond(c.id, {status: "ok", structured: {from: c.args.from, to: c.args.to,
                gpu_mj: 3600000, cpu_mj: 3600000, system_mj: 7200000, measured: {gpu: true, cpu: false}}})
        const day = named(named(t.panel, "energyTable"), "energyRow:day")
        verifyShows(day, "1.0 Wh (estimated)", "the CPU's estimated energy")
        verify(!shows(named(day, "energyGpu"), "estimated"), "the measured GPU energy isn't labelled")
    }

    function test_float_and_redock_keeps_hardware_state() { // spec: ARCH-116
        const fake = createTemporaryObject(fakeComponent, tc)
        const shell = createTemporaryObject(shellComponent, tc, {core: fake, stateDir: ""})
        shell.visible = true
        tryVerify(() => shell.docks.home.content && shell.docks.home.content.width > 0, 5000, "the docks are laid out")
        tryVerify(() => shell.docks.hardware.content && shell.docks.hardware.content.width > 0, 5000)
        wait(50)
        const dock = shell.docks.hardware
        const panel = dock.content
        verify(String(panel).startsWith("HardwarePanel"), "the Hardware panel replaces its placeholder: " + panel)
        answer(fake, "telemetry.snapshot", snapshot())
        const zoom = find(panel, item => String(item).startsWith("LButton") && item.text === "24 h")
        panel.windowS = 24 * 3600
        verify(zoom.checked)
        const before = callsTo(fake, "telemetry.series").length
        dock.isFloating = true
        tryVerify(() => dock.isFloating && panel.Window.window && panel.Window.window !== shell, 5000,
                  "Hardware floats in its own window")
        verify(dock.content === panel, "the same panel item")
        compare(panel.windowS, 24 * 3600)
        verifyShows(named(panel, "gpuCard"), "63 %", "the floating panel")
        dock.isFloating = false
        tryVerify(() => panel.Window.window === shell, 5000, "Hardware docks back")
        verify(dock.content === panel, "still the same panel item")
        compare(panel.windowS, 24 * 3600)
        verifyShows(named(panel, "gpuCard"), "63 %", "the re-docked panel")
        compare(callsTo(fake, "telemetry.series").length, before, "nothing reloaded")
    }

    function test_the_panel_passes_the_accessibility_walk() { // spec: APP-156
        const t = makePanel()
        answer(t.fake, "telemetry.snapshot", snapshot({
            alerts: [{id: "gpu_temperature", kind: "temperature", value: 88, since: "2026-10-28T12:00:05.000Z"}],
            gpu_users: [{pid: 4100, name: "comfyui", vram_local_mib: 6144, role: "comfyui"}]}))
        for (const c of callsTo(t.fake, "telemetry.energy"))
            t.fake.respond(c.id, {status: "ok", structured: {from: c.args.from, to: c.args.to,
                gpu_mj: 1, cpu_mj: 1, system_mj: 2, measured: {gpu: true, cpu: true}}})
        verify(walk.interactiveCount(t.panel) >= 14, "the walk visits the window buttons and the charts: "
               + walk.interactiveCount(t.panel))
        compare(walk.report(t.panel), [])
        verify(t.panel.chartTiles.length >= 8, t.panel.chartTiles.length + " charts")
        for (const tile of t.panel.chartTiles) {
            const a = LilShim.accessibleOf(tile)
            compare(a.role, Accessible.Chart, tile.label)
            verify(/ chart/.test(a.name), "the chart's name says chart: " + a.name)
        }
    }
}
