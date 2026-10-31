// The list a top-bar switcher opens: a keyboard-navigable list (one tab stop, the arrow keys and
// Enter work inside it, A11yWalk's rule), and a line for what went wrong. The list takes the
// keyboard focus when it opens; Escape or a press outside closes it. `activated(index)` is Enter on
// a row; the rows handle their own clicks.
import QtQuick
import QtQuick.Templates as T
import Lilith.UiKit

T.Popup {
    id: root

    property alias view: list
    property alias rows: list.model
    property alias rowDelegate: list.delegate
    // The list's accessible name.
    property string listName
    // A refusal, plain text.
    property string message
    property alias messageLine: messageLine

    signal activated(int index)

    y: parent ? parent.height + 4 : 0
    width: 420
    padding: Theme.spacing
    focus: true
    closePolicy: T.Popup.CloseOnEscape | T.Popup.CloseOnPressOutside
    implicitHeight: content.implicitHeight + topPadding + bottomPadding

    onOpened: list.forceActiveFocus(Qt.PopupFocusReason)

    background: Rectangle {
        color: Theme.surface
        radius: Theme.radius
        border.color: Theme.surfaceRaised
    }

    contentItem: Column {
        id: content
        spacing: Theme.spacing

        Text {
            id: messageLine
            visible: root.message !== ""
            width: parent.width
            text: root.message
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            color: Theme.danger
            font.pixelSize: Theme.fontSize
            Accessible.role: Accessible.AlertMessage
            Accessible.name: text
        }

        ListView {
            id: list
            width: parent.width
            height: Math.min(contentHeight, 320)
            clip: true
            spacing: 2
            activeFocusOnTab: true
            boundsBehavior: Flickable.StopAtBounds
            Accessible.role: Accessible.List
            Accessible.name: root.listName
            Keys.onReturnPressed: root.activated(currentIndex)
            Keys.onEnterPressed: root.activated(currentIndex)
        }
    }
}
