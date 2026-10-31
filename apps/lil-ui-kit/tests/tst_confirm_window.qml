// The confirm window (APP-048, APP-049, APP-054, P4a-R10, P4a-R11), over FakeCore with the real core's
// shapes: what a launch adds, what it says once a confirmation has ended, and what a lost connection
// does to it.
import QtQuick
import QtTest
import Lilith.Core
import Lilith.UiKit
import "ConfirmShapes.js" as Shapes

TestCase {
    id: tc
    name: "ConfirmWindow"
    when: windowShown
    width: 200
    height: 100

    FakeCore { id: fake }

    QtObject {
        id: hand
        function watch(item) {}
        function unwatch(item) {}
        function notePress(item) {}
        function takeClick(item) { return "" }
    }

    QtObject {
        id: fakeShim
        property var activations: []
        function activate(window, takeFocus) {
            activations = activations.concat([{window: window, takeFocus: takeFocus}])
        }
    }

    Component {
        id: windowComponent
        ConfirmWindow {
            core: fake
            shim: fakeShim
            pointerSource: hand
        }
    }

    A11yWalk { id: walk }

    readonly property var first: Shapes.view({id: "cf_0000000000000101", tool: "first.tool", arguments: {path: "C:\\one"}})
    readonly property var second: Shapes.view({id: "cf_0000000000000102", tool: "second.tool"})

    function init() {
        fake.calls = []
        fake.connection = "ready"
        fakeShim.activations = []
    }

    function makeWindow(pending, history) {
        const w = createTemporaryObject(windowComponent, tc)
        verify(w)
        w.visible = true
        Shapes.answerLists(fake, pending, history || [])
        tryVerify(() => w.list.loaded, 2000)
        return w
    }

    function launch(w, fields) {
        w.handleLaunchJson(JSON.stringify(Object.assign({intent: "open", focus: true, activate: true}, fields)))
    }

    // Scrolls down in steps shorter than the view, as a wheel does.
    function scrollThrough(s) {
        while (s.contentY + s.height < s.contentHeight - 1)
            s.contentY = Math.min(s.contentY + s.height * 0.9, s.contentHeight - s.height)
    }

    function inFront(w) {
        w.requestActivate()
        tryVerify(() => w.active, 2000)
    }

    function test_title_and_walk() { // spec: APP-048, APP-156
        const w = makeWindow([first])
        compare(w.title, "LILITH — Confirm")
        launch(w, {kind: "confirm", id: first.id})
        tryVerify(() => w.dialogOf(first.id), 2000)
        compare(walk.report(w.contentItem), [])
        verify(walk.interactiveCount(w.contentItem) >= 2)
    }

    function test_initial_focus_is_deny_and_enter_never_approves() { // spec: P4a-R10
        const w = makeWindow([first])
        inFront(w)
        launch(w, {kind: "confirm", id: first.id})
        const dialog = w.dialogOf(first.id)
        tryVerify(() => dialog.denyButton.activeFocus, 2000, "Deny has the focus")
        verify(!dialog.approveButton.activeFocus)
        wait(550)
        keyClick(Qt.Key_Return)
        const sent = Shapes.answers(fake)
        compare(sent.length, 1)
        compare(sent[0].args, {confirmation_id: first.id, answer: "deny"})
    }

    function test_focus_comes_when_the_first_card_arrives_and_a_quiet_launch_takes_none() { // spec: P4a-R10
        // Asked before the lists are in: Deny takes the focus when they come.
        const w = createTemporaryObject(windowComponent, tc)
        w.visible = true
        inFront(w)
        launch(w, {kind: "confirm", id: first.id})
        Shapes.answerLists(fake, [first], [])
        tryVerify(() => w.list.loaded, 2000)
        tryVerify(() => w.dialogOf(first.id).denyButton.activeFocus, 2000, "Deny has the focus")
        // A second link adds below and leaves the focus where it is.
        const quiet = createTemporaryObject(windowComponent, tc)
        quiet.visible = true
        Shapes.answerLists(fake, [first], [])
        tryVerify(() => quiet.list.loaded, 2000)
        inFront(quiet)
        launch(quiet, {kind: "confirm", id: first.id, focus: false, activate: false})
        const dialog = quiet.dialogOf(first.id)
        wait(50)
        verify(!dialog.denyButton.activeFocus, "a launch with no focus takes none")
    }

    function test_a_second_link_doesnt_move_the_focus() { // spec: P4a-R10, P4a-R11
        const w = makeWindow([first, second])
        inFront(w)
        launch(w, {kind: "confirm", id: first.id})
        const dialog = w.dialogOf(first.id)
        tryVerify(() => dialog.denyButton.activeFocus, 2000)
        launch(w, {kind: "confirm", id: second.id})
        wait(50)
        verify(dialog.denyButton.activeFocus, "the focus stays on what is being read")
    }

    function test_answered_elsewhere_shows_state() { // spec: APP-054
        const w = makeWindow([first])
        launch(w, {kind: "confirm", id: first.id})
        const dialog = w.dialogOf(first.id)
        verify(dialog.pending)
        verify(dialog.denyButton.visible && dialog.approveButton.visible)
        const answered = Shapes.answeredView(first, "approved", "app")
        fake.emit("confirmations", "event", Shapes.event("answered", answered))
        tryVerify(() => !dialog.pending, 2000)
        const at = Qt.formatTime(new Date(answered.answered), "HH:mm")
        verify(dialog.stateLine.text.indexOf("Answered in the app at " + at) === 0, dialog.stateLine.text)
        verify(!dialog.denyButton.visible && !dialog.approveButton.visible, "no buttons")
    }

    function test_expired_and_unknown_ids_say_so() { // spec: APP-048
        const expired = Object.assign({}, second, {state: "expired"})
        const w = makeWindow([first], [expired])
        launch(w, {kind: "confirm", id: expired.id})
        launch(w, {kind: "confirm", id: "cf_00000000000001ff"})
        const gone = w.dialogOf(expired.id)
        const unknown = w.dialogOf("cf_00000000000001ff")
        verify(gone.stateLine.text.indexOf("expired at ") > 0, gone.stateLine.text)
        verify(gone.stateLine.text.indexOf("confirm.") < 0)
        verify(!gone.denyButton.visible)
        verify(unknown.stateLine.text.indexOf("doesn't know this confirmation") >= 0, unknown.stateLine.text)
        verify(!unknown.denyButton.visible && !unknown.approveButton.visible)
        // One that ends while it is shown, and one the core cancels.
        launch(w, {kind: "confirm", id: first.id})
        const shown = w.dialogOf(first.id)
        fake.emit("confirmations", "event", Shapes.event("cancelled", Object.assign({}, first, {state: "cancelled"})))
        tryVerify(() => !shown.pending, 2000)
        verify(shown.stateLine.text.indexOf("cancelled") > 0, shown.stateLine.text)
    }

    function test_the_keyboard_comes_back_after_an_outage() { // spec: P4a-R10
        const w = makeWindow([first])
        launch(w, {kind: "confirm", id: first.id})
        const dialog = w.dialogOf(first.id)
        tryVerify(() => dialog.denyButton.activeFocus, 2000)
        fake.connection = "reconnecting"
        tryVerify(() => !dialog.denyButton.activeFocus, 1000)
        fake.connection = "ready"
        tryVerify(() => dialog.denyButton.activeFocus, 1000, "Enter works with no Tab first")
        verify(!dialog.approveButton.activeFocus)

        // The keyboard went elsewhere meanwhile, and that has gone: nothing has it now.
        w.requestActivate()
        tryVerify(() => w.active, 2000, "the window has the keyboard")
        fake.connection = "reconnecting"
        tryVerify(() => !dialog.denyButton.activeFocus, 1000)
        dialog.denyButton.focus = false
        fake.connection = "ready"
        tryVerify(() => dialog.denyButton.activeFocus, 1000, "Deny takes it")
        verify(!dialog.approveButton.activeFocus)
    }

    // This window has no shell banner and no Close, so its words don't send anyone to them.
    function test_a_core_thats_gone_says_only_what_is_true_here() { // spec: APP-034
        const w = makeWindow([first])
        launch(w, {kind: "confirm", id: first.id})
        const line = w.dialogOf(first.id).bar.reasonLine
        fake.connection = "unavailable"
        tryVerify(() => line.text !== "", 1000)
        verify(line.text.indexOf("banner") < 0 && line.text.indexOf("Close") < 0, line.text)
        verify(line.text.indexOf("come back") < 0, line.text)
        verify(line.text.indexOf("still waiting") >= 0, line.text)
    }

    function test_reconnect_keeps_the_dialog_and_its_state() { // spec: APP-033, APP-048
        const w = makeWindow([first])
        launch(w, {kind: "confirm", id: first.id})
        const dialog = w.dialogOf(first.id)
        verify(dialog.bar.live)
        verify(dialog.denyButton.enabled && dialog.approveButton.enabled)
        verify(!w.banner.visible)

        fake.connection = "reconnecting"
        tryVerify(() => !dialog.bar.live, 1000)
        verify(!dialog.denyButton.enabled && !dialog.approveButton.enabled, "the buttons are off")
        verify(w.banner.visible)
        verify(w.banner.text.indexOf("Reconnecting") === 0, w.banner.text)
        verify(w.dialogOf(first.id) === dialog, "the dialog stays")

        const before = fake.calls.length
        fake.connection = "ready"
        verify(!w.banner.visible)
        compare(fake.calls.length, before, "ready alone reads nothing (ARCH-100)")
        // The session's mark after a reconnect: the read shows its live state, and answers wait.
        fake.emit("confirmations", "resync_required", null)
        verify(!dialog.bar.live, "no answer until the read is in")
        const answered = Shapes.answeredView(first, "denied", "app")
        Shapes.answerLists(fake, [], [answered])
        tryVerify(() => w.list.loaded, 2000)
        verify(w.dialogOf(first.id) === dialog, "the same dialog")
        verify(dialog.stateLine.text.indexOf("Answered in the app at ") === 0, dialog.stateLine.text)
        verify(!dialog.denyButton.visible)
    }

    function test_without_an_id_lists_every_pending_confirmation() { // spec: APP-049
        const w = makeWindow([first, second])
        launch(w, {kind: "confirm_list"})
        tryCompare(w.dialogs, "count", 2)
        verify(w.dialogOf(first.id) && w.dialogOf(second.id))
        // One that arrives later joins them.
        const third = Shapes.view({id: "cf_0000000000000103"})
        fake.emit("confirmations", "event", Shapes.event("pending", third))
        tryCompare(w.dialogs, "count", 3)
    }

    function test_a_second_url_adds_to_the_open_window() { // spec: P4a-R11
        const w = makeWindow([first, second])
        launch(w, {kind: "confirm", id: first.id, activate: false})
        compare(w.dialogs.count, 1)
        launch(w, {kind: "confirm", id: second.id})
        compare(w.dialogs.count, 2)
        compare(fakeShim.activations.length, 1, "a forwarded launch brings it forward")
        verify(fakeShim.activations[0].window === w && fakeShim.activations[0].takeFocus)
        launch(w, {kind: "confirm", id: first.id})
        compare(w.dialogs.count, 2, "the same id isn't shown twice")
    }

    function test_a_deny_url_reaching_the_open_window_denies_without_changing_focus() { // spec: P4a-R9, P4a-R11
        const w = makeWindow([first])
        launch(w, {kind: "confirm", id: first.id, activate: false, focus: false})
        const dialog = w.dialogOf(first.id)
        // Once the core has taken the deny, the bridge tells the window (args::deny_notice_json); the
        // event and the notice can come in either order.
        launch(w, {kind: "deny", id: first.id, activate: false, focus: false})
        compare(fakeShim.activations.length, 0, "the window doesn't take the focus")
        verify(!dialog.denyButton.activeFocus)
        verify(dialog.pending, "the core has the say")
        compare(Shapes.answers(fake).length, 0, "the window sends nothing itself")
        const denied = Shapes.answeredView(first, "denied", "app")
        fake.emit("confirmations", "event", Shapes.event("answered", denied))
        tryVerify(() => !dialog.pending, 2000)
        compare(dialog.stateLine.text, "Denied from the notification")
        verify(!dialog.denyButton.visible)
    }

    function test_a_deny_with_no_notice_reads_as_the_apps() { // spec: P4a-R9
        // A link whose token the core refused tells the window nothing, so nothing is mislabelled.
        const w = makeWindow([first])
        launch(w, {kind: "confirm", id: first.id, activate: false})
        const dialog = w.dialogOf(first.id)
        fake.emit("confirmations", "event", Shapes.event("answered", Shapes.answeredView(first, "denied", "app")))
        tryVerify(() => !dialog.pending, 2000)
        verify(dialog.stateLine.text.indexOf("Answered in the app") === 0, dialog.stateLine.text)
    }

    function test_a_lost_pipe_says_so() { // spec: P4a-R11
        const w = makeWindow([first])
        launch(w, {kind: "confirm", id: first.id, activate: false, pipe_notice: "Another program holds the pipe."})
        verify(w.instanceBanner.visible)
        compare(w.instanceBanner.text, "Another program holds the pipe.")
    }

    function test_an_answer_the_gate_passes_sends_approve_for_the_id_on_screen() { // spec: P4a-R10
        const w = makeWindow([first])
        launch(w, {kind: "confirm", id: first.id})
        const dialog = w.dialogOf(first.id)
        inFront(w)
        wait(550)
        mouseClick(dialog.approveButton)
        compare(Shapes.answers(fake).length, 1)
        compare(Shapes.answers(fake)[0].args, {confirmation_id: first.id, answer: "approve"})
        fake.respond(Shapes.answers(fake)[0].id, Shapes.answerResult(Shapes.answeredView(first, "approved")))
        tryVerify(() => !dialog.pending, 2000)
        verify(!dialog.approveButton.visible)
    }

    function test_the_clock_starts_again_when_the_window_moves_or_resizes() { // spec: P4a-R10
        const w = makeWindow([first])
        launch(w, {kind: "confirm", id: first.id})
        const dialog = w.dialogOf(first.id)
        inFront(w)
        wait(550)
        for (const change of [() => { w.x += 40 }, () => { w.y += 40 }, () => { w.width += 40 }, () => { w.height += 40 }]) {
            const since = dialog.bar.visibleSinceMs
            change()
            tryVerify(() => dialog.bar.visibleSinceMs > since, 1000, "the clock starts again")
            mouseClick(dialog.approveButton)
            compare(Shapes.answers(fake).length, 0, "too soon")
            verify(dialog.bar.reason.indexOf("Wait a moment") === 0, dialog.bar.reason)
            wait(550)
        }
        mouseClick(dialog.approveButton)
        compare(Shapes.answers(fake).length, 1)
    }

    function test_approve_waits_for_the_whole_action_here_too() { // spec: APP-115, P4a-R10
        let lines = []
        for (let i = 0; i < 200; ++i)
            lines.push("line " + i)
        const long = Shapes.view({id: "cf_0000000000000104", arguments: {script: lines.join("\n")}})
        const w = makeWindow([long])
        launch(w, {kind: "confirm", id: long.id})
        const dialog = w.dialogOf(long.id)
        tryVerify(() => dialog.scroller.overflows, 2000)
        verify(!dialog.approveButton.enabled)
        verify(dialog.moreBelow.visible && dialog.blockedLine.visible)
        verify(dialog.denyButton.enabled)
        scrollThrough(dialog.scroller)
        tryVerify(() => dialog.approveButton.enabled, 1000)
    }
}
