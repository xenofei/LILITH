// The docks' title bar, handed to KDDockWidgets by the shim's DockFactory (APP-001, APP-012).
// It derives from KDDockWidgets' TitleBarBase, which drags the dock, and is exposed as a title
// bar named after the dock (APP-156; spike 10 S0.7 read it this way), with named Float and Close
// buttons (LIconButton: in the tab chain, pressed with Space, Enter or Return, and by UI
// Automation).
import QtQuick
import Lilith.UiKit
import "qrc:/kddockwidgets/qtquick/views/qml/" as KDDW

KDDW.TitleBarBase {
    id: root

    color: Theme.surfaceRaised
    heightWhenVisible: 28

    Accessible.role: Accessible.TitleBar
    Accessible.name: root.title

    Rectangle {
        width: parent.width
        height: 2
        anchors.bottom: parent.bottom
        color: root.isFocused ? Theme.accent : "transparent"
    }

    Text {
        anchors.left: parent.left
        anchors.leftMargin: Theme.spacing
        anchors.right: buttons.left
        anchors.verticalCenter: parent.verticalCenter
        text: root.title
        color: Theme.text
        font.pixelSize: Theme.fontSize
        font.bold: root.isFocused
        elide: Text.ElideRight
    }

    Row {
        id: buttons
        anchors.right: parent.right
        anchors.rightMargin: 4
        anchors.verticalCenter: parent.verticalCenter
        spacing: 2
        // Above TitleBarBase's drag area, so the buttons get their clicks.
        z: 1

        LIconButton {
            visible: root.floatButtonVisible
            implicitWidth: 22
            implicitHeight: 22
            glyph: "⧉"
            text: "Float " + root.title
            onClicked: root.floatButtonClicked()
        }

        LIconButton {
            enabled: root.closeButtonEnabled
            implicitWidth: 22
            implicitHeight: 22
            glyph: "×"
            text: "Close " + root.title
            onClicked: root.closeButtonClicked()
        }
    }
}
