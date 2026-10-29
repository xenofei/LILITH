// A panel that isn't built yet: its name and a line saying so. Tasks 21 to 27 replace each one
// in Panels.js with the real panel, which takes the same required properties.
import QtQuick
import Lilith.UiKit

Rectangle {
    id: root

    required property var core
    required property string title
    // What the float-and-redock test keeps (ARCH-116): a real panel checks its own state.
    property string note

    color: Theme.surface

    Accessible.role: Accessible.Pane
    Accessible.name: title

    Column {
        anchors.centerIn: parent
        width: Math.min(parent.width - 2 * Theme.spacing, 360)
        spacing: Theme.spacing

        Text {
            width: parent.width
            text: root.title
            color: Theme.text
            font.pixelSize: Theme.fontSize + 4
            font.bold: true
            horizontalAlignment: Text.AlignHCenter
            elide: Text.ElideRight
        }

        Text {
            width: parent.width
            text: "This panel isn't built yet."
            color: Theme.textMuted
            font.pixelSize: Theme.fontSize
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
        }
    }
}
