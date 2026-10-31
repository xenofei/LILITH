// The inline cap prompt (APP-054, APP-045, P4a-R10), over FakeCore with the real core's shapes:
// raise, allow once and cancel send their answers; Allow once and Raise take only a click the native
// gate passes, Cancel any source; and an answer given anywhere clears it.
import QtQuick
import QtTest
import Lilith.Core
import Lilith.UiKit
import "ConfirmShapes.js" as Shapes

TestCase {
    id: tc
    name: "CapPrompt"
    when: windowShown
    visible: true
    width: 700
    height: 500

    FakeCore { id: fake }

    QtObject {
        id: hand
        property string verdict: ""
        function watch(item) {}
        function unwatch(item) {}
        function takeClick(item) { return verdict }
    }

    readonly property var capped: Shapes.view({
        id: "cf_00000000000000f1", tool: "test.destructive", caller_kind: "ui:image",
        covers: ["cap"], cap: [Shapes.capHit()], arguments: {cost: "5"}
    })

    Component {
        id: promptComponent
        CapPrompt {
            width: 600
            core: fake
            confirmationId: tc.capped.id
            cap: tc.capped.cap
            pointerSource: hand
        }
    }

    Component {
        id: realGatePrompt
        CapPrompt {
            width: 600
            core: fake
            confirmationId: tc.capped.id
            cap: tc.capped.cap
        }
    }

    Component {
        id: panelComponent
        ConfirmationsPanel {
            width: 600
            height: 400
            core: fake
            title: "Confirmations"
            pointerSource: hand
        }
    }

    A11yWalk { id: walk }

    function init() {
        fake.calls = []
        hand.verdict = ""
        fake.connection = "ready"
        tc.Window.window.requestActivate()
        tryVerify(() => tc.Window.active, 2000, "the test window is in front")
    }

    function makePrompt(component) {
        const prompt = createTemporaryObject(component || promptComponent, tc)
        verify(prompt)
        tryVerify(() => prompt.bar.inFront, 2000, "its window is in front")
        tryVerify(() => prompt.bar.visibleSinceMs >= 0, 2000)
        wait(550)
        return prompt
    }

    function test_raise_allow_once_and_cancel_send_their_answers() { // spec: APP-054
        let prompt = makePrompt()
        mouseClick(prompt.allowOnceButton)
        compare(Shapes.answers(fake).length, 1)
        compare(Shapes.answers(fake)[0].args,
                {confirmation_id: capped.id, answer: "approve", cap: {action: "allow_once"}})
        fake.respond(Shapes.answers(fake)[0].id, Shapes.answerResult(Shapes.answeredView(capped, "approved")))
        verify(!prompt.visible, "an answer clears it")
        prompt.destroy()
        wait(0)

        fake.calls = []
        prompt = makePrompt()
        const row = prompt.raises.itemAt(0)
        // Not a number: nothing goes out, and it says why.
        row.field.text = "lots"
        mouseClick(row.button)
        compare(Shapes.answers(fake).length, 0)
        verify(prompt.bar.reason.indexOf("as a number") >= 0, prompt.bar.reason)
        row.field.text = "12.50"
        mouseClick(row.button)
        compare(Shapes.answers(fake).length, 1)
        compare(Shapes.answers(fake)[0].args,
                {confirmation_id: capped.id, answer: "approve",
                 cap: {action: "raise", raise_to: {cap: "monthly", amount: "12.50"}}})
        prompt.destroy()
        wait(0)

        fake.calls = []
        prompt = makePrompt()
        mouseClick(prompt.cancelButton)
        compare(Shapes.answers(fake).length, 1)
        compare(Shapes.answers(fake)[0].args, {confirmation_id: capped.id, answer: "deny"})
        prompt.destroy()
    }

    function test_allow_once_and_raise_need_the_real_pointer_and_cancel_doesnt() { // spec: P4a-R10
        const prompt = makePrompt(realGatePrompt)
        const row = prompt.raises.itemAt(0)
        row.field.text = "25"
        mouseClick(prompt.allowOnceButton)
        mouseClick(row.button)
        compare(Shapes.answers(fake).length, 0, "a test's click is no real pointer's")
        verify(prompt.bar.reason.indexOf("Raise the cap with the pointer") >= 0, prompt.bar.reason)
        // Keys and UI Automation say why too.
        prompt.allowOnceButton.forceActiveFocus(Qt.TabFocusReason)
        keyClick(Qt.Key_Return)
        keyClick(Qt.Key_Space)
        verify(TestInput.accessiblePress(prompt.allowOnceButton))
        verify(TestInput.accessiblePress(row.button))
        compare(Shapes.answers(fake).length, 0)
        // Cancel takes any source.
        verify(TestInput.accessiblePress(prompt.cancelButton))
        compare(Shapes.answers(fake).length, 1)
        compare(Shapes.answers(fake)[0].args.answer, "deny")
    }

    function test_cancel_has_the_focus_first_and_a_key_cancels() { // spec: P4a-R10
        const prompt = makePrompt()
        prompt.focusCancel()
        verify(prompt.cancelButton.activeFocus)
        keyClick(Qt.Key_Return)
        compare(Shapes.answers(fake).length, 1)
        compare(Shapes.answers(fake)[0].args.answer, "deny")
    }

    function test_a_held_key_never_answers() { // spec: P4a-R10
        const prompt = makePrompt()
        prompt.focusCancel()
        // Repeats of a key held since before the prompt came up, then its release.
        for (let i = 0; i < 3; ++i) {
            TestInput.keyRepeat(prompt.Window.window, Qt.Key_Return)
            TestInput.keyRepeat(prompt.Window.window, Qt.Key_Space)
        }
        keyRelease(Qt.Key_Space)
        compare(Shapes.answers(fake).length, 0)
    }

    function test_answering_in_the_panel_clears_the_inline_prompt() { // spec: APP-054
        const prompt = makePrompt()
        compare(OpenPrompts.count, 1)
        const panel = createTemporaryObject(panelComponent, tc)
        Shapes.answerLists(fake, [capped], [])
        tryVerify(() => panel.list.loaded, 2000)
        panel.showConfirmation(capped.id)
        tryVerify(() => panel.dialogPopup.opened, 2000)
        verify(panel.dialog.capPrompt, "the dialog answers through the cap prompt")
        tryVerify(() => panel.dialog.capPrompt.bar.inFront, 2000)
        wait(550)
        mouseClick(panel.dialog.capPrompt.allowOnceButton)
        const sent = Shapes.answers(fake)
        compare(sent.length, 1)
        compare(sent[0].args.cap, {action: "allow_once"})
        const answered = Shapes.answeredView(capped, "approved")
        fake.respond(sent[0].id, Shapes.answerResult(answered))
        fake.emit("confirmations", "event", Shapes.event("answered", answered))
        verify(!prompt.visible, "the inline prompt is cleared")
        verify(!prompt.bar.live)
        tryCompare(panel.list.pendingModel, "count", 0)
        panel.dialogPopup.close()
        tryVerify(() => !panel.dialogPopup.visible)
        compare(OpenPrompts.count, 0, "and the shell's shortcuts are back")
    }

    function test_a_resync_clears_a_prompt_whose_answer_was_missed() { // spec: APP-054
        const prompt = makePrompt()
        fake.emit("confirmations", "resync_required", null)
        const list = fake.calls.filter(c => c.name === "app/confirmations.list")
        compare(list.length, 1)
        fake.respond(list[0].id, Shapes.listAnswer([]))
        verify(!prompt.visible)
    }

    function test_it_shows_the_cap_in_money_text_and_passes_the_walk() { // spec: APP-054, APP-156
        const prompt = makePrompt()
        verify(prompt.hitWords(capped.cap[0]).indexOf("The monthly cap is 1.00 USD") === 0,
               prompt.hitWords(capped.cap[0]))
        compare(walk.report(prompt), [])
        verify(walk.interactiveCount(prompt) >= 4, "Cancel, Allow once, the field and Raise")
    }
}
