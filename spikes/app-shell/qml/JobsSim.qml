// 8 job tiles: the latest preview through image://preview (decoded off the GUI thread) and a
// progress bar, updated from PreviewHub.updated (S0.6).
import QtQuick
import QtQuick.Controls
import SpikeShim

Rectangle {
    id: root
    color: "#2e3440"

    Connections {
        target: Previews
        function onUpdated(job, frame, progress) {
            const tile = tiles.itemAt(job)
            if (tile) {
                tile.frame = frame
                tile.progress = progress
            }
        }
    }

    Grid {
        anchors.fill: parent
        anchors.margins: 6
        columns: 4
        spacing: 6

        Repeater {
            id: tiles
            model: 8
            delegate: Rectangle {
                id: tile
                required property int index
                property int frame: 0
                property real progress: 0
                width: (root.width - 12 - 3 * 6) / 4
                height: (root.height - 12 - 6) / 2
                color: "#3b4252"
                radius: 4

                Image {
                    anchors { left: parent.left; right: parent.right; top: parent.top; bottom: bar.top; margins: 4 }
                    fillMode: Image.PreserveAspectFit
                    asynchronous: true
                    cache: false
                    retainWhileLoading: true
                    source: tile.frame > 0 ? "image://preview/" + tile.index + "/" + tile.frame : ""
                }
                ProgressBar {
                    id: bar
                    anchors { left: parent.left; right: parent.right; bottom: label.top; margins: 4 }
                    value: tile.progress
                }
                Text {
                    id: label
                    anchors { left: parent.left; bottom: parent.bottom; margins: 4 }
                    text: "job " + (tile.index + 1) + " · " + Math.round(tile.progress * 100) + " %"
                    color: "#d8dee9"
                    font.pixelSize: 11
                }
            }
        }
    }
}
