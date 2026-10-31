// grab_confirmations: for the reviewer, offscreen: a pending card in the panel, the shared dialog
// with a refusal showing, a cap prompt, and the confirm window with a pending and an answered
// confirmation (<grab dir>/confirmations-*.png, confirm-window.png).
import QtQuick
import QtTest
import Lilith.Core
import Lilith.UiKit
import "ConfirmShapes.js" as Shapes

TestCase {
    id: tc
    name: "GrabConfirmations"
    when: windowShown
    visible: true
    width: 1000
    height: 760

    FakeCore { id: fake }

    QtObject {
        id: fakeShim
        function activate(window, takeFocus) {}
    }

    readonly property var crane: Shapes.view({
        id: "cf_0000000000000201", tool: "asset.delete", caller_kind: "mcp", project: "sketchbook",
        arguments: {path: "C:\\art\\crane.png", note: "keep\u202Egnp.exe", dependents: 3,
                    script: "first line\nsecond line"}
    })
    readonly property var capped: Shapes.view({
        id: "cf_0000000000000202", tool: "comfy.partner_call", caller_kind: "ui:image", covers: ["cap"],
        cap: [Shapes.capHit({limit: "20", settled: "19.5", held: "0.25", estimate: "1.2"})],
        arguments: {model: "partner-video-1", seconds: 8}
    })

    Component {
        id: panelComponent
        ConfirmationsPanel {
            width: 640
            height: 700
            core: fake
            title: "Confirmations"
        }
    }

    Component {
        id: promptComponent
        Rectangle {
            property alias prompt: prompt
            width: 560
            height: 220
            color: Theme.surface
            CapPrompt {
                id: prompt
                x: 12
                y: 12
                width: 536
                core: fake
                confirmationId: tc.capped.id
                cap: tc.capped.cap
            }
        }
    }

    Component {
        id: windowComponent
        ConfirmWindow {
            core: fake
            shim: fakeShim
        }
    }

    function save(item, name) {
        const img = grabImage(item)
        const dir = TestFiles.env("LIL_GRAB_DIR") || TestFiles.env("LIL_GRAB_DEFAULT_DIR")
        verify(dir !== "", "a grab folder")
        verify(TestFiles.mkpath(dir))
        img.save(dir + "/" + name)
        verify(TestFiles.list(dir).indexOf(name) >= 0, "saved in " + dir)
    }

    function test_grab_the_panel_and_the_dialog() { // spec: APP-115
        fake.calls = []
        const panel = createTemporaryObject(panelComponent, tc)
        Shapes.answerLists(fake, [crane, capped], [])
        tryVerify(() => panel.list.loaded, 2000)
        wait(300)
        save(panel, "confirmations-pending-card.png")
        panel.showConfirmation(crane.id)
        tryVerify(() => panel.dialogPopup.opened, 2000)
        wait(600)
        // A test's click is no real pointer's: the refusal shows next to the buttons.
        mouseClick(panel.dialog.approveButton)
        wait(100)
        save(tc, "confirmations-dialog-refusal.png")
        panel.dialogPopup.close()
        tryVerify(() => !panel.dialogPopup.visible)
    }

    function test_grab_a_cap_prompt() { // spec: APP-054
        const holder = createTemporaryObject(promptComponent, tc)
        wait(300)
        save(holder, "confirmations-cap-prompt.png")
    }

    function test_grab_the_confirm_window() { // spec: APP-048
        fake.calls = []
        const w = createTemporaryObject(windowComponent, tc)
        w.visible = true
        const answered = Shapes.answeredView(Shapes.view({id: "cf_0000000000000203", tool: "script.run"}), "denied")
        Shapes.answerLists(fake, [crane], [answered])
        tryVerify(() => w.list.loaded, 2000)
        w.handleLaunchJson(JSON.stringify({kind: "confirm", id: crane.id, activate: false}))
        w.handleLaunchJson(JSON.stringify({kind: "confirm", id: answered.id, activate: false}))
        wait(300)
        save(w.contentItem, "confirm-window.png")
    }
}
