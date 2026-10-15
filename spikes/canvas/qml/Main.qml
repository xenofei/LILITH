// Canvas compositing spike main window: a KDDockWidgets DockingArea with the Canvas dock only.
// The canvas lives in a Loader so the app can destroy and recreate it (and so its renderer) with
// --recreate-every N. The window starts hidden; main.cpp sets the graphics configuration, then shows it.
import QtQuick
import QtQuick.Controls
import com.kdab.dockwidgets 2.0 as KDDW
import CanvasShim

ApplicationWindow {
    id: root
    visible: false
    width: 1280
    height: 800
    title: "Canvas compositing spike"

    KDDW.DockingArea {
        id: dockingArea
        anchors.fill: parent
        uniqueName: "CanvasMainLayout"

        KDDW.DockWidget {
            id: canvasDock
            uniqueName: "canvas"
            title: "Canvas"

            Loader {
                id: canvasLoader
                anchors.fill: parent
                active: CanvasController.canvasActive
                sourceComponent: Component {
                    CompositeCanvas {
                        objectName: "canvas"
                        // --gui-pan: advance panTime by 1 per second on the GUI thread (animation driver),
                        // so every frame needs a scene-graph sync. Not running otherwise.
                        NumberAnimation on panTime {
                            running: CanvasController.guiPan
                            from: 0
                            to: 1000000
                            duration: 1000000000
                        }
                    }
                }
            }
        }

        Component.onCompleted: addDockWidget(canvasDock, KDDW.KDDockWidgets.Location_OnLeft)
    }
}
