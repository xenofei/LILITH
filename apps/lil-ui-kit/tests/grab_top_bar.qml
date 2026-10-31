// grab_top_bar: the shell with its top bar, offscreen, saved in <grab dir> (LIL_GRAB_DIR, else
// CMake's build/<preset>/grabs) for the reviewer: top-bar-ready.png (a paused lane, counts, spend,
// GPU), top-bar-workspaces.png (the switcher's list open), top-bar-reconnecting.png,
// top-bar-refused.png (a hello refused over its workspace) and top-bar-no-workspace.png.
import QtQuick
import QtTest
import Lilith.UiKit
import "CoreShapes.js" as Shapes

TestCase {
    id: tc
    name: "GrabTopBar"
    when: windowShown
    width: 200
    height: 100

    readonly property var workspaceOne: ({id: "ws1", path: "D:/art/harbor"})

    FakeCore {
        id: fake
        workspace: tc.workspaceOne
    }

    Component {
        id: shellComponent
        ShellWindow { core: fake }
    }

    property var answered: ({})

    function answerAll(name, outcome) {
        for (const c of fake.calls.filter(c => c.name === name && !answered[c.id])) {
            answered[c.id] = true
            fake.respond(c.id, outcome(c))
        }
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
        const lanes = [Shapes.lane({paused: true, reasons: ["manual"], held: 3}), Shapes.lane({lane: "cpu"})]
        answerAll("jobs.list", c => Shapes.jobsPage(c.args.status === "running" ? [Shapes.job("r1", "running"), Shapes.job("r2", "running")]
                                                    : c.args.order === "queue" || c.args.status === "queued" ? [Shapes.job("q1", "queued"), Shapes.job("q2", "queued"), Shapes.job("q3", "queued")]
                                                    : [], lanes))
        answerAll("caps.get", () => Shapes.tool(Shapes.capsView([Shapes.capRow("monthly", 412)])))
        answerAll("app/set_context", () => Shapes.context(tc.workspaceOne, null, "sketchbook"))
        fake.emit("telemetry", "event", Shapes.sample(Shapes.gpu()))
        tryCompare(shell.topBar.jobsIndicator, "text", "2 running · 3 queued")
        tryCompare(shell.topBar.spendIndicator, "text", "41 % of cap")
        tryCompare(shell.topBar.pausedBadge, "text", "GPU work paused by you")
        tryCompare(shell.topBar.projectSwitcher.button, "text", "sketchbook ▾")
        save(shell, "top-bar-ready.png")

        const sw = shell.topBar.workspaceSwitcher
        sw.open()
        answerAll("workspaces.list", () => Shapes.tool({workspaces: Shapes.workspaceRows().slice(0, 4)}))
        tryVerify(() => sw.popup.view.count === 4)
        save(shell, "top-bar-workspaces.png")
        sw.popup.close()
        tryVerify(() => !sw.popup.visible)

        fake.applyState({connection: "reconnecting"})
        save(shell, "top-bar-reconnecting.png")

        fake.applyState({connection: "unavailable", unavailable: Shapes.refusedHello(
            "workspace.unsupported", "This core serves only the default workspace, \"D:/art/quay\". To use \"D:/art/harbor\" instead, make it the default with `lil workspace use \"D:/art/harbor\"`.")})
        save(shell, "top-bar-refused.png")

        fake.applyState({connection: "ready", workspace: null})
        verify(shell.noWorkspaceBanner.visible)
        save(shell, "top-bar-no-workspace.png")
    }
}
