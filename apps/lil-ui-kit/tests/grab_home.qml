// grab_home: Home offscreen, saved as <grab dir>/home-<state>.png (LIL_GRAB_DIR, else CMake's
// build/<preset>/grabs) for the reviewer, on FakeCore's answers in the core's shapes: in the shell's
// Default layout with the While you were away card and the Opened card, the first run, no
// workspace, a failed read, and the core away. Each grab must have the window's size.
import QtQuick
import QtTest
import Lilith.Core
import Lilith.UiKit
import "CoreShapes.js" as Shapes

TestCase {
    id: tc
    name: "GrabHome"
    when: windowShown
    width: 200
    height: 100

    Component {
        id: fakeComponent
        FakeCore { project: "sketchbook" }
    }

    Component {
        id: shellComponent
        ShellWindow {
            shim: QtObject {
                function activate(window, takeFocus) {}
            }
        }
    }

    readonly property var fullScene: ({
        projects: [{project: "sketchbook", name: "Sketchbook", status: "active", assets: 1234},
                   {project: "lantern", name: "Lantern", status: "paused", assets: 3}],
        status: Shapes.projectStatus("sketchbook", {candidate: 4, in_review: 2, stale: 1, approved: 30}),
        assets: [Shapes.asset("sketchbook", "chr_paper_crane", "candidate", "2026-10-30T13:00:05Z"),
                 Shapes.asset("sketchbook", "bg_harbor_dusk", "in_review", "2026-10-30T11:40:00Z"),
                 Shapes.asset("lantern", "prop_lantern", "approved", "2026-10-29T09:00:00Z")],
        jobs: [Shapes.job("j_00000000000000d1", "running", {project: "sketchbook"}),
               Shapes.job("j_00000000000000d2", "queued", {project: "lantern", queue_position: 0}),
               Shapes.job("j_00000000000000a1", "succeeded", {updated: "2026-10-30T13:00:05Z", project: "sketchbook"}),
               Shapes.job("j_00000000000000a2", "failed", {updated: "2026-10-30T13:00:04Z"}),
               Shapes.job("j_00000000000000a4", "queued", {updated: "2026-10-20T11:00:00Z", substatus: "queue_paused",
                                                           pause_reasons: ["restart"], project: "sketchbook"})],
        pending: [Shapes.confirmation("cf_00000000000000b1", "comfy.run", "2026-10-30T13:00:01Z")]
    })

    function init() {
        wait(100)
    }

    function answerAll(core, s, answered) {
        for (const c of core.calls) {
            if (answered[c.id])
                continue
            let outcome = null
            if (s.fail && s.fail[c.name])
                outcome = s.fail[c.name]
            else if (c.name === "projects.list")
                outcome = Shapes.tool({projects: s.projects})
            else if (c.name === "project.status")
                outcome = s.status
            else if (c.name === "assets.query")
                outcome = Shapes.assetsPage(s.assets)
            else if (c.name === "jobs.list")
                outcome = core.jobsListAnswer(c.args, s.jobs, [Shapes.lane(), Shapes.lane({lane: "cpu"})])
            else if (c.name === "app/confirmations.list")
                outcome = Shapes.confirmations(s.pending)
            if (outcome) {
                answered[c.id] = true
                core.respond(c.id, outcome)
            }
        }
    }

    // The shell on `workspace`, its reads answered from `s`, then `before(shell)`, then the grab.
    function grab(name, workspace, lastSeen, s, before) {
        const core = createTemporaryObject(fakeComponent, tc, {workspace: workspace})
        const dir = TestFiles.newDir("grab-home")
        if (lastSeen)
            compare(ShellState.setLastSeen(dir, workspace.id, lastSeen), "")
        const shell = createTemporaryObject(shellComponent, tc, {core: core, stateDir: dir})
        shell.visible = true
        tryVerify(() => shell.docks.home.content && shell.docks.home.content.width > 0, 5000)
        const panel = shell.docks.home.content
        const answered = {}
        tryVerify(() => {
            answerAll(core, s, answered)
            return panel._open === 0 && !panel.anyDue()
        }, 5000, "the reads are in")
        if (before)
            before(shell, core)
        wait(300)
        const img = grabImage(shell.contentItem)
        compare(img.width, Math.round(shell.width * shell.devicePixelRatio))
        const out = TestFiles.env("LIL_GRAB_DIR") || TestFiles.env("LIL_GRAB_DEFAULT_DIR")
        verify(out !== "", "a grab folder")
        verify(TestFiles.mkpath(out))
        img.save(out + "/" + name)
        verify(TestFiles.list(out).indexOf(name) >= 0, "saved in " + out)
        shell.destroy()
    }

    readonly property var ws: ({id: "ws1", path: "D:/art/harbor"})

    function test_grab_home_away() { // spec: APP-111, P4a-R21
        grab("home-away.png", ws, "2026-10-30T12:00:00Z", fullScene, shell => {
            shell.handleLaunchJson(JSON.stringify({kind: "target", target: "C:\\art\\crane.png", intent: "edit", focus: false, activate: false}))
        })
    }

    function test_grab_home_first_run() { // spec: APP-111
        grab("home-first-run.png", ws, "", fullScene)
    }

    function test_grab_home_empty_workspace() { // spec: APP-111
        grab("home-empty-workspace.png", ws, "", {projects: [], assets: [], jobs: [], pending: [],
                                                  status: Shapes.refusal("project.unknown", "No such project.")})
    }

    function test_grab_home_no_workspace() { // spec: APP-016
        grab("home-no-workspace.png", null, "", fullScene)
    }

    function test_grab_home_read_failed() { // spec: APP-111
        const refused = Shapes.refusal("store.io", "The workspace's index couldn't be read.")
        grab("home-read-failed.png", ws, "2026-10-30T12:00:00Z", fullScene, (shell, core) => {
            core.emit("jobs", "resync_required", {})
            const bad = Object.assign({}, fullScene, {fail: {"projects.list": refused, "project.status": refused,
                                                             "assets.query": refused, "jobs.list": refused,
                                                             "app/confirmations.list": refused}})
            // The calls just asked; the shell's other panels' calls that are still out, too.
            const answered = {}
            for (const c of core.calls)
                answered[c.id] = !(c.id in core._open)
            tryVerify(() => {
                answerAll(core, bad, answered)
                return shell.docks.home.content.readState.projects === "failed"
            }, 5000)
        })
    }

    function test_grab_home_keyboard() { // spec: APP-156
        const jobs = []
        for (let i = 0; i < 25; ++i)
            jobs.push(Shapes.job("j_00000000000" + String(10000 + i), "succeeded", {updated: "2026-10-30T13:00:05Z", project: "sketchbook"}))
        grab("home-keyboard.png", ws, "2026-10-30T12:00:00Z", Object.assign({}, fullScene, {jobs: jobs}), shell => {
            // The scroll bar, the focus ring, and Tab bringing the card's button into view.
            const panel = shell.docks.home.content
            shell.requestActivate()
            tryVerify(() => shell.active, 3000)
            panel.scroller.forceActiveFocus()
            tryVerify(() => panel.scrollBar.visible && panel.focusRing.visible, 3000, "the bar and the ring")
            wait(100)
            const ring = grabImage(shell.contentItem)
            const out = TestFiles.env("LIL_GRAB_DIR") || TestFiles.env("LIL_GRAB_DEFAULT_DIR")
            verify(TestFiles.mkpath(out))
            ring.save(out + "/home-keyboard-ring.png")
            keyClick(Qt.Key_Tab)
            tryVerify(() => panel.awayCard.dismissButton.activeFocus, 2000)
        })
    }

    function test_grab_home_core_away() { // spec: APP-034
        grab("home-core-away.png", ws, "2026-10-30T12:00:00Z", fullScene, (shell, core) => {
            core.applyState({connection: "reconnecting"})
        })
    }
}
