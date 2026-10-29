// One panel as a KDDockWidgets dock (APP-001): the dock's unique name is the panel's id, its
// title the panel's title, and its content the panel's QML (Panels.js), made with the panel's
// required properties `core` and `title`. Floating it moves the same content item into its own
// window, so the panel keeps its state (ARCH-116).
import QtQuick
import com.kdab.dockwidgets 2.0 as KDDW
import Lilith.UiKit
import "Panels.js" as Panels

KDDW.DockWidget {
    id: root

    // A Panels.js entry.
    required property var panel
    required property var core

    // The panel's own item, once loaded.
    readonly property Item content: loader.item
    // Whether the keyboard focus is in the panel.
    readonly property bool panelHasFocus: loader.activeFocus

    uniqueName: panel.id
    title: panel.title

    // Gives the keyboard focus to the panel's first focusable item, else to the panel.
    function focusPanel() {
        if (!loader.item)
            return
        const first = loader.item.nextItemInFocusChain(true)
        if (first && first !== loader.item && isInside(first, loader.item))
            first.forceActiveFocus(Qt.ShortcutFocusReason)
        else
            loader.forceActiveFocus(Qt.ShortcutFocusReason)
    }

    function isInside(item, ancestor) {
        for (let p = item; p; p = p.parent) {
            if (p === ancestor)
                return true
        }
        return false
    }

    Loader {
        id: loader
        anchors.fill: parent
        focus: true
        Component.onCompleted: setSource(Panels.url(root.panel), {core: root.core, title: root.panel.title})
    }
}
