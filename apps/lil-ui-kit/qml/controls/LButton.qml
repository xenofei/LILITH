// A push button (APP-156): role Button, its text as its accessible name, in the tab chain;
// Space (the template's own) and Enter or Return press it.
import QtQuick
import QtQuick.Templates as T
import Lilith.UiKit
import "../core/CoreGate.js" as CoreGate

T.Button {
    id: control

    // Disabled unless the core is ready, whatever `enabled` says: the rule and how its core is
    // found are in CoreGate.js. Later panels mark their actions with this.
    property bool requiresCore: false
    property var core: null
    // A binding of its own, not the default `true`. `enabled` reads the effective value, so a gate
    // that takes over under a disabled parent would otherwise save `false` as this control's own,
    // and restore it for good. It must read a property: one with no dependency doesn't survive.
    enabled: control.requiresCore || true

    Binding {
        target: control
        property: "enabled"
        when: control.requiresCore && !CoreGate.ready(control)
        value: false
        restoreMode: Binding.RestoreBindingOrValue
    }

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
    font.family: Theme.fontFamily

    Accessible.name: text

    Keys.onReturnPressed: control.click()
    Keys.onEnterPressed: control.click()

    contentItem: Text {
        text: control.text
        textFormat: Text.PlainText
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
