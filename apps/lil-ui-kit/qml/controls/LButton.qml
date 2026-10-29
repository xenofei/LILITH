// A push button (APP-156): role Button, its text as its accessible name, in the tab chain;
// Space (the template's own) and Enter or Return press it.
import QtQuick
import QtQuick.Templates as T
import Lilith.UiKit

T.Button {
    id: control

    // An accent-filled button for the one action a view leads with.
    property bool primary: false

    implicitWidth: Math.max(implicitBackgroundWidth + leftInset + rightInset,
                            implicitContentWidth + leftPadding + rightPadding)
    implicitHeight: Math.max(implicitBackgroundHeight + topInset + bottomInset,
                             implicitContentHeight + topPadding + bottomPadding)
    padding: 6
    horizontalPadding: 12
    focusPolicy: Qt.StrongFocus
    font.pixelSize: Theme.fontSize

    Accessible.name: text

    Keys.onReturnPressed: control.click()
    Keys.onEnterPressed: control.click()

    contentItem: Text {
        text: control.text
        font: control.font
        color: !control.enabled ? Theme.textMuted : control.primary ? Theme.bg : Theme.text
        horizontalAlignment: Text.AlignHCenter
        verticalAlignment: Text.AlignVCenter
        elide: Text.ElideRight
    }

    background: Rectangle {
        implicitWidth: 72
        implicitHeight: 28
        radius: Theme.radius
        color: control.primary ? (control.down ? Qt.darker(Theme.accent, 1.2) : Theme.accent)
                               : (control.down ? Theme.bg : Theme.surfaceRaised)
        opacity: control.enabled ? 1 : 0.6
        border.color: control.visualFocus ? Theme.focus : Theme.surfaceRaised
        border.width: control.visualFocus ? Theme.focusWidth : 1
    }
}
