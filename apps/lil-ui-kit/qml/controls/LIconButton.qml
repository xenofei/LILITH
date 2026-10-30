// A button that shows a glyph (APP-156): `text` is its accessible name and its tooltip, so it
// must be set; `glyph` is what it shows. Space, Enter and Return press it.
import QtQuick
import QtQuick.Templates as T
import Lilith.UiKit

T.Button {
    id: control

    property string glyph

    implicitWidth: 28
    implicitHeight: 28
    padding: 0
    focusPolicy: Qt.StrongFocus
    font.pixelSize: Theme.fontSize + 1

    Accessible.name: text

    Keys.onReturnPressed: control.click()
    Keys.onEnterPressed: control.click()

    contentItem: Text {
        text: control.glyph
        textFormat: Text.PlainText
        font: control.font
        color: control.enabled ? Theme.text : Theme.textMuted
        horizontalAlignment: Text.AlignHCenter
        verticalAlignment: Text.AlignVCenter
    }

    background: Rectangle {
        radius: Theme.radius
        color: control.down ? Theme.bg : control.hovered ? Theme.surfaceRaised : "transparent"
        border.color: Theme.focus
        border.width: control.visualFocus ? Theme.focusWidth : 0
    }
}
