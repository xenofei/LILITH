// The shell's windows and layout (APP-001 to APP-004, ARCH-116, P4a-R25): the restore order, the
// fall-back to the Default layout, a maximized round trip (KDDockWidgets #757), floating and
// re-docking, the window title and the shortcut map's keys. Each test that keeps a layout gives
// the shell its own temporary folder (TestFiles), never the app's real state folder.
import QtQuick
import QtTest
import Lilith.Shim
import Lilith.UiKit

TestCase {
    id: tc
    name: "Layout"
    when: windowShown
    width: 200
    height: 100

    FakeCore {
        id: fake
        project: "sketchbook"
    }

    Component {
        id: shellComponent
        ShellWindow { core: fake }
    }

    A11yWalk { id: walk }

    SignalSpy {
        id: pauseSpy
        signalName: "gpuPauseToggleRequested"
    }

    readonly property var defaultPanels: ["home", "jobs", "confirmations", "hardware", "usage",
                                          "diagnostics", "settings"]

    // The shell of the test before deletes its docks later (KDDockWidgets' names are
    // process-wide): wait until they are gone.
    function init() {
        waitDocksGone()
    }

    // No main window and no dock of an earlier shell is left in KDDockWidgets.
    function waitDocksGone() {
        tryVerify(() => {
            const kddw = JSON.parse(DockLayout.serialize())
            return DockLayout.mainWindows().length === 0
                   && !(Array.isArray(kddw.allDockWidgets) && kddw.allDockWidgets.length > 0)
        }, 5000, "the earlier shell's docks are gone")
    }

    // The shell's layout is built and laid out: every shown dock has its size, and the layout
    // text is the same at two checks in a row.
    function waitLaidOut(shell) {
        let last = ""
        tryVerify(() => {
            const text = shell.layoutText()
            const same = text === last
            last = text
            return same && shell.docksSized()
        }, 5000, "the layout settles")
    }

    function makeShell(dir) {
        const shell = createTemporaryObject(shellComponent, tc, {stateDir: dir || ""})
        verify(shell, "ShellWindow created")
        return shell
    }

    function showShell(shell) {
        shell.visible = true
        tryVerify(() => shell.layoutSettled, 5000, "the docks are laid out")
    }

    // A shell is destroyed before the next one is made: KDDockWidgets' names are process-wide.
    function dropShell(shell) {
        shell.destroy()
        waitDocksGone()
    }

    function shown(shell, id) {
        const content = shell.docks[id].content
        return content.visible && content.width > 0 && content.Window.window === shell
    }

    function origin(shell, id) {
        return shell.docks[id].content.mapToItem(null, 0, 0)
    }

    function checkDefaultLayout(shell) {
        for (const id of defaultPanels) {
            verify(shell.docks[id].isOpen, id + " is open")
            verify(!shell.docks[id].isFloating, id + " is docked")
        }
        verify(!shell.docks.backups.isOpen, "Backups stays closed")
        // In front of each tab group: Home, Jobs and Hardware.
        for (const id of ["home", "jobs", "hardware"])
            tryVerify(() => shown(shell, id), 5000, id + " is shown")
        for (const id of ["confirmations", "usage", "diagnostics", "settings"])
            verify(!shell.docks[id].content.visible, id + " is a tab behind")
        verify(origin(shell, "jobs").x > origin(shell, "home").x, "Jobs is right of Home")
        verify(origin(shell, "hardware").y > origin(shell, "home").y, "Hardware is below Home")
        // The Default layout's proportions (the owner's choice: ShellWindow.defaultRightShare and
        // defaultBottomShare, 28 % each), with the bottom row under Home only.
        // Each share is what Home's group leaves of the docking area, the separator included:
        // 28 % within 4 points, so a few pixels of bars don't matter and a 50/50 split fails.
        const area = shell.dockingArea
        const home = shell.docks.home.content
        const homeEnd = home.mapToItem(area, home.width, home.height)
        const right = (area.width - homeEnd.x) / area.width
        const bottom = (area.height - homeEnd.y) / area.height
        const share = 0.28   // the owner's choice, for both
        verify(Math.abs(right - share) <= 0.04,
               "the right column is " + (right * 100).toFixed(1) + " % of the width")
        verify(Math.abs(bottom - share) <= 0.04,
               "the bottom row is " + (bottom * 100).toFixed(1) + " % of the height")
        const jobsTop = shell.docks.jobs.content.mapToItem(area, 0, 0).y
        verify(jobsTop < 0.1 * area.height && jobsTop + shell.docks.jobs.content.height > 0.98 * area.height,
               "Jobs spans the full height: from " + jobsTop + " for " + shell.docks.jobs.content.height
               + " of " + area.height)
        verify(Math.abs(shell.docks.hardware.content.width - shell.docks.home.content.width) <= 2,
               "the bottom row is as wide as Home: " + shell.docks.hardware.content.width + " against "
               + shell.docks.home.content.width)
    }

    // A layout saved with Confirmations in front of Jobs, or with every panel closed.
    function savedLayout(dir, closeAll) {
        const shell = makeShell(dir)
        showShell(shell)
        shell.docks.confirmations.setAsCurrentTab()
        tryVerify(() => shown(shell, "confirmations"))
        if (closeAll) {
            for (const id of defaultPanels)
                shell.docks[id].forceClose()
            tryVerify(() => defaultPanels.every(id => !shell.docks[id].isOpen))
        }
        compare(shell.saveLayout(), "", "the layout is saved")
        const text = TestFiles.read(dir + "/layout.json")
        verify(text !== null && text !== undefined, "layout.json is written")
        dropShell(shell)
        return text
    }

    function test_default_layout_without_a_saved_one() { // spec: APP-004, ARCH-010
        const dir = TestFiles.newDir("none")
        const shell = makeShell(dir)
        compare(shell.layoutOpened, "default")
        showShell(shell)
        checkDefaultLayout(shell)
        verify(!shell.layoutBanner.visible, "no banner")
        compare(shell.saveLayout(), "", "the 60 s save")
        compare(TestFiles.list(dir), [], "nothing is written until the layout changes")
    }

    function test_restore_runs_in_order() { // spec: APP-003
        const dir = TestFiles.newDir("order")
        savedLayout(dir)
        DockLayout.clearSteps()
        const shell = makeShell(dir)
        compare(shell.layoutOpened, "restored")
        compare(DockLayout.steps(), ["windows_created", "factory_registered", "restore", "clamp"])
        const details = DockLayout.stepDetails()
        compare(details[0].main_windows, ["image-main"], "the main window existed")
        compare(details[2].ok, true, "KDDockWidgets restored the blob")
        verify(details[3].windows >= 1, "the clamp placed the main window")
        showShell(shell)
        tryVerify(() => shown(shell, "confirmations"), 5000, "the saved tab is in front")
        verify(!shell.docks.jobs.content.visible, "Jobs is behind it")
    }

    function test_a_lone_backup_is_never_read() { // spec: APP-002
        // layout.json.bak is only ReplaceFileW's backup: with no layout.json, the Default opens.
        const text = savedLayout(TestFiles.newDir("source"))
        const dir = TestFiles.newDir("backup")
        verify(TestFiles.write(dir + "/layout.json.bak", text))
        const shell = makeShell(dir)
        compare(shell.layoutOpened, "default")
        showShell(shell)
        checkDefaultLayout(shell)
        verify(!shell.layoutBanner.visible, "no banner")
    }

    function test_a_layout_with_every_panel_closed_opens_as_saved() { // spec: APP-002, APP-156
        const dir = TestFiles.newDir("closed")
        savedLayout(dir, true)
        const shell = makeShell(dir)
        compare(shell.layoutOpened, "restored")
        compare(TestFiles.list(dir).filter(f => f.startsWith("layout.json.bad-")), [], "nothing is set aside")
        showShell(shell)
        verify(!shell.layoutBanner.visible, "no banner")
        verify(defaultPanels.every(id => !shell.docks[id].isOpen), "every panel stays closed")
        tryVerify(() => shell.noPanelsHint.visible, 5000, "the hint shows")
        compare(shell.noPanelsHint.text, "No panels open. Ctrl+1 opens Home.")
        compare(LilShim.accessibleOf(shell.noPanelsHint).name, "No panels open. Ctrl+1 opens Home.")
        shell.showPanel("home")
        tryVerify(() => shown(shell, "home"), 5000, "Home opens")
        verify(!shell.noPanelsHint.visible, "and the hint goes")
    }

    function test_the_end_of_the_session_saves() { // spec: APP-002
        const dir = TestFiles.newDir("session")
        const shell = makeShell(dir)
        showShell(shell)
        shell.docks.usage.setAsCurrentTab()
        tryVerify(() => shown(shell, "usage"), 5000, "Usage is the tab in front")
        DockLayout.saveRequested()   // as at a logoff, a shutdown or the application's quit
        const saved = TestFiles.read(dir + "/layout.json")
        verify(saved !== null && saved !== undefined, "layout.json is written")
    }

    function test_a_layout_that_never_settles_is_saved_after_the_fallback() { // spec: APP-002
        const dir = TestFiles.newDir("unsettled")
        const shell = makeShell(dir)
        // Home's content keeps a 0 width, as a panel that never laid out would.
        const content = shell.docks.home.content
        const pin = () => { if (content.width !== 0) content.width = 0 }
        content.widthChanged.connect(pin)
        pin()
        ignoreWarning(/layout_settle_timeout: .*these docks have no size: home/)
        shell.visible = true
        tryVerify(() => shell._sized, 5000, "the window shows")
        verify(!shell.layoutSettled, "not settled while Home has no size")
        tryVerify(() => shell.layoutSettled, shell.settleTimeoutMs + 3000, "settled by the fallback")
        content.widthChanged.disconnect(pin)
        shell.docks.usage.setAsCurrentTab()
        tryVerify(() => shown(shell, "usage"), 5000, "Usage is the tab in front")
        compare(shell.saveLayout(), "", "the save")
        const saved = TestFiles.read(dir + "/layout.json")
        verify(saved !== null && saved !== undefined, "layout.json is written after the fallback")
        dropShell(shell)
    }

    function test_a_layout_never_shown_is_not_saved_and_the_log_says_so() { // spec: APP-002
        const dir = TestFiles.newDir("unshown")
        const shell = makeShell(dir)
        ignoreWarning(/layout_save_skipped/)
        DockLayout.saveRequested()   // a logoff before the window ever showed
        compare(TestFiles.list(dir), [], "nothing is written")
        dropShell(shell)
    }

    function test_bad_layout_falls_back_to_default() { // spec: APP-002, APP-004
        const good = JSON.parse(savedLayout(TestFiles.newDir("good")))
        const newer = Object.assign({}, good, {kddw: Object.assign({}, good.kddw, {serializationVersion: 999})})
        // A blob KDDockWidgets accepts that opens no main window, or none of the panels.
        const noWindow = Object.assign({}, good, {kddw: {serializationVersion: good.kddw.serializationVersion,
                                                         mainWindows: []}})
        // One that names only panels this build doesn't know.
        let unknown = JSON.stringify(good.kddw)
        for (const id of defaultPanels.concat(["backups"]))
            unknown = unknown.split('"' + id + '"').join('"gone-' + id + '"')
        const unknownOnly = Object.assign({}, good, {kddw: JSON.parse(unknown)})
        for (const [what, text] of [["damaged", '{"layout": 1, "windows": [{"name": '],
                                    ["empty", ""],
                                    ["refused", JSON.stringify(newer)],
                                    ["no main window", JSON.stringify(noWindow)],
                                    ["unknown panels only", JSON.stringify(unknownOnly)]]) {
            const dir = TestFiles.newDir(what)
            verify(TestFiles.write(dir + "/layout.json", text), what)
            const shell = makeShell(dir)
            compare(shell.layoutOpened, "default_after_bad", what)
            const files = TestFiles.list(dir)
            verify(files.indexOf("layout.json") < 0, what + ": the file is set aside: " + files)
            const aside = files.filter(f => f.startsWith("layout.json.bad-"))
            compare(aside.length, 1, what + ": " + files)
            compare(TestFiles.read(dir + "/" + aside[0]), text, what + ": kept as it was")
            showShell(shell)
            checkDefaultLayout(shell)
            verify(shell.layoutBanner.visible, what + ": the banner shows")
            verify(shell.layoutBanner.text.indexOf(aside[0]) >= 0, shell.layoutBanner.text)
            dropShell(shell)
        }
    }

    function test_a_maximized_round_trip_keeps_the_docked_widths() { // spec: APP-002, APP-003
        const dir = TestFiles.newDir("maximized")
        let shell = makeShell(dir)
        showShell(shell)
        // A normal size well inside the screen, so the maximized one differs from it.
        shell.width = 1000
        shell.height = 600
        tryVerify(() => shell.width === 1000 && shell.height === 600, 5000, "the normal size")
        waitLaidOut(shell)
        shell.showMaximized()
        tryVerify(() => shell.visibility === Window.Maximized, 5000, "maximized")
        tryVerify(() => shell.width > 1100, 5000, "the maximized window is larger: " + shell.width)
        waitLaidOut(shell)
        const sizes = id => ({w: shell.docks[id].content.width, h: shell.docks[id].content.height})
        const before = {home: sizes("home"), jobs: sizes("jobs"), hardware: sizes("hardware")}
        shell.close()
        const saved = JSON.parse(TestFiles.read(dir + "/layout.json"))
        compare(saved.windows[0].maximized, true, "saved maximized")
        compare(saved.windows[0].geometry.w, 1000, "with its normal width")
        compare(saved.windows[0].geometry.h, 600, "and its normal height")
        dropShell(shell)
        for (let round = 1; round <= 3; ++round) {
            shell = makeShell(dir)
            compare(shell.layoutOpened, "restored")
            shell.visible = true
            tryVerify(() => shell.visibility === Window.Maximized, 5000, "restored maximized")
            tryVerify(() => shell.layoutSettled, 5000, "the restored layout is laid out")
            waitLaidOut(shell)
            for (const id in before) {
                const now = sizes(id)
                verify(Math.abs(now.w - before[id].w) <= 4 && Math.abs(now.h - before[id].h) <= 4,
                       "round " + round + ": " + id + " was " + JSON.stringify(before[id])
                       + ", is " + JSON.stringify(now))
            }
            shell.docks.jobs.setAsCurrentTab()   // a change, so the close saves again
            shell.close()
            dropShell(shell)
        }
    }

    // KDDockWidgets' own drag and resize areas (its separators', and the drag areas its title
    // and tab bar bases lay under our bars) are pointer gestures for rearranging the docks;
    // M1's keyboard routes are the dock's Float and Close buttons, its tabs and the shortcut map.
    function kddwDragArea(item) {
        const p = item.parent
        return String(item).startsWith("QQuickMouseArea") && p
               && (p.kddwSeparator !== undefined || p.titleBarCpp !== undefined || p.tabBarCpp !== undefined)
    }

    function test_the_shell_chrome_passes_the_walk() { // spec: APP-156
        const shell = makeShell("")
        showShell(shell)
        checkDefaultLayout(shell)
        verify(walk.interactiveCount(shell.contentItem, kddwDragArea) >= 12,
               "the walk sees the title-bar buttons and the tabs: "
               + walk.interactiveCount(shell.contentItem, kddwDragArea))
        compare(walk.report(shell.contentItem, kddwDragArea), [])
    }

    function test_dock_buttons_work_from_the_keyboard() { // spec: APP-156
        const shell = makeShell("")
        showShell(shell)
        shell.requestActivate()
        tryVerify(() => shell.active, 5000)
        const close = findItem(shell.contentItem, item => String(item).startsWith("LIconButton")
                                                           && item.text === "Close Hardware")
        verify(close, "the Close Hardware button")
        close.forceActiveFocus(Qt.TabFocusReason)
        tryVerify(() => close.activeFocus, 2000)
        keyClick(Qt.Key_Space)
        tryVerify(() => !shell.docks.hardware.isOpen, 5000, "Space closes Hardware")
    }

    // The first item under `root` for which `test` holds.
    function findItem(root, test) {
        if (test(root))
            return root
        for (let i = 0; i < root.children.length; ++i) {
            const found = findItem(root.children[i], test)
            if (found)
                return found
        }
        return null
    }

    function test_float_and_redock_a_placeholder_keeps_its_state() { // spec: ARCH-116
        const shell = makeShell("")
        showShell(shell)
        const dock = shell.docks.home
        const panel = dock.content
        panel.note = "kept"
        dock.isFloating = true
        tryVerify(() => dock.isFloating && panel.Window.window && panel.Window.window !== shell, 5000,
                  "Home floats in its own window")
        tryVerify(() => panel.Window.window.visible, 5000)
        verify(dock.content === panel, "the same panel item")
        compare(panel.note, "kept")
        dock.isFloating = false
        tryVerify(() => panel.Window.window === shell, 5000, "Home docks back")
        verify(dock.content === panel, "still the same panel item")
        compare(panel.note, "kept")
    }

    function test_window_title_names_the_project() { // spec: APP-164
        const shell = makeShell("")
        compare(shell.title, "LILITH — sketchbook")
        fake.project = "storyboard"
        compare(shell.title, "LILITH — storyboard")
        fake.project = ""
        compare(shell.title, "LILITH")
        fake.project = "sketchbook"
    }

    function test_shortcuts_show_their_panels() { // spec: APP-011
        const shell = makeShell("")
        showShell(shell)
        shell.requestActivate()
        tryVerify(() => shell.active, 5000, "the shell is the active window")
        keySequence("Ctrl+3")
        tryVerify(() => shown(shell, "confirmations"), 5000, "Ctrl+3 shows Confirmations")
        verify(shell.docks.confirmations.panelHasFocus, "and focuses it")
        keySequence("Ctrl+,")
        tryVerify(() => shown(shell, "settings"), 5000, "Ctrl+, shows Settings")
        keySequence("Ctrl+1")
        keyClick(Qt.Key_Comma, Qt.ControlModifier)
        tryVerify(() => shell.docks.settings.panelHasFocus, 5000, "Ctrl and the comma key show Settings")
        keySequence("Ctrl+1")
        tryVerify(() => shell.docks.home.panelHasFocus, 5000, "Ctrl+1 focuses Home")
        keySequence("F6")
        tryVerify(() => shell.docks.jobs.panelHasFocus && shown(shell, "jobs"), 5000, "F6 moves to Jobs")
        keySequence("Shift+F6")
        tryVerify(() => shell.docks.home.panelHasFocus, 5000, "Shift+F6 moves back to Home")
        pauseSpy.target = shell
        keySequence("Ctrl+Shift+P")
        compare(pauseSpy.count, 1, "Ctrl+Shift+P asks to pause or resume GPU work")
    }
}
