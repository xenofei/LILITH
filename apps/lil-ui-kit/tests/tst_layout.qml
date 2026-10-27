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

    SignalSpy {
        id: pauseSpy
        signalName: "gpuPauseToggleRequested"
    }

    readonly property var defaultPanels: ["home", "jobs", "confirmations", "hardware", "usage",
                                          "diagnostics", "settings"]

    // The shell of the test before deletes its docks later (KDDockWidgets' names are
    // process-wide): let that run first.
    function init() {
        wait(100)
    }

    function makeShell(dir) {
        const shell = createTemporaryObject(shellComponent, tc, {stateDir: dir || ""})
        verify(shell, "ShellWindow created")
        return shell
    }

    function showShell(shell) {
        shell.visible = true
        tryVerify(() => shell.docks.home.content && shell.docks.home.content.width > 0, 5000,
                  "the docks are laid out")
        wait(50)
    }

    // A shell is destroyed before the next one is made: KDDockWidgets' names are process-wide.
    function dropShell(shell) {
        shell.destroy()
        wait(100)
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
    }

    // A layout saved with Confirmations in front of Jobs.
    function savedLayout(dir) {
        const shell = makeShell(dir)
        showShell(shell)
        shell.docks.confirmations.setAsCurrentTab()
        tryVerify(() => shown(shell, "confirmations"))
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
        compare(TestFiles.list(dir), [], "nothing is written until the layout changes")
    }

    function test_restore_runs_in_order() { // spec: APP-003
        const dir = TestFiles.newDir("order")
        savedLayout(dir)
        DockLayout.clearSteps()
        const shell = makeShell(dir)
        compare(shell.layoutOpened, "restored")
        compare(DockLayout.steps(), ["windows_created", "factory_registered", "restore", "clamp"])
        showShell(shell)
        tryVerify(() => shown(shell, "confirmations"), 5000, "the saved tab is in front")
        verify(!shell.docks.jobs.content.visible, "Jobs is behind it")
    }

    function test_bad_layout_falls_back_to_default() { // spec: APP-002, APP-004
        const good = JSON.parse(savedLayout(TestFiles.newDir("good")))
        const newer = Object.assign({}, good, {kddw: Object.assign({}, good.kddw, {serializationVersion: 999})})
        for (const [what, text] of [["damaged", '{"layout": 1, "windows": [{"name": '],
                                    ["empty", ""],
                                    ["refused", JSON.stringify(newer)]]) {
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
        shell.showMaximized()
        tryVerify(() => shell.visibility === Window.Maximized, 5000, "maximized")
        wait(100)
        const sizes = id => ({w: shell.docks[id].content.width, h: shell.docks[id].content.height})
        const before = {home: sizes("home"), jobs: sizes("jobs"), hardware: sizes("hardware")}
        shell.close()
        verify(TestFiles.read(dir + "/layout.json") !== null, "the layout is saved on close")
        dropShell(shell)
        for (let round = 1; round <= 3; ++round) {
            shell = makeShell(dir)
            compare(shell.layoutOpened, "restored")
            shell.visible = true
            tryVerify(() => shell.visibility === Window.Maximized, 5000, "restored maximized")
            tryVerify(() => shell.docks.home.content.width > 0, 5000)
            wait(100)
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
