// One chart of the Hardware panel (APP-129; P4a-R17): a metric over the panel's window, drawn as
// two PathPolylines, each pixel's max and its min, which its ChartSeries makes in Rust with at
// most a point per pixel of the plot. A focusable tile with role Chart, which UI Automation
// exposes as Custom, so its name carries the word "chart" (APP-156): "GPU load chart, latest
// 63 %, last 15 minutes".
import QtQuick
import QtQuick.Shapes
import Lilith.Core
import Lilith.UiKit

Rectangle {
    id: root

    // The telemetry.series metric, its label, and its value as text.
    required property string metric
    required property string label
    property var format: v => String(Math.round(v))
    // The top of the scale; 0 scales to the window's peak.
    property real scaleMax: 0
    property int windowS: 15 * 60
    property string windowText: ""

    readonly property alias series: series
    readonly property int plotWidth: Math.max(0, Math.floor(plot.width))
    readonly property string latestText: series.hasLatest ? format(series.latest) : "no data yet"

    implicitWidth: 240
    implicitHeight: 110
    radius: Theme.radius
    color: Theme.surface
    border.color: activeFocus ? Theme.focus : Theme.surfaceRaised
    border.width: activeFocus ? Theme.focusWidth : 1
    activeFocusOnTab: true

    Accessible.role: Accessible.Chart
    Accessible.name: label + " chart, latest " + latestText + (windowText ? ", " + windowText : "")
    Accessible.focusable: true

    ChartSeries {
        id: series
        windowS: root.windowS
        widthPx: root.plotWidth
        heightPx: plot.height
        scaleMax: root.scaleMax
    }

    Text {
        id: caption
        x: Theme.spacing
        y: 6
        width: parent.width - 2 * Theme.spacing - value.implicitWidth - Theme.spacing
        text: root.label
        color: Theme.textMuted
        font.pixelSize: Theme.fontSize - 1
        elide: Text.ElideRight
    }

    Text {
        id: value
        anchors.right: parent.right
        anchors.rightMargin: Theme.spacing
        y: 6
        text: root.latestText
        color: Theme.text
        font.pixelSize: Theme.fontSize - 1
        font.bold: true
    }

    Item {
        id: plot
        anchors {
            left: parent.left
            right: parent.right
            top: caption.bottom
            bottom: parent.bottom
            margins: Theme.spacing
        }

        Rectangle {
            width: parent.width
            height: 1
            y: parent.height
            color: Theme.surfaceRaised
        }

        Shape {
            anchors.fill: parent

            ShapePath {
                strokeColor: Theme.accent
                strokeWidth: 1.5
                fillColor: "transparent"
                PathPolyline { path: series.upperPath }
            }

            ShapePath {
                strokeColor: Qt.rgba(Theme.accent.r, Theme.accent.g, Theme.accent.b, 0.45)
                strokeWidth: 1
                fillColor: "transparent"
                PathPolyline { path: series.lowerPath }
            }
        }
    }
}
