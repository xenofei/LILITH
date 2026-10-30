// An on/off switch (APP-156): role CheckBox (the template's), its text as its accessible name;
// Space (the template's own), Enter and Return flip it.
import QtQuick
import QtQuick.Templates as T
import Lilith.UiKit

T.Switch {
    id: control

    implicitWidth: Math.max(implicitBackgroundWidth + leftInset + rightInset,
                            implicitContentWidth + leftPadding + rightPadding)
    implicitHeight: Math.max(implicitBackgroundHeight + topInset + bottomInset,
                             implicitContentHeight + topPadding + bottomPadding,
                             implicitIndicatorHeight + topPadding + bottomPadding)
    padding: 4
    spacing: Theme.spacing
    focusPolicy: Qt.StrongFocus
    font.pixelSize: Theme.fontSize
    font.family: Theme.fontFamily

    Accessible.name: text

    Keys.onReturnPressed: control.click()
    Keys.onEnterPressed: control.click()

    indicator: Rectangle {
        implicitWidth: 34
        implicitHeight: 18
        x: control.leftPadding
        y: control.topPadding + (control.availableHeight - height) / 2
        radius: height / 2
        color: control.checked ? Theme.accent : Theme.surfaceRaised
        border.color: control.visualFocus ? Theme.focus : Theme.textMuted
        border.width: control.visualFocus ? Theme.focusWidth : 1

        Rectangle {
            width: 12
            height: 12
            radius: 6
            y: 3
            x: control.checked ? parent.width - width - 3 : 3
            color: control.checked ? Theme.bg : Theme.text
        }
    }

    contentItem: Text {
        leftPadding: control.indicator.width + control.spacing
        text: control.text
        textFormat: Text.PlainText
        font: control.font
        color: control.enabled ? Theme.text : Theme.textMuted
        verticalAlignment: Text.AlignVCenter
    }
}
