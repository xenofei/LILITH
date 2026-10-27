// grab_shell: the shell's Default layout, offscreen, saved as <grab dir>/shell-default.png
// (LIL_GRAB_DIR, else CMake's build/<preset>/grabs) for the reviewer. The grab must have the
// window's size and not be one flat colour.
import QtQuick
import QtTest
import Lilith.UiKit

TestCase {
    id: tc
    name: "GrabShell"
    when: windowShown
    width: 200
    height: 100

    FakeCore {
        id: fake
        project: "sketchbook"
    }

    Component {
        id: shellComponent
        ShellWindow { core: fake }
    }

    // Distinct colours on a 32 x 32 sample grid.
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

    function test_grab_shell_default_layout() { // spec: APP-004, APP-012
        const shell = createTemporaryObject(shellComponent, tc)
        shell.visible = true
        tryVerify(() => shell.docks.home.content && shell.docks.home.content.width > 0, 5000)
        wait(300)
        const img = grabImage(shell.contentItem)
        const dpr = shell.devicePixelRatio
        compare(img.width, Math.round(shell.width * dpr), "the grab has the window's width")
        compare(img.height, Math.round(shell.height * dpr), "the grab has the window's height")
        const colours = distinctColours(img)
        verify(colours >= 4, "the grab has " + colours + " distinct colours")
        const dir = TestFiles.env("LIL_GRAB_DIR") || TestFiles.env("LIL_GRAB_DEFAULT_DIR")
        verify(dir !== "", "a grab folder")
        verify(TestFiles.mkpath(dir))
        img.save(dir + "/shell-default.png")
        verify(TestFiles.list(dir).indexOf("shell-default.png") >= 0, "saved in " + dir)
    }
}
