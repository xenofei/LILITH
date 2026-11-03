// The Settings panel (APP-146, APP-022, ARCH-127, CORE-160; P4a-R26, P4a-R36) on FakeCore's scripted
// answers, shaped as the core's (CoreShapes.js), and a fake of the setup bridge: nothing here writes.
import QtQuick
import QtTest
import Lilith.Core
import Lilith.Shim
import Lilith.UiKit
import "CoreShapes.js" as Shapes
import "../qml/panels/diagnostics/NotificationText.js" as NotificationText

TestCase {
    id: tc
    name: "Settings"
    when: windowShown
    width: 200
    height: 100

    Component {
        id: fakeComponent
        FakeCore {}
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
        id: panelComponent
        Window {
            id: host
            required property var core
            required property var setup
            property var opened: []
            property bool opens: true
            readonly property alias panel: settings
            width: 1280
            height: 800
            visible: true
            color: Theme.bg

            SettingsPanel {
                id: settings
                anchors.fill: parent
                core: host.core
                title: "Settings"
                setup: host.setup
                openExternally: url => {
                    host.opened = host.opened.concat([url])
                    return host.opens
                }
            }
        }
    }

    Component {
        id: shellComponent
        ShellWindow {}
    }

    A11yWalk { id: walk }

    property var answered: ({})

    function init() {
        answered = {}
        wait(100)
    }

    function makeCore() {
        return fakeComponent.createObject(tc)
    }

    function makeSetup(over) {
        return fakeSetupComponent.createObject(tc, over || {})
    }

    function makeHost(core, setup) {
        const host = createTemporaryObject(panelComponent, tc, {core: core, setup: setup || makeSetup()})
        verify(host && host.panel, "SettingsPanel created")
        return host
    }

    function nextCall(core, pred) {
        let call = null
        tryVerify(() => (call = core.calls.find(c => pred(c) && !answered[c.id]) || null) !== null, 5000, "a call")
        return call
    }

    function answer(core, pred, outcome) {
        const call = nextCall(core, pred)
        answered[call.id] = true
        verify(core.respond(call.id, outcome), "answered " + call.name)
        return call
    }

    function isRead(c) {
        return c.name === "desktop.get"
    }

    function isSet(c) {
        return c.name === "app/desktop.set"
    }

    function readOk(settings) {
        return Shapes.tool(settings || Shapes.desktopSettings())
    }

    function setOk(settings) {
        return Shapes.app(settings)
    }

    function reads(core) {
        return core.calls.filter(isRead).length
    }

    function sets(core) {
        return core.calls.filter(isSet)
    }

    // A panel on its first answer.
    function loaded(core, settings, setup) {
        const host = makeHost(core, setup)
        answer(core, isRead, readOk(settings))
        tryVerify(() => host.panel.fresh, 5000, "the first answer")
        return host
    }

    function walkItems(item, fn) {
        fn(item)
        for (let n = 0; n < item.children.length; ++n)
            walkItems(item.children[n], fn)
    }

    // The switch called `name` under `root`, shown or not, or null.
    function toggleNamed(root, name) {
        let found = null
        walkItems(root, i => {
            if (typeof i.checked === "boolean" && i.text === name)
                found = i
        })
        return found
    }

    function buttonNamed(root, name) {
        let found = null
        walkItems(root, i => {
            if (typeof i.click === "function" && i.text === name && i.visible)
                found = i
        })
        return found
    }

    function reallyVisible(item) {
        for (let p = item; p; p = p.parent) {
            if (!p.visible)
                return false
        }
        return true
    }

    function texts(item) {
        const out = []
        const visit = i => {
            if (!i.visible)
                return
            if (typeof i.text === "string" && typeof i.lineCount === "number")
                out.push(i.text)
            for (let n = 0; n < i.children.length; ++n)
                visit(i.children[n])
        }
        visit(item)
        return out
    }

    function shows(item, text) {
        return texts(item).some(t => t.indexOf(text) >= 0)
    }

    readonly property var sixLabels: ["Job finished", "Job failed", "Confirmation waiting", "Spending alerts",
                                      "Director-task decisions", "Claude Code and agent activity"]

    function test_toggles_show_desktop_get() { // spec: APP-146, P4a-R36
        const core = makeCore()
        const settings = Shapes.desktopSettings()
        settings.notifications.cap_alert = false
        const host = loaded(core, settings)
        const panel = host.panel
        for (const label of sixLabels) {
            const toggle = toggleNamed(panel, label)
            verify(toggle && reallyVisible(toggle), label)
            compare(toggle.checked, label !== "Spending alerts", label)
        }
        compare(toggleNamed(panel, "Quiet while fullscreen").checked, true)
        compare(toggleNamed(panel, "Pause GPU work while a game or video is fullscreen").checked, false)
        compare(toggleNamed(panel, "Keep this PC awake while GPU work runs").checked, true)
        compare(toggleNamed(panel, "Job progress"), null, "no Job progress toggle")
        // Six categories, one quiet, in the Notifications card.
        let switches = 0
        walkItems(panel.notifications, i => {
            if (typeof i.checked === "boolean")
                ++switches
        })
        compare(switches, 7)
        verify(shows(panel, "Jobs held after a restart"), "Job failed says what else it covers")
        compare(callArgs(core, isRead).fresh, true, "never the core's 10 s cache")
    }

    function callArgs(core, pred) {
        return core.calls.find(pred).args
    }

    function test_each_toggle_calls_app_desktop_set_with_one_key() { // spec: APP-146, APP-039
        const cases = [
            ["Job finished", {notifications: {job_finished: false}}],
            ["Job failed", {notifications: {job_failed: false}}],
            ["Confirmation waiting", {notifications: {confirmation_waiting: false}}],
            ["Spending alerts", {notifications: {cap_alert: false}}],
            ["Claude Code and agent activity", {notifications: {agent_activity: false}}],
            ["Quiet while fullscreen", {quiet_while_fullscreen: false}],
            ["Pause GPU work while a game or video is fullscreen", {fullscreen_autopause: true}],
            ["Keep this PC awake while GPU work runs", {keep_awake_gpu: false}]
        ]
        const core = makeCore()
        let now = Shapes.desktopSettings()
        const host = loaded(core, now)
        const panel = host.panel
        for (const [label, params] of cases) {
            const before = sets(core).length
            const toggle = toggleNamed(panel, label)
            const was = toggle.checked
            toggle.click()
            compare(sets(core).length, before + 1, label)
            const call = sets(core)[before]
            compare(call.kind, "appCall")
            compare(call.args, params, label)
            compare(toggle.checked, was, label + " waits for the core's answer")
            // The core's answer: everything as it now is.
            now = JSON.parse(JSON.stringify(now))
            if (params.notifications)
                Object.assign(now.notifications, params.notifications)
            else
                Object.assign(now, params)
            answer(core, c => c.id === call.id, setOk(now))
            tryCompare(toggle, "checked", !was)
        }
    }

    function test_the_director_toggle_is_shown_held() { // spec: APP-146
        const core = makeCore()
        const host = loaded(core)
        const toggle = toggleNamed(host.panel, "Director-task decisions")
        compare(toggle.enabled, false)
        compare(toggle.checked, true, "the core's word, though it can't be changed yet")
        verify(toggle.opacity < 1, "and it looks off the controls")
        verify(toggle.Accessible.description.indexOf("isn't used yet") >= 0, toggle.Accessible.description)
        toggle.click()
        compare(sets(core), [])
        verify(shows(host.panel, "Director tasks arrive in a later version of LILITH, so this isn't used yet."))
    }

    function test_keep_awake_toggle_hidden_when_unavailable() { // spec: ARCH-127
        const core = makeCore()
        const host = loaded(core, Shapes.desktopSettings({keep_awake_available: false}))
        verify(!reallyVisible(toggleNamed(host.panel, "Keep this PC awake while GPU work runs")))
        verify(reallyVisible(toggleNamed(host.panel, "Pause GPU work while a game or video is fullscreen")))
    }

    function test_settings_event_updates_the_toggles() { // spec: APP-146
        const core = makeCore()
        const host = loaded(core)
        const panel = host.panel
        tryVerify(() => core.subscriberCount("desktop") > 0, 5000)
        const settings = Shapes.desktopSettings({quiet_while_fullscreen: false, fullscreen_autopause: true})
        settings.notifications.job_finished = false
        core.emit("desktop", "event", Shapes.settingsChanged(settings))
        tryCompare(toggleNamed(panel, "Job finished"), "checked", false)
        compare(toggleNamed(panel, "Quiet while fullscreen").checked, false)
        compare(toggleNamed(panel, "Pause GPU work while a game or video is fullscreen").checked, true)
        // Another kind of the topic's events is nothing to these switches.
        core.emit("desktop", "event", {kind: "fullscreen", on: true})
        core.emit("desktop", "event", {kind: "resumed", slept_from: "2026-10-31T01:00:00Z", at: "2026-10-31T02:00:00Z"})
        compare(toggleNamed(panel, "Quiet while fullscreen").checked, false)
        verify(panel.fresh)
    }

    function test_an_event_doesnt_take_a_toggle_with_a_set_out() { // spec: APP-146
        const core = makeCore()
        const host = loaded(core)
        const panel = host.panel
        tryVerify(() => core.subscriberCount("desktop") > 0, 5000)
        const failed = toggleNamed(panel, "Job failed")
        failed.click()
        const call = sets(core)[0]
        // Another app's change: this switch holds for its own answer, the others move.
        const other = Shapes.desktopSettings()
        other.notifications.job_finished = false
        core.emit("desktop", "event", Shapes.settingsChanged(other))
        tryCompare(toggleNamed(panel, "Job finished"), "checked", false)
        compare(failed.checked, true)
        const mine = Shapes.desktopSettings()
        mine.notifications.job_finished = false
        mine.notifications.job_failed = false
        answer(core, c => c.id === call.id, setOk(mine))
        tryCompare(failed, "checked", false)
        // The set's own event, after the answer, changes nothing.
        core.emit("desktop", "event", Shapes.settingsChanged(mine))
        compare(failed.checked, false)
    }

    function test_a_refused_set_restores_the_toggle_and_says_why() { // spec: APP-146, APP-039
        for (const [code, message] of [
            ["caller.not_allowed", "Only the LILITH app changes these settings."],
            // A -32602 as it really reaches the app (the e2e test checks it): a refusal.
            ["params.invalid", "The LILITH core answered with a protocol error (-32602): invalid params: \"job_failed\" is not a notification category"]]) {
            answered = {}
            const core = makeCore()
            const host = loaded(core)
            const panel = host.panel
            const toggle = toggleNamed(panel, "Job failed")
            toggle.click()
            verify(shows(panel, "Saving…"), "it says so while the set is out")
            answer(core, isSet, Shapes.refusal(code, message))
            compare(toggle.checked, true, "back as the core has it")
            tryVerify(() => panel.banner.visible, 5000)
            verify(panel.banner.text.indexOf(message) >= 0, panel.banner.text)
            verify(panel.banner.text.indexOf("Job failed") >= 0, panel.banner.text)
            verify(!shows(panel, "Saving…"))
            // A refusal: nothing changed, and the words say so.
            verify(panel.banner.text.indexOf("\"Job failed\" wasn't changed: ") === 0, panel.banner.text)
            // The next set that works takes the message away, and the switch still sets.
            wait(Qt.styleHints.mouseDoubleClickInterval + 50)
            toggle.click()
            const now = Shapes.desktopSettings()
            now.notifications.job_failed = false
            answer(core, isSet, setOk(now))
            tryCompare(toggle, "checked", false)
            verify(!panel.banner.visible, "no stuck message")
        }
    }

    function test_a_double_click_sends_one_set_and_ends_on_the_answer() { // spec: APP-146
        const core = makeCore()
        const host = loaded(core)
        const toggle = toggleNamed(host.panel, "Job failed")
        toggle.click()
        toggle.click()
        toggle.click()
        compare(sets(core).length, 1, "held while the first is out")
        compare(toggle.checked, true, "and shows no guess")
        const now = Shapes.desktopSettings()
        now.notifications.job_failed = false
        answer(core, isSet, setOk(now))
        tryCompare(toggle, "checked", false)
        wait(50)
        compare(toggle.checked, false, "a double-click doesn't undo itself")
        compare(sets(core).length, 1)
    }

    // Real answers take a few ms, so the first can land before a double-click's second click.
    function test_a_double_click_answered_at_once_sends_one_set() { // spec: APP-146
        const core = makeCore()
        const host = loaded(core)
        const toggle = toggleNamed(host.panel, "Job failed")
        toggle.click()
        const now = Shapes.desktopSettings()
        now.notifications.job_failed = false
        answer(core, isSet, setOk(now))
        tryCompare(toggle, "checked", false)
        toggle.click()
        compare(sets(core).length, 1, "the second click is the same double-click")
        compare(toggle.checked, false)
        // A flip after that is a flip.
        wait(Qt.styleHints.mouseDoubleClickInterval + 50)
        toggle.click()
        compare(sets(core).length, 2)
        compare(sets(core)[1].args, {notifications: {job_failed: true}})
    }

    function test_a_read_open_when_a_set_answers_doesnt_undo_it() { // spec: APP-146
        const core = makeCore()
        const host = loaded(core)
        const panel = host.panel
        const toggle = toggleNamed(panel, "Job failed")
        wait(1100)
        panel.desktopRead.request()
        tryVerify(() => reads(core) === 2, 5000)
        toggle.click()
        const now = Shapes.desktopSettings()
        now.notifications.job_failed = false
        answer(core, isSet, setOk(now))
        tryCompare(toggle, "checked", false)
        // The read was made before the set: what it says is older.
        answer(core, isRead, readOk())
        wait(50)
        compare(toggle.checked, false, "an older read doesn't undo the answer")
        tryVerify(() => reads(core) === 3, 5000, "and a read that knows follows")
    }

    function test_a_read_open_when_an_event_comes_doesnt_undo_it() { // spec: APP-146
        const core = makeCore()
        const host = loaded(core)
        const panel = host.panel
        const toggle = toggleNamed(panel, "Job finished")
        tryVerify(() => core.subscriberCount("desktop") > 0, 5000)
        wait(1100)
        panel.desktopRead.request()
        tryVerify(() => reads(core) === 2, 5000)
        const now = Shapes.desktopSettings()
        now.notifications.job_finished = false
        core.emit("desktop", "event", Shapes.settingsChanged(now))
        tryCompare(toggle, "checked", false)
        answer(core, isRead, readOk())
        wait(50)
        compare(toggle.checked, false, "an older read doesn't undo the event")
        tryVerify(() => reads(core) === 3, 5000, "and a read that knows follows")
    }

    function test_a_read_open_when_set_up_answers_doesnt_undo_it() { // spec: APP-022
        const core = makeCore()
        const setup = makeSetup()
        const missing = {toasts: "ready", shortcut: "missing", handlers: "ok"}
        const host = loaded(core, Shapes.desktopSettings({notification_path: missing}), setup)
        const panel = host.panel
        wait(1100)
        panel.desktopRead.request()
        tryVerify(() => reads(core) === 2, 5000)
        setup.finish(Shapes.setupReport())
        tryVerify(() => !panel.pathCard.setUpButton.visible, 5000)
        answer(core, isRead, readOk(Shapes.desktopSettings({notification_path: missing})))
        wait(50)
        verify(!panel.pathCard.setUpButton.visible, "the setup's re-read is newer than the read that was open")
        compare(panel.path.shortcut, "ok")
    }

    function test_set_up_after_a_failed_read_reads_again() { // spec: APP-022
        const core = makeCore()
        const setup = makeSetup()
        const host = loaded(core, undefined, setup)
        const panel = host.panel
        wait(1100)
        panel.desktopRead.request()
        answer(core, isRead, Shapes.refusal("store.io", "No."))
        tryVerify(() => !panel.fresh, 5000)
        wait(1100)
        const before = reads(core)
        setup.finish(Shapes.setupReport())
        tryVerify(() => reads(core) === before + 1, 5000, "the report's path alone doesn't end the failed read")
        answer(core, isRead, readOk())
        tryVerify(() => panel.fresh, 5000)
    }

    function hideAndShow(panel) {
        panel.visible = false
        tryVerify(() => !panel.shown, 5000)
        panel.visible = true
        tryVerify(() => panel.shown, 5000)
    }

    function test_a_report_ends_when_the_panel_is_left() { // spec: APP-022
        const core = makeCore()
        const setup = makeSetup()
        const host = loaded(core, Shapes.desktopSettings({notification_path: {toasts: "ready", shortcut: "missing", handlers: "ok"}}), setup)
        const panel = host.panel
        const card = panel.pathCard
        tryVerify(() => panel.shown && card.setUpButton.enabled, 5000)
        setup.finish(Shapes.setupReport({notification_path: {toasts: "ready", shortcut: "missing", handlers: "ok"}}))
        tryVerify(() => shows(card, "are set up"), 5000, texts(card).join(" | "))
        hideAndShow(panel)
        answer(core, isRead, readOk(Shapes.desktopSettings({notification_path: {toasts: "ready", shortcut: "missing", handlers: "ok"}})))
        tryVerify(() => panel.fresh, 5000)
        verify(!shows(card, "are set up"), "not a stale line: " + texts(card).join(" | "))

        setup.refuses = true
        card.setUpButton.click()
        verify(shows(card, "Set up didn't start"))
        hideAndShow(panel)
        answer(core, isRead, readOk(Shapes.desktopSettings({notification_path: {toasts: "ready", shortcut: "missing", handlers: "ok"}})))
        tryVerify(() => panel.fresh, 5000)
        verify(!shows(card, "Set up didn't start"), texts(card).join(" | "))
    }

    // A run from the first-start card, say, put away there: why it failed is here.
    function test_a_report_that_came_behind_a_tab_still_shows() { // spec: APP-022
        const core = makeCore()
        const setup = makeSetup()
        const host = loaded(core, Shapes.desktopSettings({notification_path: {toasts: "ready", shortcut: "missing", handlers: "ok"}}), setup)
        const panel = host.panel
        const card = panel.pathCard
        tryVerify(() => panel.shown, 5000)
        panel.visible = false
        tryVerify(() => !panel.shown, 5000)
        setup.finish(Shapes.setupReport({shortcut: {state: "failed", why: "Access is denied."}, scheme: {state: "ok"},
                                         notification_path: {toasts: "ready", shortcut: "missing", handlers: "ok"}}))
        panel.visible = true
        tryVerify(() => panel.shown, 5000)
        answer(core, isRead, readOk(Shapes.desktopSettings({notification_path: {toasts: "ready", shortcut: "missing", handlers: "ok"}})))
        tryVerify(() => panel.fresh, 5000)
        verify(shows(card, "couldn't be written: Access is denied."), texts(card).join(" | "))
        hideAndShow(panel)
        answer(core, isRead, readOk(Shapes.desktopSettings({notification_path: {toasts: "ready", shortcut: "missing", handlers: "ok"}})))
        tryVerify(() => panel.fresh, 5000)
        verify(!shows(card, "Access is denied"), "seen, and then left: " + texts(card).join(" | "))
    }

    // Behind a tab it follows nothing, so the connection is what tells it.
    function test_a_loss_behind_a_tab_holds_until_a_read_after_it() { // spec: APP-146, ARCH-100
        const core = makeCore()
        const host = loaded(core)
        const panel = host.panel
        const toggle = toggleNamed(panel, "Job failed")
        tryVerify(() => toggle.enabled && panel.shown, 5000)
        panel.visible = false
        tryVerify(() => !panel.shown, 5000)
        core.applyState({connection: "reconnecting"})
        core.applyState({connection: "ready"})
        wait(2100)
        const before = reads(core)
        panel.visible = true
        tryVerify(() => panel.shown, 5000)
        verify(!panel.fresh, "old until a read made after the loss")
        compare(toggle.enabled, false)
        toggle.click()
        compare(sets(core), [], "no write from what may be old")
        tryVerify(() => reads(core) === before + 1, 5000)
        answer(core, isRead, readOk())
        tryVerify(() => panel.fresh, 5000)
        tryVerify(() => toggle.enabled, 5000)
    }

    function test_a_read_open_at_a_loss_doesnt_count_as_after_it() { // spec: APP-146, ARCH-100
        const core = makeCore()
        const host = loaded(core)
        const panel = host.panel
        wait(1100)
        panel.desktopRead.request()
        tryVerify(() => reads(core) === 2, 5000)
        core.applyState({connection: "reconnecting"})
        core.applyState({connection: "ready"})
        answer(core, isRead, readOk())
        wait(50)
        verify(!panel.fresh, "that read was made before the loss")
        tryVerify(() => reads(core) === 3, 5000, "the read after the loss")
        answer(core, isRead, readOk())
        tryVerify(() => panel.fresh, 5000)
    }

    // A same-core reconnect gets no mark; a new core's `ready` comes before its `resync_required`.
    function test_ready_reads_and_a_new_core_s_mark_reads_again() { // spec: APP-146, ARCH-100
        const core = makeCore()
        const host = loaded(core)
        const panel = host.panel
        tryVerify(() => core.subscriberCount("desktop") > 0, 5000)
        wait(1100)
        const before = reads(core)
        core.applyState({connection: "reconnecting"})
        verify(!panel.fresh)
        core.applyState({connection: "ready"})
        verify(!panel.fresh, "ready alone says nothing of what the switches show")
        tryVerify(() => reads(core) === before + 1, 5000, "ready reads")
        // The new core's mark, with that read open: only a read made after it counts.
        core.emit("desktop", "resync_required", null)
        answer(core, isRead, readOk())
        wait(50)
        verify(!panel.fresh, "that read was made before the mark")
        tryVerify(() => reads(core) === before + 2, 5000, "the read after the mark")
        answer(core, isRead, readOk())
        tryVerify(() => panel.fresh, 5000)
    }

    function test_read_again_hides_while_the_core_is_away() { // spec: APP-034
        const core = makeCore()
        const host = makeHost(core)
        const panel = host.panel
        answer(core, isRead, Shapes.refusal("store.io", "No."))
        tryVerify(() => panel.readAgainButton.visible, 5000)
        core.applyState({connection: "reconnecting"})
        verify(!panel.readAgainButton.visible, "it couldn't be pressed")
        verify(panel.statusLine.text.indexOf("read when it's back") >= 0, panel.statusLine.text)
        core.applyState({connection: "ready"})
        answer(core, isRead, readOk())
        tryVerify(() => panel.fresh, 5000)
    }

    function test_the_path_card_goes_first_while_something_needs_you() { // spec: APP-146, APP-156
        const core = makeCore()
        const setup = makeSetup()
        const host = loaded(core, Shapes.desktopSettings({notification_path: {toasts: "ready", shortcut: "missing", handlers: "ok"}}), setup)
        const panel = host.panel
        const card = panel.pathCard
        // Wide: at the top of its column; narrow, as the Default layout's dock is, at the top.
        tryVerify(() => panel.scroller.wide && card.y === 0 && panel.desktopToggles.y > 0, 5000, "wide: " + card.y)
        host.width = 620
        tryVerify(() => !panel.scroller.wide && card.y === 0 && panel.notifications.y > 0, 5000, "narrow")
        verify(panel.desktopToggles.y >= panel.notifications.y + panel.notifications.height, "the rest below")
        compare(panel.scroller.nextItemInFocusChain(true), card.setUpButton, "the keyboard's order is the same")
        setup.finish(Shapes.setupReport())
        tryVerify(() => !card.setUpButton.visible, 5000)
        wait(50)
        compare(card.y, 0, "a success's words stay in sight")
        tryVerify(() => card.y + card.height <= panel.scroller.height || card.height > panel.scroller.height, 5000)
        verify(shows(card, "are set up"), texts(card).join(" | "))
        verify(panel.scroller.nextItemInFocusChain(true) !== card.setUpButton)
        // Seen and left: back at the end.
        hideAndShow(panel)
        answer(core, isRead, readOk())
        tryVerify(() => card.y >= panel.desktopToggles.y + panel.desktopToggles.height, 5000, "back at the end")
        verify(panel.notifications.y === 0)
    }

    function test_the_switch_notes_are_their_descriptions() { // spec: APP-156
        const core = makeCore()
        const host = loaded(core)
        verify(toggleNamed(host.panel, "Job failed").Accessible.description.indexOf("Jobs held after a restart") >= 0)
        verify(toggleNamed(host.panel, "Quiet while fullscreen").Accessible.description.indexOf("notification centre") >= 0)
        compare(toggleNamed(host.panel, "Job finished").Accessible.description, "")
    }

    function test_two_sets_racing_end_on_what_the_core_last_said() { // spec: APP-146
        const core = makeCore()
        const host = loaded(core)
        const panel = host.panel
        const failed = toggleNamed(panel, "Job failed")
        const finished = toggleNamed(panel, "Job finished")
        failed.click()
        finished.click()
        const [first, second] = sets(core)
        // The core ran the second first, so the first's answer has both; they arrive out of order: the
        // second's, which doesn't know of the first, comes last.
        const both = Shapes.desktopSettings()
        both.notifications.job_failed = false
        both.notifications.job_finished = false
        const onlySecond = Shapes.desktopSettings()
        onlySecond.notifications.job_finished = false
        answer(core, c => c.id === first.id, setOk(both))
        tryCompare(failed, "checked", false)
        const before = reads(core)
        answer(core, c => c.id === second.id, setOk(onlySecond))
        tryCompare(finished, "checked", false)
        compare(failed.checked, false, "an older answer doesn't speak for another's key")
        // And the last to settle asks the core.
        tryVerify(() => reads(core) === before + 1, 5000, "a read after the race")
        answer(core, isRead, readOk(both))
        wait(50)
        compare(failed.checked, false)
        compare(finished.checked, false)
    }

    function test_writes_wait_for_the_first_answer() { // spec: APP-146, APP-033
        const core = makeCore()
        const host = makeHost(core)
        const panel = host.panel
        tryVerify(() => reads(core) === 1, 5000)
        verify(!reallyVisible(panel.notifications), "no switch before the core has said")
        verify(panel.statusLine.visible && panel.statusLine.text.indexOf("Reading") >= 0, panel.statusLine.text)
        verify(panel.pathCard.visible)
        verify(shows(panel.pathCard, "Not known until the core answers"))
        answer(core, isRead, readOk())
        tryVerify(() => reallyVisible(panel.notifications), 5000)
        verify(!panel.statusLine.visible)
    }

    function test_a_failed_read_says_so_and_ready_reads_again() { // spec: APP-146, ARCH-100
        const core = makeCore()
        const host = makeHost(core)
        const panel = host.panel
        answer(core, isRead, Shapes.refusal("store.io", "The settings file can't be read."))
        tryVerify(() => panel.statusLine.visible && panel.statusLine.text.indexOf("can't be read") >= 0, 5000)
        verify(!reallyVisible(panel.notifications), "no switch from nothing")
        verify(panel.readAgainButton.visible)
        verify(!NotificationText.setupOffered(panel.pathCard.path), "no Set up from nothing")
        // The core comes back: a read that failed goes again.
        core.applyState({connection: "reconnecting"})
        verify(panel.statusLine.text.indexOf("isn't connected") >= 0, panel.statusLine.text)
        core.applyState({connection: "ready"})
        answer(core, isRead, readOk())
        tryVerify(() => panel.fresh, 5000)
        verify(!panel.statusLine.visible, "no stuck message after the core returns")
        verify(reallyVisible(panel.notifications))
    }

    function test_read_again_after_a_failure() { // spec: APP-146
        const core = makeCore()
        const host = makeHost(core)
        const panel = host.panel
        answer(core, isRead, Shapes.refusal("store.io", "No."))
        tryVerify(() => panel.readAgainButton.visible, 5000)
        panel.readAgainButton.click()
        answer(core, isRead, readOk())
        tryVerify(() => panel.fresh, 5000)
    }

    function test_a_reload_reads_again_with_no_stale_switch_in_a_normal_colour() { // spec: APP-146, ARCH-100
        const core = makeCore()
        const host = loaded(core)
        const panel = host.panel
        const toggle = toggleNamed(panel, "Job failed")
        tryVerify(() => core.subscriberCount("desktop") > 0, 5000)
        compare(panel.notifications.opacity, 1)
        // A new core: the topic says so.
        const before = reads(core)
        core.emit("desktop", "resync_required", null)
        verify(!panel.fresh)
        compare(panel.notifications.opacity, 0.5)
        compare(toggle.enabled, false, "no write from what may be old")
        tryVerify(() => reads(core) === before + 1, 5000, "resync reads desktop.get")
        const now = Shapes.desktopSettings()
        now.notifications.job_failed = false
        // The re-read fails first: it says so, the switch stays dimmed.
        answer(core, isRead, Shapes.refusal("store.io", "Not yet."))
        tryVerify(() => panel.statusLine.text.indexOf("Not yet.") >= 0, 5000)
        verify(panel.statusLine.text.indexOf("what the core last said") >= 0, panel.statusLine.text)
        compare(panel.notifications.opacity, 0.5)
        verify(reallyVisible(panel.pathCard))
        compare(panel.pathCard.path, null, "no old path as if it were the core's word")
        panel.readAgainButton.click()
        answer(core, isRead, readOk(now))
        tryVerify(() => panel.fresh, 5000)
        compare(toggle.checked, false, "the new core's word")
        compare(panel.notifications.opacity, 1)
        tryVerify(() => toggle.enabled, 5000)
    }

    function test_a_snapshot_reads_again_too() { // spec: ARCH-100
        const core = makeCore()
        const host = loaded(core)
        tryVerify(() => core.subscriberCount("desktop") > 0, 5000)
        wait(1100)
        const before = reads(core)
        core.emit("desktop", "snapshot", null)
        tryVerify(() => reads(core) === before + 1, 5000)
    }

    function test_a_core_away_holds_the_switches_and_a_lost_answer_is_read_again() { // spec: APP-146, APP-034
        const core = makeCore()
        const host = loaded(core)
        const panel = host.panel
        const toggle = toggleNamed(panel, "Job failed")
        tryVerify(() => toggle.enabled, 5000)
        toggle.click()
        const call = sets(core)[0]
        core.applyState({connection: "reconnecting"})
        compare(toggle.enabled, false, "requiresCore")
        verify(panel.statusLine.text.indexOf("can't be changed now") >= 0, panel.statusLine.text)
        compare(panel.notifications.opacity, 0.5)
        // The set's answer is lost with the connection: maybe written.
        answer(core, c => c.id === call.id, Shapes.refusal("core.unreachable", "The core went away."))
        tryVerify(() => panel.banner.visible, 5000)
        verify(panel.banner.text.indexOf("may not have changed") >= 0, panel.banner.text)
        compare(toggle.checked, true, "what the core last said")
        wait(1100)
        const before = reads(core)
        core.applyState({connection: "ready"})
        tryVerify(() => reads(core) === before + 1, 5000, "read again as the core is back")
        const now = Shapes.desktopSettings()
        now.notifications.job_failed = false
        answer(core, isRead, readOk(now))
        tryCompare(toggle, "checked", false)
        verify(!panel.banner.visible, "no stuck message after the core returns")
        verify(panel.fresh)
    }

    function test_unreachable_path_shows_the_reason_and_a_fix() { // spec: APP-146, APP-022
        const core = makeCore()
        const host = loaded(core, Shapes.desktopSettings({notification_path: {
            toasts: "unavailable", toasts_reason: "disabled_for_app", shortcut: "ok", handlers: "ok"}}))
        const card = host.panel.pathCard
        verify(shows(card, "Notifications are off for LILITH in Windows Settings"), texts(card).join(" | "))
        verify(card.windowsButton.visible)
        compare(card.windowsButton.text, "Open Windows notification settings")
        verify(!card.setUpButton.visible, "Set up can't fix what Windows decides")
        card.windowsButton.click()
        compare(host.opened, ["ms-settings:notifications"])
        // It didn't open: it says so.
        host.opens = false
        card.windowsButton.click()
        verify(shows(card, "didn't open"))
        // Fixed there: Check again reads the core.
        wait(1100)
        const before = reads(core)
        card.checkButton.click()
        answer(core, isRead, readOk())
        tryVerify(() => shows(card, NotificationText.reachable), 5000)
        verify(!card.windowsButton.visible)
        verify(reads(core) === before + 1)
    }

    // Every state of the notification path: the words are true, and the button a sentence names is
    // there whenever it names it.
    function test_every_path_state_has_true_words_and_the_button_they_name() { // spec: APP-146, APP-022
        const path = over => Object.assign({toasts: "ready", shortcut: "ok", handlers: "ok"}, over)
        const off = reason => path({toasts: "unavailable", toasts_reason: reason})
        const cases = [
            // path, a sentence, Set up, Windows' page
            [path({}), NotificationText.reachable, false, false],
            [path({toasts: "not_yet_shown"}), NotificationText.reachable, false, false],
            [off("disabled_for_app"), "Notifications are off for LILITH", false, true],
            [off("disabled_for_user"), "Notifications are off in Windows Settings", false, true],
            [off("disabled_by_policy"), "A policy blocks notifications", false, false],
            [off("elevated"), "The core runs as administrator", false, false],
            [off("no_platform"), "Windows isn't showing LILITH's notifications", false, false],
            [off("a_later_reason"), "a_later_reason", false, false],
            [path({shortcut: "missing"}), "The Start-menu shortcut is missing", true, false],
            [path({shortcut: "wrong"}), "The Start-menu shortcut isn't right", true, false],
            [path({handlers: "missing"}), "This LILITH's lil: links aren't registered", true, false],
            [path({handlers: "other_channel"}), "Another LILITH build or another program", false, false],
            [path({shortcut: "missing", handlers: "other_channel"}), "Another LILITH build or another program", true, false],
        ]
        const core = makeCore()
        const host = loaded(core)
        const card = host.panel.pathCard
        tryVerify(() => core.subscriberCount("desktop") > 0, 5000)
        for (const [p, sentence, setUp, windows] of cases) {
            core.emit("desktop", "resync_required", null)
            answer(core, isRead, readOk(Shapes.desktopSettings({notification_path: p})))
            tryVerify(() => host.panel.fresh, 5000)
            verify(shows(card, sentence), sentence + ": " + texts(card).join(" | "))
            compare(card.setUpButton.visible, setUp, sentence)
            compare(card.windowsButton.visible, windows, sentence)
            const named = texts(card).some(t => t.indexOf("\"Set up\"") >= 0)
            if (named)
                verify(card.setUpButton.visible, "a sentence names a button that isn't there: " + sentence)
        }
    }

    // `lil doctor`'s rule: outside a test channel, nothing is offered while the app isn't there.
    function test_a_missing_app_offers_nothing_and_says_so() { // spec: APP-022
        const core = makeCore()
        const setup = makeSetup({appMissing: true, appPath: "C:\\gone\\lil-image.exe"})
        const host = loaded(core, Shapes.desktopSettings({notification_path: {toasts: "ready", shortcut: "missing", handlers: "missing"}}), setup)
        const card = host.panel.pathCard
        verify(!card.setUpButton.visible)
        verify(shows(card, "lil-image.exe isn't where the core looks for it"), texts(card).join(" | "))
        verify(!texts(card).some(t => t.indexOf("\"Set up\"") >= 0), "no sentence names the button")
        verify(shows(card, "lil-image.exe should be at C:\\gone\\lil-image.exe."), texts(card).join(" | "))
    }

    function test_set_up_button_writes_then_rereads() { // spec: APP-022, P4a-R26
        const core = makeCore()
        const setup = makeSetup()
        const host = loaded(core, Shapes.desktopSettings({notification_path: {toasts: "not_yet_shown", shortcut: "missing", handlers: "missing"}}), setup)
        const card = host.panel.pathCard
        verify(card.setUpButton.visible)
        verify(shows(card, "The Start-menu shortcut is missing"))
        tryVerify(() => card.setUpButton.enabled, 5000)
        const calls = core.calls.length
        card.setUpButton.click()
        compare(setup.runs, 1)
        compare(card.setUpButton.text, "Setting up…")
        verify(card.setUpButton.enabled, "the keyboard stays on it")
        // Two presses, one run.
        card.setUpButton.click()
        compare(setup.runs, 1)
        setup.finish(Shapes.setupReport({notification_path: {toasts: "not_yet_shown", shortcut: "ok", handlers: "ok"}}))
        // What the bridge's own desktop.get {fresh: true} said, with no call of the panel's.
        tryVerify(() => !card.setUpButton.visible, 5000, "the button leaves at once")
        verify(shows(card, NotificationText.reachable))
        verify(shows(card, "The Start-menu shortcut and this LILITH's lil: links are set up."), texts(card).join(" | "))
        compare(core.calls.length, calls, "the bridge read it")
    }

    function test_the_keyboard_stays_through_set_up() { // spec: APP-156
        const core = makeCore()
        const setup = makeSetup()
        const host = loaded(core, Shapes.desktopSettings({notification_path: {toasts: "ready", shortcut: "missing", handlers: "ok"}}), setup)
        const panel = host.panel
        const card = panel.pathCard
        host.requestActivate()
        tryVerify(() => host.active, 5000)
        tryVerify(() => card.setUpButton.enabled, 5000)
        card.setUpButton.forceActiveFocus()
        verify(card.setUpButton.activeFocus)
        keyClick(Qt.Key_Space)
        compare(setup.runs, 1)
        verify(card.setUpButton.activeFocus, "held while it runs")
        keyClick(Qt.Key_Space)
        compare(setup.runs, 1, "a second press runs nothing")
        setup.finish(Shapes.setupReport())
        tryVerify(() => !card.setUpButton.visible, 5000)
        tryVerify(() => panel.scroller.activeFocus, 5000, "the keyboard goes on to the list, not to nothing")
    }

    Component {
        id: outsideComponent
        Item {
            width: 10
            height: 10
            focus: true
            activeFocusOnTab: true
        }
    }

    // The keyboard moved on while it ran: it stays where it went.
    function test_set_up_takes_no_keyboard_from_where_it_went() { // spec: APP-156
        const core = makeCore()
        const setup = makeSetup()
        const host = loaded(core, Shapes.desktopSettings({notification_path: {toasts: "ready", shortcut: "missing", handlers: "ok"}}), setup)
        const panel = host.panel
        const card = panel.pathCard
        host.requestActivate()
        tryVerify(() => host.active, 5000)
        tryVerify(() => card.setUpButton.enabled, 5000)
        card.setUpButton.forceActiveFocus()
        keyClick(Qt.Key_Space)
        compare(setup.runs, 1)
        const toggle = toggleNamed(panel, "Job failed")
        toggle.forceActiveFocus()
        tryVerify(() => toggle.activeFocus, 5000)
        setup.finish(Shapes.setupReport())
        tryVerify(() => !card.setUpButton.visible, 5000)
        wait(50)
        verify(toggle.activeFocus, "the switch keeps it")
    }

    // A run that failed leaves nothing behind for a later hide to act on.
    function test_a_failed_run_takes_no_keyboard_later() { // spec: APP-156
        const core = makeCore()
        const setup = makeSetup()
        const host = loaded(core, Shapes.desktopSettings({notification_path: {toasts: "ready", shortcut: "missing", handlers: "ok"}}), setup)
        const panel = host.panel
        const card = panel.pathCard
        const outside = createTemporaryObject(outsideComponent, host.contentItem)
        host.requestActivate()
        tryVerify(() => host.active, 5000)
        tryVerify(() => card.setUpButton.enabled, 5000)
        card.setUpButton.forceActiveFocus()
        keyClick(Qt.Key_Space)
        setup.finish(Shapes.setupReport({shortcut: {state: "failed", why: "Access is denied."}, scheme: {state: "ok"},
                                         notification_path: {toasts: "ready", shortcut: "missing", handlers: "ok"}}))
        wait(50)
        verify(card.setUpButton.visible)
        outside.forceActiveFocus()
        tryVerify(() => outside.activeFocus, 5000)
        panel.visible = false
        wait(50)
        verify(outside.activeFocus, "a field elsewhere keeps it")
    }

    function test_a_clock_stepped_back_doesnt_stop_a_switch() { // spec: APP-146
        const core = makeCore()
        const host = loaded(core)
        const toggle = toggleNamed(host.panel, "Job failed")
        // The switch's row: as if the last set went out before the clock moved back an hour.
        toggle.parent.parent._lastSent = Date.now() + 3600000
        toggle.click()
        compare(sets(core).length, 1, "a press still sends")
    }

    function test_set_up_that_cant_start_is_said() { // spec: APP-156
        const core = makeCore()
        const setup = makeSetup({refuses: true})
        const host = loaded(core, Shapes.desktopSettings({notification_path: {toasts: "ready", shortcut: "missing", handlers: "ok"}}), setup)
        const said = []
        host.panel.announced.connect(t => said.push(t))
        tryVerify(() => host.panel.pathCard.setUpButton.enabled, 5000)
        host.panel.pathCard.setUpButton.click()
        compare(said, ["Set up didn't start. Run lil doctor instead."])
    }

    // The failure was fixed elsewhere, `lil doctor`, say: what the core reads now takes its words away.
    function test_a_fresh_read_takes_away_the_failure_of_what_is_fine() { // spec: APP-022, APP-146
        const core = makeCore()
        const setup = makeSetup()
        const missing = {toasts: "ready", shortcut: "missing", handlers: "missing"}
        const host = loaded(core, Shapes.desktopSettings({notification_path: missing}), setup)
        const panel = host.panel
        const card = panel.pathCard
        tryVerify(() => card.setUpButton.enabled, 5000)
        card.setUpButton.click()
        setup.finish(Shapes.setupReport({shortcut: {state: "failed", why: "Access is denied."},
                                         scheme: {state: "failed", why: "Access is denied."},
                                         notification_path: missing}))
        tryVerify(() => shows(card, "The Start-menu shortcut couldn't be written"), 5000, texts(card).join(" | "))
        verify(shows(card, "lil: links couldn't be registered"))
        // One part fixed: its words go, the other's stay.
        card.checkButton.click()
        answer(core, isRead, readOk(Shapes.desktopSettings({notification_path: {toasts: "ready", shortcut: "ok", handlers: "missing"}})))
        tryVerify(() => !shows(card, "The Start-menu shortcut couldn't be written"), 5000, texts(card).join(" | "))
        verify(shows(card, "lil: links couldn't be registered"), texts(card).join(" | "))
        // Both: nothing left over beside the good words.
        card.checkButton.click()
        answer(core, isRead, readOk())
        tryVerify(() => shows(card, NotificationText.reachable), 5000, texts(card).join(" | "))
        verify(!shows(card, "couldn't"), texts(card).join(" | "))
        verify(!shows(card, "Nothing needed writing"), texts(card).join(" | "))
    }

    // A run that stopped with an error never asked the core, and what it wrote isn't known.
    function test_a_run_that_stopped_says_what_isnt_known() { // spec: APP-022
        const core = makeCore()
        const setup = makeSetup()
        const host = loaded(core, Shapes.desktopSettings({notification_path: {toasts: "ready", shortcut: "missing", handlers: "missing"}}), setup)
        const panel = host.panel
        const card = panel.pathCard
        tryVerify(() => card.setUpButton.enabled, 5000)
        card.setUpButton.click()
        wait(1100)
        setup.finish(Shapes.setupReport({stopped: true, shortcut: {state: "not_tried"}, scheme: {state: "not_tried"},
                                         notification_path: null}))
        tryVerify(() => shows(card, "Set up stopped with an error, so what it wrote isn't known."), 5000, texts(card).join(" | "))
        verify(!shows(card, "The core couldn't say"), texts(card).join(" | "))
        // The panel asks the core itself. Still missing: the words stay. All fine: they go.
        answer(core, isRead, readOk(Shapes.desktopSettings({notification_path: {toasts: "ready", shortcut: "missing", handlers: "missing"}})))
        wait(50)
        verify(shows(card, "what it wrote isn't known"), texts(card).join(" | "))
        card.checkButton.click()
        answer(core, isRead, readOk())
        tryVerify(() => !shows(card, "what it wrote isn't known"), 5000, texts(card).join(" | "))
    }

    function test_the_words_promise_only_what_is_checked_and_fit_the_button() { // spec: APP-146
        const core = makeCore()
        const off = reason => ({toasts: "unavailable", toasts_reason: reason, shortcut: "ok", handlers: "ok"})
        const host = loaded(core, Shapes.desktopSettings())
        const card = host.panel.pathCard
        verify(shows(card, "Nothing LILITH can check stands in the way of its notifications."), texts(card).join(" | "))
        tryVerify(() => core.subscriberCount("desktop") > 0, 5000)
        for (const reason of ["disabled_for_app", "disabled_for_user", "disabled_by_policy", "elevated"]) {
            core.emit("desktop", "resync_required", null)
            answer(core, isRead, readOk(Shapes.desktopSettings({notification_path: off(reason)})))
            tryVerify(() => host.panel.fresh && card.checkButton.visible && shows(card, "then press Check again."), 5000, reason + ": " + texts(card).join(" | "))
            verify(!shows(card, "then run lil doctor"), reason)
        }
        // Where there is no such button, the doctor is still the way.
        verify(NotificationText.problems(off("disabled_for_app"))[0].indexOf("then run lil doctor to check the rest.") >= 0)
    }

    function test_set_up_s_outcome_is_announced() { // spec: APP-156
        const core = makeCore()
        const setup = makeSetup()
        const host = loaded(core, Shapes.desktopSettings({notification_path: {toasts: "ready", shortcut: "missing", handlers: "ok"}}), setup)
        const said = []
        host.panel.announced.connect(t => said.push(t))
        tryVerify(() => host.panel.pathCard.setUpButton.enabled, 5000)
        // A run pressed elsewhere is said elsewhere.
        setup.finish(Shapes.setupReport({notification_path: {toasts: "ready", shortcut: "missing", handlers: "ok"}}))
        compare(said, [])
        setup.busy = false
        host.panel.pathCard.setUpButton.click()
        setup.finish(Shapes.setupReport())
        compare(said, ["The Start-menu shortcut and this LILITH's lil: links are set up."])
    }

    function test_set_up_says_which_part_failed() { // spec: APP-022
        const core = makeCore()
        const setup = makeSetup()
        const missing = {toasts: "ready", shortcut: "missing", handlers: "missing"}
        const host = loaded(core, Shapes.desktopSettings({notification_path: missing}), setup)
        const card = host.panel.pathCard
        tryVerify(() => card.setUpButton.enabled, 5000)
        card.setUpButton.click()
        setup.finish(Shapes.setupReport({scheme: {state: "failed", why: "Access is denied."},
                                         notification_path: {toasts: "ready", shortcut: "ok", handlers: "missing"}}))
        tryVerify(() => shows(card, "This LILITH's lil: links couldn't be registered: Access is denied."), 5000, texts(card).join(" | "))
        verify(shows(card, "The Start-menu shortcut is set up."))
        verify(card.setUpButton.visible, "what is still missing can be tried again")
        verify(shows(card, "This LILITH's lil: links aren't registered"))
        verify(!shows(card, "The Start-menu shortcut is missing"))
    }

    function test_set_up_with_no_answer_from_the_core_reads_through_the_session() { // spec: APP-022
        const core = makeCore()
        const setup = makeSetup()
        const missing = {toasts: "ready", shortcut: "missing", handlers: "ok"}
        const host = loaded(core, Shapes.desktopSettings({notification_path: missing}), setup)
        const card = host.panel.pathCard
        tryVerify(() => card.setUpButton.enabled, 5000)
        card.setUpButton.click()
        wait(1100)
        const before = reads(core)
        setup.finish(Shapes.setupReport({scheme: {state: "ok"}, notification_path: null, reread_error: "no answer within 5 s"}))
        tryVerify(() => shows(card, "The core couldn't say how things stand now: no answer within 5 s."), 5000, texts(card).join(" | "))
        verify(shows(card, "The Start-menu shortcut is set up."))
        tryVerify(() => reads(core) === before + 1, 5000, "it asks again")
        answer(core, isRead, readOk())
        tryVerify(() => !card.setUpButton.visible, 5000)
        verify(!shows(card, "couldn't say"), "no stuck message")
    }

    function test_set_up_that_cant_start_says_so() { // spec: APP-022
        const core = makeCore()
        const setup = makeSetup({refuses: true})
        const host = loaded(core, Shapes.desktopSettings({notification_path: {toasts: "ready", shortcut: "missing", handlers: "ok"}}), setup)
        const card = host.panel.pathCard
        tryVerify(() => card.setUpButton.enabled, 5000)
        card.setUpButton.click()
        verify(shows(card, "Set up didn't start"), texts(card).join(" | "))
    }

    function test_a_set_up_needs_the_core() { // spec: APP-034
        const core = makeCore()
        const setup = makeSetup()
        const host = loaded(core, Shapes.desktopSettings({notification_path: {toasts: "ready", shortcut: "missing", handlers: "ok"}}), setup)
        const card = host.panel.pathCard
        tryVerify(() => card.setUpButton.enabled, 5000)
        core.applyState({connection: "reconnecting"})
        // What may be old isn't offered, until a read after the loss.
        verify(!card.setUpButton.visible)
        core.applyState({connection: "ready"})
        verify(!card.setUpButton.visible)
        answer(core, isRead, readOk(Shapes.desktopSettings({notification_path: {toasts: "ready", shortcut: "missing", handlers: "ok"}})))
        tryVerify(() => card.setUpButton.visible, 5000)
    }

    // The bridge's answer can land after the panel is gone.
    function test_a_setup_answer_after_the_panel_is_gone_is_nothing() { // spec: APP-022
        const core = makeCore()
        const setup = makeSetup()
        const host = panelComponent.createObject(tc, {core: core, setup: setup})
        answer(core, isRead, readOk(Shapes.desktopSettings({notification_path: {toasts: "ready", shortcut: "missing", handlers: "ok"}})))
        tryVerify(() => host.panel.pathCard.setUpButton.enabled, 5000)
        host.panel.pathCard.setUpButton.click()
        host.destroy()
        wait(0)
        setup.finish(Shapes.setupReport())
        wait(50)
        compare(setup.busy, false)
    }

    // The real bridge in a process with no core session (every QML test's): nothing runs, so no test
    // can reach a real registration.
    function test_the_real_bridge_runs_nothing_without_a_session() { // spec: APP-022, QA-038
        compare(SetupBridge.run(), false)
        compare(SetupBridge.busy, false)
        compare(SetupBridge.appMissing, false)
    }

    function test_nothing_to_write_says_so() { // spec: APP-022
        compare(NotificationText.setupOutcome(Shapes.setupReport({shortcut: {state: "ok"}, scheme: {state: "held"}}), t => t),
                "Nothing needed writing from here. Run lil doctor to check the rest.")
    }

    function test_a_later_core_s_category_shows_by_its_key_as_text() { // spec: APP-146, P4a-R8
        const core = makeCore()
        const settings = Shapes.desktopSettings()
        const key = "<img src=\"http://example.invalid/x.png\">"
        settings.notifications[key] = true
        const requests = TestFiles.networkRequests()
        const host = loaded(core, settings)
        const toggle = toggleNamed(host.panel, core.shownText(key, 80))
        verify(toggle, "shown by its key")
        toggle.click()
        const call = sets(core)[0]
        compare(Object.keys(call.args.notifications), [key])
        wait(50)
        compare(TestFiles.networkRequests(), requests, "markup loads nothing")
    }

    function test_an_outside_message_is_plain_text() { // spec: APP-146, P4a-R8
        const core = makeCore()
        const requests = TestFiles.networkRequests()
        const host = makeHost(core)
        const panel = host.panel
        answer(core, isRead, Shapes.refusal("store.io", "<img src=\"http://example.invalid/y.png\"> no"))
        tryVerify(() => panel.statusLine.text.indexOf("<img") >= 0, 5000)
        compare(panel.statusLine.textFormat, Text.PlainText)
        wait(50)
        compare(TestFiles.networkRequests(), requests)
    }

    function test_a_hidden_panel_reads_nothing_and_reads_when_shown() { // spec: APP-146
        const core = makeCore()
        const host = loaded(core)
        const panel = host.panel
        tryVerify(() => core.subscriberCount("desktop") > 0, 5000)
        wait(50)
        panel.visible = false
        tryVerify(() => core.subscriberCount("desktop") === 0, 5000, "follows nothing behind a tab")
        const before = reads(core)
        wait(2100)
        panel.visible = true
        tryVerify(() => reads(core) === before + 1, 5000, "reads as it shows")
        answer(core, isRead, readOk(Shapes.desktopSettings({quiet_while_fullscreen: false})))
        tryCompare(toggleNamed(panel, "Quiet while fullscreen"), "checked", false)
    }

    function test_a_toggle_keeps_the_keyboard_through_its_answer() { // spec: APP-156
        const core = makeCore()
        const host = loaded(core)
        const toggle = toggleNamed(host.panel, "Job failed")
        host.requestActivate()
        tryVerify(() => host.active, 5000)
        toggle.forceActiveFocus()
        tryVerify(() => toggle.enabled && toggle.activeFocus, 5000)
        keyClick(Qt.Key_Space)
        compare(sets(core).length, 1, "Space flips it")
        verify(toggle.activeFocus, "held, not disabled")
        const now = Shapes.desktopSettings()
        now.notifications.job_failed = false
        answer(core, isSet, setOk(now))
        tryCompare(toggle, "checked", false)
        verify(toggle === toggleNamed(host.panel, "Job failed"), "the same item")
        verify(toggle.activeFocus)
    }

    function test_the_walk_finds_nothing() { // spec: APP-156
        const core = makeCore()
        const setup = makeSetup()
        const host = loaded(core, Shapes.desktopSettings({notification_path: {
            toasts: "unavailable", toasts_reason: "disabled_for_app", shortcut: "missing", handlers: "other_channel"}}), setup)
        const panel = host.panel
        tryVerify(() => panel.pathCard.setUpButton.enabled, 5000)
        verify(panel.pathCard.windowsButton.visible)
        compare(walk.report(panel), [])
        verify(walk.interactiveCount(panel) >= 11, String(walk.interactiveCount(panel)))
    }

    function test_float_and_redock_keeps_settings_state() { // spec: ARCH-116
        const core = makeCore()
        const setup = makeSetup()
        const shell = createTemporaryObject(shellComponent, tc, {core: core, setup: setup})
        shell.visible = true
        const dock = shell.docks.settings
        tryVerify(() => shell.docks.home.content && shell.docks.home.content.width > 0, 5000, "the docks are laid out")
        tryVerify(() => dock.content && dock.content.core === core, 5000)
        const panel = dock.content
        compare(panel.setup, setup, "the shell gives it the shared bridge")
        wait(50)
        dock.setAsCurrentTab()
        tryVerify(() => panel.visible && panel.width > 0, 5000, "the settings tab is in front")
        const settings = Shapes.desktopSettings()
        settings.notifications.job_failed = false
        answer(core, isRead, readOk(settings))
        tryVerify(() => panel.fresh, 5000)
        const toggle = toggleNamed(panel, "Job failed")
        compare(toggle.checked, false)
        const calls = reads(core)
        dock.isFloating = true
        tryVerify(() => dock.isFloating && panel.Window.window && panel.Window.window !== shell, 5000,
                  "the panel floats in its own window")
        verify(dock.content === panel, "the same panel item")
        compare(toggleNamed(panel, "Job failed").checked, false)
        verify(toggle === toggleNamed(panel, "Job failed"), "the same switch")
        dock.isFloating = false
        tryVerify(() => !dock.isFloating && panel.Window.window === shell, 5000, "docked again")
        wait(50)
        verify(dock.content === panel)
        compare(toggle.checked, false)
        compare(reads(core), calls, "moving it reads nothing again")
    }
}
