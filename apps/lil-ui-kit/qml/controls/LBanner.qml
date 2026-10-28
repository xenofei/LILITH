// A banner across the top of a view (APP-156): role AlertMessage, its text as its accessible
// name, in the tab chain. show(text) shows it and announces the text (Accessible.announce, which
// UI Automation raises as a notification; show(text, false) leaves the announcement to a later
// announce()); Escape, or its Dismiss button, closes it.
import QtQuick
import Lilith.UiKit

Item {
    id: root

    property string text
    // "info", "warning", "danger" or "ok": the colour of its edge.
    property string kind: "info"
    property bool closable: true
    readonly property alias closeButton: closeButton

    signal closed()

    function show(message, announceNow) {
        if (message !== undefined)
            text = message
        visible = true
        if (announceNow !== false)
            announce()
    }

    function announce() {
        Accessible.announce(text)
    }

    function close() {
        if (!visible)
            return
        // The keyboard focus moves on to the next item outside the banner, not to nothing.
        if (activeFocus || closeButton.activeFocus) {
            let next = closeButton.nextItemInFocusChain(true)
            for (let n = 0; next && (next === root || next === closeButton) && n < 3; ++n)
                next = next.nextItemInFocusChain(true)
            if (next && next !== root && next !== closeButton)
                next.forceActiveFocus(Qt.TabFocusReason)
        }
        visible = false
        closed()
    }

    visible: false
    // A fixed default: deriving it from the row, whose width follows the banner's, loops.
    implicitWidth: 480
    implicitHeight: Math.max(row.implicitHeight + Theme.spacing, 36)
    activeFocusOnTab: true

    Accessible.role: Accessible.AlertMessage
    Accessible.name: text

    Keys.onEscapePressed: event => {
        if (root.closable) {
            root.close()
            event.accepted = true
        }
    }

    Rectangle {
        anchors.fill: parent
        color: Theme.surfaceRaised
        border.color: root.activeFocus ? Theme.focus : "transparent"
        border.width: Theme.focusWidth

        Rectangle {
            width: 4
            height: parent.height
            color: root.kind === "danger" ? Theme.danger
                 : root.kind === "warning" ? Theme.warning
                 : root.kind === "ok" ? Theme.ok : Theme.accent
        }
    }

    Row {
        id: row
        anchors.verticalCenter: parent.verticalCenter
        x: Theme.spacing + 4
        width: parent.width - x - Theme.spacing
        spacing: Theme.spacing

        Text {
            width: row.width - (closeButton.visible ? closeButton.width + row.spacing : 0)
            anchors.verticalCenter: parent.verticalCenter
            text: root.text
            color: Theme.text
            font.pixelSize: Theme.fontSize
            wrapMode: Text.Wrap
        }

        LIconButton {
            id: closeButton
            visible: root.closable
            anchors.verticalCenter: parent.verticalCenter
            glyph: "×"
            text: "Dismiss"
            onClicked: root.close()
        }
    }
}
