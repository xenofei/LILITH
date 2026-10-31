// The inline cap prompt (APP-054, APP-045, P4a-R10), over FakeCore with the real core's shapes:
// raise, allow once and cancel send their answers; Allow once and Raise take only a click the native
// gate passes, Cancel any source; what is sent is what the button said; and an answer given anywhere
// clears it.
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
        property int watches: 0
        function watch(item) { ++watches }
        function unwatch(item) {}
        function notePress(item) {}
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
        hand.watches = 0
        fake.connection = "ready"
        tc.Window.window.requestActivate()
        tryVerify(() => tc.Window.active, 2000, "the test window is in front")
    }

    // Scrolls down in steps shorter than the view, as a wheel does.
    function scrollThrough(s) {
        while (s.contentY + s.height < s.contentHeight - 1)
            s.contentY = Math.min(s.contentY + s.height * 0.9, s.contentHeight - s.height)
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
        wait(550)
        mouseClick(row.button)
        compare(Shapes.answers(fake).length, 0)
        verify(prompt.bar.reason.indexOf("as a number") >= 0, prompt.bar.reason)
        row.field.text = "12.50"
        wait(550)
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

    function test_the_raise_button_says_the_amount_and_the_limit_stands_beside_it() { // spec: APP-054
        const prompt = makePrompt()
        const row = prompt.raises.itemAt(0)
        compare(row.button.text, "Raise the cap", "no amount yet")
        row.field.text = "12"
        compare(row.button.text, "Raise the cap to 12.00 USD and approve")
        row.field.text = "25000000"
        compare(row.button.text, "Raise the cap to 25,000,000.00 USD and approve")
        verify(row.children[2].text.indexOf("Limit when this was asked: 1.00 USD") === 0, row.children[2].text)
    }

    function test_every_control_waits_for_the_core() { // spec: APP-034
        const prompt = makePrompt()
        const row = prompt.raises.itemAt(0)
        const controls = [prompt.cancelButton, prompt.allowOnceButton, row.field, row.button]
        row.field.text = "25"
        tryVerify(() => controls.every(c => c.enabled), 1000)
        fake.connection = "reconnecting"
        tryVerify(() => controls.every(c => !c.enabled), 1000)
        fake.connection = "ready"
        tryVerify(() => controls.every(c => c.enabled), 1000)
    }

    function test_a_raise_says_what_is_sent_and_is_higher() { // spec: APP-054, P4a-R10
        const prompt = makePrompt()
        const row = prompt.raises.itemAt(0)
        // The limit is 1 USD. Each of these keeps the plain label, and a click says why.
        for (const [amount, words] of [["25.999", "more decimals"], ["0.5", "isn't higher"], ["1", "isn't higher"],
                                       ["lots", "as a number"]]) {
            row.field.text = amount
            compare(row.button.text, "Raise the cap", amount)
            wait(550)
            mouseClick(row.button)
            verify(prompt.bar.reason.indexOf(words) >= 0, amount + ": " + prompt.bar.reason)
        }
        compare(Shapes.answers(fake).length, 0)
    }

    // Any change to the field, from a person or a program (posted WM_CHAR, UI Automation's value, which
    // both land here): the same re-arm. The owner sees the new amount on the button, the pointer must
    // come back, the half second starts again.
    function test_any_change_to_the_amount_re_arms_the_gate() { // spec: P4a-R10
        const prompt = makePrompt()
        const row = prompt.raises.itemAt(0)
        row.field.text = "25"
        wait(550)
        const watches = hand.watches
        const since = prompt.bar.visibleSinceMs
        row.field.text = "25000000"
        verify(hand.watches > watches, "the button is new to the gate")
        verify(prompt.bar.visibleSinceMs > since, "the clock starts again")
        mouseClick(row.button)
        compare(Shapes.answers(fake).length, 0, "too soon")
        verify(prompt.bar.reason.indexOf("Wait a moment") === 0, prompt.bar.reason)
        wait(550)
        mouseClick(row.button)
        compare(Shapes.answers(fake).length, 1)
        compare(Shapes.answers(fake)[0].args.cap.raise_to.amount, "25000000", "what the button said")
    }

    function test_a_change_during_the_click_refuses_it() { // spec: P4a-R10
        const prompt = makePrompt()
        const row = prompt.raises.itemAt(0)
        row.field.text = "25"
        wait(550)
        mousePress(row.button)
        row.field.text = "25000000"
        mouseRelease(row.button)
        compare(Shapes.answers(fake).length, 0, "the amount at the press is gone")
        verify(prompt.bar.reason.indexOf("changed while it was being clicked") >= 0, prompt.bar.reason)
    }

    function test_a_change_needs_the_real_gate_again() { // spec: P4a-R10
        // Against the real gate the new serial means no token: the replay of that is
        // refuses_a_moving_hand_on_a_button_that_changed_under_it in crate::pointer_gate.
        const prompt = makePrompt(realGatePrompt)
        const row = prompt.raises.itemAt(0)
        const before = ConfirmInput.takeClick(row.button)
        compare(before, "not_pointer")
        row.field.text = "25"
        wait(550)
        mouseClick(row.button)
        compare(Shapes.answers(fake).length, 0)
    }

    function test_allow_once_and_raise_need_the_real_pointer_and_cancel_doesnt() { // spec: P4a-R10
        const prompt = makePrompt(realGatePrompt)
        const row = prompt.raises.itemAt(0)
        row.field.text = "25"
        wait(550)
        mouseClick(prompt.allowOnceButton)
        mouseClick(row.button)
        compare(Shapes.answers(fake).length, 0, "a test's click is no real pointer's")
        verify(prompt.bar.reason.indexOf("with the pointer") >= 0, prompt.bar.reason)
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

    // An agent's request that also passes a cap: the buttons say they approve it, by name.
    function test_a_confirmation_covering_both_says_what_the_buttons_approve() { // spec: APP-045
        const both = Shapes.view({id: "cf_00000000000000f2", tool: "asset\u202E.delete", caller_kind: "mcp",
                                  covers: ["confirm", "cap"], cap: [Shapes.capHit()], arguments: {path: "C:\\art"}})
        const panel = createTemporaryObject(panelComponent, tc)
        Shapes.answerLists(fake, [both], [])
        tryVerify(() => panel.list.loaded, 2000)
        panel.showConfirmation(both.id)
        tryVerify(() => panel.dialogPopup.opened, 2000)
        const prompt = panel.dialog.capPrompt
        verify(prompt)
        compare(prompt.allowOnceButton.text, "Allow once and approve asset⟨U+202E⟩.delete")
        const row = prompt.raises.itemAt(0)
        row.field.text = "12"
        compare(row.button.text, "Raise the cap to 12.00 USD and approve asset⟨U+202E⟩.delete")
        verify(!panel.dialog.approveButton.visible, "no plain Approve beside them")
        tryVerify(() => prompt.bar.inFront, 2000)
        wait(550)
        mouseClick(prompt.allowOnceButton)
        compare(Shapes.answers(fake)[0].args, {confirmation_id: both.id, answer: "approve", cap: {action: "allow_once"}})
        panel.dialogPopup.close()
        tryVerify(() => !panel.dialogPopup.visible)
    }

    function test_a_cap_in_the_dialog_waits_for_the_whole_action_too() { // spec: APP-115
        let lines = []
        for (let i = 0; i < 200; ++i)
            lines.push("line " + i)
        const both = Shapes.view({id: "cf_00000000000000f3", covers: ["confirm", "cap"], cap: [Shapes.capHit()],
                                  arguments: {script: lines.join("\n")}})
        const panel = createTemporaryObject(panelComponent, tc)
        Shapes.answerLists(fake, [both], [])
        tryVerify(() => panel.list.loaded, 2000)
        panel.showConfirmation(both.id)
        tryVerify(() => panel.dialogPopup.opened, 2000)
        const dialog = panel.dialog
        tryVerify(() => dialog.scroller.overflows, 2000)
        const prompt = dialog.capPrompt
        verify(!prompt.allowOnceButton.enabled && !prompt.raises.itemAt(0).button.enabled, "off")
        verify(prompt.cancelButton.enabled, "Cancel works")
        verify(prompt.blockedLine.visible, "it says why")
        scrollThrough(dialog.scroller)
        tryVerify(() => prompt.allowOnceButton.enabled, 1000)
        panel.dialogPopup.close()
        tryVerify(() => !panel.dialogPopup.visible)
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
