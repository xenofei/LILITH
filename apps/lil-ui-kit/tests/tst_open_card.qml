// The shell's open requests (APP-014, APP-020, APP-021, APP-065, P4a-R21): a launch the running
// instance was handed (ShellWindow.handleLaunch, the JSON of lil_qt::args::launch_json) and an
// `open_requests:image` event each show their target in the Opened card, or say why a link was
// refused; a `task` link opens Jobs on that job; and the window is brought forward, or only
// flashes, through the shim's `activate(window, takeFocus)`, which a fake shim records here.
import QtQuick
import QtTest
import Lilith.UiKit

TestCase {
    id: tc
    name: "OpenCard"
    when: windowShown
    width: 200
    height: 100

    FakeCore {
        id: fake
        project: "sketchbook"
    }

    // LilShim's activate as a record; the rest of the shim the shell calls is passed through.
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

    A11yWalk { id: walk }

    readonly property string sealed: "lil://0123456789abcdef/sketchbook/chr_paper_crane@1.0.0#sha256="
                                     + "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"

    // The shell of the test before deletes its docks later (KDDockWidgets' names are
    // process-wide): let that run first.
    function init() {
        wait(100)
        fakeShim.activations = []
    }

    function makeShell() {
        const shell = createTemporaryObject(shellComponent, tc)
        verify(shell, "ShellWindow created")
        shell.visible = true
        tryVerify(() => shell.docks.home.content && shell.docks.home.content.width > 0, 5000,
                  "the docks are laid out")
        return shell
    }

    function launch(shell, fields) {
        shell.handleLaunchJson(JSON.stringify(Object.assign({intent: "open", focus: true, activate: true},
                                                            fields)))
    }

    function test_shows_the_target_and_its_actions() {
        const shell = makeShell()
        const card = shell.openedCard
        verify(!card.visible, "no card until something is opened")
        launch(shell, {kind: "lilref", target: sealed, title: "Paper crane"})
        tryVerify(() => card.visible, 2000, "the card shows")
        compare(card.heading, "Paper crane")
        compare(card.targetText, sealed)
        compare(card.intentText, "Open")
        compare(card.message, "")
        verify(card.copyButton.visible && card.dismissButton.visible, "Copy and Dismiss")
        compare(card.copyButton.text, "Copy")
        compare(card.dismissButton.text, "Dismiss")
        compare(walk.problems(card), [])
        compare(fakeShim.activations.length, 1)
        verify(fakeShim.activations[0].window === shell && fakeShim.activations[0].takeFocus,
               "a launch's window comes forward")
        // A path, with another intent; the newest replaces the card's target.
        launch(shell, {kind: "target", target: "C:\\art\\crane.png", intent: "edit"})
        compare(card.heading, "Opened")
        compare(card.targetText, "C:\\art\\crane.png")
        compare(card.intentText, "Edit")
        card.dismissButton.clicked()
        verify(!card.visible, "Dismiss hides the card")
    }

    function test_untrusted_text_is_plain_and_marked() { // spec: AI-222, P4a-R8
        const shell = makeShell()
        const card = shell.openedCard
        const title = '<b>x</b>​<img src="http://127.0.0.1:9/probe.png">'
        const before = TestFiles.networkRequests()
        launch(shell, {kind: "lilref", target: sealed + "‮", title: title})
        compare(card.heading, '<b>x</b>⟨U+200B⟩<img src="http://127.0.0.1:9/probe.png">',
                "the title is shown as written, its zero-width space marked")
        compare(card.targetText, sealed + "‮", "the target is kept as received")
        verify(card.shownTarget.endsWith("⟨U+202E⟩"), "and shown with its bidi mark marked: "
               + card.shownTarget)
        launch(shell, {kind: "refused", message: "This link isn't one LILITH opens: a⁦b"})
        compare(card.message, "This link isn't one LILITH opens: a⟨U+2066⟩b")
        wait(300)
        compare(TestFiles.networkRequests(), before, "nothing was loaded")
    }

    function test_refused_link_shows_message() {
        const shell = makeShell()
        const card = shell.openedCard
        const message = "This link isn't one LILITH opens: lil-dev:open/@@@"
        launch(shell, {kind: "refused", message: message})
        tryVerify(() => card.visible, 2000, "the card shows")
        compare(card.message, message)
        compare(card.targetText, "")
        verify(!card.copyButton.visible, "nothing to copy")
        verify(card.dismissButton.visible)
        compare(fake.calls, [], "a refused link asks the core nothing")
        compare(shell.shownJob, "", "and opens no job")
        compare(shell.shownConfirmation, "", "nor a confirmation")
        compare(fakeShim.activations.length, 1, "only the window comes forward")
        compare(walk.problems(card), [])
    }

    function test_no_activate_flashes_instead_of_focusing() {
        const shell = makeShell()
        launch(shell, {kind: "target", target: "C:\\art\\crane.png", focus: false})
        compare(fakeShim.activations.length, 1)
        verify(fakeShim.activations[0].window === shell)
        compare(fakeShim.activations[0].takeFocus, false)
        verify(shell.openedCard.visible)
        // The first launch's window was shown by main.cpp: the card, and no second activation.
        launch(shell, {kind: "target", target: "C:\\b.png", activate: false})
        compare(fakeShim.activations.length, 1)
        compare(shell.openedCard.targetText, "C:\\b.png")
    }

    function test_task_url_opens_jobs_on_that_job() {
        const shell = makeShell()
        shell.docks.jobs.forceClose()
        tryVerify(() => !shell.docks.jobs.isOpen, 2000)
        launch(shell, {kind: "task", job: "j_0123456789abcdef"})
        compare(shell.shownJob, "j_0123456789abcdef")
        tryVerify(() => shell.docks.jobs.isOpen && shell.docks.jobs.content.visible, 2000,
                  "Jobs opens")
        verify(!shell.openedCard.visible, "a task isn't an Opened target")
        compare(fakeShim.activations.length, 1)
        verify(fakeShim.activations[0].takeFocus)
    }

    function test_agent_open_request_never_takes_focus() {
        const shell = makeShell()
        tryVerify(() => fake.subscriberCount("open_requests:image") === 1, 2000,
                  "the shell follows its open requests")
        fake.emit("open_requests:image", "event", {target: sealed, intent: "open", focus: false})
        tryVerify(() => shell.openedCard.visible, 2000, "the card shows")
        compare(shell.openedCard.targetText, sealed)
        compare(fakeShim.activations.length, 1)
        compare(fakeShim.activations[0].takeFocus, false, "APP-065: the window only flashes")
        // A person's request (from an app or lil) may bring it forward.
        fake.emit("open_requests:image", "event", {target: "C:\\x.png", intent: "edit", focus: true})
        compare(fakeShim.activations.length, 2)
        compare(fakeShim.activations[1].takeFocus, true)
        compare(shell.openedCard.intentText, "Edit")
        // A resync carries no request: nothing happens.
        fake.emit("open_requests:image", "resync_required", {})
        compare(fakeShim.activations.length, 2)
    }

    function test_a_held_pipe_shows_a_banner() { // spec: APP-013
        const shell = makeShell()
        verify(!shell.instanceBanner.visible)
        launch(shell, {kind: "activate", activate: false})
        verify(!shell.instanceBanner.visible, "no banner while the pipe is the app's own")
        launch(shell, {kind: "activate", activate: false, pipe_notice: null})
        verify(!shell.instanceBanner.visible)
        launch(shell, {kind: "activate", activate: false,
                       pipe_notice: "Another program is holding LILITH's launch channel, so a "
                                    + "second launch may open a second window."})
        verify(shell.instanceBanner.visible)
        compare(shell.instanceBanner.text, "Another program is holding LILITH's launch channel, so a "
                + "second launch may open a second window.")
        tryVerify(() => shell.dockingArea.y >= shell.instanceBanner.height, 2000,
                  "the docks sit below it")
    }

    function test_confirm_link_opens_confirmations() {
        const shell = makeShell()
        shell.docks.confirmations.forceClose()
        launch(shell, {kind: "confirm", id: "cf_0123456789abcdef"})
        tryVerify(() => shell.docks.confirmations.isOpen, 2000, "Confirmations opens")
        compare(shell.shownConfirmation, "cf_0123456789abcdef")
        verify(!shell.openedCard.visible)
    }
}
