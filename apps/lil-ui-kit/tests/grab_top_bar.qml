// grab_top_bar: the shell with its top bar, offscreen, saved in <grab dir> (LIL_GRAB_DIR, else
// CMake's build/<preset>/grabs) for the reviewer: top-bar-ready.png (a paused lane, counts, spend,
// GPU), top-bar-workspaces.png (the switcher's list open), top-bar-reconnecting.png and
// top-bar-no-workspace.png.
import QtQuick
import QtTest
import Lilith.UiKit

TestCase {
    id: tc
    name: "GrabTopBar"
    when: windowShown
    width: 200
    height: 100

    FakeCore {
        id: fake
        workspace: ({id: "ws1", path: "D:/art/harbor"})
    }

    Component {
        id: shellComponent
        ShellWindow { core: fake }
    }

    property var answered: ({})

    function ok(structured) {
        return {status: "ok", structured: structured}
    }

    function answerAll(name, outcome) {
        for (const c of fake.calls.filter(c => c.name === name && !answered[c.id])) {
            answered[c.id] = true
            fake.respond(c.id, outcome(c))
        }
    }

    function job(id, status) {
        return {job_id: id, kind: "comfy.prompt", lane: "gpu-heavy", priority: "user", status: status,
                substatus: status === "running" ? "executing" : "waiting", created: "2026-10-30T12:00:00Z",
                updated: "2026-10-30T12:00:01Z", attempt: 1, trace_id: "t_" + id, batch_id: "b_" + id, index: 0,
                origin: {surface: "panel", id: "image"}}
    }

    function save(shell, name) {
        wait(200)
        const img = grabImage(shell.contentItem)
        const dir = TestFiles.env("LIL_GRAB_DIR") || TestFiles.env("LIL_GRAB_DEFAULT_DIR")
        verify(dir !== "", "a grab folder")
        verify(TestFiles.mkpath(dir))
        img.save(dir + "/" + name)
        verify(TestFiles.list(dir).indexOf(name) >= 0, "saved in " + dir)
    }

    function test_grab_the_top_bar() { // spec: APP-006, APP-016, APP-034
        const shell = createTemporaryObject(shellComponent, tc)
        shell.visible = true
        tryVerify(() => shell.layoutSettled, 5000, "the docks are laid out")
        const lanes = [{lane: "gpu-heavy", paused: true, reasons: ["manual"], held: 3, restart_held: 0, withdrawn: 0}]
        answerAll("jobs.list", c => ok({jobs: c.args.status === "running" ? [job("r1", "running"), job("r2", "running")]
                                            : c.args.order === "queue" ? [job("q1", "queued"), job("q2", "queued"), job("q3", "queued")]
                                            : [],
                                        next_cursor: null, lanes: lanes}))
        answerAll("caps.get", () => ok({display_currency: "USD", period: "2026-10",
                                        caps: [{cap: "monthly", limit: "100", settled: "41.2", held: "0", ratio_permille: 412}]}))
        fake.emit("telemetry", "event", {kind: "sample", snapshot: {ts: "2026-10-30T12:00:00Z", gpu: {
            name: "GPU", util_pct: 63, vram_used_mib: 9728, vram_total_mib: 16384, temp_c: 71, measured: true}}})
        tryCompare(shell.topBar.jobsIndicator, "text", "2 running · 3 queued")
        tryCompare(shell.topBar.spendIndicator, "text", "41 % of cap")
        tryCompare(shell.topBar.pausedBadge, "text", "Paused by you")
        save(shell, "top-bar-ready.png")

        const sw = shell.topBar.workspaceSwitcher
        sw.open()
        answerAll("workspaces.list", () => ok({workspaces: [
            {id: "ws1", path: "D:/art/harbor", name: "Harbor", default: true, served: true, missing: false},
            {id: "ws3", path: "D:/art/other", name: "Other", default: false, served: false, missing: false},
            {id: "ws4", path: "D:/art/gone", default: false, served: false, missing: true}]}))
        tryVerify(() => sw.popup.view.count === 3)
        save(shell, "top-bar-workspaces.png")
        sw.popup.close()
        tryVerify(() => !sw.popup.visible)

        fake.connection = "reconnecting"
        save(shell, "top-bar-reconnecting.png")

        fake.connection = "ready"
        fake.workspace = null
        verify(shell.noWorkspaceBanner.visible)
        save(shell, "top-bar-no-workspace.png")
    }
}
