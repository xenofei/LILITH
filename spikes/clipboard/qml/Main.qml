import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import SpikeShim

ApplicationWindow {
    id: win
    width: 420
    height: 440
    visible: true
    title: "Worldwright clipboard spike"

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 16
        spacing: 12

        Label {
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            text: "1. Click Copy, then paste into Discord, Chrome or Paint.\n" +
                  "2. Or drag the picture into an Explorer folder."
        }

        Button {
            objectName: "copyButton"
            text: "Copy"
            Layout.alignment: Qt.AlignHCenter
            onClicked: Bridge.copyToClipboard()
        }

        // Checkerboard under the picture, so the transparent parts are visible.
        Rectangle {
            Layout.alignment: Qt.AlignHCenter
            width: 256
            height: 256
            color: "#cccccc"
            Canvas {
                anchors.fill: parent
                onPaint: {
                    var c = getContext("2d");
                    c.fillStyle = "#ffffff";
                    for (var y = 0; y < 256; y += 16)
                        for (var x = 0; x < 256; x += 16)
                            if (((x + y) / 16) % 2 === 0) c.fillRect(x, y, 16, 16);
                }
            }
            Image {
                id: thumb
                objectName: "thumb"
                anchors.fill: parent
                source: "qrc:/assets/source.png"
            }
            MouseArea {
                id: ma
                anchors.fill: parent
                cursorShape: Qt.OpenHandCursor
                property point p0
                property bool started: false
                onPressed: (m) => { p0 = Qt.point(m.x, m.y); started = false }
                onReleased: started = false
                onPositionChanged: (m) => {
                    if (!started && pressed && Math.hypot(m.x - p0.x, m.y - p0.y) > 8) {
                        started = true
                        Bridge.startDrag(thumb)   // blocks until the drop is over
                        // The drag loop swallowed the button release: reset the MouseArea.
                        ma.enabled = false
                        ma.enabled = true
                    }
                }
            }
        }

        Label {
            Layout.fillWidth: true
            text: Bridge.status
        }
        Label {
            Layout.fillWidth: true
            wrapMode: Text.WrapAnywhere
            font.pixelSize: 11
            text: Bridge.exportPath === "" ? "" : "Exported copy: " + Bridge.exportPath
        }
    }
}
