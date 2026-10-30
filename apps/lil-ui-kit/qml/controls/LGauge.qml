// A horizontal gauge (APP-156): role ProgressBar, accessible name "<label>: <n> %". `value` is a
// fraction from 0 to 1; at `warnAt` and `dangerAt` it takes the warning and danger colours
// (the cap gauges use 0.5, 0.8 and 1, APP-121).
import QtQuick
import Lilith.UiKit

Item {
    id: root

    property string label
    property real value: 0
    property real warnAt: 0.8
    property real dangerAt: 1.0

    readonly property int percent: Math.round(Math.max(0, value) * 100)

    implicitWidth: 200
    implicitHeight: caption.implicitHeight + 4 + 8

    Accessible.role: Accessible.ProgressBar
    Accessible.name: label + ": " + percent + " %"

    Text {
        id: caption
        width: parent.width
        text: root.label
        textFormat: Text.PlainText
        color: Theme.textMuted
        font.pixelSize: Theme.fontSize - 1
        elide: Text.ElideRight

        Text {
            anchors.right: parent.right
            text: root.percent + " %"
            textFormat: Text.PlainText
            color: Theme.text
            font: caption.font
        }
    }

    Rectangle {
        y: caption.height + 4
        width: parent.width
        height: 8
        radius: 4
        color: Theme.surfaceRaised

        Rectangle {
            width: parent.width * Math.min(1, Math.max(0, root.value))
            height: parent.height
            radius: parent.radius
            color: root.value >= root.dangerAt ? Theme.danger
                 : root.value >= root.warnAt ? Theme.warning : Theme.accent
        }
    }
}
