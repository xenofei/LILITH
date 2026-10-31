// The list a top-bar switcher opens: a keyboard-navigable list (one tab stop, the arrow keys and
// Enter work inside it, A11yWalk's rule), and a line for what went wrong. The list takes the
// keyboard focus when it opens; Escape or a press outside its switcher closes it (a press on the
// switcher's own button toggles it there). `activated(index)` is Enter on a row; the rows handle
// their own clicks. It stays inside the window: its list shrinks to the room below the switcher.
//
// While the core has refused the session's workspace there is nothing to list: `offerDefault` shows
// "Use the default workspace" instead, which is `useDefault()`.
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
    property bool offerDefault: false
    property alias defaultButton: defaultButton
    // The height the list may take, from the window's room when it opened.
    property real _listRoom: 320

    signal activated(int index)
    signal useDefault()

    margins: Theme.spacing
    y: parent ? parent.height + 4 : 0
    width: {
        const window = parent ? parent.Window.window : null
        return window ? Math.min(420, window.width - 2 * margins) : 420
    }
    padding: Theme.spacing
    focus: true
    closePolicy: T.Popup.CloseOnEscape | T.Popup.CloseOnPressOutsideParent
    implicitHeight: content.implicitHeight + topPadding + bottomPadding

    onAboutToShow: {
        const window = parent ? parent.Window.window : null
        if (!window)
            return
        const below = parent.mapToItem(null, 0, parent.height).y + 4
        _listRoom = Math.max(60, Math.min(320, window.height - below - margins - topPadding - bottomPadding))
    }
    onOpened: {
        if (offerDefault)
            defaultButton.forceActiveFocus(Qt.PopupFocusReason)
        else
            list.forceActiveFocus(Qt.PopupFocusReason)
    }

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

        LButton {
            id: defaultButton
            visible: root.offerDefault
            primary: true
            text: "Use the default workspace"
            onClicked: root.useDefault()
        }

        ListView {
            id: list
            visible: !root.offerDefault
            width: parent.width
            height: Math.min(contentHeight, root._listRoom)
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
