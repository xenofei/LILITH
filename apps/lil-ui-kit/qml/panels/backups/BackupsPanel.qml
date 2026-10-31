// The Backups panel (APP-133, opened from Diagnostics): its empty state until backups arrive.
import QtQuick
import Lilith.UiKit
import "../diagnostics/DiagnosticsText.js" as Words

Rectangle {
    id: root

    required property var core
    required property string title

    color: Theme.surface

    Accessible.role: Accessible.Pane
    Accessible.name: title

    Column {
        anchors.centerIn: parent
        width: Math.min(parent.width - 2 * Theme.spacing, 420)
        spacing: Theme.spacing

        Text {
            width: parent.width
            text: root.title
            textFormat: Text.PlainText
            color: Theme.text
            font.pixelSize: Theme.fontSize + 4
            font.bold: true
            horizontalAlignment: Text.AlignHCenter
            elide: Text.ElideRight
        }

        Text {
            width: parent.width
            text: Words.noBackups
            textFormat: Text.PlainText
            color: Theme.textMuted
            font.pixelSize: Theme.fontSize
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }
    }
}
