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
// gpuPauseToggleRequested() for the top bar's pause control (Task 20). Closing a panel announces
// the key that opens it again (Accessible.announce, a UI Automation notification).
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
    // How the layout opened: "restored", "default", "default_after_bad" (a file was set aside) or
    // "default_unreadable" (the file couldn't be read: it is left alone and not saved over).
    property string layoutOpened
    // The Default layout's proportions: the right column's share of the width and the bottom
    // row's share of the height; the bottom row spans the full width. Placeholders until the
    // owner picks the values.
    readonly property real defaultRightShare: 0.28
    readonly property real defaultBottomShare: 0.28
    // The plan 4a shortcut map: [{keys, action, scope}].
    readonly property var bindings: JSON.parse(ShellState.shortcuts())
    property string _lastSaved
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
        onWidthChanged: root.areaSized()
        onHeightChanged: root.areaSized()
    }

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
            // A blob KDDockWidgets accepts but that opens no main window of this shell, or none
            // of the known panels, counts as refused too.
            if (restored) {
                const windows = saved.layout.kddw.mainWindows || []
                restored = windows.some(m => m.uniqueName === dockingArea.uniqueName)
                           && Panels.all.some(p => docks[p.id] && docks[p.id].isOpen)
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
        Qt.callLater(() => { root._lastSaved = root.layoutText() })
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
            const right = Math.round(dockingArea.width * defaultRightShare)
            const bottom = Math.round(dockingArea.height * defaultBottomShare)
            if (panel.area === "centre")
                dockingArea.addDockWidget(dock, KDDW.KDDockWidgets.Location_OnLeft)
            else if (panel.area === "right")
                dockingArea.addDockWidget(dock, KDDW.KDDockWidgets.Location_OnRight, null, Qt.size(right, 0))
            else
                dockingArea.addDockWidget(dock, KDDW.KDDockWidgets.Location_OnBottom, null, Qt.size(0, bottom))
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
        if (stateDir === "" || _defaultPending || _saveBlocked || _closing)
            return ""
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
}
