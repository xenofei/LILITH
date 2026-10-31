// grab_diagnostics: the Diagnostics and Backups panels offscreen, saved as
// <grab dir>/diagnostics-<state>.png (LIL_GRAB_DIR, else CMake's build/<preset>/grabs) for the
// reviewer, on FakeCore's scripted answers: the panel alone at the shell's 1280 x 800 with
// everything in order, the same with problems on each card, scrolled to its end, the panel docked
// in the shell's Default layout, and the Backups panel's empty state. Each grab must have the
// window's size and not be one flat colour.
import QtQuick
import QtTest
import Lilith.UiKit

TestCase {
    id: tc
    name: "GrabDiagnostics"
    when: windowShown
    width: 200
    height: 100

    Component {
        id: fakeComponent
        FakeCore {
            project: "sketchbook"
            startTimes: ({firstFrameMs: 377.4, interactiveMs: 414.2})
        }
    }

    Component {
        id: fakeShellComponent
        QtObject {
            function openPanelOn(id, method, arg, focus) {}
        }
    }

    Component {
        id: panelComponent
        Window {
            id: host
            required property var core
            required property var shell
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
                shell: host.shell
            }
        }
    }

    Component {
        id: backupsComponent
        Window {
            id: host
            required property var core
            width: 1280
            height: 800
            visible: true
            color: Theme.bg

            BackupsPanel {
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

    property var answered: ({})

    function init() {
        answered = {}
        wait(100)
    }

    function ok(structured) {
        return {status: "ok", structured: structured, artifacts: [], summary_text: ""}
    }

    function answer(core, pred, structured) {
        let call = null
        tryVerify(() => (call = core.calls.find(c => pred(c) && !answered[c.id]) || null) !== null, 5000)
        answered[call.id] = true
        verify(core.respond(call.id, ok(structured)))
    }

    function isTool(name) {
        return c => c.name === name
    }

    function engine(over) {
        return Object.assign({
            engine: "comfyui", class: "owned", state: "ready", url: "http://127.0.0.1:8188", pid: 4242, version: "0.42.1",
            listen: "loopback", outside_jobs: {running: 0, queued: 0}, restart_needed: false,
            inventory: {models: 412, refreshed: "2026-10-30T10:00:00Z"},
            launch_profile: {reserve_vram_gb: "1.5", preview_method: "taesd", manager: false, dynamic_vram: true,
                             flags: ["--reserve-vram", "1.5", "--preview-method", "taesd"]},
            asset_catalogue: "off"
        }, over || {})
    }

    function capture(at) {
        return {system_stats: {system: {comfyui_version: "0.42.1"}, devices: []}, driver: "999.10", comfyui: "abc1234", nodes: [], captured: at}
    }

    function snapshot() {
        return {ts: "2026-10-30T10:00:00Z",
                gpu: {name: "Example GPU", driver: "999.10", util_pct: 42, power_mw: 180000, temp_c: 63, sm_clock_mhz: 2000,
                      mem_clock_mhz: 9000, throttle: [], vram_total_mib: 16384, vram_used_mib: 9728, measured: true},
                processes: [], cpu: {util_permille: 125, per_core_permille: [], package_power_mw: 0, measured: false},
                ram: {total_mib: 65536, used_mib: 32768}, disks: [], sizes: {}, net: [], system_power_mw: 240000,
                system_parts: [], alerts: [], gpu_users: []}
    }

    function load(core, over) {
        const o = over || {}
        answer(core, isTool("engines.status"), {engines: [o.engine || engine()]})
        answer(core, isTool("engines.environment"), o.environment || {captures: [capture("2026-10-30T10:00:00Z")], changes: []})
        answer(core, isTool("diagnostics.recent"), {errors: o.errors || []})
        answer(core, isTool("desktop.get"), {notifications: {}, quiet_while_fullscreen: true, fullscreen_autopause: true,
            keep_awake_gpu: true, keep_awake_available: o.keepAwake !== false,
            notification_path: o.path || {toasts: "ready", shortcut: "ok", handlers: "ok"}})
        answer(core, isTool("models.report"), o.report || {root: "D:\\comfy", refreshed: "2026-10-30T10:00:00Z", registry: "ok", models: [
            {path: "checkpoints\\example-xl.safetensors", size: 6938000000, sha256: "aa", family: "sdxl", states: [], model_id: "m_xl", model_name: "Example XL", untrusted: true},
            {path: "loras\\style.safetensors", size: 144000000, sha256: "bb", family: null, states: ["orphan", "unknown_family"], untrusted: true},
            {path: "loras\\style copy.safetensors", size: 144000000, sha256: "bb", family: null, states: ["orphan", "duplicate", "unknown_family"], untrusted: true},
            {path: "vae\\placeholder.safetensors", size: 335000000, sha256: null, family: "flux", states: ["orphan"], untrusted: true}],
            missing: [{model_id: "m_gone", model_name: "A recorded model", sha256: "cc"}]})
        answer(core, isTool("telemetry.snapshot"), snapshot())
        const lanes = [{lane: "gpu-heavy", paused: !!o.paused, reasons: o.paused ? ["manual"] : [], held: o.paused ? 3 : 0, restart_held: 0, withdrawn: 0},
                       {lane: "cpu", paused: false, reasons: [], held: 0, restart_held: 0, withdrawn: 0}]
        answer(core, c => c.name === "jobs.list" && c.args.status === "running", {jobs: [{job_id: "j_1"}], next_cursor: null, lanes: lanes})
        answer(core, c => c.name === "jobs.list" && c.args.status === "queued", {jobs: [{job_id: "j_2"}, {job_id: "j_3"}], next_cursor: null, lanes: lanes})
        // What the log really holds: events of the followed topics, as the core publishes them.
        for (let i = 0; i < 8; ++i) {
            core.emit("jobs", "event", {kind: "job", job: {job_id: "j_000000000000000" + i, kind: "comfy.prompt", status: "running"}})
            core.emit("engines", "event", {kind: "status", engine: engine()})
            core.emit("diagnostics", "event", {kind: "error", code: "store.io", tool: "asset.get", at: "2026-10-30T10:00:0" + i + ".000Z"})
        }
    }

    function problems() {
        return {
            engine: engine({version: undefined, class: "attached", launch_profile: null, listen: "all_interfaces",
                            url: "http://0.0.0.0:8188", restart_needed: true, asset_catalogue: "on",
                            outside_jobs: {running: 1, queued: 2}, suite_job: "j_0123456789abcdef"}),
            environment: {captures: [capture("2026-10-30T10:00:00Z"), capture("2026-10-29T10:00:00Z")],
                          changes: [{what: "driver", from: "998.20", to: "999.10", change: "changed"},
                                    {what: "node", name: "ComfyUI-Example", to: "1.2 (abc1234)", change: "added"}]},
            errors: [{code: "comfyui.out_of_memory", count: 3, first_at: "2026-10-30T09:00:00Z", last_at: "2026-10-30T10:00:00Z",
                      last_message: "ComfyUI ran out of GPU memory, also on 2 retries.", last_tool: "comfy.prompt", last_job_id: "j_0123456789abcdef"},
                     {code: "store.io", count: 1, first_at: "2026-10-30T08:00:00Z", last_at: "2026-10-30T08:00:00Z",
                      last_message: "The file couldn't be written: <b>%USERPROFILE%</b>\\pictures\\a.png", last_tool: "asset.get"}],
            path: {toasts: "unavailable", toasts_reason: "disabled_for_app", shortcut: "wrong", handlers: "other_channel"},
            keepAwake: false,
            paused: true,
            report: {root: "D:\\comfy", refreshed: "2026-10-30T10:00:00Z", registry: "unreadable", missing: [], models: [
                {path: "checkpoints\\a.safetensors", size: 6938000000, sha256: "aa", family: "sdxl", states: [], untrusted: true}]}
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

    function save(win, state) {
        wait(300)
        compare(win.width, 1280)
        const img = grabImage(win.contentItem)
        const dpr = win.devicePixelRatio
        compare(img.width, Math.round(win.width * dpr))
        compare(img.height, Math.round(win.height * dpr))
        verify(distinctColours(img) >= 3, "not one flat colour")
        const dir = TestFiles.env("LIL_GRAB_DIR") || TestFiles.env("LIL_GRAB_DEFAULT_DIR")
        verify(dir !== "", "a grab folder")
        verify(TestFiles.mkpath(dir))
        img.save(dir + "/diagnostics-" + state + ".png")
        verify(TestFiles.list(dir).indexOf("diagnostics-" + state + ".png") >= 0)
    }

    function makeHost(core) {
        return createTemporaryObject(panelComponent, tc, {core: core, shell: fakeShellComponent.createObject(tc)})
    }

    function test_grab_all_in_order() { // spec: ARCH-121, APP-134
        const core = fakeComponent.createObject(tc)
        const host = makeHost(core)
        load(core)
        tryVerify(() => host.panel.models.view.count === 5 && host.panel.log.view.count === 24, 5000)
        save(host, "ok")
        host.panel.scroller.contentY = Math.max(0, host.panel.scroller.contentHeight - host.panel.scroller.height)
        save(host, "ok-end")
    }

    function test_grab_problems() { // spec: ARCH-121, ARCH-127, CORE-164, APP-146
        const core = fakeComponent.createObject(tc)
        const host = makeHost(core)
        load(core, problems())
        tryVerify(() => host.panel.errors.rows.length === 2 && host.panel.models.view.count === 1, 5000)
        save(host, "problems")
    }

    function test_grab_saved_bundle() { // spec: ARCH-121
        const core = fakeComponent.createObject(tc)
        const host = makeHost(core)
        load(core)
        const dir = TestFiles.newDir("grab-bundle")
        verify(TestFiles.write(dir + "/source.zip", "bytes"))
        host.panel.exportTo("file:///" + dir + "/lilith.zip")
        answer(core, isTool("diagnostics.bundle"), {path: dir + "/source.zip", bytes: 5, files: ["manifest.json", "errors.json"], redactions: 12})
        tryVerify(() => host.panel.explorerButton.visible, 5000)
        // A grab can end up on a PR: this process's temporary folder names the account.
        host.panel.banner.text = host.panel.banner.text.split(dir).join("%TEMP%")
        save(host, "bundle-saved")
    }

    function test_grab_docked() { // spec: APP-004, ARCH-121
        const core = fakeComponent.createObject(tc)
        const shell = createTemporaryObject(shellComponent, tc, {core: core})
        shell.visible = true
        tryVerify(() => shell.docks.diagnostics.content && shell.docks.home.content && shell.docks.home.content.width > 0, 5000)
        wait(50)
        shell.docks.diagnostics.setAsCurrentTab()
        tryVerify(() => shell.docks.diagnostics.content.visible && shell.docks.diagnostics.content.width > 0, 5000)
        core.emit("engines", "event", {kind: "status", engine: engine()})
        save(shell, "docked")
        shell.destroy()
        wait(100)
    }

    function test_grab_backups() { // spec: APP-133
        const core = fakeComponent.createObject(tc)
        const host = createTemporaryObject(backupsComponent, tc, {core: core})
        save(host, "backups")
    }
}
