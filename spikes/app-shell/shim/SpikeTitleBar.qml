// The spike's themed dock title bar (S0.1), handed to KDDockWidgets by SpikeDockFactory.
// It derives from KDDockWidgets' TitleBarBase (which handles dragging) and adds an accessible
// role and name for the Narrator check (S0.7).
import QtQuick
import "qrc:/kddockwidgets/qtquick/views/qml/" as KDDW

KDDW.TitleBarBase {
    id: root
    color: "#1f2430"
    border.color: "#3b4252"
    border.width: 1
    heightWhenVisible: 28

    Accessible.role: Accessible.TitleBar
    Accessible.name: root.title

    Text {
        text: root.title
        color: root.isFocused ? "#88c0d0" : "#d8dee9"
        font.pixelSize: 13
        font.bold: root.isFocused
        anchors {
            left: parent.left
            leftMargin: 8
            verticalCenter: parent.verticalCenter
        }
    }

    Row {
        spacing: 4
        anchors {
            right: parent.right
            rightMargin: 6
            verticalCenter: parent.verticalCenter
        }

        Rectangle {
            visible: root.floatButtonVisible
            width: 18
            height: 18
            radius: 3
            color: floatArea.containsMouse ? "#4c566a" : "#3b4252"
            Accessible.role: Accessible.Button
            Accessible.name: "Float " + root.title
            Accessible.onPressAction: root.floatButtonClicked()
            Text {
                anchors.centerIn: parent
                text: "↗"
                color: "#d8dee9"
                font.pixelSize: 11
            }
            MouseArea {
                id: floatArea
                anchors.fill: parent
                hoverEnabled: true
                onClicked: root.floatButtonClicked()
            }
        }

        Rectangle {
            enabled: root.closeButtonEnabled
            width: 18
            height: 18
            radius: 3
            color: closeArea.containsMouse ? "#bf616a" : "#3b4252"
            Accessible.role: Accessible.Button
            Accessible.name: "Close " + root.title
            Accessible.onPressAction: root.closeButtonClicked()
            Text {
                anchors.centerIn: parent
                text: "×"
                color: "#d8dee9"
                font.pixelSize: 13
            }
            MouseArea {
                id: closeArea
                anchors.fill: parent
                hoverEnabled: true
                onClicked: root.closeButtonClicked()
            }
        }
    }
}
