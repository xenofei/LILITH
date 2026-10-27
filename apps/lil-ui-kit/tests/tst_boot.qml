// The shell boots offscreen: Theme loads, ShellWindow opens with a KDDockWidgets main window
// that a dock can join, float from and return to, and the Rust bridge's Lilith.Core module
// answers. A broad smoke test of the pinned stack (Qt 6.12.0, cxx-qt 0.10.0, KDDockWidgets
// 2.5.0), which no upstream CI covers yet.
import QtQuick
import QtTest
import com.kdab.dockwidgets 2.0 as KDDW
import Lilith.Core
import Lilith.UiKit

TestCase {
    id: tc
    name: "Boot"
    when: windowShown
    width: 400
    height: 300

    Component {
        id: shellComponent
        ShellWindow { core: FakeCore {} }
    }

    Component {
        id: dockComponent
        KDDW.DockWidget {
            property alias content: content
            uniqueName: "tst-boot-dock"
            title: "Dock"
            Rectangle {
                id: content
                anchors.fill: parent
                color: Theme.surface
            }
        }
    }

    function test_theme_loads() {
        verify(Qt.colorEqual(Theme.bg, "#16181d"), "Theme.bg")
        compare(typeof Theme.spacing, "number")
        verify(Theme.spacing > 0 && Theme.radius >= 0 && Theme.fontSize > 0)
    }

    function test_core_holds_the_build_id() {
        // `<CARGO_PKG_VERSION>+<LIL_BUILD_ID or "local">` (lil_protocol's BUILD_ID).
        verify(/^\d+\.\d+\.\d+\+[0-9A-Za-z.-]+$/.test(Core.buildId), "buildId is " + Core.buildId)
    }

    function test_shell_window_opens_with_a_main_window() {
        const win = createTemporaryObject(shellComponent, tc)
        verify(win, "ShellWindow created")
        verify(!win.visible, "it starts hidden; main.cpp shows it")
        win.visible = true
        tryVerify(() => win.visible && win.dockingArea.width > 0 && win.dockingArea.height > 0, 5000,
                  "the main window is laid out")
        compare(win.dockingArea.uniqueName, "image-main")

        const dock = createTemporaryObject(dockComponent, tc)
        verify(dock, "dock created")
        win.dockingArea.addDockWidget(dock, KDDW.KDDockWidgets.Location_OnLeft)
        tryVerify(() => dock.content.Window.window === win && dock.content.width > 0, 5000,
                  "the dock is in the main window")

        dock.isFloating = true
        tryVerify(() => dock.isFloating && dock.content.Window.window !== null
                        && dock.content.Window.window !== win, 5000, "the dock floats in its own window")
        tryVerify(() => dock.content.Window.window.visible, 5000, "the floating window is visible")
        dock.isFloating = false
        tryVerify(() => dock.content.Window.window === win, 5000, "the dock docks back")
    }
}
