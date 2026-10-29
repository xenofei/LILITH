// A titled card that groups content (APP-156): role Grouping, its title as its accessible name.
// Children go into its content area, below the title.
import QtQuick
import Lilith.UiKit

Rectangle {
    id: root

    property string title
    default property alias content: body.data

    implicitWidth: Math.max(heading.implicitWidth, body.childrenRect.width) + 2 * Theme.spacing
    implicitHeight: heading.implicitHeight + body.childrenRect.height + 3 * Theme.spacing
    radius: Theme.radius
    color: Theme.surface
    border.color: Theme.surfaceRaised
    border.width: 1

    Accessible.role: Accessible.Grouping
    Accessible.name: title

    Text {
        id: heading
        x: Theme.spacing
        y: Theme.spacing
        width: parent.width - 2 * Theme.spacing
        text: root.title
        textFormat: Text.PlainText
        color: Theme.text
        font.pixelSize: Theme.fontSize + 1
        font.bold: true
        elide: Text.ElideRight
    }

    Item {
        id: body
        x: Theme.spacing
        y: heading.y + heading.height + Theme.spacing
        width: parent.width - 2 * Theme.spacing
        height: parent.height - y - Theme.spacing
    }
}
