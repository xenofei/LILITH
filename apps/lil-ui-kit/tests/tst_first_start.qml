// The first-start card (APP-022; owner decision 2026-10-22) in the real shell, on FakeCore's scripted
// answers, shaped as the core's (CoreShapes.js), a fake of the setup bridge and a temporary state
// folder: nothing here writes anything real.
import QtQuick
import QtTest
import Lilith.Core
import Lilith.Shim
import Lilith.UiKit
import "CoreShapes.js" as Shapes

TestCase {
    id: tc
    name: "FirstStart"
    when: windowShown
    width: 200
    height: 100

    Component {
        id: fakeComponent
        FakeCore {
            workspace: ({id: "ws1", path: "D:/art/harbor"})
        }
    }

    Component {
        id: fakeSetupComponent
        QtObject {
            property bool busy: false
            property bool appMissing: false
            property string appPath
            property int runs: 0
            property bool refuses: false
            signal finished(string reportJson)
            function run() {
                if (busy || refuses)
                    return false
                runs++
                busy = true
                return true
            }
            function finish(report) {
                busy = false
                finished(JSON.stringify(report))
            }
        }
    }

    Component {
        id: shellComponent
        ShellWindow {}
    }

    A11yWalk { id: walk }

    property var answered: ({})
    // The shells go before their FakeCore, so their subscriptions end on a live core.
    property var windows: []

    readonly property var missing: ({toasts: "not_yet_shown", shortcut: "missing", handlers: "missing"})

    function init() {
        answered = {}
        tryVerify(() => {
            const kddw = JSON.parse(DockLayout.serialize())
            return DockLayout.mainWindows().length === 0
                   && !(Array.isArray(kddw.allDockWidgets) && kddw.allDockWidgets.length > 0)
        }, 5000, "the earlier shell's docks are gone")
    }

    function cleanup() {
        for (const w of windows)
            w.destroy()
        windows = []
        wait(0)
    }

    function makeShell(core, dir, setup) {
        const shell = shellComponent.createObject(tc, {core: core, stateDir: dir, setup: setup})
        verify(shell, "ShellWindow created")
        windows = windows.concat([shell])
        shell.visible = true
        return shell
    }

    function dropShell(shell) {
        windows = windows.filter(w => w !== shell)
        shell.destroy()
        tryVerify(() => {
            const kddw = JSON.parse(DockLayout.serialize())
            return DockLayout.mainWindows().length === 0
                   && !(Array.isArray(kddw.allDockWidgets) && kddw.allDockWidgets.length > 0)
        }, 5000, "the shell's docks are gone")
    }

    function answerRead(core, path) {
        let call = null
        tryVerify(() => (call = core.calls.find(c => c.name === "desktop.get" && !answered[c.id]) || null) !== null,
                  5000, "a desktop.get")
        answered[call.id] = true
        compare(call.args.fresh, true, "never the core's 10 s cache")
        verify(core.respond(call.id, Shapes.tool(Shapes.desktopSettings({notification_path: path}))))
    }

    function reads(core) {
        return core.calls.filter(c => c.name === "desktop.get").length
    }

    function test_card_shows_on_the_first_start_while_setup_is_missing() { // spec: APP-022
        const dir = TestFiles.newDir("first")
        const core = fakeComponent.createObject(tc)
        const shell = makeShell(core, dir, fakeSetupComponent.createObject(tc))
        const card = shell.firstStartCard
        verify(!card.visible, "nothing until the core has said")
        answerRead(core, missing)
        tryVerify(() => card.visible, 5000, "it offers the setup")
        verify(card.setUpButton.visible)
        verify(card.notNowButton.visible)
        verify(card.text.indexOf("a Start-menu shortcut and a link handler") >= 0, card.text)
        verify(card.text.indexOf(".lilref") < 0, card.text)
        // Above the docks, not over them.
        tryVerify(() => card.mapToItem(null, 0, 0).y > 0 && shell.dockingArea.mapToItem(null, 0, 0).y >= card.mapToItem(null, 0, card.height).y,
                  5000, "the card lies between the bar and the docks")
        compare(walk.report(card), [])
    }

    function test_card_says_only_what_set_up_would_write() { // spec: APP-022
        const cases = [
            [{toasts: "ready", shortcut: "missing", handlers: "ok"}, "a Start-menu shortcut,", ""],
            [{toasts: "ready", shortcut: "wrong", handlers: "ok"}, "a Start-menu shortcut,", ""],
            [{toasts: "ready", shortcut: "ok", handlers: "missing"}, "a link handler,", ""],
            [{toasts: "ready", shortcut: "missing", handlers: "other_channel"}, "a Start-menu shortcut,", "lil doctor"],
        ]
        for (const [path, words, note] of cases) {
            answered = {}
            const core = fakeComponent.createObject(tc)
            const shell = makeShell(core, TestFiles.newDir("words"), fakeSetupComponent.createObject(tc))
            answerRead(core, path)
            tryVerify(() => shell.firstStartCard.visible, 5000)
            const text = shell.firstStartCard.text
            verify(text.indexOf(words) >= 0, text)
            if (note !== "")
                verify(text.indexOf(note) >= 0, text)
            dropShell(shell)
        }
    }

    function test_card_stays_away_while_nothing_is_missing() { // spec: APP-022
        // What Set up can't write is no reason to show it: `not_yet_shown` and Windows' reasons alone
        // are the Settings panel's to say, and so is a scheme another exe holds.
        for (const path of [
            {toasts: "ready", shortcut: "ok", handlers: "ok"},
            {toasts: "not_yet_shown", shortcut: "ok", handlers: "ok"},
            {toasts: "unavailable", toasts_reason: "disabled_for_app", shortcut: "ok", handlers: "ok"},
            {toasts: "ready", shortcut: "ok", handlers: "other_channel"}]) {
            answered = {}
            const dir = TestFiles.newDir("nothing")
            const core = fakeComponent.createObject(tc)
            const shell = makeShell(core, dir, fakeSetupComponent.createObject(tc))
            answerRead(core, path)
            wait(50)
            verify(!shell.firstStartCard.visible, JSON.stringify(path))
            dropShell(shell)
        }
    }

    function test_card_never_shows_once_state_json_exists() { // spec: APP-022, APP-111
        const dir = TestFiles.newDir("seen")
        verify(TestFiles.write(dir + "/state.json", JSON.stringify({state: 1, workspaces: {}})))
        const core = fakeComponent.createObject(tc)
        const shell = makeShell(core, dir, fakeSetupComponent.createObject(tc))
        const card = shell.firstStartCard
        tryVerify(() => shell.docks.home.content && shell.docks.home.content.width > 0, 5000, "the docks are laid out")
        wait(100)
        compare(reads(core), 0, "it doesn't even ask")
        verify(!card.visible)
    }

    function test_a_state_set_aside_isnt_a_first_start() { // spec: APP-022, APP-111
        const dir = TestFiles.newDir("aside")
        verify(TestFiles.write(dir + "/state.json.bad-20261031T120000Z", "{nope"))
        const core = fakeComponent.createObject(tc)
        const shell = makeShell(core, dir, fakeSetupComponent.createObject(tc))
        wait(100)
        compare(reads(core), 0)
        verify(!shell.firstStartCard.visible)
    }

    function test_no_state_folder_means_no_card() { // spec: APP-022
        const core = fakeComponent.createObject(tc)
        const shell = makeShell(core, "", fakeSetupComponent.createObject(tc))
        wait(100)
        compare(reads(core), 0)
        verify(!shell.firstStartCard.firstStart)
    }

    function test_not_now_dismisses_it() { // spec: APP-022
        const dir = TestFiles.newDir("notnow")
        const core = fakeComponent.createObject(tc)
        const setup = fakeSetupComponent.createObject(tc)
        const shell = makeShell(core, dir, setup)
        const card = shell.firstStartCard
        answerRead(core, missing)
        tryVerify(() => card.visible, 5000)
        verify(TestFiles.list(dir).indexOf("state.json") >= 0, "the first start is on record")
        compare(JSON.parse(TestFiles.read(dir + "/state.json")).state, 1)
        tryVerify(() => shell.dockingArea.mapToItem(null, 0, 0).y >= card.mapToItem(null, 0, card.height).y && card.mapToItem(null, 0, 0).y > 0, 5000)
        const bottom = shell.dockingArea.mapToItem(null, 0, 0).y
        card.notNowButton.click()
        verify(!card.visible)
        tryVerify(() => shell.dockingArea.mapToItem(null, 0, 0).y < bottom, 5000, "the docks take its room")
        compare(setup.runs, 0)
        // It doesn't come back with the next answer or event.
        core.emit("desktop", "resync_required", null)
        wait(50)
        verify(!card.visible)
        dropShell(shell)

        // A later start: never again, and Settings still has the button.
        answered = {}
        const again = fakeComponent.createObject(tc)
        const later = makeShell(again, dir, fakeSetupComponent.createObject(tc))
        tryVerify(() => later.docks.settings.content, 5000)
        wait(100)
        verify(!later.firstStartCard.firstStart)
        verify(!later.firstStartCard.visible)
        const dock = later.docks.settings
        tryVerify(() => later.docks.home.content && later.docks.home.content.width > 0, 5000, "the docks are laid out")
        dock.setAsCurrentTab()
        answerRead(again, missing)
        tryVerify(() => dock.content.pathCard.setUpButton.visible, 5000, "Settings still does")
    }

    function test_a_start_that_was_left_unanswered_counts() { // spec: APP-022
        const dir = TestFiles.newDir("left")
        const core = fakeComponent.createObject(tc)
        const shell = makeShell(core, dir, fakeSetupComponent.createObject(tc))
        answerRead(core, missing)
        tryVerify(() => shell.firstStartCard.visible, 5000)
        dropShell(shell)
        answered = {}
        const later = makeShell(fakeComponent.createObject(tc), dir, fakeSetupComponent.createObject(tc))
        wait(100)
        verify(!later.firstStartCard.visible)
    }

    function test_set_up_on_the_card_leaves_at_once_when_the_core_says_ok() { // spec: APP-022, P4a-R26
        const core = fakeComponent.createObject(tc)
        const setup = fakeSetupComponent.createObject(tc)
        const shell = makeShell(core, TestFiles.newDir("setup"), setup)
        const card = shell.firstStartCard
        answerRead(core, missing)
        tryVerify(() => card.visible && card.setUpButton.enabled, 5000)
        card.setUpButton.click()
        compare(setup.runs, 1)
        compare(card.setUpButton.text, "Setting up…")
        card.setUpButton.click()
        compare(setup.runs, 1, "two presses run once")
        setup.finish(Shapes.setupReport({notification_path: {toasts: "not_yet_shown", shortcut: "ok", handlers: "ok"}}))
        tryVerify(() => !card.visible, 5000, "it leaves at once")
        // Settings heard the same report.
        const panel = shell.docks.settings.content
        compare(panel.path.shortcut, "ok")
    }

    function test_a_set_up_that_fails_partway_stays_and_says_which() { // spec: APP-022
        const core = fakeComponent.createObject(tc)
        const setup = fakeSetupComponent.createObject(tc)
        const shell = makeShell(core, TestFiles.newDir("partway"), setup)
        const card = shell.firstStartCard
        answerRead(core, missing)
        tryVerify(() => card.visible && card.setUpButton.enabled, 5000)
        card.setUpButton.click()
        setup.finish(Shapes.setupReport({scheme: {state: "failed", why: "Access is denied."},
                                         notification_path: {toasts: "ready", shortcut: "ok", handlers: "missing"}}))
        tryVerify(() => card.text.indexOf("couldn't be registered: Access is denied.") >= 0, 5000, card.text)
        verify(card.text.indexOf("The Start-menu shortcut is set up.") >= 0, card.text)
        verify(card.visible, "still missing, so still offered")
        verify(card.setUpButton.enabled)
        verify(card.notNowButton.visible, "and the way out is there")
    }

    function stateFile(dir) {
        return TestFiles.list(dir).indexOf("state.json") >= 0
    }

    // No answer uses nothing up: the promise is an offer you actually saw.
    function test_a_start_with_no_answer_stays_a_first_start() { // spec: APP-022
        const dir = TestFiles.newDir("noanswer2")
        const core = fakeComponent.createObject(tc)
        const shell = makeShell(core, dir, fakeSetupComponent.createObject(tc))
        tryVerify(() => reads(core) === 1, 5000)
        verify(!stateFile(dir), "nothing written until the core answers")
        let call = core.calls.find(c => c.name === "desktop.get")
        answered[call.id] = true
        core.respond(call.id, Shapes.refusal("core.unreachable", "No core."))
        wait(50)
        verify(!stateFile(dir), "nor when it fails")
        dropShell(shell)

        answered = {}
        const again = fakeComponent.createObject(tc)
        const later = makeShell(again, dir, fakeSetupComponent.createObject(tc))
        answerRead(again, missing)
        tryVerify(() => later.firstStartCard.visible, 5000, "still offered")
        verify(stateFile(dir), "and recorded now")
    }

    function test_an_answer_with_nothing_missing_is_recorded_and_ends_the_offer() { // spec: APP-022
        const dir = TestFiles.newDir("recorded")
        const core = fakeComponent.createObject(tc)
        const shell = makeShell(core, dir, fakeSetupComponent.createObject(tc))
        answerRead(core, {toasts: "ready", shortcut: "ok", handlers: "ok"})
        tryVerify(() => stateFile(dir), 5000)
        compare(JSON.parse(TestFiles.read(dir + "/state.json")), {state: 1, workspaces: {}})
        // Days later, a resync finds something missing: too late to be a first start's offer.
        const before = reads(core)
        wait(1100)
        core.emit("desktop", "resync_required", null)
        wait(1100)
        compare(reads(core), before, "it has stopped reading")
        verify(!shell.firstStartCard.visible)
    }

    function test_a_card_that_left_stays_gone() { // spec: APP-022
        const core = fakeComponent.createObject(tc)
        const setup = fakeSetupComponent.createObject(tc)
        const shell = makeShell(core, TestFiles.newDir("gone2"), setup)
        const card = shell.firstStartCard
        answerRead(core, missing)
        tryVerify(() => card.visible, 5000)
        // Fixed elsewhere: a resync says so, and the card goes.
        wait(1100)
        core.emit("desktop", "resync_required", null)
        answerRead(core, {toasts: "ready", shortcut: "ok", handlers: "ok"})
        tryVerify(() => !card.visible, 5000)
        const before = reads(core)
        wait(1100)
        core.emit("desktop", "resync_required", null)
        wait(1100)
        compare(reads(core), before, "it has stopped reading")
        verify(!card.visible)
    }

    // Home's own write can get there first on a slow answer.
    function test_the_record_never_replaces_a_file() { // spec: APP-022, APP-111
        const dir = TestFiles.newDir("home")
        const core = fakeComponent.createObject(tc)
        const shell = makeShell(core, dir, fakeSetupComponent.createObject(tc))
        tryVerify(() => reads(core) === 1, 5000)
        const home = JSON.stringify({state: 1, workspaces: {ws1: {last_seen_at: "2026-10-31T10:00:00Z"}}})
        verify(TestFiles.write(dir + "/state.json", home))
        answerRead(core, missing)
        tryVerify(() => shell.firstStartCard.visible, 5000, "this start began as a first")
        compare(TestFiles.read(dir + "/state.json"), home)
    }

    // On the stack with Home: its last-seen write, through the same ShellState call Home makes, lands
    // before the card's first answer. This start was a first, so the card still shows, and Home's row
    // is kept, since the record never replaces a file.
    function test_home_s_write_first_keeps_the_offer_and_home_s_row() { // spec: APP-022, APP-111
        const dir = TestFiles.newDir("homefirst")
        const core = fakeComponent.createObject(tc)
        const shell = makeShell(core, dir, fakeSetupComponent.createObject(tc))
        verify(shell.firstStartCard.firstStart)
        tryVerify(() => reads(core) >= 1, 5000)
        compare(ShellState.setLastSeen(dir, "ws1", "2026-10-31T10:00:00Z"), "")
        verify(TestFiles.list(dir).indexOf("state.json") >= 0)
        answerRead(core, missing)
        tryVerify(() => shell.firstStartCard.visible, 5000, "still offered")
        compare(ShellState.lastSeen(dir, "ws1"), "2026-10-31T10:00:00Z", "Home's row is kept")
        compare(JSON.parse(TestFiles.read(dir + "/state.json")).workspaces.ws1.last_seen_at, "2026-10-31T10:00:00Z")
        // And the next start is no first start.
        dropShell(shell)
        answered = {}
        const later = makeShell(fakeComponent.createObject(tc), dir, fakeSetupComponent.createObject(tc))
        verify(!later.firstStartCard.firstStart)
    }

    function test_a_read_open_when_set_up_answers_doesnt_undo_it() { // spec: APP-022
        const core = fakeComponent.createObject(tc)
        const setup = fakeSetupComponent.createObject(tc)
        const shell = makeShell(core, TestFiles.newDir("late"), setup)
        const card = shell.firstStartCard
        answerRead(core, missing)
        tryVerify(() => card.visible && card.setUpButton.enabled, 5000)
        wait(1100)
        const before = reads(core)
        core.emit("desktop", "resync_required", null)
        tryVerify(() => reads(core) === before + 1, 5000)
        card.setUpButton.click()
        const after = {toasts: "ready", shortcut: "ok", handlers: "missing"}
        setup.finish(Shapes.setupReport({scheme: {state: "failed", why: "Access is denied."}, notification_path: after}))
        answerRead(core, missing)
        wait(50)
        compare(card.path.shortcut, "ok", "the read was made before the setup's")
        tryVerify(() => reads(core) === before + 2, 5000, "a read that knows follows")
    }

    function test_a_failed_reread_doesnt_stay_once_a_read_answers() { // spec: APP-022
        const core = fakeComponent.createObject(tc)
        const setup = fakeSetupComponent.createObject(tc)
        const shell = makeShell(core, TestFiles.newDir("stuck"), setup)
        const card = shell.firstStartCard
        answerRead(core, missing)
        tryVerify(() => card.visible && card.setUpButton.enabled, 5000)
        card.setUpButton.click()
        setup.finish(Shapes.setupReport({notification_path: null, reread_error: "no answer within 5 s"}))
        tryVerify(() => card.text.indexOf("couldn't say") >= 0, 5000, card.text)
        answerRead(core, {toasts: "ready", shortcut: "ok", handlers: "missing"})
        tryVerify(() => card.text.indexOf("couldn't say") < 0, 5000, card.text)
        verify(card.visible)
    }

    function test_set_up_that_cant_start_is_said() { // spec: APP-156
        const core = fakeComponent.createObject(tc)
        const setup = fakeSetupComponent.createObject(tc, {refuses: true})
        const shell = makeShell(core, TestFiles.newDir("nostart"), setup)
        const card = shell.firstStartCard
        answerRead(core, missing)
        tryVerify(() => card.visible && card.setUpButton.enabled, 5000)
        const said = []
        card.announced.connect(t => said.push(t))
        card.setUpButton.click()
        compare(said, ["Set up didn't start. Run lil doctor instead."])
        compare(card.text, "Set up didn't start. Run lil doctor instead.")
    }

    function test_a_fresh_read_takes_away_the_failure_of_what_is_fine() { // spec: APP-022
        const core = fakeComponent.createObject(tc)
        const setup = fakeSetupComponent.createObject(tc)
        const shell = makeShell(core, TestFiles.newDir("fine"), setup)
        const card = shell.firstStartCard
        answerRead(core, missing)
        tryVerify(() => card.visible && card.setUpButton.enabled, 5000)
        card.setUpButton.click()
        setup.finish(Shapes.setupReport({shortcut: {state: "failed", why: "Access is denied."}, scheme: {state: "written"},
                                         notification_path: missing}))
        tryVerify(() => card.text.indexOf("couldn't be written") >= 0, 5000, card.text)
        wait(1100)
        core.emit("desktop", "resync_required", null)
        answerRead(core, {toasts: "ready", shortcut: "ok", handlers: "missing"})
        tryVerify(() => card.text.indexOf("couldn't be written") < 0, 5000, card.text)
        verify(card.visible)
    }

    function test_a_later_start_takes_nothing_from_a_settings_run() { // spec: APP-022
        const dir = TestFiles.newDir("later")
        verify(TestFiles.write(dir + "/state.json", JSON.stringify({state: 1, workspaces: {}})))
        const setup = fakeSetupComponent.createObject(tc)
        const shell = makeShell(fakeComponent.createObject(tc), dir, setup)
        wait(50)
        setup.finish(Shapes.setupReport({notification_path: missing}))
        wait(50)
        compare(shell.firstStartCard.report, null)
        compare(shell.firstStartCard.path, null)
        verify(!shell.firstStartCard.visible)
    }

    function test_set_up_s_outcome_is_said_and_the_keyboard_goes_on() { // spec: APP-156
        const core = fakeComponent.createObject(tc)
        const setup = fakeSetupComponent.createObject(tc)
        const shell = makeShell(core, TestFiles.newDir("said"), setup)
        const card = shell.firstStartCard
        answerRead(core, missing)
        tryVerify(() => card.visible && card.setUpButton.enabled, 5000)
        const said = []
        card.announced.connect(t => said.push(t))
        shell.requestActivate()
        tryVerify(() => shell.active, 5000)
        card.setUpButton.forceActiveFocus()
        keyClick(Qt.Key_Space)
        compare(setup.runs, 1)
        verify(card.setUpButton.enabled && card.setUpButton.activeFocus, "held while it runs")
        setup.finish(Shapes.setupReport())
        tryVerify(() => !card.visible, 5000)
        compare(said, ["The Start-menu shortcut and this LILITH's lil: links are set up."])
        tryVerify(() => !!shell.activeFocusItem && !String(shell.activeFocusItem).startsWith("QQuickRootItem"), 5000,
                  "the keyboard isn't dropped: " + shell.activeFocusItem)
    }

    // Put away during a run, the card says nothing more: Settings says why it failed.
    function test_not_now_during_a_run_leaves_the_failure_to_settings() { // spec: APP-022
        const core = fakeComponent.createObject(tc)
        const setup = fakeSetupComponent.createObject(tc)
        const shell = makeShell(core, TestFiles.newDir("during"), setup)
        const card = shell.firstStartCard
        answerRead(core, missing)
        tryVerify(() => card.visible && card.setUpButton.enabled, 5000)
        card.setUpButton.click()
        card.notNowButton.click()
        verify(!card.visible)
        setup.finish(Shapes.setupReport({shortcut: {state: "failed", why: "Access is denied."}, scheme: {state: "failed", why: "Access is denied."},
                                         notification_path: missing}))
        verify(!card.visible, "it doesn't come back for it")
        tryVerify(() => shell.docks.home.content && shell.docks.home.content.width > 0, 5000, "the docks are laid out")
        wait(100)
        const dock = shell.docks.settings
        dock.setAsCurrentTab()
        const panel = dock.content
        tryVerify(() => panel.shown, 5000, "Settings in front")
        answerRead(core, missing)
        tryVerify(() => panel.fresh, 5000)
        let found = false
        const visit = i => {
            if (i.visible && typeof i.text === "string" && typeof i.lineCount === "number"
                    && i.text.indexOf("couldn't be written: Access is denied.") >= 0)
                found = true
            for (let n = 0; n < i.children.length; ++n)
                visit(i.children[n])
        }
        visit(panel.pathCard)
        verify(found, "Settings shows why the last run failed")
    }

    function test_a_set_up_with_no_answer_from_the_core_reads_again() { // spec: APP-022
        const core = fakeComponent.createObject(tc)
        const setup = fakeSetupComponent.createObject(tc)
        const shell = makeShell(core, TestFiles.newDir("noanswer"), setup)
        const card = shell.firstStartCard
        answerRead(core, missing)
        tryVerify(() => card.visible && card.setUpButton.enabled, 5000)
        card.setUpButton.click()
        setup.finish(Shapes.setupReport({notification_path: null, reread_error: "no answer within 5 s"}))
        tryVerify(() => card.text.indexOf("couldn't say how things stand now: no answer within 5 s.") >= 0, 5000, card.text)
        answerRead(core, {toasts: "ready", shortcut: "ok", handlers: "ok"})
        tryVerify(() => !card.visible, 5000, "the session's read says it is done")
    }

    function test_a_failed_read_shows_no_card_and_ready_reads_again() { // spec: APP-022, ARCH-100
        const core = fakeComponent.createObject(tc)
        const shell = makeShell(core, TestFiles.newDir("failed"), fakeSetupComponent.createObject(tc))
        const card = shell.firstStartCard
        let call = null
        tryVerify(() => (call = core.calls.find(c => c.name === "desktop.get") || null) !== null, 5000)
        answered[call.id] = true
        core.respond(call.id, Shapes.refusal("core.unreachable", "No core."))
        wait(50)
        verify(!card.visible)
        core.applyState({connection: "reconnecting"})
        core.applyState({connection: "ready"})
        answerRead(core, missing)
        tryVerify(() => card.visible, 5000, "offered as the answer comes")
    }

    function test_a_missing_app_shows_no_card() { // spec: APP-022
        const core = fakeComponent.createObject(tc)
        const setup = fakeSetupComponent.createObject(tc, {appMissing: true, appPath: "C:\\gone\\lil-image.exe"})
        const shell = makeShell(core, TestFiles.newDir("gone"), setup)
        answerRead(core, missing)
        wait(50)
        verify(!shell.firstStartCard.visible)
    }

    function test_the_set_up_button_needs_the_core() { // spec: APP-034
        const core = fakeComponent.createObject(tc)
        const shell = makeShell(core, TestFiles.newDir("away"), fakeSetupComponent.createObject(tc))
        const card = shell.firstStartCard
        answerRead(core, missing)
        tryVerify(() => card.visible && card.setUpButton.enabled, 5000)
        core.applyState({connection: "reconnecting"})
        compare(card.setUpButton.enabled, false)
        verify(card.notNowButton.enabled, "Not now needs nothing of it")
        core.applyState({connection: "ready"})
        tryVerify(() => card.setUpButton.enabled, 5000)
    }

    // A first start with no workspace: both show, one above the other, each with its way out, and the
    // no-workspace state alone is no reason for the card.
    function test_card_and_banners_stack_with_every_way_out_in_sight() { // spec: APP-022, APP-016
        const core = fakeComponent.createObject(tc, {workspace: null})
        const shell = makeShell(core, TestFiles.newDir("stack"), fakeSetupComponent.createObject(tc))
        const card = shell.firstStartCard
        const banner = shell.noWorkspaceBanner
        answerRead(core, {toasts: "ready", shortcut: "ok", handlers: "ok"})
        tryVerify(() => banner.visible, 5000, "the no-workspace banner")
        wait(50)
        verify(!card.visible, "no card for the no-workspace state alone")
        dropShell(shell)

        answered = {}
        const core2 = fakeComponent.createObject(tc, {workspace: null})
        const shell2 = makeShell(core2, TestFiles.newDir("stack2"), fakeSetupComponent.createObject(tc))
        const card2 = shell2.firstStartCard
        const banner2 = shell2.noWorkspaceBanner
        answerRead(core2, missing)
        tryVerify(() => card2.visible && banner2.visible, 5000, "both")
        tryVerify(() => card2.mapToItem(null, 0, 0).y >= banner2.mapToItem(null, 0, banner2.height).y, 5000,
                  "the card under the banner")
        tryVerify(() => shell2.dockingArea.mapToItem(null, 0, 0).y >= card2.mapToItem(null, 0, card2.height).y, 5000,
                  "the docks under both")
    }
}
