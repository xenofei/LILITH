// The Diagnostics and Backups panels (ARCH-121, ARCH-126, ARCH-127, APP-133, APP-134, APP-136,
// CORE-164, QA-083, ARCH-116) on FakeCore's scripted answers: versions, engines and their launch
// profile, the queue, errors by code, the event log, the model inventory, the notification path,
// the status lines, the bundle's export (into this process's temporary folder only, with a fake
// shim: no window of Explorer's opens), floating and re-docking, and the accessibility walk.
import QtQuick
import QtTest
import Lilith.Core
import Lilith.Shim
import Lilith.UiKit
import "../qml/panels/diagnostics/NotificationText.js" as NotificationText

TestCase {
    id: tc
    name: "Diagnostics"
    when: windowShown
    width: 200
    height: 100

    Component {
        id: fakeComponent
        FakeCore { project: "sketchbook" }
    }

    // What the panel reaches the shell and Explorer through, recorded.
    Component {
        id: fakeShimComponent
        QtObject {
            property var shown: []
            property var versions: ({qt: "6.12.0", webEngine: "6.140.0", kddockwidgets: "2.5.0"})
            property bool accepts: true
            function showInExplorer(path) {
                shown = shown.concat([path])
                return accepts
            }
            function componentVersions() { return versions }
        }
    }

    Component {
        id: fakeShellComponent
        QtObject {
            property var opened: []
            function openPanelOn(id, method, arg, focus) {
                opened = opened.concat([{id: id, method: method, arg: arg, focus: focus}])
            }
        }
    }

    Component {
        id: logComponent
        EventLogModel {}
    }

    Component {
        id: bundleExportComponent
        BundleExport {}
    }

    // The panel alone in a window of the shell's size (a TestCase itself isn't visible).
    Component {
        id: panelComponent
        Window {
            id: host
            required property var core
            property var shim: null
            property var shell: null
            property var eventLog: null
            readonly property alias panel: diagnostics
            width: 1280
            height: 800
            visible: true
            color: Theme.bg

            DiagnosticsPanel {
                id: diagnostics
                anchors.fill: parent
                core: host.core
                title: "Diagnostics"
                shim: host.shim
                shell: host.shell
                eventLog: host.eventLog
            }
        }
    }

    Component {
        id: backupsComponent
        Window {
            id: host
            required property var core
            readonly property alias panel: backups
            width: 800
            height: 400
            visible: true
            BackupsPanel {
                id: backups
                anchors.fill: parent
                core: host.core
                title: "Backups"
            }
        }
    }

    Component {
        id: shellComponent
        ShellWindow {}
    }

    A11yWalk { id: walk }

    property var answered: ({})

    // The shell of the test before deletes its docks later (KDDockWidgets' names are
    // process-wide): let that run first.
    function init() {
        answered = {}
        wait(100)
    }

    // A FakeCore that outlives the panel made after it (never destroyed before the process ends).
    function makeCore() {
        return fakeComponent.createObject(tc)
    }

    function makeShim() {
        return fakeShimComponent.createObject(tc)
    }

    function makePanel(core, props) {
        const host = createTemporaryObject(panelComponent, tc, Object.assign({core: core, shim: makeShim()}, props || {}))
        verify(host && host.panel, "DiagnosticsPanel created")
        return host.panel
    }

    function ok(structured) {
        return {status: "ok", structured: structured, artifacts: [], summary_text: ""}
    }

    function failed(error) {
        return {status: "error", error: Object.assign({category: "user_fixable", trace_id: "t_0000000000000001", retry: "after_resolve"}, error)}
    }

    function isTool(name) {
        return c => c.name === name
    }

    // The call matching `pred` that isn't answered yet, waited for.
    function nextCall(core, pred) {
        let call = null
        tryVerify(() => (call = core.calls.find(c => pred(c) && !answered[c.id]) || null) !== null, 5000)
        return call
    }

    function answer(core, pred, outcome) {
        const call = nextCall(core, pred)
        answered[call.id] = true
        verify(core.respond(call.id, outcome), "answered " + call.name)
        return call
    }

    function comfy(over) {
        return Object.assign({
            engine: "comfyui", class: "owned", state: "ready", url: "http://127.0.0.1:8188", pid: 4242,
            version: "0.42.1", listen: "loopback", outside_jobs: {running: 0, queued: 0},
            restart_needed: false, inventory: {models: 1234, refreshed: "2026-10-30T10:00:00Z"},
            launch_profile: {reserve_vram_gb: "1.5", preview_method: "taesd", manager: false, dynamic_vram: true,
                             flags: ["--reserve-vram", "1.5", "--preview-method", "taesd"]},
            asset_catalogue: "off"
        }, over || {})
    }

    function desktop(over) {
        return Object.assign({
            notifications: {job_finished: true, job_failed: true, confirmation_waiting: true, cap_alert: true,
                            director_decision: true, agent_activity: true},
            quiet_while_fullscreen: true, fullscreen_autopause: true,
            keep_awake_gpu: true, keep_awake_available: true,
            notification_path: {toasts: "ready", shortcut: "ok", handlers: "ok"}
        }, over || {})
    }

    function snapshot(over) {
        return Object.assign({
            ts: "2026-10-30T10:00:00Z",
            gpu: {name: "Test GPU", driver: "999.1", util_pct: 40, power_mw: 120000, temp_c: 61, sm_clock_mhz: 2000,
                  mem_clock_mhz: 9000, throttle: [], vram_total_mib: 16384, vram_used_mib: 9728, measured: true},
            processes: [], cpu: {util_permille: 125, per_core_permille: [], package_power_mw: 0, measured: false},
            ram: {total_mib: 65536, used_mib: 32768}, disks: [], sizes: {}, net: [], system_power_mw: 240000,
            system_parts: [{part: "gpu", mw: 120000, measured: "measured"}, {part: "cpu", mw: 0, measured: "unknown"},
                           {part: "rest", mw: 0, measured: "unknown"}],
            alerts: [], gpu_users: []
        }, over || {})
    }

    // What `telemetry.snapshot` answers before the first sample (Snapshot::empty).
    function emptySnapshot() {
        return {ts: "2026-10-30T10:00:00Z", processes: [],
                cpu: {util_permille: 0, per_core_permille: [], package_power_mw: 0, measured: false},
                ram: {total_mib: 0, used_mib: 0}, disks: [], sizes: {}, net: [], system_power_mw: 0,
                system_parts: [{part: "gpu", mw: 0, measured: "unknown"}, {part: "cpu", mw: 0, measured: "unknown"},
                               {part: "rest", mw: 0, measured: "unknown"}],
                alerts: [], gpu_users: []}
    }

    // A capture as `engines.environment` holds it.
    function capture(at) {
        return {system_stats: {system: {comfyui_version: "0.42.1"}, devices: []}, driver: "999.1", comfyui: "abc1234",
                nodes: [], captured: at}
    }

    function report(over) {
        return Object.assign({root: "D:\\comfy", refreshed: "2026-10-30T10:00:00Z", registry: "ok", models: [], missing: []}, over || {})
    }

    // Answers the panel's first reads; `over` replaces a tool's answer by its name.
    function load(core, over) {
        const given = over || {}
        const of = (name, fallback) => name in given ? given[name] : ok(fallback)
        answer(core, isTool("engines.status"), of("engines.status", {engines: [comfy()]}))
        answer(core, isTool("engines.environment"), of("engines.environment", {captures: [], changes: []}))
        answer(core, isTool("diagnostics.recent"), of("diagnostics.recent", {errors: []}))
        answer(core, isTool("desktop.get"), of("desktop.get", desktop()))
        answer(core, isTool("models.report"), of("models.report", report()))
        answer(core, isTool("telemetry.snapshot"), of("telemetry.snapshot", snapshot()))
        answer(core, c => c.name === "jobs.list" && c.args.status === "running",
               of("jobs.running", {jobs: [], next_cursor: null, lanes: []}))
        answer(core, c => c.name === "jobs.list" && c.args.status === "queued",
               of("jobs.queued", {jobs: [], next_cursor: null, lanes: []}))
    }

    // The visible texts under `item`.
    function texts(item) {
        const out = []
        const walkItem = i => {
            if (!i.visible)
                return
            // A Text, or a component built on one (lineCount is Text's own).
            if (typeof i.text === "string" && typeof i.lineCount === "number")
                out.push(i.text)
            for (let n = 0; n < i.children.length; ++n)
                walkItem(i.children[n])
        }
        walkItem(item)
        return out
    }

    function shows(item, s) {
        return texts(item).some(t => t.indexOf(s) >= 0)
    }

    function find(item, pred) {
        if (pred(item))
            return item
        for (let n = 0; n < item.children.length; ++n) {
            const found = find(item.children[n], pred)
            if (found)
                return found
        }
        return null
    }

    function button(item, label) {
        return find(item, i => i.visible && typeof i.click === "function" && i.text === label)
    }

    function callsOf(core, name) {
        return core.calls.filter(c => c.name === name)
    }

    // --- Versions and engines -------------------------------------------------------------------

    function test_versions_and_engine_health() { // spec: ARCH-121, APP-134
        const core = makeCore()
        const panel = makePanel(core)
        load(core)
        tryVerify(() => shows(panel.engines, "ComfyUI 0.42.1 · owned · ready"), 5000)
        verify(shows(panel.versions, "Core 0.0.0+fakecore"), texts(panel.versions).join(" | "))
        verify(shows(panel.versions, "This app 0.0.0+fake"))
        verify(shows(panel.versions, "Qt 6.12.0"))
        verify(shows(panel.versions, "Qt WebEngine 6.140.0"))
        verify(shows(panel.versions, "KDDockWidgets 2.5.0"))
        verify(shows(panel.versions, "ComfyUI 0.42.1"))
        verify(shows(panel.versions, "GPU driver 999.1"))
        verify(shows(panel.engines, "GPU memory 9.5 / 16 GiB"), texts(panel.engines).join(" | "))
        verify(shows(panel.engines, "1,234 model files"))

        // A full-state event replaces the engine, an attached one says so and has no profile.
        core.emit("engines", "event", {kind: "status", engine: comfy({class: "attached", launch_profile: null, version: "0.43"})})
        tryVerify(() => shows(panel.engines, "ComfyUI 0.43 · attached · ready"), 5000)
        verify(!shows(panel.engines, "owned"))
        // Applying the same state again changes nothing.
        core.emit("engines", "event", {kind: "status", engine: comfy({class: "attached", launch_profile: null, version: "0.43"})})
        compare(panel.engines.engines.length, 1)

        // No version known, and no engine running: said, never an empty line.
        core.emit("engines", "event", {kind: "status", engine: comfy({class: "none", state: "stopped", version: undefined})})
        tryVerify(() => shows(panel.engines, "ComfyUI (version not known) · stopped"), 5000)
        verify(shows(panel.versions, "ComfyUI not known"), texts(panel.versions).join(" | "))
    }

    function test_a_version_that_cant_be_read_says_not_known() { // spec: ARCH-121
        const core = makeCore()
        core.coreBuildId = ""
        const shim = makeShim()
        shim.versions = {qt: "6.12.0"}
        const panel = makePanel(core, {shim: shim})
        load(core, {"engines.status": ok({engines: []}), "telemetry.snapshot": ok(snapshot({gpu: undefined}))})
        tryVerify(() => shows(panel.versions, "ComfyUI not known"), 5000, texts(panel.versions).join(" | "))
        for (const line of ["Core not known", "Qt WebEngine not known", "KDDockWidgets not known", "GPU driver not known"])
            verify(shows(panel.versions, line), line)
        verify(!texts(panel.versions).some(t => t.trim() === ""), "no empty line")
        verify(shows(panel.engines, "No GPU reading"), texts(panel.engines).join(" | "))
    }

    function test_the_real_shim_gives_the_versions() { // spec: ARCH-121, APP-137
        const v = LilShim.componentVersions()
        verify(/^6\.12\./.test(v.qt), v.qt)
        verify(/^\d+\.\d+/.test(v.webEngine), v.webEngine)
        verify(/^\d+\.\d+/.test(v.kddockwidgets), v.kddockwidgets)
        verify(!LilShim.isModuleLoaded("Qt6WebEngineCore.dll"), "read without loading WebEngine")
    }

    function test_launch_profile_flags_and_catalogue_are_shown() { // spec: ARCH-126, ARCH-065
        const core = makeCore()
        const panel = makePanel(core)
        load(core)
        tryVerify(() => shows(panel.engines, "--reserve-vram 1.5 --preview-method taesd"), 5000)
        verify(shows(panel.engines, "keeps 1.5 GB of GPU memory free"), texts(panel.engines).join(" | "))
        verify(shows(panel.engines, "previews taesd"))
        verify(shows(panel.engines, "Manager off"))
        verify(shows(panel.engines, "dynamic VRAM allowed"))
        verify(shows(panel.engines, "ComfyUI's asset catalogue is off"))
        core.emit("engines", "event", {kind: "status", engine: comfy({asset_catalogue: "on", class: "attached", launch_profile: null})})
        tryVerify(() => shows(panel.engines, "ComfyUI's asset catalogue is on"), 5000)
        verify(!shows(panel.engines, "--reserve-vram"))
        verify(shows(panel.engines, "An attached ComfyUI keeps its own launch settings"))
        core.emit("engines", "event", {kind: "status", engine: comfy({asset_catalogue: "unknown"})})
        tryVerify(() => shows(panel.engines, "ComfyUI's asset catalogue is unknown"), 5000)
    }

    function test_the_environments_changes_are_listed() { // spec: CORE-130, CORE-166
        const core = makeCore()
        const panel = makePanel(core)
        load(core, {"engines.environment": ok({captures: [capture("2026-10-30T10:00:00Z"), capture("2026-10-29T10:00:00Z")],
            changes: [{what: "driver", from: "998", to: "999.1", change: "changed"},
                      {what: "node", name: "<b>pack</b>", to: "1.2 (abc)", change: "added"},
                      {what: "comfyui", from: "aaa", change: "removed"}]})})
        tryVerify(() => shows(panel.engines, "GPU driver: 998 → 999.1"), 5000, texts(panel.engines).join(" | "))
        verify(shows(panel.engines, "<b>pack</b>: added, 1.2 (abc)"), texts(panel.engines).join(" | "))
        verify(shows(panel.engines, "ComfyUI: removed, was aaa"))
    }

    function test_the_queue_is_shown() { // spec: ARCH-121
        const core = makeCore()
        const panel = makePanel(core)
        const job = n => ({job_id: "j_" + n, status: "queued"})
        load(core, {"jobs.running": ok({jobs: [job(1)], next_cursor: null,
                                        lanes: [{lane: "gpu-heavy", paused: false, reasons: [], held: 0, restart_held: 0, withdrawn: 0},
                                                {lane: "cpu", paused: false, reasons: [], held: 0, restart_held: 0, withdrawn: 0}]}),
                    "jobs.queued": ok({jobs: Array.from({length: 100}, (x, n) => job(n)), next_cursor: "more", lanes: []})})
        tryVerify(() => shows(panel.queue, "1 running, 100 or more queued"), 5000, texts(panel.queue).join(" | "))
        verify(shows(panel.queue, "GPU work: not paused"), texts(panel.queue).join(" | "))
        // A queue event is the lane's full state.
        core.emit("jobs", "event", {kind: "queue", state: {lane: "gpu-heavy", paused: true, reasons: ["manual", "x<i>y"], held: 2, restart_held: 0, withdrawn: 0}})
        tryVerify(() => shows(panel.queue, "GPU work: paused by you and by x<i>y, 2 held"), 5000, texts(panel.queue).join(" | "))
        // A job's change reads the counts again, at most once a second.
        const reads = callsOf(core, "jobs.list").length
        core.emit("jobs", "event", {kind: "job", job: job(1)})
        core.emit("jobs", "event", {kind: "job", job: job(2)})
        core.emit("jobs", "event", {kind: "job", job: job(3)})
        answer(core, c => c.name === "jobs.list" && c.args.status === "running", ok({jobs: [], next_cursor: null, lanes: []}))
        answer(core, c => c.name === "jobs.list" && c.args.status === "queued", ok({jobs: [job(1)], next_cursor: null, lanes: []}))
        tryVerify(() => shows(panel.queue, "0 running, 1 queued"), 5000, texts(panel.queue).join(" | "))
        compare(callsOf(core, "jobs.list").length, reads + 2, "one read for the burst so far")
    }

    function test_a_read_older_than_an_engine_event_doesnt_overwrite_it() { // spec: ARCH-100, APP-033
        const core = makeCore()
        const panel = makePanel(core)
        // The read is open when the event comes; its answer is older than the event.
        const open = nextCall(core, isTool("engines.status"))
        core.emit("engines", "event", {kind: "status", engine: comfy({state: "ready"})})
        tryVerify(() => shows(panel.engines, "· owned · ready"), 5000)
        answered[open.id] = true
        core.respond(open.id, ok({engines: [comfy({state: "starting"})]}))
        verify(shows(panel.engines, "· owned · ready"), texts(panel.engines).join(" | "))
        verify(!shows(panel.engines, "starting"))
        // A fresh read follows, and its answer is the state.
        answer(core, isTool("engines.status"), ok({engines: [comfy({state: "ready", version: "0.50"})]}))
        tryVerify(() => shows(panel.engines, "ComfyUI 0.50 · owned · ready"), 5000)
    }

    function test_a_read_older_than_a_queue_event_doesnt_overwrite_it() { // spec: ARCH-100, APP-033
        const core = makeCore()
        const panel = makePanel(core)
        const lane = paused => ({lane: "gpu-heavy", paused: paused, reasons: paused ? ["manual"] : [], held: 0, restart_held: 0, withdrawn: 0})
        const running = nextCall(core, c => c.name === "jobs.list" && c.args.status === "running")
        const queued = nextCall(core, c => c.name === "jobs.list" && c.args.status === "queued")
        core.emit("jobs", "event", {kind: "queue", state: lane(true)})
        tryVerify(() => shows(panel.queue, "GPU work: paused by you"), 5000, texts(panel.queue).join(" | "))
        for (const c of [running, queued]) {
            answered[c.id] = true
            core.respond(c.id, ok({jobs: [{job_id: "j_1", status: c.args.status}], next_cursor: null, lanes: [lane(false)]}))
        }
        verify(shows(panel.queue, "GPU work: paused by you"), texts(panel.queue).join(" | "))
        // The counts of that answer still count; a fresh read follows.
        verify(shows(panel.queue, "1 running, 1 queued"), texts(panel.queue).join(" | "))
        answer(core, c => c.name === "jobs.list" && c.args.status === "running", ok({jobs: [], next_cursor: null, lanes: [lane(true)]}))
        answer(core, c => c.name === "jobs.list" && c.args.status === "queued", ok({jobs: [], next_cursor: null, lanes: [lane(true)]}))
        tryVerify(() => shows(panel.queue, "0 running, 0 queued"), 5000, texts(panel.queue).join(" | "))
        verify(shows(panel.queue, "GPU work: paused by you"))
    }

    function test_telemetry_says_so_before_the_first_sample() { // spec: APP-134
        const core = makeCore()
        const panel = makePanel(core)
        load(core, {"telemetry.snapshot": ok(emptySnapshot())})
        tryVerify(() => shows(panel.telemetry, "No hardware reading yet"), 5000, texts(panel.telemetry).join(" | "))
        for (const zero of ["CPU 0", "RAM 0", "No hardware alerts", "No GPU reading"])
            verify(!shows(panel.telemetry, zero), zero)
        verify(shows(panel.engines, "GPU memory: not read yet"), texts(panel.engines).join(" | "))
        verify(!shows(panel.engines, "No GPU reading"))
        core.emit("telemetry", "event", {kind: "sample", snapshot: snapshot({ts: "2026-10-30T10:00:05Z"})})
        tryVerify(() => shows(panel.telemetry, "Test GPU: 40 % load"), 5000)
    }

    function test_telemetry_summary_follows_the_samples() { // spec: ARCH-121, APP-134
        const core = makeCore()
        const panel = makePanel(core)
        load(core)
        tryVerify(() => shows(panel.telemetry, "Test GPU: 40 % load, 61 °C"), 5000, texts(panel.telemetry).join(" | "))
        verify(shows(panel.telemetry, "CPU 13 % load"), texts(panel.telemetry).join(" | "))
        verify(shows(panel.telemetry, "RAM 32 / 64 GiB"))
        verify(shows(panel.telemetry, "240 W, estimated"))
        verify(shows(panel.telemetry, "No hardware alerts"))
        const later = snapshot({ts: "2026-10-30T10:00:05Z", gpu: Object.assign(snapshot().gpu, {util_pct: 90, vram_used_mib: 2048}),
                                alerts: [{id: "gpu_temperature", kind: "temperature", value: 88, since: "2026-10-30T10:00:05.000Z"}]})
        core.emit("telemetry", "event", {kind: "sample", snapshot: later})
        tryVerify(() => shows(panel.telemetry, "Test GPU: 90 % load"), 5000)
        verify(shows(panel.telemetry, "1 hardware alert"))
        verify(shows(panel.engines, "GPU memory 2 / 16 GiB"), "the engines card follows the same sample")
        // An older sample (events can overtake one another) changes nothing.
        core.emit("telemetry", "event", {kind: "sample", snapshot: snapshot()})
        verify(shows(panel.telemetry, "Test GPU: 90 % load"))
    }

    // --- Errors ---------------------------------------------------------------------------------

    function test_recent_errors_are_grouped_by_code() { // spec: ARCH-121
        const core = makeCore()
        const shell = fakeShellComponent.createObject(tc)
        const panel = makePanel(core, {shell: shell})
        load(core, {"diagnostics.recent": ok({errors: [
            {code: "comfyui.out_of_memory", count: 1234, first_at: "2026-10-30T09:00:00Z", last_at: "2026-10-30T10:00:00Z",
             last_message: "ComfyUI ran out of GPU memory.", last_tool: "comfy.prompt", last_job_id: "j_0123"},
            {code: "store.io", count: 1, first_at: "2026-10-30T08:00:00Z", last_at: "2026-10-30T08:00:00Z",
             last_message: "<img src=\"http://x/y\">%USERPROFILE%\\a.png", last_tool: "asset.get"}]})})
        tryVerify(() => panel.errors.rows.length === 2, 5000)
        verify(shows(panel.errors, "comfyui.out_of_memory · 1,234 times"), texts(panel.errors).join(" | "))
        verify(shows(panel.errors, "store.io · once"), texts(panel.errors).join(" | "))
        verify(shows(panel.errors, "ComfyUI ran out of GPU memory."))
        verify(shows(panel.errors, "<img src=\"http://x/y\">"), "shown as its text")
        verify(shows(panel.errors, "from asset.get"))
        // Only the row that names a job offers it, and it opens Jobs on that job through the shell.
        const show = button(panel.errors, "Show the job")
        verify(show, "the job's row has its button")
        show.click()
        compare(shell.opened.length, 1)
        compare(shell.opened[0].id, "jobs")
        compare(shell.opened[0].method, "showJob")
        compare(shell.opened[0].arg, "j_0123")
        compare(shell.opened[0].focus, true)

        // An event reads the list again, a burst once a second at most.
        const reads = callsOf(core, "diagnostics.recent").length
        core.emit("diagnostics", "event", {kind: "error", code: "x", at: "2026-10-30T10:00:00Z"})
        core.emit("diagnostics", "event", {kind: "error", code: "x", at: "2026-10-30T10:00:00Z"})
        answer(core, isTool("diagnostics.recent"), ok({errors: []}))
        tryVerify(() => shows(panel.errors, "No errors since the core started"), 5000)
        compare(callsOf(core, "diagnostics.recent").length, reads + 1)
    }

    // --- The event log --------------------------------------------------------------------------

    function test_event_log_keeps_the_newest_1000() { // spec: ARCH-121, APP-134
        const core = makeCore()
        const panel = makePanel(core)
        load(core)
        compare(panel.log.view.count, 0)
        verify(shows(panel.log, "No events yet"))
        for (let i = 0; i < 1003; ++i)
            core.emit("things", "event", {n: i})
        core.emit("<b>topic</b>", "resync_required", {text: "<img src=\"http://x/y\">"})
        compare(panel.log.view.count, 1000)
        tryVerify(() => shows(panel.log, "1,000 events"), 5000, texts(panel.log).join(" | "))
        verify(panel.log.model.get(0).text.indexOf('"n":4') >= 0, panel.log.model.get(0).text)
        // It follows the newest, shown as plain text.
        tryVerify(() => panel.log.view.atYEnd, 5000, "at the newest")
        tryVerify(() => shows(panel.log, "<b>topic</b> resync_required"), 5000, texts(panel.log).slice(0, 4).join(" | "))
        verify(shows(panel.log, "<img src="))
        compare(TestFiles.networkRequests(), 0, "markup in an event loads nothing")
        // A reader who scrolled back isn't moved.
        panel.log.view.positionViewAtBeginning()
        panel.log.view.movementEnded()
        verify(!panel.log.following)
        core.emit("things", "event", {n: "x"})
        wait(50)
        verify(lastRowShown(panel.log.view) < 900, "stays where you read: " + lastRowShown(panel.log.view))
    }

    // The last row on screen, read from the view rather than `atYEnd`, which goes stale at the cap.
    function lastRowShown(view) {
        return view.indexAt(20, view.contentY + view.height - 2)
    }

    function test_event_log_follows_the_newest_past_the_cap() { // spec: ARCH-121, APP-134
        const core = makeCore()
        const panel = makePanel(core)
        load(core)
        let n = 0
        for (let batch = 0; batch < 26; ++batch) {
            for (let i = 0; i < 100; ++i)
                core.emit("things", "event", {n: n++})
            wait(20)
            const count = panel.log.view.count
            tryVerify(() => lastRowShown(panel.log.view) >= count - 2, 2000,
                      "after " + n + " events the last row on screen is " + lastRowShown(panel.log.view))
        }
        compare(panel.log.view.count, 1000)
        verify(panel.log.following)
        const newest = panel.log.view.itemAtIndex(999)
        verify(newest, "the newest row is made")
        verify(shows(newest, '"n":' + (n - 1)), texts(newest).join(" | "))
    }

    function test_the_keyboard_moves_from_the_newest_row() { // spec: APP-156
        const core = makeCore()
        const panel = makePanel(core)
        load(core)
        for (let i = 0; i < 200; ++i)
            core.emit("things", "event", {n: i})
        const view = panel.log.view
        tryVerify(() => lastRowShown(view) >= 198, 5000)
        panel.Window.window.requestActivate()
        tryVerify(() => panel.Window.window.active, 5000)
        view.forceActiveFocus(Qt.TabFocusReason)
        const end = view.contentY
        compare(view.currentIndex, 199, "the current row is the newest")
        keyClick(Qt.Key_Down)
        wait(50)
        verify(view.contentY >= end - 1, "stays at the end: " + view.contentY + " from " + end)
        verify(panel.log.following)
        keyClick(Qt.Key_Up)
        tryVerify(() => view.currentIndex === 198, 2000, "one row back: " + view.currentIndex)
        verify(view.contentY >= end - 40, "one row, not the top: " + view.contentY + " from " + end)
        verify(!panel.log.following, "reading back")
        core.emit("things", "event", {n: "x"})
        wait(50)
        compare(view.currentIndex, 198, "a new event leaves the reader's row")
        verify(!panel.log.following)
        keyClick(Qt.Key_Down)
        keyClick(Qt.Key_Down)
        tryVerify(() => panel.log.following, 2000, "back at the newest, it follows again")
    }

    function test_event_log_says_what_it_leaves_out() { // spec: APP-134
        const core = makeCore()
        const panel = makePanel(core)
        load(core)
        verify(shows(panel.log, "Telemetry samples, job progress and preview frames aren't kept here"),
               texts(panel.log).join(" | "))
        core.emit("diagnostics", "event", {kind: "error", code: "store.io", tool: "asset.get", at: "2026-10-30T10:00:00Z"})
        for (let i = 0; i < 1000; ++i)
            core.emit("telemetry", "event", {kind: "sample", snapshot: snapshot()})
        tryVerify(() => shows(panel.log, "1 event"), 5000, texts(panel.log).join(" | "))
        compare(panel.log.view.count, 1, "1,000 samples push nothing out")
        verify(panel.log.model.get(0).text.indexOf("store.io") >= 0)
    }

    function test_event_log_takes_the_shells_model_when_it_has_one() { // spec: ARCH-121
        const core = makeCore()
        const given = createTemporaryObject(logComponent, tc, {core: core})
        core.emit("before", "event", {early: true})
        const panel = makePanel(core, {eventLog: given})
        load(core)
        compare(panel.log.model, given)
        tryVerify(() => panel.log.view.count === 1, 5000, "the events from before the panel")
        core.emit("after", "event", {})
        compare(panel.log.view.count, 2)
        compare(panel.ownLog.rowCount(), 0, "its own model stays out of the way")

        // With none, the panel's own model follows the core.
        const alone = makePanel(makeCore())
        verify(alone.log.model === alone.ownLog)
        alone.core.emit("things", "event", {})
        compare(alone.log.view.count, 1)
    }

    // --- The bundle -----------------------------------------------------------------------------

    function bundleOf(dir) {
        // As the core names one.
        const source = dir + "/lilith-diagnostics-20261030T100000Z.zip"
        verify(TestFiles.write(source, "the bundle's bytes"))
        return {path: source, bytes: 18, files: ["manifest.json", "versions.json"], redactions: 12345}
    }

    function test_export_calls_diagnostics_bundle_and_shows_the_path() { // spec: ARCH-121, ARCH-122, APP-134
        const core = makeCore()
        const shim = makeShim()
        const panel = makePanel(core, {shim: shim})
        load(core)
        const dir = TestFiles.newDir("bundle-saved")
        const source = bundleOf(dir)
        verify(panel.exportButton.visible)
        compare(panel.exportButton.text, "Export diagnostics bundle")
        verify(!panel.explorerButton.visible, "nothing to show yet")
        panel.exportTo("file:///" + dir + "/picked.zip")
        verify(!panel.exportButton.enabled, "one at a time")
        const call = answer(core, isTool("diagnostics.bundle"), ok(source))
        compare(call.args, {}, "no job, and no path named by the panel")
        tryVerify(() => !panel.exporter.busy && panel.exportButton.enabled, 5000)
        compare(TestFiles.read(dir + "/picked.zip"), "the bundle's bytes")
        compare(panel.banner.kind, "ok")
        verify(panel.banner.text.indexOf(dir + "/picked.zip") >= 0, panel.banner.text)
        verify(panel.banner.text.indexOf("12,345") >= 0, panel.banner.text)
        verify(panel.banner.text.indexOf("leaves this PC only when you hand it on") >= 0, panel.banner.text)

        verify(panel.explorerButton.visible)
        compare(panel.explorerButton.text, "Show in Explorer")
        panel.explorerButton.click()
        compare(shim.shown, [dir + "/picked.zip"])
    }

    function test_a_failed_copy_leaves_the_core_s_bundle_to_show() { // spec: ARCH-121
        const core = makeCore()
        const shim = makeShim()
        const panel = makePanel(core, {shim: shim})
        load(core)
        const dir = TestFiles.newDir("bundle-copy")
        const source = bundleOf(dir)
        // A folder where the file should go.
        const taken = dir + "/taken.zip"
        verify(TestFiles.mkpath(taken))
        panel.exportTo("file:///" + taken)
        answer(core, isTool("diagnostics.bundle"), ok(source))
        tryVerify(() => !panel.exporter.busy && panel.banner.visible, 5000)
        compare(panel.banner.kind, "danger")
        verify(panel.banner.text.indexOf("wasn't saved as") >= 0, panel.banner.text)
        verify(panel.banner.text.indexOf(source.path) >= 0, "where the bundle still is: " + panel.banner.text)
        panel.explorerButton.click()
        compare(shim.shown, [source.path])
    }

    function test_a_failed_bundle_says_why_and_its_fix() { // spec: ARCH-121
        const core = makeCore()
        const shell = fakeShellComponent.createObject(tc)
        const panel = makePanel(core, {shell: shell})
        load(core)
        const dir = TestFiles.newDir("bundle-failed")
        // An unknown action is shown by its label and does nothing else.
        panel.exportTo("file:///" + dir + "/a.zip")
        answer(core, isTool("diagnostics.bundle"), failed({code: "store.io", message: "The disk is <b>full</b>.",
            fix: {ui_action: {id: "free_disk", label: "Free some disk"}, user_hint: "Remove old files."}}))
        tryVerify(() => panel.banner.visible, 5000)
        compare(panel.banner.kind, "danger")
        for (const s of ["The disk is <b>full</b>.", "Remove old files.", "Free some disk"])
            verify(panel.banner.text.indexOf(s) >= 0, panel.banner.text)
        verify(!panel.fixButton.visible, "no button for an action this build doesn't know")
        verify(!panel.explorerButton.visible)
        compare(TestFiles.list(dir), [], "nothing was written")

        // show_job with its job: "Show the job", which opens Jobs on it through the shell.
        panel.exportTo("file:///" + dir + "/b.zip")
        answer(core, isTool("diagnostics.bundle"), failed({code: "x.y", message: "No.", data: {job_id: "j_77"},
            fix: {ui_action: {id: "show_job", label: "Show the job"}}}))
        tryVerify(() => panel.fixButton.visible, 5000)
        compare(panel.fixButton.text, "Show the job")
        panel.fixButton.click()
        compare(shell.opened, [{id: "jobs", method: "showJob", arg: "j_77", focus: true}])
    }

    function test_a_show_job_fix_with_no_button_keeps_its_label() { // spec: ARCH-121
        const fix = {ui_action: {id: "show_job", label: "Show the job"}, user_hint: "Look at it."}
        const core = makeCore()
        const panel = makePanel(core)
        load(core, {"models.report": failed({code: "x.y", message: "No report.", fix: fix})})
        tryVerify(() => shows(panel.models, "No report."), 5000)
        verify(shows(panel.models, "Show the job"), "a card has no button, so its label stays: " + texts(panel.models).join(" | "))
        const dir = TestFiles.newDir("bundle-fix")
        panel.exportTo("file:///" + dir + "/a.zip")
        answer(core, isTool("diagnostics.bundle"), failed({code: "x.y", message: "No bundle.", fix: fix}))
        tryVerify(() => panel.banner.visible, 5000)
        verify(!panel.fixButton.visible, "no job to open")
        verify(panel.banner.text.indexOf("Show the job") >= 0, panel.banner.text)
    }

    function test_a_file_explorer_cant_show_is_said() { // spec: ARCH-121
        const core = makeCore()
        const shim = makeShim()
        const panel = makePanel(core, {shim: shim})
        load(core)
        const dir = TestFiles.newDir("bundle-moved")
        const source = bundleOf(dir)
        panel.exportTo("file:///" + dir + "/picked.zip")
        answer(core, isTool("diagnostics.bundle"), ok(source))
        tryVerify(() => panel.explorerButton.visible, 5000)
        shim.accepts = false
        panel.explorerButton.click()
        compare(panel.banner.kind, "danger")
        verify(panel.banner.text.indexOf("Explorer can't show") >= 0, panel.banner.text)
        verify(panel.banner.text.indexOf(dir + "/picked.zip") >= 0, panel.banner.text)
    }

    function test_a_bundle_that_cant_be_read_isnt_called_still_there() { // spec: ARCH-121
        const core = makeCore()
        const panel = makePanel(core)
        load(core)
        const dir = TestFiles.newDir("bundle-gone")
        panel.exportTo("file:///" + dir + "/picked.zip")
        answer(core, isTool("diagnostics.bundle"), ok({path: dir + "/gone.zip", bytes: 1, files: [], redactions: 0}))
        tryVerify(() => !panel.exporter.busy && panel.banner.visible, 5000)
        compare(panel.banner.kind, "danger")
        verify(panel.banner.text.indexOf("couldn't be read") >= 0, panel.banner.text)
        verify(panel.banner.text.indexOf("still at") < 0, panel.banner.text)
        verify(!panel.explorerButton.visible, "nothing to show")
        compare(TestFiles.list(dir), [], "nothing was written")
    }

    function test_a_second_export_says_it_is_under_way() { // spec: ARCH-121
        const core = makeCore()
        const panel = makePanel(core)
        load(core)
        const dir = TestFiles.newDir("bundle-second")
        const source = bundleOf(dir)
        panel.exportTo("file:///" + dir + "/one.zip")
        answer(core, isTool("diagnostics.bundle"), ok(source))
        tryVerify(() => panel.explorerButton.visible && panel.banner.visible, 5000)
        panel.exportTo("file:///" + dir + "/two.zip")
        verify(!panel.banner.visible, "the old banner goes")
        compare(panel.exportButton.text, "Exporting…")
        answer(core, isTool("diagnostics.bundle"), ok(source))
        tryVerify(() => panel.banner.visible && !panel.exporter.busy, 5000)
        compare(panel.exportButton.text, "Export diagnostics bundle")
        verify(panel.banner.text.indexOf("two.zip") >= 0, panel.banner.text)
    }

    function test_going_mid_copy_leaves_no_temporary_file() { // spec: ARCH-121
        const dir = TestFiles.newDir("bundle-going")
        const source = dir + "/source.zip"
        verify(TestFiles.write(source, "x".repeat(64 * 1024 * 1024)))
        const exporter = bundleExportComponent.createObject(tc)
        verify(exporter.save("file:///" + dir + "/kept.zip", source))
        exporter.destroy()
        wait(0)
        const names = TestFiles.list(dir)
        verify(names.indexOf("kept.zip") >= 0, names.join(", "))
        verify(!names.some(n => n.indexOf("lil-tmp") >= 0), "left: " + names.join(", "))
    }

    function test_explorer_takes_only_an_existing_zip() { // spec: ARCH-121
        const dir = TestFiles.newDir("explorer")
        const zip = dir + "/a.zip"
        verify(TestFiles.write(zip, "x"))
        verify(TestFiles.write(dir + "/a.exe", "x"))
        verify(TestFiles.mkpath(dir + "/folder.zip"))
        const argument = LilShim.explorerSelectArgument(zip)
        verify(argument.startsWith('/select,"') && argument.endsWith('a.zip"'), argument)
        verify(argument.indexOf("/", 1) < 0 || argument.indexOf("\\") > 0, "native separators: " + argument)
        for (const refused of [dir + "/missing.zip", dir + "/a.exe", dir + "/folder.zip", "a.zip", ""])
            compare(LilShim.explorerSelectArgument(refused), "", refused)
    }

    // --- The model inventory --------------------------------------------------------------------

    function test_models_report_states_are_listed() { // spec: CORE-164
        const core = makeCore()
        const panel = makePanel(core)
        load(core, {"models.report": ok(report({
            models: [
                {path: "checkpoints\\a<b>.safetensors", size: 2147483648, sha256: "aa", family: "sdxl", states: ["orphan"], untrusted: true},
                {path: "loras\\b.safetensors", size: 1048576, sha256: "bb", family: null, states: ["duplicate", "unknown_family"], untrusted: true},
                {path: "vae\\c.gguf", size: 1048576, sha256: null, family: "flux", states: [], model_id: "m_c", model_name: "Flux VAE", untrusted: true}],
            missing: [{model_id: "m_gone", model_name: "Gone model", sha256: "cc"}]}))})
        tryVerify(() => panel.models.rows.length === 4, 5000)
        verify(shows(panel.models, "3 model files, in D:\\comfy"), texts(panel.models).join(" | "))
        verify(shows(panel.models, "1 orphan, 1 duplicate, 1 of unknown family, 1 missing"), texts(panel.models).join(" | "))
        const row = n => panel.models.view.itemAtIndex(n)
        tryVerify(() => row(0) !== null, 5000)
        verify(shows(row(0), "Missing"), texts(row(0)).join(" | "))
        verify(shows(row(0), "Gone model"))
        verify(shows(row(1), "checkpoints\\a<b>.safetensors"), "the name as its text")
        verify(shows(row(1), "2 GiB"), texts(row(1)).join(" | "))
        verify(shows(row(1), "sdxl"))
        verify(shows(row(1), "orphan"))
        verify(shows(row(2), "duplicate"))
        verify(shows(row(2), "unknown family"))
        verify(shows(row(3), "Flux VAE"))
        verify(shows(row(3), "not hashed"), "a file never read: " + texts(row(3)).join(" | "))
    }

    function test_a_registry_that_isnt_read_calls_nothing_orphan() { // spec: CORE-164
        for (const [status, words] of [["missing", "registry/models.yaml doesn't exist"],
                                       ["unreadable", "registry/models.yaml couldn't be read"],
                                       ["no_workspace", "There is no default workspace"]]) {
            answered = {}
            const core = makeCore()
            const panel = makePanel(core)
            load(core, {"models.report": ok(report({registry: status, models: [
                {path: "a.safetensors", size: 10, sha256: "aa", family: "sdxl", states: ["orphan"], untrusted: true}]}))})
            tryVerify(() => panel.models.rows.length === 1, 5000, status)
            verify(shows(panel.models, words), status + ": " + texts(panel.models).join(" | "))
            verify(shows(panel.models, "no file is called orphan or missing"))
            tryVerify(() => panel.models.view.itemAtIndex(0) !== null, 5000)
            verify(!shows(panel.models.view.itemAtIndex(0), "orphan"), status)
            verify(!texts(panel.models).some(t => /\d orphan/.test(t)), status)
        }
    }

    function test_models_report_before_the_first_inventory_and_when_not_configured() { // spec: CORE-164
        const core = makeCore()
        const panel = makePanel(core)
        load(core, {"engines.status": ok({engines: [comfy({inventory: undefined})]}),
                    "models.report": ok(report({refreshed: null}))})
        tryVerify(() => shows(panel.models, "No inventory yet"), 5000, texts(panel.models).join(" | "))
        // The core's `status` event when the job keeps the capture: the environment is read again.
        const environments = callsOf(core, "engines.environment").length
        core.emit("engines", "event", {kind: "status", engine: comfy({inventory: undefined})})
        answer(core, isTool("engines.environment"), ok({captures: [capture("2026-10-30T10:00:00Z")], changes: []}))
        tryVerify(() => shows(panel.engines, "Engine environment recorded"), 5000, texts(panel.engines).join(" | "))
        compare(callsOf(core, "engines.environment").length, environments + 1)
        verify(shows(panel.models, "No inventory yet"), "no new inventory yet")
        // Its `status` event when the job keeps the inventory (set_inventory, core test
        // engines_topic_says_when_the_inventory_and_the_capture_are_kept): the report is read again;
        // the same inventory isn't.
        const before = callsOf(core, "models.report").length
        core.emit("engines", "event", {kind: "status", engine: comfy()})
        answer(core, isTool("models.report"), ok(report()))
        tryVerify(() => shows(panel.models, "0 model files"), 5000, texts(panel.models).join(" | "))
        verify(shows(panel.engines, "1,234 model files"))
        compare(callsOf(core, "models.report").length, before + 1)
        core.emit("engines", "event", {kind: "status", engine: comfy({state: "starting"})})
        wait(1100)
        compare(callsOf(core, "models.report").length, before + 1)

        const core2 = makeCore()
        const panel2 = makePanel(core2)
        answered = {}
        load(core2, {"models.report": failed({code: "engine.not_configured", message: "ComfyUI has no root."})})
        tryVerify(() => shows(panel2.models, "ComfyUI has no root."), 5000, texts(panel2.models).join(" | "))
    }

    function test_model_sizes_read_in_their_own_unit() { // spec: CORE-164
        const core = makeCore()
        const panel = makePanel(core)
        const entry = (path, size) => ({path: path, size: size, sha256: "a", family: "x", states: [], untrusted: true})
        load(core, {"models.report": ok(report({models: [entry("a", 500), entry("b", 30 * 1024), entry("c", 144 * 1048576), entry("d", 6.5 * 1073741824)]}))})
        tryVerify(() => panel.models.view.itemAtIndex(3) !== null, 5000)
        for (const [row, size] of [[0, "500 B"], [1, "30 KiB"], [2, "144 MiB"], [3, "6.5 GiB"]])
            verify(shows(panel.models.view.itemAtIndex(row), size), size + ": " + texts(panel.models.view.itemAtIndex(row)).join(" | "))
    }

    function test_no_configured_comfyui_means_no_report_read() { // spec: CORE-164
        const core = makeCore()
        const panel = makePanel(core)
        answer(core, isTool("engines.status"), ok({engines: [{engine: "comfyui", class: "none", state: "stopped", url: "http://127.0.0.1:8188", listen: "unknown", outside_jobs: {running: 0, queued: 0}, restart_needed: false, launch_profile: null, asset_catalogue: "unknown"}]}))
        tryVerify(() => shows(panel.models, "ComfyUI isn't configured"), 5000, texts(panel.models).join(" | "))
        wait(50)
        compare(callsOf(core, "models.report"), [], "no engine.not_configured for a panel nobody asked of")
        verify(shows(panel.engines, "ComfyUI isn't configured"), texts(panel.engines).join(" | "))
        verify(!shows(panel.engines, "Where it listens"), texts(panel.engines).join(" | "))
        // Configured later: it reads.
        core.emit("engines", "event", {kind: "status", engine: comfy({state: "stopped", class: "none", inventory: undefined})})
        answer(core, isTool("models.report"), ok(report({refreshed: null})))
        tryVerify(() => shows(panel.models, "No inventory yet"), 5000)
    }

    function test_engine_lines_say_what_is_wrong() { // spec: ARCH-121
        const core = makeCore()
        const panel = makePanel(core)
        load(core, {"engines.status": ok({engines: [comfy({state: "unreachable"})]}),
                    "engines.environment": ok({captures: [capture("2026-10-30T10:00:00Z"), capture("2026-10-29T10:00:00Z")],
                        changes: [{what: "node", name: "pack_a", change: "added"},
                                  {what: "node", name: "pack_b", change: "removed"},
                                  {what: "node", name: "pack_c", from: "1.0", change: "changed"}]})})
        tryVerify(() => shows(panel.engines, "pack_c"), 5000, texts(panel.engines).join(" | "))
        const head = find(panel.engines, i => typeof i.tone === "string" && i.text.indexOf("· unreachable") >= 0)
        verify(head, texts(panel.engines).join(" | "))
        compare(head.tone, "warning")
        verify(shows(panel.engines, "pack_a: added, version not known"), texts(panel.engines).join(" | "))
        verify(shows(panel.engines, "pack_b: removed, version not known"))
        verify(shows(panel.engines, "pack_c: 1.0 → version not known"))
        verify(!texts(panel.engines).some(t => /, $|→ $|was $/.test(t)), "no line ends on nothing")
    }

    // --- The notification path and the status lines ---------------------------------------------

    function test_notification_path_problems_are_named() { // spec: APP-146, APP-022
        const path = (over) => Object.assign({toasts: "ready", shortcut: "ok", handlers: "ok"}, over)
        const off = reason => path({toasts: "unavailable", toasts_reason: reason})
        const cases = [
            [off("disabled_for_app"), "Notifications are off for LILITH in Windows Settings"],
            [off("disabled_for_user"), "Notifications are off in Windows Settings"],
            [off("disabled_by_policy"), "A policy blocks notifications"],
            [off("elevated"), "The core runs as administrator, so Windows won't show its notifications"],
            [off("no_platform"), "Windows isn't showing LILITH's notifications on this PC"],
            [off("a_new_reason"), "Windows won't show LILITH's notifications (a_new_reason)"],
            [path({shortcut: "missing"}), "The Start-menu shortcut is missing"],
            [path({shortcut: "wrong"}), "The Start-menu shortcut opens another program or build"],
            [path({handlers: "missing"}), "This LILITH's lil: links aren't registered"],
            [path({handlers: "other_channel"}), "Another LILITH build handles lil: links"]
        ]
        for (const [p, sentence] of cases) {
            const problems = NotificationText.problems(p)
            compare(problems.length, 1, sentence)
            verify(problems[0].indexOf(sentence) === 0, problems[0])
            verify(problems[0].indexOf("lil doctor") >= 0, "says what to do: " + problems[0])
        }
        // The Settings panel's "Set up" is offered only for what it can set up (Task 27).
        for (const p of [path({shortcut: "missing"}), path({shortcut: "wrong"}), path({handlers: "missing"})])
            verify(NotificationText.problems(p)[0].indexOf('"Set up"') >= 0, NotificationText.problems(p)[0])
        for (const p of [path({handlers: "other_channel"}), off("disabled_for_app"), off("elevated")])
            verify(NotificationText.problems(p)[0].indexOf("Set up") < 0, NotificationText.problems(p)[0])
        verify(NotificationText.problems(path({shortcut: "missing"}))[0].indexOf("Run lil doctor to fix") >= 0)
        // Reachable: not yet shown is no problem.
        compare(NotificationText.problems(path({})), [])
        compare(NotificationText.problems(path({toasts: "not_yet_shown"})), [])
        // Several parts: each its own sentence.
        compare(NotificationText.problems({toasts: "unavailable", toasts_reason: "elevated", shortcut: "wrong", handlers: "missing"}).length, 3)

        // On the card, as read and as it changes.
        const core = makeCore()
        const panel = makePanel(core)
        load(core, {"desktop.get": ok(desktop({notification_path: path({toasts: "not_yet_shown"})}))})
        tryVerify(() => shows(panel.notifications, NotificationText.reachable), 5000, texts(panel.notifications).join(" | "))
        core.emit("desktop", "event", {kind: "settings", settings: desktop()})
        answer(core, isTool("desktop.get"), ok(desktop({notification_path: path({toasts: "unavailable", toasts_reason: "disabled_for_user", shortcut: "wrong"})})))
        tryVerify(() => shows(panel.notifications, "Notifications are off in Windows Settings"), 5000)
        verify(shows(panel.notifications, "The Start-menu shortcut opens another program or build"))
        verify(!shows(panel.notifications, NotificationText.reachable))
    }

    function test_a_failed_desktop_read_is_said() { // spec: APP-146
        const core = makeCore()
        const panel = makePanel(core)
        load(core, {"desktop.get": failed({code: "core.bug", message: "The desktop can't be read."})})
        tryVerify(() => shows(panel.notifications, "The desktop can't be read."), 5000, texts(panel.notifications).join(" | "))
        verify(shows(panel.status, "Keep-awake: not known"), texts(panel.status).join(" | "))
    }

    function test_keep_awake_unavailable_is_said() { // spec: ARCH-127
        const core = makeCore()
        const panel = makePanel(core)
        load(core, {"desktop.get": ok(desktop({keep_awake_available: false}))})
        tryVerify(() => shows(panel.status, "This PC doesn't let LILITH keep it awake while GPU work runs"), 5000, texts(panel.status).join(" | "))
        const core2 = makeCore()
        const panel2 = makePanel(core2)
        answered = {}
        load(core2)
        tryVerify(() => shows(panel2.status, "Keep-awake is on"), 5000, texts(panel2.status).join(" | "))
        core2.emit("desktop", "event", {kind: "settings", settings: desktop({keep_awake_gpu: false})})
        answer(core2, isTool("desktop.get"), ok(desktop({keep_awake_gpu: false})))
        tryVerify(() => shows(panel2.status, "Keep-awake is off"), 5000, texts(panel2.status).join(" | "))
    }

    function test_start_times_are_shown() { // spec: APP-136
        const core = makeCore()
        const panel = makePanel(core)
        load(core)
        verify(shows(panel.status, "This start: not measured yet"), texts(panel.status).join(" | "))
        core.startTimes = {firstFrameMs: 377.4, interactiveMs: 1414.2}
        tryVerify(() => shows(panel.status, "This start: first frame in 377 ms, interactive in 1,414 ms"), 5000, texts(panel.status).join(" | "))
    }

    function test_backup_status_line_and_the_backups_panel_empty_state() { // spec: APP-133, APP-134
        const line = "No backups yet: backups arrive in a later version of LILITH"
        const core = makeCore()
        const shell = fakeShellComponent.createObject(tc)
        const panel = makePanel(core, {shell: shell})
        load(core)
        verify(shows(panel.status, line), texts(panel.status).join(" | "))
        const open = button(panel.status, "Open Backups")
        verify(open, "the status line's button")
        open.click()
        compare(shell.opened, [{id: "backups", method: "", arg: "", focus: true}])

        const host = createTemporaryObject(backupsComponent, tc, {core: core})
        verify(shows(host.panel, line), texts(host.panel).join(" | "))
        compare(host.panel.Accessible.name, "Backups")
        compare(walk.report(host.panel), [])
    }

    function test_open_backups_opens_the_closed_panel_in_the_shell() { // spec: APP-008, APP-133
        const core = makeCore()
        const shell = createTemporaryObject(shellComponent, tc, {core: core})
        shell.visible = true
        const dock = shell.docks.diagnostics
        tryVerify(() => shell.docks.home.content && shell.docks.home.content.width > 0, 5000, "the docks are laid out")
        tryVerify(() => dock.content && dock.content.core === core, 5000)
        verify(dock.content.Accessible.name === "Diagnostics")
        compare(dock.content.shell, shell, "the shell reaches the panel")
        verify(!shell.docks.backups.isOpen, "Backups stays closed in the Default layout")
        const line = "No backups yet: backups arrive in a later version of LILITH"
        verify(shell.docks.backups.content.Accessible.name === "Backups")
        dock.content.status.openBackups()
        tryVerify(() => shell.docks.backups.isOpen, 5000)
        tryVerify(() => shows(shell.docks.backups.content, line), 5000, "its empty state")
    }

    // --- Reads ----------------------------------------------------------------------------------

    function test_reads_follow_the_session() { // spec: ARCH-100, APP-033
        const core = makeCore()
        const panel = makePanel(core)
        load(core)
        for (const topic of ["engines", "diagnostics", "desktop", "telemetry", "jobs"])
            verify(core.subscriberCount(topic) >= 1, topic)
        const reads = name => callsOf(core, name).length
        const before = reads("engines.status")
        // Never on `ready`.
        core.connection = "reconnecting"
        core.connection = "ready"
        wait(50)
        compare(reads("engines.status"), before)
        // On `resync_required` and on a snapshot.
        core.emit("engines", "resync_required", null)
        answer(core, isTool("engines.status"), ok({engines: [comfy()]}))
        compare(reads("engines.status"), before + 1)
        wait(1100)
        core.emit("engines", "snapshot", {engines: [comfy()]})
        answer(core, isTool("engines.status"), ok({engines: [comfy()]}))
        compare(reads("engines.status"), before + 2)
        const recent = reads("diagnostics.recent")
        core.emit("diagnostics", "resync_required", null)
        compare(reads("diagnostics.recent"), recent + 1)
    }

    function test_a_panel_behind_a_tab_reads_nothing() { // spec: ARCH-121, APP-136
        const core = makeCore()
        const shell = createTemporaryObject(shellComponent, tc, {core: core})
        shell.visible = true
        const dock = shell.docks.diagnostics
        tryVerify(() => shell.layoutSettled, 5000, "the docks are laid out")
        const panel = dock.content
        verify(!panel.visible, "a tab behind")
        const own = ["diagnostics.recent", "desktop.get", "models.report", "engines.environment"]
        for (const name of own)
            compare(callsOf(core, name), [], name + " isn't read behind a tab")
        // The log still hears the topics only this panel follows, from the start.
        for (const topic of ["diagnostics", "desktop"])
            verify(core.subscriberCount(topic) >= 1, topic)
        // A job's change behind the tab costs no read of its own.
        const lists = callsOf(core, "jobs.list").filter(c => c.args.limit === 100 && c.args.status === "queued").length
        // First shown, it reads.
        dock.setAsCurrentTab()
        tryVerify(() => panel.visible && panel.width > 0, 5000)
        for (const name of own.slice(0, 2))
            tryVerify(() => callsOf(core, name).length === 1, 5000, name)
        compare(callsOf(core, "jobs.list").filter(c => c.args.limit === 100 && c.args.status === "queued").length, lists + 1)
        // And behind a tab again it follows no more, but for the log.
        shell.docks.hardware.setAsCurrentTab()
        tryVerify(() => !panel.visible, 5000)
        tryVerify(() => !panel.shown, 5000)
        const reads = callsOf(core, "diagnostics.recent").length
        core.emit("diagnostics", "event", {kind: "error", code: "x", at: "2026-10-30T10:00:00Z"})
        wait(1100)
        compare(callsOf(core, "diagnostics.recent").length, reads)
        shell.destroy()
        wait(100)
    }

    function test_shown_again_it_reads_afresh() { // spec: APP-146, CORE-164
        const core = makeCore()
        const panel = makePanel(core)
        load(core, {"desktop.get": ok(desktop({notification_path: {toasts: "ready", shortcut: "missing", handlers: "ok"}}))})
        tryVerify(() => shows(panel.notifications, "The Start-menu shortcut is missing"), 5000)
        compare(callsOf(core, "desktop.get")[0].args.fresh, true, "never the 10 s cache")
        wait(2100)
        // You run lil doctor, and come back.
        panel.visible = false
        tryVerify(() => !panel.shown, 5000)
        panel.visible = true
        const call = answer(core, isTool("desktop.get"), ok(desktop()))
        compare(call.args.fresh, true)
        tryVerify(() => shows(panel.notifications, "Nothing stands in the way"), 5000, texts(panel.notifications).join(" | "))
        for (const name of ["models.report", "engines.environment", "engines.status", "diagnostics.recent", "telemetry.snapshot"])
            answer(core, isTool(name), ok(name === "models.report" ? report() : name === "engines.environment" ? {captures: [], changes: []}
                                          : name === "engines.status" ? {engines: [comfy()]} : name === "diagnostics.recent" ? {errors: []} : snapshot()))
        // Back and forth: at most once in 2 s, the last answer still fresh at the end.
        const reads = callsOf(core, "desktop.get").length
        panel.visible = false
        tryVerify(() => !panel.shown, 5000)
        panel.visible = true
        wait(100)
        compare(callsOf(core, "desktop.get").length, reads, "within 2 s of the last read")
        tryVerify(() => callsOf(core, "desktop.get").length === reads + 1, 5000, "the read held back")
    }

    function test_a_short_flicker_of_the_panel_reads_nothing() { // spec: ARCH-116
        const core = makeCore()
        const panel = makePanel(core)
        load(core)
        wait(2100)
        const calls = core.calls.length
        panel.visible = false
        wait(20)
        panel.visible = true
        wait(800)
        compare(core.calls.length, calls)
        verify(panel.shown)
    }

    function test_a_first_read_open_when_ready_comes_is_retried() { // spec: APP-033
        const core = makeCore()
        core.connection = "connecting"
        const panel = makePanel(core)
        const open = nextCall(core, isTool("engines.environment"))
        core.connection = "ready"
        answered[open.id] = true
        core.respond(open.id, failed({code: "core.unreachable", message: "Not connected."}))
        answer(core, isTool("engines.environment"), ok({captures: [capture("2026-10-30T10:00:00Z")], changes: []}))
        tryVerify(() => shows(panel.engines, "Engine environment recorded"), 5000, texts(panel.engines).join(" | "))
    }

    function test_a_re_read_keeps_the_focus_on_a_rows_button() { // spec: APP-156
        const core = makeCore()
        const shell = fakeShellComponent.createObject(tc)
        const panel = makePanel(core, {shell: shell})
        const row = (count, message) => ({code: "comfyui.out_of_memory", count: count, first_at: "2026-10-30T09:00:00Z", last_at: "2026-10-30T10:00:00Z",
                                          last_message: message, last_tool: "comfy.prompt", last_job_id: "j_0123"})
        load(core, {"diagnostics.recent": ok({errors: [row(2, "First.")]})})
        tryVerify(() => button(panel.errors, "Show the job") !== null, 5000)
        const show = button(panel.errors, "Show the job")
        show.forceActiveFocus(Qt.TabFocusReason)
        verify(show.activeFocus)
        core.emit("diagnostics", "event", {kind: "error", code: "comfyui.out_of_memory", at: "2026-10-30T10:00:00Z"})
        answer(core, isTool("diagnostics.recent"), ok({errors: [{code: "new.one", count: 1, first_at: "2026-10-30T10:00:00Z", last_at: "2026-10-30T10:00:00Z", last_message: "New."}, row(3, "Second.")]}))
        tryVerify(() => shows(panel.errors, "Second."), 5000, texts(panel.errors).join(" | "))
        verify(shows(panel.errors, "3 times"))
        verify(button(panel.errors, "Show the job") === show, "the same button")
        verify(show.activeFocus, "the focus stays on it")
        compare(panel.errors.rowsModel.count, 2)
    }

    function test_a_first_read_that_failed_reads_again_when_ready() { // spec: APP-033
        const core = makeCore()
        core.connection = "connecting"
        const panel = makePanel(core)
        load(core, {"engines.status": failed({code: "core.unreachable", message: "The core isn't there."})})
        tryVerify(() => shows(panel.engines, "The core isn't there."), 5000, texts(panel.engines).join(" | "))
        core.connection = "ready"
        answer(core, isTool("engines.status"), ok({engines: [comfy()]}))
        tryVerify(() => shows(panel.engines, "ComfyUI 0.42.1 · owned · ready"), 5000)
        verify(!shows(panel.engines, "The core isn't there."))
    }

    function test_outside_text_is_plain_and_cut() { // spec: APP-156
        const core = makeCore()
        const panel = makePanel(core)
        const hostile = "<img src=\"http://127.0.0.1:9/x\"><b>x</b>" + "y".repeat(5000)
        load(core, {
            "engines.status": ok({engines: [comfy({version: hostile, url: hostile})]}),
            "engines.environment": ok({captures: [{captured: hostile}], changes: [{what: "node", name: hostile, to: hostile, change: "added"}]}),
            "diagnostics.recent": ok({errors: [{code: hostile, count: 1, first_at: hostile, last_at: hostile, last_message: hostile, last_tool: hostile, last_job_id: hostile}]}),
            "models.report": ok(report({root: hostile, models: [{path: hostile, size: 1, sha256: null, family: hostile, states: [hostile], untrusted: true}], missing: [{model_id: hostile, model_name: hostile, sha256: hostile}]})),
            "telemetry.snapshot": ok(snapshot({gpu: Object.assign(snapshot().gpu, {name: hostile, driver: hostile})}))
        })
        core.emit(hostile, "event", {text: hostile})
        tryVerify(() => panel.models.view.itemAtIndex(0) !== null && panel.errors.rows.length === 1, 5000)
        wait(100)
        const checked = []
        const walkItem = i => {
            if (typeof i.text === "string" && typeof i.lineCount === "number") {
                checked.push(i.text)
                compare(i.textFormat, Text.PlainText, "plain text: " + i.text.slice(0, 80))
                verify(i.text.length <= 1200, "cut: " + i.text.length + ": " + i.text.slice(0, 80))
            }
            for (let n = 0; n < i.children.length; ++n)
                walkItem(i.children[n])
        }
        walkItem(panel)
        verify(checked.some(t => t.indexOf("<img src=") >= 0), "the markup reached a Text as text")
        compare(TestFiles.networkRequests(), 0, "and loaded nothing")
    }

    function test_providers_and_claude_code_say_when_they_come() { // spec: QA-083
        const core = makeCore()
        const panel = makePanel(core)
        load(core)
        // What plan 4a delivers of QA-083, each on the panel...
        tryVerify(() => shows(panel.engines, "ComfyUI 0.42.1 · owned · ready"), 5000)
        verify(shows(panel.versions, "Core 0.0.0+fakecore"))
        verify(shows(panel.errors, "No errors"))
        verify(shows(panel.telemetry, "Test GPU"))
        verify(shows(panel.queue, "running"))
        verify(shows(panel.status, "No backups yet"))
        // ...and what it doesn't says so.
        verify(shows(panel.providers, "Provider health"), texts(panel.providers).join(" | "))
        verify(shows(panel.providers, "Claude Code"))
        verify(shows(panel.providers, "later version of LILITH"))
        compare(panel.providers.title, "Providers and Claude Code")
    }

    // --- Floating, and the gate -----------------------------------------------------------------

    function test_float_and_redock_keeps_diagnostics_state() { // spec: ARCH-116
        const core = makeCore()
        const shell = createTemporaryObject(shellComponent, tc, {core: core})
        shell.visible = true
        const dock = shell.docks.diagnostics
        tryVerify(() => shell.docks.home.content && shell.docks.home.content.width > 0, 5000, "the docks are laid out")
        tryVerify(() => dock.content && dock.content.core === core, 5000)
        const panel = dock.content
        wait(50)
        dock.setAsCurrentTab()
        tryVerify(() => panel.visible && panel.width > 0, 5000, "the diagnostics tab is in front")
        // The other panels ask the same tools here, so the panel's state comes from an event.
        tryVerify(() => panel.shown, 5000)
        core.emit("engines", "event", {kind: "status", engine: comfy()})
        tryVerify(() => shows(panel.engines, "ComfyUI 0.42.1 · owned · ready"), 5000)
        core.emit("things", "event", {n: 1})
        const logged = panel.log.view.count
        verify(logged >= 1)
        panel.scroller.contentY = 40
        const calls = core.calls.length
        dock.isFloating = true
        tryVerify(() => dock.isFloating && panel.Window.window && panel.Window.window !== shell, 5000,
                  "the panel floats in its own window")
        verify(dock.content === panel, "the same panel item")
        verify(shows(panel.engines, "ComfyUI 0.42.1 · owned · ready"))
        compare(panel.log.view.count, logged)
        dock.isFloating = false
        tryVerify(() => !dock.isFloating && panel.Window.window === shell, 5000, "docked again")
        wait(50)
        verify(dock.content === panel)
        verify(shows(panel.engines, "ComfyUI 0.42.1 · owned · ready"))
        compare(panel.log.view.count, logged)
        compare(core.calls.filter(c => ["engines.status", "diagnostics.recent", "models.report"].indexOf(c.name) >= 0
                                       && core.calls.indexOf(c) >= calls), [],
                "moving it reads nothing again")
    }

    function test_the_keyboard_reaches_what_is_scrolled_away() { // spec: APP-156
        const core = makeCore()
        const panel = makePanel(core)
        load(core, {"models.report": ok(report({models: Array.from({length: 40}, (x, n) => (
            {path: "m" + n + ".safetensors", size: 1, sha256: "a", family: "x", states: [], untrusted: true}))}))})
        tryVerify(() => panel.models.view.count === 40, 5000)
        panel.Window.window.height = 300
        tryVerify(() => panel.scroller.contentHeight > panel.scroller.height, 5000)
        panel.scroller.contentY = 0
        panel.log.view.forceActiveFocus(Qt.TabFocusReason)
        tryVerify(() => panel.scroller.contentY > 0, 5000, "the event log scrolled into view")
    }

    function test_a11y_walk_passes() { // spec: APP-156
        const core = makeCore()
        const shell = fakeShellComponent.createObject(tc)
        const panel = makePanel(core, {shell: shell})
        load(core, {
            "diagnostics.recent": ok({errors: [{code: "a.b", count: 2, first_at: "2026-10-30T09:00:00Z", last_at: "2026-10-30T10:00:00Z",
                                                last_message: "It failed.", last_tool: "comfy.prompt", last_job_id: "j_1"}]}),
            "models.report": ok(report({models: [{path: "a.safetensors", size: 1, sha256: "a", family: "x", states: ["orphan"], untrusted: true}],
                                        missing: [{model_id: "m", sha256: "b"}]}))
        })
        core.emit("things", "event", {n: 1})
        tryVerify(() => panel.models.view.count === 2 && panel.errors.rows.length === 1 && panel.log.view.count >= 1, 5000)
        const dir = TestFiles.newDir("bundle-a11y")
        const source = bundleOf(dir)
        panel.exportTo("file:///" + dir + "/picked.zip")
        answer(core, isTool("diagnostics.bundle"), ok(source))
        tryVerify(() => panel.explorerButton.visible, 5000)
        wait(100)
        compare(walk.report(panel), [])
        verify(walk.interactiveCount(panel) >= 8,
               "the banner, export, Explorer, the figures, the lists and the buttons: " + walk.interactiveCount(panel))
        for (const view of [panel.models.view, panel.log.view]) {
            compare(LilShim.accessibleOf(view).role, Accessible.List)
            verify(view.activeFocusOnTab)
            const row = view.itemAtIndex(0)
            verify(row, "a row")
            compare(LilShim.accessibleOf(row).role, Accessible.ListItem)
            verify(LilShim.accessibleOf(row).name !== "", "a row's name")
        }
    }
}
