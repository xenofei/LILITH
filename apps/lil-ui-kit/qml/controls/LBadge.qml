// A small label for a count or a state (APP-156): role StaticText; `name` is what a screen
// reader says (for "3", say "3 waiting confirmations"), the text when it is empty.
import QtQuick
import Lilith.UiKit

Rectangle {
    id: root

    property string text
    property string name
    // "info", "warning", "danger" or "ok".
    property string kind: "info"

    implicitWidth: Math.max(label.implicitWidth + 12, implicitHeight)
    implicitHeight: label.implicitHeight + 4
    radius: height / 2
    color: kind === "danger" ? Theme.danger
         : kind === "warning" ? Theme.warning
         : kind === "ok" ? Theme.ok : Theme.accent

    Accessible.role: Accessible.StaticText
    Accessible.name: name !== "" ? name : text

    Text {
        id: label
        anchors.centerIn: parent
        text: root.text
        color: Theme.bg
        font.pixelSize: Theme.fontSize - 1
        font.bold: true
    }
}
