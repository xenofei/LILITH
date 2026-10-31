// The Confirmations panel and the shared dialog (APP-044, APP-046, APP-054, APP-115, P4a-R10), over
// FakeCore with the real core's shapes (ConfirmShapes.js). `hand` stands in for the native gate where
// a test needs a click the hand made; the tests that say otherwise use the real one (ConfirmInput),
// to which a test's click is what any program's is: not a real pointer.
import QtQuick
import QtTest
import Lilith.Core
import Lilith.Shim
import Lilith.UiKit
import "ConfirmShapes.js" as Shapes

TestCase {
    id: tc
    name: "Confirmations"
    when: windowShown
    visible: true
    width: 900
    height: 700

    FakeCore { id: fake }

    QtObject {
        id: hand
        property string verdict: ""
        property int taken: 0
        function watch(item) {}
        function unwatch(item) {}
        function takeClick(item) {
            ++taken
            return verdict
        }
    }

    QtObject {
        id: fakeShim
        function activate(window, takeFocus) {}
    }

    Component {
        id: panelComponent
        ConfirmationsPanel {
            width: 800
            height: 640
            core: fake
            title: "Confirmations"
            pointerSource: hand
        }
    }

    Component {
        id: realGatePanel
        ConfirmationsPanel {
            width: 800
            height: 640
            core: fake
            title: "Confirmations"
        }
    }

    Component {
        id: shellComponent
        ShellWindow {
            core: fake
            shim: fakeShim
        }
    }

    A11yWalk { id: walk }

    function init() {
        fake.calls = []
        hand.verdict = ""
        hand.taken = 0
        fake.connection = "ready"
        tc.Window.window.requestActivate()
        tryVerify(() => tc.Window.active, 2000, "the test window is in front")
    }

    // A panel holding `pending` and `history`, loaded.
    function makePanel(pending, history, component) {
        const panel = createTemporaryObject(component || panelComponent, tc)
        verify(panel, "the panel")
        Shapes.answerLists(fake, pending, history || [])
        tryVerify(() => panel.list.loaded, 2000, "the lists are in")
        return panel
    }

    // The panel with the dialog open on `id`, past the 500 ms.
    function openDialog(panel, id) {
        panel.showConfirmation(id)
        tryVerify(() => panel.dialogPopup.opened, 2000, "the dialog opens")
        tryVerify(() => panel.dialog.bar.inFront || panel.dialog.isCap, 2000, "its window is in front")
        wait(550)
    }

    readonly property var crane: Shapes.view({
        id: "cf_00000000000000a1", tool: "asset.delete", caller_kind: "mcp", project: "sketchbook",
        arguments: {path: "C:\\art\\crane.png", packages: ["left-pack", "right-pack"], force: true}
    })

    function test_card_shows_who_the_exact_action_and_the_files() { // spec: APP-115
        const panel = makePanel([crane])
        compare(panel.cardRepeater.count, 1)
        const card = panel.cardRepeater.itemAt(0)
        compare(card.whoText, "Claude Code")
        compare(card.actionText, "asset.delete")
        verify(card.title.indexOf("Claude Code") >= 0 && card.title.indexOf("asset.delete") >= 0, card.title)
        verify(card.factsLine.text.indexOf("sketchbook") >= 0, card.factsLine.text)
        compare(card.files, ["C:\\art\\crane.png", "left-pack", "right-pack"])
        const rows = card.argumentRows.map(r => r.key + "=" + r.text)
        verify(rows[0] === "path=C:\\art\\crane.png", rows[0])
        verify(rows[2] === "force=true", rows[2])
        compare(walk.report(panel), [])
        verify(walk.interactiveCount(panel) >= 3, "tabs and the card's button")
    }

    function test_untrusted_arguments_are_shown_normalised() { // spec: APP-115, P4a-R8
        const before = TestFiles.networkRequests()
        const evil = Shapes.view({
            id: "cf_00000000000000a2", tool: "evil\u202E.tool", caller_kind: "weird\u0007caller",
            arguments: {"k\u202Ey": "a\u202Eb\u0007c\u0001d\nsecond\tline", img: '<img src="http://127.0.0.1:9/p.png">'}
        })
        const panel = makePanel([evil])
        const card = panel.cardRepeater.itemAt(0)
        compare(card.actionText, "evil⟨U+202E⟩.tool")
        compare(card.whoText, "weirdcaller")
        compare(card.argumentRows[0].key, "k⟨U+202E⟩y")
        verify(card.argumentRows[0].text.indexOf("a⟨U+202E⟩bcd") === 0, card.argumentRows[0].text)
        verify(card.argumentRows[0].text.indexOf("\n") > 0, "a script still reads as lines")
        verify(card.argumentRows[0].text.indexOf("\u0007") < 0 && card.argumentRows[0].text.indexOf("\u0001") < 0)
        verify(card.argumentRows[0].text.indexOf("\t") < 0)
        wait(200)
        compare(TestFiles.networkRequests(), before, "markup loads nothing")
    }

    function test_a_value_too_long_to_show_says_so() { // spec: APP-115
        const long = "x".repeat(5000) + "THE-END"
        const panel = makePanel([Shapes.view({id: "cf_00000000000000a3", arguments: {script: long}})])
        const row = panel.cardRepeater.itemAt(0).argumentRows[0]
        verify(row.cut, "it says part isn't shown")
        verify(row.text.indexOf("THE-END") < 0)
        verify(row.text.length <= 4001, "at most what the card shows: " + row.text.length)
    }

    function test_initial_focus_is_deny_and_enter_does_not_approve() { // spec: P4a-R10
        const panel = makePanel([crane])
        openDialog(panel, crane.id)
        const dialog = panel.dialog
        tryVerify(() => dialog.denyButton.activeFocus, 2000, "Deny has the focus")
        verify(!dialog.approveButton.activeFocus)
        // Enter answers what has the focus: Deny, never Approve.
        keyClick(Qt.Key_Return)
        const sent = Shapes.answers(fake)
        compare(sent.length, 1)
        compare(sent[0].args.answer, "deny")
    }

    function test_no_key_and_no_automation_approves() { // spec: P4a-R10
        const panel = makePanel([crane])
        openDialog(panel, crane.id)
        const dialog = panel.dialog
        dialog.approveButton.forceActiveFocus(Qt.TabFocusReason)
        for (const key of [Qt.Key_Space, Qt.Key_Return, Qt.Key_Enter])
            keyClick(key)
        compare(Shapes.answers(fake).length, 0, "a key on Approve answers nothing")
        verify(dialog.bar.reason.indexOf("No key answers Approve") === 0, dialog.bar.reason)
        // UI Automation's Invoke is this action (QWindowsUiaInvokeProvider::Invoke).
        dialog.bar.reason = ""
        verify(TestInput.accessiblePress(dialog.approveButton), "it has a press action to refuse")
        compare(Shapes.answers(fake).length, 0, "Invoke on Approve answers nothing")
        verify(dialog.bar.reason.indexOf("didn't come from your mouse or touchpad") >= 0, dialog.bar.reason)
        compare(hand.taken, 0, "and neither asked for the hand's click")
        // Deny comes from any source: Invoke denies.
        verify(TestInput.accessiblePress(dialog.denyButton))
        compare(Shapes.answers(fake).length, 1)
        compare(Shapes.answers(fake)[0].args.answer, "deny")
    }

    function test_a_click_the_native_gate_refuses_answers_nothing_and_says_why() { // spec: P4a-R10
        // The real gate: a test's click has no real pointer message behind it, as a sent message or
        // a program's own has none. Injected and posted input are crate::pointer_gate's tests, and
        // tools/sandbox/confirm-check.ps1's on a real window.
        const panel = makePanel([crane], [], realGatePanel)
        openDialog(panel, crane.id)
        const dialog = panel.dialog
        mouseClick(dialog.approveButton)
        compare(Shapes.answers(fake).length, 0)
        verify(dialog.bar.reason.indexOf("didn't come from your mouse or touchpad") >= 0, dialog.bar.reason)
        verify(dialog.bar.reasonLine.visible, "the reason shows next to the buttons")
        // Deny by the same click goes through: refusing is always safe.
        mouseClick(dialog.denyButton)
        compare(Shapes.answers(fake).length, 1)
        compare(Shapes.answers(fake)[0].args.answer, "deny")
    }

    function test_each_native_refusal_reads_as_words() { // spec: P4a-R10
        const panel = makePanel([crane])
        openDialog(panel, crane.id)
        const dialog = panel.dialog
        for (const [verdict, words] of [["no_rest", "came too soon"], ["no_move", "Move the pointer a little"],
                                        ["no_real_press", "Press and release"],
                                        ["brought_forward", "brought this window to the front"]]) {
            hand.verdict = verdict
            mouseClick(dialog.approveButton)
            verify(dialog.bar.reason.indexOf(words) >= 0, verdict + ": " + dialog.bar.reason)
        }
        compare(Shapes.answers(fake).length, 0)
    }

    function test_approve_waits_500_ms_and_for_the_front() { // spec: P4a-R10
        const panel = makePanel([crane])
        panel.showConfirmation(crane.id)
        tryVerify(() => panel.dialogPopup.opened, 2000)
        const dialog = panel.dialog
        wait(100)
        dialog.bar.inFront = true
        dialog.bar.visibleSinceMs = ConfirmInput.nowMs()
        mouseClick(dialog.approveButton)
        compare(Shapes.answers(fake).length, 0, "too soon")
        verify(dialog.bar.reason.indexOf("Wait a moment") === 0, dialog.bar.reason)
        wait(550)
        dialog.bar.inFront = false
        mouseClick(dialog.approveButton)
        compare(Shapes.answers(fake).length, 0, "not in front")
        verify(dialog.bar.reason.indexOf("to the front") >= 0, dialog.bar.reason)
        // Back in front: the half second starts again.
        dialog.bar.inFront = true
        mouseClick(dialog.approveButton)
        compare(Shapes.answers(fake).length, 0, "again too soon")
        wait(550)
        mouseClick(dialog.approveButton)
        compare(Shapes.answers(fake).length, 1)
        compare(Shapes.answers(fake)[0].args, {confirmation_id: crane.id, answer: "approve"})
    }

    function test_one_answer_sends_one_app_call() { // spec: APP-044
        const panel = makePanel([crane])
        openDialog(panel, crane.id)
        const dialog = panel.dialog
        mouseClick(dialog.approveButton)
        mouseClick(dialog.approveButton)
        mouseClick(dialog.denyButton)
        keyClick(Qt.Key_Return)
        const sent = Shapes.answers(fake)
        compare(sent.length, 1, "one call while the first is out")
        verify(!dialog.bar.live, "the buttons are off")
        fake.respond(sent[0].id, Shapes.answerResult(Shapes.answeredView(crane, "approved")))
        tryCompare(panel.list.pendingModel, "count", 0)
        mouseClick(dialog.approveButton)
        compare(Shapes.answers(fake).length, 1, "and none after it")
        verify(dialog.stateLine.text.indexOf("Answered in the app at ") === 0, dialog.stateLine.text)
        panel.dialogPopup.close()
        tryVerify(() => !panel.dialogPopup.visible)
        compare(OpenPrompts.count, 0)
    }

    function test_the_dialog_answers_one_confirmation_then_another() { // spec: APP-044
        const next = Shapes.view({id: "cf_00000000000000a4", tool: "next.tool"})
        const capOne = Shapes.view({id: "cf_00000000000000a5", covers: ["cap"], cap: [Shapes.capHit()]})
        const capTwo = Shapes.view({id: "cf_00000000000000a6", covers: ["cap"], cap: [Shapes.capHit()]})
        const panel = makePanel([crane, next, capOne, capTwo])
        openDialog(panel, crane.id)
        mouseClick(panel.dialog.denyButton)
        const sent = Shapes.answers(fake)
        fake.respond(sent[0].id, Shapes.answerResult(Shapes.answeredView(crane, "denied")))
        panel.dialogPopup.close()
        tryVerify(() => !panel.dialogPopup.visible)
        openDialog(panel, next.id)
        verify(panel.dialog.bar.live, "the first answer doesn't hold the next one's buttons")
        compare(panel.dialog.card.actionText, "next.tool")
        panel.dialogPopup.close()
        tryVerify(() => !panel.dialogPopup.visible)
        // The same for two caps, whose prompt the dialog keeps.
        openDialog(panel, capOne.id)
        tryVerify(() => panel.dialog.capPrompt.bar.inFront, 2000)
        wait(550)
        mouseClick(panel.dialog.capPrompt.allowOnceButton)
        const third = Shapes.answers(fake)[1]
        compare(third.args.confirmation_id, capOne.id)
        fake.respond(third.id, Shapes.answerResult(Shapes.answeredView(capOne, "approved")))
        panel.dialogPopup.close()
        tryVerify(() => !panel.dialogPopup.visible)
        openDialog(panel, capTwo.id)
        verify(panel.dialog.capPrompt.visible, "the next cap's prompt shows")
        verify(panel.dialog.capPrompt.bar.live)
        panel.dialogPopup.close()
        tryVerify(() => !panel.dialogPopup.visible)
    }

    function test_confirm_expired_reads_as_plain_words() { // spec: P4a-R10
        const panel = makePanel([crane])
        openDialog(panel, crane.id)
        const dialog = panel.dialog
        mouseClick(dialog.denyButton)
        const sent = Shapes.answers(fake)
        fake.respond(sent[0].id, {status: "error", error: {code: "confirm.expired", category: "cancelled",
                                                           message: "Confirmation cf_x expired before it was answered.",
                                                           trace_id: "t", retry: "after_resolve"}})
        compare(dialog.bar.reason, "This confirmation has expired, so that answer didn't count.")
        verify(dialog.bar.reason.indexOf("confirm.") < 0)
    }

    function test_answered_elsewhere_clears_the_card() { // spec: APP-054
        const panel = makePanel([crane])
        openDialog(panel, crane.id)
        fake.emit("confirmations", "event", Shapes.event("answered", Shapes.answeredView(crane, "denied")))
        tryCompare(panel.list.pendingModel, "count", 0)
        compare(panel.cardRepeater.count, 0, "the card is gone")
        verify(panel.emptyText.visible)
        const dialog = panel.dialog
        verify(dialog.stateLine.text.indexOf("Answered in the app at ") === 0, dialog.stateLine.text)
        verify(!dialog.bar.visible, "the open dialog has no buttons now")
        verify(panel.closeDialogButton.visible)
    }

    function test_show_confirmation_says_why_it_cant_open() { // spec: APP-115
        const answered = Shapes.answeredView(Shapes.view({id: "cf_00000000000000b1"}), "approved")
        const panel = makePanel([crane], [answered])
        panel.showConfirmation("cf_00000000000000ff")
        verify(panel.banner.visible)
        verify(panel.banner.text.indexOf("doesn't know that confirmation") >= 0, panel.banner.text)
        verify(!panel.dialogPopup.opened)
        panel.showConfirmation(answered.id)
        verify(panel.banner.text.indexOf("Answered in the app at ") === 0, panel.banner.text)
        verify(!panel.dialogPopup.opened)
        // One asked about before the lists are in waits for them.
        const later = createTemporaryObject(panelComponent, tc)
        later.showConfirmation(crane.id)
        verify(!later.dialogPopup.opened)
        Shapes.answerLists(fake, [crane], [])
        tryVerify(() => later.dialogPopup.opened, 2000, "it opens once they are")
        later.dialogPopup.close()
        tryVerify(() => !later.dialogPopup.visible)
    }

    function test_history_tab_shows_confirmed_via() { // spec: APP-046
        const viaApp = Shapes.answeredView(Shapes.view({id: "cf_00000000000000c1", tool: "one.tool"}), "approved", "app")
        const viaCode = Shapes.answeredView(Shapes.view({id: "cf_00000000000000c2", tool: "two.tool"}), "denied", "claude_code")
        const panel = makePanel([], [viaApp, viaCode])
        panel.historyTab.clicked()
        compare(panel.tab, "history")
        tryCompare(panel.history, "count", 2)
        tryVerify(() => panel.history.itemAtIndex(1), 2000, "the rows are made")
        const first = panel.history.itemAtIndex(0)
        const second = panel.history.itemAtIndex(1)
        verify(first.outcomeText.indexOf("Answered in the app at ") === 0, first.outcomeText)
        verify(second.outcomeText.indexOf("Answered in Claude Code at ") === 0, second.outcomeText)
        verify(second.outcomeText.indexOf("denied") > 0)
        compare(walk.report(panel), [])
        // A live answer goes first.
        const viaNow = Shapes.answeredView(crane, "denied")
        fake.emit("confirmations", "event", Shapes.event("answered", viaNow))
        tryCompare(panel.history, "count", 3)
        compare(panel.list.historyModel.get(0).cid, crane.id)
    }

    function test_expiry_countdown() { // spec: APP-115
        const soon = Shapes.view({id: "cf_00000000000000d1", expires_at: Shapes.iso(Date.now() + 2500)})
        const panel = makePanel([soon])
        openDialog(panel, soon.id)
        const dialog = panel.dialog
        verify(/^Expires in 0:0\d$/.test(dialog.card.countdownText), dialog.card.countdownText)
        tryVerify(() => dialog.card.ranOut, 5000, "it runs out")
        compare(dialog.card.countdownText, "")
        verify(!dialog.bar.live, "no answer once it has run out")
        verify(dialog.stateLine.text.indexOf("run out of time") >= 0, dialog.stateLine.text)
    }

    function test_an_event_can_never_undo_an_ending() { // spec: APP-054
        const panel = makePanel([crane])
        fake.emit("confirmations", "event", Shapes.event("answered", Shapes.answeredView(crane, "approved")))
        fake.emit("confirmations", "event", Shapes.event("pending", crane))
        compare(panel.list.pendingModel.count, 0)
        compare(panel.list.view(crane.id).state, "approved")
    }

    function test_a_resync_reads_again_and_ready_alone_never_does() { // spec: APP-033
        const panel = makePanel([crane])
        const count = () => fake.calls.filter(c => c.name === "app/confirmations.list").length
        const before = count()
        fake.connection = "reconnecting"
        fake.connection = "ready"
        compare(count(), before, "ready reads nothing")
        fake.emit("confirmations", "resync_required", null)
        compare(count(), before + 2)
        verify(!panel.list.loaded, "answers wait for the read")
        // An event during the read is held, and wins over the read's older row.
        fake.emit("confirmations", "event", Shapes.event("answered", Shapes.answeredView(crane, "denied")))
        Shapes.answerLists(fake, [crane], [])
        tryVerify(() => panel.list.loaded)
        compare(panel.list.view(crane.id).state, "denied")
        compare(panel.list.pendingModel.count, 0)
    }

    function test_a_load_that_failed_retries_and_says_so() { // spec: APP-033
        const panel = createTemporaryObject(panelComponent, tc)
        const lists = () => fake.calls.filter(c => c.name === "app/confirmations.list")
        fake.respond(lists()[0].id, {status: "error", error: {code: "core.unreachable", category: "transient",
                                                              message: "The core isn't reachable.", trace_id: "t", retry: "safe"}})
        verify(panel.emptyText.text.indexOf("isn't reachable") >= 0, panel.emptyText.text)
        const before = lists().length
        tryVerify(() => lists().length > before, 3000, "it asks again")
        Shapes.answerLists(fake, [], [])
        tryVerify(() => panel.list.loaded)
    }

    function test_the_dialog_passes_the_accessibility_walk() { // spec: APP-156
        const capped = Shapes.view({id: "cf_00000000000000e1", covers: ["cap"], cap: [Shapes.capHit()]})
        const panel = makePanel([crane, capped])
        openDialog(panel, crane.id)
        compare(walk.report(panel.dialogPopup.contentItem), [])
        verify(walk.interactiveCount(panel.dialogPopup.contentItem) >= 2)
        panel.dialogPopup.close()
        tryVerify(() => !panel.dialogPopup.visible)
        openDialog(panel, capped.id)
        compare(walk.report(panel.dialogPopup.contentItem), [])
        panel.dialogPopup.close()
        tryVerify(() => !panel.dialogPopup.visible)
    }

    function test_app_wide_shortcuts_stay_quiet_under_the_dialog() { // spec: P4a-R10, APP-011
        wait(100)
        const shell = createTemporaryObject(shellComponent, tc)
        verify(shell)
        shell.visible = true
        tryVerify(() => shell.docks.home.content && shell.docks.home.content.width > 0, 5000)
        tryVerify(() => shell.layoutSettled, 5000)
        shell.requestActivate()
        tryVerify(() => shell.active, 2000)
        const panel = shell.docks.confirmations.content
        panel.pointerSource = hand
        Shapes.answerLists(fake, [crane], [])
        tryVerify(() => panel.list.loaded, 2000)
        const spy = pauseSpy.createObject(tc, {target: shell})
        keySequence("Ctrl+Shift+P")
        compare(spy.count, 1, "the shortcut works with no dialog open")
        shell.handleLaunchJson(JSON.stringify({kind: "confirm", id: crane.id, focus: true, activate: true}))
        tryVerify(() => panel.dialogPopup.opened, 2000, "a confirm link opens the dialog")
        compare(OpenPrompts.count, 1)
        tryVerify(() => panel.dialog.denyButton.activeFocus, 2000, "Deny has the focus")
        keySequence("Ctrl+Shift+P")
        compare(spy.count, 1, "Ctrl+Shift+P does nothing under the dialog")
        for (const seq of ["F6", "Shift+F6", shell.keysFor("panel.home")]) {
            keySequence(seq)
            verify(panel.dialog.denyButton.activeFocus, seq + " moved the focus out of the dialog")
            verify(!shell.docks.home.panelHasFocus, seq + " reached the panels")
        }
        panel.dialogPopup.close()
        tryVerify(() => !panel.dialogPopup.visible)
        compare(OpenPrompts.count, 0)
        keySequence("Ctrl+Shift+P")
        compare(spy.count, 2, "and works again once it is closed")
        spy.destroy()
    }

    Component {
        id: pauseSpy
        SignalSpy { signalName: "gpuPauseToggleRequested" }
    }

    // The gate and the filter, with the real native event loop and no window on screen: a mouse
    // message posted to a hidden window reaches the gate, which doesn't call it real.
    Component {
        id: watchedComponent
        ApproveButton { width: 80; height: 30 }
    }

    function test_a_posted_message_reaches_the_gate_and_is_not_real() { // spec: P4a-R10
        const button = createTemporaryObject(watchedComponent, tc)
        verify(button)
        const seen = ConfirmInput.seenMessages()
        const real = ConfirmInput.realMessages()
        verify(TestInput.postClickToHiddenWindow())
        tryVerify(() => ConfirmInput.seenMessages() >= seen + 2, 2000, "the filter read them: "
                  + ConfirmInput.seenMessages())
        compare(ConfirmInput.realMessages(), real, "none of them is called real")
        compare(ConfirmInput.takeClick(button), "not_pointer")
    }
}
