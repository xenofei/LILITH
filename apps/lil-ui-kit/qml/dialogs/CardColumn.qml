// A scrolling column of cards. The cards stay made, so a focused button or a rest on Approve isn't
// lost when another card comes or goes. Tab to a button below the fold scrolls it into view; the
// keyboard coming back to the button that last had it (after an outage) leaves the view where it is.
import QtQuick
import QtQuick.Controls.Basic as QQC
import Lilith.UiKit

Flickable {
    id: root

    default property alias content: column.data
    readonly property alias column: column

    contentWidth: width
    contentHeight: column.implicitHeight
    clip: true
    boundsBehavior: Flickable.StopAtBounds
    interactive: contentHeight > height

    QQC.ScrollBar.vertical: QQC.ScrollBar {}

    // The control in the column that last had the keyboard.
    property Item _lastFocus: null

    function _isInside(item) {
        for (let p = item; p; p = p.parent) {
            if (p === column)
                return true
        }
        return false
    }

    // `item` is the column or holds it.
    function _holds(item) {
        for (let p = root; p; p = p.parent) {
            if (p === item)
                return true
        }
        return false
    }

    Connections {
        target: root.Window.window
        function onActiveFocusItemChanged() {
            const window = root.Window.window
            const item = window.activeFocusItem
            // A control elsewhere took it (not what holds the column, which has it while nothing
            // does): coming back here later is a move to scroll for.
            if (item && !root._isInside(item) && !root._holds(item))
                root._lastFocus = null
            if (!item || !root._isInside(item) || item === root._lastFocus)
                return
            root._lastFocus = item
            const top = item.mapToItem(column, 0, 0).y
            if (top < root.contentY)
                root.contentY = Math.max(0, top - Theme.spacing)
            else if (top + item.height > root.contentY + root.height)
                root.contentY = top + item.height + Theme.spacing - root.height
        }
    }

    Column {
        id: column
        width: root.width
        spacing: Theme.spacing
    }
}
