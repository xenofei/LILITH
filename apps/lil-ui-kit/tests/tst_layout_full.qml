// The whole shell of plan 4a at once (APP-001, ARCH-116): the Default layout holds the seven real
// panels (Home, Jobs, Confirmations, Hardware, Usage & spend, Diagnostics, Settings), each its own
// panel and none a placeholder, and Backups opens from Diagnostics' button. Then, in one shell with
// every panel holding state of its own (and the first-start card offered, and Home's kept time
// written), each panel floats and docks back in turn: after every move every panel still has its
// state, the card and the kept time are untouched, and nothing is read again. Every read is answered
// as the real core answers it (ShellScene.js, over CoreShapes.js); the app state is a temporary folder.
import QtQuick
import QtTest
import Lilith.Core
import Lilith.Shim
import Lilith.UiKit
import "CoreShapes.js" as Shapes
import "ShellScene.js" as ShellScene
import "../qml/shell/Panels.js" as Panels

TestCase {
    id: tc
    name: "LayoutFull"
    when: windowShown
    width: 200
    height: 100

    Component {
        id: fakeComponent
        FakeCore {
            project: "sketchbook"
            workspace: ({id: "ws1", path: "D:/art/harbor"})
        }
    }

    // The setup bridge's fake (tst_first_start's): nothing real is written.
    Component {
        id: fakeSetupComponent
        QtObject {
            property bool busy: false
            property bool appMissing: false
            property string appPath
            property int runs: 0
            signal finished(string reportJson)
            function run() { return false }
        }
    }

    Component {
        id: shellComponent
        ShellWindow {
            shim: QtObject {
                function activate(window, takeFocus) {}
            }
        }
    }

    readonly property var realPanels: ({home: "HomePanel", jobs: "JobsPanel", confirmations: "ConfirmationsPanel",
                                        hardware: "HardwarePanel", usage: "UsagePanel",
                                        diagnostics: "DiagnosticsPanel", settings: "SettingsPanel",
                                        backups: "BackupsPanel"})
    readonly property var sevenPanels: ["home", "jobs", "confirmations", "hardware", "usage", "diagnostics", "settings"]
    readonly property var missingPath: ({toasts: "not_yet_shown", shortcut: "missing", handlers: "missing"})

    property var answered: ({})
    property var windows: []

    function init() {
        answered = {}
        tryVerify(() => {
            const kddw = JSON.parse(DockLayout.serialize())
            return DockLayout.mainWindows().length === 0
                   && !(Array.isArray(kddw.allDockWidgets) && kddw.allDockWidgets.length > 0)
        }, 5000, "the earlier shell's docks are gone")
    }

    // The shells go before their FakeCore, so their subscriptions end on a live core.
    function cleanup() {
        for (const w of windows)
            w.destroy()
        windows = []
        wait(0)
    }

    function makeShell(core, dir) {
        const shell = shellComponent.createObject(tc, {core: core, stateDir: dir,
                                                       setup: fakeSetupComponent.createObject(tc)})
        verify(shell, "ShellWindow created")
        windows = windows.concat([shell])
        shell.visible = true
        tryVerify(() => shell.layoutSettled, 5000, "the docks are laid out")
        return shell
    }

    // Every text under `item`, shown or behind a tab.
    function allTexts(item) {
        const out = []
        const visit = i => {
            if (typeof i.text === "string" && typeof i.lineCount === "number")
                out.push(i.text)
            for (let n = 0; n < i.children.length; ++n)
                visit(i.children[n])
        }
        visit(item)
        return out
    }

    function holds(item, text) {
        return allTexts(item).some(t => t.indexOf(text) >= 0)
    }

    function find(item, pred) {
        if (pred(item))
            return item
        for (let n = 0; n < item.children.length; ++n) {
            const found = find(item.children[n], pred)
            if (found)
                return found
        }
        return null
    }

    function toggleNamed(root, name) {
        return find(root, i => typeof i.checked === "boolean" && i.text === name)
    }

    function count(core, name) {
        return core.calls.filter(c => c.name === name).length
    }

    function keptTime(dir) {
        const text = TestFiles.read(dir + "/state.json")
        if (text === null || text === undefined)
            return null
        const row = (JSON.parse(text).workspaces || {}).ws1
        return row ? row.last_seen_at : null
    }

    // Brings a dock's panel to the front of its tab group and answers what it asks.
    function front(shell, core, s, id) {
        const dock = shell.docks[id]
        dock.setAsCurrentTab()
        tryVerify(() => {
            ShellScene.answerOpen(core, answered, s)
            return dock.content.visible && dock.content.width > 0
        }, 5000, id + " is in front")
    }

    function test_default_layout_has_every_panel_of_this_plan() { // spec: APP-001, ARCH-010, ARCH-116
        const core = fakeComponent.createObject(tc)
        const shell = makeShell(core, "")
        const s = ShellScene.scene()
        compare(Panels.all.map(p => p.id), sevenPanels.concat(["backups"]), "the plan's panels, in F6 order")
        for (const id of sevenPanels) {
            const dock = shell.docks[id]
            verify(dock.isOpen, id + " is open")
            verify(!dock.isFloating, id + " is docked")
            verify(String(dock.content).startsWith(realPanels[id]), id + " is its own panel: " + dock.content)
            verify(!String(dock.content).startsWith("PlaceholderPanel"), id + " is no placeholder")
            compare(dock.content.title, Panels.byId(id).title)
        }
        verify(!shell.docks.backups.isOpen, "Backups stays closed")
        // Each panel, brought to the front, shows itself with its reads answered.
        for (const id of sevenPanels)
            front(shell, core, s, id)
        // Backups opens from Diagnostics' own button.
        front(shell, core, s, "diagnostics")
        const open = find(shell.docks.diagnostics.content,
                          i => typeof i.clicked === "function" && i.text === "Open Backups" && i.visible)
        verify(open, "Diagnostics' Open Backups button")
        verify(open.enabled, "it can open Backups in this shell")
        open.clicked()
        tryVerify(() => shell.docks.backups.isOpen && shell.docks.backups.content.visible
                        && shell.docks.backups.content.width > 0, 5000, "Backups opens")
        verify(String(shell.docks.backups.content).startsWith("BackupsPanel"), String(shell.docks.backups.content))
        compare(shell.docks.backups.content.title, "Backups")
        for (const id of sevenPanels)
            verify(shell.docks[id].isOpen, id + " stays open")
    }

    function test_float_and_redock_every_panel_keeps_its_state() { // spec: ARCH-116, APP-001, APP-022, APP-111
        const dir = TestFiles.newDir("full")
        const core = fakeComponent.createObject(tc)
        const shell = makeShell(core, dir)
        verify(shell.firstStartCard.firstStart, "an empty state folder is a first start")
        shell.requestActivate()
        tryVerify(() => shell.active, 5000, "the shell is the active window")
        const failedOff = Shapes.desktopSettings({notification_path: missingPath})
        failedOff.notifications.job_failed = false
        const monthKey = new Date().toISOString().slice(0, 7)
        const s = ShellScene.scene({
            jobs: [Shapes.job("j_00000000000000a1", "running"),
                   Shapes.job("j_00000000000000a2", "queued", {queue_position: 0})],
            lanes: [Shapes.lane({paused: true, reasons: ["manual"]}), Shapes.lane({lane: "cpu"})],
            pending: [Shapes.confirmation("cf_00000000000000b1", "comfy.run", "2026-10-30T13:00:01Z")],
            desktop: failedOff,
            usageRows: args => args.period === "month" && JSON.stringify(args.group_by) === '["model"]'
                               ? [ShellScene.usageRow(monthKey, {model: "flux-dev"})] : []
        })

        // Each panel's state, set through its own controls.
        for (const id of sevenPanels)
            front(shell, core, s, id)
        const p = id => shell.docks[id].content
        // Home's first run: its time starts once it has been in the active window for 3 s.
        front(shell, core, s, "home")
        tryVerify(() => {
            ShellScene.answerOpen(core, answered, s)
            return keptTime(dir) !== null
        }, 8000, "Home writes its kept time")
        const kept = keptTime(dir)
        tryVerify(() => p("home").projectsCard.rows.count === 2, 5000, "Home's projects")
        // The first-start card is offered.
        tryVerify(() => shell.firstStartCard.visible, 5000, "the first-start card shows")
        const cardText = shell.firstStartCard.text
        // Jobs: a running and a queued job, the queued one chosen, the lane paused by you.
        tryVerify(() => p("jobs").model.loaded && p("jobs").liveView.count === 2, 5000, "Jobs' two jobs")
        p("jobs").liveView.currentIndex = 1
        tryVerify(() => p("jobs").laneHeader("gpu-heavy")
                        && p("jobs").laneHeader("gpu-heavy").text.indexOf("Paused by you") >= 0, 5000)
        // Confirmations: one waiting, and the history tab chosen.
        tryVerify(() => p("confirmations").cardRepeater.count === 1, 5000, "a confirmation's card")
        p("confirmations").tab = "history"
        // Hardware: the 24-hour window.
        p("hardware").windowS = 24 * 3600
        tryVerify(() => {
            ShellScene.answerOpen(core, answered, s)
            return holds(find(p("hardware"), i => i.objectName === "gpuCard"), "63 %")
        }, 5000, "Hardware's GPU card")
        // Usage & spend: by model, on the day tab.
        p("usage").setDimension("model")
        p("usage").setTab("day")
        tryVerify(() => {
            ShellScene.answerOpen(core, answered, s)
            return p("usage").month.shown("money").length === 1
        }, 5000, "Usage's month by model")
        // Diagnostics: the engine's line, and the event log. It follows its topics only while shown.
        front(shell, core, s, "diagnostics")
        tryVerify(() => p("diagnostics").shown, 5000, "Diagnostics is shown")
        // The core's state moves with its event, so a later read answers the same.
        s.engines = [ShellScene.comfy({version: "0.42.2"})]
        core.emit("engines", "event", {kind: "status", engine: s.engines[0]})
        tryVerify(() => {
            ShellScene.answerOpen(core, answered, s)
            return holds(p("diagnostics").engines, "ComfyUI 0.42.2 · owned · ready")
        }, 5000, "Diagnostics' engine: " + JSON.stringify(allTexts(p("diagnostics").engines)))
        const logged = p("diagnostics").log.view.count
        verify(logged >= 1, "the event log has the event")
        // Settings: "Job failed" off, as the core said.
        tryVerify(() => p("settings").fresh, 5000, "Settings has read")
        const failedToggle = toggleNamed(p("settings"), "Job failed")
        verify(failedToggle, "the Job failed switch")
        compare(failedToggle.checked, false)
        // Backups, opened from Diagnostics.
        // Backups, opened from Diagnostics: it has no place in the Default layout, so it opens in a
        // window of its own; docked as a tab beside Diagnostics, as a person would, it moves like the rest.
        p("diagnostics").shell.openPanelOn("backups", "", "", false)
        tryVerify(() => shell.docks.backups.isOpen && shell.docks.backups.isFloating, 5000, "Backups opens in its own window")
        shell.docks.diagnostics.addDockWidgetAsTab(shell.docks.backups)
        tryVerify(() => !shell.docks.backups.isFloating && p("backups").Window.window === shell, 5000, "Backups docks")
        wait(200)
        ShellScene.answerOpen(core, answered, s)

        const items = {}
        for (const id of sevenPanels.concat(["backups"]))
            items[id] = p(id)
        const steady = ["projects.list", "assets.query", "project.status", "app/confirmations.list", "usage.summary",
                        "desktop.get", "telemetry.series", "telemetry.snapshot", "engines.status", "diagnostics.recent",
                        "models.report"]
        let reads = {}
        const countReads = () => {
            const out = {}
            for (const name of steady)
                out[name] = count(core, name)
            return out
        }

        const checkAll = when => {
            for (const id in items)
                verify(shell.docks[id].content === items[id], when + ": " + id + " is the same panel item")
            compare(p("home").projectsCard.rows.count, 2, when + ": Home's projects")
            compare(p("home").lastSeen, "", when + ": Home's first run has no card")
            compare(keptTime(dir), kept, when + ": Home's kept time")
            verify(shell.firstStartCard.visible, when + ": the first-start card")
            compare(shell.firstStartCard.text, cardText, when + ": the card's words")
            compare(p("jobs").liveView.count, 2, when + ": Jobs' rows")
            compare(p("jobs").liveView.currentIndex, 1, when + ": Jobs' chosen row")
            verify(p("jobs").laneHeader("gpu-heavy").text.indexOf("Paused by you") >= 0, when + ": the lane's pause")
            compare(p("confirmations").cardRepeater.count, 1, when + ": the confirmation")
            compare(p("confirmations").tab, "history", when + ": Confirmations' tab")
            compare(p("hardware").windowS, 24 * 3600, when + ": Hardware's window")
            verify(holds(find(p("hardware"), i => i.objectName === "gpuCard"), "63 %"), when + ": Hardware's GPU")
            compare(p("usage").dimension, "model", when + ": Usage's dimension")
            compare(p("usage").tab, "day", when + ": Usage's tab")
            compare(p("usage").month.shown("money")[0].label, "flux-dev", when + ": Usage's row")
            verify(holds(p("diagnostics").engines, "ComfyUI 0.42.2 · owned · ready"), when + ": Diagnostics' engine, from its event")
            compare(p("diagnostics").log.view.count, logged, when + ": the event log")
            verify(toggleNamed(p("settings"), "Job failed") === failedToggle, when + ": the same switch")
            compare(failedToggle.checked, false, when + ": Job failed stays off")
            verify(shell.docks.backups.isOpen, when + ": Backups stays open")
            for (const name of steady)
                compare(count(core, name), reads[name], when + ": " + name + " isn't read again")
        }
        reads = countReads()
        checkAll("before")

        for (const id of sevenPanels.concat(["backups"])) {
            const dock = shell.docks[id]
            const panel = items[id]
            // In front first: a panel that reads each time it is shown (Diagnostics) reads now, before
            // the move, and that read is answered.
            front(shell, core, s, id)
            wait(300)
            ShellScene.answerOpen(core, answered, s)
            reads = countReads()
            dock.isFloating = true
            tryVerify(() => dock.isFloating && panel.Window.window && panel.Window.window !== shell
                            && panel.Window.window.visible, 5000, id + " floats in its own window")
            checkAll(id + " floating")
            dock.isFloating = false
            tryVerify(() => !dock.isFloating && panel.Window.window === shell, 5000, id + " docks back")
            wait(50)
            checkAll(id + " docked again")
        }
    }
}
