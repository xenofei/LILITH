// The Hardware panel (APP-127 to APP-129, APP-131, APP-132; P4a-R17): live values from the
// telemetry snapshot, no CPU temperature, the 15-minute to 24-hour charts, the energy table by
// period, the alerts strip, "who's using the GPU", the estimated and missing labels, its state
// kept through a float and re-dock (ARCH-116), and the accessibility walk. The core is FakeCore.
import QtQuick
import QtTest
import Lilith.Core
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
    // process-wide): wait until they are gone.
    function init() {
        tryVerify(() => {
            const kddw = JSON.parse(DockLayout.serialize())
            return DockLayout.mainWindows().length === 0
                   && !(Array.isArray(kddw.allDockWidgets) && kddw.allDockWidgets.length > 0)
        }, 5000, "an earlier shell's docks are gone")
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
        compare(Date.parse(args.to) - Date.parse(args.from), 15 * 60 * 1000 - 2000,
                "15 minutes of 1-second points, two short of the tool's cap at an hour")
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
        compare(tile.series.upperPaths.length, 1, "one line: the series has no hole")
        compare(tile.series.upperPaths[0].length, tile.series.pointCount, "a point per pixel at most")
        // The spike reaches the polyline drawn: its highest point is at 99 % of the plot (the
        // load's scale is 100).
        const top = Math.min(...tile.series.upperPaths[0].map(p => p.y))
        fuzzyCompare(top, tile.series.heightPx * (1 - 0.99), 0.01, "the spike is drawn")
        verify(/peak 99 %/.test(LilShim.accessibleOf(tile).name), LilShim.accessibleOf(tile).name)
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
        compare(Date.parse(args.to) - Date.parse(args.from), 24 * 3600 * 1000 - 2 * 60 * 1000)
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
        // 999.96 Wh: a kWh once rounded.
        t.fake.respond(calls[0].id, {status: "ok", structured: {from: calls[0].args.from, to: to,
            gpu_mj: 3600000, cpu_mj: 1800000, system_mj: 3599856000, measured: measured}})
        t.fake.respond(calls[1].id, {status: "ok", structured: {from: calls[1].args.from, to: to,
            gpu_mj: 360000000, cpu_mj: 36000000, system_mj: 1800000000, measured: measured,
            energy_cost: {amount: "0.42", currency: "EUR"}}})
        t.fake.respond(calls[2].id, {status: "ok", structured: {from: calls[2].args.from, to: to,
            gpu_mj: 36000000000, cpu_mj: 3600000000, system_mj: 54000000000, measured: measured,
            energy_cost: {amount: "12.60", currency: "EUR"}}})
        t.fake.respond(calls[3].id, {status: "ok", structured: {from: calls[3].args.from, to: to,
            gpu_mj: 0, cpu_mj: 0, system_mj: 10709000000000, measured: measured}})
        answer(t.fake, "telemetry.jobs", {jobs: [{job_id: "j_0123456789", workspace: "loose",
            from: "2026-10-28T11:00:00.000Z", to: "2026-10-28T11:05:00.000Z", gpu_mj_gross: 36000000,
            gpu_mj_above_idle: 18000000, cpu_mj_gross: 3600000, cpu_mj_above_idle: 1800000,
            system_mj: 54000000, gpu_seconds_milli: 250000, measured: measured}]})

        const table = named(t.panel, "energyTable")
        verify(table, "the energy table")
        const row = p => named(table, "energyRow:" + p)
        verifyShows(row("session"), "This session", "the session row")
        verifyShows(row("session"), "1.00 kWh", "the session row")
        verifyShows(row("session"), "1.0 Wh", "the session row")
        verifyShows(table, "Whole system (estimated)", "the whole system is always an estimate")
        verify(/whole system 500 Wh \(estimated\)/.test(LilShim.accessibleOf(row("day")).name),
               LilShim.accessibleOf(row("day")).name)
        verifyShows(row("day"), "Today", "the day row")
        verifyShows(row("day"), "500 Wh", "the day row")
        verifyShows(row("day"), "100 Wh", "the day row")
        verifyShows(row("day"), "0.42 EUR", "the day row's cost")
        verifyShows(row("month"), "15.00 kWh", "the month row")
        verifyShows(row("month"), "12.60 EUR", "the month row's cost")
        verifyShows(row("year"), "This year", "the year row")
        verifyShows(row("year"), "0.0 Wh", "a measured nothing is 0")
        verifyShows(row("year"), "2,974.72 kWh", "thousands are grouped")
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

        // Hidden (closed, or behind another tab), it stops reading every minute.
        verify(table.refreshing)
        t.panel.visible = false
        verify(!table.refreshing, "no reads while hidden")
        t.panel.visible = true
        verify(table.refreshing)
    }

    function test_alerts_show_and_clear() { // spec: APP-132
        const t = makePanel()
        answer(t.fake, "telemetry.snapshot", snapshot())
        const strip = named(t.panel, "alertsStrip")
        verify(strip, "the alerts strip")
        verify(!strip.visible, "no alerts, no strip")
        // A cleared alert is held a minute; shortened here.
        strip.holdMs = 300
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
        verifyShows(strip, "Cleared at", "the cleared alert says so while it is held")
        compare(strip.activeKinds(), ["throttle_thermal"])
        verifyShows(strip, "slowing down", "the other stays")
        tryVerify(() => !shows(strip, "88 °C"), 3000, "the cleared alert goes after its hold")
        // The next sample's alerts are the state.
        t.fake.emit("telemetry", "event", {kind: "sample", snapshot: snapshot({ts: "2026-10-28T12:00:09.000Z", alerts: []})})
        tryVerify(() => !strip.visible, 3000, "all cleared, no strip")
        t.fake.emit("telemetry", "event", {kind: "sample", snapshot: snapshot({ts: "2026-10-28T12:00:10.000Z",
            alerts: [{id: "gpu_throttle_power_brake", kind: "throttle_power_brake", value: 128, since: "2026-10-28T12:00:10.000Z"}]})})
        tryVerify(() => strip.visible, 2000)
        verifyShows(strip, "power brake", "an alert from a sample")
        // An alert event that overtook an older sample (the throttle holds samples, not alerts):
        // that sample's alerts don't clear it.
        t.fake.emit("telemetry", "event", {kind: "alert", alert: {id: "gpu_temperature", kind: "temperature",
                                                                 value: 86, since: "2026-10-28T12:00:20.000Z"}})
        t.fake.emit("telemetry", "event", {kind: "sample", snapshot: snapshot({ts: "2026-10-28T12:00:15.000Z",
            alerts: [{id: "gpu_throttle_power_brake", kind: "throttle_power_brake", value: 128, since: "2026-10-28T12:00:10.000Z"}]})})
        compare(strip.activeKinds().sort(), ["temperature", "throttle_power_brake"])
    }

    function banners(strip) {
        const out = []
        visitAll(strip, item => { if (String(item).startsWith("LBanner") && item.visible) out.push(item) })
        return out
    }

    function test_a_flapping_alert_keeps_one_banner_announced_once() { // spec: APP-132, APP-156
        const t = makePanel()
        answer(t.fake, "telemetry.snapshot", snapshot())
        const strip = named(t.panel, "alertsStrip")
        const raise = s => t.fake.emit("telemetry", "event", {kind: "alert", alert: {id: "gpu_throttle_thermal",
            kind: "throttle_thermal", value: 64, since: "2026-10-28T12:00:0" + s + ".000Z"}})
        const clear = () => t.fake.emit("telemetry", "event", {kind: "alert_cleared", id: "gpu_throttle_thermal"})
        // A GPU at its thermal limit: the bit flips sample to sample.
        raise(1)
        const first = banners(strip)[0]
        verify(first, "a banner")
        for (let s = 2; s < 7; ++s) {
            clear()
            raise(s)
        }
        clear()
        const now = banners(strip)
        compare(now.length, 1, "one banner for the kind")
        verify(now[0] === first, "the same banner, updated in place")
        compare(strip.announcements, 1, "announced once")
        verifyShows(strip, "Cleared at", "held, cleared")
        raise(8)
        verify(!shows(strip, "Cleared at"), "raised again in place")
        compare(strip.announcements, 1, "and not announced again within the minute")
        // A minute on (shortened here): the held banner goes, and the next raise is announced.
        strip.holdMs = 200
        strip.announceEveryMs = 200
        clear()
        tryVerify(() => !strip.visible, 3000, "the cleared alert goes after its hold")
        raise(9)
        compare(strip.announcements, 2)
    }

    function test_gpu_users_list_names_the_app_and_engines() { // spec: APP-127
        const t = makePanel()
        // An untracked process's name is untrusted: shown through Core.shownText (P4a-R8).
        const longName = "Night Racer " + "x".repeat(80) + ".exe"
        answer(t.fake, "telemetry.snapshot", snapshot({gpu_users: [
            {pid: 4100, name: "comfyui", vram_local_mib: 6144, sm_permille: 900, role: "comfyui"},
            {pid: 3300, name: longName, vram_local_mib: 2048},
            {pid: Hardware.pid, name: "app", vram_local_mib: 512, role: "app"},
            {pid: Hardware.pid + 1, name: "app", vram_local_mib: 256, role: "app"}
        ]}))
        const users = named(t.panel, "gpuUsers")
        verify(users, "the GPU users list")
        const rows = []
        visitAll(users, item => { if (item.objectName === "gpuUser" && item.visible) rows.push(item) })
        compare(rows.length, 4)
        verifyShows(rows[3], "LILITH app", "another LILITH app's row")
        verify(!shows(rows[3], "this app"), "isn't this app")
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
        verifyShows(parts, "rest of system not set", "the parts")
        verifyShows(named(t.panel, "systemPower"), "412 W (estimated)", "the whole system is an estimate")
        const chart = m => t.panel.chartTiles.find(c => c.metric === m)
        verify(/CPU power \(estimated\) chart/.test(LilShim.accessibleOf(chart("cpu.power")).name),
               LilShim.accessibleOf(chart("cpu.power")).name)
        verify(/System power \(estimated\) chart/.test(LilShim.accessibleOf(chart("system.power")).name),
               LilShim.accessibleOf(chart("system.power")).name)
        verifyShows(chart("cpu.power"), "CPU power (estimated)", "the CPU power chart")
        verify(!/estimated/.test(LilShim.accessibleOf(chart("gpu.power")).name), "the GPU power is measured")
        // A GPU power taken from its energy counter is an estimate.
        t.fake.emit("telemetry", "event", {kind: "sample", snapshot: Object.assign({}, s, {ts: "2026-10-28T12:00:00.500Z",
            system_parts: [{part: "gpu", mw: 285000, measured: "estimated"}, s.system_parts[1], s.system_parts[2]]})})
        verifyShows(named(t.panel, "gpuCard"), "285 W (estimated)", "an estimated GPU power")
        verify(/GPU power \(estimated\) chart/.test(LilShim.accessibleOf(chart("gpu.power")).name))
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

    // The visible Text items showing `text`; each must render it as plain text.
    function verifyLiteral(root, text, what) {
        const items = []
        visitAll(root, item => {
            if (item.visible && String(item).startsWith("QQuickText") && typeof item.text === "string"
                    && item.text.indexOf(text) >= 0)
                items.push(item)
        })
        verify(items.length > 0, what + " is shown: " + JSON.stringify(texts(root)))
        for (const item of items)
            compare(item.textFormat, Text.PlainText, what + " shows as plain text, never markup: " + item.text)
    }

    function test_markup_in_untrusted_text_shows_literally() { // spec: APP-127, APP-132
        const t = makePanel()
        // The core's error message: through shownText (cut here, at 200), as plain text.
        const message = "<u>bad</u> " + "y".repeat(300)
        verify(t.fake.respond(callsTo(t.fake, "telemetry.snapshot")[0].id,
                              {status: "error", error: {code: "x.failed", message: message}}))
        verifyLiteral(t.panel, "<u>bad</u>", "the error message")
        verify(!shows(t.panel, message), "the message is shown through shownText")
        // The GPU's name.
        t.fake.emit("telemetry", "event", {kind: "sample", snapshot: snapshot({
            gpu: Object.assign(snapshot().gpu, {name: "<b>MARK</b> GPU"})})})
        verifyLiteral(named(t.panel, "gpuCard"), "<b>MARK</b>", "the GPU's name")
        // An alert kind this build doesn't know.
        const kind = "<i>KIND</i>" + "z".repeat(60)
        t.fake.emit("telemetry", "event", {kind: "alert", alert: {id: "gpu_new", kind: kind, value: 1,
                                                                 since: "2026-10-28T12:00:05.000Z"}})
        verifyLiteral(named(t.panel, "alertsStrip"), "<i>KIND</i>", "an unknown alert kind")
        verify(!shows(named(t.panel, "alertsStrip"), kind), "the kind is shown through shownText")
    }

    function test_missing_energy_parts_show_as_not_measured() { // spec: APP-131
        const t = makePanel()
        answer(t.fake, "telemetry.snapshot", snapshot({gpu: undefined}))
        const calls = callsTo(t.fake, "telemetry.energy")
        const answerPeriod = (i, e) => t.fake.respond(calls[i].id, {status: "ok", structured: Object.assign(
            {from: calls[i].args.from, to: calls[i].args.to}, e)})
        // The session: no samples yet. Today: no GPU, the CPU measured. This month: the CPU
        // unknown. This year: only the rest of the system's baseline.
        answerPeriod(0, {gpu_mj: 0, cpu_mj: 0, system_mj: 0, measured: {gpu: false, cpu: false}})
        answerPeriod(1, {gpu_mj: 0, cpu_mj: 36000000, system_mj: 72000000, measured: {gpu: false, cpu: true}})
        answerPeriod(2, {gpu_mj: 360000000, cpu_mj: 0, system_mj: 500000000, measured: {gpu: true, cpu: false}})
        answerPeriod(3, {gpu_mj: 0, cpu_mj: 0, system_mj: 3600000, measured: {gpu: false, cpu: false}})
        const table = named(t.panel, "energyTable")
        const row = p => named(table, "energyRow:" + p)
        const cell = (p, c) => named(row(p), c).text
        for (const p of ["session", "day", "month", "year"])
            verify(!shows(row(p), "0.0 Wh"), p + ": what wasn't measured is never 0 Wh: " + JSON.stringify(texts(row(p))))
        compare([cell("session", "energySystem"), cell("session", "energyGpu"), cell("session", "energyCpu")],
                ["—", "—", "—"], "a period with no samples")
        verify(/no data yet/.test(LilShim.accessibleOf(row("session")).name), LilShim.accessibleOf(row("session")).name)
        compare(cell("day", "energyGpu"), "—", "no GPU")
        verify(/GPU not measured/.test(LilShim.accessibleOf(row("day")).name), LilShim.accessibleOf(row("day")).name)
        compare(cell("day", "energyCpu"), "10 Wh")
        compare(cell("month", "energyCpu"), "—", "an unknown CPU")
        compare(cell("year", "energySystem"), "1.0 Wh", "the rest of the system's estimate")
        compare([cell("year", "energyGpu"), cell("year", "energyCpu")], ["—", "—"])
    }

    function test_a_cleared_alert_isnt_raised_again_by_an_older_sample() { // spec: APP-132
        const t = makePanel()
        answer(t.fake, "telemetry.snapshot", snapshot())
        const strip = named(t.panel, "alertsStrip")
        const alert = {id: "gpu_temperature", kind: "temperature", value: 88, since: "2026-10-28T12:00:05.000Z"}
        // Each event with its own seq, as Core delivers them after the throttle.
        const send = (seq, data) => t.fake.coreEvent("telemetry", "event", seq, JSON.stringify(data))
        send(10, {kind: "sample", snapshot: snapshot({ts: "2026-10-28T12:00:05.000Z", alerts: [alert]})})
        compare(strip.activeKinds(), ["temperature"])
        // The clear (seq 12) overtakes an older sample the throttle held (seq 11), which still
        // carries the alert.
        send(12, {kind: "alert_cleared", id: "gpu_temperature"})
        send(11, {kind: "sample", snapshot: snapshot({ts: "2026-10-28T12:00:06.000Z", alerts: [alert]})})
        compare(strip.activeKinds(), [], "the older sample doesn't raise it again")
        verifyShows(strip, "Cleared at", "the banner stays cleared")
        // A sample after the clear that has it raises it again.
        send(13, {kind: "sample", snapshot: snapshot({ts: "2026-10-28T12:00:07.000Z", alerts: [alert]})})
        compare(strip.activeKinds(), ["temperature"])
        // After a resync the seqs start again: a sample with a low seq is current again.
        send(14, {kind: "alert_cleared", id: "gpu_temperature"})
        t.fake.coreEvent("telemetry", "resync_required", 0, "null")
        send(1, {kind: "sample", snapshot: snapshot({ts: "2026-10-28T12:00:09.000Z", alerts: [alert]})})
        compare(strip.activeKinds(), ["temperature"], "the old seqs don't hold after a resync")
    }

    function test_parts_no_source_can_measure_say_so() { // spec: APP-131
        // No NVIDIA GPU, no Energy Meter and no TDP, no rest of system set: nothing on this PC
        // can be measured, however many samples there are.
        const t = makePanel()
        answer(t.fake, "telemetry.snapshot", snapshot({gpu: undefined, system_power_mw: 0,
            system_parts: [{part: "gpu", mw: 0, measured: "unknown"}, {part: "cpu", mw: 0, measured: "unknown"},
                           {part: "rest", mw: 0, measured: "unknown"}]}))
        for (const c of callsTo(t.fake, "telemetry.energy"))
            t.fake.respond(c.id, {status: "ok", structured: {from: c.args.from, to: c.args.to,
                gpu_mj: 0, cpu_mj: 0, system_mj: 0, measured: {gpu: false, cpu: false}}})
        const table = named(t.panel, "energyTable")
        for (const p of ["session", "day", "month", "year"]) {
            const name = LilShim.accessibleOf(named(table, "energyRow:" + p)).name
            verify(/not measured on this PC/.test(name), p + ": " + name)
            verify(!/no data yet/.test(name), p + ": " + name)
        }
        // Only the GPU can't be measured here: the CPU is, and today's row says which.
        const u = makePanel()
        answer(u.fake, "telemetry.snapshot", snapshot({gpu: undefined,
            system_parts: [{part: "gpu", mw: 0, measured: "unknown"}, {part: "cpu", mw: 64000, measured: "measured"},
                           {part: "rest", mw: 63000, measured: "estimated"}]}))
        const calls = callsTo(u.fake, "telemetry.energy")
        u.fake.respond(calls[1].id, {status: "ok", structured: {from: calls[1].args.from, to: calls[1].args.to,
            gpu_mj: 0, cpu_mj: 36000000, system_mj: 72000000, measured: {gpu: false, cpu: true}}})
        const day = LilShim.accessibleOf(named(named(u.panel, "energyTable"), "energyRow:day")).name
        verify(/GPU not measured on this PC/.test(day), day)
        verify(/CPU 10 Wh/.test(day), day)
    }

    function test_a_hole_breaks_the_line_and_a_stopped_series_says_so() { // spec: APP-129
        const t = makePanel()
        answer(t.fake, "telemetry.snapshot", snapshot())
        const args = callsTo(t.fake, "telemetry.series")[0].args
        const to = Date.parse(args.to)
        // 15 minutes with a 10-minute hole: the core was stopped.
        const points = []
        for (let i = 0; i < 898; ++i) {
            if (i < 200 || i >= 800)
                points.push([new Date(to - (897 - i) * 1000).toISOString(), 40, 50, 60])
        }
        answer(t.fake, "telemetry.series", {series: [{metric: "gpu.util", points: points},
                                                     {metric: "cpu.util", points: points}]})
        const chart = m => t.panel.chartTiles.find(c => c.metric === m)
        compare(chart("gpu.util").series.upperPaths.length, 2, "the hole breaks the line")
        // The GPU is lost; the CPU goes on.
        for (let k = 1; k <= 30; ++k)
            t.fake.emit("telemetry", "event", {kind: "sample", snapshot: snapshot({ts: new Date(to + k * 1000).toISOString(),
                                                                                  gpu: undefined})})
        const gpu = chart("gpu.util")
        verify(gpu.series.stale, "the GPU's chart has stopped")
        verify(!chart("cpu.util").series.stale, "the CPU's goes on")
        const lines = gpu.series.upperPaths
        const right = lines[lines.length - 1][lines[lines.length - 1].length - 1].x
        verify(right < gpu.plotWidth - 5, "its right edge is empty: " + right + " of " + gpu.plotWidth)
        const since = "No data since " + Qt.formatTime(new Date(to), "HH:mm")
        verifyShows(gpu, since, "the stopped chart")
        verify(LilShim.accessibleOf(gpu).name.indexOf(since) >= 0, LilShim.accessibleOf(gpu).name)
        verify(!shows(chart("cpu.util"), "No data since"))
    }

    function test_reloads_on_resync_a_snapshot_and_a_first_load_that_failed() { // spec: APP-127
        const t = makePanel()
        const count = tool => callsTo(t.fake, tool).length
        // A first load that failed (the session wasn't connected yet): `ready` loads again.
        verify(t.fake.respond(callsTo(t.fake, "telemetry.snapshot")[0].id,
                              {status: "error", error: {code: "core.unreachable", message: "not connected"}}))
        t.fake.connection = "connecting"
        t.fake.connection = "ready"
        compare(count("telemetry.snapshot"), 2, "loaded again once ready")
        answer(t.fake, "telemetry.snapshot", snapshot())
        // Loaded: a later `ready` loads nothing.
        t.fake.connection = "reconnecting"
        t.fake.connection = "ready"
        compare(count("telemetry.snapshot"), 2, "no reload on ready")
        // resync_required: everything again.
        const series = count("telemetry.series"), energy = count("telemetry.energy")
        t.fake.emit("telemetry", "resync_required", null)
        compare(count("telemetry.snapshot"), 3)
        compare(count("telemetry.series"), series + 1)
        compare(count("telemetry.energy"), energy + 4)
        // A snapshot of the topic: its state, then the series and the energy again.
        t.fake.emit("telemetry", "snapshot", snapshot({ts: "2026-10-28T12:05:00.000Z",
                                                      gpu: Object.assign(snapshot().gpu, {util_pct: 12})}))
        verifyShows(named(t.panel, "gpuCard"), "12 %", "the snapshot's state")
        compare(count("telemetry.series"), series + 2)
        compare(count("telemetry.energy"), energy + 8)
        // A window change gives up the series call still open: its answer would be the old window.
        const open = callsTo(t.fake, "telemetry.series").slice(-1)[0]
        t.panel.windowS = 3600
        verify(t.fake.cancelled.indexOf(open.id) >= 0, "the old window's call is cancelled")
        compare(callsTo(t.fake, "telemetry.series").slice(-1)[0].args.resolution, "1s")
    }

    function test_float_and_redock_keeps_hardware_state() { // spec: ARCH-116
        const fake = createTemporaryObject(fakeComponent, tc)
        const shell = createTemporaryObject(shellComponent, tc, {core: fake, stateDir: ""})
        shell.visible = true
        tryVerify(() => shell.layoutSettled, 5000, "the docks are laid out")
        tryVerify(() => shell.docks.hardware.content && shell.docks.hardware.content.width > 0, 5000)
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
