// App-shell spike: two KDDockWidgets main windows and one floating dock (S0.1).
//   main-a: Jobs | Grid (+ Web tabbed behind Grid)
//   main-b: Charts
//   floating: Counter
// Nothing here imports QtWebEngine or SpikeWeb: the Web dock's Loader loads WebPanel.qml (the
// only file importing SpikeWeb) when the dock is first really shown (APP-137).
import QtQuick
import QtQuick.Controls
import com.kdab.dockwidgets 2.0 as KDDW
import SpikeBridge
import SpikeShim

ApplicationWindow {
    id: mainA
    objectName: "main-a"
    title: "LILITH spike — main-a"
    visible: true
    x: 40; y: 40; width: 900; height: 620
    flags: onTop ? (Qt.Window | Qt.WindowStaysOnTopHint) : Qt.Window

    // Set from main.cpp.
    property bool onTop: false
    property int quitAfterMs: -1
    property int showWebMs: -1
    property string webUrl: "about:blank"
    property string layoutSavePath: ""
    property string layoutRestorePath: ""
    property string windowRoundtripPath: ""
    property bool loadOn: false
    property int loadRateHz: 10
    property bool chartsBatched: false
    property int measureSeconds: -1
    property int autoCounterMs: -1
    property string screenshotPath: ""

    property bool webShown: false
    property bool layoutBuilt: false

    // Default and test geometries (logical px). They fit a 1920x1080 primary screen at (0, 0), so the
    // check doesn't depend on which monitors are on: KDDockWidgets moves a floating window that
    // would restore off-screen back onto a screen.
    readonly property var defaultGeometry: ({
        "main-a": [40, 40, 900, 620], "main-b": [960, 60, 880, 600], "floating:counter": [300, 690, 400, 280] })
    readonly property var testGeometry: ({
        "main-a": [70, 70, 880, 600], "main-b": [990, 90, 860, 580], "floating:counter": [360, 720, 420, 260] })

    function quitNow(reason) {
        if (screenshotPath !== "")
            SpikeLog.grabWindow(mainA, screenshotPath)
        SpikeLog.quit(reason, 0)
    }

    // ---- layout snapshot (S0.1) ---------------------------------------------------------
    readonly property var docks: [
        { name: "jobs", dock: jobsDock, content: jobsContent },
        { name: "grid", dock: gridDock, content: gridContent },
        { name: "web", dock: webDock, content: webHost },
        { name: "charts", dock: chartsDock, content: chartsContent },
        { name: "counter", dock: counterDock, content: counterContent }
    ]

    function windowName(w) {
        if (!w) return null
        if (w === mainA) return "main-a"
        if (w === mainB) return "main-b"
        const inside = docks.filter(d => d.dock.isOpen && d.content.Window.window === w).map(d => d.name).sort()
        return "floating:" + inside.join("+")
    }

    function geometryOf(w) { return [w.x, w.y, w.width, w.height] }

    function snapshot(label) {
        const wins = {}
        const dockInfo = []
        for (const d of docks) {
            const w = d.content.Window.window
            const name = d.dock.isOpen ? windowName(w) : null
            if (name && !(name in wins))
                wins[name] = geometryOf(w)
            const p = (w && d.dock.isOpen) ? d.content.mapToItem(null, 0, 0) : Qt.point(0, 0)
            dockInfo.push({ name: d.name, open: d.dock.isOpen, floating: d.dock.isFloating, window: name,
                            rect: [Math.round(p.x), Math.round(p.y), Math.round(d.content.width), Math.round(d.content.height)] })
        }
        SpikeLog.log("layout_snapshot", { label: label, windows: wins, docks: dockInfo })
    }

    function counterWindow() { return counterContent.Window.window }

    function applyGeometry(g) {
        mainA.x = g["main-a"][0]; mainA.y = g["main-a"][1]; mainA.width = g["main-a"][2]; mainA.height = g["main-a"][3]
        mainB.x = g["main-b"][0]; mainB.y = g["main-b"][1]; mainB.width = g["main-b"][2]; mainB.height = g["main-b"][3]
        const fw = counterWindow()
        if (fw && fw !== mainA && fw !== mainB) {
            const f = g["floating:counter"]
            fw.x = f[0]; fw.y = f[1]; fw.width = f[2]; fw.height = f[3]
        }
    }

    // ---- a tiny step runner: [delay ms, function] pairs, run in order --------------------
    property var steps: []
    function runSteps(list) { steps = list; stepTimer.next() }
    Timer {
        id: stepTimer
        function next() {
            if (mainA.steps.length === 0) return
            const s = mainA.steps[0]
            interval = Math.max(1, s[0])
            start()
        }
        onTriggered: {
            const s = mainA.steps.shift()
            s[1]()
            next()
        }
    }

    // ---- the docks ------------------------------------------------------------------------
    SpikeCounter { id: counter }

    KDDW.DockingArea {
        id: areaA
        anchors.fill: parent
        uniqueName: "main-a"

        KDDW.DockWidget {
            id: jobsDock
            uniqueName: "jobs"
            title: "Jobs"
            JobsSim { id: jobsContent; anchors.fill: parent }
        }

        KDDW.DockWidget {
            id: gridDock
            uniqueName: "grid"
            title: "Grid"
            GridSim { id: gridContent; anchors.fill: parent; scrolling: mainA.loadOn }
        }

        KDDW.DockWidget {
            id: webDock
            uniqueName: "web"
            title: "Web"
            Item {
                id: webHost
                anchors.fill: parent
                function checkShown() {
                    if (mainA.webShown || !visible || width <= 0 || !mainA.visible)
                        return
                    // Stop the start-up probe and log first: setting webShown activates the Loader.
                    SpikeLog.stopModuleProbe()
                    SpikeLog.log("web_dock_shown", { webengine_dll_in_process: SpikeLog.moduleLoaded("Qt6WebEngineCore.dll") })
                    mainA.webShown = true
                }
                // Deferred, so the transient visibility while the tab is built doesn't count.
                onVisibleChanged: Qt.callLater(checkShown)
                Loader {
                    id: webLoader
                    anchors.fill: parent
                    active: mainA.webShown
                    source: mainA.webShown ? "WebPanel.qml" : ""
                    onLoaded: {
                        item.webUrl = mainA.webUrl
                        SpikeLog.log("webengine_loaded", { webengine_dll_in_process: SpikeLog.moduleLoaded("Qt6WebEngineCore.dll") })
                    }
                    onStatusChanged: if (status === Loader.Error)
                        SpikeLog.log("error", { what: "WebPanel.qml failed to load" })
                }
            }
        }

        KDDW.DockWidget {
            id: counterDock
            uniqueName: "counter"
            title: "Counter"
            Rectangle {
                id: counterContent
                anchors.fill: parent
                color: "#24292f"
                Column {
                    anchors.centerIn: parent
                    spacing: 12
                    Label {
                        id: countLabel
                        text: "Count: " + counter.count
                        color: "white"
                        font.pixelSize: 22
                        Accessible.role: Accessible.StaticText
                        Accessible.name: text
                    }
                    Button {
                        id: incrementButton
                        text: "Increment"
                        Accessible.name: "Increment the counter"
                        onClicked: {
                            counter.increment()
                            Accessible.announce("Count is now " + counter.count)
                            SpikeLog.log("counter", { count: counter.count, text: countLabel.text })
                        }
                    }
                }
            }
        }

        Component.onCompleted: {
            addDockWidget(jobsDock, KDDW.KDDockWidgets.Location_OnLeft)
            addDockWidget(gridDock, KDDW.KDDockWidgets.Location_OnRight, null, Qt.size(520, 700))
            gridDock.addDockWidgetAsTab(webDock)
            gridDock.setAsCurrentTab()
        }
    }

    Window {
        id: mainB
        objectName: "main-b"
        title: "LILITH spike — main-b"
        transientParent: null
        visible: true
        x: 960; y: 60; width: 880; height: 600
        color: "#2e3440"

        KDDW.DockingArea {
            id: areaB
            anchors.fill: parent
            uniqueName: "main-b"

            KDDW.DockWidget {
                id: chartsDock
                uniqueName: "charts"
                title: "Charts"
                ChartsSim { id: chartsContent; anchors.fill: parent; batched: mainA.chartsBatched }
            }

            Component.onCompleted: addDockWidget(chartsDock, KDDW.KDDockWidgets.Location_OnLeft)
        }
    }

    // ---- start-up ---------------------------------------------------------------------------
    Component.onCompleted: {
        Heartbeat.attach(mainA)
        // The counter dock starts floating; then the scripted part runs.
        runSteps([
            [300, () => {
                counterDock.open()
            }],
            [400, () => applyGeometry(defaultGeometry)],
            [600, () => {
                mainA.layoutBuilt = true
                SpikeLog.log("layout_built", { webengine_dll_in_process: SpikeLog.moduleLoaded("Qt6WebEngineCore.dll") })
                snapshot("default")
                mainA.afterLayout()
            }]
        ])
    }

    function afterLayout() {
        if (layoutSavePath !== "") {
            runSteps([
                [200, () => applyGeometry(testGeometry)],
                [1000, () => { snapshot("saved"); SpikeLayout.saveAll(layoutSavePath) }],
                [300, () => quitNow("layout-save")]
            ])
        } else if (layoutRestorePath !== "") {
            runSteps([
                [500, () => { snapshot("before_restore"); SpikeLayout.restore(layoutRestorePath) }],
                [1200, () => snapshot("restored")],
                [300, () => quitNow("layout-restore")]
            ])
        } else if (windowRoundtripPath !== "") {
            runSteps([
                [200, () => applyGeometry(testGeometry)],
                [1000, () => {
                    snapshot("window_saved")
                    SpikeLayout.saveMainWindow("main-b", windowRoundtripPath)
                    SpikeLog.log("window_file", SpikeLayout.describe(windowRoundtripPath))
                }],
                [300, () => {
                    // Disturb: move main-b, float its dock, move main-a.
                    mainB.x = 1020; mainB.y = 120; mainB.width = 820; mainB.height = 540
                    chartsDock.isFloating = true
                    mainA.x = 100; mainA.y = 100; mainA.width = 860; mainA.height = 580
                }],
                [1200, () => { snapshot("window_disturbed"); SpikeLayout.restore(windowRoundtripPath) }],
                [1200, () => snapshot("window_restored")],
                [300, () => quitNow("window-roundtrip")]
            ])
        }
    }

    // --show-web <ms>
    Timer {
        interval: Math.max(0, mainA.showWebMs)
        running: mainA.showWebMs >= 0
        onTriggered: {
            SpikeLog.log("show_web_requested", {})
            webDock.setAsCurrentTab()
            Qt.callLater(webHost.checkShown)
        }
    }

    // --auto-counter <ms>
    Timer {
        interval: Math.max(0, mainA.autoCounterMs)
        running: mainA.autoCounterMs >= 0
        onTriggered: clickTimer.start()
    }
    Timer {
        id: clickTimer
        property int clicks: 0
        interval: 500
        repeat: true
        triggeredOnStart: true
        onTriggered: {
            incrementButton.clicked()
            if (++clicks >= 3)
                stop()
        }
    }

    // --load and --measure <s> (S0.6)
    Timer {
        interval: 800
        running: mainA.loadOn
        onTriggered: {
            Feed.start(mainA.loadRateHz)
            SpikeLog.log("load_started", { rate_hz: mainA.loadRateHz, charts_batched: mainA.chartsBatched })
            if (mainA.measureSeconds > 0) {
                Heartbeat.start(4)
                runSteps([
                    [5000, () => Heartbeat.report("warmup")],
                    [mainA.measureSeconds * 1000, () => {
                        Heartbeat.report("measured")
                        SpikeLog.log("load_counts", { feed_ticks: Feed.ticks, decoded: Previews.decodedCount(),
                                                      grid_loaded: gridContent.loadedRows, grid_fetches: gridContent.fetches,
                                                      grid_rows_scrolled: gridContent.rowsScrolled,
                                                      chart_points: chartsContent.pointsReceived })
                    }],
                    [200, () => quitNow("measure")]
                ])
            }
        }
    }

    // --quit-after <ms>
    Timer {
        interval: Math.max(0, mainA.quitAfterMs)
        running: mainA.quitAfterMs >= 0
        onTriggered: mainA.quitNow("quit-after")
    }
}
