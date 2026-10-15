// Qt stack spike main window: a KDDockWidgets DockingArea with three docks.
//   Canvas  - SpikeCanvas (QQuickRhiItem shim, compute + sample)
//   Counter - a button bound to the Rust SpikeCounter (cxx-qt)
//   Web     - tabbed behind Counter; its Loader creates the WebEngineView (qml/WebDock.qml) only
//             when the dock is first shown (APP-137). Nothing here imports QtWebEngine.
import QtQuick
import QtQuick.Controls
import com.kdab.dockwidgets 2.0 as KDDW
import SpikeBridge
import SpikeShim

ApplicationWindow {
    id: root
    visible: true
    width: 1280
    height: 800
    title: "Qt stack spike"

    // Set from main.cpp (command-line switches).
    property bool autoFloatCanvas: false
    property int showWebMs: -1
    property int autoCounterMs: -1
    property int quitAfterMs: -1
    property string screenshotPath: ""

    // Latched once the Web dock's content has really been shown.
    property bool webShown: false

    function quitNow(reason) {
        if (screenshotPath !== "")
            SpikeLog.grabWindow(root, screenshotPath)
        SpikeLog.log("quit_requested", { reason: reason })
        Qt.quit()
    }

    function logDocks() {
        const docks = [canvasDock, counterDock, webDock].map(d => ({
            name: d.uniqueName, title: d.title, open: d.isOpen, floating: d.isFloating
        }))
        SpikeLog.log("docks", { docks: docks })
    }

    SpikeCounter {
        id: counter
    }

    // QML logging of renderer (re)creation goes through the Rust object's signal.
    Connections {
        target: counter
        function onRendererRecreated() {
            SpikeLog.log("renderer_recreated", {
                initialize_count: canvas.initializeCount,
                backend: canvas.backend,
                canvas_in_main_window: canvas.Window.window === root
            })
        }
    }

    KDDW.DockingArea {
        id: dockingArea
        anchors.fill: parent
        uniqueName: "SpikeMainLayout"

        KDDW.DockWidget {
            id: canvasDock
            uniqueName: "canvas"
            title: "Canvas"
            onIsFloatingChanged: Qt.callLater(() => SpikeLog.log("float", {
                floating: canvasDock.isFloating,
                canvas_in_main_window: canvas.Window.window === root
            }))

            SpikeCanvas {
                id: canvas
                anchors.fill: parent
                onRendererInitialized: (count, backend) => counter.rendererRecreated()
            }
        }

        KDDW.DockWidget {
            id: counterDock
            uniqueName: "counter"
            title: "Counter"

            Rectangle {
                anchors.fill: parent
                color: "#24292f"
                Column {
                    anchors.centerIn: parent
                    spacing: 12
                    Label {
                        id: countLabel
                        objectName: "countLabel"
                        text: "Count: " + counter.count
                        color: "white"
                        font.pixelSize: 22
                    }
                    Button {
                        id: incrementButton
                        text: "Increment"
                        onClicked: counter.increment()
                    }
                }
            }
        }

        KDDW.DockWidget {
            id: webDock
            uniqueName: "web"
            title: "Web"

            Item {
                id: webHost
                anchors.fill: parent

                function checkShown() {
                    if (root.webShown || !visible || width <= 0 || !root.visible)
                        return
                    // Log first: setting webShown activates the Loader synchronously.
                    SpikeLog.log("web_dock_shown", {
                        webengine_dll_in_process: SpikeLog.moduleLoaded("Qt6WebEngineCore.dll")
                    })
                    root.webShown = true
                }
                // Deferred, so the transient visibility while the dock is being tabbed at
                // start-up does not count as "shown".
                onVisibleChanged: Qt.callLater(checkShown)

                Loader {
                    id: webLoader
                    anchors.fill: parent
                    active: root.webShown
                    source: root.webShown ? "WebDock.qml" : ""
                    onLoaded: SpikeLog.log("webengine_loaded", {
                        webengine_dll_in_process: SpikeLog.moduleLoaded("Qt6WebEngineCore.dll")
                    })
                    onStatusChanged: if (status === Loader.Error)
                        SpikeLog.log("error", { what: "WebDock.qml failed to load" })
                }
            }
        }

        Component.onCompleted: {
            addDockWidget(canvasDock, KDDW.KDDockWidgets.Location_OnLeft)
            addDockWidget(counterDock, KDDW.KDDockWidgets.Location_OnRight, null, Qt.size(380, 600))
            counterDock.addDockWidgetAsTab(webDock)
            counterDock.setAsCurrentTab()
        }
    }

    // Start-up snapshot: which docks are open, and that WebEngine is not in the process.
    Timer {
        interval: 1000
        running: true
        onTriggered: {
            SpikeLog.log("window_shown", {
                webengine_dll_in_process: SpikeLog.moduleLoaded("Qt6WebEngineCore.dll"),
                web_shown: root.webShown
            })
            root.logDocks()
        }
    }

    // --auto-float-canvas: float at 2 s, re-dock at 4 s, quit at 6 s.
    Timer {
        interval: 2000
        running: root.autoFloatCanvas
        onTriggered: canvasDock.isFloating = true
    }
    Timer {
        interval: 4000
        running: root.autoFloatCanvas
        onTriggered: canvasDock.isFloating = false
    }
    Timer {
        interval: 6000
        running: root.autoFloatCanvas
        onTriggered: root.quitNow("auto-float-canvas")
    }

    // --show-web <ms>
    Timer {
        interval: Math.max(0, root.showWebMs)
        running: root.showWebMs >= 0
        onTriggered: {
            SpikeLog.log("show_web_requested", {})
            webDock.setAsCurrentTab()
            Qt.callLater(webHost.checkShown)
        }
    }

    // --auto-counter <ms>: three clicks, 500 ms apart; logs the bound label after each.
    Timer {
        interval: Math.max(0, root.autoCounterMs)
        running: root.autoCounterMs >= 0
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
            SpikeLog.log("counter", { count: counter.count, text: countLabel.text })
            if (++clicks >= 3)
                stop()
        }
    }

    // --quit-after <ms>
    Timer {
        interval: Math.max(0, root.quitAfterMs)
        running: root.quitAfterMs >= 0
        onTriggered: root.quitNow("quit-after")
    }
}
