// A scrolling column of cards. The cards stay made, so a focused button or a rest on Approve isn't
// lost when another card comes or goes. Tab to a button below the fold scrolls it into view.
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

    function _isInside(item) {
        for (let p = item; p; p = p.parent) {
            if (p === column)
                return true
        }
        return false
    }

    Connections {
        target: root.Window.window
        function onActiveFocusItemChanged() {
            const item = root.Window.window.activeFocusItem
            if (!item || !root._isInside(item))
                return
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
