// The app's main window: a KDDockWidgets main window (DockingArea) holding one dock per panel of
// Panels.js (APP-001, ARCH-010). It starts hidden; main.cpp shows it through lilshim::activate,
// with or without the focus. Later tasks add the top bar and the open requests.
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
// gpuPauseToggleRequested() for the top bar's pause control (Task 20).
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
    // Panel id -> its DockedPanel.
    readonly property var docks: ({})
    // How the layout opened: "restored", "default", or "default_after_bad" (a file was set aside).
    property string layoutOpened
    property string _lastSaved

    signal gpuPauseToggleRequested()

    width: 1280
    height: 800
    visible: false
    title: core && core.project ? "LILITH — " + core.project : "LILITH"
    color: Theme.bg

    LBanner {
        id: layoutBanner
        width: parent.width
        kind: "warning"
    }

    KDDW.DockingArea {
        id: dockingArea
        anchors.top: layoutBanner.visible ? layoutBanner.bottom : parent.top
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        uniqueName: "image-main"
    }

    Component {
        id: dockComponent
        DockedPanel {}
    }

    Instantiator {
        model: JSON.parse(ShellState.shortcuts())
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

    // Saved on close before KDDockWidgets closes the docks (DockLayout's close filter).
    Connections {
        target: DockLayout
        function onMainWindowClosing(name) {
            if (name === dockingArea.uniqueName)
                root.saveLayout()
        }
    }

    // The banner is announced once the window is on screen, so a screen reader hears it.
    onVisibleChanged: {
        if (visible && layoutBanner.text !== "" && !layoutBanner.visible)
            layoutBanner.show()
    }

    Component.onCompleted: {
        LilShim.applyPalette(Theme.palette)
        for (const panel of Panels.all)
            createDock(panel.id)
        restoreLayout()
    }

    // KDDockWidgets keeps a dock (and its unique name) after its QML item goes, so the shell
    // deletes its docks with it: a later shell in the same process (the QML tests) can reuse the
    // names.
    Component.onDestruction: {
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
        return dock
    }

    function restoreLayout() {
        DockLayout.windowsCreated()
        DockLayout.registerDockFactory(name => root.createDock(name))
        const saved = stateDir !== "" ? JSON.parse(ShellState.loadLayout(stateDir)) : {status: "missing"}
        let restored = false
        let setAside = saved.status === "bad" ? saved.setAside : ""
        if (saved.status === "ok") {
            restored = DockLayout.restore(JSON.stringify(saved.layout.kddw))
            if (!restored)
                setAside = ShellState.setLayoutAside(stateDir)
        }
        if (!restored)
            applyDefaultLayout()
        DockLayout.clamp(restored ? saved.layout.windows : [],
                         (geometry, screen, screens) => JSON.parse(
                             ShellState.clamp(JSON.stringify(geometry), screen, JSON.stringify(screens))))
        layoutOpened = restored ? "restored" : saved.status === "missing" ? "default" : "default_after_bad"
        if (layoutOpened === "default_after_bad") {
            layoutBanner.text = "The saved layout couldn't be opened, so the Default layout is open."
                + (setAside ? " The old file was kept as " + setAside + "." : "")
            if (visible)
                layoutBanner.show()
        }
        _lastSaved = layoutText()
    }

    // The Default layout (APP-004): Home in the centre; Jobs and Confirmations tabbed on the
    // right; Hardware, Usage & spend, Diagnostics and Settings tabbed at the bottom. Panels with
    // no area stay closed.
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
            if (panel.area === "centre")
                dockingArea.addDockWidget(dock, KDDW.KDDockWidgets.Location_OnLeft)
            else if (panel.area === "right")
                dockingArea.addDockWidget(dock, KDDW.KDDockWidgets.Location_OnRight, null, Qt.size(360, 0))
            else
                dockingArea.addDockWidget(dock, KDDW.KDDockWidgets.Location_OnBottom, null, Qt.size(0, 240))
        }
        for (const area in first)
            first[area].setAsCurrentTab()
    }

    // The layout file's text now: {layout: 1, windows, kddw}.
    function layoutText() {
        return JSON.stringify({layout: 1, windows: DockLayout.mainWindows(),
                               kddw: JSON.parse(DockLayout.serialize())})
    }

    // Writes the layout when it changed since it was read or last written. Empty, or why not.
    function saveLayout() {
        if (stateDir === "")
            return ""
        const text = layoutText()
        if (text === _lastSaved)
            return ""
        const why = ShellState.saveLayout(stateDir, text)
        if (why === "")
            _lastSaved = text
        return why
    }

    function showPanel(id) {
        const dock = docks[id]
        if (!dock)
            return
        if (!dock.isOpen)
            dock.open()
        dock.raise()
        dock.focusPanel()
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
}
