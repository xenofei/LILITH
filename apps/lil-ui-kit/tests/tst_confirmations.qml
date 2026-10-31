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
        property int watches: 0
        function watch(item) { ++watches }
        function unwatch(item) {}
        function notePress(item) {}
        function takeClick(item) {
            ++taken
            return verdict
        }
    }

    QtObject {
        id: fakeShim
        property var activations: []
        function activate(window, takeFocus) {
            activations = activations.concat([{window: window, takeFocus: takeFocus}])
        }
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

    Component {
        id: otherWindow
        Window {
            width: 100
            height: 100
            visible: true
        }
    }

    A11yWalk { id: walk }

    function init() {
        fake.calls = []
        hand.verdict = ""
        hand.taken = 0
        hand.watches = 0
        fakeShim.activations = []
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

    function closeDialog(panel) {
        panel.dialogPopup.close()
        tryVerify(() => !panel.dialogPopup.visible, 2000, "the dialog closes")
    }

    // A label as the reader sees it, without the first strong mark that lays it out left to right.
    function plain(text) {
        return text.replace(/‎/g, "")
    }

    // Scrolls down in steps shorter than the view, as a wheel does.
    function scrollThrough(s) {
        while (s.contentY + s.height < s.contentHeight - 1)
            s.contentY = Math.min(s.contentY + s.height * 0.9, s.contentHeight - s.height)
    }

    readonly property var crane: Shapes.view({
        id: "cf_00000000000000a1", tool: "asset.delete", caller_kind: "mcp", project: "sketchbook",
        arguments: {path: "C:\\art\\crane.png", packages: ["left-pack", "right-pack"], force: true}
    })
    readonly property var heron: Shapes.view({
        id: "cf_00000000000000a7", tool: "dangerous.tool", caller_kind: "mcp", arguments: {path: "C:\\everything"}
    })

    function test_card_shows_who_the_exact_action_and_the_files() { // spec: APP-115
        const panel = makePanel([crane])
        compare(panel.cardRepeater.count, 1)
        const card = panel.cardRepeater.itemAt(0)
        compare(card.whoText, "Claude Code")
        compare(card.actionText, "asset.delete")
        verify(card.title.indexOf("Claude Code") >= 0 && card.title.indexOf("asset.delete") >= 0, card.title)
        verify(card.factsLine.text.indexOf("sketchbook") >= 0, card.factsLine.text)
        // The files are in the exact arguments, under the keys they came with: no guessed list.
        compare(card.argumentRows.map(r => r.key), ["path", "packages", "force"])
        compare(card.argumentRows[0].text, "C:\\art\\crane.png")
        verify(card.argumentRows[1].text.indexOf("left-pack") >= 0 && card.argumentRows[1].text.indexOf("right-pack") >= 0)
        compare(card.argumentRows[2].text, "true")
        tryCompare(card.rowRepeater, "count", 3)
        compare(plain(card.rowRepeater.itemAt(0).keyText.text), "path")
        compare(plain(card.rowRepeater.itemAt(0).valueText.text), "C:\\art\\crane.png")
        verify(!("files" in card), "no list guessed from the names of the arguments")
        compare(walk.report(panel), [])
        verify(walk.interactiveCount(panel) >= 3, "tabs and the card's button")
    }

    function test_untrusted_arguments_are_shown_normalised() { // spec: APP-115, P4a-R8
        const before = TestFiles.networkRequests()
        const evil = Shapes.view({
            id: "cf_00000000000000a2", tool: "evil\u202E.tool", caller_kind: "weird\u0007caller",
            arguments: {"k\u202Ey": "a\u202Eb\u0007c\u0001d\nsecond\tline\u2028third",
                        tag: "ok\u{E0041}\u{E0001}", img: '<img src="http://127.0.0.1:9/p.png">'}
        })
        const panel = makePanel([evil])
        const card = panel.cardRepeater.itemAt(0)
        compare(card.actionText, "evil⟨U+202E⟩.tool")
        compare(card.whoText, "weird⟨U+0007⟩caller")
        compare(card.argumentRows[0].key, "k⟨U+202E⟩y")
        compare(card.argumentRows[0].text, "a⟨U+202E⟩b⟨U+0007⟩c⟨U+0001⟩d\n↳ second⟨U+0009⟩line⟨U+2028⟩third")
        compare(card.argumentRows[1].text, "ok⟨U+E0041⟩⟨U+E0001⟩")
        wait(200)
        compare(TestFiles.networkRequests(), before, "markup loads nothing")
    }

    function test_a_value_cant_pass_for_another_argument() { // spec: APP-115
        const panel = makePanel([Shapes.view({id: "cf_00000000000000a8",
                                              arguments: {a: "x\npath: C:\\safe", path: "C:\\evil"}})])
        const card = panel.cardRepeater.itemAt(0)
        tryCompare(card.rowRepeater, "count", 2)
        compare(plain(card.rowRepeater.itemAt(0).valueText.text), "x\n↳ path: C:\\safe")
        compare(plain(card.rowRepeater.itemAt(1).keyText.text), "path")
    }

    function test_numbers_and_key_order_are_the_cores_not_qmls() { // spec: APP-115
        const panel = makePanel([])
        // The event's text as the core writes it: JSON.parse would round the number and move "1" first.
        const raw = '{"kind":"pending","confirmation":{"id":"cf_00000000000000a9","tool":"t","caller_kind":"mcp",'
                    + '"covers":["confirm"],"state":"pending","created":"2026-10-30T00:00:00Z",'
                    + '"expires_at":"2099-01-01T00:00:00Z","arguments":{"z":"last-key-first","1":"one","n":9007199254740993}}}'
        fake.coreEvent("confirmations", "event", 1, raw)
        tryCompare(panel.cardRepeater, "count", 1)
        const card = panel.cardRepeater.itemAt(0)
        compare(card.argumentRows.map(r => r.key), ["z", "1", "n"])
        compare(card.argumentRows[2].text, "9007199254740993")
    }

    function test_blank_lines_cant_hide_the_action_and_approve_waits_for_the_end() { // spec: APP-115, P4a-R10
        let lines = []
        for (let i = 0; i < 200; ++i)
            lines.push("line " + i)
        const long = Shapes.view({id: "cf_00000000000000aa",
                                  arguments: {note: "\n".repeat(80) + "ok", script: lines.join("\n"), command: "del /s /q C:\\Users"}})
        const panel = makePanel([long])
        openDialog(panel, long.id)
        const dialog = panel.dialog
        compare(dialog.card.argumentRows[0].text, "⟨80 empty lines⟩\n↳ ok")
        verify(dialog.scroller.overflows, "the action needs scrolling")
        verify(!dialog.seenAll)
        verify(!dialog.approvable)
        compare(dialog.moreBelow.text, "More below.")
        verify(dialog.blockedLine.visible)
        verify(dialog.blockedLine.text.indexOf("Scroll through all of the request") === 0, dialog.blockedLine.text)
        verify(/\d+% hasn't been on screen/.test(dialog.blockedLine.text), dialog.blockedLine.text)
        verify(!dialog.approveButton.enabled, "Approve is off")
        verify(dialog.denyButton.enabled, "Deny works")
        mouseClick(dialog.approveButton)
        compare(Shapes.answers(fake).length, 0)
        // Who asks and for what stay in sight outside the view.
        verify(dialog.header.visible)
        verify(dialog.header.y < dialog.scroller.y, "above the view, not in it")
        scrollPartWay(dialog)
        verify(!dialog.approvable, "part of the way isn't all of it")
        compare(dialog.moreBelow.text, "More above and below.")
        scrollThrough(dialog.scroller)
        tryVerify(() => dialog.approvable, 1000, "every line has been drawn")
        compare(dialog.moreBelow.text, "More above.")
        verify(dialog.approveButton.enabled)
        compare(dialog.blockedLine.text, "")
        // It stays on after scrolling back up: all of it has been on screen.
        dialog.scroller.contentY = 0
        verify(dialog.approvable)
        compare(dialog.moreBelow.text, "More below.")
        wait(50)
        mouseClick(dialog.approveButton)
        compare(Shapes.answers(fake).length, 1)
    }

    function scrollPartWay(dialog) {
        dialog.scroller.contentY = dialog.scroller.height * 0.9
    }

    // A jump of the thumb, or a program's: the middle was never drawn.
    function test_a_jump_to_the_end_doesnt_count_as_reading() { // spec: APP-115, P4a-R10
        let lines = []
        for (let i = 0; i < 400; ++i)
            lines.push("dull line " + i)
        const hiding = Shapes.view({id: "cf_00000000000000ad",
                                    arguments: {note: lines.join("\n"), command: "del /s /q C:\\Users", note2: lines.join("\n")}})
        const panel = makePanel([hiding])
        openDialog(panel, hiding.id)
        const dialog = panel.dialog
        const s = dialog.scroller
        tryVerify(() => s.overflows, 2000)
        s.contentY = s.contentHeight - s.height
        verify(s.atEnd)
        verify(!dialog.seenAll, "the end was reached, the middle never drawn")
        verify(!dialog.approvable)
        verify(!dialog.approveButton.enabled)
        verify(dialog.blockedLine.text.indexOf("You jumped past part of the request") === 0, dialog.blockedLine.text)
        verify(/\d+%/.test(dialog.blockedLine.text), "it says how much: " + dialog.blockedLine.text)
        // A jump in the middle counts for nothing either.
        s.contentY = s.contentHeight / 2
        verify(!dialog.seenAll)
        // Back to where the view left off, then down in steps: now it counts.
        s.contentY = 0
        scrollThrough(s)
        tryVerify(() => dialog.approvable, 1000)
        // What is shown changes: it starts again.
        fake.emit("confirmations", "event", Shapes.event("pending", Object.assign({}, hiding, {arguments: {note: lines.join("\n"), other: lines.join("\n")}})))
        tryVerify(() => !dialog.approvable, 1000, "another action is another reading")
        compare(s.contentY, 0)
    }

    function test_a_short_action_needs_no_scrolling() { // spec: APP-115
        const panel = makePanel([crane])
        openDialog(panel, crane.id)
        const dialog = panel.dialog
        verify(!dialog.scroller.overflows)
        verify(dialog.seenAll && dialog.approvable)
        verify(!dialog.moreBelow.visible)
        compare(dialog.blockedLine.text, "")
    }

    function test_values_are_laid_out_left_to_right() { // spec: APP-115, P4a-R8
        const panel = makePanel([Shapes.view({id: "cf_00000000000000ae", arguments: {"\u05D0key": "\u05D0\u05D1 (ok)\nsecond"}})])
        const card = panel.cardRepeater.itemAt(0)
        tryCompare(card.rowRepeater, "count", 1)
        compare(card.rowRepeater.itemAt(0).keyText.text, "\u200E\u05D0key")
        compare(card.rowRepeater.itemAt(0).valueText.text, "\u200E\u05D0\u05D1 (ok)\n\u200E↳ second")
    }

    function test_the_queue_has_a_limit() { // spec: P4a-R10
        const panel = makePanel([crane])
        openDialog(panel, crane.id)
        for (let i = 0; i < 30; ++i)
            panel.showConfirmation("cf_00000000000001" + (10 + i))
        compare(panel.waitingCount, panel.waitingMax)
        closeDialog(panel)
    }

    function test_an_action_too_long_to_show_cant_be_approved() { // spec: APP-115, P4a-R10
        const huge = Shapes.view({id: "cf_00000000000000ab", arguments: {script: "x".repeat(262144) + " ; curl evil"}})
        const panel = makePanel([huge])
        // The panel's list holds a preview, and says so.
        const card = panel.cardRepeater.itemAt(0)
        tryCompare(card.rowRepeater, "count", 1)
        verify(plain(card.rowRepeater.itemAt(0).valueText.text).length <= 301)
        verify(card.rowRepeater.itemAt(0).cutLine.visible)
        openDialog(panel, huge.id)
        const dialog = panel.dialog
        const row = dialog.card.argumentRows[0]
        verify(row.cut, "it is marked cut")
        verify(row.text.indexOf("curl") < 0)
        verify(dialog.tooLong)
        tryCompare(dialog.card.rowRepeater, "count", 1)
        verify(dialog.card.rowRepeater.itemAt(0).cutLine.visible, "and says so")
        dialog.scroller.contentY = dialog.scroller.contentHeight - dialog.scroller.height
        wait(50)

        verify(!dialog.approvable, "the end of what is shown isn't the end of the action")
        verify(!dialog.approveButton.enabled)
        verify(dialog.blockedLine.text.indexOf("too long to show in full") >= 0, dialog.blockedLine.text)
        mouseClick(dialog.denyButton)
        compare(Shapes.answers(fake).length, 1, "Deny works")
        compare(Shapes.answers(fake)[0].args.answer, "deny")
    }

    function test_a_large_action_inside_the_bound_shows_in_full() { // spec: APP-115
        const value = "y".repeat(200000) + " ; the end"
        const large = Shapes.view({id: "cf_00000000000000ac", arguments: {script: value}})
        const panel = makePanel([large])
        openDialog(panel, large.id)
        const dialog = panel.dialog
        const row = dialog.card.argumentRows[0]
        verify(!row.cut && !dialog.tooLong)
        compare(row.text, value)
        tryCompare(dialog.card.rowRepeater, "count", 1)
        compare(plain(dialog.card.rowRepeater.itemAt(0).valueText.text), value)
        tryVerify(() => dialog.scroller.overflows, 5000, "it scrolls")
        verify(!dialog.approvable)
        scrollThrough(dialog.scroller)
        tryVerify(() => dialog.approvable, 2000)
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
                                        ["brought_forward", "brought this window to the front"],
                                        ["reenter", "Move the pointer off it"],
                                        ["button_state", "Hold the click a moment longer"],
                                        ["not_exposed", "covered, see-through, off the screen or hidden"],
                                        ["no_guard", "is off. Deny still works"]]) {
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
        tryVerify(() => dialog.bar.inFront, 2000)
        wait(50)
        mouseClick(dialog.approveButton)
        compare(Shapes.answers(fake).length, 0, "too soon")
        verify(dialog.bar.reason.indexOf("Wait a moment") === 0, dialog.bar.reason)
        wait(550)
        // The real Window.active, as another window takes the front.
        const other = createTemporaryObject(otherWindow, null)
        other.requestActivate()
        tryVerify(() => other.active && !dialog.bar.inFront, 2000, "another window is in front")
        wait(550)
        mouseClick(dialog.approveButton)
        compare(Shapes.answers(fake).length, 0, "not in front")
        verify(dialog.bar.reason.indexOf("to the front") >= 0, dialog.bar.reason)
        // Back in front: the half second starts again.
        tc.Window.window.requestActivate()
        tryVerify(() => dialog.bar.inFront, 2000)
        mouseClick(dialog.approveButton)
        compare(Shapes.answers(fake).length, 0, "again too soon")
        wait(550)
        mouseClick(dialog.approveButton)
        compare(Shapes.answers(fake).length, 1)
        compare(Shapes.answers(fake)[0].args, {confirmation_id: crane.id, answer: "approve"})
    }

    function test_the_clock_starts_again_when_the_buttons_come_back() { // spec: P4a-R10
        const panel = makePanel([crane])
        openDialog(panel, crane.id)
        const dialog = panel.dialog
        fake.connection = "reconnecting"
        tryVerify(() => !dialog.bar.live, 1000)
        wait(550)
        fake.connection = "ready"
        tryVerify(() => dialog.bar.live, 1000)
        mouseClick(dialog.approveButton)
        compare(Shapes.answers(fake).length, 0, "the buttons just came back on")
        verify(dialog.bar.reason.indexOf("Wait a moment") === 0, dialog.bar.reason)
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
        closeDialog(panel)
        compare(OpenPrompts.count, 0)
    }

    // The review's scenario, through a real link: a program starts `confirm/<dangerous id>` while the
    // owner's hand is on its way to Approve on the harmless one.
    function test_a_link_never_swaps_the_open_dialog() { // spec: P4a-R10, P4a-R11
        wait(100)
        const shell = createTemporaryObject(shellComponent, tc)
        verify(shell)
        shell.visible = true
        tryVerify(() => shell.docks.home.content && shell.docks.home.content.width > 0, 5000)
        shell.requestActivate()
        tryVerify(() => shell.active, 2000)
        const panel = shell.docks.confirmations.content
        panel.pointerSource = hand
        Shapes.answerLists(fake, [crane, heron], [])
        tryVerify(() => panel.list.loaded, 2000)
        const link = id => shell.handleLaunchJson(JSON.stringify({kind: "confirm", id: id, focus: true, activate: true}))
        link(crane.id)
        tryVerify(() => panel.dialogPopup.opened, 2000)
        const dialog = panel.dialog
        tryVerify(() => dialog.bar.inFront, 2000)
        wait(550)
        const watches = hand.watches
        const since = dialog.bar.visibleSinceMs
        const activations = fakeShim.activations.length

        link(heron.id)
        compare(dialog.confirmationId, crane.id, "the dialog stays on what was read")
        compare(dialog.card.actionText, "asset.delete")
        verify(panel.waitingNotice.visible, "it says another is waiting")
        verify(panel.waitingNotice.text.indexOf("Another confirmation is waiting") === 0, panel.waitingNotice.text)
        compare(hand.watches, watches, "nothing changed, so nothing re-armed")
        compare(dialog.bar.visibleSinceMs, since)
        compare(fakeShim.activations.length, activations + 1)
        verify(!fakeShim.activations[activations].takeFocus, "the window only flashes")
        verify(dialog.denyButton.activeFocus, "and the focus stays")
        link(heron.id)
        compare(panel.waitingCount, 1, "once")

        // The click is bound to what is on screen.
        mouseClick(dialog.approveButton)
        const sent = Shapes.answers(fake)
        compare(sent.length, 1)
        compare(sent[0].args.confirmation_id, crane.id)
        fake.respond(sent[0].id, Shapes.answerResult(Shapes.answeredView(crane, "approved")))
        tryCompare(panel.list.pendingModel, "count", 1)
        // Answered: it stays on its outcome until it is closed.
        compare(dialog.confirmationId, crane.id)
        verify(dialog.stateLine.visible)
        verify(panel.waitingNotice.visible)
        const beforeClose = hand.watches
        panel.closeDialogButton.clicked()
        tryVerify(() => panel.dialogPopup.opened && dialog.confirmationId === heron.id, 2000,
                  "the waiting one opens as a new dialog")
        compare(dialog.card.actionText, "dangerous.tool")
        verify(hand.watches > beforeClose, "its button is new to the gate")
        verify(!panel.waitingNotice.visible)
        tryVerify(() => dialog.denyButton.activeFocus, 2000, "Deny has the focus")
        mouseClick(dialog.approveButton)
        compare(Shapes.answers(fake).length, 1, "the half second starts again")
        verify(dialog.bar.reason.indexOf("Wait a moment") === 0, dialog.bar.reason)
        closeDialog(panel)
    }

    function test_other_links_wait_for_the_dialog() { // spec: P4a-R10
        wait(100)
        const shell = createTemporaryObject(shellComponent, tc)
        shell.visible = true
        tryVerify(() => shell.docks.home.content && shell.docks.home.content.width > 0, 5000)
        shell.requestActivate()
        tryVerify(() => shell.active, 2000)
        const panel = shell.docks.confirmations.content
        panel.pointerSource = hand
        Shapes.answerLists(fake, [crane], [])
        tryVerify(() => panel.list.loaded, 2000)
        shell.handleLaunchJson(JSON.stringify({kind: "confirm", id: crane.id, focus: true, activate: true}))
        tryVerify(() => panel.dialogPopup.opened, 2000)
        tryVerify(() => panel.dialog.denyButton.activeFocus, 2000)
        shell.handleLaunchJson(JSON.stringify({kind: "task", job: "j_0123456789abcdef", focus: true, activate: true}))
        verify(panel.dialog.denyButton.activeFocus, "a task link doesn't pull the focus out of the dialog")
        compare(shell.shownJob, "", "it waits")
        closeDialog(panel)
        tryCompare(shell, "shownJob", "j_0123456789abcdef", 2000, "and runs once the dialog is closed")
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
        tryVerify(() => panel.emptyText.visible, 1000)
        const dialog = panel.dialog
        verify(dialog.stateLine.text.indexOf("Answered in the app at ") === 0, dialog.stateLine.text)
        verify(!dialog.bar.visible, "the open dialog has no buttons now")
        verify(panel.closeDialogButton.visible)
    }

    function test_show_confirmation_says_why_it_cant_open() { // spec: APP-115
        const answered = Shapes.answeredView(Shapes.view({id: "cf_00000000000000b1"}), "approved")
        const panel = makePanel([crane], [answered])
        panel.showConfirmation("cf_00000000000000ff")
        tryVerify(() => panel.banner.visible, 1000)
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
        closeDialog(later)
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

    function test_the_dialog_answers_one_confirmation_then_another() { // spec: APP-044
        const next = Shapes.view({id: "cf_00000000000000a4", tool: "next.tool"})
        const capOne = Shapes.view({id: "cf_00000000000000a5", covers: ["cap"], cap: [Shapes.capHit()]})
        const capTwo = Shapes.view({id: "cf_00000000000000a6", covers: ["cap"], cap: [Shapes.capHit()]})
        const panel = makePanel([crane, next, capOne, capTwo])
        openDialog(panel, crane.id)
        mouseClick(panel.dialog.denyButton)
        const sent = Shapes.answers(fake)
        fake.respond(sent[0].id, Shapes.answerResult(Shapes.answeredView(crane, "denied")))
        closeDialog(panel)
        openDialog(panel, next.id)
        verify(panel.dialog.bar.live, "the first answer doesn't hold the next one's buttons")
        compare(panel.dialog.card.actionText, "next.tool")
        closeDialog(panel)
        // The same for two caps, whose prompt the dialog keeps.
        openDialog(panel, capOne.id)
        tryVerify(() => panel.dialog.capPrompt.bar.inFront, 2000)
        wait(550)
        mouseClick(panel.dialog.capPrompt.allowOnceButton)
        const third = Shapes.answers(fake)[1]
        compare(third.args.confirmation_id, capOne.id)
        fake.respond(third.id, Shapes.answerResult(Shapes.answeredView(capOne, "approved")))
        closeDialog(panel)
        openDialog(panel, capTwo.id)
        verify(panel.dialog.capPrompt.visible, "the next cap's prompt shows")
        verify(panel.dialog.capPrompt.bar.live)
        closeDialog(panel)
    }

    function test_the_dialog_passes_the_accessibility_walk() { // spec: APP-156
        const capped = Shapes.view({id: "cf_00000000000000e1", covers: ["cap"], cap: [Shapes.capHit()]})
        const panel = makePanel([crane, capped])
        openDialog(panel, crane.id)
        compare(walk.report(panel.dialogPopup.contentItem), [])
        verify(walk.interactiveCount(panel.dialogPopup.contentItem) >= 2)
        closeDialog(panel)
        openDialog(panel, capped.id)
        compare(walk.report(panel.dialogPopup.contentItem), [])
        closeDialog(panel)
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
        closeDialog(panel)
        compare(OpenPrompts.count, 0)
        keySequence("Ctrl+Shift+P")
        compare(spy.count, 2, "and works again once it is closed")
        spy.destroy()
    }

    Component {
        id: pauseSpy
        SignalSpy { signalName: "gpuPauseToggleRequested" }
    }

    // The filter and the hook, with the real native event loop and no window on screen. On this
    // thread no hardware input has ever been read: this shows the plumbing, not what Windows would
    // report beside a real hand, which tools/sandbox/confirm-check.ps1 measures.
    Component {
        id: watchedComponent
        ApproveButton { width: 80; height: 30 }
    }

    function test_the_filter_reads_posted_messages_from_the_event_loop() { // spec: P4a-R10
        const button = createTemporaryObject(watchedComponent, tc)
        verify(button)
        const seen = ConfirmInput.seenMessages()
        const real = ConfirmInput.realMessages()
        verify(TestInput.postClickToHiddenWindow())
        tryVerify(() => ConfirmInput.seenMessages() >= seen + 2, 2000, "the filter read them: "
                  + ConfirmInput.seenMessages())
        compare(ConfirmInput.realMessages(), real)
        compare(ConfirmInput.takeClick(button), "not_pointer")
    }

    function test_the_hook_sees_a_pair_sent_from_another_thread() { // spec: P4a-R10
        const button = createTemporaryObject(watchedComponent, tc)
        verify(button)
        const sent = ConfirmInput.sentMessages()
        const seen = ConfirmInput.seenMessages()
        verify(TestInput.sendClickFromAnotherThread())
        tryVerify(() => ConfirmInput.sentMessages() >= sent + 2, 2000, "the hook saw them: "
                  + ConfirmInput.sentMessages())
        compare(ConfirmInput.seenMessages(), seen, "the filter never did")
        compare(ConfirmInput.takeClick(button), "not_pointer")
    }
}
