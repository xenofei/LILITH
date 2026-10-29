// A one-line text field (APP-156): role EditableText (the template's), `label` as its accessible
// name (the placeholder when there is no label), in the tab chain.
import QtQuick
import QtQuick.Templates as T
import Lilith.UiKit

T.TextField {
    id: control

    // What the field is for, as a screen reader says it.
    property string label

    implicitWidth: Math.max(implicitBackgroundWidth + leftInset + rightInset, 200)
    implicitHeight: Math.max(implicitBackgroundHeight + topInset + bottomInset,
                             contentHeight + topPadding + bottomPadding)
    padding: 6
    leftPadding: 8
    rightPadding: 8
    focusPolicy: Qt.StrongFocus
    font.pixelSize: Theme.fontSize
    color: Theme.text
    selectionColor: Theme.accent
    selectedTextColor: Theme.bg
    placeholderTextColor: Theme.textMuted
    verticalAlignment: TextInput.AlignVCenter

    Accessible.name: label !== "" ? label : placeholderText

    Text {
        x: control.leftPadding
        y: control.topPadding
        width: control.width - control.leftPadding - control.rightPadding
        height: control.height - control.topPadding - control.bottomPadding
        text: control.placeholderText
        font: control.font
        color: control.placeholderTextColor
        verticalAlignment: control.verticalAlignment
        visible: !control.length && !control.preeditText
        elide: Text.ElideRight
    }

    background: Rectangle {
        implicitHeight: 28
        radius: Theme.radius
        color: Theme.surfaceRaised
        border.color: control.activeFocus ? Theme.focus : Theme.textMuted
        border.width: control.activeFocus ? Theme.focusWidth : 1
    }
}
