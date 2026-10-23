// S0.9: the shell's GUI pieces under QT_QPA_PLATFORM=offscreen. One test function per item, so
// run-spike.ps1 can run and record each item on its own:
//   test_kddw_main_and_floating  a KDDockWidgets main window and one floating window
//   test_shape_polyline          a Qt Quick Shapes PathPolyline renders
//   test_web_loader              a Loader of the SpikeWeb plugin reaches Ready
//   test_web_about_blank         ...and its web view loads about:blank (recorded, not an item)
//   test_grab_not_flat           a grab of the KDDockWidgets window isn't one flat colour
import QtQuick
import QtQuick.Shapes
import QtTest
import com.kdab.dockwidgets 2.0 as KDDW

TestCase {
    id: tc
    name: "AppShell"
    when: windowShown
    width: 400
    height: 300

    Component {
        id: shellWindow
        Window {
            id: win
            width: 640
            height: 420
            visible: true
            color: "#2e3440"
            property alias dockA: dockA
            property alias dockB: dockB
            property alias contentA: contentA
            property alias contentB: contentB
            property alias shape: shapeItem

            KDDW.DockingArea {
                anchors.fill: parent
                uniqueName: "tst-main"

                KDDW.DockWidget {
                    id: dockA
                    uniqueName: "tst-a"
                    title: "A"
                    Rectangle {
                        id: contentA
                        anchors.fill: parent
                        color: "#3b4252"
                        Shape {
                            id: shapeItem
                            anchors.fill: parent
                            anchors.margins: 10
                            ShapePath {
                                strokeColor: "#88c0d0"
                                strokeWidth: 3
                                fillColor: "transparent"
                                PathPolyline {
                                    path: {
                                        const pts = []
                                        for (let k = 0; k < 900; ++k)
                                            pts.push(Qt.point(k * 600 / 899, 150 + 120 * Math.sin(k / 40)))
                                        return pts
                                    }
                                }
                            }
                        }
                    }
                }
                KDDW.DockWidget {
                    id: dockB
                    uniqueName: "tst-b"
                    title: "B"
                    Rectangle { id: contentB; anchors.fill: parent; color: "#a3be8c" }
                }
                Component.onCompleted: {
                    addDockWidget(dockA, KDDW.KDDockWidgets.Location_OnLeft)
                    addDockWidget(dockB, KDDW.KDDockWidgets.Location_OnRight)
                }
            }
        }
    }

    Component {
        id: webLoaderComponent
        Item {
            width: 400
            height: 300
            property alias loader: loader
            property var loads: []
            Loader {
                id: loader
                anchors.fill: parent
                source: Qt.resolvedUrl("WebLoaderFixture.qml")
                onLoaded: item.loaded.connect(function(status) { parent.loads.push(status) })
            }
        }
    }

    property var win: null

    function makeWindow() {
        if (!win) {
            win = createTemporaryObject(shellWindow, tc)
            verify(win, "shell window created")
            tryVerify(() => win.contentA.width > 0 && win.contentB.width > 0, 5000, "docks laid out")
        }
        return win
    }

    // Distinct colours on a 16x16 sample grid of the image.
    function distinctColours(img) {
        const seen = {}
        let n = 0
        for (let gy = 0; gy < 16; ++gy) {
            for (let gx = 0; gx < 16; ++gx) {
                const c = img.pixel(Math.floor((gx + 0.5) * img.width / 16), Math.floor((gy + 0.5) * img.height / 16))
                const key = c.toString()
                if (!seen[key]) { seen[key] = true; ++n }
            }
        }
        return n
    }

    function test_kddw_main_and_floating() {
        const w = makeWindow()
        w.dockB.isFloating = true
        tryVerify(() => w.dockB.isFloating, 5000, "dock B floats")
        tryVerify(() => w.contentB.Window.window !== null && w.contentB.Window.window !== w, 5000,
                  "dock B is in its own window")
        tryVerify(() => w.contentB.Window.window.visible, 5000, "the floating window is visible")
        verify(w.contentA.Window.window === w, "dock A stays in the main window")
        w.dockB.isFloating = false
        tryVerify(() => w.contentB.Window.window === w, 5000, "dock B docks back")
    }

    function test_shape_polyline() {
        const w = makeWindow()
        tryVerify(() => w.shape.status === Shape.Ready, 5000, "shape ready")
        const img = grabImage(w.shape)
        verify(img.width > 0 && img.height > 0, "shape grabbed")
        verify(distinctColours(img) >= 2, "the polyline drew something")
    }

    function test_grab_not_flat() {
        const w = makeWindow()
        wait(200)
        const img = grabImage(w.contentA)
        verify(img.width > 0 && img.height > 0, "grabbed")
        const n = distinctColours(img)
        console.log("grab: graphics api " + w.contentA.GraphicsInfo.api + ", distinct colours " + n)
        verify(n >= 2, "grab has " + n + " distinct colours")
    }

    function test_web_loader() {
        const holder = createTemporaryObject(webLoaderComponent, tc)
        tryVerify(() => holder.loader.status === Loader.Ready || holder.loader.status === Loader.Error, 15000)
        compare(holder.loader.status, Loader.Ready, "SpikeWeb loaded through a Loader")
    }

    function test_web_about_blank() {
        const holder = createTemporaryObject(webLoaderComponent, tc)
        tryVerify(() => holder.loader.status === Loader.Ready, 15000, "loader ready")
        tryVerify(() => holder.loads.indexOf("succeeded") >= 0 || holder.loads.indexOf("failed") >= 0, 30000,
                  "about:blank finished loading")
        verify(holder.loads.indexOf("succeeded") >= 0, "about:blank loaded: " + JSON.stringify(holder.loads))
    }
}
