// grab_open_card: the shell with the Opened card (P4a-R21), offscreen, for the reviewer: a
// `.lilref` target (<grab dir>/shell-opened.png) and a refused link (shell-opened-refused.png).
import QtQuick
import QtTest
import Lilith.UiKit

TestCase {
    id: tc
    name: "GrabOpenCard"
    when: windowShown
    width: 200
    height: 100

    FakeCore {
        id: fake
        project: "sketchbook"
    }

    QtObject {
        id: fakeShim
        function activate(window, takeFocus) {}
    }

    Component {
        id: shellComponent
        ShellWindow {
            core: fake
            shim: fakeShim
        }
    }

    function init() {
        wait(100)
    }

    function grab(fields, name) {
        const shell = createTemporaryObject(shellComponent, tc)
        shell.visible = true
        tryVerify(() => shell.docks.home.content && shell.docks.home.content.width > 0, 5000)
        shell.handleLaunchJson(JSON.stringify(Object.assign({intent: "open", focus: true, activate: true},
                                                            fields)))
        tryVerify(() => shell.openedCard.visible, 2000)
        wait(300)
        const img = grabImage(shell.contentItem)
        compare(img.width, Math.round(shell.width * shell.devicePixelRatio))
        const dir = TestFiles.env("LIL_GRAB_DIR") || TestFiles.env("LIL_GRAB_DEFAULT_DIR")
        verify(dir !== "", "a grab folder")
        verify(TestFiles.mkpath(dir))
        img.save(dir + "/" + name)
        verify(TestFiles.list(dir).indexOf(name) >= 0, "saved in " + dir)
        shell.destroy()
    }

    function test_grab_opened_lilref() { // spec: APP-021
        grab({kind: "lilref", title: "Paper crane",
              target: "lil://0123456789abcdef/sketchbook/chr_paper_crane@1.0.0#sha256="
                      + "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"},
             "shell-opened.png")
    }

    function test_grab_opened_refused() { // spec: APP-020
        grab({kind: "refused", message: "This link isn't one LILITH opens: lil-dev:open/@@@"},
             "shell-opened-refused.png")
    }
}
