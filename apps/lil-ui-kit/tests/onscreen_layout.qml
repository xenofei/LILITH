// KDDockWidgets #757 on the windows platform (label onscreen; it shows windows, so it runs only
// in a hands-off stretch): a maximized save, a restore and a save again, three times, keep the
// docked widths within a few pixels. Offscreen, tst_layout's round trip covers the logic but not
// the platform's maximize.
//   pwsh -NoProfile -File tools/dev-shell.ps1 ctest --test-dir build/dev -L onscreen
import QtQuick
import QtTest
import Lilith.UiKit

TestCase {
    id: tc
    name: "OnscreenLayout"
    when: windowShown

    FakeCore {
        id: fake
        project: "sketchbook"
    }

    Component {
        id: shellComponent
        ShellWindow { core: fake }
    }

    function sizes(shell) {
        const out = {}
        for (const id of ["home", "jobs", "hardware"])
            out[id] = {w: shell.docks[id].content.width, h: shell.docks[id].content.height}
        return out
    }

    function test_a_maximized_round_trip_keeps_the_docked_widths() { // spec: APP-002, APP-003
        const dir = TestFiles.newDir("onscreen")
        let shell = createTemporaryObject(shellComponent, tc, {stateDir: dir})
        shell.visible = true
        tryVerify(() => shell.docks.home.content && shell.docks.home.content.width > 0, 5000)
        shell.width = 1000
        shell.height = 640
        wait(300)
        shell.showMaximized()
        tryVerify(() => shell.visibility === Window.Maximized, 5000, "maximized")
        wait(500)
        const before = sizes(shell)
        console.log("757 saved maximized " + shell.width + "x" + shell.height + ": " + JSON.stringify(before))
        shell.close()
        const saved = JSON.parse(TestFiles.read(dir + "/layout.json"))
        console.log("757 saved normal geometry " + JSON.stringify(saved.windows[0].geometry))
        verify(saved.windows[0].maximized, "saved maximized")
        verify(saved.windows[0].geometry.w < shell.width, "with a smaller normal geometry")
        shell.destroy()
        wait(200)
        for (let round = 1; round <= 3; ++round) {
            shell = createTemporaryObject(shellComponent, tc, {stateDir: dir})
            compare(shell.layoutOpened, "restored")
            shell.visible = true
            tryVerify(() => shell.visibility === Window.Maximized, 5000, "restored maximized")
            wait(500)
            const now = sizes(shell)
            console.log("757 round " + round + " " + shell.width + "x" + shell.height + ": " + JSON.stringify(now))
            for (const id in before) {
                verify(Math.abs(now[id].w - before[id].w) <= 4 && Math.abs(now[id].h - before[id].h) <= 4,
                       "round " + round + ": " + id + " was " + JSON.stringify(before[id])
                       + ", is " + JSON.stringify(now[id]))
            }
            shell.docks.jobs.setAsCurrentTab()   // a change, so the close saves again
            shell.close()
            shell.destroy()
            wait(200)
        }
    }
}
