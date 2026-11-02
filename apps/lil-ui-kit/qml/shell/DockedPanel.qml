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
    // The shell's state folder, for a panel that asks for it (Panels.js `wantsStateDir`).
    property string stateDir

    // The panel's own item, once loaded.
    readonly property Item content: loader.item
    // Whether the keyboard focus is in the panel.
    readonly property bool panelHasFocus: loader.activeFocus

    uniqueName: panel.id
    title: panel.title

    // The item that holds this dock's tabs: two docks with the same one are tabs of one group. Null
    // while it is closed, when no one can say where it would come back.
    function tabHolder() {
        return loader.parent ? loader.parent.parent : null
    }

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
        Component.onCompleted: {
            const props = {core: root.core, title: root.panel.title}
            if (root.panel.wantsStateDir)
                props.stateDir = root.stateDir
            setSource(Panels.url(root.panel), props)
        }
    }
}
