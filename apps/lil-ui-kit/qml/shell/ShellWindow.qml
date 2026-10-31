// The app's main window: a KDDockWidgets main window (DockingArea) holding one dock per panel of
// Panels.js (APP-001, ARCH-010). It starts hidden; main.cpp shows it through lilshim::activate,
// with or without the focus.
//
// Top (APP-006, APP-016, APP-034): the top bar (TopBar) and the connection and no-workspace banners
// stack above the shell's own banners and the docks. `eventLog` runs from the shell's start, so
// Diagnostics shows the events from before it was opened.
//
// Layout (APP-002 to APP-004, P4a-R16, P4a-R33): when it is made it restores the last layout from
// `stateDir` in APP-003's order (DockLayout: the windows exist, the dock factory is registered,
// KDDockWidgets restores, every window is clamped to a screen), or opens the Default layout when
// there is none, or when the file doesn't parse or KDDockWidgets refuses it: then the file is set
// aside and a banner says so. The layout is saved on close and every 60 s while it changes. With
// no `stateDir` (the QML tests that don't give one) nothing is read or written.
//
// Keys (APP-011): the plan 4a shortcut map (ShellState.shortcuts), all global: one per panel,
// F6 and Shift+F6 to the next and previous panel, and Ctrl+Shift+P, which emits
// gpuPauseToggleRequested() for the top bar's pause control. Closing a panel announces
// the key that opens it again (Accessible.announce, a UI Automation notification).
//
// Open requests (APP-014, APP-020, APP-065, P4a-R21; the block marked "Open requests" below):
// a launch (handleLaunchJson, from main.cpp: this start's own, then each one a second launch
// forwards) and each `open_requests:image` event. A target, raw ref or `.lilref` shows in the
// Opened card, and a refused link's message there; `task` opens Jobs on that job (`shownJob`, and
// the panel's showJob(id) when it has one); `confirm` opens Confirmations on that confirmation
// (`shownConfirmation`, and the panel's showConfirmation(id)). The window comes forward through
// `shim.activate(window, focus)` (LilShim; the tests pass a fake): an agent's request, or a launch
// with --no-activate, only flashes the taskbar button.
import QtQuick
import com.kdab.dockwidgets 2.0 as KDDW
import Lilith.Core
import Lilith.Shim
import Lilith.UiKit
import "Panels.js" as Panels

Window {
    id: root

    // The core (Core, or the tests' FakeCore).
    required property var core
    // The app's state folder (ShellState's `dir`); empty: no layout is read or saved.
    property string stateDir

    property alias dockingArea: dockingArea
    property alias layoutBanner: layoutBanner
    property alias noPanelsHint: noPanelsHint
    // How many of the panels are open (the hint shows at none).
    property int openCount: 0
    // Panel id -> its DockedPanel.
    readonly property var docks: ({})
    // How the layout opened: "restored", "default", "default_after_bad" (a file was set aside) or
    // "default_unreadable" (the file couldn't be read: it is left alone and not saved over).
    property string layoutOpened
    // The Default layout's proportions (the owner's choice): the right column's share of the
    // width, at the full height, and the bottom row's share of the height, under Home only.
    readonly property real defaultRightShare: 0.28
    readonly property real defaultBottomShare: 0.28
    // The plan 4a shortcut map: [{keys, action, scope}].
    readonly property var bindings: JSON.parse(ShellState.shortcuts())
    property string _lastSaved
    // The layout as built and laid out: set once every shown dock has its size and the layout's
    // text has stopped changing, which is when `_lastSaved` is taken. No save before then. If
    // that hasn't happened `settleTimeoutMs` after the window showed (or the layout was
    // restored), it is set anyway, with a warning in the log naming the check that never passed.
    property bool layoutSettled: false
    readonly property int settleTimeoutMs: 3000
    property string _settleCandidate
    property real _settleStartMs: 0
    // True while the shell itself opens or closes docks (the restore, the Default layout, the
    // window's close), which aren't announced.
    property bool _settling: true
    // The Default layout waits for the docking area's real size (it is 0 x 0 until the window
    // shows), or KDDockWidgets ignores the sizes asked for.
    property bool _defaultPending: false
    property bool _sized: false
    // No saves: the layout file couldn't be read, or the window is closing.
    property bool _saveBlocked: false
    property bool _closing: false
    // The panels closed in this turn of the event loop, announced together.
    property var _closedBatch: []

    signal gpuPauseToggleRequested()

    property alias topBar: topBar
    property alias connectionBanner: connectionBanner
    property alias noWorkspaceBanner: noWorkspaceBanner
    property EventLogModel eventLog: EventLogModel {
        core: root.core
    }

    onGpuPauseToggleRequested: topBar.togglePause()

    // --- Open requests (Task 19) ---
    // The shim the window is activated through: LilShim, or a test's fake.
    property var shim: LilShim
    property alias openedCard: openedCard
    property alias instanceBanner: instanceBanner
    // The job a `task` link last opened Jobs on, and the confirmation a `confirm` link last
    // opened Confirmations on.
    property string shownJob
    property string shownConfirmation
    property var _openRequests: null
    // --- end of open requests ---

    width: 1280
    height: 800
    visible: false
    // The project calls would really use: the chosen one, else what the core says it falls back to.
    readonly property string _titleProject: core && core.project ? core.project
                                            : topBar.projectSwitcher.effectiveProject
    title: _titleProject !== "" ? "LILITH — " + _titleProject : "LILITH"
    color: Theme.bg

    Column {
        id: shellTop
        width: parent.width

        TopBar {
            id: topBar
            width: parent.width
            core: root.core
        }

        ConnectionBanner {
            id: connectionBanner
            width: parent.width
            core: root.core
        }

        NoWorkspaceBanner {
            id: noWorkspaceBanner
            width: parent.width
            core: root.core
            onNoticed: (text, kind) => topBar.showNotice(text, kind)
        }
    }

    LBanner {
        id: layoutBanner
        anchors.top: shellTop.bottom
        width: parent.width
        kind: "warning"
    }

    // --- Open requests (Task 19): while another program holds the launch channel ---
    LBanner {
        id: instanceBanner
        anchors.top: layoutBanner.visible ? layoutBanner.bottom : shellTop.bottom
        width: parent.width
        kind: "warning"
    }
    // --- end of open requests ---

    KDDW.DockingArea {
        id: dockingArea
        anchors.top: instanceBanner.visible ? instanceBanner.bottom
                   : layoutBanner.visible ? layoutBanner.bottom : shellTop.bottom
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        uniqueName: "image-main"
        onWidthChanged: root.areaSized()
        onHeightChanged: root.areaSized()
    }

    // A layout saved with every panel closed opens that way (a valid layout): the centre says how
    // to bring one back.
    Text {
        id: noPanelsHint
        anchors.centerIn: dockingArea
        visible: root._sized && root.openCount === 0 && !root._defaultPending
        text: "No panels open. " + root.keysFor("panel.home") + " opens Home."
        textFormat: Text.PlainText
        color: Theme.textMuted
        font.pixelSize: Theme.fontSize + 1
        Accessible.role: Accessible.StaticText
        Accessible.name: text
    }

    // --- Open requests (Task 19): the Opened card, until Task 26 moves it into Home ---
    OpenedCard {
        id: openedCard
        anchors.right: dockingArea.right
        anchors.bottom: dockingArea.bottom
        anchors.margins: 2 * Theme.spacing
        width: Math.min(420, dockingArea.width - 4 * Theme.spacing)
        height: implicitHeight
        z: 10
    }
    // --- end of open requests ---

    Component {
        id: dockComponent
        DockedPanel {}
    }

    Instantiator {
        model: root.bindings
        delegate: Shortcut {
            required property var modelData
            sequence: modelData.keys
            context: Qt.ApplicationShortcut
            onActivated: root.runAction(modelData.action)
        }
    }

    Timer {
        // P4a-R33: saved every 60 s while it changes.
        interval: 60000
        repeat: true
        running: root.stateDir !== ""
        onTriggered: root.saveLayout()
    }

    // Saved on close before KDDockWidgets closes the docks (DockLayout's application filter),
    // then no more: the docks KDDockWidgets closes next aren't the layout. A close that is
    // vetoed leaves the window visible, and the shell goes on as before. Saved too when the
    // session ends or the application quits.
    Connections {
        target: DockLayout
        function onMainWindowClosing(name) {
            if (name !== dockingArea.uniqueName)
                return
            root._settling = true
            root.saveLayout()
            root._closing = true
            Qt.callLater(() => {
                if (root.visible) {
                    root._closing = false
                    root._settling = false
                }
            })
        }
        function onSaveRequested() {
            root.saveLayout()
        }
    }

    // The layout banner shows with the window and is announced a second later: Qt starts its
    // accessibility only when a screen reader first asks the new window for it, so an
    // announcement made as the window first shows can be lost (Step 2b found it was).
    onVisibleChanged: {
        if (visible && layoutBanner.text !== "" && !layoutBanner.visible) {
            layoutBanner.show(undefined, false)
            bannerAnnouncement.start()
        }
    }

    Timer {
        id: bannerAnnouncement
        interval: 1000
        onTriggered: {
            if (layoutBanner.visible)
                layoutBanner.announce()
        }
    }

    Component.onCompleted: {
        LilShim.applyPalette(Theme.palette)
        for (const panel of Panels.all)
            createDock(panel.id)
        restoreLayout()
        _settling = false
        if (core)
            _openRequests = CoreCall.subscribe(core, "open_requests:image", root, onOpenRequest)
    }

    // KDDockWidgets keeps a dock (and its unique name) after its QML item goes, so the shell
    // deletes its docks with it: a later shell in the same process (the QML tests) can reuse the
    // names.
    Component.onDestruction: {
        // While the core (a test's own FakeCore child among them) is still there.
        if (_openRequests)
            _openRequests.unsubscribe()
        for (const id in docks) {
            if (docks[id])
                docks[id].deleteDockWidgetLater()
        }
    }

    function createDock(id) {
        if (docks[id])
            return docks[id]
        const panel = Panels.byId(id)
        if (!panel)
            return null
        const dock = dockComponent.createObject(root, {panel: panel, core: root.core})
        docks[id] = dock
        dock.isOpenChanged.connect(() => root.panelOpenChanged(dock))
        return dock
    }

    function restoreLayout() {
        DockLayout.windowsCreated()
        DockLayout.registerDockFactory(name => root.createDock(name))
        const saved = stateDir !== "" ? JSON.parse(ShellState.loadLayout(stateDir)) : {status: "missing"}
        let restored = false
        let setAside = saved.status === "bad" ? saved.setAside : ""
        _saveBlocked = saved.status === "unreadable"
        if (saved.status === "ok") {
            restored = DockLayout.restore(JSON.stringify(saved.layout.kddw))
            // A blob KDDockWidgets accepts but that has no main window of this shell, or that
            // names only panels this build doesn't know, counts as refused too. One with every
            // panel closed is a layout like any other.
            if (restored) {
                const windows = saved.layout.kddw.mainWindows || []
                const blob = JSON.stringify(saved.layout.kddw)
                restored = windows.some(m => m.uniqueName === dockingArea.uniqueName)
                           && Panels.all.some(p => blob.indexOf('"' + p.id + '"') >= 0)
            }
            if (!restored)
                setAside = ShellState.setLayoutAside(stateDir)
        }
        _defaultPending = !restored
        DockLayout.clamp(restored ? saved.layout.windows : [],
                         (geometry, screen, screens, margins) => JSON.parse(
                             ShellState.clamp(JSON.stringify(geometry), screen, JSON.stringify(screens),
                                              JSON.stringify(margins))))
        layoutOpened = restored ? "restored"
                     : saved.status === "missing" ? "default"
                     : saved.status === "unreadable" ? "default_unreadable" : "default_after_bad"
        if (layoutOpened === "default_after_bad") {
            layoutBanner.text = "The saved layout couldn't be opened, so the Default layout is open."
                + (setAside ? " The old file was kept as " + setAside + "." : "")
            if (visible)
                layoutBanner.show()
        }
        areaSized()
    }

    // Once the docking area has its real size: the Default layout when it is waiting, and the
    // layout as it then stands taken as the one last saved, so an unchanged layout isn't written.
    function areaSized() {
        if (_sized || dockingArea.width <= 0 || dockingArea.height <= 0)
            return
        _sized = true
        if (_defaultPending) {
            const settling = _settling
            _settling = true
            applyDefaultLayout()
            _settling = settling
            _defaultPending = false
        }
        _settleStartMs = Date.now()
        settleCheck.start()
    }

    // The Default layout (APP-004): Home in the centre; Jobs and Confirmations tabbed on the
    // right, at the full height; Hardware, Usage & spend, Diagnostics and Settings tabbed at the
    // bottom, under Home. Panels with no area stay closed.
    function applyDefaultLayout() {
        for (const id in docks) {
            if (docks[id].isOpen)
                docks[id].forceClose()
        }
        const first = {}
        for (const panel of Panels.all) {
            const dock = docks[panel.id]
            if (panel.area === "")
                continue
            if (first[panel.area]) {
                first[panel.area].addDockWidgetAsTab(dock)
                continue
            }
            first[panel.area] = dock
            const right = Math.round(dockingArea.width * defaultRightShare)
            const bottom = Math.round(dockingArea.height * defaultBottomShare)
            if (panel.area === "centre")
                dockingArea.addDockWidget(dock, KDDW.KDDockWidgets.Location_OnLeft)
            else if (panel.area === "right")
                dockingArea.addDockWidget(dock, KDDW.KDDockWidgets.Location_OnRight, null, Qt.size(right, 0))
            else
                dockingArea.addDockWidget(dock, KDDW.KDDockWidgets.Location_OnBottom, first.centre, Qt.size(0, bottom))
        }
        for (const area in first)
            first[area].setAsCurrentTab()
    }

    // The layout file's text now: {layout: 1, windows, kddw}.
    function layoutText() {
        return JSON.stringify({layout: 1, windows: DockLayout.mainWindows(),
                               kddw: JSON.parse(DockLayout.serialize())})
    }

    // The open docks that show but have no size yet.
    function unsizedDocks() {
        const out = []
        for (const id in docks) {
            const content = docks[id] ? docks[id].content : null
            if (docks[id] && docks[id].isOpen && content && content.visible
                    && (content.width <= 0 || content.height <= 0))
                out.push(id)
        }
        return out
    }

    // Every open dock that shows has its size.
    function docksSized() {
        return unsizedDocks().length === 0
    }

    // Takes the layout last saved once it has settled: two checks in a row that find the docks
    // sized and the same layout text; or, past `settleTimeoutMs`, as it then stands, with a
    // warning naming what never passed.
    Timer {
        id: settleCheck
        interval: 50
        repeat: true
        onTriggered: {
            const unsized = root.unsizedDocks()
            const text = unsized.length === 0 ? root.layoutText() : ""
            if (unsized.length === 0 && text === root._settleCandidate) {
                root._lastSaved = text
                root.layoutSettled = true
                stop()
                return
            }
            root._settleCandidate = text
            if (Date.now() - root._settleStartMs < root.settleTimeoutMs)
                return
            console.warn("layout_settle_timeout: the layout didn't settle in " + root.settleTimeoutMs
                         + " ms (" + (unsized.length > 0 ? "these docks have no size: " + unsized.join(", ")
                                                         : "its text kept changing")
                         + "); it is taken as it stands, and saving resumes")
            root._lastSaved = root.layoutText()
            root.layoutSettled = true
            stop()
        }
    }

    // Writes the layout when it changed since it was read or last written. Empty, or why not.
    // A layout that was never shown and laid out isn't saved, and the log says so.
    function saveLayout() {
        if (stateDir === "" || _saveBlocked || _closing)
            return ""
        if (_defaultPending || !layoutSettled) {
            console.warn("layout_save_skipped: the layout hasn't been shown and laid out yet, so it isn't saved")
            return ""
        }
        const text = layoutText()
        if (text === _lastSaved)
            return ""
        const why = ShellState.saveLayout(stateDir, text)
        if (why === "")
            _lastSaved = text
        return why
    }

    // A panel you close says how to bring it back (APP-156): "Home closed. Ctrl+1 opens it
    // again." Panels closed together (a floating window with several tabs) are one message:
    // "Jobs and Confirmations closed. Ctrl+2 and Ctrl+3 open them again."
    function panelOpenChanged(dock) {
        openCount = openPanels().length
        if (_settling || dock.isOpen)
            return
        if (_closedBatch.length === 0)
            Qt.callLater(announceClosed)
        _closedBatch = _closedBatch.concat([dock])
    }

    function announceClosed() {
        const closed = _closedBatch
        _closedBatch = []
        if (closed.length === 0)
            return
        const list = a => a.length < 2 ? a.join("") : a.slice(0, -1).join(", ") + " and " + a[a.length - 1]
        const keyOf = d => {
            const b = bindings.find(b => b.action === d.panel.action)
            return b ? b.keys : ""
        }
        let text = list(closed.map(d => d.panel.title)) + " closed."
        if (closed.every(d => keyOf(d) !== ""))
            text += " " + list(closed.map(keyOf)) + (closed.length === 1 ? " opens it again." : " open them again.")
        else
            for (const d of closed.filter(d => keyOf(d) !== ""))
                text += " " + keyOf(d) + " opens " + d.panel.title + " again."
        dockingArea.Accessible.announce(text)
    }

    // The keys of `action` in the shortcut map, or "" when it has none.
    function keysFor(action) {
        const b = bindings.find(b => b.action === action)
        return b ? b.keys : ""
    }

    function showPanel(id) {
        const dock = docks[id]
        if (!dock)
            return
        if (!dock.isOpen)
            dock.open()
        dock.raise()
        dock.focusPanel()
        // A panel in a floating window: that window takes the keys.
        const win = dock.content ? dock.content.Window.window : null
        if (win && win !== root)
            win.requestActivate()
    }

    // The open panels in Panels.js order.
    function openPanels() {
        return Panels.all.map(p => docks[p.id]).filter(d => d && d.isOpen)
    }

    // The index in `open` of the panel with the keyboard focus, or -1.
    function focusedPanelIndex(open) {
        for (let i = 0; i < open.length; ++i) {
            if (open[i].panelHasFocus)
                return i
        }
        return -1
    }

    function cyclePanel(step) {
        const open = openPanels()
        if (open.length === 0)
            return
        const at = focusedPanelIndex(open)
        const next = at < 0 ? (step > 0 ? 0 : open.length - 1) : (at + step + open.length) % open.length
        showPanel(open[next].uniqueName)
    }

    function runAction(action) {
        if (action === "panel.next")
            cyclePanel(1)
        else if (action === "panel.previous")
            cyclePanel(-1)
        else if (action === "gpu.toggle_pause")
            gpuPauseToggleRequested()
        else {
            const panel = Panels.byAction(action)
            if (panel)
                showPanel(panel.id)
        }
    }

    // --- Open requests (Task 19) ---
    function handleLaunchJson(json) {
        handleLaunch(JSON.parse(json))
    }

    // A launch: {kind, intent, focus, activate, ...} (lil_qt::args::launch_json).
    function handleLaunch(launch) {
        const focus = launch.focus !== false
        // This start runs without the app's pipe: the bridge's words for why.
        if (typeof launch.pipe_notice === "string" && launch.pipe_notice !== "")
            instanceBanner.show(launch.pipe_notice)
        if (launch.activate !== false)
            shim.activate(root, focus)
        if (launch.kind === "target" || launch.kind === "ref" || launch.kind === "lilref") {
            openedCard.show(launch)
        } else if (launch.kind === "refused") {
            openedCard.showRefused(launch.message || "")
        } else if (launch.kind === "task") {
            shownJob = launch.job
            openPanelOn("jobs", "showJob", launch.job, focus)
        } else if (launch.kind === "confirm") {
            shownConfirmation = launch.id
            openPanelOn("confirmations", "showConfirmation", launch.id, focus)
        } else if (launch.kind === "confirm_list") {
            openPanelOn("confirmations", "", "", focus)
        }
    }

    // An `open_requests:image` event: {target, intent, focus} (APP-063, APP-065).
    function onOpenRequest(kind, seq, data) {
        if (kind !== "event" || !data || typeof data.target !== "string")
            return
        handleLaunch({kind: "target", target: data.target, intent: data.intent,
                      focus: data.focus === true, activate: true})
    }

    // Opens panel `id`, and calls its `method(arg, focus)` when the panel has one; the keyboard
    // focus moves into it only with `focus`.
    function openPanelOn(id, method, arg, focus) {
        const dock = docks[id]
        if (!dock)
            return
        if (focus) {
            showPanel(id)
        } else {
            if (!dock.isOpen)
                dock.open()
            dock.raise()
        }
        const content = dock.content
        if (method && content && typeof content[method] === "function")
            content[method](arg, focus)
    }
    // --- end of open requests ---
}
