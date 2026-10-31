// A launch through lil-image's own path to the shell (APP-014, P4a-R21; review I-5): this process
// claims the app's pipe as main.cpp's step 3b does (on a test channel of its own), a second
// launch (this runner started again with --forward-launch) hands its arguments over and exits 0,
// and the bridge's router, launch sink and the shell's handleLaunchJson show it in the Opened
// card. After the instance's end, a launch is the instance itself.
import QtQuick
import QtTest
import Lilith.UiKit

TestCase {
    id: tc
    name: "Launch"
    when: windowShown
    width: 200
    height: 100

    FakeCore {
        id: fake
        project: "sketchbook"
    }

    QtObject {
        id: fakeShim
        property var activations: []
        function activate(window, takeFocus) {
            activations = activations.concat([{window: window, takeFocus: takeFocus}])
        }
    }

    Component {
        id: shellComponent
        ShellWindow {
            core: fake
            shim: fakeShim
        }
    }

    // `<scheme>:open/<base64url of text, unpadded>` for this process's test channel.
    function openUrl(text) {
        const b64 = Qt.btoa(text).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "")
        return "lil-" + TestFiles.env("LIL_CHANNEL") + ":open/" + b64
    }

    function test_a_second_launch_reaches_the_opened_card() { // spec: APP-013, APP-014, P4a-R21
        verify(TestLaunch.useTestChannel(), "a test channel")
        compare(TestLaunch.claim(""), -1, "this process is the instance")
        const shell = createTemporaryObject(shellComponent, tc)
        shell.visible = true
        tryVerify(() => shell.docks.home.content && shell.docks.home.content.width > 0, 5000)
        TestLaunch.deliverTo(shell)
        // This start's own launch was shown by the start: not activated again.
        wait(200)
        compare(fakeShim.activations.length, 0)
        verify(!shell.openedCard.visible)
        compare(TestLaunch.secondLaunch("--intent edit " + openUrl("C:\\art\\crane.png")), 0,
                "the second launch hands over and exits 0")
        tryVerify(() => shell.openedCard.visible, 5000, "the Opened card shows the launch")
        compare(shell.openedCard.targetText, "C:\\art\\crane.png")
        compare(shell.openedCard.intentText, "Edit")
        compare(fakeShim.activations.length, 1)
        verify(fakeShim.activations[0].window === shell && fakeShim.activations[0].takeFocus,
               "the window comes forward")
        // --no-activate and a refused link, the same way.
        compare(TestLaunch.secondLaunch("--no-activate lil-test-nope:open/x"), 0)
        tryVerify(() => shell.openedCard.message !== "", 5000, "the refusal shows")
        verify(shell.openedCard.message.indexOf("This link isn't one LILITH opens") === 0,
               shell.openedCard.message)
        compare(fakeShim.activations.length, 2)
        compare(fakeShim.activations[1].takeFocus, false, "it only flashes")

        // The instance's end: from then on a launch is the instance itself.
        TestLaunch.release()
        compare(TestLaunch.secondLaunch(""), 3, "a launch after the end becomes the instance")
    }
}
