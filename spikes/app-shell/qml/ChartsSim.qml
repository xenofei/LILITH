// 6 telemetry charts drawn with Qt Quick Shapes PathPolyline, 900 points each, a point a second
// from Feed.telemetry (S0.6, P4a-R17). Each tile is a focusable delegate with an accessible role
// and name (S0.7). With `batched`, all charts redraw together once a second (S0.6's fallback).
import QtQuick
import QtQuick.Shapes
import SpikeShim

Rectangle {
    id: root
    color: "#2e3440"
    property bool batched: false
    property int pointsReceived: 0
    readonly property int pointCount: 900
    readonly property var names: ["GPU load", "VRAM", "CPU load", "RAM", "Power", "Temperature"]
    readonly property var units: ["%", "GiB", "%", "GiB", "W", "°C"]
    readonly property var ranges: [[0, 100], [0, 16], [0, 100], [0, 64], [0, 450], [20, 90]]

    // Series values, oldest first; prefilled so every polyline has 900 points from the start.
    property var series: []
    property var dirty: [false, false, false, false, false, false]

    Component.onCompleted: {
        const s = []
        for (let i = 0; i < 6; ++i) {
            const v = []
            const mid = (ranges[i][0] + ranges[i][1]) / 2
            const amp = (ranges[i][1] - ranges[i][0]) / 4
            for (let k = 0; k < pointCount; ++k)
                v.push(mid + amp * Math.sin(k / (15 + 4 * i)))
            s.push(v)
        }
        series = s
        for (let i = 0; i < 6; ++i)
            tiles.itemAt(i).redraw()
    }

    Connections {
        target: Feed
        function onTelemetry(seriesIndex, value) {
            const v = root.series[seriesIndex]
            v.shift()
            v.push(value)
            root.pointsReceived++
            if (root.batched)
                root.dirty[seriesIndex] = true
            else
                tiles.itemAt(seriesIndex).redraw()
        }
    }

    Timer {
        interval: 1000
        repeat: true
        running: root.batched
        onTriggered: {
            for (let i = 0; i < 6; ++i) {
                if (root.dirty[i]) {
                    root.dirty[i] = false
                    tiles.itemAt(i).redraw()
                }
            }
        }
    }

    Grid {
        anchors.fill: parent
        anchors.margins: 6
        columns: 3
        spacing: 6

        Repeater {
            id: tiles
            model: 6
            delegate: Rectangle {
                id: tile
                required property int index
                property real latest: 0
                width: (root.width - 12 - 2 * 6) / 3
                height: (root.height - 12 - 6) / 2
                color: activeFocus ? "#434c5e" : "#3b4252"
                radius: 4
                activeFocusOnTab: true
                border.color: activeFocus ? "#88c0d0" : "transparent"

                Accessible.role: Accessible.Chart
                Accessible.name: root.names[index] + " chart, latest " + latest.toFixed(1) + " " + root.units[index]
                Accessible.focusable: true

                function redraw() {
                    const v = root.series[index]
                    if (!v) return
                    const r = root.ranges[index]
                    const w = plot.width, h = plot.height
                    const pts = new Array(v.length)
                    for (let k = 0; k < v.length; ++k)
                        pts[k] = Qt.point(k * w / (v.length - 1), h - (v[k] - r[0]) / (r[1] - r[0]) * h)
                    line.path = pts
                    latest = v[v.length - 1]
                }

                Text {
                    id: caption
                    x: 6; y: 4
                    text: root.names[tile.index] + "  " + tile.latest.toFixed(1) + " " + root.units[tile.index]
                    color: "#d8dee9"
                    font.pixelSize: 11
                }

                Item {
                    id: plot
                    anchors { left: parent.left; right: parent.right; top: caption.bottom; bottom: parent.bottom; margins: 6 }
                    onWidthChanged: tile.redraw()
                    onHeightChanged: tile.redraw()
                    Shape {
                        anchors.fill: parent
                        ShapePath {
                            strokeColor: ["#88c0d0", "#a3be8c", "#ebcb8b", "#b48ead", "#d08770", "#bf616a"][tile.index]
                            strokeWidth: 1.5
                            fillColor: "transparent"
                            PathPolyline { id: line }
                        }
                    }
                }
            }
        }
    }
}
